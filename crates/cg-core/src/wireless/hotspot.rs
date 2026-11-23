//! The access point hand-over (`hotspot-v1`), on the cable pairing port, USB only: the
//! computer tells a paired phone its access point's name and passphrase, so the phone's app
//! can ask to join it (the phone defines the protocol).
//!
//! Computer to phone: `CG1 HOTSPOT <computerID> <ssid64> <pass64> <tag64>\n`, each field
//! base64url without padding; the tag is HMAC-SHA256, keyed by the pairing's 32-byte long-term
//! key, over the line before ` <tag64>`. Phone to computer: `CG1 HOTSPOT OK\n` (saved) or
//! `CG1 HOTSPOT UNKNOWN\n` (no such computer, or the tag is wrong); anything else, or the
//! connection closing, is a failure.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::encoding::{base64url, is_token};
use super::pairing::Key;

/// Capability name the phone reports when it takes `CG1 HOTSPOT` (with `wireless-v2`).
pub const CAPABILITY: &str = "hotspot-v1";
/// Longest request, in bytes, including the trailing newline.
pub const MAX_REQUEST: usize = 384;
/// Longest reply read, in bytes, including the trailing newline.
pub const MAX_REPLY: usize = 64;
/// How long the computer waits for the reply: the phone answers at once (nothing is asked).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Every access point name starts with this; the phone refuses any other.
const SSID_PREFIX: &str = "CellGlide-";

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Saved,
    Unknown,
}

/// The request line, including its trailing newline; it carries the passphrase, so it is wiped
/// when dropped. `None` when a field is outside what the phone takes (it would close without a
/// reply).
pub fn request(
    computer_id: &str,
    ssid: &str,
    passphrase: &str,
    key: &Key,
) -> Option<Zeroizing<String>> {
    let ssid_ok = ssid.starts_with(SSID_PREFIX) && ssid.len() <= 32;
    let passphrase_ok = (8..=63).contains(&passphrase.len())
        && passphrase.bytes().all(|b| (0x20..=0x7e).contains(&b));
    if !is_token(computer_id) || !ssid_ok || !passphrase_ok {
        return None;
    }
    let body = Zeroizing::new(format!(
        "CG1 HOTSPOT {computer_id} {} {}",
        base64url(ssid.as_bytes()),
        base64url(passphrase.as_bytes())
    ));
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_bytes())
        .expect("HMAC takes any key length");
    mac.update(body.as_bytes());
    let tag = mac.finalize().into_bytes();
    let line = Zeroizing::new(format!("{} {}\n", body.as_str(), base64url(&tag)));
    (line.len() <= MAX_REQUEST).then_some(line)
}

/// `line` is the reply without its trailing newline.
pub fn parse_reply(line: &str) -> Option<Reply> {
    match line {
        "CG1 HOTSPOT OK" => Some(Reply::Saved),
        "CG1 HOTSPOT UNKNOWN" => Some(Reply::Unknown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "8C1F2A3B-0D4E-4F56-9A7B-1C2D3E4F5A6B";

    fn key() -> Key {
        Key::from_bytes([7u8; 32])
    }

    #[test]
    fn the_ssid_is_encoded_as_the_phone_expects() {
        // The phone's test vector.
        let line = request(ID, "CellGlide-1HSL", "passphrase1", &key()).unwrap();
        let fields: Vec<&str> = line.trim_end_matches('\n').split(' ').collect();
        assert_eq!(fields[..3], ["CG1", "HOTSPOT", ID]);
        assert_eq!(fields[3], "Q2VsbEdsaWRlLTFIU0w");
        assert_eq!(fields[4], "cGFzc3BocmFzZTE");
        assert_eq!(fields.len(), 6);
        assert!(line.ends_with('\n') && !line.contains('\r'));
    }

    #[test]
    fn the_tag_is_hmac_sha256_over_the_line_before_it() {
        let line = request(ID, "CellGlide-1HSL", "passphrase1", &key()).unwrap();
        let line = line.trim_end_matches('\n');
        let (body, tag) = line.rsplit_once(' ').unwrap();
        assert_eq!(tag.len(), 43);
        assert_eq!(
            body,
            format!("CG1 HOTSPOT {ID} Q2VsbEdsaWRlLTFIU0w cGFzc3BocmFzZTE")
        );
        // Computed independently with .NET's HMACSHA256 (key 32 bytes of 7), base64url, no "=".
        assert_eq!(tag, "10EH2PXuUgies3MeBP8LfbW6auv3TjC3qULpgkpYTww");
    }

    #[test]
    fn fields_the_phone_refuses_are_not_sent() {
        let k = key();
        assert!(request(ID, "Other-1HSL", "passphrase1", &k).is_none());
        assert!(
            request(
                ID,
                &format!("CellGlide-{}", "A".repeat(23)),
                "passphrase1",
                &k
            )
            .is_none()
        );
        assert!(request(ID, "CellGlide-1HSL", "short", &k).is_none());
        assert!(request(ID, "CellGlide-1HSL", &"a".repeat(64), &k).is_none());
        assert!(request(ID, "CellGlide-1HSL", "pass\u{e9}phrase", &k).is_none());
        assert!(request(ID, "CellGlide-1HSL", "pass\nphrase", &k).is_none());
        assert!(request("not a token", "CellGlide-1HSL", "passphrase1", &k).is_none());
        assert!(
            request(
                ID,
                &format!("CellGlide-{}", "A".repeat(22)),
                &"a".repeat(63),
                &k
            )
            .is_some()
        );
    }

    #[test]
    fn only_the_two_replies_parse() {
        assert_eq!(parse_reply("CG1 HOTSPOT OK"), Some(Reply::Saved));
        assert_eq!(parse_reply("CG1 HOTSPOT UNKNOWN"), Some(Reply::Unknown));
        for other in [
            "",
            "CG1 HOTSPOT",
            "CG1 HOTSPOT OK ",
            "CG1 KNOWN x",
            "cg1 hotspot ok",
        ] {
            assert_eq!(parse_reply(other), None, "{other:?}");
        }
    }
}
