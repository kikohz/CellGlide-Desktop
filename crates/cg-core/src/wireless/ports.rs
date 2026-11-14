//! What the session needs from the platform for the wireless link. The Windows implementations
//! live in `cg-platform`; tests use in-memory stand-ins.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::ALLOWED_PORTS;
use super::pairing::LinkKey;
use super::policy::DIAL_TIMEOUT;
use super::request::Request;
use crate::mux::MuxStream;
use crate::{CoreError, Result};

/// A boxed, sendable future (the `futures` crate's `BoxFuture`, without the dependency).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An established wireless link to one paired phone. Like usbmuxd over the cable, it opens
/// connections to the phone's loopback ports, so everything above it works the same way.
pub trait WirelessLink: Send + Sync {
    /// The phone's random id (its Bonjour instance name).
    fn phone_id(&self) -> &str;

    /// A new connection to `port` on the phone.
    fn open(&self, port: u16) -> BoxFuture<'_, Result<MuxStream>>;
}

/// A phone advertising the CellGlide service on the access point.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoundPhone {
    /// The service instance name: the phone's identifier (phoneID).
    pub instance: String,
    pub address: IpAddr,
    pub port: u16,
}

/// Why a TLS connection to the phone did not come up.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DialError {
    /// The phone does not know the PSK identity (TLS alert 115, unknown_psk_identity).
    #[error("the phone does not know this pairing")]
    UnknownIdentity,
    #[error("no connection within the time limit")]
    Timeout,
    /// Anything else; text for logs only.
    #[error("{0}")]
    Io(String),
}

/// Why the access point could not be started (defined beside the snapshot, which reports it).
pub use cg_ipc::wireless::ApFailure;

/// Starts this computer's temporary access point.
pub trait AccessPoint: Send + Sync {
    fn start<'a>(
        &'a self,
        ssid: &'a str,
        passphrase: &'a str,
    ) -> BoxFuture<'a, std::result::Result<Box<dyn RunningAccessPoint>, ApFailure>>;
}

/// A started access point. Dropping it stops it.
pub trait RunningAccessPoint: Send {
    /// This computer's address on the access point's network, once known.
    fn local_address(&self) -> Option<IpAddr>;

    /// Addresses of the devices that joined.
    fn clients(&self) -> Vec<IpAddr>;

    /// Completes when the system stopped the access point on its own (aborted). It may be
    /// called again after the future is dropped, and then completes at once if the access point
    /// was already lost.
    fn lost(&mut self) -> BoxFuture<'_, ()>;
}

/// Finds the phones advertising the CellGlide service on the access point.
pub trait Browser: Send + Sync {
    /// One discovery round on the network of `local` (this computer's address there), taking
    /// about `timeout`. Finding nothing in a round does not mean the phone left.
    fn browse(
        &self,
        local: IpAddr,
        timeout: Duration,
    ) -> BoxFuture<'_, io::Result<Vec<FoundPhone>>>;
}

/// Opens TLS-PSK connections to the phone's wireless listener.
///
/// The stream must keep a half-close half: shutting down its write side sends neither
/// close_notify nor a TCP FIN-triggered teardown, so the phone's reply can still be read;
/// dropping it closes the TCP connection at once.
pub trait Dialer: Send + Sync {
    /// Connects and completes the handshake; nothing is written. The request line is the
    /// caller's (`TlsLink` writes `CG1 DATA <port>`, code pairing `CG1 PAIR`).
    fn dial<'a>(
        &'a self,
        to: SocketAddr,
        identity: &'a str,
        psk: &'a LinkKey,
    ) -> BoxFuture<'a, std::result::Result<MuxStream, DialError>>;
}

/// The platform's wireless capabilities, injected into the controller.
#[derive(Clone)]
pub struct WirelessPorts {
    pub access_point: Arc<dyn AccessPoint>,
    pub browser: Arc<dyn Browser>,
    pub dialer: Arc<dyn Dialer>,
}

impl std::fmt::Debug for WirelessPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WirelessPorts").finish_non_exhaustive()
    }
}

/// The wireless link to a paired phone: each `open(port)` dials a new TLS connection with the
/// pairing's identity and link key and writes `CG1 DATA <port>`, all within `DIAL_TIMEOUT`.
pub struct TlsLink {
    phone_id: String,
    to: SocketAddr,
    identity: String,
    key: LinkKey,
    dialer: Arc<dyn Dialer>,
}

impl TlsLink {
    pub fn new(
        phone_id: &str,
        to: SocketAddr,
        identity: &str,
        key: LinkKey,
        dialer: Arc<dyn Dialer>,
    ) -> TlsLink {
        TlsLink {
            phone_id: phone_id.to_string(),
            to,
            identity: identity.to_string(),
            key,
            dialer,
        }
    }

    /// Where the phone is dialled.
    pub fn address(&self) -> SocketAddr {
        self.to
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    async fn dial_and_ask(&self, port: u16) -> Result<MuxStream> {
        let mut stream = self
            .dialer
            .dial(self.to, &self.identity, &self.key)
            .await
            .map_err(CoreError::Dial)?;
        // The phone gives handshake and request line 5 s together: send it at once.
        async {
            stream
                .write_all(Request::Data(port).line().as_bytes())
                .await?;
            stream.flush().await
        }
        .await
        .map_err(|e| CoreError::Dial(DialError::Io(e.to_string())))?;
        Ok(stream)
    }
}

impl std::fmt::Debug for TlsLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsLink")
            .field("phone_id", &self.phone_id)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

impl WirelessLink for TlsLink {
    fn phone_id(&self) -> &str {
        &self.phone_id
    }

    fn open(&self, port: u16) -> BoxFuture<'_, Result<MuxStream>> {
        Box::pin(async move {
            if !ALLOWED_PORTS.contains(&port) {
                return Err(CoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("port {port} is not reachable over the wireless link"),
                )));
            }
            tokio::time::timeout(DIAL_TIMEOUT, self.dial_and_ask(port))
                .await
                .map_err(|_| CoreError::Dial(DialError::Timeout))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wireless::CABLE_PAIRING_PORT;
    use crate::wireless::pairing::{Key, link_key};
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream};

    const PHONE: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

    #[derive(Clone, Copy)]
    enum Behaviour {
        Connect { buffer: usize },
        Fail,
        Hang,
    }

    /// Hands out one end of an in-memory pipe and keeps the other for the test.
    struct TestDialer {
        behaviour: Behaviour,
        calls: Mutex<Vec<(SocketAddr, String, [u8; 32])>>,
        phone_ends: Mutex<Vec<DuplexStream>>,
    }

    impl TestDialer {
        fn new(behaviour: Behaviour) -> Arc<TestDialer> {
            Arc::new(TestDialer {
                behaviour,
                calls: Mutex::new(Vec::new()),
                phone_ends: Mutex::new(Vec::new()),
            })
        }
    }

    impl Dialer for TestDialer {
        fn dial<'a>(
            &'a self,
            to: SocketAddr,
            identity: &'a str,
            psk: &'a LinkKey,
        ) -> BoxFuture<'a, std::result::Result<MuxStream, DialError>> {
            self.calls
                .lock()
                .unwrap()
                .push((to, identity.to_string(), *psk.as_bytes()));
            Box::pin(async move {
                match self.behaviour {
                    Behaviour::Connect { buffer } => {
                        let (ours, theirs) = tokio::io::duplex(buffer);
                        self.phone_ends.lock().unwrap().push(theirs);
                        Ok(Box::new(ours) as MuxStream)
                    }
                    Behaviour::Fail => Err(DialError::UnknownIdentity),
                    Behaviour::Hang => std::future::pending().await,
                }
            })
        }
    }

    fn long_term() -> Key {
        Key::from_bytes([7; 32])
    }

    fn link(dialer: &Arc<TestDialer>) -> TlsLink {
        TlsLink::new(
            PHONE,
            "192.168.137.205:56582".parse().unwrap(),
            IDENTITY,
            link_key(&long_term()),
            dialer.clone(),
        )
    }

    #[tokio::test]
    async fn open_dials_with_the_pairing_and_writes_the_request_line_first() {
        let dialer = TestDialer::new(Behaviour::Connect { buffer: 1024 });
        let link = link(&dialer);
        assert_eq!(link.phone_id(), PHONE);
        let mut stream = link.open(8088).await.unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let calls = dialer.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(
                "192.168.137.205:56582".parse().unwrap(),
                IDENTITY.to_string(),
                *link_key(&long_term()).as_bytes()
            )]
        );
        let phone = dialer.phone_ends.lock().unwrap().pop().unwrap();
        let mut phone = BufReader::new(phone);
        let mut line = String::new();
        phone.read_line(&mut line).await.unwrap();
        assert_eq!(line, "CG1 DATA 8088\n");
        line.clear();
        phone.read_line(&mut line).await.unwrap();
        assert_eq!(line, "GET / HTTP/1.1\r\n");
    }

    #[tokio::test]
    async fn every_allowed_port_gets_its_own_connection() {
        let dialer = TestDialer::new(Behaviour::Connect { buffer: 1024 });
        let link = link(&dialer);
        for port in ALLOWED_PORTS {
            let _stream = link.open(port).await.unwrap();
            let phone = dialer.phone_ends.lock().unwrap().pop().unwrap();
            let mut line = String::new();
            BufReader::new(phone).read_line(&mut line).await.unwrap();
            assert_eq!(line, format!("CG1 DATA {port}\n"));
        }
        assert_eq!(dialer.calls.lock().unwrap().len(), ALLOWED_PORTS.len());
    }

    #[tokio::test]
    async fn a_port_outside_the_allowlist_is_refused_without_dialing() {
        let dialer = TestDialer::new(Behaviour::Connect { buffer: 1024 });
        let link = link(&dialer);
        for port in [CABLE_PAIRING_PORT, 22, 62078, 0] {
            let error = link.open(port).await.err().expect("refused");
            assert!(
                matches!(&error, CoreError::Io(e) if e.kind() == io::ErrorKind::InvalidInput),
                "{error:?}"
            );
        }
        assert!(dialer.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dial_failure_comes_back_as_it_was() {
        let dialer = TestDialer::new(Behaviour::Fail);
        let error = link(&dialer).open(9876).await.err().unwrap();
        assert!(
            matches!(error, CoreError::Dial(DialError::UnknownIdentity)),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_dial_that_never_completes_times_out_at_four_seconds() {
        let dialer = TestDialer::new(Behaviour::Hang);
        let link = link(&dialer);
        let started = tokio::time::Instant::now();
        let error = link.open(9876).await.err().unwrap();
        assert!(
            matches!(error, CoreError::Dial(DialError::Timeout)),
            "{error:?}"
        );
        assert_eq!(started.elapsed(), DIAL_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn the_request_line_counts_toward_the_same_four_seconds() {
        // The phone never reads, and the pipe holds only 4 bytes: the line can't be written.
        let dialer = TestDialer::new(Behaviour::Connect { buffer: 4 });
        let link = link(&dialer);
        let started = tokio::time::Instant::now();
        let error = link.open(9876).await.err().unwrap();
        assert!(
            matches!(error, CoreError::Dial(DialError::Timeout)),
            "{error:?}"
        );
        assert_eq!(started.elapsed(), DIAL_TIMEOUT);
    }

    #[test]
    fn debug_shows_no_key() {
        let dialer = TestDialer::new(Behaviour::Fail);
        let shown = format!("{:?}", link(&dialer));
        assert!(shown.contains(PHONE), "{shown}");
        assert!(!shown.contains("key"), "{shown}");
    }
}
