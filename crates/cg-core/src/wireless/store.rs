//! What the computer remembers about its wireless pairings, in two files in one directory:
//!
//! - `wireless.json`: the pairing records, this computer's id, the USB device to phone memory
//!   and the access point's network name. No secret is in it.
//! - `wireless-keys.bin`: the long-term keys (identity to key) and the access point's
//!   passphrase, as JSON sealed by a `SecretStore` (DPAPI on Windows).
//!
//! Both are replaced atomically (temporary file, flush, rename), so a crash leaves the old file
//! or the new one. A save writes the new key first, then the records, and only then drops the
//! key it replaces, so there is no moment where a record points at a key that was never stored
//! or where the old pairing is gone before the new one is on disk.
//!
//! A key file that can't be read (damaged, or sealed by something else) is not an error: the
//! store opens, no key is available, so every record shows as stale (the phone has to be paired
//! again), and nothing is deleted. The next write that changes keys first renames it to `wireless-keys.bin.bad` (replacing an older one) and then writes a new file. The
//! records file is different: if it is unreadable `open` fails, because starting empty would
//! overwrite the user's pairings with the next save.
//!
//! The format is this program's own; it is not the Mac's keychain layout.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::encoding::{base64url, decode_base64url, is_token, is_valid_identity};
use super::pairing::Key;
use crate::settings::write_atomic;

const RECORDS_FILE: &str = "wireless.json";
const KEYS_FILE: &str = "wireless-keys.bin";
const BAD_KEYS_FILE: &str = "wireless-keys.bin.bad";

/// Seals and opens the key file's contents. The platform provides the real one (DPAPI).
pub trait SecretStore: Send + Sync {
    fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>>;
    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>>;
}

/// Test stand-in for a `SecretStore`: XOR with a constant. This is NOT encryption; it only
/// makes sealed bytes differ from plain ones so tests notice mix-ups.
#[cfg(any(test, feature = "testing"))]
pub struct PlainSecrets;

#[cfg(any(test, feature = "testing"))]
impl PlainSecrets {
    const MASK: u8 = 0x5A;
}

#[cfg(any(test, feature = "testing"))]
impl SecretStore for PlainSecrets {
    fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
        Ok(plain.iter().map(|b| b ^ Self::MASK).collect())
    }

    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>> {
        self.protect(sealed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Code,
    Cable,
}

/// One completed pairing. The key itself is kept apart, under `identity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The phone's random id.
    pub peer_id: String,
    pub name: String,
    /// The pre-shared key's identity on the wire (random, not the name).
    pub identity: String,
    /// Unix seconds, from the caller's clock.
    pub created_at: u64,
    pub method: Method,
    /// The phone no longer accepts this pairing (or its key is lost); pair again. `records()`
    /// also reports a record as stale when its key is missing.
    #[serde(default)]
    pub stale: bool,
}

/// `wireless.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    version: u32,
    local_id: Option<String>,
    records: Vec<Record>,
    /// USB serial to the phone id last seen on it.
    usb_devices: BTreeMap<String, String>,
    access_point_ssid: Option<String>,
}

/// A secret string, wiped when dropped. Never printed.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct Secret(String);

impl PartialEq for Secret {
    fn eq(&self, other: &Secret) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

/// The plaintext of `wireless-keys.bin`. Its strings hold secrets and are wiped on drop.
#[derive(Serialize, Deserialize)]
struct KeyFile {
    #[serde(default)]
    version: u32,
    /// identity to base64url key
    #[serde(default)]
    keys: BTreeMap<String, String>,
    #[serde(default)]
    access_point_passphrase: Option<String>,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        for value in self.keys.values_mut() {
            value.zeroize();
        }
        self.access_point_passphrase.zeroize();
    }
}

#[derive(Clone)]
struct State {
    persisted: Persisted,
    keys: BTreeMap<String, Key>,
    passphrase: Option<Secret>,
}

pub struct WirelessStore {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    state: Mutex<State>,
    /// The key file on disk could not be used when opened and has not been set aside yet.
    keys_file_unusable: AtomicBool,
}

impl fmt::Debug for WirelessStore {
    /// No key or passphrase is ever printed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WirelessStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl WirelessStore {
    /// Opens (creating the directory and this computer's id if needed). Fails if
    /// `wireless.json` exists but can't be read; a bad key file does not (see the module docs).
    pub fn open(dir: &Path, secrets: Arc<dyn SecretStore>) -> io::Result<WirelessStore> {
        fs::create_dir_all(dir)?;
        let mut persisted = match fs::read(dir.join(RECORDS_FILE)) {
            Ok(bytes) => serde_json::from_slice::<Persisted>(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Persisted::default(),
            Err(e) => return Err(e),
        };
        let (keys, passphrase, unusable) = load_keys(&*secrets, &dir.join(KEYS_FILE))?;
        let store = WirelessStore {
            dir: dir.to_path_buf(),
            secrets,
            state: Mutex::new(State {
                persisted: persisted.clone(),
                keys,
                passphrase,
            }),
            keys_file_unusable: AtomicBool::new(unusable),
        };
        if !persisted.local_id.as_deref().is_some_and(is_token) {
            persisted.version = 1;
            persisted.local_id = Some(random_uuid()?);
            let mut state = store.lock();
            let mut next = state.clone();
            next.persisted = persisted;
            store.commit(&mut state, next)?;
        }
        Ok(store)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// This computer's id as the phone sees it (random uppercase UUID, made on first open).
    pub fn local_id(&self) -> String {
        self.lock().persisted.local_id.clone().unwrap_or_default()
    }

    pub fn records(&self) -> Vec<Record> {
        let state = self.lock();
        state
            .persisted
            .records
            .iter()
            .map(|r| Record {
                stale: r.stale || !state.keys.contains_key(&r.identity),
                ..r.clone()
            })
            .collect()
    }

    pub fn key(&self, identity: &str) -> Option<Key> {
        self.lock().keys.get(identity).cloned()
    }

    /// Stores a pairing, replacing any earlier one for the same `peer_id` (its record and key go
    /// only after the new ones are on disk). A new pairing is never stale. Fails with
    /// `InvalidInput` for an unusable identity or id, or an identity another phone already uses.
    pub fn save(&self, record: Record, key: Key) -> io::Result<()> {
        if !is_valid_identity(&record.identity) || !is_token(&record.peer_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unusable pairing identity or peer id",
            ));
        }
        let mut state = self.lock();
        if state
            .persisted
            .records
            .iter()
            .any(|r| r.identity == record.identity && r.peer_id != record.peer_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity already belongs to another phone",
            ));
        }
        let mut next = state.clone();
        next.persisted.version = 1;
        next.persisted
            .records
            .retain(|r| r.peer_id != record.peer_id);
        next.keys.insert(record.identity.clone(), key);
        next.persisted.records.push(Record {
            stale: false,
            ..record
        });
        self.commit(&mut state, next)
    }

    pub fn remove(&self, peer_id: &str) -> io::Result<()> {
        let mut state = self.lock();
        let mut next = state.clone();
        next.persisted.records.retain(|r| r.peer_id != peer_id);
        next.persisted
            .usb_devices
            .retain(|_, phone| phone != peer_id);
        self.commit(&mut state, next)
    }

    /// Marks (or unmarks) a pairing the phone turned out to have forgotten. Unknown ids are
    /// ignored.
    pub fn set_stale(&self, peer_id: &str, stale: bool) -> io::Result<()> {
        let mut state = self.lock();
        let mut next = state.clone();
        for record in next
            .persisted
            .records
            .iter_mut()
            .filter(|r| r.peer_id == peer_id)
        {
            record.stale = stale;
        }
        self.commit(&mut state, next)
    }

    /// Remembers which phone this USB device was last seen as.
    pub fn remember_phone(&self, usb_serial: &str, phone_id: &str) -> io::Result<()> {
        let mut state = self.lock();
        let mut next = state.clone();
        next.persisted
            .usb_devices
            .insert(usb_serial.to_string(), phone_id.to_string());
        self.commit(&mut state, next)
    }

    pub fn phone_id(&self, usb_serial: &str) -> Option<String> {
        self.lock().persisted.usb_devices.get(usb_serial).cloned()
    }

    /// The access point's network name and passphrase, made on first use and then kept.
    pub fn access_point(&self) -> io::Result<(String, String)> {
        let mut state = self.lock();
        if let (Some(ssid), Some(pass)) = (&state.persisted.access_point_ssid, &state.passphrase) {
            return Ok((ssid.clone(), pass.0.clone()));
        }
        self.new_access_point(&mut state)
    }

    /// Replaces the network name and passphrase (phones paired by code must join again).
    pub fn regenerate_access_point(&self) -> io::Result<()> {
        let mut state = self.lock();
        self.new_access_point(&mut state).map(|_| ())
    }

    fn new_access_point(&self, state: &mut State) -> io::Result<(String, String)> {
        let ssid = format!("CellGlide-{}", random_chars(UPPER_DIGITS, 4)?);
        let pass = Secret(format!(
            "{}-{}-{}",
            random_chars(LOWER_DIGITS, 4)?,
            random_chars(LOWER_DIGITS, 4)?,
            random_chars(LOWER_DIGITS, 4)?
        ));
        let mut next = state.clone();
        next.persisted.version = 1;
        next.persisted.access_point_ssid = Some(ssid.clone());
        next.passphrase = Some(pass.clone());
        self.commit(state, next)?;
        Ok((ssid, pass.0.clone()))
    }

    /// Puts `next` on disk and, only if that worked, makes it the current state. Order: keys
    /// that are new or changed (together with the old ones), then the records, then the key
    /// file without keys no record uses. If the records can't be written the key file goes
    /// back to the old keys.
    fn commit(&self, current: &mut State, mut next: State) -> io::Result<()> {
        let used: Vec<&str> = next
            .persisted
            .records
            .iter()
            .map(|r| r.identity.as_str())
            .collect();
        next.keys
            .retain(|identity, _| used.contains(&identity.as_str()));

        let adds_secrets = next
            .keys
            .iter()
            .any(|(identity, key)| current.keys.get(identity) != Some(key))
            || next.passphrase != current.passphrase;
        // Keys the key file holds that the new state no longer wants.
        let drops_keys = current
            .keys
            .keys()
            .any(|identity| !next.keys.contains_key(identity));
        let keys_path = self.dir.join(KEYS_FILE);

        if adds_secrets {
            let mut with_old = current.keys.clone();
            with_old.extend(next.keys.iter().map(|(i, k)| (i.clone(), k.clone())));
            self.write_keys(&keys_path, &with_old, next.passphrase.as_ref())?;
        }
        if next.persisted != current.persisted {
            let bytes = serde_json::to_vec_pretty(&next.persisted).map_err(io::Error::other)?;
            if let Err(error) = write_atomic(&self.dir.join(RECORDS_FILE), &bytes) {
                if adds_secrets
                    && let Err(undo) =
                        self.write_keys(&keys_path, &current.keys, current.passphrase.as_ref())
                {
                    tracing::warn!("can't restore the wireless key file: {undo}");
                }
                return Err(error);
            }
        }
        if drops_keys {
            // The new records are on disk; leaving an unused key behind is only untidy, so a
            // failure here is logged, not reported.
            if let Err(error) = self.write_keys(&keys_path, &next.keys, next.passphrase.as_ref()) {
                tracing::warn!("can't tidy the wireless key file: {error}");
            }
        }
        *current = next;
        Ok(())
    }

    fn write_keys(
        &self,
        path: &Path,
        keys: &BTreeMap<String, Key>,
        passphrase: Option<&Secret>,
    ) -> io::Result<()> {
        let file = KeyFile {
            version: 1,
            keys: keys
                .iter()
                .map(|(identity, key)| (identity.clone(), base64url(key.as_bytes())))
                .collect(),
            access_point_passphrase: passphrase.map(|p| p.0.clone()),
        };
        let plain = Zeroizing::new(serde_json::to_vec(&file).map_err(io::Error::other)?);
        let sealed = self.secrets.protect(&plain)?;
        if self.keys_file_unusable.load(Ordering::SeqCst) {
            // Keep what we could not read: it may be sealed by something that can still open it.
            // `rename` replaces an older .bad.
            match fs::rename(path, self.dir.join(BAD_KEYS_FILE)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            self.keys_file_unusable.store(false, Ordering::SeqCst);
        }
        write_atomic(path, &sealed)
    }
}

/// Keys and passphrase from the key file. Missing, undecryptable or malformed means none
/// (logged, never deleted); only an I/O error reading it is an error.
fn load_keys(
    secrets: &dyn SecretStore,
    path: &Path,
) -> io::Result<(BTreeMap<String, Key>, Option<Secret>, bool)> {
    let sealed = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e),
    };
    let parsed = secrets
        .unprotect(&sealed)
        .map(Zeroizing::new)
        .map_err(|e| e.to_string())
        .and_then(|plain| parse_keys(&plain));
    Ok(match parsed {
        Ok((keys, passphrase)) => (keys, passphrase, false),
        Err(reason) => {
            tracing::warn!(
                "wireless key file {} can't be used ({reason}); its pairings are stale",
                path.display()
            );
            (Default::default(), None, true)
        }
    })
}

fn parse_keys(plain: &[u8]) -> Result<(BTreeMap<String, Key>, Option<Secret>), String> {
    let file: KeyFile = serde_json::from_slice(plain).map_err(|_| "not valid".to_string())?;
    let mut keys = BTreeMap::new();
    for (identity, text) in &file.keys {
        let bytes = decode_base64url(text).map(Zeroizing::new);
        let array: Option<[u8; 32]> = bytes.as_deref().and_then(|b| b.as_slice().try_into().ok());
        match array {
            Some(array) if is_valid_identity(identity) => {
                keys.insert(identity.clone(), Key::from_bytes(array));
            }
            _ => return Err("an entry is not a key".to_string()),
        }
    }
    Ok((keys, file.access_point_passphrase.clone().map(Secret)))
}

pub(crate) const UPPER_DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
pub(crate) const LOWER_DIGITS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const SYMBOLS: usize = 36;
/// The largest multiple of 36 that fits in a byte; bytes from here up are discarded so every
/// symbol is equally likely.
const UNBIASED_LIMIT: u8 = 252;

/// The symbol index a random byte stands for, or `None` for bytes in the uneven tail.
pub(crate) fn symbol_for(byte: u8) -> Option<usize> {
    (byte < UNBIASED_LIMIT).then(|| usize::from(byte) % SYMBOLS)
}

/// `count` symbols of a 36-symbol alphabet, uniformly drawn (rejection sampling).
pub(crate) fn random_chars(alphabet: &[u8], count: usize) -> io::Result<String> {
    debug_assert_eq!(alphabet.len(), SYMBOLS);
    let mut result = String::with_capacity(count);
    let mut buffer = [0u8; 32];
    while result.len() < count {
        getrandom::fill(&mut buffer).map_err(io::Error::other)?;
        for &byte in &buffer {
            if result.len() == count {
                break;
            }
            if let Some(index) = symbol_for(byte) {
                result.push(char::from(alphabet[index]));
            }
        }
    }
    Ok(result)
}

/// A random (version 4) UUID, uppercase.
fn random_uuid() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

#[cfg(test)]
mod tests;
