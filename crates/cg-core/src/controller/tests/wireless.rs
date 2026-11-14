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
