//! The wireless backup link between the computer and the iPhone. Everything here is
//! platform-neutral protocol logic that must match the phone's implementation byte for byte;
//! the OS-specific parts (access point, discovery, TLS) live in `cg-platform`.

pub mod cable;
pub mod cable_pairing;
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

/// What this computer is called when it has no usable name of its own.
pub const FALLBACK_COMPUTER_NAME: &str = "Windows PC";

/// The name this computer gives the phone on the cable request and the code pairing `M` line:
/// `raw` cleaned the way the line encodes it (`encoding::clean_name`), trimmed, and the fallback
/// when nothing is left. The phone refuses an empty name (and counts that toward its lockout), so
/// neither line is ever sent with one.
pub fn computer_name(raw: Option<&str>) -> String {
    let cleaned = encoding::clean_name(raw.unwrap_or_default());
    match cleaned.trim() {
        "" => FALLBACK_COMPUTER_NAME.to_string(),
        name => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_empty_computer_name_becomes_windows_pc() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("\u{0}\n\t\u{7f}"),
            Some(" \r\n "),
        ] {
            assert_eq!(computer_name(raw), "Windows PC", "{raw:?}");
        }
    }

    #[test]
    fn a_usable_computer_name_is_cleaned_and_kept() {
        assert_eq!(computer_name(Some("Desk PC")), "Desk PC");
        assert_eq!(computer_name(Some("  Desk\u{0} PC\n")), "Desk PC");
        assert_eq!(computer_name(Some(&"名".repeat(200))).len(), 120);
    }
}
