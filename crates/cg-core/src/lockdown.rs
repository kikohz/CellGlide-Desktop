//! Lockdown over usbmuxd: the computer's saved pairing record (from usbmuxd, no root)
//! opens a lockdown session, upgraded to TLS with the pairing's host identity, which
//! starts the diagnostics relay for the battery gauge. Read-only; nothing on the phone
//! changes. The protocol is undocumented, so any failure only means "no battery details".

use crate::battery;
use crate::mux::{self, MuxEndpoint, MuxStream, REQUEST_TIMEOUT, Stream};
use crate::phone::Phone;
use crate::{CoreError, Result, tls};
use cg_ipc::phone::{BatteryReading, PhoneLabel, SimCarrier};
use cg_protocol::lockdown;
use plist::{Dictionary, Value};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::fmt;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

const LABEL: &str = "CellGlide";
const DIAGNOSTICS_RELAY: &str = "com.apple.mobile.diagnostics_relay";

fn lockdown_error(detail: impl Into<String>) -> CoreError {
    CoreError::Lockdown(detail.into())
}

/// The host identity from the computer's pairing record with this phone.
pub struct Pairing {
    pub host_id: String,
    pub system_buid: String,
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl Clone for Pairing {
    fn clone(&self) -> Self {
        Self {
            host_id: self.host_id.clone(),
            system_buid: self.system_buid.clone(),
            cert: self.cert.clone(),
            key: self.key.clone_key(),
        }
    }
}

impl fmt::Debug for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pairing")
            .field("host_id", &self.host_id)
            .finish_non_exhaustive()
    }
}

/// Reads the pairing record usbmuxd keeps for `serial`. The certificate and key are PEM;
/// the key may be PKCS#1 or PKCS#8.
pub async fn pairing(endpoint: &MuxEndpoint, serial: &str) -> Result<Pairing> {
    let data = mux::pair_record(endpoint, serial)
        .await?
        .ok_or_else(|| lockdown_error("no pairing record for this phone"))?;
    let record =
        lockdown::decode(&data).map_err(|_| lockdown_error("unreadable pairing record"))?;
    let text = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_string)
            .map(str::to_owned)
            .ok_or_else(|| lockdown_error(format!("pairing record has no {key}")))
    };
    let pem = |key: &str| match record.get(key) {
        Some(Value::Data(data)) => Ok(data.as_slice()),
        Some(Value::String(text)) => Ok(text.as_bytes()),
        _ => Err(lockdown_error(format!("pairing record has no {key}"))),
    };
    Ok(Pairing {
        host_id: text("HostID")?,
        system_buid: text("SystemBUID")?,
        cert: CertificateDer::from_pem_slice(pem("HostCertificate")?)
            .map_err(|_| lockdown_error("unreadable host certificate"))?,
        key: PrivateKeyDer::from_pem_slice(pem("HostPrivateKey")?)
            .map_err(|_| lockdown_error("unreadable host key"))?,
    })
}

// TLS. lockdownd and the services it starts present the phone's own certificate, made at
// pairing time and not issued by any CA, and ask for the pairing's host certificate.

/// Accepts the phone's certificate as it is; still checks the handshake signatures.
#[derive(Debug)]
struct PairedPhone(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for PairedPhone {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// Every TLS choice for lockdown lives here.
async fn start_tls(stream: MuxStream, pairing: &Pairing) -> Result<TlsStream<MuxStream>> {
    let provider = tls::provider();
    let verifier = Arc::new(PairedPhone(provider.signature_verification_algorithms));
    let mut config: ClientConfig = tls::client_builder(provider)
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![pairing.cert.clone()], pairing.key.clone_key())
        .map_err(|e| lockdown_error(format!("host identity: {e}")))?;
    config.enable_sni = false;
    let name = ServerName::try_from("lockdown").expect("a valid DNS name");
    timeout(
        REQUEST_TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(name, stream),
    )
    .await
    .map_err(|_| CoreError::Timeout)?
    .map_err(CoreError::from)
}

/// A lockdown-framed plist channel, in the clear or over TLS started mid-stream.
enum Channel {
    Plain(MuxStream),
    Tls(Box<TlsStream<MuxStream>>),
}

impl Channel {
    fn io(&mut self) -> &mut dyn Stream {
        match self {
            Channel::Plain(stream) => stream.as_mut(),
            Channel::Tls(stream) => stream.as_mut(),
        }
    }

    fn is_tls(&self) -> bool {
        matches!(self, Channel::Tls(_))
    }

    async fn send(&mut self, message: &Dictionary) -> Result<()> {
        let frame = lockdown::encode(message)?;
        let io = self.io();
        timeout(REQUEST_TIMEOUT, async {
            io.write_all(&frame).await?;
            io.flush().await
        })
        .await
        .map_err(|_| CoreError::Timeout)??;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Dictionary> {
        let io = self.io();
        timeout(REQUEST_TIMEOUT, async {
            let mut header = [0u8; 4];
            io.read_exact(&mut header).await?;
            let mut body = vec![0u8; lockdown::body_len(header)?];
            io.read_exact(&mut body).await?;
            Ok(lockdown::decode(&body)?)
        })
        .await
        .map_err(|_| CoreError::Timeout)?
    }

    async fn request(&mut self, message: &Dictionary) -> Result<Dictionary> {
        self.send(message).await?;
        self.receive().await
    }

    async fn upgrade(self, pairing: &Pairing) -> Result<Channel> {
        match self {
            Channel::Plain(stream) => Ok(Channel::Tls(Box::new(start_tls(stream, pairing).await?))),
            tls => Ok(tls),
        }
    }

    /// The value lockdown holds for `key`; `None` when it has none. Transport errors stay errors.
    async fn value(&mut self, key: &str) -> Result<Option<Value>> {
        let reply = self
            .request(&message([
                ("Label", LABEL.into()),
                ("Request", "GetValue".into()),
                ("Key", key.into()),
            ]))
            .await?;
        Ok(reply.get("Value").cloned())
    }

    async fn close(mut self) {
        let _ = timeout(REQUEST_TIMEOUT, self.io().shutdown()).await;
    }
}

fn message<const N: usize>(entries: [(&str, Value); N]) -> Dictionary {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn string(value: Option<Value>) -> Option<String> {
    value.and_then(Value::into_string)
}

fn reply_error(reply: &Dictionary) -> &str {
    reply
        .get("Error")
        .and_then(Value::as_string)
        .unwrap_or("unknown")
}

/// The phone's name and model identifier, asked of lockdown without a session (it answers
/// these even before pairing). Any failure leaves both unknown. The name is never logged.
pub async fn describe(phone: &Phone) -> PhoneLabel {
    let read = async {
        let mut channel = Channel::Plain(phone.open(lockdown::PORT).await?);
        let name = string(channel.value("DeviceName").await?);
        let model = string(channel.value("ProductType").await?);
        channel.close().await;
        Ok::<_, CoreError>(PhoneLabel {
            name: name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()),
            model,
        })
    };
    read.await.unwrap_or_else(|error| {
        tracing::debug!("lockdown describe failed: {error}");
        PhoneLabel::default()
    })
}

/// Facts about the phone read in the lockdown session alongside the battery.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LockdownInfo {
    pub model: Option<String>,
    pub os_version: Option<String>,
    pub baseband: Option<String>,
    pub sim_tray: Option<String>,
    pub sims: Vec<SimCarrier>,
}

/// An open lockdown session.
pub struct Session {
    channel: Channel,
    id: String,
}

/// Starts a session with the pairing's host ID, switching to TLS when lockdown asks.
pub async fn start_session(phone: &Phone, pairing: &Pairing) -> Result<Session> {
    let mut channel = Channel::Plain(phone.open(lockdown::PORT).await?);
    let reply = channel
        .request(&message([
            ("Label", LABEL.into()),
            ("Request", "StartSession".into()),
            ("HostID", pairing.host_id.as_str().into()),
            ("SystemBUID", pairing.system_buid.as_str().into()),
        ]))
        .await?;
    let Some(id) = reply.get("SessionID").and_then(Value::as_string) else {
        return Err(lockdown_error(format!(
            "session refused: {}",
            reply_error(&reply)
        )));
    };
    let id = id.to_owned();
    if reply.get("EnableSessionSSL").and_then(Value::as_boolean) == Some(true) {
        channel = channel.upgrade(pairing).await?;
    }
    tracing::debug!("lockdown session started (tls {})", channel.is_tls());
    Ok(Session { channel, id })
}

impl Session {
    pub fn tls(&self) -> bool {
        self.channel.is_tls()
    }

    /// Model, iOS version, modem and each SIM's carrier. Only the carrier bundle and network
    /// code are kept from the SIM list; its other fields (ICCID, IMSI) are dropped here.
    pub async fn info(&mut self) -> Result<LockdownInfo> {
        let model = string(self.channel.value("ProductType").await?);
        let os_version = string(self.channel.value("ProductVersion").await?);
        let baseband = string(self.channel.value("BasebandVersion").await?);
        let sim_tray = string(self.channel.value("SIMTrayStatus").await?);
        let sims = self
            .channel
            .value("CarrierBundleInfoArray")
            .await?
            .and_then(Value::into_array)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_dictionary)
            .map(|sim| {
                let field = |key: &str| sim.get(key).and_then(Value::as_string).map(str::to_owned);
                SimCarrier {
                    bundle: field("CFBundleIdentifier"),
                    mcc: field("MCC"),
                    mnc: field("MNC"),
                }
            })
            .collect();
        Ok(LockdownInfo {
            model,
            os_version,
            baseband,
            sim_tray,
            sims,
        })
    }

    /// Starts the diagnostics relay, ends the session and connects to the relay.
    pub async fn start_relay(mut self, phone: &Phone, pairing: &Pairing) -> Result<Relay> {
        let service = self
            .channel
            .request(&message([
                ("Label", LABEL.into()),
                ("Request", "StartService".into()),
                ("Service", DIAGNOSTICS_RELAY.into()),
            ]))
            .await;
        let _ = self
            .channel
            .send(&message([
                ("Label", LABEL.into()),
                ("Request", "StopSession".into()),
                ("SessionID", self.id.as_str().into()),
            ]))
            .await;
        self.channel.close().await;
        let service = service?;
        let port = service
            .get("Port")
            .and_then(Value::as_unsigned_integer)
            .and_then(|p| u16::try_from(p).ok())
            .ok_or_else(|| {
                lockdown_error(format!(
                    "diagnostics service refused: {}",
                    reply_error(&service)
                ))
            })?;
        let mut channel = Channel::Plain(phone.open(port).await?);
        if service.get("EnableServiceSSL").and_then(Value::as_boolean) == Some(true) {
            channel = channel.upgrade(pairing).await?;
        }
        Ok(Relay { channel })
    }
}

/// A diagnostics relay connection, kept open between battery reads.
pub struct Relay {
    channel: Channel,
}

/// Opens the relay and reads the lockdown facts on the way.
pub async fn open_relay(phone: &Phone, pairing: &Pairing) -> Result<(Relay, LockdownInfo)> {
    let mut session = start_session(phone, pairing).await?;
    let info = session.info().await?;
    let relay = session.start_relay(phone, pairing).await?;
    Ok((relay, info))
}

impl Relay {
    pub fn tls(&self) -> bool {
        self.channel.is_tls()
    }

    /// The raw AppleSmartBattery registry entry.
    pub async fn registry(&mut self) -> Result<Dictionary> {
        let mut reply = self
            .channel
            .request(&message([
                ("Request", "IORegistry".into()),
                ("EntryName", "AppleSmartBattery".into()),
            ]))
            .await?;
        let status = reply.get("Status").and_then(Value::as_string);
        if status != Some("Success") {
            return Err(lockdown_error(format!(
                "battery query failed: {}",
                status.unwrap_or("unknown")
            )));
        }
        reply
            .remove("Diagnostics")
            .and_then(Value::into_dictionary)
            .and_then(|mut d| d.remove("IORegistry"))
            .and_then(Value::into_dictionary)
            .ok_or_else(|| lockdown_error("battery query returned no registry"))
    }

    /// `Ok(None)` when the gauge reported none of the fields a reading needs.
    pub async fn battery(&mut self) -> Result<Option<BatteryReading>> {
        Ok(battery::reading_from_registry(&self.registry().await?))
    }

    /// Says goodbye to the relay and closes the connection; failures are ignored.
    pub async fn goodbye(mut self) {
        let _ = self
            .channel
            .send(&message([("Request", "Goodbye".into())]))
            .await;
        self.channel.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::battery_fixtures::charging;
    use crate::testing::{
        DEVICE_CERT_PEM, DEVICE_KEY_PEM, FIXTURE_HOST_ID, FIXTURE_SYSTEM_BUID, FakeLockdown,
        FakeMux, FakePhone, FakePhoneOptions, device, fixture_pair_record, host_cert_der,
    };
    use plist::Value;

    async fn setup(lockdown: FakeLockdown) -> (FakeMux, FakePhone, Phone) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        mux.add_pair_record_with("aaaa", fixture_pair_record());
        let fake = FakePhone::start(
            &mux,
            FakePhoneOptions {
                lockdown: Some(lockdown),
                ..FakePhoneOptions::default()
            },
        )
        .await;
        let phone = Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        };
        (mux, fake, phone)
    }

    fn air() -> FakeLockdown {
        FakeLockdown {
            name: Some("Alice’s iPhone".into()),
            model: Some("iPhone18,4".into()),
            os_version: Some("26.1".into()),
            sims: vec![
                ("com.apple.ATT_aio_NR_US".into(), "310".into(), "410".into()),
                (
                    "com.apple.TMobile_MVNO_US".into(),
                    "310".into(),
                    "240".into(),
                ),
            ],
            registry: Some(charging()),
            ..FakeLockdown::default()
        }
    }

    #[tokio::test]
    async fn describe_reads_name_and_model_without_a_session() {
        let (_mux, _phone, p) = setup(FakeLockdown {
            name: Some(" Alice’s iPhone ".into()),
            model: Some("iPhone18,4".into()),
            ..Default::default()
        })
        .await;
        let label = describe(&p).await;
        assert_eq!(label.name.as_deref(), Some("Alice’s iPhone"));
        assert_eq!(label.model.as_deref(), Some("iPhone18,4"));

        let (_mux, _phone, blank) = setup(FakeLockdown {
            name: Some("  ".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(describe(&blank).await, PhoneLabel::default());

        // No lockdownd at all: both unknown, no error.
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let silent = Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        };
        assert_eq!(describe(&silent).await, PhoneLabel::default());
    }

    #[tokio::test]
    async fn pairing_reads_the_record() {
        let (mux, _phone, _p) = setup(FakeLockdown::default()).await;
        let pairing = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        assert_eq!(pairing.host_id, FIXTURE_HOST_ID);
        assert_eq!(pairing.system_buid, FIXTURE_SYSTEM_BUID);
        assert_eq!(pairing.cert, host_cert_der());

        // A PKCS#8 key is accepted too.
        let mut record = fixture_pair_record();
        record.insert(
            "HostPrivateKey".into(),
            Value::Data(DEVICE_KEY_PEM.to_vec()),
        );
        mux.add_pair_record_with("bbbb", record);
        pairing_from(&mux, "bbbb").await.unwrap();
    }

    async fn pairing_from(mux: &FakeMux, serial: &str) -> Result<Pairing> {
        pairing(&mux.endpoint(), serial).await
    }

    #[tokio::test]
    async fn relay_reads_battery_and_lockdown_info() {
        let (mux, fake, p) = setup(air()).await;
        let pairing = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        let (mut relay, info) = open_relay(&p, &pairing).await.unwrap();
        assert_eq!(info.model.as_deref(), Some("iPhone18,4"));
        assert_eq!(info.os_version.as_deref(), Some("26.1"));
        assert_eq!(info.baseband.as_deref(), Some("1.00.00"));
        assert_eq!(
            info.sim_tray.as_deref(),
            Some("kCTSIMSupportSIMTrayInsertedSIM")
        );
        assert_eq!(info.sims.len(), 2);
        assert_eq!(
            info.sims[0].bundle.as_deref(),
            Some("com.apple.ATT_aio_NR_US")
        );
        assert_eq!(info.sims[0].mcc.as_deref(), Some("310"));
        assert_eq!(info.sims[1].mnc.as_deref(), Some("240"));
        let reading = relay.battery().await.unwrap().unwrap();
        assert_eq!(reading.level, Some(75));
        assert_eq!(reading.cycles, Some(396));
        // The relay stays open between reads.
        assert_eq!(relay.battery().await.unwrap().unwrap().level, Some(75));
        assert_eq!(fake.relay_requests(), 2);
        assert!(
            fake.lockdown_client_certs().is_empty(),
            "no TLS was asked for"
        );
        relay.goodbye().await;
    }

    #[tokio::test]
    async fn session_upgrades_to_tls_with_the_pair_record() {
        let (mux, fake, p) = setup(FakeLockdown {
            session_tls: true,
            ..air()
        })
        .await;
        let pairing = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        let (mut relay, info) = open_relay(&p, &pairing).await.unwrap();
        assert!(relay.tls());
        // Values read inside the TLS session.
        assert_eq!(info.model.as_deref(), Some("iPhone18,4"));
        assert_eq!(
            info.sims[0].bundle.as_deref(),
            Some("com.apple.ATT_aio_NR_US")
        );
        // A diagnostics request round-trips through the TLS relay.
        let reading = relay.battery().await.unwrap().unwrap();
        assert_eq!(reading.level, Some(75));
        assert!((reading.watts.unwrap() - 2.562).abs() < 0.001);
        assert_eq!(fake.relay_requests(), 1);
        // The fake demanded a client certificate chaining to host-cert.pem on both the
        // session and the relay, and saw exactly the pairing's certificate each time.
        // (Checked after the round trip: in TLS 1.3 the server finishes its side last.)
        assert_eq!(
            fake.lockdown_client_certs(),
            vec![host_cert_der(), host_cert_der()]
        );
        relay.goodbye().await;
    }

    #[tokio::test]
    async fn a_foreign_host_identity_fails_the_handshake() {
        let (mux, fake, p) = setup(FakeLockdown {
            session_tls: true,
            ..air()
        })
        .await;
        let mut record = fixture_pair_record();
        record.insert(
            "HostCertificate".into(),
            Value::Data(DEVICE_CERT_PEM.to_vec()),
        );
        record.insert(
            "HostPrivateKey".into(),
            Value::Data(DEVICE_KEY_PEM.to_vec()),
        );
        mux.add_pair_record_with("aaaa", record);
        let foreign = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        assert!(open_relay(&p, &foreign).await.is_err());
        assert!(fake.lockdown_client_certs().is_empty());
        assert_eq!(fake.relay_requests(), 0);
    }

    #[tokio::test]
    async fn missing_pair_record_or_refused_relay_is_an_error() {
        let (mux, fake, p) = setup(FakeLockdown {
            relay_refused: true,
            ..air()
        })
        .await;
        assert!(pairing(&mux.endpoint(), "bbbb").await.is_err());
        mux.add_pair_record("cccc"); // contents that aren't a plist
        assert!(pairing(&mux.endpoint(), "cccc").await.is_err());
        let pairing = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        assert!(open_relay(&p, &pairing).await.is_err());
        assert_eq!(fake.relay_requests(), 0);
    }

    #[tokio::test]
    async fn a_failed_battery_query_is_an_error() {
        let (mux, _fake, p) = setup(FakeLockdown {
            registry: None,
            ..air()
        })
        .await;
        let pairing = pairing(&mux.endpoint(), "aaaa").await.unwrap();
        let (mut relay, _) = open_relay(&p, &pairing).await.unwrap();
        assert!(relay.battery().await.is_err());
    }
}
