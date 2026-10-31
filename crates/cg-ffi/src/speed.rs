//! The speed test: `SpeedStart`, `SpeedCancel`, `SpeedHistoryClear`, and [`Event::SpeedProgress`]
//! whenever the page changes. A run measures the phone and, to compare, this computer's own
//! Wi‑Fi or Ethernet (`cg_tools::speed::session`), sends the page every `REFRESH` while it
//! runs, and adds the finished run to the history. Once the app subscribes, the page is also
//! sent again when the uplink changes (looked up every `Config::tick` while no test runs), when
//! the phone comes or goes, and when the preferences or the language change.

use crate::api::{Event, ReplyResult, SpeedCaps};
use crate::runtime::{Context, panic_message};
use crate::views::{self, SpeedDialog, SpeedLeg, SpeedPage, SpeedView};
use cg_i18n::tr;
use cg_ipc::describe;
use cg_ipc::{ConnState, Snapshot};
use cg_tools::speed::session::{self, Run};
use cg_tools::speed::text::error_text;
use cg_tools::speed::{
    HistoryOp, HistoryStore, Params, Plan, Progress, Record, Route, SpeedError, Stage, history,
    needs_confirmation, plan,
};
use futures::FutureExt;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Cloudflare refuses single downloads of 100 MB or more (as `Params::default`).
const MAX_REQUEST: u64 = 90_000_000;

/// The speed page as the DLL keeps it.
#[derive(Default)]
pub(crate) struct Speed {
    state: Mutex<State>,
    /// The page follows the uplink, the snapshot, the preferences and the language.
    watching: AtomicBool,
    /// Tests only (`testing::panic_speed_watch_next`): the page's watch panics when it next
    /// wakes up.
    pub(crate) panic_next: AtomicBool,
}

#[derive(Default)]
struct State {
    /// The last uplink looked up.
    uplink: Option<String>,
    dialog: Option<SpeedDialog>,
    /// The run going on; cancelling it stops it.
    run: Option<CancellationToken>,
    /// One per route of the last run.
    legs: Vec<SpeedLeg>,
    /// The recent results, newest first; read from the file the first time they're needed.
    records: Option<Vec<Record>>,
    history_failed: bool,
    /// The page as last sent.
    sent: Option<SpeedView>,
}

impl Speed {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Connected through a phone that shows its card, as the Linux window's `Ctx::phone_connected`.
pub(crate) fn phone_connected(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|s| matches!(s.state, ConnState::Connected { .. }) && s.phone.is_some())
}

/// The page now. Sent when `force` or when it differs from the last one sent; the lock is held
/// while sending so pages go out in the order they were made.
fn show(context: &Context, state: &mut State, force: bool) {
    let config = &context.config;
    let records = state.records.get_or_insert_with(|| {
        HistoryStore::open(config.history_path.clone())
            .records()
            .to_vec()
    });
    let snapshot = context.shared.snapshot();
    let prefs = cg_tools::prefs::load(&config.prefs_path);
    let view = views::speed(
        &SpeedPage {
            running: state.run.is_some(),
            dialog: state.dialog,
            phone: phone_connected(snapshot.as_ref()),
            uplink: state.uplink.as_deref(),
            include_wifi: prefs.include_wifi,
            legs: &state.legs,
            records,
            history_failed: state.history_failed,
        },
        &TimeZone::system(),
    );
    if force || state.sent.as_ref() != Some(&view) {
        state.sent = Some(view.clone());
        context.events.emit(Event::SpeedProgress { view });
    }
}

/// This computer's own uplink, off the async workers (it asks the OS).
async fn uplink(context: &Context) -> Option<String> {
    let find = context.config.uplink;
    tokio::task::spawn_blocking(find).await.unwrap_or(None)
}

/// After `Subscribe`: sends the page now, then whenever something it shows changes. Once per
/// runtime; it ends with the runtime. A panic is a `Fault`, and after `Config::retry` the page
/// is followed again by itself (`Subscribe` doesn't start it twice).
pub(crate) fn watch(context: &Context) {
    if context.speed.watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let context = context.clone();
    tokio::spawn(async move {
        let cancel = context.cancel.clone();
        loop {
            let run = AssertUnwindSafe(follow(&context)).catch_unwind();
            let ended = tokio::select! {
                () = cancel.cancelled() => None,
                ended = run => Some(ended),
            };
            let Some(Err(payload)) = ended else { break };
            context.events.emit(Event::Fault {
                message: panic_message(&*payload),
            });
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(context.config.retry) => {}
            }
        }
        context.speed.watching.store(false, Ordering::SeqCst);
    });
}

async fn follow(context: &Context) {
    let mut tick = tokio::time::interval(context.config.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let looked_up = tokio::select! {
            _ = tick.tick() => {
                let idle = context.speed.lock().run.is_none();
                if idle { Some(uplink(context).await) } else { None }
            }
            () = context.shared.pages.notified() => None,
        };
        if context.speed.panic_next.swap(false, Ordering::SeqCst) {
            panic!("a test asked the speed page's watch to panic");
        }
        {
            let mut state = context.speed.lock();
            if let Some(found) = looked_up
                && state.run.is_none()
            {
                state.uplink = found;
            }
            // The warning is about phone data: with the phone gone it no longer applies.
            if state.dialog.is_some() && !phone_connected(context.shared.snapshot().as_ref()) {
                state.dialog = None;
            }
            show(context, &mut state, false);
        }
        crate::leak::refresh(context);
    }
}

/// The measuring parameters: the default test, or a small one within `caps`.
fn params(caps: Option<&SpeedCaps>) -> Params {
    match caps {
        // Upload request sizes are the engine's own (`UPLOAD_REQUEST`); `request_size` is for
        // downloads only.
        Some(caps) => Params {
            streams: 2,
            duration: Duration::from_secs(u64::from(caps.seconds)),
            download_cap: caps.download_bytes,
            upload_cap: caps.upload_bytes,
            request_size: MAX_REQUEST.min(caps.download_bytes),
            ..Params::default()
        },
        None => Params::default(),
    }
}

/// `Command::SpeedStart`.
pub(crate) async fn start(
    context: &Context,
    include_wifi: bool,
    confirmed: bool,
    caps: Option<SpeedCaps>,
) -> ReplyResult {
    if context.speed.lock().run.is_some() {
        return ReplyResult::err(tr("speed.already_running", &[]));
    }
    let found = uplink(context).await;
    let snapshot = context.shared.snapshot();
    let config = &context.config;
    let mut state = context.speed.lock();
    // Looked again: another start may have begun while the uplink was looked up.
    if state.run.is_some() {
        return ReplyResult::err(tr("speed.already_running", &[]));
    }
    state.uplink = found.clone();
    state.dialog = None;
    let Some(plan) = plan(phone_connected(snapshot.as_ref()), found, include_wifi) else {
        show(context, &mut state, false);
        return ReplyResult::err(error_text(&SpeedError::NotConnected));
    };
    let prefs = cg_tools::prefs::load(&config.prefs_path);
    if needs_confirmation(&plan, confirmed || prefs.speed_confirmed) {
        state.dialog = Some(SpeedDialog::DataWarning);
        show(context, &mut state, true);
        return ReplyResult::done();
    }
    // P17: the carrier and radio as reported, never the card's placeholder title.
    let card = plan
        .phone
        .then(|| snapshot.as_ref().and_then(|s| s.phone.as_ref()))
        .flatten()
        .map(describe::card_text);
    let carrier = card.as_ref().and_then(|c| c.carrier.clone());
    let generation = card.and_then(|c| c.radio);
    let routes = session::labelled_routes(&plan);
    state.legs = routes
        .iter()
        .map(|(route, _label)| SpeedLeg {
            route: route.clone(),
            progress: Progress::default(),
        })
        .collect();
    let cancel = context.cancel.child_token();
    state.run = Some(cancel.clone());
    show(context, &mut state, true);
    drop(state);
    let job = Job {
        plan,
        routes,
        carrier,
        generation,
        params: params(caps.as_ref()),
    };
    let context = context.clone();
    tokio::spawn(async move {
        let outcome = AssertUnwindSafe(run(&context, job, &cancel))
            .catch_unwind()
            .await;
        if let Err(payload) = outcome {
            context.events.emit(Event::Fault {
                message: panic_message(&*payload),
            });
            // Only this run clears `run`, so no other run can have started meanwhile.
            let mut state = context.speed.lock();
            state.run = None;
            end_legs(&mut state.legs, None);
            show(&context, &mut state, true);
        }
    });
    ReplyResult::done()
}

struct Job {
    plan: Plan,
    routes: Vec<(Route, String)>,
    carrier: Option<String>,
    generation: Option<String>,
    params: Params,
}

async fn run(context: &Context, job: Job, cancel: &CancellationToken) {
    let config = &context.config;
    let server = config.speed_server.clone();
    let route_override = config.route_override.clone();
    let params = job.params;
    // Legs are measured in order, so the n-th call is leg n. Each leg's page shows how it
    // ended as soon as it ends, not only once the whole run is over.
    let mut next = 0;
    let measure = move |route: Route, tx: watch::Sender<Progress>, cancel| {
        let index = next;
        next += 1;
        let route = route_override.clone().unwrap_or(route);
        let measuring =
            cg_tools::speed::measure(route, server.clone(), params.clone(), tx.clone(), cancel);
        let context = context.clone();
        async move {
            let result = measuring.await;
            let mut state = context.speed.lock();
            if let Some(leg) = state.legs.get_mut(index) {
                leg.progress = tx.borrow().clone();
            }
            show(&context, &mut state, false);
            result
        }
    };
    let run = session::run_legs(job.routes, cancel, measure, |index, _label, progress| {
        let mut state = context.speed.lock();
        if let Some(leg) = state.legs.get_mut(index) {
            leg.progress = progress.clone();
        }
        show(context, &mut state, false);
    })
    .await;
    // After a cancel (the user's or shutdown's) nothing is recorded.
    let record = session::history_record(
        &job.plan,
        &run,
        Timestamp::now(),
        job.carrier,
        job.generation,
    );
    let saved = match record {
        Some(record) => Some(save(context, HistoryOp::Add(record)).await),
        None => None,
    };
    let mut state = context.speed.lock();
    if let Some(saved) = saved {
        keep_history(&mut state, saved);
    }
    state.run = None;
    end_legs(&mut state.legs, Some(&run));
    show(context, &mut state, true);
}

/// The legs as the run ended; one cut short by a cancel says so.
fn end_legs(legs: &mut [SpeedLeg], run: Option<&Run>) {
    for (index, view) in legs.iter_mut().enumerate() {
        view.progress.waiting = false;
        match run.and_then(|run| run.legs.get(index)) {
            Some(leg) => {
                view.progress.stage = leg.stage.clone();
                view.progress.result = leg.result.clone();
                if leg.stage == Stage::Done {
                    view.progress.progress = 1.0;
                }
            }
            None if !matches!(view.progress.stage, Stage::Done | Stage::Failed(_)) => {
                view.progress.stage = Stage::Cancelled;
            }
            None => {}
        }
    }
}

/// Changes the history file, off the async workers.
async fn save(context: &Context, op: HistoryOp) -> Result<Vec<Record>, String> {
    let path = context.config.history_path.clone();
    tokio::task::spawn_blocking(move || history::apply(path, op))
        .await
        .unwrap_or_else(|error| Err(error.to_string()))
}

fn keep_history(state: &mut State, saved: Result<Vec<Record>, String>) {
    match saved {
        Ok(records) => {
            state.records = Some(records);
            state.history_failed = false;
        }
        Err(_) => state.history_failed = true,
    }
}

/// `Command::SpeedCancel`: stops the run, or puts the data warning away; `Done` either way.
pub(crate) fn cancel(context: &Context) -> ReplyResult {
    let mut state = context.speed.lock();
    if let Some(run) = &state.run {
        run.cancel();
    }
    if state.dialog.take().is_some() {
        show(context, &mut state, true);
    }
    ReplyResult::done()
}

/// `Command::SpeedHistoryClear`.
pub(crate) async fn clear_history(context: &Context) -> ReplyResult {
    let saved = save(context, HistoryOp::Clear).await;
    let mut state = context.speed.lock();
    keep_history(&mut state, saved);
    show(context, &mut state, true);
    ReplyResult::done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{connected_snapshot, disconnected_snapshot};

    #[test]
    fn caps_make_a_small_two_stream_test_and_leave_upload_requests_alone() {
        let small = params(Some(&SpeedCaps {
            download_bytes: 20_000_000,
            upload_bytes: 10_000_000,
            seconds: 5,
        }));
        assert_eq!(
            small,
            Params {
                streams: 2,
                duration: Duration::from_secs(5),
                download_cap: 20_000_000,
                upload_cap: 10_000_000,
                request_size: 20_000_000,
                ..Params::default()
            }
        );
        let big = params(Some(&SpeedCaps {
            download_bytes: 500_000_000,
            upload_bytes: 1,
            seconds: 1,
        }));
        assert_eq!(big.request_size, MAX_REQUEST);
        assert_eq!(params(None), Params::default());
    }

    #[test]
    fn the_phone_counts_when_connected_with_its_card() {
        assert!(phone_connected(Some(&connected_snapshot())));
        assert!(!phone_connected(Some(&disconnected_snapshot())));
        assert!(!phone_connected(None));
        let mut no_card = connected_snapshot();
        no_card.phone = None;
        assert!(!phone_connected(Some(&no_card)));
    }

    #[test]
    fn a_leg_cut_short_reads_cancelled_and_a_finished_one_keeps_its_end() {
        let leg = |stage| SpeedLeg {
            route: Route::Phone,
            progress: Progress {
                stage,
                waiting: true,
                ..Progress::default()
            },
        };
        let mut legs = vec![leg(Stage::Upload), leg(Stage::Done)];
        end_legs(&mut legs, None);
        assert_eq!(legs[0].progress.stage, Stage::Cancelled);
        assert_eq!(legs[1].progress.stage, Stage::Done);
        assert!(legs.iter().all(|l| !l.progress.waiting));
    }
}
