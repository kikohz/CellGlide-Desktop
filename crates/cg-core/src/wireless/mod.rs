//! The wireless backup link between the computer and the iPhone. Everything here is
//! platform-neutral protocol logic that must match the phone's implementation byte for byte;
//! the OS-specific parts (access point, discovery, TLS) live in `cg-platform`.

pub mod cable;
pub mod code_message;
pub mod code_pairing;
pub mod encoding;
pub mod pairing;
pub mod request;
pub mod store;

/// Bonjour service type the phone advertises.
pub const SERVICE_TYPE: &str = "_cellglide-p2p._tcp";

/// The phone's loopback ports a paired computer may reach wirelessly: the proxy and the side
/// channels the USB link already uses. Nothing else is forwarded.
pub const ALLOWED_PORTS: [u16; 5] = [9876, 8088, 8089, 8090, 8091];

/// Hands out the long-term key over USB; never reachable wirelessly.
pub const CABLE_PAIRING_PORT: u16 = 8092;

/// Capability name the phone reports when it supports this wireless protocol.
pub const CAPABILITY: &str = "wireless-v2";
