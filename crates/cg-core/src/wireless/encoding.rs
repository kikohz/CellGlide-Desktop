//! Text encodings shared by the wireless line formats: base64url fields, plain tokens, and
//! names the other side chose.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Names are cut to this many UTF-8 bytes before encoding, so a request always fits.
pub const MAX_NAME_BYTES: usize = 120;
/// Longest name shown in a sentence, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 40;
const MAX_TOKEN_LENGTH: usize = 64;

/// Quote characters stripped from names shown in a sentence, as the phone's app does.
const QUOTES: [char; 20] = [
    '"', '\'', '`', '´', '“', '”', '‘', '’', '„', '‚', '«', '»', '‹', '›', '「', '」', '『', '』',
    '＂', '＇',
];

/// Unicode general category Cf (format: direction overrides, zero-width joiners, ...). The
/// standard library has no category lookup, so the ranges are listed.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
        | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Unpadded base64url, the encoding of every binary field on the pairing lines.
pub fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Only the canonical form: no padding, no stray characters, no non-zero trailing bits. The
/// phone never sends anything else, and two spellings of one value would let a peer slip
/// differing bytes past a transcript hash.
pub fn decode_base64url(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

/// 1-64 characters of A-Z, a-z, 0-9 and "-".
pub fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TOKEN_LENGTH
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Names the key store keeps its own bookkeeping under; a pairing's identity must never
/// collide with one.
const RESERVED_ACCOUNTS: [&str; 4] = ["records", "local-id", "usb-devices", "stale"];

/// A PSK identity the key store accepts as a key's account: a plain token that is never one
/// of the bookkeeping accounts.
pub fn is_valid_identity(identity: &str) -> bool {
    is_token(identity) && !RESERVED_ACCOUNTS.contains(&identity)
}

/// Control characters dropped, cut on a character boundary to `MAX_NAME_BYTES`.
pub fn clean_name(name: &str) -> String {
    let mut result = String::new();
    for c in name.chars().filter(|c| !c.is_control()) {
        if result.len() + c.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        result.push(c);
    }
    result
}

pub fn encode_name(name: &str) -> String {
    base64url(clean_name(name).as_bytes())
}

pub fn decode_name(text: &str) -> Option<String> {
    let data = decode_base64url(text)?;
    if data.len() > MAX_NAME_BYTES {
        return None;
    }
    Some(clean_name(&String::from_utf8(data).ok()?))
}

/// A name the other side chose, made safe to put in a sentence: no quotes, no control or
/// format characters (direction overrides, zero-width), whitespace collapsed, at most
/// `MAX_DISPLAY_NAME_CHARS` characters.
pub fn display_name(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in name.chars().filter(|c| !QUOTES.contains(c)) {
        if c.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if !c.is_control() && !is_format(c) {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let cut: String = joined.chars().take(MAX_DISPLAY_NAME_CHARS).collect();
    cut.trim_end_matches(' ').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_is_unpadded_and_url_safe() {
        assert_eq!(base64url(b"a"), "YQ");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url(b""), "");
    }

    #[test]
    fn decode_base64url_accepts_only_canonical_unpadded() {
        assert_eq!(decode_base64url("YQ"), Some(b"a".to_vec()));
        assert_eq!(decode_base64url("-_8"), Some(vec![0xfb, 0xff]));
        for bad in ["YQ==", "YQ=", "Y", "Y+", "YR", "Y/", "Y Q", "YQ\n"] {
            assert_eq!(decode_base64url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn tokens() {
        assert!(is_token("ABC-1"));
        assert!(is_token(&"a".repeat(64)));
        assert!(!is_token(""));
        assert!(!is_token(&"a".repeat(65)));
        assert!(!is_token("a_b"));
        assert!(!is_token("a b"));
        assert!(!is_token("é"));
    }

    #[test]
    fn identities_are_tokens_that_avoid_bookkeeping_accounts() {
        assert!(is_valid_identity("2F9C1A4E-0B7D-4E55-9A3C-123456789ABC"));
        for reserved in ["records", "local-id", "usb-devices", "stale"] {
            assert!(!is_valid_identity(reserved), "{reserved}");
        }
        assert!(is_valid_identity("records2"));
        assert!(!is_valid_identity(""));
        assert!(!is_valid_identity("bad/id"));
        assert!(!is_valid_identity(&"a".repeat(65)));
    }

    #[test]
    fn name_round_trips() {
        assert_eq!(encode_name("Mac"), "TWFj");
        assert_eq!(decode_name("TWFj").as_deref(), Some("Mac"));
        let name = "Elok’s MacBook Air";
        assert_eq!(decode_name(&encode_name(name)).as_deref(), Some(name));
        assert_eq!(decode_name("").as_deref(), Some(""));
    }

    #[test]
    fn decode_name_rejects_bad_input() {
        for bad in ["not+base64", "//8", "TWFj=", "T", "TWFjT", "/w", "_wA"] {
            assert_eq!(decode_name(bad), None, "{bad:?}");
        }
        // Invalid UTF-8 (0xff).
        assert_eq!(decode_name("_w"), None);
        // More than 120 bytes of payload.
        assert_eq!(decode_name(&base64url(&[b'a'; 121])), None);
        assert!(decode_name(&base64url(&[b'a'; 120])).is_some());
    }

    #[test]
    fn clean_name_drops_controls_and_cuts_on_a_character_boundary() {
        assert_eq!(clean_name("a\u{0}b\nc\u{7f}d"), "abcd");
        // 118 ASCII bytes + one 3-byte character would be 121 bytes: the character is dropped.
        let input = format!("{}名", "a".repeat(118));
        assert_eq!(input.len(), 121);
        assert_eq!(clean_name(&input), "a".repeat(118));
        // Exactly 120 bytes fits.
        let fits = format!("{}名", "a".repeat(117));
        assert_eq!(clean_name(&fits), fits);
        // A long name stays valid UTF-8 and under the limit.
        assert_eq!(clean_name(&"名".repeat(200)).len(), 120);
    }

    #[test]
    fn display_name_is_safe_for_a_sentence() {
        assert_eq!(
            display_name("  “Eve”\u{202E} 的\tiPhone  "),
            "Eve 的 iPhone"
        );
        assert_eq!(display_name("a\u{200B}b \u{0}c"), "ab c");
        assert_eq!(display_name(&"x".repeat(41)), "x".repeat(40));
        // A cut that lands on a joining space leaves no trailing space.
        let name = format!("{} y", "x".repeat(39));
        assert_eq!(display_name(&name), "x".repeat(39));
        assert_eq!(display_name("\"'`"), "");
    }
}
