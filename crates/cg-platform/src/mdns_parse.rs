//! Multicast DNS messages for finding the phone on the access point (DNS-SD over mDNS, RFC 6762
//! and RFC 6763): building the queries and reading the answers. Only queries are ever built;
//! nothing here answers anyone. Platform-neutral so the parser is tested on every platform; the
//! Windows side (`windows::mdns`) owns the sockets and the system API.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The mDNS multicast group and port.
pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const MDNS_PORT: u16 = 5353;

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;

/// Following more compression pointers than this means a loop.
const MAX_POINTERS: usize = 64;
/// RFC 1035: a name is at most 255 bytes on the wire.
const MAX_NAME_LENGTH: usize = 255;

/// A phone advertising the CellGlide service on the access point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundPhone {
    /// The service instance name: the phone's identifier (phoneID).
    pub instance: String,
    pub address: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordData {
    Ptr(String),
    Srv {
        port: u16,
        target: String,
    },
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    /// Any other type (TXT, NSEC, ...): kept only so callers can see it was there.
    Other(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The owner name, labels joined with dots, without the trailing dot.
    pub name: String,
    pub ttl: u32,
    pub data: RecordData,
}

/// `_cellglide-p2p._tcp` → `_cellglide-p2p._tcp.local`.
pub fn service_name(service_type: &str) -> String {
    format!("{service_type}.local")
}

/// A query message (ID 0, no flags) asking each `(name, type)` in class IN, multicast answers
/// requested (QM).
pub fn build_query(questions: &[(&str, u16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + questions.len() * 48);
    let count = u16::try_from(questions.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    for (name, qtype) in questions.iter().take(usize::from(count)) {
        for label in name.split('.').filter(|label| !label.is_empty()) {
            let bytes = label.as_bytes();
            // A label is at most 63 bytes; a longer one is cut rather than corrupting the
            // message (no name this program asks for comes close).
            let bytes = &bytes[..bytes.len().min(63)];
            out.push(bytes.len() as u8);
            out.extend_from_slice(bytes);
        }
        out.push(0);
        out.extend_from_slice(&qtype.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
    }
    out
}

/// The answer, authority and additional records of an mDNS response. Queries (our own, looped
/// back, or other hosts') give nothing. A malformed or truncated message gives the records read
/// before the fault.
pub fn parse_response(packet: &[u8]) -> Vec<Record> {
    let mut records = Vec::new();
    if packet.len() < 12 || packet[2] & 0x80 == 0 {
        return records;
    }
    let count = |at: usize| usize::from(u16::from_be_bytes([packet[at], packet[at + 1]]));
    let questions = count(4);
    let total = count(6) + count(8) + count(10);
    let mut offset = 12;
    for _ in 0..questions {
        let Some((_, next)) = read_name(packet, offset) else {
            return records;
        };
        offset = next + 4;
    }
    for _ in 0..total {
        let Some((record, next)) = read_record(packet, offset) else {
            break;
        };
        records.extend(record);
        offset = next;
    }
    records
}

/// One resource record at `offset`: the record (None for a class other than IN) and the offset
/// after it.
fn read_record(packet: &[u8], offset: usize) -> Option<(Option<Record>, usize)> {
    let (name, offset) = read_name(packet, offset)?;
    let header = packet.get(offset..offset + 10)?;
    let rtype = u16::from_be_bytes([header[0], header[1]]);
    // The top bit of the class is mDNS's cache-flush bit.
    let class = u16::from_be_bytes([header[2], header[3]]) & 0x7FFF;
    let ttl = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let length = usize::from(u16::from_be_bytes([header[8], header[9]]));
    let start = offset + 10;
    let end = start + length;
    let rdata = packet.get(start..end)?;
    if class != 1 {
        return Some((None, end));
    }
    let data = match rtype {
        TYPE_PTR => RecordData::Ptr(read_name(packet, start)?.0),
        TYPE_SRV => {
            if length < 7 {
                return None;
            }
            RecordData::Srv {
                port: u16::from_be_bytes([rdata[4], rdata[5]]),
                target: read_name(packet, start + 6)?.0,
            }
        }
        TYPE_A => RecordData::A(Ipv4Addr::from(<[u8; 4]>::try_from(rdata).ok()?)),
        TYPE_AAAA => RecordData::Aaaa(Ipv6Addr::from(<[u8; 16]>::try_from(rdata).ok()?)),
        other => RecordData::Other(other),
    };
    Some((Some(Record { name, ttl, data }), end))
}

/// The name at `offset`, following compression pointers, and the offset after the name where it
/// starts (after the first pointer, if any). Pointers must point backwards and are limited in
/// number, so a crafted message cannot loop.
fn read_name(packet: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut after = None;
    let mut pointers = 0;
    let mut length = 0;
    loop {
        let byte = *packet.get(offset)?;
        match byte & 0xC0 {
            0x00 if byte == 0 => {
                return Some((labels.join("."), after.unwrap_or(offset + 1)));
            }
            0x00 => {
                let size = usize::from(byte);
                let label = packet.get(offset + 1..offset + 1 + size)?;
                length += size + 1;
                if length > MAX_NAME_LENGTH {
                    return None;
                }
                labels.push(String::from_utf8_lossy(label).into_owned());
                offset += 1 + size;
            }
            0xC0 => {
                let target = usize::from(byte & 0x3F) << 8 | usize::from(*packet.get(offset + 1)?);
                pointers += 1;
                if target >= offset || pointers > MAX_POINTERS {
                    return None;
                }
                after.get_or_insert(offset + 2);
                offset = target;
            }
            // 0x40 and 0x80 are reserved label types.
            _ => return None,
        }
    }
}

/// The most records `merge` keeps; further new ones are dropped, so a noisy device cannot grow
/// the set (and the follow-up queries built from it) without bound.
pub const MAX_RECORDS: usize = 512;

/// Adds `new` to `known`: a record already known is not added twice (its TTL is refreshed), and
/// a record with TTL 0 (a "goodbye") removes the one it names. At most `MAX_RECORDS` are kept.
pub fn merge(known: &mut Vec<Record>, new: Vec<Record>) {
    for record in new {
        let same = |r: &Record| {
            r.name.eq_ignore_ascii_case(&record.name) && same_data(&r.data, &record.data)
        };
        if record.ttl == 0 {
            known.retain(|r| !same(r));
        } else if let Some(existing) = known.iter_mut().find(|r| same(r)) {
            existing.ttl = record.ttl;
        } else if known.len() < MAX_RECORDS {
            known.push(record);
        }
    }
}

fn same_data(a: &RecordData, b: &RecordData) -> bool {
    match (a, b) {
        (RecordData::Ptr(a), RecordData::Ptr(b)) => a.eq_ignore_ascii_case(b),
        (
            RecordData::Srv {
                port: pa,
                target: ta,
            },
            RecordData::Srv {
                port: pb,
                target: tb,
            },
        ) => pa == pb && ta.eq_ignore_ascii_case(tb),
        _ => a == b,
    }
}

/// The instance label of `full` (`<instance>.<service>`), if it belongs to `service`.
pub fn instance_of(full: &str, service: &str) -> Option<String> {
    let cut = full.len().checked_sub(service.len() + 1)?;
    let (instance, rest) = full.split_at_checked(cut)?;
    let suffix = rest.strip_prefix('.')?;
    (suffix.eq_ignore_ascii_case(service) && !instance.is_empty()).then(|| instance.to_string())
}

/// Whether `address` is in the /24 of the access point's `local` address (Windows gives the
/// access point a /24, 192.168.137.0/24 by default).
pub fn in_access_point_subnet(local: IpAddr, address: IpAddr) -> bool {
    match (local, address) {
        (IpAddr::V4(local), IpAddr::V4(address)) => {
            local.octets()[..3] == address.octets()[..3] && address != local
        }
        _ => false,
    }
}

/// The phones `records` describe for `service` (`_cellglide-p2p._tcp.local`): a PTR to an
/// instance, the instance's SRV, and an A record of the SRV target inside the access point's
/// /24. An instance missing any of those is left out; one per instance.
pub fn assemble(records: &[Record], service: &str, local: IpAddr) -> Vec<FoundPhone> {
    let mut found: Vec<FoundPhone> = Vec::new();
    for record in records {
        let RecordData::Ptr(full) = &record.data else {
            continue;
        };
        if !record.name.eq_ignore_ascii_case(service) {
            continue;
        }
        let Some(instance) = instance_of(full, service) else {
            continue;
        };
        if found.iter().any(|phone| phone.instance == instance) {
            continue;
        }
        let srv = records.iter().find_map(|r| match &r.data {
            RecordData::Srv { port, target } if r.name.eq_ignore_ascii_case(full) => {
                Some((*port, target))
            }
            _ => None,
        });
        let Some((port, target)) = srv else { continue };
        let address = records.iter().find_map(|r| match r.data {
            RecordData::A(a)
                if r.name.eq_ignore_ascii_case(target)
                    && in_access_point_subnet(local, IpAddr::V4(a)) =>
            {
                Some(IpAddr::V4(a))
            }
            _ => None,
        });
        let Some(address) = address else { continue };
        found.push(FoundPhone {
            instance,
            address,
            port,
        });
    }
    found
}

/// The follow-up questions `records` still need for `service`: SRV for an instance seen only in
/// a PTR, A for an SRV target with no address yet. Responders usually send everything at once;
/// these cover the ones that don't.
pub fn missing_questions(records: &[Record], service: &str) -> Vec<(String, u16)> {
    let mut questions = Vec::new();
    for record in records {
        let RecordData::Ptr(full) = &record.data else {
            continue;
        };
        if !record.name.eq_ignore_ascii_case(service) || instance_of(full, service).is_none() {
            continue;
        }
        let srv = records.iter().find_map(|r| match &r.data {
            RecordData::Srv { target, .. } if r.name.eq_ignore_ascii_case(full) => Some(target),
            _ => None,
        });
        match srv {
            None => questions.push((full.clone(), TYPE_SRV)),
            Some(target) => {
                let has_a = records.iter().any(|r| {
                    matches!(r.data, RecordData::A(_)) && r.name.eq_ignore_ascii_case(target)
                });
                if !has_a {
                    questions.push((target.clone(), TYPE_A));
                }
            }
        }
    }
    questions.sort();
    questions.dedup();
    questions
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const SERVICE: &str = "_cellglide-p2p._tcp.local";
    const HOST: &str = "b4deb35b-bc63-4e31-99d6-2cb59d5c5079";
    const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 137, 1));

    fn label(out: &mut Vec<u8>, text: &str) {
        out.push(text.len() as u8);
        out.extend_from_slice(text.as_bytes());
    }

    fn pointer(out: &mut Vec<u8>, to: usize) {
        out.extend_from_slice(&(0xC000u16 | to as u16).to_be_bytes());
    }

    fn header(out: &mut Vec<u8>, rtype: u16, class: u16, ttl: u32, length: usize) {
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&class.to_be_bytes());
        out.extend_from_slice(&ttl.to_be_bytes());
        out.extend_from_slice(&(length as u16).to_be_bytes());
    }

    /// The answer the phone gave on 2026-11-13, laid out as mDNSResponder lays it out: the PTR
    /// in the answers, then SRV, TXT and A in the additional records, every repeated name
    /// compressed with a pointer to its first occurrence.
    fn phone_answer(address: [u8; 4], ptr_ttl: u32) -> Vec<u8> {
        let mut p = vec![0, 0, 0x84, 0x00, 0, 0, 0, 1, 0, 0, 0, 3];
        // PTR  _cellglide-p2p._tcp.local -> <instance>._cellglide-p2p._tcp.local
        let service_at = p.len();
        label(&mut p, "_cellglide-p2p");
        label(&mut p, "_tcp");
        let local_at = p.len();
        label(&mut p, "local");
        p.push(0);
        header(&mut p, TYPE_PTR, 1, ptr_ttl, 1 + INSTANCE.len() + 2);
        let instance_at = p.len();
        label(&mut p, INSTANCE);
        pointer(&mut p, service_at);
        // SRV  <instance> -> port 56582, <host>.local
        pointer(&mut p, instance_at);
        header(&mut p, TYPE_SRV, 0x8001, 120, 6 + 1 + HOST.len() + 2);
        p.extend_from_slice(&[0, 0, 0, 0]);
        p.extend_from_slice(&56582u16.to_be_bytes());
        let host_at = p.len();
        label(&mut p, HOST);
        pointer(&mut p, local_at);
        // TXT  <instance> -> one empty string
        pointer(&mut p, instance_at);
        header(&mut p, 16, 0x8001, 4500, 1);
        p.push(0);
        // A  <host>.local -> address
        pointer(&mut p, host_at);
        header(&mut p, TYPE_A, 0x8001, 120, 4);
        p.extend_from_slice(&address);
        p
    }

    #[test]
    fn reads_the_recorded_answer() {
        let records = parse_response(&phone_answer([192, 168, 137, 205], 4500));
        let instance_full = format!("{INSTANCE}.{SERVICE}");
        let host = format!("{HOST}.local");
        assert_eq!(
            records,
            vec![
                Record {
                    name: SERVICE.into(),
                    ttl: 4500,
                    data: RecordData::Ptr(instance_full.clone())
                },
                Record {
                    name: instance_full.clone(),
                    ttl: 120,
                    data: RecordData::Srv {
                        port: 56582,
                        target: host.clone()
                    }
                },
                Record {
                    name: instance_full,
                    ttl: 4500,
                    data: RecordData::Other(16)
                },
                Record {
                    name: host,
                    ttl: 120,
                    data: RecordData::A(Ipv4Addr::new(192, 168, 137, 205))
                },
            ]
        );
        assert_eq!(
            assemble(&records, SERVICE, LOCAL),
            vec![FoundPhone {
                instance: INSTANCE.into(),
                address: "192.168.137.205".parse().unwrap(),
                port: 56582,
            }]
        );
        assert!(missing_questions(&records, SERVICE).is_empty());
    }

    #[test]
    fn an_address_outside_the_access_point_is_ignored() {
        let records = parse_response(&phone_answer([192, 168, 1, 205], 4500));
        assert!(assemble(&records, SERVICE, LOCAL).is_empty());
        // The access point's own address is not a phone either.
        let records = parse_response(&phone_answer([192, 168, 137, 1], 4500));
        assert!(assemble(&records, SERVICE, LOCAL).is_empty());
    }

    #[test]
    fn queries_are_not_answers() {
        let query = build_query(&[(SERVICE, TYPE_PTR)]);
        assert!(parse_response(&query).is_empty());
        let mut expected = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        label(&mut expected, "_cellglide-p2p");
        label(&mut expected, "_tcp");
        label(&mut expected, "local");
        expected.extend_from_slice(&[0, 0, 12, 0, 1]);
        assert_eq!(query, expected);
    }

    #[test]
    fn malformed_messages_do_not_panic_or_loop() {
        let good = phone_answer([192, 168, 137, 205], 4500);
        for cut in 0..good.len() {
            let records = parse_response(&good[..cut]);
            assert!(records.len() <= 4);
        }
        // A pointer to itself, and one pointing forwards.
        let mut looped = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        pointer(&mut looped, 12);
        assert!(parse_response(&looped).is_empty());
        let mut forward = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        pointer(&mut forward, 14);
        forward.extend_from_slice(&[0, 0, 1, 0, 1]);
        assert!(parse_response(&forward).is_empty());
    }

    #[test]
    fn follow_up_questions_fill_the_gaps() {
        let full = format!("{INSTANCE}.{SERVICE}");
        let mut known = vec![Record {
            name: SERVICE.into(),
            ttl: 10,
            data: RecordData::Ptr(full.clone()),
        }];
        assert_eq!(
            missing_questions(&known, SERVICE),
            vec![(full.clone(), TYPE_SRV)]
        );
        known.push(Record {
            name: full.clone(),
            ttl: 10,
            data: RecordData::Srv {
                port: 56582,
                target: format!("{HOST}.local"),
            },
        });
        assert_eq!(
            missing_questions(&known, SERVICE),
            vec![(format!("{HOST}.local"), TYPE_A)]
        );
        assert!(assemble(&known, SERVICE, LOCAL).is_empty());
    }

    #[test]
    fn merging_dedupes_and_honours_goodbyes() {
        let mut known = Vec::new();
        merge(
            &mut known,
            parse_response(&phone_answer([192, 168, 137, 205], 4500)),
        );
        merge(
            &mut known,
            parse_response(&phone_answer([192, 168, 137, 205], 4500)),
        );
        assert_eq!(known.len(), 4);
        assert_eq!(assemble(&known, SERVICE, LOCAL).len(), 1);
        // The phone stops advertising: PTR with TTL 0.
        let goodbye = parse_response(&phone_answer([192, 168, 137, 205], 0));
        merge(&mut known, goodbye.into_iter().take(1).collect());
        assert!(assemble(&known, SERVICE, LOCAL).is_empty());
    }

    #[test]
    fn merging_caps_the_record_set() {
        let noisy = |from: usize, to: usize| -> Vec<Record> {
            (from..to)
                .map(|i| Record {
                    name: format!("host{i}.local"),
                    ttl: 120,
                    data: RecordData::A(Ipv4Addr::new(10, 0, 0, 1)),
                })
                .collect()
        };
        let mut known = Vec::new();
        merge(&mut known, noisy(0, MAX_RECORDS + 100));
        assert_eq!(known.len(), MAX_RECORDS);
        // Known records still refresh, and a goodbye still removes one and makes room.
        merge(&mut known, noisy(0, 1));
        assert_eq!(known.len(), MAX_RECORDS);
        let mut bye = noisy(0, 1);
        bye[0].ttl = 0;
        merge(&mut known, bye);
        assert_eq!(known.len(), MAX_RECORDS - 1);
        merge(&mut known, noisy(MAX_RECORDS + 100, MAX_RECORDS + 102));
        assert_eq!(known.len(), MAX_RECORDS);
    }

    #[test]
    fn instance_names() {
        assert_eq!(
            instance_of(&format!("{INSTANCE}.{SERVICE}"), SERVICE).as_deref(),
            Some(INSTANCE)
        );
        assert_eq!(
            instance_of("X._CELLGLIDE-P2P._tcp.local", SERVICE).as_deref(),
            Some("X")
        );
        assert_eq!(instance_of(SERVICE, SERVICE), None);
        assert_eq!(instance_of("X._other._tcp.local", SERVICE), None);
        assert_eq!(service_name(cg_core::wireless::SERVICE_TYPE), SERVICE);
    }
}
