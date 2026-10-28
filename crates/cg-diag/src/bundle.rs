//! What a user sends us: state, the timeline and CellGlide's own log, redacted and kept
//! under 1 MB. The same format as the Mac app's `DiagnosticsBundle`, with platform "linux".

use crate::redact::redact;
pub use cg_ipc::Contact;
use cg_ipc::health::{EventKind, HealthEvent};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 1_000_000;
pub const MAX_LOG_LINES: usize = 2000;
pub const MAX_ENVIRONMENT_VALUE: usize = 2000;
pub const MAX_DIAGNOSTICS: usize = 200_000;
/// The redactor's host pattern is quadratic on a long run without spaces, so a single
/// log or timeline line is cut before redaction.
pub const MAX_LINE: usize = 2000;
pub const MAX_NOTE: usize = 2000;
pub const MAX_EMAIL: usize = 200;
pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsBundle {
    pub format: u32,
    pub platform: String,
    pub created_at: Timestamp,
    pub app: String,
    pub os: String,
    pub device: String,
    pub from: Timestamp,
    pub to: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
    pub log: Vec<String>,
    /// Sharing session IDs seen in the range: the same ID in a Mac, phone or Linux bundle
    /// means the same sharing session. Random UUIDs, so not redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<String>>,
}

/// Whole seconds: the Mac's `.iso8601` decoder rejects fractional ones.
fn whole_seconds(t: Timestamp) -> Timestamp {
    Timestamp::from_second(t.as_second()).unwrap_or(t)
}

fn take_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

fn tail_chars(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    match text.char_indices().nth(count - limit) {
        Some((start, _)) => &text[start..],
        None => text,
    }
}

impl DiagnosticsBundle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        platform: &str,
        created_at: Timestamp,
        app: String,
        os: String,
        device: String,
        from: Timestamp,
        to: Timestamp,
        environment: BTreeMap<String, String>,
        timeline: Option<Vec<String>>,
        diagnostics: Option<String>,
        log: Vec<String>,
    ) -> Self {
        DiagnosticsBundle {
            format: FORMAT,
            platform: platform.to_string(),
            created_at: whole_seconds(created_at),
            app,
            os,
            device,
            from: whole_seconds(from),
            to: whole_seconds(to),
            contact: None,
            environment,
            timeline,
            diagnostics,
            log,
            sessions: None,
        }
    }

    /// Every text field redacted and capped, the newest lines kept, and the whole thing
    /// cut down until it encodes within `MAX_BYTES`.
    pub fn prepared(
        &self,
        names: &BTreeMap<String, String>,
        user_name: Option<&str>,
    ) -> DiagnosticsBundle {
        let clean = |text: &str| redact(text, names, user_name);
        let line = |text: &str| {
            if text.chars().count() > MAX_LINE {
                clean(&format!("{}…", take_chars(text, MAX_LINE)))
            } else {
                clean(text)
            }
        };
        let block = |text: &str| text.split('\n').map(line).collect::<Vec<_>>().join("\n");
        let tail = |text: String, limit: usize| tail_chars(&text, limit).to_string();

        let mut copy = self.clone();
        copy.created_at = whole_seconds(self.created_at);
        copy.from = whole_seconds(self.from);
        copy.to = whole_seconds(self.to);
        copy.app = clean(&self.app);
        copy.os = clean(&self.os);
        copy.device = clean(&self.device);
        // Keys can carry names too (e.g. "Wi-Fi (Alice’s iPhone)"). BTreeMap order is sorted, so a
        // collision is deterministic: the first keeps the key, later ones get " (2)", " (3)".
        let mut environment = BTreeMap::new();
        for (key, value) in &self.environment {
            let mut name = clean(key);
            if environment.contains_key(&name) {
                let mut n = 2;
                while environment.contains_key(&format!("{name} ({n})")) {
                    n += 1;
                }
                name = format!("{name} ({n})");
            }
            environment.insert(name, tail(block(value), MAX_ENVIRONMENT_VALUE));
        }
        copy.environment = environment;
        copy.timeline = self
            .timeline
            .as_ref()
            .map(|t| t.iter().map(|l| line(l)).collect());
        copy.diagnostics = self
            .diagnostics
            .as_ref()
            .map(|d| tail(block(d), MAX_DIAGNOSTICS));
        let skip = self.log.len().saturating_sub(MAX_LOG_LINES);
        copy.log = self.log[skip..].iter().map(|l| line(l)).collect();
        // The contact is the user's own words, sent on purpose, so it is not redacted; it is only capped.
        if let Some(contact) = copy.contact.as_mut() {
            contact.note = contact
                .note
                .take()
                .map(|n| take_chars(&n, MAX_NOTE).to_string());
            contact.email = contact
                .email
                .take()
                .map(|e| take_chars(&e, MAX_EMAIL).to_string());
        }
        let too_big = |bundle: &DiagnosticsBundle| bundle.encoded().len() > MAX_BYTES;
        while too_big(&copy) {
            if copy.log.len() > 100 {
                copy.log.drain(..copy.log.len() / 4);
            } else if let Some(timeline) = copy.timeline.as_mut().filter(|t| t.len() > 100) {
                timeline.drain(..timeline.len() / 4);
            } else {
                copy.log.clear();
                if let Some(timeline) = copy.timeline.as_mut() {
                    timeline.drain(..timeline.len().saturating_sub(100));
                }
                if too_big(&copy) {
                    copy.timeline = copy.timeline.map(|_| Vec::new());
                    copy.diagnostics = None;
                }
                break;
            }
        }
        copy
    }

    /// Pretty JSON with keys in sorted order (`serde_json::Value` objects are sorted maps).
    pub fn encoded(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("a bundle is plain data");
        serde_json::to_vec_pretty(&value).expect("a JSON value encodes")
    }
}

/// The sharing session IDs in a timeline: a `tunnelStarted` event whose detail is
/// `session <UUID>`. First seen first, each once.
pub fn sessions(timeline: &[HealthEvent]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for event in timeline {
        if event.kind != EventKind::TunnelStarted {
            continue;
        }
        let Some(rest) = event
            .detail
            .as_deref()
            .and_then(|d| d.strip_prefix("session "))
        else {
            continue;
        };
        let id = take_chars(rest, 36);
        if is_uuid(id) && !seen.iter().any(|s| s == id) {
            seen.push(id.to_string());
        }
    }
    seen
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::health::EventKind;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn bundle(log: Vec<String>, timeline: Option<Vec<String>>) -> DiagnosticsBundle {
        DiagnosticsBundle {
            format: 1,
            platform: "linux".into(),
            created_at: ts(1_791_000_000),
            app: "1.0.0".into(),
            os: "Ubuntu 24.04".into(),
            device: "ThinkPad".into(),
            from: ts(1_790_913_600),
            to: ts(1_791_000_000),
            contact: None,
            environment: BTreeMap::from([("Status".into(), "TUN running at 10.0.0.2".into())]),
            timeline,
            diagnostics: None,
            log,
            sessions: None,
        }
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn none() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    // testPreparedRedactsEveryTextField
    #[test]
    fn prepared_redacts_every_text_field() {
        let prepared = bundle(
            lines(&["peer 203.0.113.212", "Connected to Alice’s Mac"]),
            Some(lines(&[r#"{"x":"fe80::1"}"#])),
        )
        .prepared(
            &BTreeMap::from([("Alice’s Mac".into(), "Mac".into())]),
            None,
        );
        assert_eq!(prepared.log, lines(&["peer 203.x.x.x", "Connected to Mac"]));
        assert_eq!(prepared.environment["Status"], "TUN running at 10.x.x.x");
        assert_eq!(prepared.timeline, Some(lines(&[r#"{"x":"fe80::/16"}"#])));
    }

    // testKeepsTheNewestLinesWithinLimits
    #[test]
    fn keeps_the_newest_lines_within_limits() {
        let log: Vec<String> = (0..5000)
            .map(|i| format!("line {i} {}", "z".repeat(400)))
            .collect();
        let prepared = bundle(log.clone(), None).prepared(&none(), None);
        assert!(prepared.log.len() <= MAX_LOG_LINES);
        assert_eq!(prepared.log.last(), log.last());
        assert!(prepared.encoded().len() <= MAX_BYTES);
    }

    // testPreparedAlwaysFitsMaxBytes
    #[test]
    fn prepared_always_fits_max_bytes() {
        let big = "q".repeat(2_000_000);
        let timeline: Vec<String> = (0..100).map(|_| "t".repeat(20_000)).collect();
        let mut b = bundle(lines(&["a"]), Some(timeline));
        b.environment = BTreeMap::from([("Big".into(), big.clone())]);
        b.diagnostics = Some(big.clone());
        b.contact = Some(Contact {
            email: Some(big.clone()),
            note: Some(big),
        });
        let prepared = b.prepared(&none(), None);
        assert!(prepared.encoded().len() <= MAX_BYTES);
        let contact = prepared.contact.unwrap();
        assert_eq!(contact.email.unwrap().chars().count(), MAX_EMAIL);
        assert_eq!(contact.note.unwrap().chars().count(), MAX_NOTE);
    }

    // testRedactsEnvironmentKeysAndDeviceFields
    #[test]
    fn redacts_environment_keys_and_device_fields() {
        let mut b = bundle(vec![], None);
        b.environment = BTreeMap::from([
            ("Wi-Fi (alice’s iPhone)".into(), "on".into()),
            ("Wi-Fi (Alice’s iPhone)".into(), "off".into()),
        ]);
        b.device = "alice’s iPhone".into();
        let prepared = b.prepared(
            &BTreeMap::from([("alice’s iPhone".into(), "iPhone".into())]),
            None,
        );
        assert_eq!(prepared.device, "iPhone");
        assert_eq!(prepared.environment.len(), 2);
        assert!(prepared.environment.keys().all(|k| !k.contains("alice")));
        assert_eq!(prepared.environment["Wi-Fi (iPhone)"], "off");
    }

    // testEncodesFormatAndPlatform
    #[test]
    fn encodes_format_and_platform() {
        let json: serde_json::Value =
            serde_json::from_slice(&bundle(vec![], None).encoded()).unwrap();
        assert_eq!(json["format"], 1);
        assert_eq!(json["platform"], "linux");
        assert_eq!(json["createdAt"], "2026-10-03T04:00:00Z");
        // None is left out, as Swift leaves out nil.
        assert!(json.get("timeline").is_none());
        assert!(json.get("contact").is_none());
        assert!(json.get("sessions").is_none());
    }

    #[test]
    fn timestamps_are_whole_seconds() {
        let nanos = |s| Timestamp::new(s, 987_654_321).unwrap();
        let mut b = bundle(vec![], None);
        b.created_at = nanos(1_791_000_000);
        b.from = nanos(1_790_913_600);
        b.to = nanos(1_791_000_000);
        let json: serde_json::Value =
            serde_json::from_slice(&b.prepared(&none(), None).encoded()).unwrap();
        assert_eq!(json["createdAt"], "2026-10-03T04:00:00Z");
        assert_eq!(json["from"], "2026-10-02T04:00:00Z");
        assert_eq!(json["to"], "2026-10-03T04:00:00Z");
        let made = DiagnosticsBundle::new(
            "linux",
            nanos(1_791_000_000),
            "a".into(),
            "b".into(),
            "c".into(),
            nanos(1),
            nanos(2),
            BTreeMap::new(),
            None,
            None,
            vec![],
        );
        assert_eq!(made.created_at, ts(1_791_000_000));
        assert_eq!(made.to, ts(2));
    }

    #[test]
    fn encoding_sorts_keys() {
        let text = String::from_utf8(bundle(lines(&["x"]), None).encoded()).unwrap();
        let keys = [
            "app",
            "createdAt",
            "device",
            "environment",
            "format",
            "from",
            "log",
            "os",
            "platform",
            "to",
        ];
        let positions: Vec<usize> = keys
            .iter()
            .map(|k| text.find(&format!("\n  \"{k}\"")).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
    }

    #[test]
    fn contact_keeps_the_users_own_words() {
        let mut b = bundle(vec![], None);
        b.contact = Some(Contact {
            email: Some("alice@example.com".into()),
            note: Some("my IP is 203.0.113.212".into()),
        });
        let prepared = b.prepared(&none(), None);
        assert_eq!(prepared.contact, b.contact);
    }

    // The Mac's testSessionsFromJournalLinesSurviveRedaction, over the timeline instead of log lines.
    #[test]
    fn sessions_from_the_timeline() {
        let a = "9F1C2A44-0B7E-4C61-9D2A-3E5F6A7B8C9D";
        let b = "1A2B3C4D-5E6F-4A1B-8C2D-3E4F5A6B7C8D";
        let event = |t, kind, detail: Option<&str>| {
            let mut e = HealthEvent::new(ts(t), kind);
            e.detail = detail.map(str::to_string);
            e
        };
        let timeline = vec![
            event(1, EventKind::TunnelStarted, Some(&format!("session {a}"))),
            event(2, EventKind::TunnelStopped, None),
            event(3, EventKind::TunnelStarted, Some(&format!("session {b}"))),
            event(4, EventKind::TunnelStarted, Some(&format!("session {a}"))),
            event(5, EventKind::TunnelStarted, Some("resumed")),
            event(6, EventKind::UsbAttached, Some(&format!("session {a}"))),
            event(7, EventKind::TunnelStarted, Some("session not-a-uuid")),
        ];
        let found = sessions(&timeline);
        assert_eq!(found, vec![a.to_string(), b.to_string()]);
        let mut bundle = bundle(vec![], None);
        bundle.sessions = Some(found.clone());
        assert_eq!(bundle.prepared(&none(), None).sessions, Some(found));
    }
}
