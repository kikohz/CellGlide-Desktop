//! Platform-neutral core: talks to usbmuxd and the phone, relays traffic,
//! answers DNS, and drives the connection state machine.

pub mod battery;
pub mod controller;
pub mod doh;
mod error;
pub mod exit_check;
pub mod forwarder;
pub mod health;
pub mod lockdown;
pub mod mux;
pub mod phone;
pub mod phone_feed;
pub mod resolver;
pub mod retry;
pub mod settings;
mod tls;
pub mod tunnel;
mod udp;
mod udp_bridge;
pub mod usb_speed;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::{CoreError, Result};
