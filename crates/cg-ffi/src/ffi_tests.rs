//! The C interface through a real C callback: what it receives, what it may call, and
//! `cg_init`/`cg_shutdown` from several threads at once. Kept out of `ffi.rs` so csbindgen
//! never sees the test callbacks. No test here sleeps to make a race come out right: a held
//! callback and the lifecycle's own state say when each step has happened.

use super::*;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

/// One test's hold on the DLL. Dropped (after a pass or a failure), it lets a held callback go
/// and shuts the DLL down, so a failed test can't leave it running for the next one; then it
/// releases the lock that serialises these tests (and other users of the language).
pub(super) struct Session {
    recorder: Option<&'static Recorder>,
    _lock: MutexGuard<'static, ()>,
}

impl Session {
    pub(super) fn new() -> Session {
        Session {
            recorder: None,
            _lock: cg_i18n::lock_for_tests(),
        }
    }

    fn with(recorder: &'static Recorder) -> Session {
        Session {
            recorder: Some(recorder),
            _lock: cg_i18n::lock_for_tests(),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(recorder) = self.recorder {
            recorder.hold.store(false, SeqCst);
        }
        shutdown();
    }
}

struct Seen {
    thread: ThreadId,
    at: Instant,
    ctx: usize,
    event: Value,
}

/// What the callback saw, and what it should do next. The callback never panics (a panic in
/// an `extern "C"` function aborts the process): it records problems as flags.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<Seen>>,
    /// While set, the callback waits before returning, so events queue up behind it.
    hold: AtomicBool,
    inside: AtomicUsize,
    /// Two callbacks ran at the same time.
    overlapped: AtomicBool,
    /// Some event's bytes weren't UTF-8 JSON.
    bad_json: AtomicBool,
    /// Call `cg_send` from the next callback.
    send_inside: AtomicBool,
    sent_inside: AtomicU64,
    /// Call `cg_shutdown` from the next callback (after any hold).
    shutdown_inside: AtomicBool,
    shutdown_inside_took: Mutex<Option<Duration>>,
}

impl Recorder {
    fn new() -> &'static Recorder {
        Box::leak(Box::default())
    }

    fn ctx(&'static self) -> *mut c_void {
        std::ptr::from_ref(self).cast_mut().cast()
    }

    fn seen(&self) -> MutexGuard<'_, Vec<Seen>> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn reply_ids(&self) -> Vec<u64> {
        self.seen()
            .iter()
            .filter(|seen| seen.event["type"] == "reply")
            .filter_map(|seen| seen.event["id"].as_u64())
            .collect()
    }

    fn shutdown_inside_took(&self) -> Option<Duration> {
        *self
            .shutdown_inside_took
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

unsafe extern "C" fn record(ctx: *mut c_void, json: *const u8, len: usize) {
    // SAFETY: every test passes a leaked `Recorder` as the context.
    let recorder = unsafe { &*ctx.cast::<Recorder>() };
    if recorder.inside.fetch_add(1, SeqCst) > 0 {
        recorder.overlapped.store(true, SeqCst);
    }
    // SAFETY: the DLL passes `len` bytes at `json` for the duration of the call.
    let bytes = unsafe { bytes(json, len) };
    match std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
    {
        Some(event) => recorder.seen().push(Seen {
            thread: std::thread::current().id(),
            at: Instant::now(),
            ctx: ctx as usize,
            event,
        }),
        None => recorder.bad_json.store(true, SeqCst),
    }
    if recorder.send_inside.swap(false, SeqCst) {
        recorder.sent_inside.store(send(), SeqCst);
    }
    while recorder.hold.load(SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    if recorder.shutdown_inside.swap(false, SeqCst) {
        let started = Instant::now();
        // SAFETY: always safe.
        unsafe { cg_shutdown() };
        *recorder
            .shutdown_inside_took
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(started.elapsed());
    }
    recorder.inside.fetch_sub(1, SeqCst);
}

fn init(recorder: &'static Recorder) -> i32 {
    // SAFETY: a valid tag; `record` may be called with a leaked recorder from any thread.
    unsafe { cg_init(b"en".as_ptr(), 2, Some(record), recorder.ctx()) }
}

fn send() -> u64 {
    let json = br#"{"type":"get_prefs"}"#;
    // SAFETY: a valid slice.
    unsafe { cg_send(json.as_ptr(), json.len()) }
}

fn shutdown() {
    // SAFETY: always safe.
    unsafe { cg_shutdown() }
}

fn is_stopping() -> bool {
    matches!(*state(), Lifecycle::Stopping { .. })
}

/// Threads waiting in `cg_init` or `cg_shutdown` for a stop to finish.
fn waiting() -> usize {
    WAITING.load(SeqCst)
}

/// Waits up to 10 s for `done`.
fn wait_until(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// Starts the DLL with the callback held inside the first of ten replies, the other nine
/// queued behind it.
fn start_held(recorder: &'static Recorder) {
    recorder.hold.store(true, SeqCst);
    assert_eq!(init(recorder), CG_OK);
    for _ in 0..10 {
        send();
    }
    assert!(wait_until(|| recorder.seen().len() == 1));
    assert!(wait_until(|| queued() == 9));
}

/// Events waiting for the event thread of the running DLL.
fn queued() -> usize {
    match &*state() {
        Lifecycle::Running(runtime) => runtime.queued(),
        Lifecycle::Stopped | Lifecycle::Stopping { .. } => 0,
    }
}

#[test]
fn the_callback_gets_its_context_and_json_events() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    assert_eq!(init(recorder), CG_OK);
    let id = send();
    assert!(wait_until(|| recorder.reply_ids().contains(&id)));
    shutdown();
    assert!(!recorder.bad_json.load(SeqCst));
    let seen = recorder.seen();
    assert!(seen.iter().all(|seen| seen.ctx == recorder.ctx() as usize));
    let reply = seen.iter().find(|seen| seen.event["id"] == id).unwrap();
    assert_eq!(reply.event["result"]["status"], "ok");
    assert_eq!(reply.event["result"]["value"]["kind"], "prefs");
}

#[test]
fn the_callback_can_send() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    recorder.send_inside.store(true, SeqCst);
    assert_eq!(init(recorder), CG_OK);
    send();
    assert!(wait_until(|| recorder.sent_inside.load(SeqCst) > 0));
    let inner = recorder.sent_inside.load(SeqCst);
    assert!(wait_until(|| recorder.reply_ids().contains(&inner)));
}

#[test]
fn several_threads_send_at_once() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    assert_eq!(init(recorder), CG_OK);
    let threads: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| (0..25).map(|_| send()).collect::<Vec<u64>>()))
        .collect();
    let mut ids: Vec<u64> = threads
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=100).collect::<Vec<u64>>());
    assert!(wait_until(|| recorder.reply_ids().len() == 100));
    shutdown();
    let mut replied = recorder.reply_ids();
    replied.sort_unstable();
    assert_eq!(replied, ids);
    assert!(!recorder.overlapped.load(SeqCst));
}

#[test]
fn concurrent_shutdowns_both_wait_for_the_stop() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    start_held(recorder);
    let stop = || {
        std::thread::spawn(|| {
            shutdown();
            Instant::now()
        })
    };
    let first = stop();
    assert!(wait_until(is_stopping));
    let second = stop();
    assert!(wait_until(|| waiting() == 1));
    // Both are in: the first stopping, the second waiting for it. Let the queue drain.
    recorder.hold.store(false, SeqCst);
    let returned = [first.join().unwrap(), second.join().unwrap()];
    let seen = recorder.seen();
    assert_eq!(
        seen.len(),
        10,
        "queued events are delivered before the stop ends"
    );
    for back in returned {
        assert!(seen.iter().all(|seen| seen.at < back));
    }
}

#[test]
fn init_during_a_shutdown_waits_for_it() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    start_held(recorder);
    let old_thread = recorder.seen()[0].thread;
    let stopper = std::thread::spawn(shutdown);
    assert!(wait_until(is_stopping));
    let starter = std::thread::spawn(move || {
        let code = init(recorder);
        (code, Instant::now())
    });
    assert!(wait_until(|| waiting() == 1));
    recorder.hold.store(false, SeqCst);
    stopper.join().unwrap();
    let (code, started) = starter.join().unwrap();
    assert_eq!(code, CG_OK);
    // The old runtime was fully stopped before the new one started.
    assert!(
        recorder
            .seen()
            .iter()
            .filter(|seen| seen.thread == old_thread)
            .all(|seen| seen.at < started)
    );
    // Numbers start again at 1 in the new runtime, so its reply is told apart by thread.
    let id = send();
    assert_eq!(id, 1);
    let from_new = || {
        recorder
            .seen()
            .iter()
            .any(|seen| seen.thread != old_thread && seen.event["id"] == id)
    };
    assert!(wait_until(from_new));
    shutdown();
    assert!(!recorder.overlapped.load(SeqCst));
}

#[test]
fn shutdown_from_the_callback_returns_at_once() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    recorder.shutdown_inside.store(true, SeqCst);
    start_held(recorder);
    recorder.hold.store(false, SeqCst);
    assert!(wait_until(|| recorder.shutdown_inside_took().is_some()));
    let took = recorder.shutdown_inside_took().unwrap();
    assert!(took < Duration::from_millis(500), "{took:?}");
    // A new start waits for the stop to finish, then works.
    assert_eq!(init(recorder), CG_OK);
    // Only the event whose callback shut down came from the first runtime.
    assert_eq!(recorder.seen().len(), 1);
    assert_eq!(send(), 1);
    shutdown();
    assert!(!recorder.overlapped.load(SeqCst));
}

#[test]
fn shutdown_from_the_callback_during_another_shutdown_ends_delivery() {
    let recorder = Recorder::new();
    let _session = Session::with(recorder);
    start_held(recorder);
    let stopper = std::thread::spawn(shutdown);
    assert!(wait_until(is_stopping));
    // The held callback now shuts down too, while the other thread is stopping.
    recorder.shutdown_inside.store(true, SeqCst);
    recorder.hold.store(false, SeqCst);
    stopper.join().unwrap();
    let took = recorder.shutdown_inside_took().unwrap();
    assert!(took < Duration::from_millis(500), "{took:?}");
    // The nine queued events were dropped, not delivered after that callback returned.
    assert_eq!(recorder.seen().len(), 1);
    assert!(!is_stopping());
}

#[test]
fn the_generated_csharp_has_every_return_code() {
    let csharp = include_str!("../../../windows/CellGlide.Core/Native/NativeMethods.g.cs");
    for (name, value) in CODES {
        let line = format!("internal const int {name} = {value};");
        assert!(csharp.contains(&line), "{line}");
    }
    assert_eq!(
        csharp.matches("internal const int CG_").count(),
        CODES.len()
    );
    assert_eq!(
        CODES,
        [
            ("CG_OK", CG_OK),
            ("CG_ALREADY", CG_ALREADY),
            ("CG_BAD_ARGUMENT", CG_BAD_ARGUMENT),
            ("CG_PANIC", CG_PANIC),
        ]
    );
    // The DLL is cellglide_native.dll: the bindings load that name.
    assert!(csharp.contains("const string __DllName = \"cellglide_native\";"));
}
