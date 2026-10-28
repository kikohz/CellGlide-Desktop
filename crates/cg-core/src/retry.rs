//! Retry delays shared by the connection state machine and the battery tasks.

use std::time::Duration;

/// Retry delay after the n-th consecutive failure: 2, 4, 8, 16, then 30 seconds.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.clamp(1, 5)).min(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_thirty_seconds() {
        let secs: Vec<u64> = (1..=7).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 30, 30, 30]);
    }
}
