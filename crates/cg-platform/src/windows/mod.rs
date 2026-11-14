//! Windows tunnel pieces.

#[cfg(feature = "wireless")]
pub mod access_point;
mod backend;
pub mod device;
pub mod dns;
pub mod ip;
mod leftovers;
#[cfg(feature = "wireless")]
pub mod mdns;
pub mod secrets;
pub mod strict_dns;
pub mod wintun;
#[cfg(feature = "wireless")]
pub mod wireless_ports;
#[cfg(feature = "wireless")]
pub mod wireless_tls;

pub use backend::WindowsBackend;
pub use leftovers::{cleanup_leftovers, cleanup_leftovers_with};
