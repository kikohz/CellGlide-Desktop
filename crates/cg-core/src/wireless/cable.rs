//! The one exchange on the cable pairing port (`CABLE_PAIRING_PORT`, USB only).
//!
//! Computer to phone: `CG1 CABLE <computerID> <base64url(computerName)>\n`.
//! Phone to computer, a new pairing:
//! `<phoneID> <identity> <base64(32-byte key)> <base64url(phoneName)>\n`; a computer the phone
//! already paired with: `CG1 KNOWN <phoneID>\n` (the key is never sent twice, so knowing a
//! computer ID is worth nothing). Then the connection closes.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::encoding::{decode_name, encode_name, is_token, is_valid_identity};
use super::pairing::Key;

/// Longest request, in bytes, including the trailing newline.
pub const MAX_REQUEST: usize = 256;
/// Longest reply read, in bytes, including the trailing newline.
pub const MAX_REPLY: usize = 512;
/// How long the computer waits for the reply: the phone may be showing a confirmation to its
/// user first.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(70);

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Known {
        phone_id: String,
    },
    New {
        phone_id: String,
        identity: String,
        key: Key,
        phone_name: String,
    },
}

/// The request line, including its trailing newline. The name is cut before encoding so the
/// line always fits `MAX_REQUEST`.
pub fn request(computer_id: &str, computer_name: &str) -> String {
    format!("CG1 CABLE {computer_id} {}\n", encode_name(computer_name))
}

/// `line` is the reply without its trailing newline. Anything that is not exactly one of the
/// two forms is `None`.
pub fn parse_reply(line: &str) -> Option<Reply> {
    if line.len() >= MAX_REPLY {
        return None;
    }
    let parts: Vec<&str> = line.split(' ').collect();
    if let [tag, kind, phone_id] = parts[..] {
        return (tag == "CG1" && kind == "KNOWN" && is_token(phone_id)).then(|| Reply::Known {
            phone_id: phone_id.to_string(),
        });
    }
    let [phone_id, identity, key, name] = parts[..] else {
        return None;
    };
    if !is_token(phone_id) || !is_valid_identity(identity) {
        return None;
    }
    // Standard base64 with padding here, unlike the url-safe, unpadded name fields.
    let key: [u8; 32] = STANDARD.decode(key).ok()?.try_into().ok()?;
    Some(Reply::New {
        phone_id: phone_id.to_string(),
        identity: identity.to_string(),
        key: Key::from_bytes(key),
        phone_name: decode_name(name)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTER_ID: &str = "7C1E5B52-2D6A-4A0F-8F3B-0A9D5E4C1B22";
    const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

    fn key_text(key: &[u8]) -> String {
        STANDARD.encode(key)
    }

    #[test]
    fn request_line_format() {
        let line = request(COMPUTER_ID, "Mac");
        assert_eq!(line, format!("CG1 CABLE {COMPUTER_ID} TWFj\n"));
        let line = request(COMPUTER_ID, "Elok’s MacBook Air");
        assert!(line.starts_with(&format!("CG1 CABLE {COMPUTER_ID} ")));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn long_names_stay_under_the_line_limit() {
        let line = request(COMPUTER_ID, &"名".repeat(200));
        assert!(line.len() < MAX_REQUEST, "{}", line.len());
        // Even with the longest allowed ID.
        let line = request(&"a".repeat(64), &"名".repeat(200));
        assert!(line.len() <= MAX_REQUEST, "{}", line.len());
    }

    #[test]
    fn known_reply() {
        assert_eq!(
            parse_reply("CG1 KNOWN ABC-1"),
            Some(Reply::Known {
                phone_id: "ABC-1".into()
            })
        );
        for bad in [
            "CG1 KNOWN",
            "CG1 KNOWN bad/id",
            "CG1 KNOWN ABC-1 ",
            "CG1 KNOWN  ABC-1",
            "CG2 KNOWN ABC-1",
            "cg1 known ABC-1",
        ] {
            assert_eq!(parse_reply(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn new_pairing_reply() {
        let key = [7u8; 32];
        let line = format!(
            "{PHONE_ID} {IDENTITY} {} {}",
            key_text(&key),
            encode_name("iPhone")
        );
        match parse_reply(&line) {
            Some(Reply::New {
                phone_id,
                identity,
                key: parsed,
                phone_name,
            }) => {
                assert_eq!(phone_id, PHONE_ID);
                assert_eq!(identity, IDENTITY);
                assert_eq!(parsed.as_bytes(), &key);
                assert_eq!(phone_name, "iPhone");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn phone_name_is_cleaned() {
        let line = format!(
            "{PHONE_ID} {IDENTITY} {} {}",
            key_text(&[1; 32]),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("ab\ncd")
        );
        let Some(Reply::New { phone_name, .. }) = parse_reply(&line) else {
            panic!("expected a new pairing");
        };
        assert_eq!(phone_name, "abcd");
    }

    #[test]
    fn malformed_replies_are_rejected() {
        let name = encode_name("iPhone");
        let key = key_text(&[7; 32]);
        let cases = [
            String::new(),
            format!("{PHONE_ID} {IDENTITY} {key}"),
            format!("{PHONE_ID} {IDENTITY} {key} {name} extra"),
            format!("{PHONE_ID}  {key} {name}"),
            // Wrong key sizes.
            format!("{PHONE_ID} {IDENTITY} {} {name}", key_text(&[0; 16])),
            format!("{PHONE_ID} {IDENTITY} {} {name}", key_text(&[0; 33])),
            // Unpadded key.
            format!("{PHONE_ID} {IDENTITY} {} {name}", key.trim_end_matches('=')),
            format!("{PHONE_ID} bad/id {key} {name}"),
            format!("{PHONE_ID} records {key} {name}"),
            format!("bad/id {IDENTITY} {key} {name}"),
            format!("{PHONE_ID} {IDENTITY} {key} not+base64"),
            format!("{PHONE_ID} {IDENTITY} {key} {}", "A".repeat(500)),
        ];
        for case in &cases {
            assert_eq!(parse_reply(case), None, "{case:?}");
        }
        // A key written with base64url characters is not standard base64.
        let with_url_chars = key_text(&[0xfb; 32]);
        assert!(with_url_chars.contains('+') || with_url_chars.contains('/'));
        let line = format!(
            "{PHONE_ID} {IDENTITY} {} {name}",
            with_url_chars.replace('+', "-").replace('/', "_")
        );
        assert_eq!(parse_reply(&line), None);
    }

    #[test]
    fn a_reply_of_512_bytes_or_more_is_rejected() {
        let key = key_text(&[7; 32]);
        // Pad the name up to the limit: the line is valid except for its length.
        let fixed = format!("{PHONE_ID} {IDENTITY} {key} ").len();
        let long = format!(
            "{PHONE_ID} {IDENTITY} {key} {}",
            "A".repeat(MAX_REPLY - fixed)
        );
        assert_eq!(long.len(), MAX_REPLY);
        assert_eq!(parse_reply(&long), None);
    }

    #[test]
    fn timing_and_limits() {
        assert_eq!(REPLY_TIMEOUT, Duration::from_secs(70));
        assert_eq!((MAX_REQUEST, MAX_REPLY), (256, 512));
    }
}
