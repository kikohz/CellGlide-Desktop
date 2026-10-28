//! The phone's cumulative connect counts turned into a recent failure rate, ported from
//! the Mac's `ConnectionWindow.swift`. Too few connects say nothing; counts going backwards
//! mean sharing restarted, and a baseline older than twice the span (the phone went quiet)
//! is dropped rather than averaged over.

use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

#[derive(Clone, Debug)]
struct Sample {
    at: Timestamp,
    ok: i64,
    failed: BTreeMap<String, i64>,
}

#[derive(Clone, Debug)]
pub struct ConnectionWindow {
    span: SignedDuration,
    minimum: i64,
    threshold: f64,
    samples: VecDeque<Sample>,
    top_failure: Option<String>,
    recent_failed: BTreeMap<String, i64>,
}

impl Default for ConnectionWindow {
    fn default() -> Self {
        ConnectionWindow::new(Duration::from_secs(10), 5, 0.5)
    }
}

impl ConnectionWindow {
    pub fn new(span: Duration, minimum: i64, threshold: f64) -> Self {
        ConnectionWindow {
            span: SignedDuration::try_from(span).unwrap_or(SignedDuration::MAX),
            minimum,
            threshold,
            samples: VecDeque::new(),
            top_failure: None,
            recent_failed: BTreeMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.clear_failures();
    }

    /// The most common failure kind in the window.
    pub fn top_failure(&self) -> Option<&str> {
        self.top_failure.as_deref()
    }

    /// The failures per kind in the window.
    pub fn recent_failed(&self) -> &BTreeMap<String, i64> {
        &self.recent_failed
    }

    /// `Some(true)` when healthy, `Some(false)` when more than `threshold` of the connects
    /// in the window failed, `None` when there is too little to say.
    pub fn observe(
        &mut self,
        ok: i64,
        failed: &BTreeMap<String, i64>,
        at: Timestamp,
    ) -> Option<bool> {
        if let Some(last) = self.samples.back() {
            let backwards = ok < last.ok
                || failed
                    .iter()
                    .any(|(kind, &n)| n < last.failed.get(kind).copied().unwrap_or(0));
            if backwards {
                self.reset();
            }
        }
        self.samples.push_back(Sample {
            at,
            ok,
            failed: failed.clone(),
        });
        // Keep the newest sample at least `span` old as the baseline.
        while self.samples.len() > 2 && at.duration_since(self.samples[1].at) >= self.span {
            self.samples.pop_front();
        }
        if let Some(base) = self.samples.front()
            && at.duration_since(base.at) > self.span * 2
        {
            let keep = self.samples.len() - 1;
            self.samples.drain(..keep);
            self.clear_failures();
        }
        if self.samples.len() < 2 {
            return None;
        }
        let base = self.samples.front()?;
        let ok_delta = ok - base.ok;
        let failed_delta: BTreeMap<String, i64> = failed
            .iter()
            .filter_map(|(kind, &n)| {
                let before = base.failed.get(kind).copied().unwrap_or(0);
                (n > before).then(|| (kind.clone(), n - before))
            })
            .collect();
        let failed_total: i64 = failed_delta.values().sum();
        let total = ok_delta + failed_total;
        // Ties go to the first kind by name, so the answer doesn't depend on map order.
        self.top_failure = failed_delta
            .iter()
            .fold(None::<(&String, i64)>, |best, (k, &n)| match best {
                Some((_, m)) if m >= n => best,
                _ => Some((k, n)),
            })
            .map(|(k, _)| k.clone());
        self.recent_failed = failed_delta;
        if total < self.minimum {
            return None;
        }
        Some(failed_total as f64 / total as f64 <= self.threshold)
    }

    fn clear_failures(&mut self) {
        self.top_failure = None;
        self.recent_failed.clear();
    }
}

#[cfg(test)]
mod tests {
    //! `ConnectionWindowTests.swift`, case by case.

    use super::*;
    use jiff::Timestamp;
    use std::collections::BTreeMap;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(1_791_000_000 + s).unwrap()
    }

    fn failed(kinds: &[(&str, i64)]) -> BTreeMap<String, i64> {
        kinds.iter().map(|&(k, n)| (k.to_string(), n)).collect()
    }

    #[test]
    fn failure_rate_over_ten_seconds() {
        let mut w = ConnectionWindow::default();
        assert_eq!(w.observe(100, &failed(&[]), at(0)), None);
        // +2 ok, +4 timeouts in 5 s: 6 connects, 67 % failed.
        assert_eq!(
            w.observe(102, &failed(&[("timeout", 4)]), at(5)),
            Some(false)
        );
        assert_eq!(w.top_failure(), Some("timeout"));
        // 12 s later the old counts left the window: +10 ok, nothing failed since.
        assert_eq!(
            w.observe(112, &failed(&[("timeout", 4)]), at(17)),
            Some(true)
        );
    }

    #[test]
    fn too_few_connects_says_nothing() {
        let mut w = ConnectionWindow::default();
        w.observe(0, &failed(&[]), at(0));
        assert_eq!(w.observe(1, &failed(&[("refused", 2)]), at(3)), None);
    }

    #[test]
    fn counters_going_backwards_mean_sharing_restarted() {
        let mut w = ConnectionWindow::default();
        w.observe(500, &failed(&[("timeout", 40)]), at(0));
        // Restarted: counts start again from zero; no negative or huge rates.
        assert_eq!(w.observe(3, &failed(&[]), at(2)), None);
        assert_eq!(w.observe(10, &failed(&[]), at(4)), Some(true));
    }

    #[test]
    fn recent_failed_holds_the_window_deltas_and_clears_on_restart() {
        let mut w = ConnectionWindow::default();
        w.observe(100, &failed(&[("timeout", 10), ("refused", 3)]), at(0));
        w.observe(102, &failed(&[("timeout", 14), ("refused", 3)]), at(5));
        assert_eq!(w.recent_failed(), &failed(&[("timeout", 4)]));
        assert_eq!(w.top_failure(), Some("timeout"));
        // Restarted: no stale failure kind from before.
        assert_eq!(w.observe(1, &failed(&[]), at(7)), None);
        assert_eq!(w.recent_failed(), &failed(&[]));
        assert_eq!(w.top_failure(), None);
    }

    #[test]
    fn an_old_baseline_is_dropped() {
        let mut w = ConnectionWindow::default();
        w.observe(0, &failed(&[]), at(0));
        // The phone went quiet for a minute: the old sample would average a whole minute.
        assert_eq!(w.observe(50, &failed(&[("timeout", 50)]), at(60)), None);
        assert_eq!(w.top_failure(), None);
        assert_eq!(w.recent_failed(), &failed(&[]));
        assert_eq!(
            w.observe(51, &failed(&[("timeout", 56)]), at(63)),
            Some(false)
        );
    }

    #[test]
    fn reset_forgets_the_baseline() {
        let mut w = ConnectionWindow::default();
        w.observe(100, &failed(&[]), at(0));
        w.observe(102, &failed(&[("timeout", 4)]), at(5));
        w.reset();
        assert_eq!(w.top_failure(), None);
        assert_eq!(w.recent_failed(), &failed(&[]));
        // The next sample is a fresh baseline, so it says nothing.
        assert_eq!(w.observe(110, &failed(&[("timeout", 9)]), at(6)), None);
    }
}
