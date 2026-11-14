//! "Weak" link quality, wireless tier only (as the Mac): the median of the last five SOCKS
//! handshake times above 1150 ms. Only a hint for the user ("move the phone closer, or plug in
//! the cable"); it never ends a session.

use std::collections::VecDeque;
use std::time::Duration;

/// A median above this is weak.
pub const WEAK_MEDIAN: Duration = Duration::from_millis(1150);
/// How many of the most recent handshakes the median is taken over.
pub const WEAK_SAMPLES: usize = 5;

/// Whether `median_of_last_5_handshakes` (see `Handshakes`) means a weak link.
pub fn weak(median_of_last_5_handshakes: Duration) -> bool {
    median_of_last_5_handshakes > WEAK_MEDIAN
}

/// The middle value; for an even count the upper of the two middle ones. `None` when empty.
pub fn median(samples: &[Duration]) -> Option<Duration> {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied()
}

/// The last `WEAK_SAMPLES` handshake times of one session.
#[derive(Clone, Debug, Default)]
pub struct Handshakes {
    recent: VecDeque<Duration>,
}

impl Handshakes {
    pub fn new() -> Handshakes {
        Handshakes::default()
    }

    /// Adds one handshake time, dropping the oldest beyond `WEAK_SAMPLES`.
    pub fn push(&mut self, took: Duration) {
        if self.recent.len() == WEAK_SAMPLES {
            self.recent.pop_front();
        }
        self.recent.push_back(took);
    }

    /// The median once `WEAK_SAMPLES` handshakes are in; `None` before.
    pub fn median(&self) -> Option<Duration> {
        if self.recent.len() < WEAK_SAMPLES {
            return None;
        }
        let samples: Vec<Duration> = self.recent.iter().copied().collect();
        median(&samples)
    }

    /// Weak once there are enough samples and their median is above `WEAK_MEDIAN`.
    pub fn is_weak(&self) -> bool {
        self.median().is_some_and(weak)
    }

    pub fn clear(&mut self) {
        self.recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|&v| Duration::from_millis(v)).collect()
    }

    #[test]
    fn the_threshold_and_window() {
        assert_eq!(WEAK_MEDIAN, Duration::from_millis(1150));
        assert_eq!(WEAK_SAMPLES, 5);
        assert!(!weak(Duration::from_millis(1150)));
        assert!(weak(Duration::from_millis(1151)));
    }

    #[test]
    fn the_median_of_five_decides() {
        let slow = median(&ms(&[200, 300, 1200, 1300, 1400])).unwrap();
        assert_eq!(slow, Duration::from_millis(1200));
        assert!(weak(slow));
        let fine = median(&ms(&[200, 300, 400, 1300, 1400])).unwrap();
        assert_eq!(fine, Duration::from_millis(400));
        assert!(!weak(fine));
        // Order does not matter.
        assert_eq!(
            median(&ms(&[1400, 200, 1300, 300, 1200])),
            Some(Duration::from_millis(1200))
        );
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn handshakes_keep_the_last_five() {
        let mut handshakes = Handshakes::new();
        for took in ms(&[200, 300, 1200, 1300]) {
            handshakes.push(took);
        }
        assert_eq!(handshakes.median(), None, "four are not enough");
        assert!(!handshakes.is_weak());
        handshakes.push(Duration::from_millis(1400));
        assert!(handshakes.is_weak());
        // Two fast ones push 200 and 300 out: 1200, 1300, 1400, 100, 100 → median 1200.
        handshakes.push(Duration::from_millis(100));
        handshakes.push(Duration::from_millis(100));
        assert!(handshakes.is_weak());
        // One more: 1300, 1400, 100, 100, 100 → median 100.
        handshakes.push(Duration::from_millis(100));
        assert!(!handshakes.is_weak());
        handshakes.clear();
        assert_eq!(handshakes.median(), None);
    }
}
