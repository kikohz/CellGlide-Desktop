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
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, oneshot, watch};
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

/// Held from the start of an access point until its stop has completed, so a new start never
/// overlaps the previous stop (the controller can start the next attempt while the last access
/// point is still being stopped on its blocking thread).
static ACCESS_POINT_TURN: AsyncMutex<()> = AsyncMutex::const_new(());

/// How long a start waits for the previous access point's stop (a start takes up to 10 s; a
/// stop normally well under one).
const TURN_WAIT: Duration = Duration::from_secs(20);

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
            let started = start_off_thread(&ACCESS_POINT_TURN, move || {
                AccessPoint::start(&ssid, &passphrase)
            })
            .await;
            match started {
                Ok(held) => Ok(Box::new(Running::new(held)) as _),
                Err(error) => {
                    tracing::warn!("access point: not started: {error}");
                    Err(ap_failure(&error))
                }
            }
        })
    }
}

/// A started value (the access point) with its turn: dropping it stops the value on a blocking
/// thread and only then gives the turn back.
struct Held<T: Send + 'static> {
    value: Option<T>,
    turn: Option<MutexGuard<'static, ()>>,
}

impl<T: Send + 'static> Held<T> {
    fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Stops it on this thread (already a blocking one).
    fn stop_here(mut self) {
        drop(self.value.take());
        drop(self.turn.take());
    }
}

impl<T: Send + 'static> Drop for Held<T> {
    fn drop(&mut self) {
        let value = self.value.take();
        let turn = self.turn.take();
        if value.is_some() || turn.is_some() {
            off_thread(move || {
                drop(value);
                drop(turn);
            });
        }
    }
}

/// Takes the turn, then runs `start` on a blocking thread; the turn stays with the result
/// until it has been stopped. If the caller drops this future before the result arrives
/// (cancelled, the service stopping), the result is stopped on that thread as soon as `start`
/// returns; if the caller is already gone when the thread gets to run, `start` is not called.
async fn start_off_thread<T, F>(turn: &'static AsyncMutex<()>, start: F) -> Result<Held<T>, ApError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApError> + Send + 'static,
{
    let Ok(turn) = tokio::time::timeout(TURN_WAIT, turn.lock()).await else {
        return Err(ApError::Unsupported(
            "the previous access point is still being stopped".into(),
        ));
    };
    let (sender, result) = oneshot::channel();
    // Detached: the thread finishes on its own whether or not anyone waits.
    drop(tokio::task::spawn_blocking(move || {
        if sender.is_closed() {
            return;
        }
        let started = start().map(|value| Held {
            value: Some(value),
            turn: Some(turn),
        });
        if let Err(Ok(held)) = sender.send(started) {
            tracing::info!("access point: started after the caller gave up; stopping it");
            held.stop_here();
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
    held: Held<AccessPoint>,
    lost: watch::Receiver<bool>,
}

impl Running {
    fn new(held: Held<AccessPoint>) -> Running {
        let (_, unset) = watch::channel(false);
        Running {
            lost: held.get().map_or(unset, AccessPoint::lost_signal),
            held,
        }
    }
}

impl RunningAccessPoint for Running {
    fn local_address(&self) -> Option<IpAddr> {
        let clients = self
            .held
            .get()
            .map(AccessPoint::clients)
            .unwrap_or_default();
        local_address(&clients, on_wifi_direct_adapter)
    }

    fn clients(&self) -> Vec<IpAddr> {
        self.held
            .get()
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

/// Runs `f` on a blocking thread when called inside a runtime (the runtime waits for it when
/// it shuts down), else right here.
fn off_thread(f: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => drop(runtime.spawn_blocking(f)),
        Err(_) => f(),
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
/// group owner's usual address once the Wi-Fi Direct adapter has it (discovery works before a
/// device was accepted, docs/validation/windows-1.1-gate.md), else not known yet.
fn local_address(
    clients: &[(IpAddr, IpAddr)],
    on_access_point: impl FnOnce(Ipv4Addr) -> bool,
) -> Option<IpAddr> {
    if let Some(&(local, _)) = clients.first() {
        return Some(local);
    }
    on_access_point(ACCESS_POINT_ADDRESS).then_some(IpAddr::V4(ACCESS_POINT_ADDRESS))
}

/// Whether `address` is on the Wi-Fi Direct virtual adapter (the access point's), not on some
/// other adapter that happens to use the same private address.
fn on_wifi_direct_adapter(address: Ipv4Addr) -> bool {
    mdns::adapter_description(address).is_some_and(|description| is_wifi_direct(&description))
}

/// The description Windows gives the access point's adapter ("Microsoft Wi-Fi Direct Virtual
/// Adapter", with " #2" and so on for further instances).
fn is_wifi_direct(description: &str) -> bool {
    description.starts_with("Microsoft Wi-Fi Direct Virtual Adapter")
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

    /// A turn of its own for each test (they run in parallel).
    fn turn() -> &'static AsyncMutex<()> {
        Box::leak(Box::new(AsyncMutex::new(())))
    }

    #[tokio::test]
    async fn a_start_delivers_what_it_started_and_the_drop_stops_it() {
        let (made, (drops, dropped_on)) = token();
        let started = start_off_thread(turn(), move || Ok(made)).await;
        assert!(started.is_ok());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(started);
        until("the stop", || drops.load(Ordering::SeqCst) == 1).await;
        assert_ne!(
            *dropped_on.lock().unwrap(),
            Some(std::thread::current().id())
        );
    }

    #[tokio::test]
    async fn a_failed_start_comes_back_as_its_error_and_gives_the_turn_back() {
        let turn = turn();
        let started = start_off_thread::<Token, _>(turn, || {
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
        let (made, _) = token();
        let again = tokio::time::timeout(
            Duration::from_secs(2),
            start_off_thread(turn, move || Ok(made)),
        )
        .await;
        assert!(matches!(again, Ok(Ok(_))));
    }

    /// M1: the controller may start the next access point while the last one is still being
    /// stopped; the start waits for that stop to finish.
    #[tokio::test]
    async fn a_start_waits_until_the_previous_stop_has_finished() {
        struct SlowStop(Arc<Mutex<Option<std::time::Instant>>>);
        impl Drop for SlowStop {
            fn drop(&mut self) {
                std::thread::sleep(Duration::from_millis(300));
                *self.0.lock().unwrap() = Some(std::time::Instant::now());
            }
        }
        let turn = turn();
        let stopped_at = Arc::new(Mutex::new(None));
        let first = SlowStop(stopped_at.clone());
        let first = start_off_thread(turn, move || Ok(first)).await.unwrap();
        drop(first);
        let started_at = Arc::new(Mutex::new(None));
        let mark = started_at.clone();
        let second = start_off_thread(turn, move || {
            *mark.lock().unwrap() = Some(std::time::Instant::now());
            Ok(())
        })
        .await;
        assert!(second.is_ok());
        let stopped = stopped_at.lock().unwrap().expect("the first was stopped");
        let started = started_at.lock().unwrap().expect("the second was started");
        assert!(
            started >= stopped,
            "the second start overlapped the first stop"
        );
    }

    /// The trait's rule: a start that completes after the caller gave up is stopped at once,
    /// on the start's own thread, without anyone holding it.
    #[tokio::test]
    async fn a_start_that_completes_after_the_caller_gave_up_is_dropped_at_once() {
        let (made, (drops, dropped_on)) = token();
        let (began, began_rx) = std::sync::mpsc::channel();
        let (thread_id, thread_rx) = std::sync::mpsc::channel();
        let start = start_off_thread(turn(), move || {
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
        off_thread(move || drop(made));
        until("the drop", || drops.load(Ordering::SeqCst) == 1).await;
        assert_ne!(
            *dropped_on.lock().unwrap(),
            Some(std::thread::current().id())
        );
    }

    #[test]
    fn a_drop_outside_a_runtime_happens_here() {
        let (made, (drops, dropped_on)) = token();
        off_thread(move || drop(made));
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

    #[test]
    fn only_the_wifi_direct_adapter_counts_as_the_access_points() {
        assert!(is_wifi_direct("Microsoft Wi-Fi Direct Virtual Adapter"));
        assert!(is_wifi_direct("Microsoft Wi-Fi Direct Virtual Adapter #2"));
        assert!(!is_wifi_direct("Intel(R) Wi-Fi 6 AX201 160MHz"));
        assert!(!is_wifi_direct("Hyper-V Virtual Ethernet Adapter"));
        assert!(!is_wifi_direct(""));
    }

    /// Whatever adapters this machine has, the loopback address is not on the access point's.
    #[test]
    fn loopback_is_not_on_the_wifi_direct_adapter() {
        assert!(!on_wifi_direct_adapter(Ipv4Addr::LOCALHOST));
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
