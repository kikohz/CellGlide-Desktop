use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const PEER: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
const PEER2: &str = "0C5B6E21-1111-4222-8333-444455556666";
const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";
const IDENTITY2: &str = "11111111-2222-4333-8444-555555555555";
const IDENTITY3: &str = "22222222-3333-4444-8555-666666666666";

fn key(start: u8) -> Key {
    Key::from_bytes(std::array::from_fn(|i| start.wrapping_add(i as u8)))
}

fn record(peer: &str, identity: &str) -> Record {
    Record {
        peer_id: peer.into(),
        name: "Eve iPhone".into(),
        identity: identity.into(),
        created_at: 1_700_000_000,
        method: Method::Code,
        stale: false,
    }
}

fn open(dir: &Path) -> WirelessStore {
    WirelessStore::open(dir, Arc::new(PlainSecrets)).unwrap()
}

fn keys_file(dir: &Path) -> PathBuf {
    dir.join("wireless-keys.bin")
}

fn json_file(dir: &Path) -> PathBuf {
    dir.join("wireless.json")
}

/// Secrets whose protect can be switched to fail.
struct Flaky {
    fail: Arc<AtomicBool>,
}

impl SecretStore for Flaky {
    fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other("no protection available"));
        }
        PlainSecrets.protect(plain)
    }

    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>> {
        PlainSecrets.unprotect(sealed)
    }
}

fn contains_any_slice(haystack: &[u8], secret: &[u8], width: usize) -> bool {
    secret
        .windows(width)
        .any(|piece| haystack.windows(width).any(|h| h == piece))
}

#[test]
fn saved_records_and_keys_come_back_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path());
        store.save(record(PEER, IDENTITY), key(1)).unwrap();
        store.save(record(PEER2, IDENTITY2), key(50)).unwrap();
    }
    let store = open(dir.path());
    let records = store.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0], record(PEER, IDENTITY));
    assert_eq!(records[1], record(PEER2, IDENTITY2));
    assert_eq!(store.key(IDENTITY), Some(key(1)));
    assert_eq!(store.key(IDENTITY2), Some(key(50)));
    assert_eq!(store.key("nobody"), None);
}

#[test]
fn wireless_json_holds_no_key_bytes_and_no_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let k = key(0x90);
    store.save(record(PEER, IDENTITY), key(0x90)).unwrap();
    let (ssid, passphrase) = store.access_point().unwrap();
    let json = std::fs::read(json_file(dir.path())).unwrap();
    assert!(!contains_any_slice(&json, k.as_bytes(), 8));
    // The encoded spelling of the key as well.
    let encoded = base64url(k.as_bytes());
    assert!(!contains_any_slice(&json, encoded.as_bytes(), 8));
    assert!(!contains_any_slice(&json, passphrase.as_bytes(), 8));
    // The network name is not secret and lives there.
    assert!(String::from_utf8(json).unwrap().contains(&ssid));
}

#[test]
fn remove_deletes_record_and_key_and_the_usb_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    store.save(record(PEER2, IDENTITY2), key(2)).unwrap();
    store.remember_phone("SERIAL-A", PEER).unwrap();
    store.remember_phone("SERIAL-B", PEER2).unwrap();
    store.remove(PEER).unwrap();
    assert_eq!(store.records().len(), 1);
    assert_eq!(store.key(IDENTITY), None);
    assert_eq!(store.phone_id("SERIAL-A"), None);
    assert_eq!(store.phone_id("SERIAL-B"), Some(PEER2.to_string()));
    let store = open(dir.path());
    assert_eq!(store.records().len(), 1);
    assert_eq!(store.key(IDENTITY), None);
    assert_eq!(store.key(IDENTITY2), Some(key(2)));
    assert_eq!(store.phone_id("SERIAL-A"), None);
    // The key file no longer holds the removed key.
    let sealed = std::fs::read(keys_file(dir.path())).unwrap();
    let plain = PlainSecrets.unprotect(&sealed).unwrap();
    assert!(!contains_any_slice(
        &plain,
        base64url(key(1).as_bytes()).as_bytes(),
        8
    ));
}

#[test]
fn stale_marks_persist_and_a_new_pairing_clears_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    store.set_stale(PEER, true).unwrap();
    assert!(store.records()[0].stale);
    assert!(open(dir.path()).records()[0].stale);
    store.set_stale(PEER, false).unwrap();
    assert!(!open(dir.path()).records()[0].stale);
    store.set_stale(PEER, true).unwrap();
    // A fresh pairing for the same phone is not stale, whatever the record says.
    let mut again = record(PEER, IDENTITY2);
    again.stale = true;
    store.save(again, key(2)).unwrap();
    assert!(!store.records()[0].stale);
    assert!(!open(dir.path()).records()[0].stale);
    // Unknown peers are ignored.
    store.set_stale("nobody", true).unwrap();
    assert_eq!(store.records().len(), 1);
}

#[test]
fn saving_again_for_a_peer_replaces_its_record_and_old_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    store.save(record(PEER2, IDENTITY2), key(2)).unwrap();
    let mut newer = record(PEER, IDENTITY3);
    newer.name = "Eve new phone".into();
    store.save(newer.clone(), key(3)).unwrap();
    for store in [open(dir.path()), store] {
        let records = store.records();
        assert_eq!(records.len(), 2);
        assert!(records.contains(&newer));
        assert_eq!(store.key(IDENTITY), None);
        assert_eq!(store.key(IDENTITY3), Some(key(3)));
        assert_eq!(store.key(IDENTITY2), Some(key(2)));
    }
    let sealed = std::fs::read(keys_file(dir.path())).unwrap();
    let plain = PlainSecrets.unprotect(&sealed).unwrap();
    assert!(!contains_any_slice(
        &plain,
        base64url(key(1).as_bytes()).as_bytes(),
        8
    ));
}

#[test]
fn saving_the_same_identity_again_updates_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    let mut renamed = record(PEER, IDENTITY);
    renamed.name = "Renamed".into();
    store.save(renamed.clone(), key(1)).unwrap();
    assert_eq!(store.records(), vec![renamed]);
    assert_eq!(store.key(IDENTITY), Some(key(1)));
}

#[test]
fn an_identity_belonging_to_another_peer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    let error = store.save(record(PEER2, IDENTITY), key(2)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(store.key(IDENTITY), Some(key(1)));
    assert_eq!(store.records(), vec![record(PEER, IDENTITY)]);
}

#[test]
fn invalid_identities_and_ids_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    for identity in ["", "records", "stale", "a b", "bad/id"] {
        let error = store.save(record(PEER, identity), key(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{identity:?}");
    }
    let error = store.save(record("bad id", IDENTITY), key(1)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(store.records().is_empty());
}

#[test]
fn a_failing_protect_fails_save_and_leaves_everything_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let store = WirelessStore::open(dir.path(), Arc::new(Flaky { fail: fail.clone() })).unwrap();
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    let json_before = std::fs::read(json_file(dir.path())).unwrap();
    let keys_before = std::fs::read(keys_file(dir.path())).unwrap();

    fail.store(true, Ordering::SeqCst);
    let mut replacement = record(PEER, IDENTITY2);
    replacement.name = "Other".into();
    assert!(store.save(replacement, key(2)).is_err());
    assert!(store.save(record(PEER2, IDENTITY3), key(3)).is_err());

    assert_eq!(store.records(), vec![record(PEER, IDENTITY)]);
    assert_eq!(store.key(IDENTITY), Some(key(1)));
    assert_eq!(store.key(IDENTITY2), None);
    assert_eq!(std::fs::read(json_file(dir.path())).unwrap(), json_before);
    assert_eq!(std::fs::read(keys_file(dir.path())).unwrap(), keys_before);
}

#[test]
fn a_failing_records_write_rolls_the_key_file_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(1)).unwrap();
    // Make wireless.json impossible to replace: a directory sits where the file was.
    std::fs::remove_file(json_file(dir.path())).unwrap();
    std::fs::create_dir(json_file(dir.path())).unwrap();
    assert!(store.save(record(PEER2, IDENTITY2), key(2)).is_err());
    assert_eq!(store.records(), vec![record(PEER, IDENTITY)]);
    assert_eq!(store.key(IDENTITY2), None);
    // The new key was not left behind in the key file; the old one is still there.
    let sealed = std::fs::read(keys_file(dir.path())).unwrap();
    let plain = PlainSecrets.unprotect(&sealed).unwrap();
    assert!(!contains_any_slice(
        &plain,
        base64url(key(2).as_bytes()).as_bytes(),
        8
    ));
    assert!(contains_any_slice(
        &plain,
        base64url(key(1).as_bytes()).as_bytes(),
        8
    ));
}

#[test]
fn a_damaged_key_file_marks_every_record_stale_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path());
        store.save(record(PEER, IDENTITY), key(1)).unwrap();
        store.save(record(PEER2, IDENTITY2), key(2)).unwrap();
    }
    for damage in [
        b"garbage".to_vec(),
        Vec::new(),
        PlainSecrets.protect(b"{\"keys\": 5}").unwrap(),
        PlainSecrets
            .protect(br#"{"version":1,"keys":{"a-b":"AAAA"}}"#)
            .unwrap(),
    ] {
        std::fs::write(keys_file(dir.path()), &damage).unwrap();
        let store = open(dir.path());
        let records = store.records();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.stale));
        assert_eq!(store.key(IDENTITY), None);
        // The file is untouched until something writes keys.
        assert_eq!(std::fs::read(keys_file(dir.path())).unwrap(), damage);
        // Other bookkeeping still works without touching it.
        store.remember_phone("S", PEER).unwrap();
        assert_eq!(std::fs::read(keys_file(dir.path())).unwrap(), damage);
    }
}

#[test]
fn undecryptable_key_file_behaves_like_a_damaged_one() {
    struct Refuses;
    impl SecretStore for Refuses {
        fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            PlainSecrets.protect(plain)
        }
        fn unprotect(&self, _: &[u8]) -> io::Result<Vec<u8>> {
            Err(io::Error::other("wrong machine"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .save(record(PEER, IDENTITY), key(1))
        .unwrap();
    let store = WirelessStore::open(dir.path(), Arc::new(Refuses)).unwrap();
    assert!(store.records()[0].stale);
    assert_eq!(store.key(IDENTITY), None);
}

#[test]
fn the_next_save_after_damage_writes_a_fresh_key_file() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path());
        store.save(record(PEER, IDENTITY), key(1)).unwrap();
        store.save(record(PEER2, IDENTITY2), key(2)).unwrap();
    }
    std::fs::write(keys_file(dir.path()), b"garbage").unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY3), key(3)).unwrap();
    let store = open(dir.path());
    let records = store.records();
    let first = records.iter().find(|r| r.peer_id == PEER).unwrap();
    let second = records.iter().find(|r| r.peer_id == PEER2).unwrap();
    assert!(!first.stale);
    assert_eq!(store.key(IDENTITY3), Some(key(3)));
    // The other phone's key is gone with the damaged file; it stays listed, marked stale.
    assert!(second.stale);
    assert_eq!(store.key(IDENTITY2), None);
}

#[test]
fn a_corrupt_records_file_is_an_error_not_an_empty_start() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .save(record(PEER, IDENTITY), key(1))
        .unwrap();
    let broken = b"{ not json";
    std::fs::write(json_file(dir.path()), broken).unwrap();
    let Err(error) = WirelessStore::open(dir.path(), Arc::new(PlainSecrets)) else {
        panic!("open must fail");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    // Nothing was overwritten.
    assert_eq!(std::fs::read(json_file(dir.path())).unwrap(), broken);
}

#[test]
fn a_half_written_temporary_file_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .save(record(PEER, IDENTITY), key(1))
        .unwrap();
    // What a crash in the middle of a write leaves next to the real files.
    std::fs::write(dir.path().join("wireless-keys.bin.tmp"), b"half").unwrap();
    std::fs::write(dir.path().join("wireless.json.tmp"), b"{\"rec").unwrap();
    let store = open(dir.path());
    assert_eq!(store.key(IDENTITY), Some(key(1)));
    assert!(!store.records()[0].stale);
    // And the next write simply replaces them.
    store.save(record(PEER2, IDENTITY2), key(2)).unwrap();
    assert_eq!(open(dir.path()).records().len(), 2);
}

#[test]
fn local_id_is_stable_and_an_uppercase_uuid() {
    let dir = tempfile::tempdir().unwrap();
    let first = open(dir.path()).local_id();
    assert_eq!(first, open(dir.path()).local_id());
    assert_eq!(first, first.to_uppercase());
    assert_eq!(first.len(), 36);
    assert!(is_token(&first));
    // Version 4, variant 10xx.
    assert_eq!(first.as_bytes()[14], b'4');
    assert!(matches!(first.as_bytes()[19], b'8' | b'9' | b'A' | b'B'));
    let other = tempfile::tempdir().unwrap();
    assert_ne!(first, open(other.path()).local_id());
}

#[test]
fn usb_devices_map_to_phones_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    assert_eq!(store.phone_id("S1"), None);
    store.remember_phone("S1", PEER).unwrap();
    store.remember_phone("S1", PEER2).unwrap();
    assert_eq!(store.phone_id("S1"), Some(PEER2.to_string()));
    assert_eq!(open(dir.path()).phone_id("S1"), Some(PEER2.to_string()));
}

fn ssid_ok(ssid: &str) -> bool {
    ssid.strip_prefix("CellGlide-").is_some_and(|rest| {
        rest.len() == 4
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    })
}

fn passphrase_ok(pass: &str) -> bool {
    let groups: Vec<&str> = pass.split('-').collect();
    groups.len() == 3
        && groups.iter().all(|g| {
            g.len() == 4
                && g.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

#[test]
fn access_point_is_stable_well_formed_and_regenerates() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let (ssid, pass) = store.access_point().unwrap();
    assert!(ssid_ok(&ssid), "{ssid}");
    assert!(passphrase_ok(&pass), "{pass}");
    assert_eq!(store.access_point().unwrap(), (ssid.clone(), pass.clone()));
    assert_eq!(
        open(dir.path()).access_point().unwrap(),
        (ssid, pass.clone())
    );
    store.regenerate_access_point().unwrap();
    let (ssid2, pass2) = store.access_point().unwrap();
    // 36^12 passphrases: a collision is not a realistic flake.
    assert_ne!(pass, pass2);
    assert!(ssid_ok(&ssid2));
    assert!(passphrase_ok(&pass2));
    assert_eq!(open(dir.path()).access_point().unwrap(), (ssid2, pass2));
    // The passphrase is in the sealed file and not in the records file.
    let json = std::fs::read(json_file(dir.path())).unwrap();
    assert!(!contains_any_slice(&json, pass.as_bytes(), 8));
}

#[test]
fn access_point_without_its_passphrase_is_made_anew() {
    let dir = tempfile::tempdir().unwrap();
    let (_, pass) = open(dir.path()).access_point().unwrap();
    std::fs::write(keys_file(dir.path()), b"garbage").unwrap();
    let (ssid2, pass2) = open(dir.path()).access_point().unwrap();
    assert!(ssid_ok(&ssid2) && passphrase_ok(&pass2));
    assert_ne!(pass, pass2);
}

#[test]
fn random_characters_use_only_the_alphabet_and_every_symbol() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        for c in random_chars(UPPER_DIGITS, 20).unwrap().bytes() {
            assert!(UPPER_DIGITS.contains(&c));
            seen.insert(c);
        }
    }
    // 4000 draws over 36 symbols: all of them show up (a miss is astronomically unlikely).
    assert_eq!(seen.len(), 36);
}

#[test]
fn unbiased_mapping_rejects_the_uneven_tail() {
    // 252 = 7 * 36 is the first value that would favour the low symbols.
    assert_eq!(symbol_for(0), Some(0));
    assert_eq!(symbol_for(35), Some(35));
    assert_eq!(symbol_for(36), Some(0));
    assert_eq!(symbol_for(251), Some(35));
    assert_eq!(symbol_for(252), None);
    assert_eq!(symbol_for(255), None);
}

#[test]
fn plain_secrets_round_trip_but_hide_nothing_on_purpose() {
    let sealed = PlainSecrets.protect(b"hello").unwrap();
    assert_ne!(sealed, b"hello");
    assert_eq!(PlainSecrets.unprotect(&sealed).unwrap(), b"hello");
}

#[test]
fn debug_output_shows_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    store.save(record(PEER, IDENTITY), key(7)).unwrap();
    let (_, pass) = store.access_point().unwrap();
    let text = format!("{store:?} {:?}", store.records());
    assert!(!text.contains(&pass));
    assert!(!text.contains(&base64url(key(7).as_bytes())));
}
