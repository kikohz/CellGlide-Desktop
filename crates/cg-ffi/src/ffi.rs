//! The C interface: five functions, UTF-8 JSON in and out. csbindgen reads this file
//! (`build.rs`) to write `windows/CellGlide.Core/Native/NativeMethods.g.cs`, return codes
//! included; commit both together. No panic crosses it: each body, its pointer reads
//! included, runs inside [`guard_export`]. Doc comments here become C# XML comments, so they
//! use no angle brackets or ampersands.
//!
//! Rules for the app's callback (`on_event`):
//! - It runs on the DLL's own event thread, one event at a time, in order. It may call
//!   `cg_send` and `cg_tr`, and even `cg_shutdown` (which then returns at once and leaves the
//!   rest of the stop to another thread).
//! - `cg_shutdown` waits for the event thread, and so does a `cg_init` that arrives while a
//!   shutdown is under way. The callback must therefore never wait synchronously for a thread
//!   that is in `cg_shutdown` or `cg_init` (for instance a blocking dispatch to the UI
//!   thread): hand events on asynchronously instead.
//! - Call `cg_shutdown` before the DLL is unloaded or the process exits; until then the DLL's
//!   threads keep running and may call the callback.
#![allow(non_camel_case_types)]

use crate::api::Event;
use crate::runtime::{Config, EventGate, Runtime, lang_from};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::UnwindSafe;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;

include!("codes.rs");

/// Called on the DLL's event thread with one event as UTF-8 JSON, valid only during the call.
pub type cg_event_fn = Option<unsafe extern "C" fn(ctx: *mut c_void, json: *const u8, len: usize)>;

/// Where the DLL is in its life. Only `cg_init` and `cg_shutdown` change it; `cg_send` only
/// looks, so it never waits for a stop.
enum Lifecycle {
    Stopped,
    Running(Runtime),
    /// A `cg_shutdown` is stopping the runtime whose event thread and delivery these are.
    Stopping {
        event_thread: Option<ThreadId>,
        gate: EventGate,
    },
}

static STATE: Mutex<Lifecycle> = Mutex::new(Lifecycle::Stopped);
/// Signalled when a stop has finished.
static STOPPED: Condvar = Condvar::new();
/// How many threads wait in `cg_init` or `cg_shutdown` for a stop to finish (for tests).
#[cfg(test)]
static WAITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn state() -> MutexGuard<'static, Lifecycle> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait(guard: MutexGuard<'static, Lifecycle>) -> MutexGuard<'static, Lifecycle> {
    #[cfg(test)]
    WAITING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let guard = STOPPED.wait(guard).unwrap_or_else(PoisonError::into_inner);
    #[cfg(test)]
    WAITING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    guard
}

/// Marks the stop finished when dropped once armed, even if stopping panicked, so no one
/// waits forever. Made before the state lock is taken (so on a panic it drops after the lock
/// is released) and armed under the lock, as the state becomes `Stopping`.
struct StopFinished {
    armed: bool,
}

impl Drop for StopFinished {
    fn drop(&mut self) {
        if self.armed {
            *state() = Lifecycle::Stopped;
            STOPPED.notify_all();
        }
    }
}

/// A runtime on its way out; fields drop in order, so the stop is finished before it is
/// announced.
struct Stopping {
    runtime: Runtime,
    finished: StopFinished,
}

/// Runs `f`; a panic gives `fallback` instead of unwinding into the caller. The panic's
/// payload (a message) is dropped here, after the catch.
pub(crate) fn guard_export<T>(fallback: T, f: impl FnOnce() -> T + UnwindSafe) -> T {
    std::panic::catch_unwind(f).unwrap_or(fallback)
}

/// The bytes at `ptr`, or nothing for a null pointer.
///
/// # Safety
/// A non-null `ptr` must point to `len` readable bytes that stay put during the call.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller promises `len` readable bytes at `ptr`.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// The app's callback and its context; the app promises it may be called from any thread.
struct Callback {
    on_event: unsafe extern "C" fn(ctx: *mut c_void, json: *const u8, len: usize),
    ctx: *mut c_void,
}

// SAFETY: the C contract makes `on_event` callable with `ctx` from the DLL's own thread.
unsafe impl Send for Callback {}

impl Callback {
    fn call(&self, event: &Event) {
        let json = serde_json::to_vec(event).expect("events always serialize");
        // SAFETY: the app gave this function and context to be called with an event's bytes.
        unsafe { (self.on_event)(self.ctx, json.as_ptr(), json.len()) }
    }
}

/// Starts the DLL: lang is a tag such as zh-CN (empty for the system's language) and
/// on_event receives every event, on the DLL's own thread (see the callback rules in the
/// module documentation). CG_ALREADY if it is running, CG_BAD_ARGUMENT without a callback.
/// While a cg_shutdown is still stopping, this waits for it to finish and then starts; called
/// from that stopping runtime's own callback it returns CG_ALREADY instead of waiting.
///
/// # Safety
/// lang must point to len readable bytes (or be null with len 0). on_event must be
/// callable with ctx from another thread until cg_shutdown returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_init(
    lang: *const u8,
    len: usize,
    on_event: cg_event_fn,
    ctx: *mut c_void,
) -> i32 {
    guard_export(CG_PANIC, move || {
        let Some(on_event) = on_event else {
            return CG_BAD_ARGUMENT;
        };
        // SAFETY: the caller's promise about `lang`.
        let lang = String::from_utf8_lossy(unsafe { bytes(lang, len) }).into_owned();
        let me = std::thread::current().id();
        let mut state = state();
        loop {
            match &*state {
                Lifecycle::Stopped => break,
                Lifecycle::Running(_) => return CG_ALREADY,
                // Waiting here would wait for this very thread.
                Lifecycle::Stopping { event_thread, .. } if *event_thread == Some(me) => {
                    return CG_ALREADY;
                }
                Lifecycle::Stopping { .. } => state = wait(state),
            }
        }
        cg_i18n::set(lang_from(&lang));
        let callback = Callback { on_event, ctx };
        *state = Lifecycle::Running(Runtime::start(
            Config::production(),
            Box::new(move |event| callback.call(&event)),
        ));
        CG_OK
    })
}

/// Sends one command (JSON); returns its number, from 1 up, which its reply event carries.
/// 0 when not running, while stopping, or on a panic. Any thread may call it, the callback
/// included, and several threads at once.
///
/// # Safety
/// json must point to len readable bytes (or be null with len 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_send(json: *const u8, len: usize) -> u64 {
    guard_export(0, move || {
        // SAFETY: the caller's promise about `json`.
        let json = unsafe { bytes(json, len) };
        match &*state() {
            Lifecycle::Running(runtime) => runtime.send_json(json),
            Lifecycle::Stopped | Lifecycle::Stopping { .. } => 0,
        }
    })
}

/// One text in the current language: key, with args_json such as {"version":"2.4.0"}
/// (empty or unreadable: no arguments). The text goes to *out and *out_len as UTF-8 and must
/// be handed back to cg_free. An empty text is a non-null pointer with length 0, and it too
/// must go to cg_free. If something went wrong, *out is null and *out_len 0, and there is
/// nothing to free.
///
/// # Safety
/// key and args_json must each point to their length in readable bytes (or be null with
/// length 0); out and out_len must be writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_tr(
    key: *const u8,
    key_len: usize,
    args_json: *const u8,
    args_len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) {
    guard_export((), move || {
        if out.is_null() || out_len.is_null() {
            return;
        }
        // SAFETY: the caller promises both are writable. Cleared first, so a panic below
        // leaves null and 0.
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        // SAFETY: the caller's promises about `key` and `args_json`.
        let (key, args) = unsafe { (bytes(key, key_len), bytes(args_json, args_len)) };
        let key = String::from_utf8_lossy(key);
        let args: BTreeMap<String, String> = serde_json::from_slice(args).unwrap_or_default();
        let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let text = cg_i18n::tr_os(&key, &args).into_bytes().into_boxed_slice();
        let len = text.len();
        // SAFETY: as above; `cg_free` takes the box back with the same length.
        unsafe {
            *out = Box::into_raw(text).cast::<u8>();
            *out_len = len;
        }
    })
}

/// Frees a text from cg_tr, empty ones included.
///
/// # Safety
/// ptr and len must be exactly what one cg_tr call gave, freed once; or ptr null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_free(ptr: *mut u8, len: usize) {
    guard_export((), move || {
        if ptr.is_null() {
            return;
        }
        // SAFETY: the caller hands back a box `cg_tr` made, with its length.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    })
}

/// Stops the DLL: once this returns, no more events. cg_init may start it again. While
/// another cg_shutdown is stopping, this waits until that stop has finished. Called from the
/// callback, it returns at once, and no events are delivered after that callback returns;
/// the stop then finishes without this thread (and, if it was this call that began it, is
/// announced only after the callback has returned).
///
/// # Safety
/// Always safe to call; unsafe only to match the other exports.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_shutdown() {
    guard_export((), || {
        let me = std::thread::current().id();
        // Before the lock, so that on a panic it drops after the lock is released.
        let mut finished = StopFinished { armed: false };
        let mut state = state();
        let runtime = loop {
            match &*state {
                Lifecycle::Stopped => return,
                // The callback of the runtime another thread is stopping: that thread waits for
                // this callback, so don't wait for it; just deliver nothing more.
                Lifecycle::Stopping { event_thread, gate } if *event_thread == Some(me) => {
                    gate.close();
                    return;
                }
                Lifecycle::Stopping { .. } => state = wait(state),
                Lifecycle::Running(runtime) => {
                    let stopping = Lifecycle::Stopping {
                        event_thread: runtime.event_thread(),
                        gate: runtime.gate(),
                    };
                    let Lifecycle::Running(runtime) = std::mem::replace(&mut *state, stopping)
                    else {
                        unreachable!("matched Running above");
                    };
                    finished.armed = true;
                    break runtime;
                }
            }
        };
        // Stopped outside the lock, so a callback calling `cg_send` meanwhile gets 0 instead of
        // waiting on it.
        drop(state);
        let reentrant = runtime.event_thread() == Some(me);
        let stopping = Stopping { runtime, finished };
        if !reentrant {
            drop(stopping);
            return;
        }
        // This thread can't wait for itself: nothing more is delivered once this callback
        // returns, and another thread finishes the stop.
        stopping.runtime.gate().close();
        let slot = std::sync::Arc::new(Mutex::new(Some(stopping)));
        let stopper_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("cellglide-stop".into())
            .spawn(move || {
                let stopping = stopper_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                drop(stopping);
            });
        if spawned.is_err() {
            // No thread to spare: stop all but this thread now, and let the event thread
            // announce the stop itself once this callback has returned and it has ended.
            let stopping = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(Stopping { runtime, finished }) = stopping {
                runtime.finish_on_event_thread(Box::new(move || drop(finished)));
                drop(runtime);
            }
        }
    })
}

#[cfg(test)]
#[path = "ffi_tests.rs"]
mod callback_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::Lang;
    use std::ptr::null_mut;

    /// These tests share the DLL's one lifecycle and the process-wide language.
    /// Shuts the DLL down when dropped, so a failure here can't leave it running.
    fn serial() -> super::callback_tests::Session {
        super::callback_tests::Session::new()
    }

    unsafe extern "C" fn noop(_ctx: *mut c_void, _json: *const u8, _len: usize) {}

    fn tr_via_ffi(key: &str, args: &str) -> String {
        let mut out = null_mut();
        let mut len = 0;
        // SAFETY: valid slices and out-pointers.
        unsafe {
            cg_tr(
                key.as_ptr(),
                key.len(),
                args.as_ptr(),
                args.len(),
                &mut out,
                &mut len,
            )
        };
        assert!(!out.is_null());
        // SAFETY: `cg_tr` gave `len` bytes at `out`.
        let text = String::from_utf8(unsafe { std::slice::from_raw_parts(out, len) }.to_vec());
        // SAFETY: exactly what `cg_tr` gave.
        unsafe { cg_free(out, len) };
        text.unwrap()
    }

    #[test]
    fn init_twice_is_refused() {
        let _g = serial();
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, Some(noop), null_mut()) },
            CG_OK
        );
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, Some(noop), null_mut()) },
            CG_ALREADY
        );
        unsafe { cg_shutdown() };
    }

    #[test]
    fn send_after_shutdown_returns_zero() {
        let _g = serial();
        assert_eq!(
            unsafe { cg_init(b"".as_ptr(), 0, Some(noop), null_mut()) },
            CG_OK
        );
        let json = br#"{"type":"get_prefs"}"#;
        assert!(unsafe { cg_send(json.as_ptr(), json.len()) } > 0);
        unsafe { cg_shutdown() };
        assert_eq!(unsafe { cg_send(json.as_ptr(), json.len()) }, 0);
    }

    #[test]
    fn init_again_after_shutdown() {
        let _g = serial();
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, Some(noop), null_mut()) },
            CG_OK
        );
        unsafe { cg_shutdown() };
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, Some(noop), null_mut()) },
            CG_OK
        );
        unsafe { cg_shutdown() };
    }

    #[test]
    fn null_callback_is_a_bad_argument() {
        let _g = serial();
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, None, null_mut()) },
            CG_BAD_ARGUMENT
        );
    }

    #[test]
    fn tr_returns_text_in_current_language() {
        let _g = serial();
        assert_eq!(
            unsafe { cg_init(b"en".as_ptr(), 2, Some(noop), null_mut()) },
            CG_OK
        );
        assert_eq!(
            tr_via_ffi("nav.overview", ""),
            cg_i18n::tr_in(Lang::En, "nav.overview", &[])
        );
        assert!(tr_via_ffi("state.connected", r#"{"version":"2.4.0"}"#).contains("2.4.0"));
        unsafe { cg_shutdown() };
    }

    #[test]
    fn unreadable_arguments_are_no_arguments() {
        let _g = serial();
        cg_i18n::set(Lang::En);
        let bare = cg_i18n::tr_os("state.connected", &[]);
        assert_eq!(tr_via_ffi("state.connected", "{not json"), bare);
        assert_eq!(tr_via_ffi("state.connected", r#"{"version":1}"#), bare);
    }

    #[test]
    fn tr_without_somewhere_to_write_does_nothing() {
        let key = "nav.overview";
        let mut len = 7;
        // SAFETY: a null `out` is allowed.
        unsafe { cg_tr(key.as_ptr(), key.len(), null_mut(), 0, null_mut(), &mut len) };
        assert_eq!(len, 7);
        // SAFETY: freeing null is allowed.
        unsafe { cg_free(null_mut(), 0) };
    }

    #[test]
    fn export_panics_never_unwind() {
        assert_eq!(guard_export(7, || panic!("boom")), 7);
    }
}
