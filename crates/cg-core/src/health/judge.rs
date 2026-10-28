//! Turns probe results into a state per leg and one cause for the whole connection,
//! ported from the Mac's `HealthJudge.swift`.
//!
//! A leg fails after `fail_after` failures in a row and recovers after `recover_after`
//! successes in a row. A `None` result (no data) counts neither way and keeps the leg's
//! state; only `unknown_after` of them in a row turn it unknown. Within an open issue a
//! higher-ranked cause escalates it, keeping the issue and its start. A lower-ranked one is
//! usually legs recovering at different rates, so it takes over only once it has held for
//! `downgrade_after` (a new issue then starts, e.g. the computer's relay stalling as the
//! phone comes back). The issue ends when no leg is failed. Nothing is judged while
//! stopped or within `warmup` of a start or wake.

use cg_ipc::health::{IssueCause, Segment, SegmentState};
use jiff::{SignedDuration, Timestamp};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JudgeConfig {
    pub fail_after: u32,
    pub recover_after: u32,
    pub unknown_after: u32,
    pub warmup: Duration,
    pub downgrade_after: Duration,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        JudgeConfig {
            fail_after: 2,
            recover_after: 3,
            unknown_after: 5,
            warmup: Duration::from_secs(10),
            downgrade_after: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangePhase {
    Started,
    Escalated,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IssueChange {
    pub phase: ChangePhase,
    pub cause: IssueCause,
    pub started: Timestamp,
    pub at: Timestamp,
}

/// Counts and state for one leg.
#[derive(Clone, Copy, Debug, Default)]
struct Leg {
    state: SegmentState,
    failures: u32,
    successes: u32,
    nils: u32,
}

#[derive(Debug)]
pub struct HealthJudge {
    config: JudgeConfig,
    legs: [Leg; 4],
    cause: Option<IssueCause>,
    issue_started: Option<Timestamp>,
    judging_from: Option<Timestamp>,
    /// A lower-ranked cause seen during an open issue, and since when.
    lower_cause: Option<(IssueCause, Timestamp)>,
}

fn index(s: Segment) -> usize {
    match s {
        Segment::Channel => 0,
        Segment::PhoneExit => 1,
        Segment::Tunnel => 2,
        Segment::Connections => 3,
    }
}

fn plus(t: Timestamp, d: Duration) -> Timestamp {
    SignedDuration::try_from(d)
        .ok()
        .and_then(|d| t.checked_add(d).ok())
        .unwrap_or(Timestamp::MAX)
}

impl HealthJudge {
    pub fn new(config: JudgeConfig) -> Self {
        HealthJudge {
            config,
            legs: [Leg::default(); 4],
            cause: None,
            issue_started: None,
            judging_from: None,
            lower_cause: None,
        }
    }

    pub fn start(&mut self, at: Timestamp) {
        self.clear_counts();
        self.judging_from = Some(plus(at, self.config.warmup));
    }

    pub fn stop(&mut self, at: Timestamp) -> Vec<IssueChange> {
        let changes = self.end_issue(at);
        self.clear_counts();
        self.judging_from = None;
        changes
    }

    /// After a wake or a restart of the legs: ends any issue, forgets counts and warms up
    /// again if it was running.
    pub fn reset(&mut self, at: Timestamp) -> Vec<IssueChange> {
        let running = self.judging_from.is_some();
        let changes = self.end_issue(at);
        self.clear_counts();
        if running {
            self.judging_from = Some(plus(at, self.config.warmup));
        }
        changes
    }

    pub fn record(
        &mut self,
        segment: Segment,
        ok: Option<bool>,
        at: Timestamp,
    ) -> Vec<IssueChange> {
        match self.judging_from {
            Some(from) if at >= from => {}
            _ => return Vec::new(),
        }
        let config = self.config;
        let leg = &mut self.legs[index(segment)];
        match ok {
            None => {
                leg.failures = 0;
                leg.successes = 0;
                leg.nils += 1;
                if leg.nils >= config.unknown_after {
                    leg.state = SegmentState::Unknown;
                }
            }
            Some(true) => {
                leg.nils = 0;
                leg.failures = 0;
                leg.successes += 1;
                let needed = if leg.state == SegmentState::Failed {
                    config.recover_after
                } else {
                    1
                };
                if leg.state != SegmentState::Ok && leg.successes >= needed {
                    leg.state = SegmentState::Ok;
                }
            }
            Some(false) => {
                leg.nils = 0;
                leg.successes = 0;
                leg.failures += 1;
                if leg.failures >= config.fail_after {
                    leg.state = SegmentState::Failed;
                }
            }
        }
        self.update(at)
    }

    pub fn state(&self, segment: Segment) -> SegmentState {
        self.legs[index(segment)].state
    }

    pub fn cause(&self) -> Option<IssueCause> {
        self.cause
    }

    pub fn issue_started(&self) -> Option<Timestamp> {
        self.issue_started
    }

    /// The table in the spec, top row first.
    fn current_cause(&self) -> Option<IssueCause> {
        let failed = |s| self.state(s) == SegmentState::Failed;
        if failed(Segment::Channel) {
            return Some(IssueCause::Channel);
        }
        // The computer's traffic fails and so does the phone, either its own checks or the
        // connections it opens for the computer (its checks can pass while its upstream is
        // still coming back): the phone's side. Only a tunnel failing while the phone's
        // connections are fine, or never reach it, is the computer's relay.
        if failed(Segment::Tunnel) && (failed(Segment::PhoneExit) || failed(Segment::Connections)) {
            return Some(IssueCause::PhoneNetwork);
        }
        if failed(Segment::Tunnel) {
            return Some(IssueCause::ComputerRelay);
        }
        if failed(Segment::PhoneExit) {
            return Some(IssueCause::PhoneUnstable);
        }
        if failed(Segment::Connections) {
            return Some(IssueCause::PartialFailures);
        }
        None
    }

    fn update(&mut self, at: Timestamp) -> Vec<IssueChange> {
        let next = self.current_cause();
        let higher_or_none = match (next, self.cause) {
            (None, _) | (_, None) => true,
            (Some(n), Some(c)) => n == c || n.rank() > c.rank(),
        };
        if higher_or_none {
            self.lower_cause = None;
        }
        if next == self.cause {
            return Vec::new();
        }
        let Some(next) = next else {
            return self.end_issue(at);
        };
        if let (Some(current), Some(started)) = (self.cause, self.issue_started) {
            if next.rank() > current.rank() {
                self.cause = Some(next);
                return vec![IssueChange {
                    phase: ChangePhase::Escalated,
                    cause: next,
                    started,
                    at,
                }];
            }
            if self.lower_cause.map(|(c, _)| c) != Some(next) {
                self.lower_cause = Some((next, at));
            }
            let since = match self.lower_cause {
                Some((_, since)) if plus(since, self.config.downgrade_after) <= at => since,
                _ => return Vec::new(),
            };
            self.lower_cause = None;
            let mut changes = self.end_issue(since);
            self.cause = Some(next);
            self.issue_started = Some(since);
            changes.push(IssueChange {
                phase: ChangePhase::Started,
                cause: next,
                started: since,
                at: since,
            });
            return changes;
        }
        self.cause = Some(next);
        self.issue_started = Some(at);
        vec![IssueChange {
            phase: ChangePhase::Started,
            cause: next,
            started: at,
            at,
        }]
    }

    fn end_issue(&mut self, at: Timestamp) -> Vec<IssueChange> {
        let ended = match (self.cause.take(), self.issue_started.take()) {
            (Some(cause), Some(started)) => vec![IssueChange {
                phase: ChangePhase::Ended,
                cause,
                started,
                at,
            }],
            _ => Vec::new(),
        };
        self.lower_cause = None;
        ended
    }

    fn clear_counts(&mut self) {
        self.legs = [Leg::default(); 4];
    }
}

#[cfg(test)]
mod tests {
    //! `HealthJudgeTests.swift`, case by case.

    use super::*;
    use cg_ipc::health::{IssueCause, Segment, SegmentState};
    use jiff::Timestamp;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(1_791_000_000 + s).unwrap()
    }

    fn change(phase: ChangePhase, cause: IssueCause, started: i64, when: i64) -> IssueChange {
        IssueChange {
            phase,
            cause,
            started: at(started),
            at: at(when),
        }
    }

    /// A judge past its warm-up with every segment it's told about healthy.
    fn healthy(segments: &[Segment]) -> HealthJudge {
        let mut j = HealthJudge::new(JudgeConfig::default());
        j.start(at(0));
        for i in 0..3 {
            for &s in segments {
                j.record(s, Some(true), at(11 + i));
            }
        }
        j
    }

    const THREE: [Segment; 3] = [Segment::Channel, Segment::PhoneExit, Segment::Tunnel];

    fn fail(j: &mut HealthJudge, s: Segment, a: i64) {
        j.record(s, Some(false), at(a));
        j.record(s, Some(false), at(a + 1));
    }

    #[test]
    fn two_failures_start_an_issue_and_three_successes_end_it() {
        let mut j = healthy(&THREE);
        assert_eq!(j.record(Segment::Tunnel, Some(false), at(20)), vec![]);
        assert_eq!(
            j.record(Segment::Tunnel, Some(false), at(23)),
            vec![change(
                ChangePhase::Started,
                IssueCause::ComputerRelay,
                23,
                23
            )]
        );
        j.record(Segment::Tunnel, Some(true), at(26));
        j.record(Segment::Tunnel, Some(true), at(29));
        assert_eq!(j.cause(), Some(IssueCause::ComputerRelay));
        assert_eq!(
            j.record(Segment::Tunnel, Some(true), at(32)),
            vec![change(
                ChangePhase::Ended,
                IssueCause::ComputerRelay,
                23,
                32
            )]
        );
        assert_eq!(j.cause(), None);
    }

    #[test]
    fn alternating_results_never_flip() {
        let mut j = healthy(&THREE);
        for i in 0..10 {
            assert_eq!(
                j.record(Segment::Tunnel, Some(i % 2 == 0), at(20 + i)),
                vec![]
            );
        }
        assert_eq!(j.cause(), None);
    }

    #[test]
    fn causes_by_priority() {
        fn cause(failing: &[Segment], connections_ok: bool) -> Option<IssueCause> {
            let mut j = healthy(&Segment::ALL);
            if !connections_ok {
                j.record(Segment::Connections, Some(false), at(20));
                j.record(Segment::Connections, Some(false), at(21));
            }
            for &s in failing {
                j.record(s, Some(false), at(30));
                j.record(s, Some(false), at(31));
            }
            j.cause()
        }
        use Segment::*;
        assert_eq!(cause(&[Channel, Tunnel], true), Some(IssueCause::Channel));
        assert_eq!(
            cause(&[PhoneExit, Tunnel], true),
            Some(IssueCause::PhoneNetwork)
        );
        assert_eq!(cause(&[Tunnel], true), Some(IssueCause::ComputerRelay));
        // The phone's own checks pass but its upstream connections time out: still the phone.
        assert_eq!(cause(&[Tunnel], false), Some(IssueCause::PhoneNetwork));
        assert_eq!(cause(&[PhoneExit], true), Some(IssueCause::PhoneUnstable));
        assert_eq!(cause(&[], false), Some(IssueCause::PartialFailures));
        assert_eq!(cause(&[], true), None);
    }

    #[test]
    fn unknown_phone_data_does_not_fail_but_tunnel_still_names_the_computer() {
        let mut j = healthy(&[Segment::Channel, Segment::Tunnel]);
        j.record(Segment::PhoneExit, None, at(20));
        j.record(Segment::PhoneExit, None, at(21));
        assert_eq!(j.cause(), None);
        assert_eq!(j.state(Segment::PhoneExit), SegmentState::Unknown);
        j.record(Segment::Tunnel, Some(false), at(22));
        j.record(Segment::Tunnel, Some(false), at(25));
        assert_eq!(j.cause(), Some(IssueCause::ComputerRelay));
    }

    #[test]
    fn warm_up_ignores_results() {
        let mut j = HealthJudge::new(JudgeConfig::default());
        j.start(at(0));
        for i in 0..5 {
            assert_eq!(j.record(Segment::Tunnel, Some(false), at(i * 2)), vec![]);
        }
        assert_eq!(j.cause(), None);
    }

    #[test]
    fn stop_ends_an_open_issue() {
        let mut j = healthy(&THREE);
        j.record(Segment::Channel, Some(false), at(20));
        j.record(Segment::Channel, Some(false), at(23));
        assert_eq!(
            j.stop(at(40)),
            vec![change(ChangePhase::Ended, IssueCause::Channel, 23, 40)]
        );
        assert_eq!(j.cause(), None);
        // Stopped: nothing judged.
        assert_eq!(j.record(Segment::Channel, Some(false), at(41)), vec![]);
    }

    #[test]
    fn wake_resets_counts_and_warms_up_again() {
        let mut j = healthy(&THREE);
        j.record(Segment::Tunnel, Some(false), at(20));
        j.reset(at(100));
        // The first failure after waking is inside the warm-up and doesn't count.
        assert_eq!(j.record(Segment::Tunnel, Some(false), at(101)), vec![]);
        assert_eq!(j.record(Segment::Tunnel, Some(false), at(104)), vec![]);
        assert_eq!(j.cause(), None);
    }

    #[test]
    fn escalation_keeps_one_issue_with_its_original_start() {
        let mut j = healthy(&THREE);
        j.record(Segment::Tunnel, Some(false), at(20));
        j.record(Segment::Tunnel, Some(false), at(23));
        j.record(Segment::Channel, Some(false), at(24));
        assert_eq!(
            j.record(Segment::Channel, Some(false), at(27)),
            vec![change(ChangePhase::Escalated, IssueCause::Channel, 23, 27)]
        );
        assert_eq!(j.cause(), Some(IssueCause::Channel));
        assert_eq!(j.issue_started(), Some(at(23)));
        assert_eq!(
            j.stop(at(40)),
            vec![change(ChangePhase::Ended, IssueCause::Channel, 23, 40)]
        );
    }

    #[test]
    fn recovery_never_downgrades_the_cause() {
        let mut j = healthy(&THREE);
        fail(&mut j, Segment::Tunnel, 20);
        fail(&mut j, Segment::PhoneExit, 22);
        assert_eq!(j.cause(), Some(IssueCause::PhoneNetwork));
        j.record(Segment::PhoneExit, Some(true), at(30));
        j.record(Segment::PhoneExit, Some(true), at(31));
        assert_eq!(j.record(Segment::PhoneExit, Some(true), at(32)), vec![]);
        assert_eq!(j.cause(), Some(IssueCause::PhoneNetwork));
        j.record(Segment::Tunnel, Some(true), at(33));
        j.record(Segment::Tunnel, Some(true), at(34));
        let ended = j.record(Segment::Tunnel, Some(true), at(35));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].phase, ChangePhase::Ended);
        assert_eq!(ended[0].cause, IssueCause::PhoneNetwork);
    }

    #[test]
    fn a_lower_cause_that_lasts_twenty_seconds_takes_over() {
        let mut j = healthy(&THREE);
        fail(&mut j, Segment::Tunnel, 20);
        fail(&mut j, Segment::PhoneExit, 22);
        assert_eq!(j.cause(), Some(IssueCause::PhoneNetwork));
        // The phone recovers at 32 s but the computer's relay stays down (a stall as the phone came back).
        for t in [30, 31, 32] {
            j.record(Segment::PhoneExit, Some(true), at(t));
        }
        assert_eq!(j.record(Segment::Tunnel, Some(false), at(45)), vec![]);
        assert_eq!(j.cause(), Some(IssueCause::PhoneNetwork));
        assert_eq!(
            j.record(Segment::Tunnel, Some(false), at(52)),
            vec![
                change(ChangePhase::Ended, IssueCause::PhoneNetwork, 21, 32),
                change(ChangePhase::Started, IssueCause::ComputerRelay, 32, 32),
            ]
        );
        assert_eq!(j.cause(), Some(IssueCause::ComputerRelay));
        assert_eq!(j.issue_started(), Some(at(32)));
    }

    #[test]
    fn channel_recovering_first_does_not_start_computer_relay() {
        let mut j = healthy(&THREE);
        fail(&mut j, Segment::Tunnel, 20);
        fail(&mut j, Segment::Channel, 22);
        assert_eq!(j.cause(), Some(IssueCause::Channel));
        let mut all = Vec::new();
        for i in 0..3 {
            all.extend(j.record(Segment::Channel, Some(true), at(30 + i)));
        }
        assert!(all.iter().all(|c| c.phase != ChangePhase::Started));
        assert_eq!(j.cause(), Some(IssueCause::Channel));
    }

    #[test]
    fn one_nil_keeps_a_failure_and_five_make_it_unknown() {
        let mut j = healthy(&THREE);
        fail(&mut j, Segment::PhoneExit, 20);
        assert_eq!(j.cause(), Some(IssueCause::PhoneUnstable));
        assert_eq!(j.record(Segment::PhoneExit, None, at(30)), vec![]);
        assert_eq!(j.state(Segment::PhoneExit), SegmentState::Failed);
        assert_eq!(j.cause(), Some(IssueCause::PhoneUnstable));
        let mut changes = Vec::new();
        for i in 1..5 {
            changes.extend(j.record(Segment::PhoneExit, None, at(30 + i)));
        }
        assert_eq!(j.state(Segment::PhoneExit), SegmentState::Unknown);
        assert_eq!(
            changes.iter().map(|c| c.phase).collect::<Vec<_>>(),
            vec![ChangePhase::Ended]
        );
        assert_eq!(j.cause(), None);
    }
}
