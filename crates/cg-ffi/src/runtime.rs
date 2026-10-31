//! The DLL's engine: a tokio runtime with two workers runs the commands, and one dedicated
//! thread hands every event to the app, in order. Nothing here panics across the C interface:
//! each command runs under [`guarded`].

use crate::api::{Command, Event, ReplyResult, ReplyValue};
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    Queue(mpsc::Sender<Message>),
    Direct(Arc<Mutex<Sink>>),
}

impl Events {
    pub fn emit(&self, event: Event) {
        match &self.0 {
            // After shutdown the event thread is gone and the event is dropped, as it should be.
            Delivery::Queue(queue) => {
                let _ = queue.send(Message::Event(event));
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
struct Context {
    config: Arc<Config>,
    events: Events,
}

pub struct Runtime {
    /// `None` only once shut down.
    tokio: Option<tokio::runtime::Runtime>,
    cancel: CancellationToken,
    context: Context,
    queue: mpsc::Sender<Message>,
    event_thread: Option<JoinHandle<()>>,
    /// Set when the event thread itself shuts down: it then drops what is still queued.
    closed: Arc<AtomicBool>,
    next_id: AtomicU64,
}

impl Runtime {
    /// Starts the workers and the event thread, which calls `sink` for each event in order.
    pub fn start(config: Config, sink: Box<dyn Fn(Event) + Send + 'static>) -> Runtime {
        let (queue, received) = mpsc::channel::<Message>();
        let closed = Arc::new(AtomicBool::new(false));
        let thread_closed = closed.clone();
        let event_thread = std::thread::Builder::new()
            .name("cellglide-events".into())
            .spawn(move || {
                for message in received {
                    let Message::Event(event) = message else {
                        break;
                    };
                    if thread_closed.load(Ordering::SeqCst) {
                        break;
                    }
                    // The app's callback must not take the event thread down with it.
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| sink(event)));
                }
            })
            .expect("the event thread starts");
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cellglide-worker")
            .enable_all()
            .build()
            .expect("the tokio runtime starts");
        Runtime {
            tokio: Some(tokio),
            cancel: CancellationToken::new(),
            context: Context {
                config: Arc::new(config),
                events: Events(Delivery::Queue(queue.clone())),
            },
            queue,
            event_thread: Some(event_thread),
            closed,
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

    /// From now on the event thread delivers nothing more, not even what is already queued.
    pub(crate) fn drop_pending_events(&self) {
        self.closed.store(true, Ordering::SeqCst);
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
            // whatever is still queued once this callback returns.
            self.drop_pending_events();
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
            let result = run_guarded(id, events, async move { set_prefs(&path, &saved) }).await;
            if matches!(result, Some(ReplyResult::Ok { .. })) {
                events.emit(Event::Prefs { prefs });
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
        // Tasks 9 and 10.
        Command::Subscribe
        | Command::Toggle
        | Command::SpeedStart { .. }
        | Command::SpeedCancel
        | Command::SpeedHistoryClear
        | Command::LeakStart
        | Command::LeakCancel
        | Command::DiagnosticsPrepare { .. }
        | Command::DiagnosticsSend { .. }
        | Command::DiagnosticsSave { .. } => ReplyResult::err("not yet"),
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
