//! Tells the controller when the computer suspends and resumes, so the health judge doesn't
//! read the gap as an outage (what `sleep.rs` does with logind on Linux). Two sources: the
//! service control manager's power events (`true` = suspending, `false` = resumed), and a
//! wall clock that jumped further than the tick between two readings, which also covers
//! Modern Standby, where no event may arrive.

use cg_core::controller::{Command, Controller};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// How often the wall clock is read.
const TICK: Duration = Duration::from_secs(5);
/// How much later than the tick a reading may come before it counts as a sleep.
const SLACK: Duration = Duration::from_secs(30);

/// True when `now` is more than `tick + 30 s` after `previous`: the computer was asleep in
/// between. A clock set back is not a sleep.
pub fn slept(previous: SystemTime, now: SystemTime, tick: Duration) -> bool {
    now.duration_since(previous)
        .is_ok_and(|gap| gap > tick + SLACK)
}

/// Runs until the controller shuts down. Power events are passed on as they come; the
/// wall clock is checked every five seconds even after the event channel has closed
/// (`--console` has no service control manager to send any).
pub async fn watch(controller: Controller, mut events: mpsc::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut previous = SystemTime::now();
    let mut open = true;
    loop {
        let commands: Vec<bool> = tokio::select! {
            event = events.recv(), if open => match event {
                Some(sleeping) => vec![sleeping],
                None => {
                    open = false;
                    continue;
                }
            },
            _ = ticker.tick() => {
                let now = SystemTime::now();
                let gap = slept(previous, now, TICK);
                previous = now;
                if gap {
                    tracing::info!("the wall clock jumped: the computer was asleep");
                    vec![true, false]
                } else {
                    continue;
                }
            }
        };
        for sleeping in commands {
            if controller.send(Command::Sleep { sleeping }).await.is_err() {
                // The controller is shutting down.
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slept_detects_a_gap() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(!slept(
            t0,
            t0 + Duration::from_secs(6),
            Duration::from_secs(5)
        ));
        assert!(slept(
            t0,
            t0 + Duration::from_secs(120),
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn slept_ignores_a_clock_set_back_and_the_edge() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let tick = Duration::from_secs(5);
        assert!(!slept(t0, t0 - Duration::from_secs(3_600), tick));
        assert!(!slept(t0, t0 + Duration::from_secs(35), tick));
        assert!(slept(t0, t0 + Duration::from_millis(35_001), tick));
    }
}
