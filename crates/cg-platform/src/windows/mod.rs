//! Windows tunnel pieces.

mod backend;
pub mod device;
pub mod dns;
pub mod ip;
mod leftovers;
pub mod strict_dns;
pub mod wintun;

pub use backend::WindowsBackend;
pub use leftovers::{cleanup_leftovers, cleanup_leftovers_with};
