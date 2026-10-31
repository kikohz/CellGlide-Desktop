//! For this crate's tests only: events without a runtime, and the page tests' snapshots and day.

use crate::api::Event;
use cg_ipc::health::{
    DayStart, EventKind, EventPhase, HealthEvent, HealthSnapshot, HistoryDay, IssueCause,
};
use cg_ipc::phone::{PhoneCard, PhoneFrame, PhoneLabel};
use cg_ipc::{ConnState, DnsMode, Snapshot};

pub use crate::runtime::{Events, guarded};
pub use shape::plain_shape;

mod shape;

/// The serial of the iPhone in [`connected_snapshot`] and [`disconnected_snapshot`].
pub const SERIAL: &str = "00008140-000000000000002C";

/// The subscription panics on the next update it takes, as a bug in a view would.
pub fn panic_subscription_next(runtime: &crate::runtime::Runtime) {
    let shared = &runtime.context().shared;
    shared
        .panic_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The speed page's watch panics the next time it wakes up.
pub fn panic_speed_watch_next(runtime: &crate::runtime::Runtime) {
    let speed = &runtime.context().speed;
    speed
        .panic_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Events delivered by calling `sink` right away on the emitting thread.
pub fn events_into(sink: Box<dyn Fn(Event) + Send>) -> Events {
    Events::direct(sink)
}

/// Connected through one iPhone that reports a frame; the same data as cg-gui's page tests.
pub fn connected_snapshot() -> Snapshot {
    let mut snapshot = disconnected_snapshot();
    snapshot.state = ConnState::Connected {
        phone_version: "2.4.0".into(),
        dns: DnsMode::Phone,
    };
    snapshot.wants_connection = true;
    snapshot.phone = Some(PhoneCard {
        frame: Some(PhoneFrame {
            carrier: Some("中国移动".into()),
            radio: Some("5G".into()),
            sharing: Some(true),
            download: Some(2_500_000.0),
            upload: Some(400_000.0),
            quality_score: Some(90),
            latency: Some(38.0),
            ..PhoneFrame::default()
        }),
        ..PhoneCard::default()
    });
    snapshot.socks_port = Some(1080);
    snapshot
}

/// The same iPhone plugged in, nobody asked to connect.
pub fn disconnected_snapshot() -> Snapshot {
    Snapshot {
        state: ConnState::Idle,
        devices: vec![SERIAL.into()],
        selected: Some(SERIAL.into()),
        labels: [(
            SERIAL.to_string(),
            PhoneLabel {
                name: Some("Test iPhone".into()),
                model: Some("iPhone17,5".into()),
            },
        )]
        .into(),
        ..Snapshot::default()
    }
}

/// A week on record, looking at its fourth day (2026-10-27, UTC): the service starts, sharing
/// runs from 09:00 to 12:00, and an issue starts, gets worse and ends in between.
pub fn middle_day() -> HistoryDay {
    let at = |h: i8, m: i8| {
        jiff::civil::date(2026, 10, 27)
            .at(h, m, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .expect("a UTC time")
            .timestamp()
    };
    let issue = |t, cause, phase, duration, snapshot| HealthEvent {
        cause: Some(cause),
        phase: Some(phase),
        duration,
        snapshot,
        ..HealthEvent::new(t, EventKind::Issue)
    };
    let first = HealthSnapshot {
        channel_rtt: Some(4),
        phone_latency: Some(41),
        phone_loss: Some(0.02),
        radio: Some("5G".into()),
        connects_failed: Some([("timeout".to_string(), 3)].into()),
        thermal: Some(1),
        ..HealthSnapshot::default()
    };
    let worse = HealthSnapshot {
        thermal: Some(2),
        ..HealthSnapshot::default()
    };
    let days: Vec<String> = (24..=30).map(|d| format!("2026-10-{d}")).collect();
    HistoryDay {
        day: days[3].clone(),
        start: DayStart::default(),
        events: vec![
            HealthEvent::new(at(8, 0), EventKind::ServiceStarted),
            HealthEvent::new(at(8, 1), EventKind::UsbAttached),
            HealthEvent::new(at(9, 0), EventKind::TunnelStarted),
            issue(
                at(10, 0),
                IssueCause::PhoneUnstable,
                EventPhase::Start,
                None,
                Some(first),
            ),
            issue(
                at(10, 5),
                IssueCause::Channel,
                EventPhase::Escalate,
                None,
                Some(worse),
            ),
            issue(
                at(10, 10),
                IssueCause::Channel,
                EventPhase::End,
                Some(600),
                None,
            ),
            HealthEvent::new(at(12, 0), EventKind::TunnelStopped),
        ],
        days,
        begin: at(0, 0),
        end: at(0, 0) + jiff::SignedDuration::from_hours(24),
    }
}
