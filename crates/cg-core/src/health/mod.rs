//! Connection health: the judge that names an issue from probe results, the window that
//! turns the phone's connect counters into a failure rate, the seven-day timeline, and the
//! monitor that ties them to the running service.

pub mod judge;
pub mod log;
pub mod monitor;
pub mod window;
