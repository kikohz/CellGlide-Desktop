//! Platform tunnel backends. Only Linux is implemented.

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
