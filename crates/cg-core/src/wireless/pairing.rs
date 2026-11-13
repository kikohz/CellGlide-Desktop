//! Bluetooth-style numeric comparison over X25519 with a commitment.
//!
//! 1. The phone sends `commitment()` first; the computer then reveals its public key and
//!    nonce; the phone reveals its own, and the computer checks them with `verify_commitment`.
//! 2. Both show `Agreed::code`; the user confirms they match on the phone.
//! 3. Both keep `Agreed::long_term_key`.
//!
//! The commitment stops a man in the middle from choosing his values after seeing the
//! computer's, so he gets one guess in a million at making both codes match.

use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Public identity that only encrypts the pairing channel and proves nothing.
pub const PAIRING_IDENTITY: &str = "CG1-pair";
pub const NONCE_LENGTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Phone,
    Computer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// A public key that is not 32 bytes, or a nonce that is not 16.
    #[error("malformed pairing values")]
    Malformed,
    /// A low-order public key: the shared secret would not depend on our secret at all.
    #[error("weak peer public key")]
    WeakKey,
}

/// 32 secret bytes. Never printed, wiped on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Key([u8; 32]);

impl Key {
    pub fn from_bytes(bytes: [u8; 32]) -> Key {
        Key(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for Key {}

pub struct Session {
    role: Role,
    secret: StaticSecret,
    nonce: [u8; NONCE_LENGTH],
}

/// The outcome of a completed key agreement.
#[derive(Debug)]
pub struct Agreed {
    pub code: String,
    pub long_term_key: Key,
}

impl Session {
    pub fn new(role: Role) -> Session {
        let mut secret = [0u8; 32];
        let mut nonce = [0u8; NONCE_LENGTH];
        // No fallback: a pairing built on guessable randomness is worse than a crash, and the
        // OS generator failing means the machine is in no state to pair anyway.
        getrandom::fill(&mut secret).expect("system random generator unavailable");
        getrandom::fill(&mut nonce).expect("system random generator unavailable");
        let session = Session::with_values(role, secret, nonce);
        secret.zeroize();
        session
    }

    #[cfg(test)]
    fn fixed(role: Role, secret: [u8; 32], nonce: [u8; NONCE_LENGTH]) -> Session {
        Session::with_values(role, secret, nonce)
    }

    fn with_values(role: Role, secret: [u8; 32], nonce: [u8; NONCE_LENGTH]) -> Session {
        Session {
            role,
            secret: StaticSecret::from(secret),
            nonce,
        }
    }

    /// Raw X25519, 32 bytes.
    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.secret).to_bytes()
    }

    pub fn nonce(&self) -> [u8; NONCE_LENGTH] {
        self.nonce
    }

    /// Phone only: SHA256(public key || nonce).
    pub fn commitment(&self) -> [u8; 32] {
        commit(&self.public_key(), &self.nonce)
    }

    /// Completes the key agreement. A low-order peer key fails here, before any code is
    /// derived, so a code is never shown for a handshake whose secret the peer could force.
    pub fn agree(self, peer_public: &[u8], peer_nonce: &[u8]) -> Result<Agreed, PairingError> {
        let peer_public: [u8; 32] = peer_public
            .try_into()
            .map_err(|_| PairingError::Malformed)?;
        let peer_nonce: [u8; NONCE_LENGTH] =
            peer_nonce.try_into().map_err(|_| PairingError::Malformed)?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(peer_public));
        if !shared.was_contributory() {
            return Err(PairingError::WeakKey);
        }
        let mine = self.public_key();
        let (phone_pub, computer_pub, phone_nonce, computer_nonce) = match self.role {
            Role::Phone => (mine, peer_public, self.nonce, peer_nonce),
            Role::Computer => (peer_public, mine, peer_nonce, self.nonce),
        };

        // SHA256("CG1 code" || phonePub || computerPub || phoneNonce || computerNonce), first
        // 4 bytes as a big-endian integer, modulo 1 000 000, zero-padded to 6 digits.
        let mut hash = Sha256::new();
        hash.update(b"CG1 code");
        hash.update(phone_pub);
        hash.update(computer_pub);
        hash.update(phone_nonce);
        hash.update(computer_nonce);
        let digest = hash.finalize();
        let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        let code = format!("{:06}", value % 1_000_000);

        let mut salt = [0u8; 2 * NONCE_LENGTH];
        salt[..NONCE_LENGTH].copy_from_slice(&phone_nonce);
        salt[NONCE_LENGTH..].copy_from_slice(&computer_nonce);
        let long_term_key = hkdf_key(Some(&salt), shared.as_bytes(), b"CG1 long-term key");
        Ok(Agreed {
            code,
            long_term_key,
        })
    }
}

fn commit(public_key: &[u8], nonce: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(public_key);
    hash.update(nonce);
    hash.finalize().into()
}

fn hkdf_key(salt: Option<&[u8]>, ikm: &[u8], info: &[u8]) -> Key {
    let mut okm = [0u8; 32];
    // 32 bytes is far below the 255 * 32 limit, so expand cannot fail.
    Hkdf::<Sha256>::new(salt, ikm)
        .expand(info, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    let key = Key(okm);
    okm.zeroize();
    key
}

/// Both sides of the comparison are public, so a plain comparison is fine.
pub fn verify_commitment(commitment: &[u8], public_key: &[u8], nonce: &[u8]) -> bool {
    commit(public_key, nonce).as_slice() == commitment
}

/// The pre-shared key a pairing's connections use: HKDF-SHA256 of the long-term key, no salt,
/// info "CG2 link psk". The long-term key itself only feeds derivations, so the TLS key and
/// the pairing proofs' key never coincide.
pub fn link_key(long_term: &Key) -> Key {
    hkdf_key(None, long_term.as_bytes(), b"CG2 link psk")
}

/// The key the pairing proofs are MACed under: same derivation, info "CG2 pairing proof".
pub fn proof_key(long_term: &Key) -> Key {
    hkdf_key(None, long_term.as_bytes(), b"CG2 pairing proof")
}

/// The public key of `PAIRING_IDENTITY`: SHA256("CG1 public pairing key"). It protects nothing
/// secret; the pairing channel only needs some TLS key.
pub fn pairing_channel_key() -> Key {
    Key(Sha256::digest(b"CG1 public pairing key").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex32(s: &str) -> [u8; 32] {
        hex(s).try_into().unwrap()
    }

    fn range16(start: u8) -> [u8; 16] {
        std::array::from_fn(|i| start + i as u8)
    }

    fn phone() -> Session {
        Session::fixed(
            Role::Phone,
            hex32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"),
            range16(0x10),
        )
    }

    fn computer() -> Session {
        Session::fixed(
            Role::Computer,
            hex32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb"),
            range16(0xA0),
        )
    }

    const LONG_TERM: &str = "16cc58fa39be78a73899d87b0caeaa45f12d6d9ac0314973df3ec6d0a3ea3c43";

    #[test]
    fn public_keys_and_commitment_match_the_phone_vectors() {
        assert_eq!(
            phone().public_key().to_vec(),
            hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert_eq!(
            computer().public_key().to_vec(),
            hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        );
        assert_eq!(
            phone().commitment().to_vec(),
            hex("37d59543b75c0e4183ad43b8494d76c75e190a1c3e1aad56ebbf70a130e62793")
        );
    }

    #[test]
    fn both_sides_agree_on_code_and_key() {
        let (p, c) = (phone(), computer());
        let (p_pub, p_nonce) = (p.public_key(), p.nonce());
        let (c_pub, c_nonce) = (c.public_key(), c.nonce());
        let from_phone = p.agree(&c_pub, &c_nonce).unwrap();
        let from_computer = c.agree(&p_pub, &p_nonce).unwrap();
        assert_eq!(from_phone.code, "423265");
        assert_eq!(from_computer.code, "423265");
        assert_eq!(from_phone.long_term_key.as_bytes(), &hex32(LONG_TERM));
        assert_eq!(from_computer.long_term_key.as_bytes(), &hex32(LONG_TERM));
    }

    #[test]
    fn a_swapped_public_key_gives_different_codes() {
        let mitm = Session::fixed(Role::Computer, [9; 32], range16(0x40));
        let (p, c) = (phone(), computer());
        let (p_pub, p_nonce) = (p.public_key(), p.nonce());
        let (c_nonce, mitm_pub) = (c.nonce(), mitm.public_key());
        // The phone sees the man in the middle's key where the computer's belongs.
        let phone_code = p.agree(&mitm_pub, &c_nonce).unwrap().code;
        let computer_code = c.agree(&p_pub, &p_nonce).unwrap().code;
        assert_ne!(phone_code, computer_code);
    }

    #[test]
    fn a_low_order_peer_key_is_refused() {
        let error = phone().agree(&[0; 32], &range16(0xA0)).unwrap_err();
        assert_eq!(error, PairingError::WeakKey);
        // Order-8 point on the curve (u = 1), another known small-subgroup value.
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(
            phone().agree(&one, &range16(0xA0)).unwrap_err(),
            PairingError::WeakKey
        );
    }

    #[test]
    fn wrong_lengths_are_malformed() {
        let c = computer();
        let (key, nonce) = (c.public_key(), c.nonce());
        assert_eq!(
            phone().agree(&[1; 5], &nonce).unwrap_err(),
            PairingError::Malformed
        );
        assert_eq!(
            phone().agree(&key, &[1; 4]).unwrap_err(),
            PairingError::Malformed
        );
    }

    #[test]
    fn commitment_binds_key_and_nonce() {
        let p = phone();
        let (commitment, key, nonce) = (p.commitment(), p.public_key(), p.nonce());
        assert!(verify_commitment(&commitment, &key, &nonce));
        let mut bad_key = key;
        bad_key[31] ^= 1;
        let mut bad_nonce = nonce;
        bad_nonce[0] ^= 1;
        assert!(!verify_commitment(&commitment, &bad_key, &nonce));
        assert!(!verify_commitment(&commitment, &key, &bad_nonce));
        assert!(!verify_commitment(&commitment[..31], &key, &nonce));
    }

    #[test]
    fn derived_keys_are_distinct_and_pinned() {
        let long_term = Key::from_bytes(hex32(LONG_TERM));
        let link = link_key(&long_term);
        let proof = proof_key(&long_term);
        assert_ne!(link, proof);
        assert_ne!(link, long_term);
        assert_ne!(proof, long_term);
        // Computed independently with Python (hmac/hashlib, RFC 5869 with the default
        // all-zero salt, which is what CryptoKit's HKDF does without a salt):
        //   prk = HMAC-SHA256(b"\0" * 32, ikm); okm = HMAC-SHA256(prk, info || 0x01)
        assert_eq!(
            link.as_bytes(),
            &hex32("3f4cef035367e58a1fe4d1125d41f77d729423876f621e8842efaf7e610a0a4a")
        );
        assert_eq!(
            proof.as_bytes(),
            &hex32("71bd886d09e749a5f2a1494f0d34c1c43bacb5d225b0ba6c2c27d1db496c261f")
        );
    }

    #[test]
    fn pairing_channel_key_is_the_public_constant() {
        assert_eq!(PAIRING_IDENTITY, "CG1-pair");
        assert_eq!(
            pairing_channel_key().as_bytes(),
            &hex32("856878c3285976f206f8bff2c7c253467ce8a8db76b8c7e3db87999b4ec09df1")
        );
    }

    #[test]
    fn key_debug_hides_the_bytes() {
        let key = Key::from_bytes([0xAB; 32]);
        let shown = format!("{key:?}");
        assert!(!shown.to_lowercase().contains("ab"), "{shown}");
    }

    #[test]
    fn fresh_sessions_differ() {
        let (a, b) = (Session::new(Role::Phone), Session::new(Role::Phone));
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.nonce(), b.nonce());
    }
}
