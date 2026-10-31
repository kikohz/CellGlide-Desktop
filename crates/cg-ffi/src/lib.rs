//! cellglide.dll: everything the Windows app needs except drawing. The app talks to it through
//! five C functions ([`ffi`]) carrying UTF-8 JSON: commands go in ([`api::Command`]), events
//! come back on the DLL's own thread ([`api::Event`]). The rlib exposes the same pieces to this
//! crate's integration tests.

pub mod api;
pub mod ffi;
mod requests;
pub mod runtime;
#[doc(hidden)]
pub mod testing;
pub mod views;
