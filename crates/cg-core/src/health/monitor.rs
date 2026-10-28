//! Feeds probe results and phone frames to the judge and writes the timeline, ported from
//! the Mac's `HealthController.swift`. The controller's actor owns it and tells it what
//! happens (service start, tunnel up and down, USB, phone frames, probe results); the
//! probes themselves run as tasks off the actor.

use super::judge::{ChangePhase, HealthJudge, IssueChange};
use super::log::HealthLog;
use super::window::ConnectionWindow;
use crate::controller::HealthWatchTiming;
use cg_ipc::health::{
    EventKind, EventPhase, HealthEvent, HealthSnapshot, HealthStatus, LastIssue, Segment,
    SegmentState,
};
use cg_ipc::phone::PhoneFrame;
use jiff::Timestamp;
use std::time::{Duration, Instant};

pub struct HealthMonitor {
    log: Option<HealthLog>,
    timing: HealthWatchTiming,
    computer_version: String,
    phone_version: Option<String>,
    judge: HealthJudge,
    window: ConnectionWindow,
    running: bool,
    latest: Option<PhoneFrame>,
    latest_at: Option<Instant>,
    last_rtt: Option<Duration>,
    last_issue: Option<LastIssue>,
    /// A write failed and was logged; later failures in the same streak stay at debug.
    write_failing: bool,
}

impl HealthMonitor {
    /// `log` is `None` when nothing is written (tests, or no state directory).
    pub fn new(
        log: Option<HealthLog>,
        timing: HealthWatchTiming,
        computer_version: String,
    ) -> HealthMonitor {
        HealthMonitor {
            log,
            judge: HealthJudge::new(timing.judge),
            timing,
            computer_version,
            phone_version: None,
            window: ConnectionWindow::default(),
            running: false,
            latest: None,
            latest_at: None,
            last_rtt: None,
            last_issue: None,
            write_failing: false,
        }
    }

    pub fn service_started(&mut self, now: Timestamp) {
        if let Some(log) = &self.log {
            log.prune(now);
            self.last_issue = log.recent_issue(now);
        }
        self.write(HealthEvent::new(now, EventKind::ServiceStarted));
    }

    pub fn tunnel_started(&mut self, session: &str, now: Timestamp) {
        if self.running {
            return;
        }
        self.running = true;
        if let Some(log) = &self.log {
            log.prune(now);
        }
        self.judge.start(now);
        self.window.reset();
        self.write(HealthEvent {
            detail: Some(format!("session {session}")),
            ..HealthEvent::new(now, EventKind::TunnelStarted)
        });
    }

    /// `detail` says why, when a check released the session.
    pub fn tunnel_stopped(&mut self, detail: Option<String>, now: Timestamp) {
        if !self.running {
            return;
        }
        self.running = false;
        let changes = self.judge.stop(now);
        self.apply(changes);
        self.write(HealthEvent {
            detail,
            ..HealthEvent::new(now, EventKind::TunnelStopped)
        });
        // The next session's snapshots start from its own readings.
        self.latest = None;
        self.latest_at = None;
        self.last_rtt = None;
    }

    pub fn usb(&mut self, attached: bool, now: Timestamp) {
        let kind = if attached {
            EventKind::UsbAttached
        } else {
            EventKind::UsbDetached
        };
        self.write(HealthEvent::new(now, kind));
    }

    /// A (sanitised) frame from the phone app: its own way out and its connect counters.
    pub fn phone_frame(&mut self, m: &PhoneFrame, now: Timestamp, at: Instant) {
        self.latest = Some(m.clone());
        self.latest_at = Some(at);
        if !self.running {
            return;
        }
        let changes = self.judge.record(Segment::PhoneExit, m.exit_ok, now);
        self.apply(changes);
        let connections = m.connects_ok.and_then(|ok| {
            let failed = m.connects_failed.clone().unwrap_or_default();
            self.window.observe(ok, &failed, now)
        });
        let changes = self.judge.record(Segment::Connections, connections, now);
        self.apply(changes);
    }

    /// The SOCKS greeting through the check forwarder: its round trip, `None` if unanswered.
    pub fn channel(&mut self, rtt: Option<Duration>, now: Timestamp) {
        if !self.running {
            return;
        }
        if rtt.is_some() {
            self.last_rtt = rtt;
        }
        let changes = self
            .judge
            .record(Segment::Channel, Some(rtt.is_some()), now);
        self.apply(changes);
    }

    pub fn tunnel(&mut self, ok: bool, now: Timestamp) {
        if !self.running {
            return;
        }
        let changes = self.judge.record(Segment::Tunnel, Some(ok), now);
        self.apply(changes);
    }

    /// A phone that went quiet (app in the background) can't vouch for its own legs;
    /// without this a failed phone leg would stay failed forever.
    pub fn frames_stale(&mut self, at: Instant, now: Timestamp) {
        if !self.running {
            return;
        }
        let stale = self
            .latest_at
            .is_none_or(|t| at.saturating_duration_since(t) > self.timing.frame_stale);
        if stale {
            let changes = self.judge.record(Segment::PhoneExit, None, now);
            self.apply(changes);
            let changes = self.judge.record(Segment::Connections, None, now);
            self.apply(changes);
        }
    }

    /// The computer went to sleep or woke up (the Mac resets on willSleep and didWake):
    /// whatever the legs showed across the gap says nothing about now, so open issues end,
    /// counts clear and the warmup starts again. The session itself isn't touched.
    pub fn sleep_changed(&mut self, now: Timestamp) {
        let changes = self.judge.reset(now);
        self.apply(changes);
    }

    pub fn note(&mut self, kind: EventKind, detail: Option<String>, now: Timestamp) {
        self.write(HealthEvent {
            detail,
            ..HealthEvent::new(now, kind)
        });
    }

    /// Logs a `versionChanged` when the computer's or the phone's version differs from
    /// `previous` (the pair stored in the settings); returns the pair to store.
    pub fn versions(
        &mut self,
        previous: Option<(String, String)>,
        phone: &str,
        now: Timestamp,
    ) -> (String, String) {
        self.phone_version = Some(phone.to_string());
        let current = (self.computer_version.clone(), phone.to_string());
        if let Some(previous) = previous
            && previous != current
        {
            self.note(
                EventKind::VersionChanged,
                Some(format!(
                    "{} | {} → {} | {}",
                    previous.0, previous.1, current.0, current.1
                )),
                now,
            );
        }
        current
    }

    /// Probe the tunnel again soon: its leg failed or an issue is open.
    pub fn wants_fast_tunnel(&self) -> bool {
        self.judge.state(Segment::Tunnel) == SegmentState::Failed || self.judge.cause().is_some()
    }

    pub fn status(&self) -> HealthStatus {
        HealthStatus {
            cause: self.judge.cause(),
            since: self.judge.issue_started(),
            last_issue: self.last_issue,
        }
    }

    fn apply(&mut self, changes: Vec<IssueChange>) {
        for change in changes {
            let event = HealthEvent {
                cause: Some(change.cause),
                ..HealthEvent::new(change.at, EventKind::Issue)
            };
            match change.phase {
                ChangePhase::Started => self.write(HealthEvent {
                    phase: Some(EventPhase::Start),
                    snapshot: Some(self.snapshot()),
                    ..event
                }),
                ChangePhase::Escalated => self.write(HealthEvent {
                    phase: Some(EventPhase::Escalate),
                    snapshot: Some(self.snapshot()),
                    ..event
                }),
                ChangePhase::Ended => {
                    // From the issue's first start, through any escalation.
                    let millis = change.at.duration_since(change.started).as_millis();
                    let duration = ((millis + 500) / 1000).max(0) as i64;
                    self.write(HealthEvent {
                        phase: Some(EventPhase::End),
                        duration: Some(duration),
                        ..event
                    });
                    if self.judge.cause().is_none() {
                        self.last_issue = Some(LastIssue {
                            cause: change.cause,
                            ended: change.at,
                            duration: Some(duration),
                        });
                    }
                }
            }
        }
    }

    fn snapshot(&self) -> HealthSnapshot {
        let state = |s| Some(self.judge.state(s));
        let failed = self.window.recent_failed();
        HealthSnapshot {
            channel: state(Segment::Channel),
            phone_exit: state(Segment::PhoneExit),
            tunnel: state(Segment::Tunnel),
            connections: state(Segment::Connections),
            channel_rtt: self
                .last_rtt
                .map(|rtt| (rtt.as_secs_f64() * 1000.0).round() as i64),
            phone_latency: self
                .latest
                .as_ref()
                .and_then(|f| f.latency)
                .map(|ms| ms.round() as i64),
            phone_loss: self.latest.as_ref().and_then(|f| f.loss),
            // What failed within the connection window, not the counts since sharing began.
            connects_failed: (!failed.is_empty()).then(|| failed.clone()),
            radio: self.latest.as_ref().and_then(|f| f.radio.clone()),
            thermal: self.latest.as_ref().and_then(|f| f.thermal),
            computer_version: Some(self.computer_version.clone()),
            phone_version: self.phone_version.clone(),
        }
    }

    /// A failed write never stops the connection: it is logged once per streak of failures.
    fn write(&mut self, event: HealthEvent) {
        let Some(log) = &self.log else {
            return;
        };
        match log.append(&event) {
            Ok(()) => self.write_failing = false,
            Err(error) if !self.write_failing => {
                self.write_failing = true;
                tracing::warn!("can't write the connection history: {error}");
            }
            Err(error) => tracing::debug!("can't write the connection history: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::judge::JudgeConfig;
    use cg_ipc::health::IssueCause;
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(1_791_000_000 + s).unwrap()
    }

    fn timing() -> HealthWatchTiming {
        HealthWatchTiming {
            judge: JudgeConfig {
                warmup: Duration::ZERO,
                ..JudgeConfig::default()
            },
            ..HealthWatchTiming::default()
        }
    }

    fn monitor(dir: &std::path::Path) -> HealthMonitor {
        let log = HealthLog::new(dir.to_path_buf(), TimeZone::UTC);
        HealthMonitor::new(Some(log), timing(), "1.0.0".into())
    }

    fn written(dir: &std::path::Path) -> Vec<HealthEvent> {
        HealthLog::new(dir.to_path_buf(), TimeZone::UTC).events_on(date(2026, 10, 3))
    }

    #[test]
    fn versions_log_only_a_change_from_a_known_pair() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = monitor(dir.path());
        // Nothing stored yet: remember the pair, log nothing.
        assert_eq!(
            m.versions(None, "2.2.0", at(0)),
            ("1.0.0".to_string(), "2.2.0".to_string())
        );
        let same = Some(("1.0.0".to_string(), "2.2.0".to_string()));
        m.versions(same, "2.2.0", at(1));
        assert!(written(dir.path()).is_empty());
        let old = Some(("1.0.0".to_string(), "2.1.0".to_string()));
        m.versions(old, "2.2.0", at(2));
        let events = written(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].detail.as_deref(),
            Some("1.0.0 | 2.1.0 → 1.0.0 | 2.2.0")
        );
    }

    #[test]
    fn an_issue_writes_start_with_a_snapshot_and_end_with_its_duration() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = monitor(dir.path());
        m.tunnel_started("abc", at(0));
        let frame = PhoneFrame {
            radio: Some("5G".into()),
            thermal: Some(2),
            latency: Some(40.6),
            ..Default::default()
        };
        m.phone_frame(&frame, at(1), Instant::now());
        m.channel(Some(Duration::from_micros(3_600)), at(1));
        m.tunnel(false, at(2));
        // One failure is the controller's to speed up; the leg isn't failed yet.
        assert!(!m.wants_fast_tunnel());
        m.tunnel(false, at(3));
        assert!(
            m.wants_fast_tunnel(),
            "an open issue asks for a quicker look"
        );
        assert_eq!(m.status().cause, Some(IssueCause::ComputerRelay));
        assert_eq!(m.status().since, Some(at(3)));
        for t in 4..7 {
            m.tunnel(true, at(t));
        }
        let status = m.status();
        assert_eq!(status.cause, None);
        assert_eq!(
            status.last_issue,
            Some(LastIssue {
                cause: IssueCause::ComputerRelay,
                ended: at(6),
                duration: Some(3)
            })
        );
        let events = written(dir.path());
        let start = &events[1];
        assert_eq!(start.phase, Some(EventPhase::Start));
        let snap = start.snapshot.as_ref().unwrap();
        assert_eq!(snap.tunnel, Some(SegmentState::Failed));
        assert_eq!(snap.channel_rtt, Some(4));
        assert_eq!(snap.phone_latency, Some(41));
        assert_eq!(snap.radio.as_deref(), Some("5G"));
        assert_eq!(snap.thermal, Some(2));
        assert_eq!(snap.computer_version.as_deref(), Some("1.0.0"));
        assert_eq!(events[2].phase, Some(EventPhase::End));
        assert_eq!(events[2].duration, Some(3));
        assert_eq!(events[0].detail.as_deref(), Some("session abc"));
    }

    #[test]
    fn stopped_monitor_ignores_probes() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = monitor(dir.path());
        for t in 0..5 {
            m.tunnel(false, at(t));
            m.frames_stale(Instant::now(), at(t));
        }
        assert_eq!(m.status(), HealthStatus::default());
        assert!(written(dir.path()).is_empty());
    }

    #[test]
    fn sleep_ends_an_open_issue_and_warms_up_again() {
        let dir = tempfile::tempdir().unwrap();
        // The default 10 s warmup, unlike `timing()`.
        let log = HealthLog::new(dir.path().to_path_buf(), TimeZone::UTC);
        let mut m = HealthMonitor::new(Some(log), HealthWatchTiming::default(), "1.0.0".into());
        m.tunnel_started("abc", at(0));
        // Failures inside the warmup count for nothing.
        m.channel(None, at(5));
        m.channel(None, at(6));
        assert_eq!(m.status().cause, None);
        m.channel(None, at(11));
        m.channel(None, at(12));
        assert_eq!(m.status().cause, Some(IssueCause::Channel));

        m.sleep_changed(at(20));
        assert_eq!(m.status().cause, None, "the issue ended with the sleep");
        let events = written(dir.path());
        assert!(
            events
                .iter()
                .any(|e| e.kind == EventKind::Issue && e.phase == Some(EventPhase::End)),
            "{events:?}"
        );
        // Warming up again: failures within 10 s of the sleep change nothing.
        m.channel(None, at(21));
        m.channel(None, at(22));
        m.channel(None, at(23));
        assert_eq!(m.status().cause, None);
        // After the warmup the judge works as before.
        m.channel(None, at(31));
        m.channel(None, at(32));
        assert_eq!(m.status().cause, Some(IssueCause::Channel));
    }
}
