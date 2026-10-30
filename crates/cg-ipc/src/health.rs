//! The connection timeline: health of each leg, the issues the judge names, and the events
//! written one JSON line each to the seven-day log. Field and value names follow the Mac's
//! `HealthModel.swift` (camelCase) so both platforms' records read the same.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The four legs of a shared connection, as the timeline names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Segment {
    /// USB to the phone's proxy (SOCKS greeting).
    Channel,
    /// The phone's own way out (its DNS and HTTPS checks).
    PhoneExit,
    /// The computer's traffic through the TUN device, hev and the phone.
    Tunnel,
    /// The phone's upstream connects for the computer (failure rate).
    Connections,
}

impl Segment {
    pub const ALL: [Segment; 4] = [
        Segment::Channel,
        Segment::PhoneExit,
        Segment::Tunnel,
        Segment::Connections,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentState {
    Ok,
    Failed,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueCause {
    Channel,
    PhoneNetwork,
    /// The Mac's `macRelay`: the computer's own relay (TUN, hev) while the phone is fine.
    ComputerRelay,
    PhoneUnstable,
    PartialFailures,
}

#[cfg(test)]
impl IssueCause {
    /// Every variant; adding one fails to compile until the match below names it.
    pub(crate) fn all() -> Vec<IssueCause> {
        let all = vec![
            IssueCause::Channel,
            IssueCause::PhoneNetwork,
            IssueCause::ComputerRelay,
            IssueCause::PhoneUnstable,
            IssueCause::PartialFailures,
        ];
        for cause in &all {
            match cause {
                IssueCause::Channel
                | IssueCause::PhoneNetwork
                | IssueCause::ComputerRelay
                | IssueCause::PhoneUnstable
                | IssueCause::PartialFailures => {}
            }
        }
        all
    }
}

impl IssueCause {
    /// Higher is worse; an open issue only moves up.
    pub fn rank(self) -> u8 {
        match self {
            IssueCause::Channel => 5,
            IssueCause::PhoneNetwork => 4,
            IssueCause::ComputerRelay => 3,
            IssueCause::PhoneUnstable => 2,
            IssueCause::PartialFailures => 1,
        }
    }

    /// Shown in red; the others in orange.
    pub fn is_severe(self) -> bool {
        matches!(
            self,
            IssueCause::Channel | IssueCause::PhoneNetwork | IssueCause::ComputerRelay
        )
    }
}

/// What the legs looked like when an issue started. No addresses or device names.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HealthSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<SegmentState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_exit: Option<SegmentState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<SegmentState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections: Option<SegmentState>,
    /// Milliseconds.
    #[serde(rename = "channelRTT", skip_serializing_if = "Option::is_none")]
    pub channel_rtt: Option<i64>,
    /// Milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_latency: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_loss: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connects_failed: Option<BTreeMap<String, i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thermal: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Issue,
    TunnelStarted,
    TunnelStopped,
    UsbAttached,
    UsbDetached,
    VersionChanged,
    ServiceStarted,
    BypassUnavailable,
    BypassUpdateFailed,
    DnsNotAuthorized,
    /// Windows: strict DNS couldn't be turned on for this connection.
    StrictDnsUnavailable,
}

#[cfg(test)]
impl EventKind {
    /// Every variant; adding one fails to compile until the match below names it.
    pub(crate) fn all() -> Vec<EventKind> {
        let all = vec![
            EventKind::Issue,
            EventKind::TunnelStarted,
            EventKind::TunnelStopped,
            EventKind::UsbAttached,
            EventKind::UsbDetached,
            EventKind::VersionChanged,
            EventKind::ServiceStarted,
            EventKind::BypassUnavailable,
            EventKind::BypassUpdateFailed,
            EventKind::DnsNotAuthorized,
            EventKind::StrictDnsUnavailable,
        ];
        for kind in &all {
            match kind {
                EventKind::Issue
                | EventKind::TunnelStarted
                | EventKind::TunnelStopped
                | EventKind::UsbAttached
                | EventKind::UsbDetached
                | EventKind::VersionChanged
                | EventKind::ServiceStarted
                | EventKind::BypassUnavailable
                | EventKind::BypassUpdateFailed
                | EventKind::DnsNotAuthorized
                | EventKind::StrictDnsUnavailable => {}
            }
        }
        all
    }
}

/// `Escalate`: the open issue's cause got worse; the issue and its start stay the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventPhase {
    Start,
    Escalate,
    End,
}

/// One line of the timeline. `t` is whole seconds (the writer rounds it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthEvent {
    pub t: Timestamp,
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<IssueCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<EventPhase>,
    /// Seconds, on an issue's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<HealthSnapshot>,
    /// Short free text without addresses, e.g. "1.0.0 → 1.0.1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl HealthEvent {
    /// An event with only a time and a kind.
    pub fn new(t: Timestamp, kind: EventKind) -> Self {
        HealthEvent {
            t,
            kind,
            cause: None,
            phase: None,
            duration: None,
            snapshot: None,
            detail: None,
        }
    }
}

/// The open issue, if any, and the last one that ended within 24 hours.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HealthStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<IssueCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_issue: Option<LastIssue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastIssue {
    pub cause: IssueCause,
    pub ended: Timestamp,
    /// Seconds; `None` when the end line carried no duration (never a made-up 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
}

/// What was still going on when a day began.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DayStart {
    pub sharing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueCause>,
}

/// One day of the timeline as `history` returns it. `day` and `days` are `YYYY-MM-DD` in
/// the service's zone; `days` are the seven that can be viewed, oldest first. `begin` is
/// the day's local midnight and `end` the earlier of the next midnight and now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryDay {
    pub day: String,
    pub start: DayStart,
    pub events: Vec<HealthEvent>,
    pub days: Vec<String>,
    pub begin: Timestamp,
    pub end: Timestamp,
}

/// Replays the events before a day (oldest first) into the state at its start,
/// as the Mac's `HistoryPage.reload()` does.
pub fn day_start(before: &[HealthEvent]) -> DayStart {
    let mut start = DayStart::default();
    for e in before {
        match (e.kind, e.phase) {
            (EventKind::TunnelStarted, _) => start.sharing = true,
            (EventKind::TunnelStopped, _) => {
                start.sharing = false;
                start.issue = None;
            }
            (EventKind::Issue, Some(EventPhase::Start | EventPhase::Escalate)) => {
                start.issue = e.cause
            }
            (EventKind::Issue, Some(EventPhase::End)) => start.issue = None,
            _ => {}
        }
    }
    start
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Sharing,
    Issue(IssueCause),
}

/// One painted stretch of the 24-hour strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: Timestamp,
    pub to: Timestamp,
    pub kind: SpanKind,
}

/// The day strip's spans, as the Mac's `DayStrip` paints them: every sharing span first,
/// then the issue spans in order, so issues are drawn over sharing. The part of an issue
/// before an escalation keeps the earlier cause; an end without a start in `events`
/// starts at `t - duration`. Anything still open runs to `end`.
pub fn day_spans(
    events: &[HealthEvent],
    start: DayStart,
    day_begin: Timestamp,
    end: Timestamp,
) -> Vec<Span> {
    let mut sharing_since = start.sharing.then_some(day_begin);
    let mut issue = start.issue.map(|c| (day_begin, c));
    let mut sharing = Vec::new();
    let mut issues = Vec::new();
    for e in events {
        match (e.kind, e.phase) {
            (EventKind::TunnelStarted, _) => sharing_since = Some(e.t),
            (EventKind::TunnelStopped, _) => {
                if let Some(from) = sharing_since.take() {
                    sharing.push(Span {
                        from,
                        to: e.t,
                        kind: SpanKind::Sharing,
                    });
                }
            }
            (EventKind::Issue, Some(EventPhase::Start)) => {
                if let Some(c) = e.cause {
                    issue = Some((e.t, c));
                }
            }
            (EventKind::Issue, Some(EventPhase::Escalate)) => {
                if let Some(c) = e.cause {
                    if let Some((from, old)) = issue {
                        issues.push(Span {
                            from,
                            to: e.t,
                            kind: SpanKind::Issue(old),
                        });
                    }
                    issue = Some((e.t, c));
                }
            }
            (EventKind::Issue, Some(EventPhase::End)) => {
                if let Some((from, c)) = issue.take() {
                    issues.push(Span {
                        from,
                        to: e.t,
                        kind: SpanKind::Issue(c),
                    });
                } else if let (Some(d), Some(c)) = (e.duration, e.cause) {
                    let from =
                        e.t.checked_sub(jiff::SignedDuration::from_secs(d))
                            .unwrap_or(e.t);
                    issues.push(Span {
                        from,
                        to: e.t,
                        kind: SpanKind::Issue(c),
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(from) = sharing_since {
        sharing.push(Span {
            from,
            to: end,
            kind: SpanKind::Sharing,
        });
    }
    if let Some((from, c)) = issue {
        issues.push(Span {
            from,
            to: end,
            kind: SpanKind::Issue(c),
        });
    }
    sharing.extend(issues);
    sharing
}

/// The day's local midnight and the next local midnight: 23 or 25 hours apart on a
/// daylight-saving day.
pub fn day_bounds(day: jiff::civil::Date, tz: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    let begin = day.to_zoned(tz.clone()).ok()?.timestamp();
    let next = day.tomorrow().ok()?.to_zoned(tz.clone()).ok()?.timestamp();
    Some((begin, next))
}

/// How far `t` is through `[begin, next)`, from 0 to 1.
pub fn strip_fraction(t: Timestamp, begin: Timestamp, next: Timestamp) -> f64 {
    let total = next.duration_since(begin).as_secs_f64();
    if total <= 0.0 {
        return 0.0;
    }
    (t.duration_since(begin).as_secs_f64() / total).clamp(0.0, 1.0)
}

/// Local 0, 6, 12, 18 and 24 o'clock of the day with their place on the strip.
pub fn hour_marks(day: jiff::civil::Date, tz: &TimeZone) -> Vec<(u8, f64)> {
    let Some((begin, next)) = day_bounds(day, tz) else {
        return [0u8, 6, 12, 18, 24]
            .into_iter()
            .map(|h| (h, f64::from(h) / 24.0))
            .collect();
    };
    [0u8, 6, 12, 18, 24]
        .into_iter()
        .map(|hour| {
            let at = match hour {
                0 => begin,
                24 => next,
                h => day
                    .at(h as i8, 0, 0, 0)
                    .to_zoned(tz.clone())
                    .map(|z| z.timestamp())
                    .unwrap_or(begin),
            };
            (hour, strip_fraction(at, begin, next))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-16 at `h:m` UTC.
    fn hm(h: i64, m: i64) -> Timestamp {
        Timestamp::from_second(1_792_108_800 + h * 3600 + m * 60).unwrap()
    }

    fn issue(t: Timestamp, cause: IssueCause, phase: EventPhase) -> HealthEvent {
        HealthEvent {
            cause: Some(cause),
            phase: Some(phase),
            ..HealthEvent::new(t, EventKind::Issue)
        }
    }

    #[test]
    fn dst_days_are_23_and_25_hours_and_noon_moves() {
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let spring = jiff::civil::date(2026, 3, 8);
        let (b, n) = day_bounds(spring, &la).unwrap();
        assert_eq!(n.duration_since(b).as_hours(), 23);
        let marks = hour_marks(spring, &la);
        assert_eq!(
            marks.iter().map(|m| m.0).collect::<Vec<_>>(),
            [0, 6, 12, 18, 24]
        );
        assert!((marks[2].1 - 11.0 / 23.0).abs() < 1e-9);
        assert!((marks[1].1 - 5.0 / 23.0).abs() < 1e-9);
        assert_eq!((marks[0].1, marks[4].1), (0.0, 1.0));
        let fall = jiff::civil::date(2026, 11, 1);
        let (b, n) = day_bounds(fall, &la).unwrap();
        assert_eq!(n.duration_since(b).as_hours(), 25);
        assert!((hour_marks(fall, &la)[2].1 - 13.0 / 25.0).abs() < 1e-9);
        assert_eq!(strip_fraction(n, b, n), 1.0);
        let before = b.checked_sub(jiff::SignedDuration::from_hours(1)).unwrap();
        assert_eq!(strip_fraction(before, b, n), 0.0);
        let plain = day_bounds(jiff::civil::date(2026, 10, 19), &la).unwrap();
        assert_eq!(plain.1.duration_since(plain.0).as_hours(), 24);
    }

    #[test]
    fn day_begins_at_midnight_utc() {
        assert_eq!(hm(0, 0).to_string(), "2026-10-16T00:00:00Z");
    }

    #[test]
    fn day_spans_paint_sharing_then_issues() {
        let events = [
            HealthEvent::new(hm(9, 0), EventKind::TunnelStarted),
            issue(hm(10, 0), IssueCause::PhoneUnstable, EventPhase::Start),
            issue(hm(10, 5), IssueCause::Channel, EventPhase::Escalate),
            issue(hm(10, 10), IssueCause::Channel, EventPhase::End),
            HealthEvent::new(hm(12, 0), EventKind::TunnelStopped),
        ];
        let spans = day_spans(&events, DayStart::default(), hm(0, 0), hm(18, 0));
        assert_eq!(
            spans,
            vec![
                Span {
                    from: hm(9, 0),
                    to: hm(12, 0),
                    kind: SpanKind::Sharing
                },
                Span {
                    from: hm(10, 0),
                    to: hm(10, 5),
                    kind: SpanKind::Issue(IssueCause::PhoneUnstable)
                },
                Span {
                    from: hm(10, 5),
                    to: hm(10, 10),
                    kind: SpanKind::Issue(IssueCause::Channel)
                },
            ]
        );
    }

    #[test]
    fn open_issue_and_sharing_run_to_end() {
        let start = DayStart {
            sharing: true,
            issue: Some(IssueCause::Channel),
        };
        let spans = day_spans(&[], start, hm(0, 0), hm(15, 30));
        assert_eq!(
            spans,
            vec![
                Span {
                    from: hm(0, 0),
                    to: hm(15, 30),
                    kind: SpanKind::Sharing
                },
                Span {
                    from: hm(0, 0),
                    to: hm(15, 30),
                    kind: SpanKind::Issue(IssueCause::Channel)
                },
            ]
        );
    }

    #[test]
    fn end_without_start_uses_duration() {
        let end = HealthEvent {
            duration: Some(90),
            ..issue(hm(8, 0), IssueCause::ComputerRelay, EventPhase::End)
        };
        // An end with neither a start nor a duration paints nothing.
        let bare = issue(hm(9, 0), IssueCause::Channel, EventPhase::End);
        let spans = day_spans(&[end, bare], DayStart::default(), hm(0, 0), hm(12, 0));
        assert_eq!(
            spans,
            vec![Span {
                from: hm(7, 58)
                    .checked_add(jiff::SignedDuration::from_secs(30))
                    .unwrap(),
                to: hm(8, 0),
                kind: SpanKind::Issue(IssueCause::ComputerRelay)
            }]
        );
    }

    #[test]
    fn day_start_replays_tunnel_and_issue_events() {
        let t = hm(1, 0);
        assert_eq!(day_start(&[]), DayStart::default());
        let open = [
            HealthEvent::new(t, EventKind::TunnelStarted),
            issue(t, IssueCause::PhoneUnstable, EventPhase::Start),
            issue(t, IssueCause::PhoneNetwork, EventPhase::Escalate),
            HealthEvent::new(t, EventKind::UsbDetached),
        ];
        assert_eq!(
            day_start(&open),
            DayStart {
                sharing: true,
                issue: Some(IssueCause::PhoneNetwork)
            }
        );
        let mut ended = open.to_vec();
        ended.push(issue(t, IssueCause::PhoneNetwork, EventPhase::End));
        assert_eq!(
            day_start(&ended),
            DayStart {
                sharing: true,
                issue: None
            }
        );
        let mut stopped = open.to_vec();
        stopped.push(HealthEvent::new(t, EventKind::TunnelStopped));
        assert_eq!(day_start(&stopped), DayStart::default());
    }

    #[test]
    fn causes_rank_and_severity() {
        use IssueCause::*;
        let all = [
            Channel,
            PhoneNetwork,
            ComputerRelay,
            PhoneUnstable,
            PartialFailures,
        ];
        assert_eq!(all.map(IssueCause::rank), [5, 4, 3, 2, 1]);
        assert_eq!(
            all.map(IssueCause::is_severe),
            [true, true, true, false, false]
        );
    }

    /// Ruling P22: an unknown duration is left out, never written as 0.
    #[test]
    fn last_issue_without_duration_leaves_it_out() {
        let unknown = LastIssue {
            cause: IssueCause::Channel,
            ended: hm(8, 0),
            duration: None,
        };
        let json = serde_json::to_string(&unknown).unwrap();
        assert_eq!(
            json,
            r#"{"cause":"channel","ended":"2026-10-16T08:00:00Z"}"#
        );
        assert_eq!(serde_json::from_str::<LastIssue>(&json).unwrap(), unknown);
        let known = LastIssue {
            duration: Some(42),
            ..unknown
        };
        assert!(
            serde_json::to_string(&known)
                .unwrap()
                .ends_with(r#""duration":42}"#)
        );
    }

    #[test]
    fn linux_kinds_and_causes_are_camel_case() {
        let names: Vec<String> = [
            EventKind::ServiceStarted,
            EventKind::BypassUnavailable,
            EventKind::BypassUpdateFailed,
            EventKind::DnsNotAuthorized,
            EventKind::StrictDnsUnavailable,
            EventKind::UsbAttached,
        ]
        .iter()
        .map(|k| serde_json::to_string(k).unwrap())
        .collect();
        assert_eq!(
            names,
            [
                r#""serviceStarted""#,
                r#""bypassUnavailable""#,
                r#""bypassUpdateFailed""#,
                r#""dnsNotAuthorized""#,
                r#""strictDnsUnavailable""#,
                r#""usbAttached""#
            ]
        );
        assert_eq!(
            serde_json::to_string(&IssueCause::ComputerRelay).unwrap(),
            r#""computerRelay""#
        );
        let snap = HealthSnapshot {
            phone_exit: Some(SegmentState::Failed),
            computer_version: Some("1.0.0".into()),
            connects_failed: Some(BTreeMap::from([("timeout".into(), 3)])),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&snap).unwrap(),
            r#"{"phoneExit":"failed","connectsFailed":{"timeout":3},"computerVersion":"1.0.0"}"#
        );
    }
}
