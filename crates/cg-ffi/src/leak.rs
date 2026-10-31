//! The DNS leak test: `LeakStart` looks up the test names through `Config::resolve` and asks
//! `Config::leak_target` what it saw; [`Event::LeakProgress`] goes out when it starts and when
//! it ends, and again in a new language. `LeakCancel` stops it.

use crate::api::{Event, ReplyResult};
use crate::runtime::{Context, panic_message};
use crate::views::{self, LeakOutcome, LeakView};
use cg_ipc::{ConnState, DnsMode};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio_util::sync::CancellationToken;

/// The leak page as the DLL keeps it.
#[derive(Default)]
pub(crate) struct Leak {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The test going on; cancelling it stops it.
    run: Option<CancellationToken>,
    /// How DNS was handled when the last test started.
    sharing: Option<DnsMode>,
    outcome: Option<LeakOutcome>,
    /// The page as last sent.
    sent: Option<LeakView>,
}

impl Leak {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn view(state: &State) -> LeakView {
    views::leak(state.run.is_some(), state.outcome.as_ref(), state.sharing)
}

/// Sends the page when `force` or when it differs from the last one sent.
fn show(context: &Context, state: &mut State, force: bool) {
    let view = view(state);
    if force || state.sent.as_ref() != Some(&view) {
        state.sent = Some(view.clone());
        context.events.emit(Event::LeakProgress { view });
    }
}

/// The page again if it reads differently now (the language changed); the first time, the
/// page as it starts.
pub(crate) fn refresh(context: &Context) {
    let mut state = context.leak.lock();
    show(context, &mut state, false);
}

/// `Command::LeakStart`: `Done` once the test has started (or when one is already running).
pub(crate) fn start(context: &Context) -> ReplyResult {
    let mut state = context.leak.lock();
    if state.run.is_some() {
        return ReplyResult::done();
    }
    let sharing = match context.shared.snapshot().map(|s| s.state) {
        Some(ConnState::Connected { dns, .. }) => Some(dns),
        _ => None,
    };
    let cancel = context.cancel.child_token();
    state.run = Some(cancel.clone());
    state.sharing = sharing;
    state.outcome = None;
    show(context, &mut state, true);
    drop(state);
    let context = context.clone();
    tokio::spawn(async move {
        let config = &context.config;
        let test = AssertUnwindSafe(cg_tools::dns_leak::run(config.resolve, &config.leak_target))
            .catch_unwind();
        let ended = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            ended = test => ended,
        };
        let outcome = match ended {
            Ok(result) => result.map(|report| {
                let verdict = cg_tools::dns_leak::judge(&report, sharing);
                (report, verdict)
            }),
            Err(payload) => {
                context.events.emit(Event::Fault {
                    message: panic_message(&*payload),
                });
                Err(cg_tools::ToolError::BadReply)
            }
        };
        let mut state = context.leak.lock();
        // A cancel that came as the test ended wins: the page already says it stopped.
        if cancel.is_cancelled() {
            return;
        }
        state.run = None;
        state.outcome = Some(outcome);
        show(&context, &mut state, true);
    });
    ReplyResult::done()
}

/// `Command::LeakCancel`: stops the test; the page says it isn't running. `Done` either way.
pub(crate) fn cancel(context: &Context) -> ReplyResult {
    let mut state = context.leak.lock();
    if let Some(run) = state.run.take() {
        run.cancel();
    }
    show(context, &mut state, true);
    ReplyResult::done()
}
