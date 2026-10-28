//! Tools that run in the user's own processes (CLI, GUI). The service links this crate only
//! through cg-diag, which uses `net` for its upload client; it never runs the tools.

pub mod dns_leak;
pub mod https;
pub mod net;
pub mod paths;
pub mod speed;
pub mod uplink;

/// Why a tool could not produce a result.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// Could not connect, TLS failed, or the exchange timed out. Holds the technical reason.
    #[error("offline: {0}")]
    Offline(String),
    /// The server answered with something other than 200.
    #[error("the server answered {0}")]
    Status(u16),
    /// The reply was not what the protocol says.
    #[error("unreadable reply")]
    BadReply,
}

pub use net::user_agent;
