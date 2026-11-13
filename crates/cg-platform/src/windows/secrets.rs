//! `SecretStore` on DPAPI: `CryptProtectData` / `CryptUnprotectData` with the machine scope
//! (`CRYPTPROTECT_LOCAL_MACHINE`, because the service runs as LocalSystem and a user's own
//! data protection key is not the service's) and the fixed entropy below, so a blob made by
//! some other program on this machine does not open here. The data directory's ACL is what
//! keeps other users away from the sealed file. Nothing is written anywhere: input and output
//! are memory only.

use std::io;

use cg_core::wireless::store::SecretStore;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    CryptUnprotectData,
};
use zeroize::Zeroize;

/// Additional entropy mixed into every seal.
const ENTROPY: &[u8] = b"CellGlide wireless";

pub struct Dpapi;

fn blob(bytes: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "data too large"))?,
        pbData: bytes.as_ptr() as *mut u8,
    })
}

/// Copies an output blob DPAPI allocated, wipes it and frees it.
///
/// # Safety
/// `out` must be an output blob filled in by a successful DPAPI call, not yet freed.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: per the contract, `pbData` points to `cbData` readable and writable bytes that
    // were allocated with LocalAlloc.
    unsafe {
        let data = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
        let copy = data.to_vec();
        data.zeroize();
        LocalFree(Some(HLOCAL(out.pbData.cast())));
        copy
    }
}

impl SecretStore for Dpapi {
    fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(plain)?;
        let entropy = blob(ENTROPY)?;
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the blobs point into slices that outlive the call; DPAPI does not write to
        // its inputs; `out` is a valid destination.
        unsafe {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(io::Error::from)?;
            Ok(take(out))
        }
    }

    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(sealed)?;
        let entropy = blob(ENTROPY)?;
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(io::Error::from)?;
            Ok(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"correct horse battery staple 0123456789";

    #[test]
    fn round_trips_and_hides_the_plaintext() {
        let sealed = Dpapi.protect(SECRET).unwrap();
        assert!(!sealed.windows(8).any(|w| SECRET.windows(8).any(|s| s == w)));
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), SECRET);
    }

    #[test]
    fn empty_input_round_trips() {
        let sealed = Dpapi.protect(b"").unwrap();
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), b"");
    }

    #[test]
    fn tampered_or_foreign_bytes_are_refused() {
        let mut sealed = Dpapi.protect(SECRET).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(Dpapi.unprotect(&sealed).is_err());
        assert!(Dpapi.unprotect(b"not a dpapi blob").is_err());
        assert!(Dpapi.unprotect(b"").is_err());
    }

    #[test]
    fn the_entropy_is_part_of_the_seal() {
        // A blob sealed without the entropy must not open with it.
        use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};
        let input = CRYPT_INTEGER_BLOB {
            cbData: SECRET.len() as u32,
            pbData: SECRET.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, None, None, None, None, 0, &mut out).unwrap();
        }
        let sealed =
            unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        assert!(Dpapi.unprotect(&sealed).is_err());
    }
}
