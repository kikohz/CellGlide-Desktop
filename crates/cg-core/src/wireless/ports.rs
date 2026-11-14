//! What the session needs from the platform for the wireless link. The Windows implementations
//! live in `cg-platform`; tests use in-memory stand-ins.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

use crate::Result;
use crate::mux::MuxStream;

/// A boxed, sendable future (the `futures` crate's `BoxFuture`, without the dependency).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An established wireless link to one paired phone. Like usbmuxd over the cable, it opens
/// connections to the phone's loopback ports, so everything above it works the same way.
pub trait WirelessLink: Send + Sync {
    /// The phone's random id (its Bonjour instance name).
    fn phone_id(&self) -> &str;

    /// A new connection to `port` on the phone.
    fn open(&self, port: u16) -> BoxFuture<'_, Result<MuxStream>>;
}

/// A phone advertising the CellGlide service on the access point.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoundPhone {
    /// The service instance name: the phone's identifier (phoneID).
    pub instance: String,
    pub address: IpAddr,
    pub port: u16,
}

/// Why a TLS connection to the phone did not come up.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DialError {
    /// The phone does not know the PSK identity (TLS alert 115, unknown_psk_identity).
    #[error("the phone does not know this pairing")]
    UnknownIdentity,
    #[error("no connection within the time limit")]
    Timeout,
    /// Anything else; text for logs only.
    #[error("{0}")]
    Io(String),
}
