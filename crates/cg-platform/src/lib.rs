//! Platform tunnel backends: Linux, and the Windows pieces as they are built.

pub mod bypass;
pub mod ip;
pub mod resolved;

#[cfg(target_os = "linux")]
mod backend;
// Public only for the engine comparison test (tests/engine_parity.rs).
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub mod hev;
#[cfg(target_os = "linux")]
mod tun;

#[cfg(target_os = "linux")]
pub use backend::LinuxBackend;

#[cfg(windows)]
pub mod windows;
