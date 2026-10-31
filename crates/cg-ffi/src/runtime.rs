//! The DLL's engine: a tokio runtime with two workers runs the commands, and one dedicated
//! thread hands every event to the app, in order. Nothing here panics across the C interface:
//! each command runs under [`guarded`].

use crate::api::{Command, Event, ReplyResult, ReplyValue};
use crate::subscription::Shared;
use cg_i18n::{Lang, tr};
use cg_ipc::Endpoint;
use cg_tools::https::Target;
use cg_tools::speed::{Route, Server};
use futures::FutureExt;
use futures::future::BoxFuture;
use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long `shutdown` lets running commands wind down before dropping them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Where the DLL finds the service, keeps its files and sends its measurements.
pub struct Config {
    pub endpoint: Endpoint,
    pub prefs_path: PathBuf,
    pub history_path: PathBuf,
    /// Where diagnostics are saved when the app names no path.
    pub download_dir: Option<PathBuf>,
    pub speed_server: Server,
    pub leak_target: Target,
    pub diag_target: cg_diag::upload::Target,
    /// This computer's own uplink, for the speed test's comparison.
    pub uplink: fn() -> Option<String>,
    /// The leak test's lookups; production is `dns_leak::system_resolve`.
    pub resolve: fn(String) -> BoxFuture<'static, ()>,
    /// How long to wait before subscribing again after the service went away; production 2 s.
    pub retry: Duration,
    /// How often views are recomputed and the uplink looked up again; production 5 s.
    pub tick: Duration,
    /// Tests send every measurement through `Route::Unbound`; production is `None`.
    #[doc(hidden)]
    pub route_override: Option<Route>,
}

impl Config {
    pub fn production() -> Config {
        Config {
            endpoint: Endpoint::system(),
            prefs_path: cg_tools::prefs::path(),
            history_path: cg_tools::speed::history::default_path(),
            download_dir: cg_diag::save::default_dir(),
            speed_server: Server::cloudflare(),
            leak_target: Target::leakcheck(),
            diag_target: cg_diag::upload::Target::production(),
            uplink: cg_tools::uplink::physical_uplink,
            resolve: cg_tools::dns_leak::system_resolve,
            retry: Duration::from_secs(2),
            tick: Duration::from_secs(5),
            route_override: None,
        }
    }
}

/// The app's language from a tag such as `zh-CN`; empty for the system's.
pub(crate) fn lang_from(tag: &str) -> Lang {
    if tag.is_empty() {
        Lang::from_env()
    } else {
        Lang::from_tag(tag)
    }
}

enum Message {
    Event(Event),
    /// Deliver what came before, then end the event thread.
    Stop,
}

type Sink = Box<dyn Fn(Event) + Send + 'static>;

/// Posts events to the event thread (or, for tests, straight to a sink).
#[derive(Clone)]
pub struct Events(Delivery);

#[derive(Clone)]
enum Delivery {
    /// The event thread's queue, and how many events wait in it.
    Queue(mpsc::Sender<Message>, Arc<AtomicUsize>),
    Direct(Arc<Mutex<Sink>>),
}

impl Events {
    pub fn emit(&self, event: Event) {
        match &self.0 {
            // After shutdown the event thread is gone and the event is dropped, as it should be.
            Delivery::Queue(queue, queued) => {
                queued.fetch_add(1, Ordering::SeqCst);
                if queue.send(Message::Event(event)).is_err() {
                    queued.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Delivery::Direct(sink) => {
                let sink = sink.lock().unwrap_or_else(PoisonError::into_inner);
                sink(event);
            }
        }
    }

    /// Calls `sink` on the emitting thread; no event thread.
    pub(crate) fn direct(sink: Sink) -> Events {
        Events(Delivery::Direct(Arc::new(Mutex::new(sink))))
    }
}

/// What a command's handler needs.
#[derive(Clone)]
pub(crate) struct Context {
    pub(crate) config: Arc<Config>,
    pub(crate) events: Events,
    /// What the subscription shares with `Toggle`, the speed test and the leak test.
    pub(crate) shared: Arc<Shared>,
    pub(crate) speed: Arc<crate::speed::Speed>,
    pub(crate) leak: Arc<crate::leak::Leak>,
    /// Held while the preferences are saved and announced: two `SetPrefs` at once are written
    /// one after the other, and their `Event::Prefs` go out in the order they were written.
    pub(crate) prefs: Arc<tokio::sync::Mutex<()>>,
    /// Cancelled at shutdown; tasks that outlive their command stop with it.
    pub(crate) cancel: CancellationToken,
}

type Finisher = Box<dyn FnOnce() + Send + 'static>;

/// Whether the event thread still delivers events.
#[derive(Clone, Default)]
pub(crate) struct EventGate(Arc<AtomicBool>);

impl EventGate {
    /// Nothing more is delivered, not even what is already queued.
    pub(crate) fn close(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_closed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Runtime {
    /// `None` only once shut down.
    tokio: Option<tokio::runtime::Runtime>,
    cancel: CancellationToken,
    context: Context,
    queue: mpsc::Sender<Message>,
    event_thread: Option<JoinHandle<()>>,
    /// Once closed, the event thread drops what is still queued.
    gate: EventGate,
    /// Run by the event thread as it ends, after its last callback.
    finisher: Arc<Mutex<Option<Finisher>>>,
    next_id: AtomicU64,
}

impl Runtime {
    /// Starts the workers and the event thread, which calls `sink` for each event in order.
    pub fn start(config: Config, sink: Box<dyn Fn(Event) + Send + 'static>) -> Runtime {
        let (queue, received) = mpsc::channel::<Message>();
        let queued = Arc::new(AtomicUsize::new(0));
        let thread_queued = queued.clone();
        let gate = EventGate::default();
        let thread_gate = gate.clone();
        let finisher: Arc<Mutex<Option<Finisher>>> = Arc::default();
        let thread_finisher = finisher.clone();
        let event_thread = std::thread::Builder::new()
            .name("cellglide-events".into())
            .spawn(move || {
                for message in received {
                    let Message::Event(event) = message else {
                        break;
                    };
                    thread_queued.fetch_sub(1, Ordering::SeqCst);
                    if thread_gate.is_closed() {
                        break;
                    }
                    // The app's callback must not take the event thread down with it.
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| sink(event)));
                }
                drop(sink);
                let finish = thread_finisher
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(finish) = finish {
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(finish));
                }
            })
            .expect("the event thread starts");
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cellglide-worker")
            .enable_all()
            .build()
            .expect("the tokio runtime starts");
        let cancel = CancellationToken::new();
        Runtime {
            tokio: Some(tokio),
            cancel: cancel.clone(),
            context: Context {
                config: Arc::new(config),
                events: Events(Delivery::Queue(queue.clone(), queued)),
                shared: Arc::default(),
                speed: Arc::default(),
                leak: Arc::default(),
                prefs: Arc::default(),
                cancel,
            },
            queue,
            event_thread: Some(event_thread),
            gate,
            finisher,
            next_id: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let Some(tokio) = &self.tokio else { return };
        let cancel = self.cancel.clone();
        tokio.spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = task => {}
            }
        });
    }

    /// Runs `command`; its [`Event::Reply`] carries the number returned (from 1 up).
    pub fn send(&self, command: Command) -> u64 {
        let id = self.next_id();
        let context = self.context.clone();
        self.spawn(dispatch(id, command, context));
        id
    }

    /// Like [`Runtime::send`] for a command as JSON. One that doesn't parse still gets a
    /// number, and its reply is an error.
    pub fn send_json(&self, json: &[u8]) -> u64 {
        match serde_json::from_slice::<Command>(json) {
            Ok(command) => self.send(command),
            Err(error) => {
                let id = self.next_id();
                let events = self.context.events.clone();
                let message = format!("{} ({error})", tr("reason.bad_request", &[]));
                self.spawn(async move {
                    guarded(id, &events, async move { ReplyResult::err(message) }).await
                });
                id
            }
        }
    }

    /// Cancels every command, stops the workers and the event thread, and returns once `sink`
    /// will never be called again. Safe from any thread, an async context included. Called
    /// from inside `sink` (the event thread), it can't wait for that thread: whatever is still
    /// queued is dropped once `sink` returns.
    pub fn shutdown(self) {
        drop(self);
    }

    /// The event thread, while it runs.
    pub(crate) fn event_thread(&self) -> Option<std::thread::ThreadId> {
        self.event_thread
            .as_ref()
            .map(|thread| thread.thread().id())
    }

    /// Events waiting for the event thread (not counting one being delivered).
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        match &self.context.events.0 {
            Delivery::Queue(_, queued) => queued.load(Ordering::SeqCst),
            Delivery::Direct(_) => 0,
        }
    }

    /// What the commands share (the subscription's state).
    #[cfg(test)]
    pub(crate) fn shared(&self) -> Arc<Shared> {
        self.context.shared.clone()
    }

    /// What every command's handler is given.
    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    /// The switch that stops delivery to `sink`, even what is already queued; it keeps working
    /// after the runtime itself has been handed elsewhere.
    pub(crate) fn gate(&self) -> EventGate {
        self.gate.clone()
    }

    /// Has the event thread run `finish` as it ends, after its last callback has returned and
    /// `sink` is dropped. Only meaningful while that thread runs (e.g. from inside `sink`).
    pub(crate) fn finish_on_event_thread(&self, finish: Finisher) {
        *self.finisher.lock().unwrap_or_else(PoisonError::into_inner) = Some(finish);
    }

    fn stop(&mut self) {
        let Some(tokio) = self.tokio.take() else {
            return;
        };
        self.cancel.cancel();
        stop_tokio(tokio);
        let Some(event_thread) = self.event_thread.take() else {
            return;
        };
        if event_thread.thread().id() == std::thread::current().id() {
            // Called from the app's callback: the thread can't wait for itself, so it drops
            // whatever is still queued once this callback returns and ends at the next message.
            self.gate.close();
            let _ = self.queue.send(Message::Stop);
        } else {
            let _ = self.queue.send(Message::Stop);
            let _ = event_thread.join();
        }
    }
}

/// Stops the workers, giving running tasks [`SHUTDOWN_GRACE`]. Never panics, an async
/// context included: dropping or shutting down a runtime inside one panics, so the wait
/// happens on a fresh thread, and without a thread to spare the runtime is let go at once
/// (`shutdown_background` is allowed anywhere).
fn stop_tokio(tokio: tokio::runtime::Runtime) {
    let slot = Arc::new(Mutex::new(Some(tokio)));
    let stopper_slot = slot.clone();
    let stopper = std::thread::Builder::new()
        .name("cellglide-shutdown".into())
        .spawn(move || {
            let tokio = stopper_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(tokio) = tokio {
                tokio.shutdown_timeout(SHUTDOWN_GRACE);
            }
        });
    match stopper {
        Ok(stopper) => {
            let _ = stopper.join();
        }
        Err(_) => {
            let tokio = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(tokio) = tokio {
                tokio.shutdown_background();
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn dispatch(id: u64, command: Command, context: Context) {
    let events = &context.events;
    match command {
        Command::SetPrefs { prefs } => {
            let path = context.config.prefs_path.clone();
            let saved = prefs.clone();
            let _one_at_a_time = context.prefs.lock().await;
            let result = run_guarded(id, events, async move { set_prefs(&path, &saved) }).await;
            if matches!(result, Some(ReplyResult::Ok { .. })) {
                events.emit(Event::Prefs { prefs });
                // "Also test Wi‑Fi or Ethernet" changes what the speed page says.
                context.shared.pages_changed();
            }
        }
        command => guarded(id, events, handle(command, &context)).await,
    }
}

async fn handle(command: Command, context: &Context) -> ReplyResult {
    let config = &context.config;
    match command {
        Command::SetLang { lang } => {
            cg_i18n::set(lang_from(&lang));
            // The page and the tray change language now, not at the next tick.
            context.shared.language_changed();
            ReplyResult::done()
        }
        Command::GetPrefs => ReplyResult::Ok {
            value: ReplyValue::Prefs {
                prefs: cg_tools::prefs::load(&config.prefs_path),
            },
        },
        // `dispatch` takes it: the reply is followed by `Event::Prefs`, which only it sends.
        Command::SetPrefs { .. } => unreachable!("dispatch handles SetPrefs"),
        Command::Request { request } => crate::requests::request(&config.endpoint, request).await,
        Command::Subscribe => crate::subscription::subscribe(context),
        Command::Toggle => crate::subscription::toggle(&config.endpoint, &context.shared).await,
        Command::DiagnosticsPrepare { range, email, note } => {
            crate::requests::diagnostics(&config.endpoint, range, &email, &note).await
        }
        Command::SpeedStart {
            include_wifi,
            confirmed,
            caps,
        } => crate::speed::start(context, include_wifi, confirmed, caps).await,
        Command::SpeedCancel => crate::speed::cancel(context),
        Command::SpeedHistoryClear => crate::speed::clear_history(context).await,
        Command::LeakStart => crate::leak::start(context),
        Command::LeakCancel => crate::leak::cancel(context),
        Command::DiagnosticsSend { json } => crate::diagnostics::send(context, &json).await,
        Command::DiagnosticsSave { json, path } => {
            crate::diagnostics::save(context, json, path).await
        }
    }
}

fn set_prefs(path: &std::path::Path, prefs: &cg_tools::prefs::Prefs) -> ReplyResult {
    match cg_tools::prefs::save(path, prefs) {
        Ok(()) => ReplyResult::done(),
        Err(error) => ReplyResult::err(tr("reason.internal", &[("detail", &error.to_string())])),
    }
}

/// Runs a command's handler: a panic becomes a [`Event::Fault`] and an error reply, anything
/// else the reply `f` gives.
pub async fn guarded<F: Future<Output = ReplyResult>>(id: u64, events: &Events, f: F) {
    run_guarded(id, events, f).await;
}

/// [`guarded`], also giving back the reply it sent (`None` after a panic).
async fn run_guarded<F: Future<Output = ReplyResult>>(
    id: u64,
    events: &Events,
    f: F,
) -> Option<ReplyResult> {
    match AssertUnwindSafe(f).catch_unwind().await {
        Ok(result) => {
            events.emit(Event::Reply {
                id,
                result: result.clone(),
            });
            Some(result)
        }
        Err(payload) => {
            let message = panic_message(&*payload);
            events.emit(Event::Fault {
                message: message.clone(),
            });
            events.emit(Event::Reply {
                id,
                result: ReplyResult::err(tr("reason.internal", &[("detail", &message)])),
            });
            None
        }
    }
}

pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_messages_are_kept() {
        let payload = std::panic::catch_unwind(|| panic!("static")).unwrap_err();
        assert_eq!(panic_message(&*payload), "static");
        let payload = std::panic::catch_unwind(|| panic!("{}", 42)).unwrap_err();
        assert_eq!(panic_message(&*payload), "42");
        let payload = std::panic::catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
        assert_eq!(panic_message(&*payload), "panic");
    }

    #[test]
    fn an_empty_tag_is_the_system_language() {
        assert_eq!(lang_from(""), Lang::from_env());
        assert_eq!(lang_from("zh-CN"), Lang::ZhCn);
        assert_eq!(lang_from("en-US"), Lang::En);
    }
}
