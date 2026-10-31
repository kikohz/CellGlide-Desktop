//! The C interface through a real C callback: what it receives, what it may call, and
//! `cg_init`/`cg_shutdown` from several threads at once. Kept out of `ffi.rs` so csbindgen
//! never sees the test callbacks.

use super::*;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

/// These tests share the DLL's one lifecycle and the process-wide language.
fn serial() -> MutexGuard<'static, ()> {
    cg_i18n::lock_for_tests()
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
    /// How long each callback takes, so events queue up behind it.
    delay_ms: AtomicU64,
    inside: AtomicUsize,
    /// Two callbacks ran at the same time.
    overlapped: AtomicBool,
    /// Some event's bytes weren't UTF-8 JSON.
    bad_json: AtomicBool,
    /// Call `cg_send` from the next callback.
    send_inside: AtomicBool,
    sent_inside: AtomicU64,
    /// Call `cg_shutdown` from the next callback.
    shutdown_inside: AtomicBool,
    shutdown_inside_took: Mutex<Option<Duration>>,
}

impl Recorder {
    fn new(delay_ms: u64) -> &'static Recorder {
        let recorder: &'static Recorder = Box::leak(Box::default());
        recorder.delay_ms.store(delay_ms, SeqCst);
        recorder
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
    if recorder.shutdown_inside.swap(false, SeqCst) {
        let started = Instant::now();
        // SAFETY: always safe.
        unsafe { cg_shutdown() };
        *recorder
            .shutdown_inside_took
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(started.elapsed());
    }
    std::thread::sleep(Duration::from_millis(recorder.delay_ms.load(SeqCst)));
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

/// Waits up to 5 s for `done`.
fn wait_until(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

#[test]
fn the_callback_gets_its_context_and_json_events() {
    let _g = serial();
    let recorder = Recorder::new(0);
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
    let _g = serial();
    let recorder = Recorder::new(0);
    recorder.send_inside.store(true, SeqCst);
    assert_eq!(init(recorder), CG_OK);
    send();
    assert!(wait_until(|| recorder.sent_inside.load(SeqCst) > 0));
    let inner = recorder.sent_inside.load(SeqCst);
    assert!(wait_until(|| recorder.reply_ids().contains(&inner)));
    shutdown();
}

#[test]
fn several_threads_send_at_once() {
    let _g = serial();
    let recorder = Recorder::new(0);
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
    let _g = serial();
    let recorder = Recorder::new(20);
    assert_eq!(init(recorder), CG_OK);
    for _ in 0..10 {
        send();
    }
    assert!(wait_until(|| !recorder.seen().is_empty()));
    // Events are still queued behind the slow callback while both shut down.
    let stoppers: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                shutdown();
                Instant::now()
            })
        })
        .collect();
    let returned: Vec<Instant> = stoppers.into_iter().map(|t| t.join().unwrap()).collect();
    let first_return = returned.iter().min().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let seen = recorder.seen();
    assert!(seen.len() > 1, "nothing was queued: {}", seen.len());
    assert!(seen.iter().all(|seen| seen.at < *first_return));
}

#[test]
fn init_during_a_shutdown_waits_for_it() {
    let _g = serial();
    let recorder = Recorder::new(20);
    assert_eq!(init(recorder), CG_OK);
    for _ in 0..10 {
        send();
    }
    assert!(wait_until(|| !recorder.seen().is_empty()));
    let old_thread = recorder.seen()[0].thread;
    let stopper = std::thread::spawn(|| {
        shutdown();
        Instant::now()
    });
    std::thread::sleep(Duration::from_millis(30));
    // The stop is still draining the old queue (about 200 ms of callbacks).
    assert_eq!(init(recorder), CG_OK);
    let started = Instant::now();
    let stopped = stopper.join().unwrap();
    assert!(started >= stopped);
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
    assert!(
        recorder
            .seen()
            .iter()
            .filter(|seen| seen.thread == old_thread)
            .all(|seen| seen.at < stopped)
    );
}

#[test]
fn shutdown_from_the_callback_returns_at_once() {
    let _g = serial();
    let recorder = Recorder::new(20);
    recorder.shutdown_inside.store(true, SeqCst);
    assert_eq!(init(recorder), CG_OK);
    for _ in 0..10 {
        send();
    }
    let took = || {
        *recorder
            .shutdown_inside_took
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    };
    assert!(wait_until(|| took().is_some()));
    assert!(took().unwrap() < Duration::from_millis(500), "{:?}", took());
    // A new start waits for the stop to finish, then works.
    assert_eq!(init(recorder), CG_OK);
    std::thread::sleep(Duration::from_millis(100));
    // Only the event whose callback shut down came from the first runtime.
    assert_eq!(recorder.seen().len(), 1);
    assert_eq!(send(), 1);
    shutdown();
    assert!(!recorder.overlapped.load(SeqCst));
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
}
