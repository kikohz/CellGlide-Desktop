//! Tells the controller when the computer suspends and resumes, so the health judge doesn't
//! read the gap as an outage (what `sleep.rs` does with logind on Linux). Three sources:
//!
//! - the service control manager's power events, for S3 sleep and hibernation;
//! - Modern Standby, which sends no power event: the kernel's own record of it, Kernel-Power
//!   events 506 (entering) and 507 (exiting) in the System log, read as they are written.
//!   They mark the standby itself, so the display going off on its own doesn't count;
//! - a wall clock that jumped further than the tick between two readings, for anything the
//!   other two missed (timers keep running in Network-Connected standby, but not always).
//!
//! One sleep gives one `Sleep { sleeping: true }` and one `Sleep { sleeping: false }`,
//! whichever sources report it.

use cg_core::controller::{Command, Controller};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// How often the wall clock is read.
const TICK: Duration = Duration::from_secs(5);
/// How much later than the tick a reading may come before it counts as a sleep.
const SLACK: Duration = Duration::from_secs(30);

/// Kernel-Power: "The system is entering Modern Standby".
const STANDBY_ENTER: u32 = 506;
/// Kernel-Power: "The system is exiting Modern Standby".
const STANDBY_EXIT: u32 = 507;

/// What the sources report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Power event: the computer is suspending (S3 or hibernation).
    Suspend,
    /// Power event: the automatic resume that follows every wake.
    Resume,
    /// Kernel-Power 506, written at `at`.
    StandbyEnter { at: SystemTime, reason: Option<u32> },
    /// Kernel-Power 507, written at `at`.
    StandbyExit { at: SystemTime, reason: Option<u32> },
    /// The wall clock jumped: the computer was asleep and is awake now.
    ClockJump,
}

/// True when `now` is more than `tick + 30 s` after `previous`: the computer was asleep in
/// between. A clock set back is not a sleep.
pub fn slept(previous: SystemTime, now: SystemTime, tick: Duration) -> bool {
    now.duration_since(previous)
        .is_ok_and(|gap| gap > tick + SLACK)
}

/// Turns signals into sleep flags, one `true` and one `false` per sleep.
#[derive(Debug, Default)]
pub struct Sleep {
    asleep: bool,
    /// When the last sleep ended: a standby record written before then belongs to a sleep
    /// that has already been reported (one delivered late, after the wall clock saw it).
    woke_at: Option<SystemTime>,
}

impl Sleep {
    /// The flags to send for `signal`, received at `now`.
    pub fn on(&mut self, signal: Signal, now: SystemTime) -> Vec<bool> {
        if let Signal::StandbyEnter { at, .. } | Signal::StandbyExit { at, .. } = signal
            && self.woke_at.is_some_and(|woke| at < woke)
        {
            tracing::debug!("stale standby record ignored: {signal:?}");
            return Vec::new();
        }
        match (signal, self.asleep) {
            (Signal::Suspend, false) => {
                tracing::info!("power event: suspending");
                self.fall_asleep()
            }
            (Signal::StandbyEnter { reason, .. }, false) => {
                tracing::info!(
                    "Modern Standby: entering (Kernel-Power {STANDBY_ENTER}, reason {})",
                    describe(reason)
                );
                self.fall_asleep()
            }
            (Signal::Resume, true) => {
                tracing::info!("power event: resumed");
                self.wake(now)
            }
            (Signal::StandbyExit { reason, .. }, true) => {
                tracing::info!(
                    "Modern Standby: exiting (Kernel-Power {STANDBY_EXIT}, reason {})",
                    describe(reason)
                );
                self.wake(now)
            }
            (Signal::ClockJump, true) => {
                tracing::info!("the wall clock jumped: the computer was asleep and is awake");
                self.wake(now)
            }
            (Signal::ClockJump, false) => {
                tracing::info!("the wall clock jumped: the computer was asleep");
                self.woke_at = Some(now);
                vec![true, false]
            }
            // Already in the state reported: another source saw the same sleep first.
            (signal, _) => {
                tracing::debug!("already reported: {signal:?}");
                Vec::new()
            }
        }
    }

    fn fall_asleep(&mut self) -> Vec<bool> {
        self.asleep = true;
        vec![true]
    }

    fn wake(&mut self, now: SystemTime) -> Vec<bool> {
        self.asleep = false;
        self.woke_at = Some(now);
        vec![false]
    }
}

fn describe(reason: Option<u32>) -> String {
    reason.map_or_else(|| "unknown".to_owned(), |code| code.to_string())
}

/// Reads a Kernel-Power 506 or 507 record rendered as XML. Anything else is `None`.
pub fn parse_standby(xml: &str) -> Option<Signal> {
    let id: u32 = element(xml, "EventID")?.trim().parse().ok()?;
    let at = attribute(xml, "TimeCreated", "SystemTime")?
        .parse::<jiff::Timestamp>()
        .ok()?;
    let at = SystemTime::from(at);
    let reason = data(xml, "Reason").and_then(|r| r.trim().parse().ok());
    match id {
        STANDBY_ENTER => Some(Signal::StandbyEnter { at, reason }),
        STANDBY_EXIT => Some(Signal::StandbyExit { at, reason }),
        _ => None,
    }
}

/// The text of the first `<name ...>text</name>`.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let mut rest = xml;
    loop {
        let start = rest.find(&open)? + open.len();
        rest = &rest[start..];
        // `<EventID>` or `<EventID Qualifiers=...>`, not `<EventIDs>`.
        if rest.starts_with('>') || rest.starts_with(' ') {
            break;
        }
    }
    let body = &rest[rest.find('>')? + 1..];
    Some(&body[..body.find('<')?])
}

/// The value of `attribute='...'` (or `"..."`) on the first `<element ...>`.
fn attribute<'a>(xml: &'a str, element: &str, attribute: &str) -> Option<&'a str> {
    let tag = &xml[xml.find(&format!("<{element} "))?..];
    let tag = &tag[..tag.find('>')?];
    let value = &tag[tag.find(&format!("{attribute}="))? + attribute.len() + 1..];
    let quote = value.chars().next().filter(|q| *q == '\'' || *q == '"')?;
    let value = &value[1..];
    Some(&value[..value.find(quote)?])
}

/// The text of `<Data Name='name'>text</Data>`.
fn data<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let start = [
        format!("<Data Name='{name}'>"),
        format!("<Data Name=\"{name}\">"),
    ]
    .iter()
    .find_map(|open| xml.find(open.as_str()).map(|at| at + open.len()))?;
    let body = &xml[start..];
    Some(&body[..body.find('<')?])
}

/// Runs until the controller shuts down. Signals are passed on as they come; the wall clock
/// is checked every five seconds even after the signal channel has closed.
pub async fn watch(controller: Controller, mut events: mpsc::Receiver<Signal>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut previous = SystemTime::now();
    let mut open = true;
    let mut sleep = Sleep::default();
    loop {
        let signal = tokio::select! {
            event = events.recv(), if open => match event {
                Some(signal) => signal,
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
                    Signal::ClockJump
                } else {
                    continue;
                }
            }
        };
        for sleeping in sleep.on(signal, SystemTime::now()) {
            if controller.send(Command::Sleep { sleeping }).await.is_err() {
                // The controller is shutting down.
                return;
            }
        }
    }
}

pub use standby::Standby;

mod standby {
    use super::{Signal, parse_standby};
    use std::ffi::c_void;
    use tokio::sync::mpsc;
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::System::EventLog::{
        EVT_HANDLE, EVT_SUBSCRIBE_NOTIFY_ACTION, EvtClose, EvtRender, EvtRenderEventXml,
        EvtSubscribe, EvtSubscribeActionDeliver, EvtSubscribeStartAtOldestRecord,
        EvtSubscribeToFutureEvents,
    };
    use windows::core::w;

    /// A live subscription to Kernel-Power 506 and 507 in the System log. Dropping it
    /// unsubscribes.
    pub struct Standby {
        handle: EVT_HANDLE,
        sender: *mut mpsc::Sender<Signal>,
    }

    // The sender is only read by the callback, which the subscription handle owns; both
    // are released together in `drop`.
    unsafe impl Send for Standby {}

    impl Standby {
        /// Subscribes to records written from now on.
        pub fn subscribe(sender: mpsc::Sender<Signal>) -> windows::core::Result<Self> {
            Self::open(sender, false)
        }

        /// Also delivers the records already in the log, oldest first (for tests).
        pub fn subscribe_from_oldest(sender: mpsc::Sender<Signal>) -> windows::core::Result<Self> {
            Self::open(sender, true)
        }

        fn open(sender: mpsc::Sender<Signal>, oldest: bool) -> windows::core::Result<Self> {
            let sender = Box::into_raw(Box::new(sender));
            let flags = if oldest {
                EvtSubscribeStartAtOldestRecord.0
            } else {
                EvtSubscribeToFutureEvents.0
            };
            // SAFETY: the query strings are static, the context points at a live sender
            // that is freed only after the subscription is closed, and `deliver` matches
            // EVT_SUBSCRIBE_CALLBACK.
            let handle = unsafe {
                EvtSubscribe(
                    None,
                    None,
                    w!("System"),
                    w!(
                        "*[System[Provider[@Name='Microsoft-Windows-Kernel-Power'] and (EventID=506 or EventID=507)]]"
                    ),
                    None,
                    Some(sender as *const c_void),
                    Some(deliver),
                    flags,
                )
            };
            match handle {
                Ok(handle) => Ok(Self { handle, sender }),
                Err(error) => {
                    // SAFETY: no subscription holds the pointer.
                    drop(unsafe { Box::from_raw(sender) });
                    Err(error)
                }
            }
        }
    }

    impl Drop for Standby {
        fn drop(&mut self) {
            // SAFETY: closing the subscription waits for a running callback to return and
            // stops further ones, so the sender is no longer used afterwards.
            unsafe {
                if let Err(error) = EvtClose(self.handle) {
                    tracing::warn!("can't close the Modern Standby subscription: {error}");
                    // Leak the sender rather than free it under a callback.
                    return;
                }
                drop(Box::from_raw(self.sender));
            }
            tracing::debug!("stopped watching for Modern Standby");
        }
    }

    /// Runs on a thread pool thread for each record; must not block.
    unsafe extern "system" fn deliver(
        action: EVT_SUBSCRIBE_NOTIFY_ACTION,
        context: *const c_void,
        event: EVT_HANDLE,
    ) -> u32 {
        // SAFETY: `context` is the sender passed to EvtSubscribe, alive until EvtClose.
        let sender = unsafe { &*(context as *const mpsc::Sender<Signal>) };
        if action != EvtSubscribeActionDeliver {
            // For an error, `event` carries the Win32 error code.
            tracing::warn!("Modern Standby subscription error {}", event.0);
            return 0;
        }
        // SAFETY: `event` is the record being delivered, valid during the callback.
        match unsafe { render(event) }.as_deref().and_then(parse_standby) {
            Some(signal) => {
                if sender.try_send(signal).is_err() {
                    tracing::warn!("standby record dropped: {signal:?}");
                }
            }
            None => tracing::debug!("unreadable Kernel-Power record"),
        }
        0
    }

    /// The record as XML.
    unsafe fn render(event: EVT_HANDLE) -> Option<String> {
        let mut used = 0u32;
        let mut count = 0u32;
        // SAFETY (both calls): the buffer is `size` bytes long, or absent with size 0.
        let first = unsafe {
            EvtRender(
                None,
                event,
                EvtRenderEventXml.0,
                0,
                None,
                &mut used,
                &mut count,
            )
        };
        match first {
            Err(error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {}
            _ => return None,
        }
        let mut buffer = vec![0u16; (used as usize).div_ceil(2)];
        unsafe {
            EvtRender(
                None,
                event,
                EvtRenderEventXml.0,
                (buffer.len() * 2) as u32,
                Some(buffer.as_mut_ptr().cast()),
                &mut used,
                &mut count,
            )
        }
        .ok()?;
        let chars = (used as usize / 2).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..chars]);
        Some(text.trim_end_matches('\0').to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTER_XML: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Microsoft-Windows-Kernel-Power' Guid='{331c3b3a-2005-44c2-ac5e-77220c37d6b4}'/><EventID>506</EventID><Version>3</Version><Level>4</Level><Task>157</Task><Opcode>0</Opcode><Keywords>0x8000000000000404</Keywords><TimeCreated SystemTime='2026-10-30T16:59:09.2945317Z'/><EventRecordID>1593</EventRecordID><Correlation/><Execution ProcessID='4' ThreadID='7464'/><Channel>System</Channel><Computer>HOST</Computer><Security UserID='S-1-5-18'/></System><EventData><Data Name='Reason'>12</Data><Data Name='LidOpenState'>true</Data><Data Name='ScenarioInstanceId'>81</Data></EventData></Event>";
    const EXIT_XML: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Microsoft-Windows-Kernel-Power' Guid='{331c3b3a-2005-44c2-ac5e-77220c37d6b4}'/><EventID>507</EventID><Version>11</Version><TimeCreated SystemTime='2026-10-30T17:25:00.4743168Z'/><Channel>System</Channel></System><EventData><Data Name='EnergyDrain'>0</Data><Data Name='DurationInUs'>1551179113</Data><Data Name='Reason'>1</Data><Data Name='AudioPlaybackInUs'>0</Data></EventData></Event>";

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn enter(at: u64) -> Signal {
        Signal::StandbyEnter {
            at: t(at),
            reason: Some(12),
        }
    }

    fn exit(at: u64) -> Signal {
        Signal::StandbyExit {
            at: t(at),
            reason: Some(1),
        }
    }

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

    #[test]
    fn parses_kernel_power_records() {
        let at = |s: &str| SystemTime::from(s.parse::<jiff::Timestamp>().unwrap());
        assert_eq!(
            parse_standby(ENTER_XML),
            Some(Signal::StandbyEnter {
                at: at("2026-10-30T16:59:09.2945317Z"),
                reason: Some(12)
            })
        );
        assert_eq!(
            parse_standby(EXIT_XML),
            Some(Signal::StandbyExit {
                at: at("2026-10-30T17:25:00.4743168Z"),
                reason: Some(1)
            })
        );
    }

    #[test]
    fn parse_rejects_other_records_and_tolerates_a_missing_reason() {
        assert_eq!(
            parse_standby(&ENTER_XML.replace("<EventID>506<", "<EventID>42<")),
            None
        );
        assert_eq!(parse_standby("<Event/>"), None);
        let qualified = ENTER_XML
            .replace("<EventID>", "<EventID Qualifiers='0'>")
            .replace("<Data Name='Reason'>12</Data>", "");
        assert!(matches!(
            parse_standby(&qualified),
            Some(Signal::StandbyEnter { reason: None, .. })
        ));
    }

    #[test]
    fn a_modern_standby_gives_one_entry_and_one_exit() {
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(enter(0), t(0)), [true]);
        assert_eq!(sleep.on(exit(420), t(420)), [false]);
    }

    #[test]
    fn s3_power_events_still_work() {
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(Signal::Suspend, t(0)), [true]);
        assert_eq!(sleep.on(Signal::Resume, t(600)), [false]);
        // Repeats change nothing.
        assert_eq!(sleep.on(Signal::Resume, t(601)), Vec::<bool>::new());
    }

    #[test]
    fn a_clock_jump_inside_a_standby_ends_it_once() {
        // Lid closed: 506 at entry, the timers stop, the clock jumps on wake, then 507.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(enter(0), t(0)), [true]);
        assert_eq!(sleep.on(Signal::ClockJump, t(850)), [false]);
        assert_eq!(sleep.on(exit(849), t(851)), Vec::<bool>::new());
    }

    #[test]
    fn records_delivered_after_the_clock_jump_are_stale() {
        // The clock saw the sleep first; 506 and 507 arrive late, written before the wake.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(Signal::ClockJump, t(850)), [true, false]);
        assert_eq!(sleep.on(enter(0), t(851)), Vec::<bool>::new());
        assert_eq!(sleep.on(exit(849), t(851)), Vec::<bool>::new());
        // The next standby is reported normally.
        assert_eq!(sleep.on(enter(900), t(900)), [true]);
        assert_eq!(sleep.on(exit(960), t(960)), [false]);
    }

    #[test]
    fn standby_turning_into_suspend_is_one_sleep() {
        // Standby, then hibernation: 506, suspend, resume, 507.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(enter(0), t(0)), [true]);
        assert_eq!(sleep.on(Signal::Suspend, t(3_600)), Vec::<bool>::new());
        assert_eq!(sleep.on(Signal::Resume, t(7_200)), [false]);
        assert_eq!(sleep.on(exit(7_199), t(7_201)), Vec::<bool>::new());
    }

    #[test]
    fn an_exit_without_an_entry_is_ignored() {
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(exit(10), t(10)), Vec::<bool>::new());
        assert_eq!(sleep.on(Signal::Resume, t(11)), Vec::<bool>::new());
    }

    /// Reads this machine's own standby history through a real subscription. Needs at least
    /// one Modern Standby in the System log.
    #[tokio::test]
    #[ignore = "needs Kernel-Power 506/507 records in this machine's System log"]
    async fn subscription_delivers_the_logged_standby_records() {
        let (sender, mut events) = mpsc::channel(4096);
        let subscription = Standby::subscribe_from_oldest(sender).unwrap();
        let first = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("a record within 10 s")
            .unwrap();
        assert!(matches!(
            first,
            Signal::StandbyEnter { .. } | Signal::StandbyExit { .. }
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(subscription);
        let mut count = 1;
        while let Ok(signal) = events.try_recv() {
            assert!(matches!(
                signal,
                Signal::StandbyEnter { .. } | Signal::StandbyExit { .. }
            ));
            count += 1;
        }
        // Dropping the subscription dropped its sender.
        assert!(events.recv().await.is_none());
        println!("{count} standby records");
    }
}
