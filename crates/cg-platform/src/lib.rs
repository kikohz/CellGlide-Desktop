//! Platform tunnel backends: Linux, and the Windows pieces as they are built.

pub mod bypass;
pub mod ip;
pub mod resolved;

#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod hev;
#[cfg(target_os = "linux")]
mod tun;

#[cfg(target_os = "linux")]
pub use backend::LinuxBackend;

#[cfg(windows)]
pub mod windows;
