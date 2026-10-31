//! The C interface: five functions, UTF-8 JSON in and out. csbindgen reads this file
//! (`build.rs`) to write `windows/CellGlide.Core/Native/NativeMethods.g.cs`; commit both
//! together. No panic crosses it: every body runs inside [`guard_export`].
#![allow(non_camel_case_types)]

use crate::api::Event;
use crate::runtime::{Config, Runtime, lang_from};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::UnwindSafe;
use std::sync::{Mutex, PoisonError};

pub const CG_OK: i32 = 0;
/// `cg_init` while already initialised.
pub const CG_ALREADY: i32 = 1;
pub const CG_BAD_ARGUMENT: i32 = 2;
/// Something panicked inside the call; the DLL is still usable.
pub const CG_PANIC: i32 = -1;

/// Called on the DLL's event thread with one event as UTF-8 JSON, valid only during the call.
pub type cg_event_fn = Option<unsafe extern "C" fn(ctx: *mut c_void, json: *const u8, len: usize)>;

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

/// Runs `f`; a panic gives `fallback` instead of unwinding into the caller.
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

/// Starts the DLL: `lang` is a tag such as `zh-CN` (empty for the system's language) and
/// `on_event` receives every event, on the DLL's own thread. `CG_ALREADY` if it is running,
/// `CG_BAD_ARGUMENT` without a callback.
///
/// # Safety
/// `lang` must point to `len` readable bytes (or be null with `len` 0). `on_event` must be
/// callable with `ctx` from another thread until `cg_shutdown` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_init(
    lang: *const u8,
    len: usize,
    on_event: cg_event_fn,
    ctx: *mut c_void,
) -> i32 {
    // SAFETY: the caller's promise about `lang`.
    let lang = String::from_utf8_lossy(unsafe { bytes(lang, len) }).into_owned();
    guard_export(CG_PANIC, move || {
        let Some(on_event) = on_event else {
            return CG_BAD_ARGUMENT;
        };
        let mut runtime = RUNTIME.lock().unwrap_or_else(PoisonError::into_inner);
        if runtime.is_some() {
            return CG_ALREADY;
        }
        cg_i18n::set(lang_from(&lang));
        let callback = Callback { on_event, ctx };
        *runtime = Some(Runtime::start(
            Config::production(),
            Box::new(move |event| callback.call(&event)),
        ));
        CG_OK
    })
}

/// Sends one command (JSON); returns its number, from 1 up, which its `reply` event carries.
/// 0 when not initialised (or on a panic).
///
/// # Safety
/// `json` must point to `len` readable bytes (or be null with `len` 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_send(json: *const u8, len: usize) -> u64 {
    // SAFETY: the caller's promise about `json`.
    let json = unsafe { bytes(json, len) }.to_vec();
    guard_export(0, move || {
        let runtime = RUNTIME.lock().unwrap_or_else(PoisonError::into_inner);
        runtime
            .as_ref()
            .map_or(0, |runtime| runtime.send_json(&json))
    })
}

/// One text in the current language: `key`, with `args_json` such as `{"version":"2.4.0"}`
/// (empty or unreadable: no arguments). The text goes to `*out`/`*out_len` as UTF-8, to be
/// handed back to `cg_free`; null and 0 if something went wrong.
///
/// # Safety
/// `key` and `args_json` must each point to their length in readable bytes (or be null with
/// length 0); `out` and `out_len` must be writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_tr(
    key: *const u8,
    key_len: usize,
    args_json: *const u8,
    args_len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) {
    if out.is_null() || out_len.is_null() {
        return;
    }
    // SAFETY: the caller promises both are writable.
    unsafe {
        *out = std::ptr::null_mut();
        *out_len = 0;
    }
    // SAFETY: the caller's promises about `key` and `args_json`.
    let (key, args) = unsafe {
        (
            bytes(key, key_len).to_vec(),
            bytes(args_json, args_len).to_vec(),
        )
    };
    let text = guard_export(None, move || {
        let key = String::from_utf8_lossy(&key);
        let args: BTreeMap<String, String> = serde_json::from_slice(&args).unwrap_or_default();
        let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Some(cg_i18n::tr_os(&key, &args).into_bytes().into_boxed_slice())
    });
    let Some(text) = text else { return };
    let len = text.len();
    // SAFETY: as above; `cg_free` takes the box back with the same length.
    unsafe {
        *out = Box::into_raw(text).cast::<u8>();
        *out_len = len;
    }
}

/// Frees a text from `cg_tr`.
///
/// # Safety
/// `ptr` and `len` must be exactly what one `cg_tr` call gave, freed once; or `ptr` null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller hands back a box `cg_tr` made, with its length.
    let text = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) };
    guard_export((), move || drop(text));
}

/// Stops the DLL: once this returns, no more events. `cg_init` may start it again.
///
/// # Safety
/// Always safe to call; `unsafe` only to match the other exports.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_shutdown() {
    guard_export((), || {
        // Taken out under the lock and stopped outside it, so a callback calling `cg_send`
        // meanwhile gets 0 instead of waiting on the lock.
        let runtime = RUNTIME
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::Lang;
    use std::ptr::null_mut;

    /// These tests share one `RUNTIME` and the process-wide language.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        cg_i18n::lock_for_tests()
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
