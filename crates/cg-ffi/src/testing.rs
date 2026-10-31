//! For this crate's tests only: events without a runtime, and the page tests' snapshots.

use crate::api::Event;
use cg_ipc::phone::{PhoneCard, PhoneFrame, PhoneLabel};
use cg_ipc::{ConnState, DnsMode, Snapshot};

pub use crate::runtime::{Events, guarded};

/// The serial of the iPhone in [`connected_snapshot`] and [`disconnected_snapshot`].
pub const SERIAL: &str = "00008140-000000000000002C";

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
