//! `Command::Subscribe`: the service's snapshots as [`Event::State`], [`Event::ServiceGone`]
//! while it is away (it is asked again every `Config::retry`, and `State` resumes by itself
//! when it is back), and [`Event::PhonePlugged`] when an iPhone is plugged in. Every
//! `Config::tick` the view is worked out again at the current time and sent if it changed, so
//! the durations in the health line move. `Command::Toggle` acts on the last snapshot.

use crate::api::{Event, ReplyResult};
use crate::runtime::{Context, panic_message};
use crate::views::{self, OverviewView};
use cg_i18n::tr;
use cg_ipc::client::{self, Update};
use cg_ipc::describe::{
    self,
    ui::{self, KnownDevices, ServiceDown},
};
use cg_ipc::{Endpoint, Snapshot};
use futures::FutureExt;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;
use tokio::time::MissedTickBehavior;

/// What the subscription shares with the other commands.
#[derive(Default)]
pub(crate) struct Shared {
    /// The last snapshot, while the service is there.
    snapshot: Mutex<Option<Snapshot>>,
    /// A subscription is running.
    subscribed: AtomicBool,
}

impl Shared {
    /// The last snapshot, if the service is there.
    pub(crate) fn snapshot(&self) -> Option<Snapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn keep(&self, snapshot: Option<Snapshot>) {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = snapshot;
    }
}

/// Starts following the service unless that is already happening; either way, `Done`.
pub(crate) fn subscribe(context: &Context) -> ReplyResult {
    if context.shared.subscribed.swap(true, Ordering::SeqCst) {
        return ReplyResult::done();
    }
    let context = context.clone();
    tokio::spawn(async move {
        let cancel = context.cancel.clone();
        let run = AssertUnwindSafe(follow(&context)).catch_unwind();
        let ended = tokio::select! {
            () = cancel.cancelled() => None,
            ended = run => Some(ended),
        };
        // A panic ends this subscription only: the app hears of it and may subscribe again.
        if let Some(Err(payload)) = ended {
            context.events.emit(Event::Fault {
                message: panic_message(&*payload),
            });
        }
        context.shared.keep(None);
        context.shared.subscribed.store(false, Ordering::SeqCst);
    });
    ReplyResult::done()
}

async fn follow(context: &Context) {
    let config = &context.config;
    let mut updates = client::watch(config.endpoint.clone(), config.retry);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + config.tick, config.tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut follower = Follower::default();
    loop {
        let events = tokio::select! {
            update = updates.recv() => match update {
                Some(Update::Snapshot(snapshot)) => {
                    context.shared.keep(Some((*snapshot).clone()));
                    follower.snapshot(*snapshot, Timestamp::now(), &TimeZone::system(), Instant::now())
                }
                Some(Update::Down(down)) => {
                    context.shared.keep(None);
                    follower.down(down)
                }
                // The watch only ends with the runtime.
                None => return,
            },
            _ = tick.tick() => follower.tick(Timestamp::now(), &TimeZone::system()),
        };
        for event in events {
            context.events.emit(event);
        }
    }
}

/// What the subscription has told the app so far, and what it needs to tell the next thing.
#[derive(Default)]
struct Follower {
    snapshot: Option<Snapshot>,
    /// The last view sent since the service (re)appeared.
    view: Option<OverviewView>,
    /// The last reason sent for the service being away; the same one isn't repeated on every
    /// retry.
    down: Option<ServiceDown>,
    /// The iPhones of the last snapshot; `None` before the first snapshot of each subscription
    /// round, so the phones already there don't count as plugged in.
    known: Option<KnownDevices>,
}

impl Follower {
    fn snapshot(
        &mut self,
        snapshot: Snapshot,
        now: Timestamp,
        tz: &TimeZone,
        instant: Instant,
    ) -> Vec<Event> {
        self.down = None;
        let plugged = ui::plugged_serial(&mut self.known, &snapshot.devices, instant);
        let view = views::overview(&snapshot, now, tz);
        self.snapshot = Some(snapshot);
        let mut events: Vec<Event> = self.show(view).into_iter().collect();
        events.extend(plugged.map(|serial| Event::PhonePlugged { serial }));
        events
    }

    fn down(&mut self, down: ServiceDown) -> Vec<Event> {
        self.snapshot = None;
        self.view = None;
        self.known = None;
        if self.down.as_ref() == Some(&down) {
            return Vec::new();
        }
        self.down = Some(down.clone());
        vec![Event::ServiceGone {
            text: down.text,
            not_in_group: down.not_in_group,
        }]
    }

    fn tick(&mut self, now: Timestamp, tz: &TimeZone) -> Vec<Event> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let view = views::overview(snapshot, now, tz);
        self.show(view).into_iter().collect()
    }

    /// `State` for a view that differs from the last one sent.
    fn show(&mut self, view: OverviewView) -> Option<Event> {
        if self.view.as_ref() == Some(&view) {
            return None;
        }
        self.view = Some(view.clone());
        Some(Event::State {
            view: Box::new(view),
        })
    }
}

/// `Command::Toggle`: connect or disconnect, as the last snapshot's wish calls for.
pub(crate) async fn toggle(endpoint: &Endpoint, shared: &Shared) -> ReplyResult {
    match shared.snapshot() {
        Some(snapshot) => {
            crate::requests::request(endpoint, describe::toggle_request(&snapshot)).await
        }
        None => ReplyResult::err(tr("gui.waiting_service", &[])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{connected_snapshot, disconnected_snapshot};
    use cg_ipc::health::IssueCause;
    use std::time::Duration;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_793_000_000 + seconds).unwrap()
    }

    fn down(text: &str) -> ServiceDown {
        ServiceDown {
            text: text.into(),
            not_in_group: false,
        }
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                Event::State { .. } => "state",
                Event::ServiceGone { .. } => "gone",
                Event::PhonePlugged { .. } => "plugged",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn a_plugged_phone_follows_its_state_and_the_first_snapshot_plugs_nothing() {
        let _g = cg_i18n::lock_for_tests();
        let t0 = Instant::now();
        let mut f = Follower::default();
        let mut s = disconnected_snapshot();
        assert_eq!(
            kinds(&f.snapshot(s.clone(), at(0), &TimeZone::UTC, t0)),
            ["state"]
        );
        s.devices.push("B".into());
        let events = f.snapshot(s.clone(), at(1), &TimeZone::UTC, t0);
        assert_eq!(kinds(&events), ["state", "plugged"]);
        assert_eq!(events[1], Event::PhonePlugged { serial: "B".into() });
        // The same snapshot again: nothing new to say.
        assert!(f.snapshot(s, at(1), &TimeZone::UTC, t0).is_empty());
    }

    #[test]
    fn the_service_going_is_said_once_and_resets_everything() {
        let _g = cg_i18n::lock_for_tests();
        let t0 = Instant::now();
        let mut f = Follower::default();
        let s = disconnected_snapshot();
        f.snapshot(s.clone(), at(0), &TimeZone::UTC, t0);
        assert_eq!(kinds(&f.down(down("gone"))), ["gone"]);
        assert!(f.down(down("gone")).is_empty(), "not again on every retry");
        assert_eq!(kinds(&f.down(down("other reason"))), ["gone"]);
        assert!(
            f.tick(at(5), &TimeZone::UTC).is_empty(),
            "no state while away"
        );
        // Back with one more phone: the first snapshot of the round plugs nothing, and the
        // same view as before the service went is sent again.
        let more = Snapshot {
            devices: vec!["A".into(), "B".into()],
            ..s
        };
        let events = f.snapshot(more, at(6), &TimeZone::UTC, t0 + Duration::from_secs(120));
        assert_eq!(kinds(&events), ["state"]);
        assert_eq!(
            kinds(&f.down(down("gone"))),
            ["gone"],
            "said again after a snapshot"
        );
    }

    #[test]
    fn a_tick_sends_the_view_only_when_time_changed_it() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let t0 = Instant::now();
        let mut f = Follower::default();
        let mut s = connected_snapshot();
        s.health.cause = Some(IssueCause::PhoneNetwork);
        s.health.since = Some(at(0));
        f.snapshot(s, at(0), &TimeZone::UTC, t0);
        assert!(f.tick(at(0), &TimeZone::UTC).is_empty());
        // The issue's duration in the health line moves on.
        assert_eq!(kinds(&f.tick(at(5), &TimeZone::UTC)), ["state"]);
        assert!(f.tick(at(5), &TimeZone::UTC).is_empty());
        // Without an issue time changes nothing on the page.
        let mut f = Follower::default();
        f.snapshot(connected_snapshot(), at(0), &TimeZone::UTC, t0);
        assert!(f.tick(at(600), &TimeZone::UTC).is_empty());
    }
}
