//! Pairing for the wireless backup link over the USB cable: no code to compare, but the phone
//! asks its user once before giving a new computer its key. The decision logic is the Mac's
//! `WirelessCablePairing.pairIfNeeded`.
//!
//! Called once per USB session, after the session is up, from a task of its own: it can wait
//! up to 70 seconds on the phone's user, so it must never be on the session's path, and any
//! failure here is only reported to the caller. Nothing is saved unless the whole exchange
//! succeeded.

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use zeroize::Zeroizing;

use super::cable::{self, MAX_REPLY, REPLY_TIMEOUT, Reply};
use super::encoding::display_name;
use super::store::{Method, Record, WirelessStore};
use super::{CABLE_PAIRING_PORT, CAPABILITY};
use crate::phone::Phone;
use crate::{CoreError, Result};
use cg_protocol::phone::Hello;

/// What the phone is called when it sends no usable name.
const FALLBACK_PHONE_NAME: &str = "iPhone";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The phone has this computer's key already (nothing was asked, or it said `KNOWN`).
    AlreadyPaired,
    /// A new pairing was saved.
    Paired(Record),
    /// The phone said `KNOWN`, but this computer has no pairing (or no key) for that phone: the
    /// phone keeps a pairing this computer lost, and won't send the key again. Removing it on
    /// the phone lets the two pair again. Only which phone this USB device is was remembered.
    KnownButNoKey,
    /// The switch is off, or the phone can't do wireless: nothing was opened.
    Skipped,
}

pub async fn pair_if_needed(
    phone: &Phone,
    hello: &Hello,
    usb_serial: &str,
    enabled: bool,
    computer_name: &str,
    store: &WirelessStore,
) -> Result<Outcome> {
    if !enabled || !hello.has(CAPABILITY) {
        return Ok(Outcome::Skipped);
    }
    // A stale pairing (the phone refused it over the air) is asked about again: KNOWN means the
    // phone still has it, a new key replaces it. `records()` also calls a record stale when its
    // key is missing.
    if let Some(phone_id) = store.phone_id(usb_serial)
        && has_good_record(store, &phone_id)
    {
        return Ok(Outcome::AlreadyPaired);
    }
    let name = super::computer_name(Some(computer_name));
    let request = cable::request(&store.local_id(), &name);

    let mut stream = phone.open(CABLE_PAIRING_PORT).await?;
    stream.write_all(request.as_bytes()).await?;
    // The phone may be asking its user: wait for the answer, but not forever.
    let line = timeout(REPLY_TIMEOUT, read_line(&mut stream))
        .await
        .map_err(|_| CoreError::Timeout)??;
    // Nothing more is sent; the connection closes when `stream` is dropped.
    drop(stream);

    let reply = std::str::from_utf8(&line)
        .ok()
        .and_then(cable::parse_reply)
        .ok_or_else(|| CoreError::Pairing("the phone's reply was malformed".into()))?;
    let phone_id = match &reply {
        Reply::Known { phone_id } | Reply::New { phone_id, .. } => phone_id.clone(),
    };
    // Another USB device already is this phone, with a good pairing: a device claiming to be
    // it must not take over (or replace) that pairing. Nothing is saved or changed.
    if has_good_record(store, &phone_id)
        && store
            .usb_serials(&phone_id)
            .iter()
            .any(|serial| serial != usb_serial)
    {
        return Err(CoreError::Pairing(
            "the phone named in the reply is already paired through another USB device".into(),
        ));
    }
    match reply {
        // The phone doesn't send the key twice: remember which phone this is.
        Reply::Known { phone_id } => {
            store.remember_phone(usb_serial, &phone_id)?;
            let has_key = store
                .records()
                .iter()
                .any(|r| r.peer_id == phone_id && store.key(&r.identity).is_some());
            if !has_key {
                return Ok(Outcome::KnownButNoKey);
            }
            store.set_stale(&phone_id, false)?;
            Ok(Outcome::AlreadyPaired)
        }
        Reply::New {
            phone_id,
            identity,
            key,
            phone_name,
        } => {
            let mut name = display_name(&phone_name);
            if name.is_empty() {
                name = FALLBACK_PHONE_NAME.to_string();
            }
            let record = Record {
                peer_id: phone_id.clone(),
                name,
                identity,
                created_at: unix_now(),
                method: Method::Cable,
                stale: false,
            };
            store.save(record.clone(), key)?;
            store.remember_phone(usb_serial, &phone_id)?;
            Ok(Outcome::Paired(record))
        }
    }
}

/// A pairing with `phone_id` that is not stale and whose key is present.
fn has_good_record(store: &WirelessStore, phone_id: &str) -> bool {
    store
        .records()
        .iter()
        .any(|r| r.peer_id == phone_id && !r.stale)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One line without its newline, read a byte at a time so nothing beyond it is consumed.
/// A line that reaches `MAX_REPLY` bytes is an error. The line can carry the key, so it is
/// wiped when dropped, and allocated once at full size so no partial copy is left behind by a
/// reallocation.
async fn read_line<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Zeroizing<Vec<u8>>> {
    let mut line = Zeroizing::new(Vec::with_capacity(MAX_REPLY));
    loop {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            return Ok(line);
        }
        line.push(byte);
        if line.len() >= MAX_REPLY {
            return Err(CoreError::Pairing("the phone's reply was too long".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{CableBehaviour, FakeMux, FakePhone, FakePhoneOptions, SESSION_A, device};
    use crate::wireless::encoding::encode_name;
    use crate::wireless::store::PlainSecrets;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use std::sync::Arc;
    use std::time::Duration;

    const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";
    const IDENTITY2: &str = "11111111-2222-4333-8444-555555555555";
    const SERIAL: &str = "aaaa";

    struct Rig {
        _mux: FakeMux,
        phone: FakePhone,
        handle: Phone,
        _dir: tempfile::TempDir,
        store: WirelessStore,
    }

    fn new_line(identity: &str, key: u8, name: &str) -> String {
        format!(
            "{PHONE_ID} {identity} {} {}",
            STANDARD.encode([key; 32]),
            encode_name(name)
        )
    }

    async fn rig(cable: CableBehaviour) -> Rig {
        let mux = FakeMux::start().await;
        let phone = FakePhone::start(
            &mux,
            FakePhoneOptions {
                cable,
                ..FakePhoneOptions::default()
            },
        )
        .await;
        mux.attach(device(SERIAL, 1));
        let dir = tempfile::tempdir().unwrap();
        let store = WirelessStore::open(dir.path(), Arc::new(PlainSecrets)).unwrap();
        Rig {
            handle: Phone::usb(mux.endpoint(), device(SERIAL, 1)),
            _mux: mux,
            phone,
            _dir: dir,
            store,
        }
    }

    fn hello(capabilities: &[&str]) -> Hello {
        serde_json::from_value(crate::testing::hello_json(capabilities, true, SESSION_A)).unwrap()
    }

    fn wireless_hello() -> Hello {
        hello(&["socks5-tcp", CAPABILITY])
    }

    async fn pair(r: &Rig, enabled: bool, hello: &Hello) -> Result<Outcome> {
        pair_named(r, enabled, hello, "Desk PC").await
    }

    async fn pair_named(r: &Rig, enabled: bool, hello: &Hello, name: &str) -> Result<Outcome> {
        pair_if_needed(&r.handle, hello, SERIAL, enabled, name, &r.store).await
    }

    async fn closed(r: &Rig) {
        for _ in 0..200 {
            if r.phone.open_cable_connections() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the cable pairing connection stayed open");
    }

    fn nothing_saved(r: &Rig) {
        assert!(r.store.records().is_empty());
        assert_eq!(r.store.phone_id(SERIAL), None);
    }

    #[tokio::test]
    async fn a_new_computer_is_saved_and_the_usb_device_remembered() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "Eve iPhone"))).await;
        let Outcome::Paired(record) = pair(&r, true, &wireless_hello()).await.unwrap() else {
            panic!("expected a new pairing");
        };
        assert_eq!(record.method, Method::Cable);
        assert_eq!(record.peer_id, PHONE_ID);
        assert_eq!(record.identity, IDENTITY);
        assert_eq!(record.name, "Eve iPhone");
        assert!(!record.stale);
        assert!(record.created_at > 1_700_000_000);
        assert_eq!(r.store.records(), vec![record]);
        assert_eq!(
            r.store.key(IDENTITY).unwrap().as_bytes(),
            &[7u8; 32],
            "the key is stored"
        );
        assert_eq!(r.store.phone_id(SERIAL), Some(PHONE_ID.to_string()));
    }

    #[tokio::test]
    async fn the_request_is_exactly_the_request_line_and_nothing_follows() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        closed(&r).await;
        let expected = cable::request(&r.store.local_id(), "Desk PC").into_bytes();
        assert_eq!(r.phone.cable_requests(), vec![expected]);
    }

    #[tokio::test]
    async fn an_empty_computer_name_is_sent_as_windows_pc() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair_named(&r, true, &wireless_hello(), "  ").await.unwrap();
        closed(&r).await;
        let expected = cable::request(&r.store.local_id(), "Windows PC").into_bytes();
        assert_eq!(r.phone.cable_requests(), vec![expected]);
    }

    #[tokio::test]
    async fn a_second_session_does_not_connect_again() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(r.phone.cable_connections(), 1);
        let again = pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(again, Outcome::AlreadyPaired);
        assert_eq!(r.phone.cable_connections(), 1);
    }

    #[tokio::test]
    async fn a_stale_record_is_asked_about_again_and_known_clears_the_mark() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        r.store.set_stale(PHONE_ID, true).unwrap();
        r.phone
            .set_cable(CableBehaviour::Reply(format!("CG1 KNOWN {PHONE_ID}")));
        let outcome = pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(outcome, Outcome::AlreadyPaired);
        assert_eq!(r.phone.cable_connections(), 2);
        let records = r.store.records();
        assert_eq!(records.len(), 1);
        assert!(!records[0].stale);
        assert_eq!(r.store.key(IDENTITY).unwrap().as_bytes(), &[7u8; 32]);
    }

    #[tokio::test]
    async fn a_stale_record_gets_replaced_by_a_new_key() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        r.store.set_stale(PHONE_ID, true).unwrap();
        r.phone
            .set_cable(CableBehaviour::Reply(new_line(IDENTITY2, 9, "iPhone 2")));
        let Outcome::Paired(record) = pair(&r, true, &wireless_hello()).await.unwrap() else {
            panic!("expected a new pairing");
        };
        assert_eq!(record.identity, IDENTITY2);
        assert_eq!(r.store.records(), vec![record]);
        assert!(r.store.key(IDENTITY).is_none());
        assert_eq!(r.store.key(IDENTITY2).unwrap().as_bytes(), &[9u8; 32]);
    }

    #[tokio::test]
    async fn a_known_reply_for_a_computer_with_no_record_only_remembers_the_phone() {
        let r = rig(CableBehaviour::Reply(format!("CG1 KNOWN {PHONE_ID}"))).await;
        let outcome = pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(outcome, Outcome::KnownButNoKey);
        assert_eq!(r.store.phone_id(SERIAL), Some(PHONE_ID.to_string()));
        assert!(r.store.records().is_empty());
        // No record to rely on, so the next session asks again.
        pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(r.phone.cable_connections(), 2);
    }

    #[tokio::test]
    async fn the_switch_off_or_a_phone_without_the_capability_opens_nothing() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        let outcome = pair(&r, false, &wireless_hello()).await.unwrap();
        assert_eq!(outcome, Outcome::Skipped);
        let old_phone = hello(&["socks5-tcp", "session-watch-v1"]);
        assert_eq!(pair(&r, true, &old_phone).await.unwrap(), Outcome::Skipped);
        assert_eq!(r.phone.cable_connections(), 0);
        nothing_saved(&r);
    }

    #[tokio::test]
    async fn a_malformed_reply_is_an_error_and_saves_nothing() {
        for line in [
            "garbage".to_string(),
            String::new(),
            "CG1 KNOWN".to_string(),
            format!("{PHONE_ID} {IDENTITY} AAAA {}", encode_name("iPhone")),
            "A".repeat(600),
        ] {
            let r = rig(CableBehaviour::Reply(line.clone())).await;
            let error = pair(&r, true, &wireless_hello()).await.unwrap_err();
            assert!(matches!(error, CoreError::Pairing(_)), "{error}");
            // The log text never carries the reply.
            assert!(line.is_empty() || !error.to_string().contains(&line));
            nothing_saved(&r);
        }
    }

    #[tokio::test]
    async fn a_refused_connection_is_an_error_and_saves_nothing() {
        let r = rig(CableBehaviour::Refuse).await;
        assert!(pair(&r, true, &wireless_hello()).await.is_err());
        nothing_saved(&r);
    }

    #[tokio::test(start_paused = true)]
    async fn a_phone_that_never_answers_times_out_after_70_seconds_and_saves_nothing() {
        let r = rig(CableBehaviour::Silent).await;
        let started = tokio::time::Instant::now();
        let error = pair(&r, true, &wireless_hello()).await.unwrap_err();
        assert!(matches!(error, CoreError::Timeout), "{error}");
        assert!(started.elapsed() >= Duration::from_secs(70));
        assert!(started.elapsed() < Duration::from_secs(75));
        nothing_saved(&r);
        // The connection is closed rather than left hanging.
        closed(&r).await;
    }

    #[tokio::test]
    async fn an_unusable_identity_in_the_reply_saves_nothing() {
        let r = rig(CableBehaviour::Reply(new_line("records", 7, "iPhone"))).await;
        assert!(pair(&r, true, &wireless_hello()).await.is_err());
        nothing_saved(&r);
    }

    #[tokio::test]
    async fn a_known_reply_for_a_record_whose_key_is_missing_is_known_but_no_key() {
        struct Refuses;
        impl crate::wireless::store::SecretStore for Refuses {
            fn protect(&self, plain: &[u8]) -> std::io::Result<Vec<u8>> {
                PlainSecrets.protect(plain)
            }
            fn unprotect(&self, _: &[u8]) -> std::io::Result<Vec<u8>> {
                Err(std::io::Error::other("wrong machine"))
            }
        }
        let mut r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        // The key file can't be opened any more (another machine's, say): the record stays,
        // its key is gone.
        r.store = WirelessStore::open(r._dir.path(), Arc::new(Refuses)).unwrap();
        assert!(r.store.records()[0].stale);
        r.phone
            .set_cable(CableBehaviour::Reply(format!("CG1 KNOWN {PHONE_ID}")));
        let outcome = pair(&r, true, &wireless_hello()).await.unwrap();
        assert_eq!(outcome, Outcome::KnownButNoKey);
        assert_eq!(r.phone.cable_connections(), 2);
        assert!(r.store.records()[0].stale, "still unusable");
        assert!(r.store.key(IDENTITY).is_none());
    }

    #[tokio::test]
    async fn a_known_reply_with_the_key_present_is_still_already_paired() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        r.store.set_stale(PHONE_ID, true).unwrap();
        r.phone
            .set_cable(CableBehaviour::Reply(format!("CG1 KNOWN {PHONE_ID}")));
        assert_eq!(
            pair(&r, true, &wireless_hello()).await.unwrap(),
            Outcome::AlreadyPaired
        );
    }

    const OTHER_SERIAL: &str = "bbbb";

    /// The store as it was after pairing through `SERIAL`, for comparing after a refusal.
    fn snapshot(r: &Rig) -> (Vec<Record>, Option<String>, Option<String>, Vec<String>) {
        (
            r.store.records(),
            r.store.phone_id(SERIAL),
            r.store.phone_id(OTHER_SERIAL),
            r.store.usb_serials(PHONE_ID),
        )
    }

    #[tokio::test]
    async fn a_reply_naming_a_phone_paired_through_another_usb_device_is_refused() {
        for second in [
            format!("CG1 KNOWN {PHONE_ID}"),
            new_line(IDENTITY2, 9, "Not Eve"),
        ] {
            let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "Eve iPhone"))).await;
            pair(&r, true, &wireless_hello()).await.unwrap();
            let before = snapshot(&r);
            r.phone.set_cable(CableBehaviour::Reply(second.clone()));
            let error = pair_if_needed(
                &r.handle,
                &wireless_hello(),
                OTHER_SERIAL,
                true,
                "Desk PC",
                &r.store,
            )
            .await
            .unwrap_err();
            assert!(matches!(error, CoreError::Pairing(_)), "{second}: {error}");
            assert_eq!(snapshot(&r), before, "{second}: nothing changed");
            assert_eq!(r.store.phone_id(OTHER_SERIAL), None);
            assert_eq!(r.store.key(IDENTITY).unwrap().as_bytes(), &[7u8; 32]);
            assert!(r.store.key(IDENTITY2).is_none());
        }
    }

    #[tokio::test]
    async fn a_phone_whose_pairing_through_another_usb_device_is_stale_can_pair_again() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair(&r, true, &wireless_hello()).await.unwrap();
        r.store.set_stale(PHONE_ID, true).unwrap();
        r.phone
            .set_cable(CableBehaviour::Reply(new_line(IDENTITY2, 9, "iPhone")));
        let outcome = pair_if_needed(
            &r.handle,
            &wireless_hello(),
            OTHER_SERIAL,
            true,
            "Desk PC",
            &r.store,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Paired(_)), "{outcome:?}");
        assert_eq!(r.store.phone_id(OTHER_SERIAL), Some(PHONE_ID.to_string()));
    }

    #[tokio::test]
    async fn a_name_with_only_control_characters_is_sent_as_windows_pc() {
        let r = rig(CableBehaviour::Reply(new_line(IDENTITY, 7, "iPhone"))).await;
        pair_named(&r, true, &wireless_hello(), "\u{0}\n\u{7f}")
            .await
            .unwrap();
        closed(&r).await;
        let expected = cable::request(&r.store.local_id(), "Windows PC").into_bytes();
        assert_eq!(r.phone.cable_requests(), vec![expected]);
    }
}
