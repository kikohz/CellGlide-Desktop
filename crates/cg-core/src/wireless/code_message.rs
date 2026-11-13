//! The lines of code pairing on the wireless pairing channel (TLS with the public identity
//! `PAIRING_IDENTITY`), after the computer's `CG1 PAIR` request line. One line each, binary
//! fields unpadded base64url, names base64url of UTF-8:
//!
//! 1. phone to computer: `C <commitment> <phoneID>`
//! 2. computer to phone: `M <pub> <nonce> <computerID> <computerName>`
//! 3. phone to computer: `P <pub> <nonce>`; the computer checks the commitment, both show the code
//! 4. phone to computer: `OK <identity> <phoneName> <phoneProof>` (its user allowed) or `NO`
//! 5. computer to phone: `F <computerProof>` once its own user confirmed the code and the
//!    phone's proof checked out
//!
//! Both sides hash every line exactly as sent or received (`Transcript`: C, M, P, then
//! `OK <identity> <phoneName>`), and each proof is HMAC-SHA256 under the proof key derived from
//! the new long-term key over a label and that hash, so the proofs bind what the code does not:
//! ids, names and the identity. The computer's side of the wire is `computer`, `parse_phone`,
//! `parse_commitment`, `parse_answer` and `finished`; the others serve the phone's side (the
//! tests' stand-in for it).

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::encoding::{
    base64url, decode_base64url, decode_name, encode_name, is_token, is_valid_identity,
};
use super::pairing::{Key, NONCE_LENGTH, proof_key};

/// Longest line read, in bytes, including the trailing newline; field parsing additionally
/// refuses a line of this many bytes without it.
pub const MAX_LINE_LENGTH: usize = 512;
/// The computer waits this long for the phone's next line.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(60);
/// The phone waits this long for the computer's first line (M).
pub const FIRST_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// The computer's window for both users' answers and the phone's OK, counted from when it shows
/// the code (right after P).
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(75);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Allowed {
        identity: String,
        phone_name: String,
        proof: [u8; 32],
        /// The `OK <identity> <phoneName>\n` part as received, for the transcript.
        signed: String,
    },
    Denied,
}

/// SHA-256 over every pairing line (with its newline) exactly as sent or received.
#[derive(Clone, Default)]
pub struct Transcript {
    hash: Sha256,
}

impl Transcript {
    pub fn new() -> Transcript {
        Transcript::default()
    }

    pub fn add(&mut self, line: &str) {
        self.hash.update(line.as_bytes());
    }

    pub fn digest(&self) -> [u8; 32] {
        self.hash.clone().finalize().into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Phone,
    Computer,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::Phone => "CG1 phone ok",
            Side::Computer => "CG1 mac ok",
        }
    }
}

/// HMAC-SHA256(proof_key(long_term), label || transcript digest).
pub fn proof(side: Side, long_term: &Key, transcript: &Transcript) -> [u8; 32] {
    let key = proof_key(long_term);
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(side.label().as_bytes());
    mac.update(&transcript.digest());
    mac.finalize().into_bytes().into()
}

/// Constant-time; a proof of the wrong length is simply false.
pub fn verify(proof_bytes: &[u8], side: Side, long_term: &Key, transcript: &Transcript) -> bool {
    let expected = proof(side, long_term, transcript);
    expected.as_slice().ct_eq(proof_bytes).into()
}

/// Cut off at `MAX_LINE_LENGTH`: no valid line comes near it, so this bounds the work done on
/// hostile input.
fn too_long(line: &str) -> bool {
    line.len() >= MAX_LINE_LENGTH
}

/// The fields after `tag`, exactly `count` of them, none empty.
fn fields<'a>(line: &'a str, tag: &str, count: usize) -> Option<Vec<&'a str>> {
    if too_long(line) {
        return None;
    }
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != count + 1 || parts[0] != tag || parts[1..].iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(parts[1..].to_vec())
}

fn fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    decode_base64url(text)?.try_into().ok()
}

pub fn commitment(commitment: &[u8; 32], phone_id: &str) -> String {
    format!("C {} {phone_id}\n", base64url(commitment))
}

/// `line` is without its newline.
pub fn parse_commitment(line: &str) -> Option<([u8; 32], String)> {
    let parts = fields(line, "C", 2)?;
    let commitment = fixed::<32>(parts[0])?;
    is_token(parts[1]).then(|| (commitment, parts[1].to_string()))
}

pub fn computer(
    public_key: &[u8; 32],
    nonce: &[u8; NONCE_LENGTH],
    computer_id: &str,
    computer_name: &str,
) -> String {
    format!(
        "M {} {} {computer_id} {}\n",
        base64url(public_key),
        base64url(nonce),
        encode_name(computer_name)
    )
}

pub struct ComputerLine {
    pub public_key: [u8; 32],
    pub nonce: [u8; NONCE_LENGTH],
    pub computer_id: String,
    pub computer_name: String,
}

pub fn parse_computer(line: &str) -> Option<ComputerLine> {
    let parts = fields(line, "M", 4)?;
    if !is_token(parts[2]) {
        return None;
    }
    Some(ComputerLine {
        public_key: fixed(parts[0])?,
        nonce: fixed(parts[1])?,
        computer_id: parts[2].to_string(),
        computer_name: decode_name(parts[3])?,
    })
}

pub fn phone(public_key: &[u8; 32], nonce: &[u8; NONCE_LENGTH]) -> String {
    format!("P {} {}\n", base64url(public_key), base64url(nonce))
}

pub fn parse_phone(line: &str) -> Option<([u8; 32], [u8; NONCE_LENGTH])> {
    let parts = fields(line, "P", 2)?;
    Some((fixed(parts[0])?, fixed(parts[1])?))
}

/// The part of the OK line the phone's proof covers (it goes into the transcript first).
pub fn allowed_signed(identity: &str, phone_name: &str) -> String {
    format!("OK {identity} {}\n", encode_name(phone_name))
}

pub fn allowed(identity: &str, phone_name: &str, proof: &[u8; 32]) -> String {
    let signed = allowed_signed(identity, phone_name);
    format!("{} {}\n", signed.trim_end_matches('\n'), base64url(proof))
}

pub const DENIED: &str = "NO\n";

pub fn parse_answer(line: &str) -> Option<Answer> {
    if line == "NO" {
        return Some(Answer::Denied);
    }
    let parts = fields(line, "OK", 3)?;
    if !is_valid_identity(parts[0]) {
        return None;
    }
    Some(Answer::Allowed {
        identity: parts[0].to_string(),
        phone_name: decode_name(parts[1])?,
        proof: fixed(parts[2])?,
        signed: format!("OK {} {}\n", parts[0], parts[1]),
    })
}

pub fn finished(proof: &[u8; 32]) -> String {
    format!("F {}\n", base64url(proof))
}

pub fn parse_finished(line: &str) -> Option<[u8; 32]> {
    fixed(fields(line, "F", 1)?[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wireless::pairing::{Role, Session};

    const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const COMPUTER_ID: &str = "7C1E5B52-2D6A-4A0F-8F3B-0A9D5E4C1B22";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

    fn b(start: u8, n: usize) -> Vec<u8> {
        (0..n).map(|i| start.wrapping_add(i as u8)).collect()
    }

    fn b32(start: u8) -> [u8; 32] {
        b(start, 32).try_into().unwrap()
    }

    fn b16(start: u8) -> [u8; 16] {
        b(start, 16).try_into().unwrap()
    }

    fn body(line: &str) -> &str {
        line.strip_suffix('\n').expect("ends with a newline")
    }

    #[test]
    fn lines_round_trip() {
        let commitment_line = commitment(&b32(1), PHONE_ID);
        assert_eq!(
            parse_commitment(body(&commitment_line)),
            Some((b32(1), PHONE_ID.to_string()))
        );

        let m = computer(&b32(2), &b16(3), COMPUTER_ID, "Elok’s PC");
        let parsed = parse_computer(body(&m)).unwrap();
        assert_eq!(parsed.public_key, b32(2));
        assert_eq!(parsed.nonce, b16(3));
        assert_eq!(parsed.computer_id, COMPUTER_ID);
        assert_eq!(parsed.computer_name, "Elok’s PC");

        let p = phone(&b32(4), &b16(5));
        assert_eq!(parse_phone(body(&p)), Some((b32(4), b16(5))));

        let ok = allowed(IDENTITY, "Eve’s iPhone", &b32(6));
        assert_eq!(
            parse_answer(body(&ok)),
            Some(Answer::Allowed {
                identity: IDENTITY.into(),
                phone_name: "Eve’s iPhone".into(),
                proof: b32(6),
                signed: allowed_signed(IDENTITY, "Eve’s iPhone"),
            })
        );
        assert_eq!(parse_answer("NO"), Some(Answer::Denied));
        assert_eq!(DENIED, "NO\n");

        let f = finished(&b32(7));
        assert_eq!(parse_finished(body(&f)), Some(b32(7)));
    }

    #[test]
    fn line_formats_are_exact() {
        assert_eq!(
            commitment(&[0xFB; 32], "ABC-1"),
            format!("C {} ABC-1\n", base64url(&[0xFB; 32]))
        );
        assert_eq!(
            computer(&[0; 32], &[0; 16], "ID", "Mac"),
            format!("M {} {} ID TWFj\n", "A".repeat(43), "A".repeat(22))
        );
        assert_eq!(
            phone(&[0; 32], &[0; 16]),
            format!("P {} {}\n", "A".repeat(43), "A".repeat(22))
        );
        assert_eq!(allowed_signed("ID-1", "Mac"), "OK ID-1 TWFj\n");
        assert_eq!(
            allowed("ID-1", "Mac", &[0; 32]),
            format!("OK ID-1 TWFj {}\n", "A".repeat(43))
        );
        assert_eq!(finished(&[0; 32]), format!("F {}\n", "A".repeat(43)));
    }

    #[test]
    fn answer_signed_part_is_the_text_as_received() {
        let line = format!(
            "OK {IDENTITY} {} {}",
            encode_name("Phone"),
            base64url(&b32(1))
        );
        let Some(Answer::Allowed { signed, .. }) = parse_answer(&line) else {
            panic!("expected OK");
        };
        assert_eq!(signed, format!("OK {IDENTITY} {}\n", encode_name("Phone")));
    }

    #[test]
    fn malformed_lines_are_rejected() {
        let k = base64url(&b32(1));
        let n = base64url(&b16(1));
        let name = encode_name("Mac");
        let k31 = base64url(&b(1, 31));
        let n15 = base64url(&b(1, 15));
        let proof = base64url(&b32(9));
        let long = "A".repeat(MAX_LINE_LENGTH);
        let cases: Vec<(&str, String)> = vec![
            ("C", String::new()),
            ("C", format!("C {k}")),
            ("C", format!("C {k} {PHONE_ID} x")),
            ("C", format!("C  {PHONE_ID}")),
            ("C", format!("C {k31} {PHONE_ID}")),
            ("C", format!("C {k}= {PHONE_ID}")),
            ("C", format!("C {k} bad/id")),
            ("C", format!("X {k} {PHONE_ID}")),
            ("C", format!("C {k} {long}")),
            ("M", format!("M {k} {n} {COMPUTER_ID}")),
            ("M", format!("M {k} {n15} {COMPUTER_ID} {name}")),
            ("M", format!("M {k31} {n} {COMPUTER_ID} {name}")),
            ("M", format!("M {k} {n} bad/id {name}")),
            ("M", format!("M {k} {n} {COMPUTER_ID} not+base64")),
            ("M", format!("M {k} {n} {COMPUTER_ID} {name} ")),
            ("M", format!("M {k} {n} {COMPUTER_ID} ")),
            ("P", format!("P {k}")),
            ("P", format!("P {k} {n15}")),
            ("P", format!("P {k} {n} {n}")),
            ("P", format!("P {k}  {n}")),
            ("P", format!("P {k} {n}\r")),
            ("A", "OK".to_string()),
            ("A", "NO ".to_string()),
            ("A", "no".to_string()),
            ("A", format!("OK {IDENTITY} {name}")),
            ("A", format!("OK records {name} {proof}")),
            ("A", format!("OK bad/id {name} {proof}")),
            (
                "A",
                format!("OK {IDENTITY} {name} {}", base64url(&b(1, 31))),
            ),
            ("A", format!("OK {IDENTITY} {name} {proof} x")),
            ("A", format!("OK {IDENTITY} {name}  {proof}")),
            ("A", format!("OK {IDENTITY} {name} {proof}=")),
            ("F", "F".to_string()),
            ("F", format!("F {proof} {proof}")),
            ("F", format!("F {}", base64url(&b(1, 31)))),
            ("F", format!("F {proof}=")),
        ];
        for (kind, line) in &cases {
            let rejected = match *kind {
                "C" => parse_commitment(line).is_none(),
                "M" => parse_computer(line).is_none(),
                "P" => parse_phone(line).is_none(),
                "A" => parse_answer(line).is_none(),
                _ => parse_finished(line).is_none(),
            };
            assert!(rejected, "{kind}: {line:?}");
        }
    }

    #[test]
    fn the_length_limit_is_exclusive_of_512() {
        assert!(!too_long(&"a".repeat(MAX_LINE_LENGTH - 1)));
        assert!(too_long(&"a".repeat(MAX_LINE_LENGTH)));
        // Wired into field parsing (no valid line reaches the limit, so this is the only way to
        // see it act): a bare tag of that length is refused for its length alone.
        assert_eq!(fields(&"x".repeat(MAX_LINE_LENGTH), "x", 0), None);
        assert_eq!(
            fields(
                &"x".repeat(MAX_LINE_LENGTH - 1),
                "x".repeat(MAX_LINE_LENGTH - 1).as_str(),
                0
            ),
            Some(vec![])
        );
    }

    #[test]
    fn transcript_hashes_the_lines_in_order() {
        let mut t = Transcript::new();
        t.add("C a b\n");
        t.add("M c d\n");
        let expected: [u8; 32] = Sha256::digest(b"C a b\nM c d\n").into();
        assert_eq!(t.digest(), expected);
        // digest does not consume or reset.
        assert_eq!(t.digest(), expected);
        let mut other = Transcript::new();
        other.add("C a b\nM c d");
        other.add("\n");
        assert_eq!(other.digest(), expected);
        let mut swapped = Transcript::new();
        swapped.add("M c d\n");
        swapped.add("C a b\n");
        assert_ne!(swapped.digest(), expected);
    }

    fn sample_transcript() -> Transcript {
        let mut t = Transcript::new();
        t.add("C x y\n");
        t
    }

    #[test]
    fn proofs_verify_per_side_and_key() {
        let key = Key::from_bytes(b32(0x40));
        let t = sample_transcript();
        let phone_proof = proof(Side::Phone, &key, &t);
        let computer_proof = proof(Side::Computer, &key, &t);
        assert_ne!(phone_proof, computer_proof);
        assert!(verify(&phone_proof, Side::Phone, &key, &t));
        assert!(verify(&computer_proof, Side::Computer, &key, &t));
        // Wrong side, wrong key, wrong transcript, wrong length.
        assert!(!verify(&phone_proof, Side::Computer, &key, &t));
        assert!(!verify(
            &phone_proof,
            Side::Phone,
            &Key::from_bytes(b32(0x41)),
            &t
        ));
        let mut other = sample_transcript();
        other.add("more\n");
        assert!(!verify(&phone_proof, Side::Phone, &key, &other));
        assert!(!verify(&phone_proof[..31], Side::Phone, &key, &t));
        assert!(!verify(&[], Side::Phone, &key, &t));
        let mut flipped = phone_proof;
        flipped[31] ^= 1;
        assert!(!verify(&flipped, Side::Phone, &key, &t));
    }

    /// Pinned against an independent computation (OpenSSL):
    ///   proofKey = HKDF-SHA256(ikm = long-term key bytes 0x40.., no salt, info "CG2 pairing proof")
    ///   digest   = SHA256("C x y\n")
    ///   proof    = HMAC-SHA256(proofKey, "CG1 phone ok" || digest)
    #[test]
    fn proof_matches_an_independent_computation() {
        let key = Key::from_bytes(b32(0x40));
        let proof = proof(Side::Phone, &key, &sample_transcript());
        assert_eq!(hex(&proof), PINNED_PHONE_PROOF);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    const PINNED_PHONE_PROOF: &str =
        "ce5edf9996af37dca84cbed75ff6f06024bf4a7a04a4e212e5e12c29afdc6d29";

    #[test]
    fn both_sides_compute_the_same_proofs_from_one_agreement() {
        let phone_session = Session::fixed(Role::Phone, [1; 32], b16(0x10));
        let computer_session = Session::fixed(Role::Computer, [2; 32], b16(0xA0));
        let (pp, pn) = (phone_session.public_key(), phone_session.nonce());
        let (cp, cn) = (computer_session.public_key(), computer_session.nonce());
        let a = phone_session.agree(&cp, &cn).unwrap();
        let c = computer_session.agree(&pp, &pn).unwrap();
        let t = sample_transcript();
        assert!(verify(
            &proof(Side::Phone, &a.long_term_key, &t),
            Side::Phone,
            &c.long_term_key,
            &t
        ));
    }
}
