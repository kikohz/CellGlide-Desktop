//! `Command::Subscribe`: the service's snapshots as [`Event::State`], [`Event::ServiceGone`]
//! while it is away (it is asked again every `Config::retry`, and `State` resumes by itself
//! when it is back), and [`Event::PhonePlugged`] when an iPhone is plugged in. Every
//! `Config::tick` the view is worked out again at the current time and sent if it changed, so
//! the durations in the health line move; after `SetLang` the last `State` or `ServiceGone` is
//! said again at once in the new language. `Command::Toggle` acts on the last snapshot.

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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;

/// What the subscription shares with the other commands.
#[derive(Default)]
pub(crate) struct Shared {
    /// The last snapshot, while the service is there; the only copy.
    snapshot: Mutex<Option<Snapshot>>,
    /// A subscription is running.
    subscribed: AtomicBool,
    /// The language changed: say the last thing again.
    relabel: Notify,
    /// The snapshot, the language or the preferences changed: the speed and leak pages look
    /// again.
    pub(crate) pages: Notify,
    /// Tests: the subscription panics on its next update.
    #[cfg(test)]
    pub(crate) panic_next: AtomicBool,
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
        self.pages_changed();
    }

    /// Something the speed and leak pages show may have changed.
    pub(crate) fn pages_changed(&self) {
        self.pages.notify_one();
    }

    /// After `SetLang`: a running subscription says its last `State` or `ServiceGone` again.
    /// (Without one the wake-up is kept and does nothing but recompute.)
    pub(crate) fn language_changed(&self) {
        self.relabel.notify_one();
        self.pages_changed();
    }
}

/// Starts following the service unless that is already happening; either way, `Done`.
pub(crate) fn subscribe(context: &Context) -> ReplyResult {
    if context.shared.subscribed.swap(true, Ordering::SeqCst) {
        return ReplyResult::done();
    }
    // The speed and leak pages follow what the subscription sees from now on.
    crate::speed::watch(context);
    let context = context.clone();
    tokio::spawn(async move {
        let cancel = context.cancel.clone();
        let run = AssertUnwindSafe(follow(&context)).catch_unwind();
        let ended = tokio::select! {
            () = cancel.cancelled() => None,
            ended = run => Some(ended),
        };
        // The snapshot no longer holds once nothing follows the service: `Toggle` must not act
        // on it. Cleared before the app hears of a panic, so whatever it sends next sees that.
        context.shared.keep(None);
        context.shared.subscribed.store(false, Ordering::SeqCst);
        // A panic ends this subscription only: the app hears of it and may subscribe again.
        if let Some(Err(payload)) = ended {
            context.events.emit(Event::Fault {
                message: panic_message(&*payload),
            });
        }
    });
    ReplyResult::done()
}

async fn follow(context: &Context) {
    let config = &context.config;
    let mut updates = client::watch(config.endpoint.clone(), config.retry);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + config.tick, config.tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut follower = Follower::new(context.shared.clone());
    loop {
        let events = tokio::select! {
            update = updates.recv() => {
                #[cfg(test)]
                if context.shared.panic_next.swap(false, Ordering::SeqCst) {
                    panic!("a test asked the subscription to panic");
                }
                match update {
                    Some(Update::Snapshot(snapshot)) => follower.snapshot(
                        *snapshot,
                        Timestamp::now(),
                        &TimeZone::system(),
                        Instant::now(),
                    ),
                    Some(Update::Down(down)) => follower.down(down),
                    // The watch only ends with the runtime.
                    None => return,
                }
            }
            _ = tick.tick() => follower.tick(Timestamp::now(), &TimeZone::system()),
            () = context.shared.relabel.notified() => {
                follower.relabel(Timestamp::now(), &TimeZone::system())
            }
        };
        for event in events {
            context.events.emit(event);
        }
    }
}

/// What the subscription has told the app so far, and what it needs to tell the next thing.
/// The snapshot itself lives in [`Shared`] only.
struct Follower {
    shared: Arc<Shared>,
    /// The last view sent since the service (re)appeared.
    view: Option<OverviewView>,
    /// The last reason sent for the service being away. A retry that fails the same way (OS
    /// detail aside) isn't said again.
    down: Option<ServiceDown>,
    /// The iPhones of the last snapshot; `None` before the first snapshot of each subscription
    /// round, so the phones already there don't count as plugged in.
    known: Option<KnownDevices>,
}

impl Follower {
    fn new(shared: Arc<Shared>) -> Follower {
        Follower {
            shared,
            view: None,
            down: None,
            known: None,
        }
    }

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
        self.shared.keep(Some(snapshot));
        let mut events: Vec<Event> = self.show(view).into_iter().collect();
        events.extend(plugged.map(|serial| Event::PhonePlugged { serial }));
        events
    }

    fn down(&mut self, down: ServiceDown) -> Vec<Event> {
        self.shared.keep(None);
        self.view = None;
        self.known = None;
        if self
            .down
            .as_ref()
            .is_some_and(|said| said.same_reason(&down))
        {
            return Vec::new();
        }
        let event = gone(&down);
        self.down = Some(down);
        vec![event]
    }

    fn tick(&mut self, now: Timestamp, tz: &TimeZone) -> Vec<Event> {
        let Some(snapshot) = self.shared.snapshot() else {
            return Vec::new();
        };
        let view = views::overview(&snapshot, now, tz);
        self.show(view).into_iter().collect()
    }

    /// The language changed: the last thing said, again, in the new language.
    fn relabel(&mut self, now: Timestamp, tz: &TimeZone) -> Vec<Event> {
        if let Some(down) = &self.down {
            let down = down.retranslated();
            let event = gone(&down);
            self.down = Some(down);
            return vec![event];
        }
        self.view = None;
        self.tick(now, tz)
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

/// `ServiceGone` for `down`, with what the tray and the sidebar show meanwhile.
pub(crate) fn gone(down: &ServiceDown) -> Event {
    Event::ServiceGone {
        text: down.text.clone(),
        not_in_group: down.not_in_group,
        chrome: views::chrome(None, Some(down)),
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
    use cg_ipc::describe::ui::DownReason;
    use cg_ipc::health::IssueCause;
    use cg_ipc::{Check, CheckId};
    use std::time::Duration;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_793_000_000 + seconds).unwrap()
    }

    /// The service unreachable, the OS saying `error`.
    fn unreachable(error: &str) -> ServiceDown {
        ServiceDown::new(DownReason::Unreachable {
            check: Check::new(CheckId::Service, false).with("error", error),
        })
    }

    fn stopped() -> ServiceDown {
        ServiceDown::new(DownReason::Gone)
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
        let mut f = Follower::new(Arc::default());
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
        let mut f = Follower::new(Arc::default());
        let s = disconnected_snapshot();
        f.snapshot(s.clone(), at(0), &TimeZone::UTC, t0);
        assert_eq!(kinds(&f.down(stopped())), ["gone"]);
        assert!(f.down(stopped()).is_empty(), "not again on every retry");
        assert_eq!(
            kinds(&f.down(unreachable("busy"))),
            ["gone"],
            "another reason"
        );
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
            kinds(&f.down(stopped())),
            ["gone"],
            "said again after a snapshot"
        );
    }

    #[test]
    fn a_tick_sends_the_view_only_when_time_changed_it() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let t0 = Instant::now();
        let mut f = Follower::new(Arc::default());
        let mut s = connected_snapshot();
        s.health.cause = Some(IssueCause::PhoneNetwork);
        s.health.since = Some(at(0));
        f.snapshot(s, at(0), &TimeZone::UTC, t0);
        assert!(f.tick(at(0), &TimeZone::UTC).is_empty());
        // The issue's duration in the health line moves on.
        assert_eq!(kinds(&f.tick(at(5), &TimeZone::UTC)), ["state"]);
        assert!(f.tick(at(5), &TimeZone::UTC).is_empty());
        // Without an issue time changes nothing on the page.
        let mut f = Follower::new(Arc::default());
        f.snapshot(connected_snapshot(), at(0), &TimeZone::UTC, t0);
        assert!(f.tick(at(600), &TimeZone::UTC).is_empty());
    }

    #[test]
    fn os_detail_that_alternates_is_not_a_new_reason() {
        let _g = cg_i18n::lock_for_tests();
        let mut f = Follower::new(Arc::default());
        let mut gone = 0;
        for retry in 0..6 {
            let detail = if retry % 2 == 0 {
                "All pipe instances are busy."
            } else {
                "The system cannot find the file specified."
            };
            gone += f.down(unreachable(detail)).len();
        }
        assert_eq!(gone, 1);
        // Not in the group is another reason, and its own dot.
        let events = f.down(ServiceDown::new(DownReason::NotInGroup));
        let [
            Event::ServiceGone {
                not_in_group,
                chrome,
                ..
            },
        ] = &events[..]
        else {
            panic!("{events:?}");
        };
        assert!(*not_in_group);
        assert_eq!(chrome.badges.settings, ui::Badge::Warning);
    }

    #[test]
    fn service_gone_carries_the_chrome_for_its_reason() {
        let _g = cg_i18n::lock_for_tests();
        let mut f = Follower::new(Arc::default());
        let down = stopped();
        let events = f.down(down.clone());
        let [Event::ServiceGone { text, chrome, .. }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(*text, down.text);
        assert_eq!(*chrome, views::chrome(None, Some(&down)));
        assert_eq!(chrome.tray.title, down.text);
    }

    #[test]
    fn a_new_language_says_the_last_thing_again() {
        let _g = cg_i18n::lock_for_tests();
        let t0 = Instant::now();
        cg_i18n::set(cg_i18n::Lang::En);
        let shared = Arc::<Shared>::default();
        let mut f = Follower::new(shared.clone());
        f.snapshot(connected_snapshot(), at(0), &TimeZone::UTC, t0);
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let events = f.relabel(at(0), &TimeZone::UTC);
        let [Event::State { view }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(view.button_label, tr("gui.disconnect_long", &[]));
        // Even when nothing on the page reads differently, the State is sent again.
        assert_eq!(kinds(&f.relabel(at(0), &TimeZone::UTC)), ["state"]);
        // While the service is away, ServiceGone is said again in the new language.
        cg_i18n::set(cg_i18n::Lang::En);
        f.down(stopped());
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let events = f.relabel(at(1), &TimeZone::UTC);
        let [Event::ServiceGone { text, chrome, .. }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(*text, tr("gui.service_gone", &[]));
        assert_eq!(chrome.menu[2].label, tr("tray.connect", &[]));
        assert!(shared.snapshot().is_none());
        cg_i18n::set(cg_i18n::Lang::En);
    }

    #[test]
    fn the_snapshot_is_kept_in_one_place() {
        let _g = cg_i18n::lock_for_tests();
        let shared = Arc::<Shared>::default();
        let mut f = Follower::new(shared.clone());
        f.snapshot(connected_snapshot(), at(0), &TimeZone::UTC, Instant::now());
        assert_eq!(shared.snapshot(), Some(connected_snapshot()));
        f.down(stopped());
        assert_eq!(shared.snapshot(), None);
    }

    /// A runtime that reaches no service, and what it says.
    fn offline_runtime() -> (
        crate::runtime::Runtime,
        Arc<std::sync::Mutex<Vec<Event>>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let endpoint = Endpoint::Unix(dir.path().join("nobody.sock"));
        #[cfg(windows)]
        let endpoint = Endpoint::Pipe(format!(
            "{}ffi-unit-nobody-{}",
            cg_ipc::TEST_PIPE_PREFIX,
            std::process::id()
        ));
        let config = crate::runtime::Config {
            endpoint,
            prefs_path: dir.path().join("prefs.json"),
            history_path: dir.path().join("speedtests.json"),
            download_dir: None,
            retry: std::time::Duration::from_millis(50),
            tick: std::time::Duration::from_millis(50),
            ..crate::runtime::Config::production()
        };
        let events: Arc<std::sync::Mutex<Vec<Event>>> = Arc::default();
        let kept = events.clone();
        let rt =
            crate::runtime::Runtime::start(config, Box::new(move |e| kept.lock().unwrap().push(e)));
        (rt, events, dir)
    }

    fn wait_until(events: &std::sync::Mutex<Vec<Event>>, pred: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pred(&events.lock().unwrap()) {
            assert!(Instant::now() < deadline, "{:#?}", events.lock().unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_panicking_subscription_is_a_fault_and_can_be_started_again() {
        let (rt, events, _dir) = offline_runtime();
        let shared = rt.shared();
        // A snapshot from before (as if the service had been there): the panic drops it.
        shared.keep(Some(connected_snapshot()));
        shared.panic_next.store(true, Ordering::SeqCst);
        rt.send(crate::api::Command::Subscribe);
        wait_until(&events, |e| {
            e.iter()
                .any(|e| matches!(e, Event::Fault { message } if message.contains("panic")))
        });
        wait_until(&events, |_| !shared.subscribed.load(Ordering::SeqCst));
        assert_eq!(
            shared.snapshot(),
            None,
            "a panicked subscription keeps no snapshot"
        );
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, Event::ServiceGone { .. }))
        );
        rt.send(crate::api::Command::Subscribe);
        wait_until(&events, |e| {
            e.iter().any(|e| matches!(e, Event::ServiceGone { .. }))
        });
        assert!(shared.subscribed.load(Ordering::SeqCst));
        rt.shutdown();
    }
}
