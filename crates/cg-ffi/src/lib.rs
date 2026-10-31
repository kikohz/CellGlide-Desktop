//! cellglide_native.dll: everything the Windows app needs except drawing. The app talks to it through
//! five C functions ([`ffi`]) carrying UTF-8 JSON: commands go in ([`api::Command`]), events
//! come back on the DLL's own thread ([`api::Event`]). The rlib exposes the same pieces to this
//! crate's integration tests; the hooks they use (module and feature `testing`) are not in the
//! DLL.

mod about;
pub mod api;
mod diagnostics;
pub mod ffi;
mod leak;
mod requests;
pub mod runtime;
mod speed;
mod subscription;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;
pub mod views;
