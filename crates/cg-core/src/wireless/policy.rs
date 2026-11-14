//! The wireless session's decisions, as pure functions plus one small memory the session keeps
//! (`UnknownTracker`): which phone on the access point to use, and what a failed dial means.
//! Nothing here waits or does I/O; the caller passes the clock.
//!
//! The phone's address changes with every access point session, so no address from before can
//! be trusted. Within one `ConnectWireless` attempt an address is *verified* for a phone once a
//! TLS handshake with that phone's pairing identity completed there; only a refusal from a
//! verified address can tell that the phone forgot the pairing. A refusal from anywhere else
//! may be a device on the access point posing as the phone (it knows the access point's
//! passphrase and can advertise the phone's name), so it never marks the pairing stale.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

use super::ports::{DialError, FoundPhone};
use super::store::Record;

/// How long the access point waits for an iPhone after the user asked for a wireless connection.
pub const WAIT_FOR_PHONE: Duration = Duration::from_secs(300);
/// After a session ends, a new attempt this often…
pub const RETRY_EVERY: Duration = Duration::from_secs(2);
/// …for this long before giving up and closing the access point.
pub const RETRY_FOR: Duration = Duration::from_secs(60);
/// Right after code pairing the phone refuses the new identity until it has reloaded its keys:
/// an unknown identity this soon after pairing is retried and not even counted.
pub const FRESH_PAIRING_GRACE: Duration = Duration::from_secs(10);
/// TCP connect, TLS handshake and the request line, all together (the phone allows 5 s).
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(4);
/// The phone turned code pairing away (its pairing channel closed before the first line: another
/// pairing in progress, or the 60 s lockout after three failures): not asked again for this long.
pub const PAIRING_BUSY_WAIT: Duration = Duration::from_secs(60);
/// Two unknown identities from one verified address at least this far apart, with nothing else
/// in between, mean the phone really has forgotten the pairing.
pub const UNKNOWN_IDENTITY_SPAN: Duration = Duration::from_secs(5);

/// The phone to use, from one discovery round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// No usable phone this round (none found, or only ones this attempt must leave alone).
    None,
    /// A paired phone; dial `phone.address` with `record`'s identity.
    Paired { phone: FoundPhone, record: Record },
    /// Exactly one phone without a usable pairing: pair by code.
    Unpaired(FoundPhone),
    /// More than one phone without a usable pairing: don't guess.
    SeveralUnpaired,
    /// The chosen phone's name is advertised from more than one address and none of them is
    /// verified for it: something may be posing as it. Neither dial nor pair; wait for the next
    /// round.
    Ambiguous { instance: String },
}

/// What a failed dial means for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Try again.
    Retry,
    /// The phone, at an address verified for it, no longer knows this pairing: mark it stale,
    /// end, don't reconnect.
    EndStale,
    /// A pairing made in this attempt that the phone, at a verified address, keeps refusing
    /// after its grace: end without reconnecting (the pairing did not take on the phone).
    EndFailed,
    /// The retry window ran out on refusals that prove nothing (no verified address): end, and
    /// leave the record as it is (not stale). The user may start a new pairing themselves.
    EndRefused,
}

/// Every address `instance` was found at this round, in order of first appearance.
fn addresses(found: &[FoundPhone], instance: &str) -> Vec<FoundPhone> {
    let mut seen: Vec<FoundPhone> = Vec::new();
    for phone in found.iter().filter(|p| p.instance == instance) {
        if !seen.iter().any(|s| s.address == phone.address) {
            seen.push(phone.clone());
        }
    }
    seen
}

/// Instance names found this round, in order of first appearance.
fn instances(found: &[FoundPhone]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for phone in found {
        if !names.contains(&phone.instance.as_str()) {
            names.push(&phone.instance);
        }
    }
    names
}

/// Picks the phone to use from what one discovery round `found`.
///
/// - A phone with a pairing that is not stale wins; among several, the newest pairing
///   (`created_at`). Strangers beside it don't matter.
/// - Otherwise the phones without a usable pairing (no record, or a stale one: that phone only)
///   are pairing candidates: exactly one is `Unpaired`, more are `SeveralUnpaired`.
/// - `verified` maps a phone id to the address verified for it in this attempt (a handshake with
///   its pairing identity completed there). When the chosen phone's name is advertised from
///   several addresses, the verified one is dialled if it is among them; an unverified address
///   is never preferred over it, and several unverified ones are `Ambiguous`.
/// - `marked_stale` holds the phones whose pairing was good when this `ConnectWireless` began
///   and was marked stale during it. They are left alone (never paired by code in the same
///   attempt): only the user asking again may replace a pairing that was good.
pub fn pick(
    found: &[FoundPhone],
    records: &[Record],
    verified: &HashMap<String, IpAddr>,
    marked_stale: &HashSet<String>,
) -> Pick {
    let record_of = |instance: &str| records.iter().find(|r| r.peer_id == instance);
    // The one address to dial for `instance`; `None` when it is ambiguous.
    let resolve = |instance: &str| -> Option<FoundPhone> {
        let mut at = addresses(found, instance);
        if at.len() == 1 {
            return Some(at.remove(0));
        }
        let known = verified.get(instance);
        at.into_iter().find(|p| Some(&p.address) == known)
    };
    let ambiguous = |instance: &str| Pick::Ambiguous {
        instance: instance.to_string(),
    };

    let names = instances(found);
    let paired = names
        .iter()
        .filter_map(|name| record_of(name).filter(|r| !r.stale))
        .max_by_key(|r| r.created_at);
    if let Some(record) = paired {
        return match resolve(&record.peer_id) {
            Some(phone) => Pick::Paired {
                phone,
                record: record.clone(),
            },
            None => ambiguous(&record.peer_id),
        };
    }

    let candidates: Vec<&str> = names
        .into_iter()
        .filter(|name| !marked_stale.contains(*name))
        .collect();
    match candidates.as_slice() {
        [] => Pick::None,
        [only] => resolve(only).map_or_else(|| ambiguous(only), Pick::Unpaired),
        _ => Pick::SeveralUnpaired,
    }
}

/// Whether `since_pairing` (how long ago this pairing was made by code in this attempt) is still
/// within `FRESH_PAIRING_GRACE`.
fn in_grace(since_pairing: Option<Duration>) -> bool {
    since_pairing.is_some_and(|since| since < FRESH_PAIRING_GRACE)
}

/// Per address, when the current run of unknown identities began. A successful dial or any other
/// result from that address ends the run, so a phone that flaps (refused, then fine, then
/// refused hours later) never adds up to a verdict. Unknowns inside the fresh-pairing grace are
/// not recorded. The session holds one per `ConnectWireless` attempt.
#[derive(Clone, Debug, Default)]
pub struct UnknownTracker {
    first: HashMap<IpAddr, Instant>,
}

impl UnknownTracker {
    pub fn new() -> UnknownTracker {
        UnknownTracker::default()
    }

    /// An unknown identity from `address` at `now`. Ignored within the grace of a fresh pairing
    /// (`since_pairing` as for `DialContext`).
    pub fn on_unknown(&mut self, address: IpAddr, now: Instant, since_pairing: Option<Duration>) {
        if in_grace(since_pairing) {
            return;
        }
        self.first.entry(address).or_insert(now);
    }

    /// A successful dial or any failure other than an unknown identity from `address`.
    pub fn on_other(&mut self, address: IpAddr) {
        self.first.remove(&address);
    }

    /// How long the current run of unknowns from `address` has lasted at `now`; `None` when
    /// there is none.
    pub fn span(&self, address: IpAddr, now: Instant) -> Option<Duration> {
        self.first
            .get(&address)
            .map(|first| now.saturating_duration_since(*first))
    }

    pub fn clear(&mut self) {
        self.first.clear();
    }
}

/// What the caller knows about one failed dial to a paired phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialContext {
    /// How long ago this pairing was made by code in this attempt; `None` for one from before.
    pub since_pairing: Option<Duration>,
    /// The dialled address is verified for this phone in this attempt.
    pub verified: bool,
    /// `UnknownTracker::span` for the dialled address, after recording this failure.
    pub unknown_span: Option<Duration>,
    /// The caller's retry window (waiting for the phone, or reconnecting) has run out.
    pub window_over: bool,
}

/// What to do after a dial to a paired phone failed with `error`.
///
/// - Anything but an unknown identity: `Retry` (the caller's windows bound it, and end the
///   session their own way).
/// - An unknown identity at a verified address whose run of unknowns (`UnknownTracker`) has
///   lasted at least `UNKNOWN_IDENTITY_SPAN`: `EndStale`, or `EndFailed` for a pairing made in
///   this attempt (past its grace, since unknowns inside it are not recorded).
/// - Any other unknown identity: `Retry`, and `EndRefused` once the window is over.
pub fn after_dial_failure(error: &DialError, context: &DialContext) -> Next {
    if *error != DialError::UnknownIdentity {
        return Next::Retry;
    }
    let proven = context.verified
        && !in_grace(context.since_pairing)
        && context
            .unknown_span
            .is_some_and(|span| span >= UNKNOWN_IDENTITY_SPAN);
    if proven {
        return if context.since_pairing.is_some() {
            Next::EndFailed
        } else {
            Next::EndStale
        };
    }
    if context.window_over {
        Next::EndRefused
    } else {
        Next::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wireless::store::Method;

    const A: &str = "AAAAAAAA-0000-0000-0000-000000000001";
    const B: &str = "BBBBBBBB-0000-0000-0000-000000000002";
    const C: &str = "CCCCCCCC-0000-0000-0000-000000000003";

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 168, 137, last])
    }

    fn found(instance: &str, last: u8) -> FoundPhone {
        FoundPhone {
            instance: instance.into(),
            address: ip(last),
            port: 50_000 + u16::from(last),
        }
    }

    fn record(peer_id: &str, created_at: u64, stale: bool) -> Record {
        Record {
            peer_id: peer_id.into(),
            name: format!("{peer_id} name"),
            identity: format!("ID-{created_at}"),
            created_at,
            method: Method::Cable,
            stale,
        }
    }

    fn plain(found: &[FoundPhone], records: &[Record]) -> Pick {
        pick(found, records, &HashMap::new(), &HashSet::new())
    }

    #[test]
    fn the_constants() {
        assert_eq!(WAIT_FOR_PHONE, Duration::from_secs(300));
        assert_eq!(RETRY_EVERY, Duration::from_secs(2));
        assert_eq!(RETRY_FOR, Duration::from_secs(60));
        assert_eq!(FRESH_PAIRING_GRACE, Duration::from_secs(10));
        assert_eq!(DIAL_TIMEOUT, Duration::from_secs(4));
        assert_eq!(UNKNOWN_IDENTITY_SPAN, Duration::from_secs(5));
    }

    #[test]
    fn nothing_found_is_none() {
        assert_eq!(plain(&[], &[record(A, 1, false)]), Pick::None);
    }

    #[test]
    fn a_paired_phone_is_picked() {
        let records = [record(A, 1, false)];
        assert_eq!(
            plain(&[found(A, 5)], &records),
            Pick::Paired {
                phone: found(A, 5),
                record: records[0].clone()
            }
        );
    }

    #[test]
    fn of_two_paired_phones_the_newest_pairing_wins() {
        let records = [record(A, 10, false), record(B, 20, false)];
        let expected = Pick::Paired {
            phone: found(B, 6),
            record: records[1].clone(),
        };
        assert_eq!(plain(&[found(A, 5), found(B, 6)], &records), expected);
        assert_eq!(plain(&[found(B, 6), found(A, 5)], &records), expected);
    }

    #[test]
    fn a_paired_phone_beats_a_stranger() {
        let records = [record(A, 1, false)];
        let expected = Pick::Paired {
            phone: found(A, 5),
            record: records[0].clone(),
        };
        assert_eq!(plain(&[found(C, 9), found(A, 5)], &records), expected);
    }

    #[test]
    fn the_phone_of_a_stale_record_is_unpaired() {
        let records = [record(A, 1, true)];
        assert_eq!(plain(&[found(A, 5)], &records), Pick::Unpaired(found(A, 5)));
    }

    #[test]
    fn a_stale_record_only_concerns_its_own_phone() {
        // B's pairing is still good, so it wins over A, whose pairing went stale.
        let records = [record(A, 30, true), record(B, 20, false)];
        assert_eq!(
            plain(&[found(A, 5), found(B, 6)], &records),
            Pick::Paired {
                phone: found(B, 6),
                record: records[1].clone()
            }
        );
        // A stale phone and a stranger are both pairing candidates.
        assert_eq!(
            plain(&[found(A, 5), found(C, 9)], &records),
            Pick::SeveralUnpaired
        );
    }

    #[test]
    fn one_stranger_is_unpaired_and_two_are_several() {
        assert_eq!(plain(&[found(C, 9)], &[]), Pick::Unpaired(found(C, 9)));
        assert_eq!(
            plain(&[found(C, 9), found(B, 6)], &[record(A, 1, false)]),
            Pick::SeveralUnpaired
        );
    }

    #[test]
    fn the_same_phone_twice_at_one_address_is_one_phone() {
        assert_eq!(
            plain(&[found(C, 9), found(C, 9)], &[]),
            Pick::Unpaired(found(C, 9))
        );
    }

    #[test]
    fn one_name_at_two_addresses_is_ambiguous() {
        let records = [record(A, 1, false)];
        let ambiguous = |instance: &str| Pick::Ambiguous {
            instance: instance.into(),
        };
        assert_eq!(plain(&[found(A, 5), found(A, 7)], &records), ambiguous(A));
        assert_eq!(plain(&[found(C, 5), found(C, 7)], &records), ambiguous(C));
        // A stale pairing's phone is not paired by code when its name is ambiguous either.
        assert_eq!(
            plain(&[found(A, 5), found(A, 7)], &[record(A, 1, true)]),
            ambiguous(A)
        );
    }

    #[test]
    fn the_verified_address_settles_an_ambiguous_paired_phone() {
        let records = [record(A, 1, false)];
        let verified = HashMap::from([(A.to_string(), ip(7))]);
        assert_eq!(
            pick(
                &[found(A, 5), found(A, 7)],
                &records,
                &verified,
                &HashSet::new()
            ),
            Pick::Paired {
                phone: found(A, 7),
                record: records[0].clone()
            }
        );
        // Not among the addresses: still ambiguous.
        let elsewhere = HashMap::from([(A.to_string(), ip(8))]);
        assert_eq!(
            pick(
                &[found(A, 5), found(A, 7)],
                &records,
                &elsewhere,
                &HashSet::new()
            ),
            Pick::Ambiguous { instance: A.into() }
        );
        // Found at one new address only: that one is dialled.
        assert_eq!(
            pick(&[found(A, 5)], &records, &elsewhere, &HashSet::new()),
            Pick::Paired {
                phone: found(A, 5),
                record: records[0].clone()
            }
        );
    }

    #[test]
    fn a_pairing_marked_stale_in_this_attempt_is_not_paired_again() {
        let records = [record(A, 1, true)];
        let marked = HashSet::from([A.to_string()]);
        assert_eq!(
            pick(&[found(A, 5)], &records, &HashMap::new(), &marked),
            Pick::None
        );
        // Another phone is still a candidate, alone.
        assert_eq!(
            pick(
                &[found(A, 5), found(C, 9)],
                &records,
                &HashMap::new(),
                &marked
            ),
            Pick::Unpaired(found(C, 9))
        );
    }

    fn secs(s: u64) -> Option<Duration> {
        Some(Duration::from_secs(s))
    }

    fn context(
        verified: bool,
        since_pairing: Option<Duration>,
        span: Option<Duration>,
    ) -> DialContext {
        DialContext {
            since_pairing,
            verified,
            unknown_span: span,
            window_over: false,
        }
    }

    #[test]
    fn other_failures_are_retried() {
        for error in [DialError::Timeout, DialError::Io("reset".into())] {
            for verified in [false, true] {
                for window_over in [false, true] {
                    let context = DialContext {
                        window_over,
                        ..context(verified, secs(30), secs(30))
                    };
                    assert_eq!(after_dial_failure(&error, &context), Next::Retry);
                }
            }
        }
    }

    #[test]
    fn unknowns_at_a_verified_address_five_seconds_apart_are_stale() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(
            after_dial_failure(&unknown, &context(true, None, secs(0))),
            Next::Retry
        );
        assert_eq!(
            after_dial_failure(
                &unknown,
                &context(true, None, Some(Duration::from_millis(4_999)))
            ),
            Next::Retry
        );
        assert_eq!(
            after_dial_failure(&unknown, &context(true, None, secs(5))),
            Next::EndStale
        );
        assert_eq!(
            after_dial_failure(&unknown, &context(true, None, secs(40))),
            Next::EndStale
        );
    }

    #[test]
    fn unknowns_at_an_unverified_address_are_never_stale() {
        let unknown = DialError::UnknownIdentity;
        for span in [None, secs(0), secs(5), secs(3600)] {
            assert_eq!(
                after_dial_failure(&unknown, &context(false, None, span)),
                Next::Retry
            );
            assert_eq!(
                after_dial_failure(&unknown, &context(false, secs(30), span)),
                Next::Retry
            );
            let over = DialContext {
                window_over: true,
                ..context(false, None, span)
            };
            assert_eq!(after_dial_failure(&unknown, &over), Next::EndRefused);
        }
    }

    #[test]
    fn a_window_ending_on_unproven_unknowns_at_a_verified_address_is_refused() {
        let over = DialContext {
            window_over: true,
            ..context(true, None, secs(2))
        };
        assert_eq!(
            after_dial_failure(&DialError::UnknownIdentity, &over),
            Next::EndRefused
        );
    }

    #[test]
    fn a_fresh_pairing_refused_at_a_verified_address_after_its_grace_has_failed() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(
            after_dial_failure(&unknown, &context(true, secs(9), secs(9))),
            Next::Retry
        );
        assert_eq!(
            after_dial_failure(&unknown, &context(true, secs(12), secs(4))),
            Next::Retry
        );
        assert_eq!(
            after_dial_failure(&unknown, &context(true, secs(16), secs(5))),
            Next::EndFailed
        );
        // Not at an unverified address.
        assert_eq!(
            after_dial_failure(&unknown, &context(false, secs(16), secs(5))),
            Next::Retry
        );
    }

    #[test]
    fn the_tracker_ignores_unknowns_inside_the_grace() {
        let t0 = Instant::now();
        let mut tracker = UnknownTracker::new();
        tracker.on_unknown(ip(5), t0, secs(3));
        tracker.on_unknown(ip(5), t0 + Duration::from_secs(6), secs(9));
        assert_eq!(tracker.span(ip(5), t0 + Duration::from_secs(7)), None);
        tracker.on_unknown(ip(5), t0 + Duration::from_secs(8), secs(10));
        assert_eq!(tracker.span(ip(5), t0 + Duration::from_secs(9)), secs(1));
    }

    #[test]
    fn the_tracker_resets_on_success_or_another_result() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut tracker = UnknownTracker::new();
        tracker.on_unknown(ip(5), at(0), None);
        tracker.on_unknown(ip(5), at(3), None);
        assert_eq!(tracker.span(ip(5), at(3)), secs(3));
        // Another address has its own run.
        assert_eq!(tracker.span(ip(6), at(3)), None);
        tracker.on_other(ip(5));
        assert_eq!(tracker.span(ip(5), at(4)), None);
        tracker.on_unknown(ip(5), at(10), None);
        assert_eq!(tracker.span(ip(5), at(10)), secs(0));
        tracker.clear();
        assert_eq!(tracker.span(ip(5), at(10)), None);
    }

    /// One attempt's dials, as Task 4's session will drive them: record the result in the
    /// tracker and the verified map, then ask `after_dial_failure`.
    struct Attempt {
        start: Instant,
        window: Duration,
        paired_at: Option<Instant>,
        tracker: UnknownTracker,
        verified: HashMap<String, IpAddr>,
    }

    impl Attempt {
        fn new(start: Instant, window: Duration) -> Attempt {
            Attempt {
                start,
                window,
                paired_at: None,
                tracker: UnknownTracker::new(),
                verified: HashMap::new(),
            }
        }

        /// `None` for a successful dial.
        fn dial(
            &mut self,
            phone: &str,
            address: IpAddr,
            now: Instant,
            result: Result<(), DialError>,
        ) -> Option<Next> {
            let since_pairing = self.paired_at.map(|t| now.duration_since(t));
            match result {
                Ok(()) => {
                    self.tracker.on_other(address);
                    self.verified.insert(phone.into(), address);
                    None
                }
                Err(error) => {
                    if error == DialError::UnknownIdentity {
                        self.tracker.on_unknown(address, now, since_pairing);
                    } else {
                        self.tracker.on_other(address);
                    }
                    let context = DialContext {
                        since_pairing,
                        verified: self.verified.get(phone) == Some(&address),
                        unknown_span: self.tracker.span(address, now),
                        window_over: now.duration_since(self.start) >= self.window,
                    };
                    Some(after_dial_failure(&error, &context))
                }
            }
        }
    }

    #[test]
    fn scenario_an_impostor_elsewhere_with_the_real_phone_absent_is_never_stale() {
        let records = [record(A, 1, false)];
        let t0 = Instant::now();
        let mut attempt = Attempt::new(t0, WAIT_FOR_PHONE);
        let mut outcomes = Vec::new();
        for round in 0u64.. {
            let now = t0 + RETRY_EVERY * round as u32;
            let Pick::Paired { phone, .. } = pick(
                &[found(A, 66)],
                &records,
                &attempt.verified,
                &HashSet::new(),
            ) else {
                panic!("the impostor's advert looks like the paired phone");
            };
            let next = attempt
                .dial(A, phone.address, now, Err(DialError::UnknownIdentity))
                .unwrap();
            outcomes.push(next);
            if next != Next::Retry {
                break;
            }
        }
        assert!(!outcomes.contains(&Next::EndStale));
        assert_eq!(outcomes.last(), Some(&Next::EndRefused));
        assert_eq!(outcomes.len(), 151, "every 2 s for 300 s");
    }

    #[test]
    fn scenario_both_present_with_discovery_missing_rounds_is_never_stale() {
        let records = [record(A, 1, false)];
        // The real phone is missed in 30% of rounds. First the impostor is seen in every round;
        // then it is missed now and then too, so the real phone is sometimes seen alone.
        for impostor_misses in [false, true] {
            let t0 = Instant::now();
            let mut attempt = Attempt::new(t0, WAIT_FOR_PHONE);
            let mut outcomes = Vec::new();
            let mut connected_at = None;
            for round in 0..150u32 {
                let now = t0 + RETRY_EVERY * round;
                let mut seen = Vec::new();
                if ![1, 4, 7].contains(&(round % 10)) {
                    seen.push(found(A, 205));
                }
                if !(impostor_misses && [2, 5].contains(&(round % 10))) {
                    seen.push(found(A, 66));
                }
                let pick = pick(&seen, &records, &attempt.verified, &HashSet::new());
                let Pick::Paired { phone, .. } = pick else {
                    assert_eq!(pick, Pick::Ambiguous { instance: A.into() });
                    continue;
                };
                let result = if phone.address == ip(205) {
                    connected_at.get_or_insert(round);
                    Ok(())
                } else {
                    Err(DialError::UnknownIdentity)
                };
                if let Some(next) = attempt.dial(A, phone.address, now, result) {
                    outcomes.push(next);
                }
            }
            assert!(!outcomes.contains(&Next::EndStale), "{outcomes:?}");
            if impostor_misses {
                // Round 2: the real phone alone. After that, with both seen, the verified
                // address is the one dialled.
                assert_eq!(connected_at, Some(2));
                assert_eq!(attempt.verified.get(A), Some(&ip(205)));
                assert_eq!(
                    pick(
                        &[found(A, 66), found(A, 205)],
                        &records,
                        &attempt.verified,
                        &HashSet::new()
                    ),
                    Pick::Paired {
                        phone: found(A, 205),
                        record: records[0].clone()
                    }
                );
            } else {
                assert_eq!(connected_at, None);
            }
        }
    }

    #[test]
    fn scenario_unknowns_at_a_verified_address_five_seconds_apart_end_stale() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut attempt = Attempt::new(t0, RETRY_FOR);
        assert_eq!(attempt.dial(A, ip(205), at(0), Ok(())), None);
        // The user removes this computer on the phone.
        let unknown = || Err(DialError::UnknownIdentity);
        assert_eq!(
            attempt.dial(A, ip(205), at(2), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(
            attempt.dial(A, ip(205), at(4), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(
            attempt.dial(A, ip(205), at(6), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(
            attempt.dial(A, ip(205), at(8), unknown()),
            Some(Next::EndStale)
        );
    }

    #[test]
    fn scenario_a_flapping_phone_is_not_stale() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut attempt = Attempt::new(t0, Duration::from_secs(10 * 3600));
        let unknown = || Err(DialError::UnknownIdentity);
        assert_eq!(attempt.dial(A, ip(205), at(0), Ok(())), None);
        assert_eq!(
            attempt.dial(A, ip(205), at(10), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(attempt.dial(A, ip(205), at(12), Ok(())), None);
        // Hours later, one more refusal: a new run, not one that began at 10 s.
        assert_eq!(
            attempt.dial(A, ip(205), at(3 * 3600), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(
            attempt.dial(A, ip(205), at(3 * 3600 + 2), Err(DialError::Timeout)),
            Some(Next::Retry)
        );
        assert_eq!(
            attempt.dial(A, ip(205), at(3 * 3600 + 7), unknown()),
            Some(Next::Retry)
        );
        assert_eq!(attempt.dial(A, ip(205), at(3 * 3600 + 9), Ok(())), None);
    }

    #[test]
    fn scenario_a_fresh_pairing_reloading_for_ten_and_a_half_seconds_does_not_fail() {
        let t0 = Instant::now();
        let mut attempt = Attempt::new(t0, RETRY_FOR);
        attempt.paired_at = Some(t0);
        // Even at an address already verified for the phone (the strictest case).
        attempt.verified.insert(A.into(), ip(205));
        let mut outcomes = Vec::new();
        for half_seconds in 0..21u64 {
            let now = t0 + Duration::from_millis(500 * half_seconds);
            outcomes.push(attempt.dial(A, ip(205), now, Err(DialError::UnknownIdentity)));
        }
        let now = t0 + Duration::from_millis(10_500);
        outcomes.push(attempt.dial(A, ip(205), now, Ok(())));
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, Some(Next::Retry) | None)),
            "{outcomes:?}"
        );
    }

    #[test]
    fn scenario_a_fresh_pairing_still_refused_after_its_grace() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let unknown = || Err(DialError::UnknownIdentity);
        // At a verified address: failed, once the post-grace refusals span 5 s.
        let mut attempt = Attempt::new(t0, RETRY_FOR);
        attempt.paired_at = Some(t0);
        attempt.verified.insert(A.into(), ip(205));
        for s in [0, 4, 8, 10, 12, 14] {
            assert_eq!(
                attempt.dial(A, ip(205), at(s), unknown()),
                Some(Next::Retry),
                "{s}"
            );
        }
        assert_eq!(
            attempt.dial(A, ip(205), at(16), unknown()),
            Some(Next::EndFailed)
        );
        // Elsewhere: retried until the window is over, then refused.
        let mut attempt = Attempt::new(t0, RETRY_FOR);
        attempt.paired_at = Some(t0);
        for s in (0..60).step_by(2) {
            assert_eq!(
                attempt.dial(A, ip(66), at(s), unknown()),
                Some(Next::Retry),
                "{s}"
            );
        }
        assert_eq!(
            attempt.dial(A, ip(66), at(60), unknown()),
            Some(Next::EndRefused)
        );
    }
}
