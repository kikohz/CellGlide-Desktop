//! The wireless session's decisions, as pure functions: which phone on the access point to use,
//! and what a failed dial means. The controller keeps the clocks and the memory; nothing here
//! waits or does I/O.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use super::ports::{DialError, FoundPhone};
use super::store::Record;

/// How long the access point waits for an iPhone after the user asked for a wireless connection.
pub const WAIT_FOR_PHONE: Duration = Duration::from_secs(300);
/// After a session ends, a new attempt this often…
pub const RETRY_EVERY: Duration = Duration::from_secs(2);
/// …for this long before giving up and closing the access point.
pub const RETRY_FOR: Duration = Duration::from_secs(60);
/// Right after code pairing the phone refuses the new identity until it has reloaded its keys:
/// an unknown identity this soon after pairing is retried, never taken as a verdict.
pub const FRESH_PAIRING_GRACE: Duration = Duration::from_secs(10);
/// TCP connect, TLS handshake and the request line, all together (the phone allows 5 s).
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(4);
/// Two unknown identities from one address at least this far apart mean the phone really has
/// forgotten the pairing; one alone, or a burst within this span, could be a device on the
/// access point posing as the phone.
pub const UNKNOWN_IDENTITY_SPAN: Duration = Duration::from_secs(5);

/// The phone to use, from one discovery round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// No usable phone this round (none found, or only ones this session must leave alone).
    None,
    /// A paired phone; dial `phone.address` with `record`'s identity.
    Paired { phone: FoundPhone, record: Record },
    /// Exactly one phone without a usable pairing: pair by code.
    Unpaired(FoundPhone),
    /// More than one phone without a usable pairing: don't guess.
    SeveralUnpaired,
    /// The chosen phone's name is advertised from more than one address and none of them is
    /// where it was last connected: something is posing as it. Neither dial nor pair; wait for
    /// the next round.
    Ambiguous { instance: String },
}

/// What a failed dial means for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Try again (the session's own windows bound how long).
    Retry,
    /// The phone no longer knows this pairing: mark it stale, end, don't reconnect.
    EndStale,
    /// A pairing made in this session that the phone keeps refusing after its grace: end
    /// without reconnecting (the pairing did not take on the phone).
    EndFailed,
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
/// - `last_good` maps a phone id to the address it was last connected at. When the chosen
///   phone's name is advertised from several addresses, its last good address is dialled if it
///   is among them; otherwise the round is `Ambiguous`.
/// - `marked_stale` holds the phones whose pairing was good when this `ConnectWireless` began
///   and was marked stale during it. They are left alone (never paired by code in the same
///   attempt): only the user asking again may replace a pairing that was good.
pub fn pick(
    found: &[FoundPhone],
    records: &[Record],
    last_good: &HashMap<String, IpAddr>,
    marked_stale: &HashSet<String>,
) -> Pick {
    let record_of = |instance: &str| records.iter().find(|r| r.peer_id == instance);
    // The one address to dial for `instance`; `None` when it is ambiguous.
    let resolve = |instance: &str| -> Option<FoundPhone> {
        let mut at = addresses(found, instance);
        if at.len() == 1 {
            return Some(at.remove(0));
        }
        let known = last_good.get(instance);
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

/// What to do after a dial to a paired phone failed with `error`.
///
/// - `since_pairing`: how long ago this pairing was made by code in this session (`None` for a
///   pairing from before).
/// - `unknown_before`: how long ago the first unknown identity from this same address came, in
///   this session (`None` when this is the first).
///
/// An unknown identity is retried within `FRESH_PAIRING_GRACE` of pairing, the first time, and
/// while the unknowns from that address span less than `UNKNOWN_IDENTITY_SPAN`; two at least that
/// far apart end the session as stale (or, for a pairing made in this session, as failed).
/// Every other failure is retried: the session's own windows bound it.
pub fn after_dial_failure(
    error: &DialError,
    since_pairing: Option<Duration>,
    unknown_before: Option<Duration>,
) -> Next {
    if *error != DialError::UnknownIdentity {
        return Next::Retry;
    }
    if since_pairing.is_some_and(|since| since < FRESH_PAIRING_GRACE) {
        return Next::Retry;
    }
    match unknown_before {
        Some(span) if span >= UNKNOWN_IDENTITY_SPAN => {
            if since_pairing.is_some() {
                Next::EndFailed
            } else {
                Next::EndStale
            }
        }
        _ => Next::Retry,
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
    fn the_last_good_address_settles_an_ambiguous_paired_phone() {
        let records = [record(A, 1, false)];
        let last_good = HashMap::from([(A.to_string(), ip(7))]);
        assert_eq!(
            pick(
                &[found(A, 5), found(A, 7)],
                &records,
                &last_good,
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

    #[test]
    fn other_failures_are_retried() {
        for error in [DialError::Timeout, DialError::Io("reset".into())] {
            assert_eq!(after_dial_failure(&error, None, None), Next::Retry);
            assert_eq!(after_dial_failure(&error, None, secs(30)), Next::Retry);
            assert_eq!(after_dial_failure(&error, secs(30), secs(30)), Next::Retry);
        }
    }

    #[test]
    fn an_unknown_identity_right_after_pairing_is_retried() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(after_dial_failure(&unknown, secs(0), None), Next::Retry);
        assert_eq!(after_dial_failure(&unknown, secs(9), secs(9)), Next::Retry);
        assert_eq!(
            after_dial_failure(&unknown, Some(Duration::from_millis(9_999)), secs(60)),
            Next::Retry
        );
    }

    #[test]
    fn the_first_unknown_identity_or_a_burst_is_retried() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(after_dial_failure(&unknown, None, None), Next::Retry);
        assert_eq!(after_dial_failure(&unknown, None, secs(0)), Next::Retry);
        assert_eq!(
            after_dial_failure(&unknown, None, Some(Duration::from_millis(4_999))),
            Next::Retry
        );
    }

    #[test]
    fn two_unknown_identities_five_seconds_apart_are_stale() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(after_dial_failure(&unknown, None, secs(5)), Next::EndStale);
        assert_eq!(after_dial_failure(&unknown, None, secs(40)), Next::EndStale);
    }

    #[test]
    fn a_new_pairing_still_refused_after_its_grace_has_failed() {
        let unknown = DialError::UnknownIdentity;
        assert_eq!(after_dial_failure(&unknown, secs(10), None), Next::Retry);
        assert_eq!(after_dial_failure(&unknown, secs(12), secs(4)), Next::Retry);
        assert_eq!(
            after_dial_failure(&unknown, secs(12), secs(5)),
            Next::EndFailed
        );
    }
}
