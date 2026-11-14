//! cg-core's wireless ports on Windows: the Wi-Fi Direct access point ([`access_point`]), our
//! own mDNS queries on it ([`mdns`], `Method::Raw`) and the TLS-PSK dialer ([`wireless_tls`]).
//!
//! Every blocking call runs on a blocking thread, never on the session's task: the access
//! point's start (up to 10 s of WinRT) and its stop (`Stop`/`Close` when it is dropped), and
//! each discovery round (it blocks for the whole round).
//!
//! [`access_point`]: super::access_point

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use cg_core::mux::MuxStream;
use cg_core::wireless::pairing::LinkKey;
use cg_core::wireless::policy::DIAL_TIMEOUT;
use cg_core::wireless::ports::{
    AccessPoint as AccessPointPort, ApFailure, BoxFuture, Browser, DialError, Dialer, FoundPhone,
    RunningAccessPoint, WirelessPorts,
};
use tokio::sync::{oneshot, watch};
use zeroize::Zeroizing;

use super::access_point::{AccessPoint, ApError};
use super::mdns::{self, Method};
use super::wireless_tls::{self, HalfCloseSafe, TlsError};

/// The address Windows gives the Wi-Fi Direct group owner.
pub const ACCESS_POINT_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 137, 1);

/// The Windows implementations, for `ControllerConfig::wireless_ports`.
pub fn ports() -> WirelessPorts {
    WirelessPorts {
        access_point: Arc::new(WiFiDirect),
        browser: Arc::new(RawMdns),
        dialer: Arc::new(TlsDialer),
    }
}

// ---- Access point ---------------------------------------------------------------------------

/// The Wi-Fi Direct autonomous group owner with a legacy access point.
#[derive(Debug, Default)]
pub struct WiFiDirect;

impl AccessPointPort for WiFiDirect {
    fn start<'a>(
        &'a self,
        ssid: &'a str,
        passphrase: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn RunningAccessPoint>, ApFailure>> {
        let ssid = ssid.to_string();
        let passphrase = Zeroizing::new(passphrase.to_string());
        Box::pin(async move {
            let started = start_off_thread(move || AccessPoint::start(&ssid, &passphrase)).await;
            match started {
                Ok(access_point) => Ok(Box::new(Running::new(access_point)) as _),
                Err(error) => {
                    tracing::warn!("access point: not started: {error}");
                    Err(ap_failure(&error))
                }
            }
        })
    }
}

/// Runs `start` on a blocking thread. If the caller drops this future before the result
/// arrives (cancelled, the service stopping), the result is dropped on that thread as soon as
/// `start` returns, so a started access point is stopped at once; and if the caller is already
/// gone when the thread gets to run, `start` is not called at all.
async fn start_off_thread<T, F>(start: F) -> Result<T, ApError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApError> + Send + 'static,
{
    let (sender, result) = oneshot::channel();
    // Detached: the thread finishes on its own whether or not anyone waits.
    drop(tokio::task::spawn_blocking(move || {
        if sender.is_closed() {
            return;
        }
        let started = start();
        // On failure the value comes back and is dropped here, on this thread.
        if let Err(unwanted) = sender.send(started) {
            if unwanted.is_ok() {
                tracing::info!("access point: started after the caller gave up; stopping it");
            }
            drop(unwanted);
        }
    }));
    result
        .await
        .unwrap_or_else(|_| Err(ApError::Unsupported("the start was interrupted".into())))
}

/// Design §6.1's causes as the snapshot reports them.
pub fn ap_failure(error: &ApError) -> ApFailure {
    match error {
        ApError::NoAdapter => ApFailure::NoAdapter,
        ApError::RadioOff => ApFailure::RadioOff,
        ApError::InUse {
            mobile_hotspot: true,
        } => ApFailure::MobileHotspotOn,
        ApError::InUse {
            mobile_hotspot: false,
        } => ApFailure::InUse,
        ApError::Unsupported(_) => ApFailure::Unsupported,
    }
}

/// A started access point; dropping it stops it on a blocking thread.
struct Running {
    inner: Option<AccessPoint>,
    lost: watch::Receiver<bool>,
}

impl Running {
    fn new(access_point: AccessPoint) -> Running {
        Running {
            lost: access_point.lost_signal(),
            inner: Some(access_point),
        }
    }
}

impl RunningAccessPoint for Running {
    fn local_address(&self) -> Option<IpAddr> {
        let clients = self
            .inner
            .as_ref()
            .map(AccessPoint::clients)
            .unwrap_or_default();
        local_address(&clients, mdns::is_local_address)
    }

    fn clients(&self) -> Vec<IpAddr> {
        self.inner
            .as_ref()
            .map(AccessPoint::clients)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, remote)| remote)
            .collect()
    }

    fn lost(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(wait_lost(self.lost.clone()))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(access_point) = self.inner.take() {
            drop_off_thread(access_point);
        }
    }
}

/// Drops `value` on a blocking thread when called inside a runtime (the runtime waits for it
/// when it shuts down), else right here.
fn drop_off_thread<T: Send + 'static>(value: T) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => drop(runtime.spawn_blocking(move || drop(value))),
        Err(_) => drop(value),
    }
}

/// Completes once the flag is true (at once if it already is). The sender lives as long as the
/// access point; should it go first, nothing was reported lost, so this stays pending.
async fn wait_lost(mut lost: watch::Receiver<bool>) {
    if lost.wait_for(|lost| *lost).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// This computer's address on the access point: the one a joined device reached, else the
/// group owner's usual address once some adapter has it (discovery works before a device was
/// accepted, docs/validation/windows-1.1-gate.md), else not known yet.
fn local_address(
    clients: &[(IpAddr, IpAddr)],
    assigned: impl FnOnce(Ipv4Addr) -> bool,
) -> Option<IpAddr> {
    if let Some(&(local, _)) = clients.first() {
        return Some(local);
    }
    assigned(ACCESS_POINT_ADDRESS).then_some(IpAddr::V4(ACCESS_POINT_ADDRESS))
}

// ---- Discovery ------------------------------------------------------------------------------

/// Our own mDNS queries (`Method::Raw`), one round per call on a blocking thread.
#[derive(Debug, Default)]
pub struct RawMdns;

impl Browser for RawMdns {
    fn browse(
        &self,
        local: IpAddr,
        timeout: Duration,
    ) -> BoxFuture<'_, io::Result<Vec<FoundPhone>>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || mdns::browse_with(Method::Raw, local, timeout))
                .await
                .map_err(io::Error::other)?
        })
    }
}

// ---- Dialer ---------------------------------------------------------------------------------

/// TLS-PSK to the phone's listener; the stream is wrapped in [`HalfCloseSafe`].
#[derive(Debug, Default)]
pub struct TlsDialer;

impl Dialer for TlsDialer {
    fn dial<'a>(
        &'a self,
        to: SocketAddr,
        identity: &'a str,
        psk: &'a LinkKey,
    ) -> BoxFuture<'a, Result<MuxStream, DialError>> {
        Box::pin(async move {
            // The caller's own limit (DIAL_TIMEOUT for dial and request line together) cuts
            // this short by dropping it; this one bounds a caller that has none.
            let stream = wireless_tls::connect(to, identity, psk.as_bytes(), DIAL_TIMEOUT)
                .await
                .map_err(dial_error)?;
            Ok(Box::new(HalfCloseSafe::new(stream)) as MuxStream)
        })
    }
}

pub fn dial_error(error: TlsError) -> DialError {
    match error {
        TlsError::UnknownIdentity => DialError::UnknownIdentity,
        TlsError::Timeout => DialError::Timeout,
        TlsError::WrongSuite => DialError::Io(error.to_string()),
        TlsError::Io(message) => DialError::Io(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::wireless_tls::CIPHER;
    use crate::windows::wireless_tls::tests::{
        REPLY_LEN, assert_half_close_safe, reply_byte, serve_one, serve_reply, server_context_for,
    };
    use cg_core::wireless::pairing::{Key, link_key};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn access_point_errors_map_to_the_snapshots_causes() {
        let table = [
            (ApError::NoAdapter, ApFailure::NoAdapter),
            (ApError::RadioOff, ApFailure::RadioOff),
            (
                ApError::InUse {
                    mobile_hotspot: true,
                },
                ApFailure::MobileHotspotOn,
            ),
            (
                ApError::InUse {
                    mobile_hotspot: false,
                },
                ApFailure::InUse,
            ),
            (
                ApError::Unsupported("timeout".into()),
                ApFailure::Unsupported,
            ),
            (
                ApError::Unsupported("the publisher aborted (WiFiDirectError 3)".into()),
                ApFailure::Unsupported,
            ),
        ];
        for (error, failure) in table {
            assert_eq!(ap_failure(&error), failure, "{error:?}");
        }
    }

    #[test]
    fn tls_errors_map_to_dial_errors() {
        assert_eq!(
            dial_error(TlsError::UnknownIdentity),
            DialError::UnknownIdentity
        );
        assert_eq!(dial_error(TlsError::Timeout), DialError::Timeout);
        assert!(matches!(dial_error(TlsError::WrongSuite), DialError::Io(_)));
        assert_eq!(
            dial_error(TlsError::Io("connect: refused".into())),
            DialError::Io("connect: refused".into())
        );
    }

    /// Stands in for an access point: records where and how often it was dropped.
    struct Token {
        drops: Arc<AtomicUsize>,
        dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
    }

    impl Drop for Token {
        fn drop(&mut self) {
            *self.dropped_on.lock().unwrap() = Some(std::thread::current().id());
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    type Dropped = (Arc<AtomicUsize>, Arc<Mutex<Option<std::thread::ThreadId>>>);

    fn token() -> (Token, Dropped) {
        let drops = Arc::new(AtomicUsize::new(0));
        let dropped_on = Arc::new(Mutex::new(None));
        (
            Token {
                drops: drops.clone(),
                dropped_on: dropped_on.clone(),
            },
            (drops, dropped_on),
        )
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_start_delivers_what_it_started() {
        let (made, (drops, _)) = token();
        let started = start_off_thread(move || Ok(made)).await;
        assert!(started.is_ok());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(started);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_start_comes_back_as_its_error() {
        let started = start_off_thread::<Token, _>(|| {
            Err(ApError::InUse {
                mobile_hotspot: true,
            })
        })
        .await;
        assert_eq!(
            started.err(),
            Some(ApError::InUse {
                mobile_hotspot: true
            })
        );
    }

    /// The trait's rule: a start that completes after the caller gave up is stopped at once,
    /// on the start's own thread, without anyone holding it.
    #[tokio::test]
    async fn a_start_that_completes_after_the_caller_gave_up_is_dropped_at_once() {
        let (made, (drops, dropped_on)) = token();
        let (began, began_rx) = std::sync::mpsc::channel();
        let (thread_id, thread_rx) = std::sync::mpsc::channel();
        let start = start_off_thread(move || {
            began.send(()).unwrap();
            thread_id.send(std::thread::current().id()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            Ok(made)
        });
        let gave_up = tokio::time::timeout(Duration::from_millis(100), start).await;
        assert!(gave_up.is_err(), "the start was still running");
        began_rx.recv().unwrap();
        until("the late access point to be dropped", || {
            drops.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(*dropped_on.lock().unwrap(), Some(thread_rx.recv().unwrap()));
    }

    #[tokio::test]
    async fn a_drop_inside_the_runtime_happens_on_another_thread() {
        let (made, (drops, dropped_on)) = token();
        drop_off_thread(made);
        until("the drop", || drops.load(Ordering::SeqCst) == 1).await;
        assert_ne!(
            *dropped_on.lock().unwrap(),
            Some(std::thread::current().id())
        );
    }

    #[test]
    fn a_drop_outside_a_runtime_happens_here() {
        let (made, (drops, dropped_on)) = token();
        drop_off_thread(made);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            *dropped_on.lock().unwrap(),
            Some(std::thread::current().id())
        );
    }

    #[tokio::test]
    async fn lost_completes_once_the_publisher_aborted_and_again_at_once() {
        let (sender, receiver) = watch::channel(false);
        let pending =
            tokio::time::timeout(Duration::from_millis(50), wait_lost(receiver.clone())).await;
        assert!(pending.is_err(), "not lost yet");
        sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), wait_lost(receiver.clone()))
            .await
            .expect("lost");
        tokio::time::timeout(Duration::from_millis(50), wait_lost(receiver))
            .await
            .expect("lost again at once");
    }

    #[tokio::test]
    async fn lost_stays_pending_if_the_signal_goes_away_unset() {
        let (sender, receiver) = watch::channel(false);
        drop(sender);
        let waited = tokio::time::timeout(Duration::from_millis(50), wait_lost(receiver)).await;
        assert!(waited.is_err());
    }

    #[test]
    fn the_local_address_is_a_clients_else_the_group_owners_once_assigned() {
        let local: IpAddr = "192.168.137.1".parse().unwrap();
        let phone: IpAddr = "192.168.137.205".parse().unwrap();
        let other_local: IpAddr = "192.168.49.1".parse().unwrap();
        assert_eq!(
            local_address(&[(other_local, phone)], |_| panic!("not asked")),
            Some(other_local)
        );
        assert_eq!(
            local_address(&[], |address| address == ACCESS_POINT_ADDRESS),
            Some(local)
        );
        assert_eq!(local_address(&[], |_| false), None);
    }

    #[tokio::test]
    async fn browsing_an_ipv6_address_is_refused_without_a_query() {
        let error = RawMdns
            .browse("fe80::1".parse().unwrap(), Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

    fn long_term() -> Key {
        Key::from_bytes([7; 32])
    }

    /// The dialer's stream as the session gets it (a `MuxStream`) keeps Review Focus 4.
    #[tokio::test]
    async fn the_dialers_stream_survives_a_half_close() {
        let link = link_key(&long_term());
        let (half_closed, wait) = tokio::sync::oneshot::channel();
        let context = server_context_for(CIPHER, IDENTITY, *link.as_bytes());
        let (address, server) = serve_reply(context, wait).await;
        let mut stream = TlsDialer.dial(address, IDENTITY, &link).await.unwrap();
        stream.write_all(b"CG1 DATA 8088\n").await.unwrap();
        stream.shutdown().await.unwrap();
        half_closed.send(()).unwrap();
        let mut reply = vec![0u8; REPLY_LEN];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut reply))
            .await
            .expect("the reply arrived")
            .unwrap();
        assert!(reply.iter().enumerate().all(|(i, b)| *b == reply_byte(i)));
        drop(stream);
        assert_half_close_safe(&server.await.unwrap(), "CG1 DATA 8088\n");
    }

    #[tokio::test]
    async fn the_dialer_reports_an_unknown_identity() {
        // This server knows only its own test identity.
        let (address, server) = serve_one(CIPHER).await;
        let link = link_key(&long_term());
        let error = TlsDialer.dial(address, IDENTITY, &link).await.err();
        assert_eq!(error, Some(DialError::UnknownIdentity));
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn the_dialer_reports_nothing_listening_as_io() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let link = link_key(&long_term());
        let error = TlsDialer.dial(address, IDENTITY, &link).await.err();
        assert!(matches!(error, Some(DialError::Io(_))), "{error:?}");
    }
}
