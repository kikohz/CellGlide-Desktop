//! A stand-in for the phone's lockdownd on 62078 and the diagnostics relay it starts,
//! with the test pairing identities from `tests/fixtures/lockdown`.

use crate::mux::Stream;
use cg_protocol::lockdown;
use plist::{Dictionary, Value};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub const HOST_CERT_PEM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lockdown/host-cert.pem"
));
/// PKCS#1, as usbmuxd stores it. A throwaway test key (see tests/fixtures/lockdown/README.md).
pub const HOST_KEY_PEM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lockdown/host-key.pem"
));
pub const DEVICE_CERT_PEM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lockdown/device-cert.pem"
));
/// PKCS#8. A throwaway test key (see tests/fixtures/lockdown/README.md).
pub const DEVICE_KEY_PEM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lockdown/device-key.pem"
));
pub const FIXTURE_HOST_ID: &str = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
pub const FIXTURE_SYSTEM_BUID: &str = "11223344-5566-7788-99AA-BBCCDDEEFF00";
/// Where the fake starts the diagnostics relay.
pub const FAKE_RELAY_PORT: u16 = 49_999;

/// The test host certificate in DER, as the fake sees it in the client's TLS handshake.
pub fn host_cert_der() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(HOST_CERT_PEM).unwrap()
}

/// A pairing record as usbmuxd keeps it, with the test host identity.
pub fn fixture_pair_record() -> Dictionary {
    let mut record = Dictionary::new();
    record.insert("HostID".into(), FIXTURE_HOST_ID.into());
    record.insert("SystemBUID".into(), FIXTURE_SYSTEM_BUID.into());
    record.insert(
        "HostCertificate".into(),
        Value::Data(HOST_CERT_PEM.to_vec()),
    );
    record.insert("HostPrivateKey".into(), Value::Data(HOST_KEY_PEM.to_vec()));
    record.insert(
        "DeviceCertificate".into(),
        Value::Data(DEVICE_CERT_PEM.to_vec()),
    );
    record.insert("WiFiMACAddress".into(), "00:00:00:00:00:00".into());
    record
}

/// What the fake lockdownd answers. `None` values are missing on the phone.
#[derive(Clone, Debug, Default)]
pub struct FakeLockdown {
    pub name: Option<String>,
    pub model: Option<String>,
    pub os_version: Option<String>,
    /// (CFBundleIdentifier, MCC, MNC) per SIM.
    pub sims: Vec<(String, String, String)>,
    /// The AppleSmartBattery entry; `None` makes the IORegistry query fail.
    pub registry: Option<Dictionary>,
    /// Session and relay both switch to TLS, requiring the test host certificate.
    pub session_tls: bool,
    /// StartService for the diagnostics relay is refused.
    pub relay_refused: bool,
    /// The relay reads the IORegistry request and never answers (a phone that has stopped
    /// responding mid-frame); it still notices when the client hangs up.
    pub registry_stalls: bool,
}

#[derive(Default)]
pub(super) struct Observed {
    /// The client certificate of every completed TLS handshake, session and relay alike.
    pub client_certs: Mutex<Vec<CertificateDer<'static>>>,
    /// IORegistry requests answered on the relay.
    pub relay_requests: AtomicUsize,
    /// Connections to the relay (TLS handshake included) that are still open.
    pub open_relays: AtomicUsize,
    /// Connections to lockdownd that are still open.
    pub open_sessions: AtomicUsize,
}

/// Counts a connection as open until it is dropped, however the serving task ends.
struct Open<'a>(&'a AtomicUsize);

impl<'a> Open<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Fake {
    options: FakeLockdown,
    tls: Option<TlsAcceptor>,
    observed: Arc<Observed>,
}

fn server_config() -> Arc<ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.add(host_cert_der()).unwrap();
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .unwrap();
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(DEVICE_CERT_PEM).unwrap()],
            PrivateKeyDer::from_pem_slice(DEVICE_KEY_PEM).unwrap(),
        )
        .unwrap();
    Arc::new(config)
}

pub(super) async fn start(mux: &super::FakeMux, options: FakeLockdown) -> Arc<Observed> {
    let observed = Arc::new(Observed::default());
    let fake = Arc::new(Fake {
        tls: options
            .session_tls
            .then(|| TlsAcceptor::from(server_config())),
        options,
        observed: observed.clone(),
    });
    for (port, relay) in [(lockdown::PORT, false), (FAKE_RELAY_PORT, true)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        mux.route(port, listener.local_addr().unwrap());
        let fake = fake.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let fake = fake.clone();
                tokio::spawn(async move {
                    let stream: Box<dyn Stream> = Box::new(stream);
                    if relay {
                        serve_relay(stream, fake).await;
                    } else {
                        serve_lockdown(stream, fake).await;
                    }
                });
            }
        });
    }
    observed
}

async fn receive(stream: &mut Box<dyn Stream>) -> Option<Dictionary> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.ok()?;
    let mut body = vec![0u8; lockdown::body_len(header).ok()?];
    stream.read_exact(&mut body).await.ok()?;
    lockdown::decode(&body).ok()
}

async fn send(stream: &mut Box<dyn Stream>, reply: Dictionary) -> Option<()> {
    stream
        .write_all(&lockdown::encode(&reply).ok()?)
        .await
        .ok()?;
    stream.flush().await.ok()
}

/// Wraps the stream in TLS as the phone does, recording the client's certificate.
async fn upgrade(stream: Box<dyn Stream>, fake: &Fake) -> Option<Box<dyn Stream>> {
    let acceptor = fake.tls.as_ref()?;
    let tls = acceptor.accept(stream).await.ok()?;
    if let Some(cert) = tls.get_ref().1.peer_certificates().and_then(|c| c.first()) {
        fake.observed
            .client_certs
            .lock()
            .unwrap()
            .push(cert.clone().into_owned());
    }
    Some(Box::new(tls))
}

fn text(message: &Dictionary, key: &str) -> String {
    message
        .get(key)
        .and_then(Value::as_string)
        .unwrap_or_default()
        .to_owned()
}

fn error(request: &str, error: &str) -> Dictionary {
    let mut reply = Dictionary::new();
    reply.insert("Request".into(), request.into());
    reply.insert("Error".into(), error.into());
    reply
}

fn value(options: &FakeLockdown, key: &str) -> Option<Value> {
    let string = |v: &Option<String>| v.as_deref().map(Value::from);
    match key {
        "DeviceName" => string(&options.name),
        "ProductType" => string(&options.model),
        "ProductVersion" => string(&options.os_version),
        "BasebandVersion" => Some("1.00.00".into()),
        "SIMTrayStatus" => Some("kCTSIMSupportSIMTrayInsertedSIM".into()),
        "CarrierBundleInfoArray" => Some(Value::Array(
            options
                .sims
                .iter()
                .map(|(bundle, mcc, mnc)| {
                    let mut sim = Dictionary::new();
                    sim.insert("CFBundleIdentifier".into(), bundle.as_str().into());
                    sim.insert("CFBundleVersion".into(), "1.0".into());
                    sim.insert("MCC".into(), mcc.as_str().into());
                    sim.insert("MNC".into(), mnc.as_str().into());
                    // Identifiers the client must drop.
                    sim.insert(
                        "IntegratedCircuitCardIdentity".into(),
                        "8901000000000000000".into(),
                    );
                    sim.insert(
                        "InternationalMobileSubscriberIdentity".into(),
                        "310410000000000".into(),
                    );
                    Value::Dictionary(sim)
                })
                .collect(),
        )),
        _ => None,
    }
}

async fn serve_lockdown(mut stream: Box<dyn Stream>, fake: Arc<Fake>) {
    let _open = Open::new(&fake.observed.open_sessions);
    while let Some(message) = receive(&mut stream).await {
        let request = text(&message, "Request");
        let mut reply = Dictionary::new();
        reply.insert("Request".into(), request.as_str().into());
        match request.as_str() {
            "GetValue" => {
                let key = text(&message, "Key");
                reply.insert("Key".into(), key.as_str().into());
                match value(&fake.options, &key) {
                    Some(v) => {
                        reply.insert("Value".into(), v);
                    }
                    None => reply = error("GetValue", "MissingValue"),
                }
            }
            "StartSession" => {
                if text(&message, "HostID") != FIXTURE_HOST_ID
                    || text(&message, "SystemBUID") != FIXTURE_SYSTEM_BUID
                {
                    reply = error("StartSession", "InvalidHostID");
                } else {
                    reply.insert("SessionID".into(), "FAKE-SESSION".into());
                    reply.insert(
                        "EnableSessionSSL".into(),
                        Value::Boolean(fake.options.session_tls),
                    );
                }
            }
            "StartService" => {
                if fake.options.relay_refused
                    || text(&message, "Service") != "com.apple.mobile.diagnostics_relay"
                {
                    reply = error("StartService", "InvalidService");
                } else {
                    reply.insert(
                        "Port".into(),
                        Value::Integer(u64::from(FAKE_RELAY_PORT).into()),
                    );
                    reply.insert(
                        "EnableServiceSSL".into(),
                        Value::Boolean(fake.options.session_tls),
                    );
                }
            }
            "StopSession" => {}
            _ => reply = error(&request, "InvalidRequest"),
        }
        let upgrade_now = request == "StartSession" && reply.contains_key("SessionID");
        if send(&mut stream, reply).await.is_none() {
            return;
        }
        if upgrade_now && fake.options.session_tls {
            match upgrade(stream, &fake).await {
                Some(tls) => stream = tls,
                None => return,
            }
        }
    }
}

async fn serve_relay(mut stream: Box<dyn Stream>, fake: Arc<Fake>) {
    let _open = Open::new(&fake.observed.open_relays);
    if fake.options.session_tls {
        match upgrade(stream, &fake).await {
            Some(tls) => stream = tls,
            None => return,
        }
    }
    while let Some(message) = receive(&mut stream).await {
        let mut reply = Dictionary::new();
        match text(&message, "Request").as_str() {
            "IORegistry" if text(&message, "EntryName") == "AppleSmartBattery" => {
                fake.observed.relay_requests.fetch_add(1, Ordering::SeqCst);
                if fake.options.registry_stalls {
                    while receive(&mut stream).await.is_some() {}
                    return;
                }
                match &fake.options.registry {
                    Some(registry) => {
                        let mut diagnostics = Dictionary::new();
                        diagnostics
                            .insert("IORegistry".into(), Value::Dictionary(registry.clone()));
                        reply.insert("Status".into(), "Success".into());
                        reply.insert("Diagnostics".into(), Value::Dictionary(diagnostics));
                    }
                    None => {
                        reply.insert("Status".into(), "Failure".into());
                    }
                }
            }
            "Goodbye" => {
                reply.insert("Status".into(), "Success".into());
                let _ = send(&mut stream, reply).await;
                return;
            }
            _ => {
                reply.insert("Status".into(), "UnknownRequest".into());
            }
        }
        if send(&mut stream, reply).await.is_none() {
            return;
        }
    }
}
