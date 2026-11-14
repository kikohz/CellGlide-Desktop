//! Wireless sessions with a paired phone (Plan B Task 4): `FakeWireless` plays the access
//! point, discovery and the TLS dialer; behind a successful dial is the rig's `FakePhone`.
//!
//! Tests with a session run in real time on small windows (`timing()`). Tests without any
//! TCP in flight (no usbmuxd, no phone) run on paused time with the real windows.

use super::*;
use crate::testing::{
    CableBehaviour, DialMatch, DialOutcome, FakeWireless, Rounds, WirelessPhone, missing_mux,
};
use crate::wireless::pairing::Key;
use crate::wireless::ports::{ApFailure, DialError};
use crate::wireless::store::{Method, PlainSecrets, Record, WirelessStore};
use cg_ipc::wireless::{WirelessFailure, WirelessPhaseView};
use std::net::{IpAddr, Ipv4Addr};

const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";
const SELECTED: &str = "wireless:4961212D-61DD-4BD0-9FE9-8989FB6B8952";

/// No usbmuxd, failing at once: no I/O stays in flight (a closed loopback port takes about
/// 2 s to refuse on Windows; port 0 is refused immediately).
fn no_usbmuxd() -> MuxEndpoint {
    #[cfg(unix)]
    {
        MuxEndpoint::Unix("/nonexistent/usbmuxd".into())
    }
    #[cfg(not(unix))]
    {
        MuxEndpoint::Tcp("127.0.0.1:0".parse().unwrap())
    }
}

fn at(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 137, last))
}

fn long_term() -> Key {
    Key::from_bytes([7; 32])
}

fn record() -> Record {
    Record {
        peer_id: PHONE_ID.into(),
        name: "Test iPhone".into(),
        identity: IDENTITY.into(),
        created_at: 1,
        method: Method::Cable,
        stale: false,
    }
}

/// Small windows for tests in real time.
fn timing() -> WirelessTiming {
    WirelessTiming {
        wait_for_phone: Duration::from_secs(5),
        retry_every: Duration::from_millis(50),
        retry_for: Duration::from_secs(3),
        browse: Duration::from_millis(10),
        dial_timeout: Duration::from_millis(300),
        clash_rounds: 3,
        usb_probe_every: Duration::from_secs(10),
    }
}

/// The wireless tier with its checks out of the way.
fn quiet_watch() -> HealthWatchTiming {
    HealthWatchTiming {
        channel: Duration::from_secs(60),
        tunnel: Duration::from_secs(60),
        fast: Duration::from_secs(60),
        ..HealthWatchTiming::wireless()
    }
}

struct W {
    r: Rig,
    fake: Arc<FakeWireless>,
    store: Arc<WirelessStore>,
    _dir: tempfile::TempDir,
}

impl W {
    /// The paired phone at .205, serving the rig's fake phone.
    fn phone(&self) -> WirelessPhone {
        WirelessPhone::new(PHONE_ID, at(205))
            .serving(&self.r.mux)
            .paired(IDENTITY, &long_term())
    }

    fn add_phone(&self, rounds: Rounds) {
        self.fake.add_phone(self.phone(), rounds);
    }

    async fn start(&self) {
        self.r
            .controller
            .send(Command::ConnectWireless)
            .await
            .unwrap();
    }

    async fn connected(&self) -> Snapshot {
        wait_for(&self.r.controller, "connected wirelessly", on_air).await
    }
}

fn open_store(dir: &std::path::Path) -> Arc<WirelessStore> {
    let store = Arc::new(WirelessStore::open(dir, Arc::new(PlainSecrets)).unwrap());
    store.save(record(), long_term()).unwrap();
    store
}

/// A rig with the switch on, auto connect off, a store holding the phone's pairing and
/// `FakeWireless`; `adjust` goes last.
async fn wireless_rig_with(
    options: FakePhoneOptions,
    adjust: impl FnOnce(&mut ControllerConfig),
) -> W {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let fake = FakeWireless::new();
    fake.set_browse_delay(Duration::from_millis(10));
    let (shared, ports) = (store.clone(), fake.ports());
    let r = rig_config(options, any_port(), quiet_timing(), |config| {
        config.settings = SettingsSource::Memory(Stored {
            auto_connect: false,
            wireless_enabled: true,
            ..Stored::default()
        });
        config.wireless = Some(shared);
        config.wireless_ports = Some(ports);
        config.wireless_timing = timing();
        config.wireless_health = quiet_timing();
        config.wireless_health_watch = quiet_watch();
        adjust(config);
    })
    .await;
    W {
        r,
        fake,
        store,
        _dir: dir,
    }
}

async fn wireless_rig() -> W {
    wireless_rig_with(FakePhoneOptions::default(), |_| {}).await
}

/// A rig whose phone is found in every round and that is connected.
async fn connected_rig(adjust: impl FnOnce(&mut ControllerConfig)) -> W {
    let w = wireless_rig_with(FakePhoneOptions::default(), adjust).await;
    w.add_phone(Rounds::all());
    w.start().await;
    w.connected().await;
    w
}

fn phase(s: &Snapshot) -> Option<WirelessPhaseView> {
    s.wireless.as_ref().map(|w| w.phase.clone())
}

fn on_air(s: &Snapshot) -> bool {
    connected(s) && phase(s) == Some(WirelessPhaseView::Connected)
}

fn failed(s: &Snapshot, reason: WirelessFailure) -> bool {
    phase(s) == Some(WirelessPhaseView::Failed { reason })
}

async fn wait_within(
    c: &Controller,
    what: &str,
    within: Duration,
    pred: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    let mut rx = c.subscribe();
    let found = tokio::time::timeout(within, async {
        loop {
            let snap = rx.borrow_and_update().clone();
            if pred(&snap) {
                return snap;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("timed out waiting for {what}; last: {:?}", c.snapshot()))
}

fn bad_request(result: Result<()>) -> bool {
    matches!(result, Err(CoreError::BadRequest))
}

// --- Connecting.

#[tokio::test]
async fn a_paired_phone_connects_and_disconnect_closes_the_access_point() {
    let w = wireless_rig().await;
    w.add_phone(Rounds::all());
    w.start().await;
    let snap = w.connected().await;
    assert_eq!(snap.selected.as_deref(), Some(SELECTED));
    assert_eq!(
        snap.state,
        ConnState::Connected {
            phone_version: "2.2.0".into(),
            dns: DnsMode::Phone
        }
    );
    let status = snap.wireless.clone().unwrap();
    let (ssid, _) = w.store.access_point().unwrap();
    assert_eq!(status.ssid.as_deref(), Some(ssid.as_str()));
    assert!(!status.weak && !status.name_clash);
    assert!(snap.socks_port.is_some());
    assert_eq!(w.r.backend.started(), 1, "the tunnel is up");
    assert!(w.fake.is_up());
    assert_eq!(w.fake.started_with(), [w.store.access_point().unwrap()]);
    assert_eq!(w.fake.browsed_on()[0], crate::testing::ACCESS_POINT_ADDRESS);
    let dials = w.fake.dials();
    assert!(dials.iter().all(|d| d.identity == IDENTITY));
    assert!(
        dials
            .iter()
            .all(|d| d.to == SocketAddr::new(at(205), 56582))
    );
    // The snapshot never carries the passphrase.
    let json = String::from_utf8(cg_ipc::to_line(&snap)).unwrap();
    assert!(!json.contains(&w.store.access_point().unwrap().1));

    w.r.controller.send(Command::Disconnect).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.fake.stops(), 1);
    assert_eq!(w.r.backend.stopped(), 1);
    let snap = w.r.controller.snapshot();
    assert_eq!(snap.wireless, None);
    assert!(!connected(&snap));
}

/// Review Focus 1: without usbmuxd (Apple Devices missing or stopped) the attempt starts and
/// holds, and is never reported as "no USB service".
#[tokio::test]
async fn without_usbmuxd_it_connects_and_never_reports_no_usb_service() {
    let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.endpoint = no_usbmuxd();
    })
    .await;
    wait_for(&w.r.controller, "no usb service", |s| {
        s.state == ConnState::NoUsbService
    })
    .await;
    w.add_phone(Rounds::all());
    let mut rx = w.r.controller.subscribe();
    w.start().await;
    let mut states = Vec::new();
    let reached = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snap = rx.borrow_and_update().clone();
            states.push(snap.state.clone());
            if on_air(&snap) {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert!(reached.is_ok(), "not connected: {states:?}");
    assert!(!states.contains(&ConnState::NoUsbService), "{states:?}");
    assert!(w.fake.is_up());
}

/// Review Focus 2: usbmuxd going away does not end a wireless session.
#[tokio::test]
async fn usbmuxd_going_away_leaves_the_wireless_session_up() {
    let w = wireless_rig().await;
    eventually("usbmuxd joined", Duration::from_secs(5), || {
        w.r.mux.listener_count() == 1
    })
    .await;
    w.add_phone(Rounds::all());
    w.start().await;
    w.connected().await;
    w.r.mux.drop_listeners();
    // Listening again means the loss was handled.
    eventually("usbmuxd rejoined", Duration::from_secs(5), || {
        w.r.mux.listener_count() == 1
    })
    .await;
    assert!(on_air(&w.r.controller.snapshot()));
    assert_eq!(w.r.backend.stopped(), 0);
    assert!(w.fake.is_up());
}

/// Discovery misses rounds on hardware: a phone first seen in round 5 is connected, and three
/// rounds without it after the session ended do not end the attempt.
#[tokio::test]
async fn a_phone_found_late_and_then_missed_is_still_connected() {
    let w = wireless_rig().await;
    w.add_phone(Rounds::starting(5).except([6, 7, 8]));
    w.start().await;
    w.connected().await;
    assert_eq!(w.fake.browse_count(), 6, "found in round 5");
    w.r.phone.end_session();
    wait_for(&w.r.controller, "reconnected", |s| {
        on_air(s) && w.r.backend.started() == 2
    })
    .await;
    assert_eq!(w.fake.browse_count(), 10, "rounds 6 to 8 found nothing");
    assert_eq!(w.fake.start_attempts(), 1);
    assert!(w.fake.is_up());
}

/// A session that ends (the phone stopped sharing): the tunnel goes first, then the attempt
/// reconnects on the same access point after `retry_every`.
#[tokio::test]
async fn a_session_that_ends_is_torn_down_then_rebuilt_on_the_same_access_point() {
    let w = connected_rig(|_| {}).await;
    let ended = tokio::time::Instant::now();
    w.r.phone.end_session();
    let snap = wait_for(&w.r.controller, "reconnecting", |s| {
        phase(s) == Some(WirelessPhaseView::Reconnecting)
    })
    .await;
    assert_eq!(w.r.backend.stopped(), 1, "the tunnel is down first");
    assert!(!connected(&snap));
    assert_eq!(snap.state, ConnState::Connecting);
    wait_for(&w.r.controller, "reconnected", |s| {
        on_air(s) && w.r.backend.started() == 2
    })
    .await;
    assert!(ended.elapsed() >= timing().retry_every);
    assert_eq!(w.fake.start_attempts(), 1, "one access point throughout");
    assert_eq!(w.fake.stops(), 0);
    assert!(w.fake.is_up());
}

#[tokio::test]
async fn reconnecting_gives_up_after_its_window() {
    let w = connected_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(1);
    })
    .await;
    // The phone hangs every dial from now on (as one walked out of range).
    w.fake
        .script_dials(DialMatch::address(at(205)), vec![DialOutcome::Hang; 100]);
    w.r.phone.end_session();
    wait_for(&w.r.controller, "lost", |s| {
        failed(s, WirelessFailure::Lost)
    })
    .await;
    assert!(!w.fake.is_up());
    assert_eq!(w.fake.stops(), 1);
    assert_eq!(w.r.backend.started(), 1);
    assert_eq!(w.r.backend.stopped(), 1);
    assert!(!connected(&w.r.controller.snapshot()));
}

/// The phone, at the address where it answered this pairing, refuses it for 5 s and more: the
/// pairing is marked stale (and kept), the attempt ends and nothing reconnects.
#[tokio::test]
async fn refusals_at_a_verified_address_mark_the_pairing_stale_and_end() {
    let w = connected_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(30);
    })
    .await;
    w.fake.script_dials(
        DialMatch::address(at(205)),
        vec![DialOutcome::Fail(DialError::UnknownIdentity); 1000],
    );
    w.r.phone.end_session();
    wait_within(&w.r.controller, "stale", Duration::from_secs(20), |s| {
        failed(s, WirelessFailure::PairingStale)
    })
    .await;
    let records = w.store.records();
    assert_eq!(records.len(), 1, "kept");
    assert!(records[0].stale);
    assert!(w.store.key(IDENTITY).is_some(), "the key is kept too");
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.started(), 1, "no reconnect");
}

/// Review Focus 6: something on the access point advertising the phone's name and refusing
/// the pairing, with the phone itself absent, never marks the pairing stale.
#[tokio::test]
async fn refusals_from_an_unverified_address_end_as_refused_and_leave_the_pairing() {
    let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    w.fake.add_impostor(PHONE_ID, at(66), Rounds::all());
    w.start().await;
    wait_for(&w.r.controller, "refused", |s| {
        failed(s, WirelessFailure::PairingRefused)
    })
    .await;
    assert!(!w.store.records()[0].stale);
    assert!(!w.fake.dials().is_empty());
    assert!(w.fake.dials().iter().all(|d| d.to.ip() == at(66)));
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.started(), 0);
}

/// The phone's name at two addresses: nothing is dialled, the clash is shown after a few
/// rounds, and the attempt ends as a clash.
#[tokio::test]
async fn a_name_at_two_addresses_is_shown_and_ends_as_a_clash() {
    let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    w.add_phone(Rounds::all());
    w.fake.add_impostor(PHONE_ID, at(66), Rounds::all());
    w.start().await;
    let snap = wait_for(&w.r.controller, "a clash", |s| {
        s.wireless.as_ref().is_some_and(|w| w.name_clash)
    })
    .await;
    assert_eq!(phase(&snap), Some(WirelessPhaseView::Waiting));
    assert!(w.fake.browse_count() >= 3);
    wait_for(&w.r.controller, "clash", |s| {
        failed(s, WirelessFailure::NameClash)
    })
    .await;
    assert!(w.fake.dials().is_empty(), "never dialled");
    assert!(!w.store.records()[0].stale);
    assert!(!w.fake.is_up());
}

#[tokio::test]
async fn a_lost_access_point_ends_the_session() {
    let w = connected_rig(|_| {}).await;
    w.fake.lose_access_point();
    wait_for(&w.r.controller, "lost", |s| {
        failed(s, WirelessFailure::Lost)
    })
    .await;
    assert!(!w.fake.is_up(), "dropped");
    assert_eq!(w.r.backend.stopped(), 1);
    assert_eq!(w.r.backend.started(), 1, "not reconnected");
    assert!(!connected(&w.r.controller.snapshot()));
}

// --- What USB, the user and the computer do meanwhile.

/// Review Focus 2: an iPhone on USB takes over, even with auto connect off.
#[tokio::test]
async fn an_iphone_on_usb_takes_over_even_with_auto_connect_off() {
    let w = connected_rig(|_| {}).await;
    w.r.mux.attach(device("aaaa", 1));
    let snap = wait_for(&w.r.controller, "on usb", |s| {
        connected(s) && s.selected.as_deref() == Some("aaaa")
    })
    .await;
    assert_eq!(snap.wireless, None);
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.started(), 2);
    assert_eq!(w.r.backend.stopped(), 1);
    assert!(!snap.settings.auto_connect);
}

#[tokio::test]
async fn suspend_ends_it_and_resume_does_not_start_it_again() {
    let w = connected_rig(|_| {}).await;
    w.r.controller
        .send(Command::Sleep { sleeping: true })
        .await
        .unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.stopped(), 1);
    assert_eq!(w.r.controller.snapshot().wireless, None);
    w.r.controller
        .send(Command::Sleep { sleeping: false })
        .await
        .unwrap();
    // Nothing to wait on for "nothing starts": let a few ticks (250 ms each) pass.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let snap = w.r.controller.snapshot();
    assert_eq!(snap.wireless, None);
    assert!(!connected(&snap));
    assert_eq!(w.fake.start_attempts(), 1);
}

#[tokio::test]
async fn connect_wireless_needs_the_switch_and_turning_it_off_ends_it() {
    let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.settings = SettingsSource::Memory(Stored {
            auto_connect: false,
            wireless_enabled: false,
            ..Stored::default()
        });
    })
    .await;
    w.add_phone(Rounds::all());
    assert!(bad_request(
        w.r.controller.send(Command::ConnectWireless).await
    ));
    assert_eq!(w.fake.start_attempts(), 0);
    assert_eq!(w.r.controller.snapshot().wireless, None);

    w.r.controller
        .send(Command::SetWireless { enabled: true })
        .await
        .unwrap();
    w.start().await;
    w.connected().await;
    w.r.controller
        .send(Command::SetWireless { enabled: false })
        .await
        .unwrap();
    let snap = w.r.controller.snapshot();
    assert!(failed(&snap, WirelessFailure::TurnedOff), "{snap:?}");
    assert!(!connected(&snap));
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.stopped(), 1);
}

#[tokio::test]
async fn a_usb_session_refuses_connect_wireless_and_stays() {
    let w = wireless_rig().await;
    w.add_phone(Rounds::all());
    connect_phone(&w.r).await;
    assert!(bad_request(
        w.r.controller.send(Command::ConnectWireless).await
    ));
    assert_eq!(w.fake.start_attempts(), 0);
    let snap = w.r.controller.snapshot();
    assert!(connected(&snap));
    assert_eq!(snap.selected.as_deref(), Some("aaaa"));
    assert_eq!(snap.wireless, None);
    assert_eq!(w.r.backend.stopped(), 0);
}

#[tokio::test]
async fn connect_wireless_is_refused_with_an_iphone_on_usb_or_without_the_platform() {
    let w = wireless_rig().await;
    w.r.mux.attach(device("aaaa", 1));
    wait_for(&w.r.controller, "idle", |s| s.state == ConnState::Idle).await;
    assert!(bad_request(
        w.r.controller.send(Command::ConnectWireless).await
    ));
    assert_eq!(w.fake.start_attempts(), 0);

    for (ports, store) in [(false, true), (true, false)] {
        let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
            if !ports {
                config.wireless_ports = None;
            }
            if !store {
                config.wireless = None;
            }
        })
        .await;
        assert!(bad_request(
            w.r.controller.send(Command::ConnectWireless).await
        ));
        assert_eq!(w.fake.start_attempts(), 0);
        assert_eq!(w.r.controller.snapshot().wireless, None);
    }
}

/// A client echoing `selected` back (`wireless:<id>`) neither tears the session down nor makes
/// the wireless phone a USB choice.
#[tokio::test]
async fn echoing_the_selected_phone_back_changes_nothing() {
    let w = connected_rig(|_| {}).await;
    assert!(bad_request(
        w.r.controller
            .send(Command::Connect {
                serial: Some(SELECTED.into())
            })
            .await
    ));
    w.r.controller
        .send(Command::SetSettings {
            auto_connect: None,
            phone: Some(PhoneChoice::Serial {
                serial: SELECTED.into(),
            }),
            strict_dns: None,
        })
        .await
        .unwrap();
    let snap = w.r.controller.snapshot();
    assert!(on_air(&snap));
    assert_eq!(snap.settings.chosen_serial, None);
    assert_eq!(w.r.backend.stopped(), 0);
    assert!(w.fake.is_up());
}

#[tokio::test]
async fn cancel_while_waiting_closes_the_access_point() {
    let w = wireless_rig().await;
    w.start().await;
    wait_for(&w.r.controller, "waiting", |s| {
        phase(s) == Some(WirelessPhaseView::Waiting)
    })
    .await;
    assert!(w.fake.is_up());
    // Asking again while it runs changes nothing.
    w.start().await;
    assert_eq!(w.fake.start_attempts(), 1);
    w.r.controller.send(Command::CancelWireless).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.r.controller.snapshot().wireless, None);
}

#[tokio::test]
async fn shutdown_closes_the_access_point() {
    let w = connected_rig(|_| {}).await;
    w.r.controller.shutdown().await;
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.stopped(), 1);
}

/// Review Focus 3 for the task itself: aborted while the access point is up, it drops it.
#[tokio::test]
async fn the_attempt_aborted_drops_the_access_point() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWireless::new();
    let (events, mut received) = mpsc::channel(64);
    let task = tokio::spawn(super::super::wireless_session::run(
        super::super::wireless_session::Context {
            attempt: 1,
            ports: fake.ports(),
            store: open_store(dir.path()),
            timing: timing(),
            events,
        },
    ));
    loop {
        match received.recv().await {
            Some(Event::Wireless(1, WirelessEvent::Phase(WirelessPhase::Waiting { .. }))) => break,
            Some(_) => {}
            None => panic!("the task ended"),
        }
    }
    assert!(fake.is_up());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!fake.is_up());
    assert_eq!(fake.stops(), 1);
}

// --- What the session is.

/// No cable pairing (8092) and no lockdown over the wireless link.
#[tokio::test]
async fn a_wireless_session_opens_neither_8092_nor_lockdown() {
    let options = FakePhoneOptions {
        cable: CableBehaviour::Reply("ignored".into()),
        ..lockdown_phone(true, false)
    };
    let w = wireless_rig_with(options, |_| {}).await;
    w.r.mux
        .add_pair_record_with("aaaa", crate::testing::fixture_pair_record());
    w.add_phone(Rounds::all());
    w.start().await;
    w.connected().await;
    // Metrics arrive: the session's tasks are all running.
    let metrics = r#"{"service":"CellGlide-metrics","version":1,"sharing":true,"download":2048,"upload":1024}"#;
    let mut rx = w.r.controller.subscribe();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            w.r.phone.send_metrics(metrics);
            tokio::time::sleep(Duration::from_millis(100)).await;
            if rx
                .borrow_and_update()
                .phone
                .as_ref()
                .is_some_and(|c| c.frame.is_some())
            {
                return;
            }
        }
    })
    .await
    .expect("a phone frame");
    assert_eq!(w.r.phone.cable_connections(), 0);
    assert_eq!(w.r.phone.lockdown_connections(), 0);
    assert_eq!(w.r.phone.relay_requests(), 0);
    assert_eq!(w.r.controller.snapshot().phone.unwrap().usb, None);
    let ports: Vec<_> = w
        .fake
        .dials()
        .iter()
        .filter_map(|d| d.request_line.clone())
        .collect();
    assert!(
        ports.iter().all(|line| crate::wireless::ALLOWED_PORTS
            .iter()
            .any(|p| *line == format!("CG1 DATA {p}"))),
        "{ports:?}"
    );
}

/// The wireless tier: handshakes are timed and their median makes the link weak; the
/// timeline marks the session as wireless.
#[tokio::test]
async fn the_wireless_tier_reports_a_weak_link_and_the_timeline_marks_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let health = dir.path().join("health");
    let w = connected_rig(|config| {
        config.health_dir = Some(health);
        config.time_zone = TimeZone::UTC;
        config.wireless_health_watch = HealthWatchTiming {
            channel: Duration::from_millis(30),
            // Any handshake is above this.
            weak_median: Some(Duration::ZERO),
            ..quiet_watch()
        };
    })
    .await;
    wait_for(&w.r.controller, "weak", |s| {
        s.wireless.as_ref().is_some_and(|w| w.weak)
    })
    .await;
    let started: Vec<_> =
        w.r.controller
            .history(None)
            .events
            .into_iter()
            .filter(|e| e.kind == EventKind::TunnelStarted)
            .map(|e| e.detail)
            .collect();
    assert_eq!(started, [Some(format!("session {SESSION_A}, wireless"))]);
}

#[test]
fn the_defaults_are_the_policy_and_the_wireless_tier() {
    let config = ControllerConfig::new(missing_mux(), Arc::new(SharedFake(FakeBackend::new())));
    assert_eq!(
        config.wireless_timing,
        WirelessTiming {
            wait_for_phone: Duration::from_secs(300),
            retry_every: Duration::from_secs(2),
            retry_for: Duration::from_secs(60),
            browse: Duration::from_secs(2),
            dial_timeout: Duration::from_secs(4),
            clash_rounds: 3,
            usb_probe_every: Duration::from_secs(10),
        }
    );
    assert_eq!(
        config.wireless_health.quick_fail_after,
        HealthTiming::wireless().quick_fail_after
    );
    assert_eq!(
        config.wireless_health.quick_timeout,
        HealthTiming::wireless().quick_timeout
    );
    assert_eq!(config.wireless_health_watch, HealthWatchTiming::wireless());
    assert_eq!(config.health_watch, HealthWatchTiming::default());
}

// --- Paused time: no usbmuxd and no phone, so no TCP is in flight.

struct Paused {
    controller: Controller,
    fake: Arc<FakeWireless>,
    _dir: tempfile::TempDir,
}

/// Real windows, and no usbmuxd.
fn paused_rig() -> Paused {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWireless::new();
    let mut config = ControllerConfig::new(no_usbmuxd(), Arc::new(SharedFake(FakeBackend::new())));
    config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
    config.tunnel_probe = FakeTunnelProbe::new();
    config.health = quiet_timing();
    config.settings = SettingsSource::Memory(Stored {
        auto_connect: false,
        wireless_enabled: true,
        ..Stored::default()
    });
    config.wireless = Some(open_store(dir.path()));
    config.wireless_ports = Some(fake.ports());
    Paused {
        controller: Controller::spawn(config),
        fake,
        _dir: dir,
    }
}

/// Waits on paused time, bounded by real time (the clock jumps whenever nothing is ready).
async fn paused_wait(c: &Controller, what: &str, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let started = std::time::Instant::now();
    let mut rx = c.subscribe();
    loop {
        let snap = rx.borrow_and_update().clone();
        if pred(&snap) {
            return snap;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}; last: {snap:?}"
        );
        tokio::select! {
            _ = rx.changed() => {}
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn no_phone_for_300_seconds_fails_and_closes_the_access_point() {
    let p = paused_rig();
    let asked = tokio::time::Instant::now();
    p.controller.send(Command::ConnectWireless).await.unwrap();
    paused_wait(&p.controller, "waiting", |s| {
        phase(s) == Some(WirelessPhaseView::Waiting)
    })
    .await;
    assert!(p.fake.is_up());
    paused_wait(&p.controller, "no phone", |s| {
        failed(s, WirelessFailure::NoPhone)
    })
    .await;
    assert!(asked.elapsed() >= Duration::from_secs(300));
    assert!(!p.fake.is_up());
    assert_eq!(p.fake.starts(), 1);
    assert!(p.fake.browse_count() >= 140, "a round every 2 s");
}

#[tokio::test(start_paused = true)]
async fn each_access_point_failure_is_reported() {
    for cause in [
        ApFailure::NoAdapter,
        ApFailure::RadioOff,
        ApFailure::MobileHotspotOn,
        ApFailure::InUse,
        ApFailure::Unsupported,
    ] {
        let p = paused_rig();
        p.fake.fail_starts(Some(cause));
        p.controller.send(Command::ConnectWireless).await.unwrap();
        let snap = paused_wait(&p.controller, "the failure", |s| {
            matches!(phase(s), Some(WirelessPhaseView::Failed { .. }))
        })
        .await;
        assert!(
            failed(&snap, WirelessFailure::AccessPoint { cause }),
            "{cause:?}: {snap:?}"
        );
        assert_eq!(p.fake.start_attempts(), 1);
        assert!(!p.fake.is_up());
        assert_eq!(p.fake.browse_count(), 0);
        // A failure is shown until the user moves on.
        p.controller.send(Command::CancelWireless).await.unwrap();
        assert_eq!(p.controller.snapshot().wireless, None);
    }
}

#[tokio::test(start_paused = true)]
async fn a_lost_access_point_while_waiting_fails_the_attempt() {
    let p = paused_rig();
    p.controller.send(Command::ConnectWireless).await.unwrap();
    paused_wait(&p.controller, "waiting", |s| {
        phase(s) == Some(WirelessPhaseView::Waiting)
    })
    .await;
    p.fake.lose_access_point();
    paused_wait(&p.controller, "lost", |s| failed(s, WirelessFailure::Lost)).await;
    assert!(!p.fake.is_up());
}

// --- Fix round 1.

/// Where the USB device's hello goes instead of the fake phone: every connection is counted
/// and closed at once, as a device whose phone app does not share (an iPad, an iPhone only
/// charging).
async fn not_sharing() -> (SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(stream);
        }
    });
    (addr, count)
}

fn probes(count: &std::sync::atomic::AtomicUsize) -> usize {
    count.load(std::sync::atomic::Ordering::SeqCst)
}

/// I1: a device on USB whose phone app does not answer leaves the wireless session alone, is
/// asked again only after it appears again, and then not before `usb_probe_every`.
#[tokio::test]
async fn a_usb_device_that_does_not_share_leaves_the_wireless_session_up() {
    let w = connected_rig(|config| {
        config.wireless_timing.usb_probe_every = Duration::from_secs(1);
    })
    .await;
    // The wireless phone keeps the routes it was given; only USB's hello goes nowhere.
    let (addr, count) = not_sharing().await;
    w.r.mux.route(cg_protocol::phone::port::CONTROL, addr);
    w.r.mux.attach(device("aaaa", 1));
    eventually("the first probe", Duration::from_secs(5), || {
        probes(&count) == 1
    })
    .await;
    let asked = std::time::Instant::now();
    w.r.mux.detach(1);
    w.r.mux.attach(device("aaaa", 2));
    eventually("the second probe", Duration::from_secs(5), || {
        probes(&count) == 2
    })
    .await;
    assert!(
        asked.elapsed() >= Duration::from_millis(900),
        "not before usb_probe_every: {:?}",
        asked.elapsed()
    );
    // Still there, not attached again: not asked a third time.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(probes(&count), 2);
    let snap = w.r.controller.snapshot();
    assert!(on_air(&snap), "{snap:?}");
    assert_eq!(snap.devices, ["aaaa"]);
    assert!(w.fake.is_up());
    assert_eq!(w.r.backend.started(), 1);
    assert_eq!(w.r.backend.stopped(), 0);
}

/// I1: only a USB phone that shares blocks `ConnectWireless`; one that doesn't, or two phones
/// with none chosen, do not.
#[tokio::test]
async fn connect_wireless_is_allowed_beside_a_usb_device_that_does_not_share() {
    let w = wireless_rig().await;
    w.add_phone(Rounds::all());
    let (addr, count) = not_sharing().await;
    w.r.mux.route(cg_protocol::phone::port::CONTROL, addr);
    w.r.mux.attach(device("aaaa", 1));
    wait_for(&w.r.controller, "idle", |s| s.state == ConnState::Idle).await;
    w.start().await;
    assert_eq!(probes(&count), 1, "asked once, inline");
    w.connected().await;
    assert!(w.fake.is_up());

    // Two sharing phones, none chosen: wireless may start too.
    let w = wireless_rig().await;
    w.add_phone(Rounds::all());
    w.r.mux.attach(device("aaaa", 1));
    w.r.mux.attach(device("bbbb", 2));
    wait_for(&w.r.controller, "two phones", |s| s.devices.len() == 2).await;
    w.start().await;
    w.connected().await;
}

/// I2: a phone whose app is too old ends the attempt at once.
#[tokio::test]
async fn a_phone_too_old_ends_the_attempt_at_once() {
    let options = FakePhoneOptions {
        hello: hello_json(&["socks5-tcp", "session-watch-v1"], true, SESSION_A),
        ..FakePhoneOptions::default()
    };
    let w = wireless_rig_with(options, |_| {}).await;
    w.add_phone(Rounds::all());
    w.start().await;
    wait_for(&w.r.controller, "refused", |s| {
        failed(
            s,
            WirelessFailure::PhoneRefused {
                reason: Reason::PhoneTooOld,
            },
        )
    })
    .await;
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.started(), 0);
    assert_eq!(w.fake.dials().len(), 2, "the check and the hello, once");
}

/// I2: an error that may heal is tried again, and shown while it lasts.
#[tokio::test]
async fn other_errors_keep_trying_and_show_the_last_reason() {
    let w = wireless_rig().await;
    w.r.probe.set_ok(false);
    w.add_phone(Rounds::all());
    w.start().await;
    let snap = wait_for(&w.r.controller, "the reason", |s| {
        s.wireless
            .as_ref()
            .is_some_and(|w| w.last_error == Some(Reason::NoExit))
    })
    .await;
    assert!(phase(&snap).is_some_and(|p| p != WirelessPhaseView::Connected));
    w.r.probe.set_ok(true);
    let snap = w.connected().await;
    assert_eq!(snap.wireless.unwrap().last_error, None);
    assert_eq!(w.fake.start_attempts(), 1);
}

/// M1: on a multi-threaded runtime (the service's) the access point is down when
/// `CancelWireless` returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_returns_with_the_access_point_down_on_a_threaded_runtime() {
    let w = connected_rig(|_| {}).await;
    w.r.controller.send(Command::CancelWireless).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.fake.stops(), 1);
    assert_eq!(w.r.backend.stopped(), 1);
}

/// M3 (i): after a verified session dropped, a device elsewhere advertising the phone's name
/// and refusing the pairing ends the reconnect as refused; the pairing stays good.
#[tokio::test]
async fn an_impostor_elsewhere_after_the_session_dropped_is_refused_not_stale() {
    let w = connected_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(1);
    })
    .await;
    w.fake.stop_advertising(PHONE_ID, at(205));
    w.fake.add_impostor(PHONE_ID, at(66), Rounds::all());
    w.fake.phone_leaves(at(205));
    wait_for(&w.r.controller, "refused", |s| {
        failed(s, WirelessFailure::PairingRefused)
    })
    .await;
    assert!(!w.store.records()[0].stale);
    assert!(w.fake.dials().iter().any(|d| d.to.ip() == at(66)));
    assert!(!w.fake.is_up());
}

/// M3 (ii): cancelling while the pairing is being checked.
#[tokio::test]
async fn cancel_while_dialing_closes_the_access_point() {
    let w = wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.wireless_timing.dial_timeout = Duration::from_secs(30);
    })
    .await;
    w.fake.script_dials(DialMatch::any(), [DialOutcome::Hang]);
    w.add_phone(Rounds::all());
    w.start().await;
    wait_for(&w.r.controller, "dialing", |s| {
        phase(s) == Some(WirelessPhaseView::Connecting)
    })
    .await;
    w.r.controller.send(Command::CancelWireless).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.r.controller.snapshot().wireless, None);
    assert_eq!(w.r.backend.started(), 0);
}

/// M3 (ii): cancelling while the actor builds the session: the session it just built goes too.
#[tokio::test]
async fn cancel_while_the_session_is_built_ends_both() {
    let w = wireless_rig().await;
    w.r.probe.set_delay(Duration::from_millis(500));
    w.add_phone(Rounds::all());
    w.start().await;
    eventually("building", Duration::from_secs(5), || {
        w.r.probe.calls() >= 1
    })
    .await;
    w.r.controller.send(Command::CancelWireless).await.unwrap();
    assert!(!w.fake.is_up());
    let snap = w.r.controller.snapshot();
    assert_eq!(snap.wireless, None);
    assert!(!connected(&snap));
    assert_eq!(w.r.backend.started(), w.r.backend.stopped());
}

/// M3 (iii): the switch turned off while reconnecting.
#[tokio::test]
async fn turning_the_switch_off_while_reconnecting_ends_it() {
    let w = connected_rig(|_| {}).await;
    w.fake
        .script_dials(DialMatch::address(at(205)), vec![DialOutcome::Hang; 100]);
    w.r.phone.end_session();
    wait_for(&w.r.controller, "reconnecting", |s| {
        phase(s) == Some(WirelessPhaseView::Reconnecting)
    })
    .await;
    w.r.controller
        .send(Command::SetWireless { enabled: false })
        .await
        .unwrap();
    assert!(failed(
        &w.r.controller.snapshot(),
        WirelessFailure::TurnedOff
    ));
    assert!(!w.fake.is_up());
    assert_eq!(w.r.backend.stopped(), 1);
}

// --- Code pairing (Plan B Task 5).

use crate::testing::{PAIRED_IDENTITY, PHONE_NAME, PairingAnswer, PhonePairing};
use crate::wireless::pairing::PAIRING_IDENTITY;

/// A phone this computer has no pairing with.
const NEW_ID: &str = "0A1B2C3D-4E5F-4061-8273-8495A6B7C8D9";
const OTHER_ID: &str = "1B2C3D4E-5F60-4172-8384-95A6B7C8D9EA";

fn code(s: &Snapshot) -> Option<String> {
    match phase(s)? {
        WirelessPhaseView::Pairing { code, .. } => Some(code),
        _ => None,
    }
}

fn pairing(s: &Snapshot) -> bool {
    code(s).is_some()
}

fn hint(s: &Snapshot) -> Option<WirelessFailure> {
    s.wireless.as_ref().and_then(|w| w.hint.clone())
}

fn pairing_dials(fake: &FakeWireless) -> usize {
    fake.dials()
        .iter()
        .filter(|d| d.identity == PAIRING_IDENTITY)
        .count()
}

impl W {
    /// A phone at `.last` serving the rig's fake phone, knowing no pairing.
    fn stranger(&self, id: &str, last: u8) -> WirelessPhone {
        WirelessPhone::new(id, at(last)).serving(&self.r.mux)
    }

    async fn confirm(&self, matches: bool) -> Result<()> {
        self.r
            .controller
            .send(Command::ConfirmCode { matches })
            .await
    }

    async fn code_shown(&self) -> (Snapshot, String) {
        let snap = wait_for(&self.r.controller, "the code", pairing).await;
        let code = code(&snap).unwrap();
        (snap, code)
    }

    fn saved(&self, peer_id: &str) -> Option<Record> {
        self.store
            .records()
            .into_iter()
            .find(|r| r.peer_id == peer_id)
    }

    /// Nothing of the pairing in progress is left: no record, no key, no `F` sent, no code
    /// shown, nothing to confirm, the access point down; and nothing turns up later.
    async fn nothing_left_of_the_pairing(&self) {
        assert!(bad_request(self.confirm(true).await));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(self.saved(NEW_ID), None);
        assert!(self.store.key(PAIRED_IDENTITY).is_none());
        assert!(self.fake.pairings().iter().all(|p| p.f_valid.is_none()));
        assert!(!self.fake.is_up());
        assert!(!pairing(&self.r.controller.snapshot()));
        // The pairing that was there before is untouched.
        assert_eq!(self.saved(PHONE_ID), Some(record()));
    }
}

/// A rig as `wireless_rig_with`, with this computer's name set.
async fn pairing_rig(adjust: impl FnOnce(&mut ControllerConfig)) -> W {
    wireless_rig_with(FakePhoneOptions::default(), |config| {
        config.computer_name = Some("Desk PC".into());
        adjust(config);
    })
    .await
}

/// A rig showing the code of a pairing with the stranger at .9.
async fn showing_a_code() -> W {
    let w = pairing_rig(|_| {}).await;
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    w.start().await;
    w.code_shown().await;
    w
}

#[tokio::test]
async fn an_unpaired_phone_shows_the_code_then_pairs_and_connects() {
    let w = pairing_rig(|_| {}).await;
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    w.start().await;
    let (snap, code) = w.code_shown().await;
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));
    eventually("the phone's code", Duration::from_secs(5), || {
        w.fake.pairings().first().is_some_and(|p| p.code.is_some())
    })
    .await;
    let report = w.fake.pairings()[0].clone();
    assert_eq!(report.code.as_deref(), Some(code.as_str()), "the same code");
    assert_eq!(report.phone, NEW_ID);
    assert_eq!(report.computer_id, Some(w.store.local_id()));
    assert_eq!(report.computer_name.as_deref(), Some("Desk PC"));
    assert_eq!(snap.state, ConnState::Connecting);
    assert_eq!(w.saved(NEW_ID), None, "nothing saved before both agreed");
    // The code is on screen, never in what gets logged.
    assert!(!format!("{snap:?}").contains(&code));
    assert!(!format!("{:?}", snap.wireless).contains(&code));

    w.confirm(true).await.unwrap();
    let snap = w.connected().await;
    assert_eq!(snap.selected, Some(format!("wireless:{NEW_ID}")));
    let saved = w.saved(NEW_ID).expect("saved");
    assert_eq!(saved.identity, PAIRED_IDENTITY);
    assert_eq!(saved.name, PHONE_NAME);
    assert_eq!(saved.method, Method::Code);
    assert!(!saved.stale);
    assert!(w.store.key(PAIRED_IDENTITY).is_some());
    assert_eq!(w.fake.pairings()[0].f_valid, Some(true), "F was sent");
    let dials = w.fake.dials();
    assert_eq!(dials[0].identity, PAIRING_IDENTITY);
    assert_eq!(dials[0].request_line.as_deref(), Some("CG1 PAIR"));
    assert_eq!(pairing_dials(&w.fake), 1);
    assert!(
        dials[1..]
            .iter()
            .all(|d| d.identity == PAIRED_IDENTITY && d.to.ip() == at(9))
    );
    assert!(w.fake.is_up());
    assert_eq!(w.fake.start_attempts(), 1);
    // The other phone's pairing stays.
    assert_eq!(w.saved(PHONE_ID), Some(record()));
    assert!(w.store.key(IDENTITY).is_some());
}

#[test]
fn the_pairing_phase_never_prints_its_code() {
    let phase = WirelessPhase::Pairing {
        code: "042137".into(),
        phone_name: Some("Test iPhone".into()),
        confirmed: false,
    };
    assert!(!format!("{phase:?}").contains("042137"));
    assert!(!format!("{:?}", phase.view()).contains("042137"));
}

#[tokio::test]
async fn a_code_that_does_not_match_fails_and_saves_nothing() {
    let w = showing_a_code().await;
    w.confirm(false).await.unwrap();
    let snap = w.r.controller.snapshot();
    assert!(failed(&snap, WirelessFailure::PairingFailed), "{snap:?}");
    assert!(!w.fake.is_up());
    w.nothing_left_of_the_pairing().await;
    assert_eq!(w.r.backend.started(), 0);
}

#[tokio::test]
async fn the_phone_saying_no_is_denied_and_saves_nothing() {
    let w = pairing_rig(|_| {}).await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        answer: PairingAnswer::Deny,
        answer_after: Duration::from_millis(300),
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    w.start().await;
    w.code_shown().await;
    w.confirm(true).await.unwrap();
    wait_for(&w.r.controller, "denied", |s| {
        failed(s, WirelessFailure::PairingDenied)
    })
    .await;
    w.nothing_left_of_the_pairing().await;
    assert_eq!(pairing_dials(&w.fake), 1, "not asked again");
}

// Review Focus 5: every end of the attempt while the code is shown.

#[tokio::test]
async fn cancel_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.r.controller.send(Command::CancelWireless).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.r.controller.snapshot().wireless, None);
    w.nothing_left_of_the_pairing().await;
}

#[tokio::test]
async fn disconnect_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.r.controller.send(Command::Disconnect).await.unwrap();
    assert!(!w.fake.is_up());
    assert_eq!(w.r.controller.snapshot().wireless, None);
    w.nothing_left_of_the_pairing().await;
}

#[tokio::test]
async fn turning_the_switch_off_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.r.controller
        .send(Command::SetWireless { enabled: false })
        .await
        .unwrap();
    assert!(failed(
        &w.r.controller.snapshot(),
        WirelessFailure::TurnedOff
    ));
    w.nothing_left_of_the_pairing().await;
}

#[tokio::test]
async fn suspend_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.r.controller
        .send(Command::Sleep { sleeping: true })
        .await
        .unwrap();
    assert_eq!(w.r.controller.snapshot().wireless, None);
    w.r.controller
        .send(Command::Sleep { sleeping: false })
        .await
        .unwrap();
    w.nothing_left_of_the_pairing().await;
    assert_eq!(w.r.controller.snapshot().wireless, None);
}

#[tokio::test]
async fn an_iphone_on_usb_while_the_code_is_shown_takes_over_and_saves_nothing() {
    let w = showing_a_code().await;
    w.r.mux.attach(device("aaaa", 1));
    let snap = wait_for(&w.r.controller, "on usb", |s| {
        connected(s) && s.selected.as_deref() == Some("aaaa")
    })
    .await;
    assert_eq!(snap.wireless, None);
    w.nothing_left_of_the_pairing().await;
}

#[tokio::test]
async fn the_access_point_lost_while_the_code_is_shown_fails_and_saves_nothing() {
    let w = showing_a_code().await;
    w.fake.lose_access_point();
    wait_for(&w.r.controller, "lost", |s| {
        failed(s, WirelessFailure::Lost)
    })
    .await;
    w.nothing_left_of_the_pairing().await;
}

#[tokio::test]
async fn the_phone_leaving_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.fake.phone_leaves(at(9));
    // The channel ends cleanly after P: as a phone with another question showing, the
    // pairing is turned away (fix round 1, M1), not failed; the attempt goes on waiting.
    let snap = wait_for(&w.r.controller, "turned away", |s| {
        hint(s) == Some(WirelessFailure::PairingBusy)
    })
    .await;
    assert_eq!(phase(&snap), Some(WirelessPhaseView::Waiting));
    assert!(bad_request(w.confirm(true).await));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.saved(NEW_ID), None);
    assert!(w.store.key(PAIRED_IDENTITY).is_none());
    assert!(w.fake.pairings().iter().all(|p| p.f_valid.is_none()));
    assert!(!pairing(&w.r.controller.snapshot()));
    assert_eq!(pairing_dials(&w.fake), 1);
}

// --- Fix round 1.

/// After this computer's user confirms, the code stays shown (confirmed) until the phone
/// answers; asking again is refused.
#[tokio::test]
async fn after_a_match_the_code_stays_shown_until_the_phone_answers() {
    let w = pairing_rig(|_| {}).await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        answer_after: Duration::from_millis(800),
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    w.start().await;
    let (_, shown) = w.code_shown().await;
    w.confirm(true).await.unwrap();
    let confirmed = WirelessPhaseView::Pairing {
        code: shown.clone(),
        phone_name: None,
        confirmed: true,
    };
    assert_eq!(phase(&w.r.controller.snapshot()), Some(confirmed.clone()));
    assert!(bad_request(w.confirm(true).await));
    assert!(bad_request(w.confirm(false).await));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snap = w.r.controller.snapshot();
    assert_eq!(phase(&snap), Some(confirmed), "until the phone answers");
    assert!(!format!("{snap:?}").contains(&shown));
    assert_eq!(w.saved(NEW_ID), None);
    w.connected().await;
    assert!(w.saved(NEW_ID).is_some());
}

/// M1: the phone closing the channel right after P, with no answer line (another question on
/// its screen), turns the pairing away: shown as busy, not asked again in the window, and the
/// attempt ends as busy.
#[tokio::test]
async fn a_phone_with_another_question_showing_is_busy_not_failed() {
    let w = pairing_rig(|config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        answer: PairingAnswer::CloseAfterP,
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    w.start().await;
    let snap = wait_for(&w.r.controller, "busy", |s| {
        hint(s) == Some(WirelessFailure::PairingBusy)
    })
    .await;
    assert_eq!(phase(&snap), Some(WirelessPhaseView::Waiting));
    assert!(w.fake.is_up());
    wait_for(&w.r.controller, "the end", |s| {
        failed(s, WirelessFailure::PairingBusy)
    })
    .await;
    assert_eq!(pairing_dials(&w.fake), 1, "not asked again");
    assert_eq!(w.fake.pairings()[0].f_valid, None);
    w.nothing_left_of_the_pairing().await;
}

/// The phone says no while this computer's user confirms: whichever the actor sees first,
/// nothing is saved and the attempt ends as denied.
#[tokio::test]
async fn a_no_from_the_phone_racing_a_confirmation_saves_nothing() {
    for delay in [0u64, 1, 5, 20, 60] {
        let w = pairing_rig(|_| {}).await;
        let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
            answer: PairingAnswer::Deny,
            ..PhonePairing::default()
        });
        w.fake.add_phone(phone, Rounds::all());
        w.start().await;
        w.code_shown().await;
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let answered = w.confirm(true).await;
        assert!(
            answered.is_ok() || bad_request(answered),
            "delay {delay} ms"
        );
        wait_for(&w.r.controller, "denied", |s| {
            failed(s, WirelessFailure::PairingDenied)
        })
        .await;
        w.nothing_left_of_the_pairing().await;
    }
}

#[tokio::test]
async fn shutdown_while_the_code_is_shown_saves_nothing() {
    let w = showing_a_code().await;
    w.r.controller.shutdown().await;
    assert!(!w.fake.is_up());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.saved(NEW_ID), None);
    assert!(w.fake.pairings().iter().all(|p| p.f_valid.is_none()));
}

/// The task aborted while it waits for the user's answer: the question goes with it.
#[tokio::test]
async fn the_attempt_aborted_while_the_code_is_shown_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let fake = FakeWireless::new();
    fake.set_browse_delay(Duration::from_millis(10));
    fake.add_phone(WirelessPhone::new(NEW_ID, at(9)), Rounds::all());
    let (events, mut received) = mpsc::channel(64);
    let task = tokio::spawn(super::super::wireless_session::run(
        super::super::wireless_session::Context {
            attempt: 1,
            ports: fake.ports(),
            store: store.clone(),
            timing: timing(),
            events,
        },
    ));
    let reply = loop {
        match received.recv().await {
            Some(Event::Wireless(1, WirelessEvent::Confirm { reply, .. })) => break reply,
            Some(_) => {}
            None => panic!("the task ended"),
        }
    };
    assert!(fake.is_up());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(reply.is_closed(), "the question is withdrawn");
    assert!(!fake.is_up());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(store.records().iter().all(|r| r.peer_id != NEW_ID));
    assert!(fake.pairings().iter().all(|p| p.f_valid.is_none()));
}

#[tokio::test]
async fn an_unknown_identity_right_after_pairing_is_retried_and_connects() {
    let w = pairing_rig(|_| {}).await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        accept_after: Duration::from_millis(400),
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    w.start().await;
    w.code_shown().await;
    w.confirm(true).await.unwrap();
    w.connected().await;
    let data: Vec<_> = w
        .fake
        .dials()
        .into_iter()
        .filter(|d| d.identity == PAIRED_IDENTITY)
        .collect();
    assert!(data.len() >= 2, "refused first: {data:?}");
    assert!(!w.saved(NEW_ID).unwrap().stale);
    assert_eq!(pairing_dials(&w.fake), 1);
}

/// A pairing the phone never takes: its refusals come from an address never verified for the
/// new identity (the pairing channel's handshake does not count), so they prove nothing even
/// long after the grace; the window ends as refused and the record is not marked stale.
#[tokio::test(start_paused = true)]
async fn a_new_pairing_the_phone_never_takes_is_never_verified_nor_marked_stale() {
    let p = paused_rig();
    p.fake.add_phone(
        WirelessPhone::new(NEW_ID, at(9)).pairing(PhonePairing {
            accept_after: Duration::from_secs(3600),
            ..PhonePairing::default()
        }),
        Rounds::all(),
    );
    p.controller.send(Command::ConnectWireless).await.unwrap();
    paused_wait(&p.controller, "the code", pairing).await;
    let confirmed = tokio::time::Instant::now();
    p.controller
        .send(Command::ConfirmCode { matches: true })
        .await
        .unwrap();
    let snap = paused_wait(&p.controller, "the end", |s| {
        matches!(phase(s), Some(WirelessPhaseView::Failed { .. }))
    })
    .await;
    assert!(failed(&snap, WirelessFailure::PairingRefused), "{snap:?}");
    assert!(
        confirmed.elapsed() >= Duration::from_secs(60),
        "the whole window: {:?}",
        confirmed.elapsed()
    );
    let store = WirelessStore::open(p._dir.path(), Arc::new(PlainSecrets)).unwrap();
    let saved = store.records().into_iter().find(|r| r.peer_id == NEW_ID);
    assert!(!saved.unwrap().stale);
    assert_eq!(pairing_dials(&p.fake), 1, "not paired again");
    assert!(!p.fake.is_up());
}

/// After a pairing by code the phone may refuse the new identity for a while; refusals within
/// the 10 s grace are not counted even at a verified address, so the pairing ends as failed (it
/// did not take) only 5 s after the grace, and is kept.
#[tokio::test]
async fn refusals_within_the_grace_after_pairing_are_not_counted() {
    let w = pairing_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(30);
    })
    .await;
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    w.start().await;
    w.code_shown().await;
    let confirmed = std::time::Instant::now();
    w.confirm(true).await.unwrap();
    w.connected().await;
    w.fake.script_dials(
        DialMatch::identity(PAIRED_IDENTITY),
        vec![DialOutcome::Fail(DialError::UnknownIdentity); 100_000],
    );
    w.r.phone.end_session();
    wait_within(&w.r.controller, "failed", Duration::from_secs(25), |s| {
        failed(s, WirelessFailure::PairingFailed)
    })
    .await;
    let after = confirmed.elapsed();
    assert!(after >= Duration::from_millis(14_500), "after {after:?}");
    let saved = w.saved(NEW_ID).unwrap();
    assert!(!saved.stale, "kept as it is");
    assert!(w.store.key(PAIRED_IDENTITY).is_some());
    assert!(!w.fake.is_up());
}

/// A reconnect brings back the phone it lost; a stranger on the access point meanwhile is not
/// paired.
#[tokio::test]
async fn reconnecting_never_pairs_another_phone() {
    let w = connected_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(1);
    })
    .await;
    w.fake.stop_advertising(PHONE_ID, at(205));
    w.fake.phone_leaves(at(205));
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    wait_for(&w.r.controller, "lost", |s| {
        failed(s, WirelessFailure::Lost)
    })
    .await;
    assert_eq!(pairing_dials(&w.fake), 0);
    assert!(w.fake.pairings().is_empty());
}

/// A pairing that completes after the waiting window ran out still gets its own retries.
#[tokio::test]
async fn a_pairing_finished_after_the_window_still_gets_its_chance() {
    let w = pairing_rig(|config| {
        config.wireless_timing.wait_for_phone = Duration::from_millis(500);
    })
    .await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        answer_after: Duration::from_millis(800),
        accept_after: Duration::from_millis(300),
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    w.start().await;
    w.code_shown().await;
    w.confirm(true).await.unwrap();
    w.connected().await;
    assert!(w.saved(NEW_ID).is_some());
}

#[tokio::test]
async fn two_unpaired_phones_wait_and_pairing_goes_on_when_one_is_left() {
    let w = pairing_rig(|_| {}).await;
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    w.fake.add_phone(w.stranger(OTHER_ID, 10), Rounds::all());
    w.start().await;
    let snap = wait_for(&w.r.controller, "the hint", |s| {
        hint(s) == Some(WirelessFailure::SeveralPhones)
    })
    .await;
    assert_eq!(phase(&snap), Some(WirelessPhaseView::Waiting));
    assert!(w.fake.dials().is_empty());
    w.fake.stop_advertising(OTHER_ID, at(10));
    let (snap, _) = w.code_shown().await;
    assert_eq!(hint(&snap), None);
    w.confirm(true).await.unwrap();
    let snap = w.connected().await;
    assert_eq!(snap.selected, Some(format!("wireless:{NEW_ID}")));
    assert!(w.fake.dials().iter().all(|d| d.to.ip() == at(9)));
}

#[tokio::test]
async fn two_unpaired_phones_until_the_window_ends_fail_as_several() {
    let w = pairing_rig(|config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    w.fake.add_phone(w.stranger(NEW_ID, 9), Rounds::all());
    w.fake.add_phone(w.stranger(OTHER_ID, 10), Rounds::all());
    w.start().await;
    let started = std::time::Instant::now();
    wait_for(&w.r.controller, "several", |s| {
        failed(s, WirelessFailure::SeveralPhones)
    })
    .await;
    assert!(started.elapsed() >= Duration::from_millis(900), "kept up");
    assert!(w.fake.dials().is_empty());
    assert!(!w.fake.is_up());
}

/// A pairing stale from before: that phone is paired by code, and the new pairing replaces the
/// old one (its key is gone).
#[tokio::test]
async fn a_stale_pairing_is_replaced_by_code_for_that_phone() {
    let w = pairing_rig(|_| {}).await;
    w.store.set_stale(PHONE_ID, true).unwrap();
    w.fake.add_phone(w.stranger(PHONE_ID, 205), Rounds::all());
    w.start().await;
    let (snap, _) = w.code_shown().await;
    assert_eq!(
        phase(&snap),
        Some(WirelessPhaseView::Pairing {
            code: code(&snap).unwrap(),
            phone_name: Some("Test iPhone".into()),
            confirmed: false,
        }),
        "the name from the earlier pairing"
    );
    w.confirm(true).await.unwrap();
    w.connected().await;
    let records = w.store.records();
    assert_eq!(records.len(), 1, "replaced: {records:?}");
    assert_eq!(records[0].peer_id, PHONE_ID);
    assert_eq!(records[0].identity, PAIRED_IDENTITY);
    assert_eq!(records[0].method, Method::Code);
    assert!(!records[0].stale);
    assert!(w.store.key(IDENTITY).is_none(), "the old key is gone");
    assert!(w.store.key(PAIRED_IDENTITY).is_some());
}

#[tokio::test]
async fn confirm_code_without_a_code_shown_is_refused_and_changes_nothing() {
    let w = wireless_rig().await;
    assert!(bad_request(w.confirm(true).await));
    assert!(bad_request(w.confirm(false).await));
    assert_eq!(w.r.controller.snapshot().wireless, None);
    assert_eq!(w.fake.start_attempts(), 0);

    w.start().await;
    wait_for(&w.r.controller, "waiting", |s| {
        phase(s) == Some(WirelessPhaseView::Waiting)
    })
    .await;
    assert!(bad_request(w.confirm(false).await));
    assert_eq!(
        phase(&w.r.controller.snapshot()),
        Some(WirelessPhaseView::Waiting)
    );
    assert!(w.fake.is_up());

    w.add_phone(Rounds::all());
    w.connected().await;
    assert!(bad_request(w.confirm(false).await));
    assert!(on_air(&w.r.controller.snapshot()));
    assert!(w.fake.is_up());
    assert_eq!(w.r.backend.stopped(), 0);
}

/// A `C` line naming another phone (here the user's paired iPhone) ends the pairing before
/// this computer sends anything of its own, and nothing is saved or replaced.
#[tokio::test]
async fn a_c_line_naming_another_phone_fails_before_m_and_saves_nothing() {
    let w = pairing_rig(|_| {}).await;
    let phone = w.stranger(NEW_ID, 9).pairing(PhonePairing {
        claim_phone_id: Some(PHONE_ID.into()),
        ..PhonePairing::default()
    });
    w.fake.add_phone(phone, Rounds::all());
    let mut rx = w.r.controller.subscribe();
    w.start().await;
    let shown = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snap = rx.borrow_and_update().clone();
            if pairing(&snap) {
                return true;
            }
            if failed(&snap, WirelessFailure::PairingFailed) {
                return false;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("an end");
    assert!(!shown, "no code shown");
    assert!(!w.fake.pairings()[0].got_m, "no M sent");
    w.nothing_left_of_the_pairing().await;
}

/// Review Focus 6: something advertising the paired phone's name and refusing its pairing
/// (the phone itself away) is never paired by code, and the pairing stays good.
#[tokio::test]
async fn an_impostor_with_the_paired_name_is_never_paired_by_code() {
    let w = pairing_rig(|config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    w.fake.add_impostor(PHONE_ID, at(66), Rounds::all());
    w.start().await;
    wait_for(&w.r.controller, "refused", |s| {
        failed(s, WirelessFailure::PairingRefused)
    })
    .await;
    assert_eq!(w.saved(PHONE_ID), Some(record()), "not stale");
    assert_eq!(pairing_dials(&w.fake), 0);
    assert!(w.fake.pairings().is_empty());
}

/// Never pair while the name is ambiguous, even for a pairing that is stale.
#[tokio::test]
async fn a_stale_phone_at_two_addresses_is_neither_dialled_nor_paired() {
    let w = pairing_rig(|config| {
        config.wireless_timing.wait_for_phone = Duration::from_secs(1);
    })
    .await;
    w.store.set_stale(PHONE_ID, true).unwrap();
    w.fake.add_phone(w.stranger(PHONE_ID, 205), Rounds::all());
    w.fake.add_impostor(PHONE_ID, at(66), Rounds::all());
    w.start().await;
    wait_for(&w.r.controller, "clash", |s| {
        failed(s, WirelessFailure::NameClash)
    })
    .await;
    assert!(w.fake.dials().is_empty());
    assert!(w.fake.pairings().is_empty());
}

/// A pairing that was good when the attempt began and that it marked stale is not paired by
/// code in the same attempt; the user asking again pairs it.
#[tokio::test]
async fn a_pairing_marked_stale_is_paired_by_code_only_when_asked_again() {
    let w = connected_rig(|config| {
        config.wireless_timing.retry_for = Duration::from_secs(30);
    })
    .await;
    w.fake.script_dials(
        DialMatch {
            address: Some(at(205)),
            identity: Some(IDENTITY.into()),
        },
        vec![DialOutcome::Fail(DialError::UnknownIdentity); 1000],
    );
    w.r.phone.end_session();
    wait_within(&w.r.controller, "stale", Duration::from_secs(20), |s| {
        failed(s, WirelessFailure::PairingStale)
    })
    .await;
    assert_eq!(pairing_dials(&w.fake), 0);
    assert!(w.store.records()[0].stale);

    w.start().await;
    w.code_shown().await;
    assert_eq!(pairing_dials(&w.fake), 1);
    assert_eq!(w.fake.pairings()[0].phone, PHONE_ID);
    w.r.controller.send(Command::CancelWireless).await.unwrap();
}

/// The phone turning pairing away (closing the pairing channel before its first line, as in
/// its lockout): shown, not asked again for a minute, and the attempt ends as busy.
#[tokio::test(start_paused = true)]
async fn a_phone_that_turns_pairing_away_is_asked_again_only_after_a_minute() {
    let p = paused_rig();
    p.fake.add_phone(
        WirelessPhone::new(NEW_ID, at(9)).pairing(PhonePairing {
            answer: PairingAnswer::CloseAtOnce,
            ..PhonePairing::default()
        }),
        Rounds::all(),
    );
    let asked = tokio::time::Instant::now();
    p.controller.send(Command::ConnectWireless).await.unwrap();
    let snap = paused_wait(&p.controller, "busy", |s| {
        hint(s) == Some(WirelessFailure::PairingBusy)
    })
    .await;
    assert_eq!(phase(&snap), Some(WirelessPhaseView::Waiting));
    let first = tokio::time::Instant::now();
    assert_eq!(pairing_dials(&p.fake), 1);
    let real = std::time::Instant::now();
    while pairing_dials(&p.fake) < 2 {
        assert!(
            real.elapsed() < Duration::from_secs(30),
            "never asked again"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let again = first.elapsed();
    assert!(again >= Duration::from_secs(59), "after {again:?}");
    assert!(again <= Duration::from_secs(66), "after {again:?}");
    paused_wait(&p.controller, "the end", |s| {
        failed(s, WirelessFailure::PairingBusy)
    })
    .await;
    assert!(asked.elapsed() >= Duration::from_secs(300));
    assert!(pairing_dials(&p.fake) <= 5, "{}", pairing_dials(&p.fake));
    assert!(!p.fake.is_up());
}

/// 75 s without this computer's user answering: the pairing fails and nothing is saved.
#[tokio::test(start_paused = true)]
async fn no_answer_for_75_seconds_fails_and_saves_nothing() {
    let p = paused_rig();
    p.fake
        .add_phone(WirelessPhone::new(NEW_ID, at(9)), Rounds::all());
    p.controller.send(Command::ConnectWireless).await.unwrap();
    paused_wait(&p.controller, "the code", pairing).await;
    let shown = tokio::time::Instant::now();
    paused_wait(&p.controller, "failed", |s| {
        failed(s, WirelessFailure::PairingFailed)
    })
    .await;
    let after = shown.elapsed();
    assert!(after >= Duration::from_secs(74), "after {after:?}");
    assert!(after <= Duration::from_secs(80), "after {after:?}");
    assert!(!p.fake.is_up());
    assert!(matches!(
        p.controller
            .send(Command::ConfirmCode { matches: true })
            .await,
        Err(CoreError::BadRequest)
    ));
    let store = WirelessStore::open(p._dir.path(), Arc::new(PlainSecrets)).unwrap();
    assert!(store.records().iter().all(|r| r.peer_id != NEW_ID));
    assert!(p.fake.pairings().iter().all(|r| r.f_valid.is_none()));
}
