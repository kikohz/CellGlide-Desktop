//! The speed test: the phone through `cellglide0` and, to compare, this computer's own Wi‑Fi or
//! Ethernet, each socket bound to its interface with SO_BINDTODEVICE so no route ever changes.
//! Same server, durations and math as the Mac's SharedSpeed.

pub mod engine;
pub mod history;
pub mod run;
pub mod session;
pub mod text;
pub mod transport;

pub use engine::{Note, Params, Progress, SpeedResult, Stage, Verdict, measure, verdict};
pub use history::{HistoryStore, Record, Summary, default_path};
pub use run::{Plan, needs_confirmation, plan, record};
pub use transport::{Route, Server, SpeedError, Stream, connect};
