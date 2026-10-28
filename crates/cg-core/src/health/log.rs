//! The timeline on disk, ported from the Mac's `HealthLog.swift`: one JSON line per event,
//! one file per local day (`YYYY-MM-DD.jsonl` in the service's zone), kept seven days.
//! Append-only; a line that doesn't decode is skipped.
//!
//! The zone can change between runs (a laptop that travels), so a file's name is only a
//! hint: every read looks at the files one day either side and filters by time.

use cg_ipc::health::{DayStart, EventKind, EventPhase, HealthEvent, LastIssue, day_start};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;

/// Days kept, today included.
const RETENTION_DAYS: i64 = 7;

#[derive(Clone, Debug)]
pub struct HealthLog {
    dir: PathBuf,
    tz: TimeZone,
}

impl HealthLog {
    pub fn new(dir: PathBuf, tz: TimeZone) -> Self {
        HealthLog { dir, tz }
    }

    /// Writes one line to the file for the event's local day. `t` is cut to whole seconds
    /// first: the Mac's ISO 8601 reader can't take fractions.
    pub fn append(&self, e: &HealthEvent) -> io::Result<()> {
        let mut e = e.clone();
        if let Ok(whole) = Timestamp::from_second(e.t.as_second()) {
            e.t = whole;
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut line = serde_json::to_vec(&e)?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o640);
        }
        let mut file = options.open(self.file(self.day_of(e.t)))?;
        file.write_all(&line)
    }

    /// The events whose time falls on `day` in this log's zone, oldest first.
    pub fn events_on(&self, day: Date) -> Vec<HealthEvent> {
        let (Some(start), Some(end)) = (
            self.midnight(day),
            day.tomorrow().ok().and_then(|d| self.midnight(d)),
        ) else {
            return Vec::new();
        };
        self.read_range(start, end, false)
    }

    /// The events from `start` to `end`, both included.
    pub fn events_between(&self, start: Timestamp, end: Timestamp) -> Vec<HealthEvent> {
        self.read_range(start, end, true)
    }

    /// Removes day files older than the seven days kept.
    pub fn prune(&self, now: Timestamp) {
        let Ok(cutoff) = self.day_of(now).checked_sub((RETENTION_DAYS - 1).days()) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".jsonl")) else {
                continue;
            };
            if let Ok(day) = stem.parse::<Date>()
                && day < cutoff
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// The local date of `t` in this log's zone.
    pub fn day_of(&self, t: Timestamp) -> Date {
        t.to_zoned(self.tz.clone()).date()
    }

    /// The seven days kept, today included, oldest first.
    pub fn days(&self, now: Timestamp) -> Vec<Date> {
        let today = self.day_of(now);
        (0..RETENTION_DAYS)
            .rev()
            .filter_map(|back| today.checked_sub(back.days()).ok())
            .collect()
    }

    /// The last issue that ended within 24 hours of `now`.
    pub fn recent_issue(&self, now: Timestamp) -> Option<LastIssue> {
        let since = now.checked_sub(SignedDuration::from_hours(24)).ok()?;
        let e = self
            .events_between(since, now)
            .into_iter()
            .rev()
            .find(|e| e.kind == EventKind::Issue && e.phase == Some(EventPhase::End))?;
        Some(LastIssue {
            cause: e.cause?,
            ended: e.t,
            duration: e.duration,
        })
    }

    /// What was still going on when `day` began, from the seven days before it.
    pub fn start_of(&self, day: Date) -> DayStart {
        let begin = self.midnight(day);
        let first = day
            .checked_sub(RETENTION_DAYS.days())
            .ok()
            .and_then(|d| self.midnight(d));
        match (first, begin) {
            (Some(first), Some(begin)) => day_start(&self.read_range(first, begin, false)),
            _ => DayStart::default(),
        }
    }

    /// The first instant of `day` in this log's zone.
    fn midnight(&self, day: Date) -> Option<Timestamp> {
        day.to_zoned(self.tz.clone()).ok().map(|z| z.timestamp())
    }

    fn file(&self, day: Date) -> PathBuf {
        self.dir.join(format!("{}.jsonl", day.strftime("%Y-%m-%d")))
    }

    /// Events from `start` up to `end` (included when `inclusive`), oldest first. Files are
    /// named in the zone in force when written, which may differ from this one, so read
    /// one day either side; the time filter keeps the result exact.
    fn read_range(&self, start: Timestamp, end: Timestamp, inclusive: bool) -> Vec<HealthEvent> {
        let end = end.max(start);
        let (Ok(first), Ok(last)) = (self.day_of(start).yesterday(), self.day_of(end).tomorrow())
        else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for day in first.series(1.day()).take_while(|d| *d <= last) {
            events.extend(
                self.read_file(day)
                    .into_iter()
                    .filter(|e| e.t >= start && (e.t < end || (inclusive && e.t == end))),
            );
        }
        // Files from different zones can interleave; keep the timeline in order.
        events.sort_by_key(|e| e.t);
        events
    }

    fn read_file(&self, day: Date) -> Vec<HealthEvent> {
        let Ok(text) = std::fs::read(self.file(day)) else {
            return Vec::new();
        };
        text.split(|&b| b == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //! `HealthLogTests.swift`, case by case, plus the time-zone and day-start cases
    //! (Review Focus 2) and the on-disk line format.

    use super::*;
    use cg_ipc::health::{
        DayStart, EventKind, EventPhase, HealthEvent, HealthSnapshot, IssueCause, LastIssue,
        SegmentState,
    };
    use jiff::Timestamp;
    use jiff::civil::{Date, date};
    use jiff::tz::TimeZone;

    fn la() -> TimeZone {
        TimeZone::get("America/Los_Angeles").unwrap()
    }

    fn tokyo() -> TimeZone {
        TimeZone::get("Asia/Tokyo").unwrap()
    }

    /// 2026-10-`d` at `h:m` in `tz`.
    fn when(tz: &TimeZone, d: i8, h: i8, m: i8) -> Timestamp {
        date(2026, 10, d)
            .at(h, m, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
    }

    /// 2026-10-`d` at `h`:00 in Los Angeles.
    fn day(d: i8, h: i8) -> Timestamp {
        when(&la(), d, h, 0)
    }

    fn event(t: Timestamp, kind: EventKind) -> HealthEvent {
        HealthEvent::new(t, kind)
    }

    fn issue(
        t: Timestamp,
        cause: IssueCause,
        phase: EventPhase,
        duration: Option<i64>,
    ) -> HealthEvent {
        HealthEvent {
            cause: Some(cause),
            phase: Some(phase),
            duration,
            ..HealthEvent::new(t, EventKind::Issue)
        }
    }

    fn kinds(events: &[HealthEvent]) -> Vec<EventKind> {
        events.iter().map(|e| e.kind).collect()
    }

    fn files(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn appends_and_reads_by_day() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().join("health"), la());
        log.append(&event(day(7, 9), EventKind::TunnelStarted))
            .unwrap();
        log.append(&issue(
            day(7, 21),
            IssueCause::ComputerRelay,
            EventPhase::Start,
            None,
        ))
        .unwrap();
        log.append(&event(day(8, 1), EventKind::TunnelStopped))
            .unwrap();
        assert_eq!(
            kinds(&log.events_on(date(2026, 10, 7))),
            [EventKind::TunnelStarted, EventKind::Issue]
        );
        assert_eq!(
            kinds(&log.events_between(day(7, 20), day(8, 23))),
            [EventKind::Issue, EventKind::TunnelStopped]
        );
        assert_eq!(
            files(&dir.path().join("health")),
            ["2026-10-07.jsonl", "2026-10-08.jsonl"]
        );
    }

    /// Review Focus 2: a write at 23:59 read after midnight, and the zone changing between runs.
    #[test]
    fn reads_events_written_in_another_time_zone() {
        let dir = tempfile::tempdir().unwrap();
        let la_log = HealthLog::new(dir.path().to_path_buf(), la());
        la_log
            .append(&event(day(7, 23), EventKind::TunnelStarted))
            .unwrap();
        // 23:00 in Los Angeles on 10-07 is already 10-08 in Tokyo.
        let tokyo_log = HealthLog::new(dir.path().to_path_buf(), tokyo());
        let start = when(&tokyo(), 8, 0, 0);
        let end = when(&tokyo(), 8, 23, 0);
        assert_eq!(
            kinds(&tokyo_log.events_between(start, end)),
            [EventKind::TunnelStarted]
        );

        // Written at 23:59, read after midnight: the entry stays on the day it was written.
        let late = when(&la(), 7, 23, 59);
        la_log
            .append(&event(late, EventKind::TunnelStopped))
            .unwrap();
        let after_midnight = when(&la(), 8, 0, 1);
        assert_eq!(la_log.day_of(after_midnight), date(2026, 10, 8));
        assert_eq!(
            kinds(&la_log.events_on(date(2026, 10, 7))),
            [EventKind::TunnelStarted, EventKind::TunnelStopped]
        );
        assert_eq!(la_log.events_on(date(2026, 10, 8)), vec![]);
        assert_eq!(la_log.days(after_midnight).last(), Some(&date(2026, 10, 8)));

        // The laptop flew to Tokyo: both entries are on Tokyo's 10-08, none on its 10-07,
        // although they sit in Los Angeles's 2026-10-07.jsonl.
        assert_eq!(
            kinds(&tokyo_log.events_on(date(2026, 10, 8))),
            [EventKind::TunnelStarted, EventKind::TunnelStopped]
        );
        assert_eq!(tokyo_log.events_on(date(2026, 10, 7)), vec![]);
        // New writes go to Tokyo's date, and the Los Angeles reader still places them right.
        let tokyo_morning = when(&tokyo(), 9, 8, 0);
        tokyo_log
            .append(&event(tokyo_morning, EventKind::UsbAttached))
            .unwrap();
        assert_eq!(files(dir.path()), ["2026-10-07.jsonl", "2026-10-09.jsonl"]);
        assert_eq!(
            kinds(&la_log.events_on(date(2026, 10, 8))),
            [EventKind::UsbAttached]
        );
    }

    #[test]
    fn skips_broken_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        log.append(&event(day(7, 9), EventKind::TunnelStarted))
            .unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("2026-10-07.jsonl"))
            .unwrap();
        f.write_all(b"{\"t\":\"broken\n").unwrap();
        log.append(&event(day(7, 10), EventKind::TunnelStopped))
            .unwrap();
        assert_eq!(
            kinds(&log.events_on(date(2026, 10, 7))),
            [EventKind::TunnelStarted, EventKind::TunnelStopped]
        );
    }

    #[test]
    fn prunes_files_older_than_seven_days() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        for d in [1, 2, 8] {
            log.append(&event(day(d, 12), EventKind::TunnelStarted))
                .unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "keep").unwrap();
        log.prune(day(8, 12));
        assert_eq!(
            files(dir.path()),
            ["2026-10-02.jsonl", "2026-10-08.jsonl", "notes.txt"]
        );
    }

    /// Review Focus 2: the state at midnight comes from earlier days' files, even when the
    /// day before has none, and follows the reader's zone.
    #[test]
    fn day_start_replays_previous_days() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        // 10-06 writes tunnelStarted and issue/start(phoneNetwork); 10-07 writes nothing.
        log.append(&event(day(6, 10), EventKind::TunnelStarted))
            .unwrap();
        log.append(&issue(
            day(6, 11),
            IssueCause::PhoneNetwork,
            EventPhase::Start,
            None,
        ))
        .unwrap();
        assert_eq!(
            log.start_of(date(2026, 10, 8)),
            DayStart {
                sharing: true,
                issue: Some(IssueCause::PhoneNetwork)
            }
        );
        // Only the seven days before count: 10-06 is in 10-13's window but not 10-14's.
        assert_eq!(
            log.start_of(date(2026, 10, 13)),
            DayStart {
                sharing: true,
                issue: Some(IssueCause::PhoneNetwork)
            }
        );
        assert_eq!(log.start_of(date(2026, 10, 14)), DayStart::default());
        // Events on the day itself don't count towards its start.
        log.append(&event(day(8, 0), EventKind::TunnelStopped))
            .unwrap();
        assert_eq!(
            log.start_of(date(2026, 10, 8)),
            DayStart {
                sharing: true,
                issue: Some(IssueCause::PhoneNetwork)
            }
        );
        // After a tunnelStopped written at 10-07 23:00.
        log.append(&event(day(7, 23), EventKind::TunnelStopped))
            .unwrap();
        assert_eq!(
            log.start_of(date(2026, 10, 8)),
            DayStart {
                sharing: false,
                issue: None
            }
        );
        // Read in Tokyo, 10-07 23:00 in Los Angeles falls on Tokyo's 10-08, so Tokyo's
        // midnight still saw sharing and the open issue.
        let tokyo_log = HealthLog::new(dir.path().to_path_buf(), tokyo());
        assert_eq!(
            tokyo_log.start_of(date(2026, 10, 8)),
            DayStart {
                sharing: true,
                issue: Some(IssueCause::PhoneNetwork)
            }
        );
        assert_eq!(
            tokyo_log.start_of(date(2026, 10, 9)),
            DayStart {
                sharing: false,
                issue: None
            }
        );
    }

    #[test]
    fn recent_issue_is_the_last_end_within_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        assert_eq!(log.recent_issue(day(8, 12)), None);
        log.append(&issue(
            day(7, 9),
            IssueCause::Channel,
            EventPhase::End,
            Some(30),
        ))
        .unwrap();
        log.append(&issue(
            day(7, 20),
            IssueCause::PhoneUnstable,
            EventPhase::Start,
            None,
        ))
        .unwrap();
        log.append(&issue(
            day(7, 21),
            IssueCause::PhoneUnstable,
            EventPhase::End,
            Some(3600),
        ))
        .unwrap();
        log.append(&event(day(7, 22), EventKind::TunnelStopped))
            .unwrap();
        assert_eq!(
            log.recent_issue(day(8, 12)),
            Some(LastIssue {
                cause: IssueCause::PhoneUnstable,
                ended: day(7, 21),
                duration: Some(3600)
            })
        );
        // More than 24 hours later it is no longer recent.
        assert_eq!(log.recent_issue(day(8, 22)), None);
    }

    /// Ruling P22: an end line without a duration reports none rather than 0.
    #[test]
    fn recent_issue_without_duration_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        log.append(&issue(
            day(7, 9),
            IssueCause::Channel,
            EventPhase::End,
            None,
        ))
        .unwrap();
        assert_eq!(
            log.recent_issue(day(7, 10)),
            Some(LastIssue {
                cause: IssueCause::Channel,
                ended: day(7, 9),
                duration: None
            })
        );
    }

    /// Ruling P23: `append` writes whole seconds whatever the caller passes.
    #[test]
    fn append_cuts_time_to_whole_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), la());
        let t = Timestamp::from_second(1_791_000_000)
            .unwrap()
            .checked_add(SignedDuration::from_millis(1_750))
            .unwrap();
        log.append(&event(t, EventKind::UsbAttached)).unwrap();
        let text = std::fs::read_to_string(dir.path().join("2026-10-02.jsonl")).unwrap();
        assert_eq!(
            text,
            "{\"t\":\"2026-10-03T04:00:01Z\",\"kind\":\"usbAttached\"}\n"
        );
        let read = log.events_on(date(2026, 10, 2));
        assert_eq!(read[0].t, Timestamp::from_second(1_791_000_001).unwrap());
    }

    #[test]
    fn days_are_the_last_seven_oldest_first() {
        let log = HealthLog::new("/nonexistent".into(), la());
        let days: Vec<Date> = (2..=8).map(|d| date(2026, 10, d)).collect();
        // 23:30 in Los Angeles is already the next day in UTC.
        assert_eq!(log.days(when(&la(), 8, 23, 30)), days);
        // Nothing on disk: reads are empty, not errors.
        assert_eq!(log.events_on(date(2026, 10, 8)), vec![]);
        assert_eq!(log.start_of(date(2026, 10, 8)), DayStart::default());
    }

    #[cfg(unix)]
    #[test]
    fn append_creates_the_directory_and_a_0640_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/health");
        let log = HealthLog::new(path.clone(), la());
        log.append(&event(day(7, 9), EventKind::ServiceStarted))
            .unwrap();
        let mode = std::fs::metadata(path.join("2026-10-07.jsonl"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
        let text = std::fs::read_to_string(path.join("2026-10-07.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn event_json_matches_the_mac() {
        let e = HealthEvent {
            t: Timestamp::from_second(1_791_000_000).unwrap(),
            kind: EventKind::Issue,
            cause: Some(IssueCause::ComputerRelay),
            phase: Some(EventPhase::Start),
            duration: None,
            snapshot: Some(HealthSnapshot {
                channel: Some(SegmentState::Ok),
                channel_rtt: Some(4),
                ..Default::default()
            }),
            detail: None,
        };
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(
            line,
            r#"{"t":"2026-10-03T04:00:00Z","kind":"issue","cause":"computerRelay","phase":"start","snapshot":{"channel":"ok","channelRTT":4}}"#
        );
        assert_eq!(serde_json::from_str::<HealthEvent>(&line).unwrap(), e);
    }

    /// A line the Mac wrote (sorted keys, its own kinds) still reads, minus what Linux lacks.
    #[test]
    fn reads_a_mac_line() {
        let line = r#"{"cause":"phoneNetwork","duration":12,"kind":"issue","phase":"end","snapshot":{"channelRTT":3,"helperPID":42,"macVersion":"1.0.0"},"t":"2026-10-03T04:00:00Z"}"#;
        let e: HealthEvent = serde_json::from_str(line).unwrap();
        assert_eq!(e.kind, EventKind::Issue);
        assert_eq!(e.duration, Some(12));
        assert_eq!(e.snapshot.unwrap().channel_rtt, Some(3));
    }
}
