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
use tokio_util::sync::CancellationToken;

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
    /// The wall clock jumped from the reading `since`: the computer was asleep at some
    /// point after it and is awake now.
    ClockJump { since: SystemTime },
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
        // The timers stop in S3 (and in a standby that reaches its deepest state), so the
        // jump is seen up to a tick after the wake; a resume or 507 since the reading before
        // the gap has already reported this sleep.
        if let Signal::ClockJump { since } = signal
            && self.woke_at.is_some_and(|woke| woke >= since)
        {
            tracing::debug!("wall clock jump already reported by a wake");
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
            // A new standby while still marked asleep: the last 507 was lost (a full queue,
            // or a gap in the subscription). Report the entry again rather than swallow it;
            // a second `Sleep { sleeping: true }` only resets the health judge once more.
            (Signal::StandbyEnter { reason, .. }, true) => {
                tracing::info!(
                    "Modern Standby: entering (Kernel-Power {STANDBY_ENTER}, reason {}; the last exit was missed)",
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
            (Signal::ClockJump { .. }, true) => {
                tracing::info!("the wall clock jumped: the computer was asleep and is awake");
                self.wake(now)
            }
            (Signal::ClockJump { .. }, false) => {
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
                let since = previous;
                previous = now;
                if slept(since, now, TICK) {
                    Signal::ClockJump { since }
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

/// A subscription that can report that it stopped working.
pub trait Subscription {
    /// Completes when the subscription has failed and should be replaced.
    fn failed(&self) -> impl Future<Output = ()> + Send + '_;
}

/// How long a subscription must have worked before its failure starts the backoff over.
const STEADY: Duration = Duration::from_secs(60);

/// Keeps a subscription open until `stop`, opening a new one with backoff (`delay(n)` after
/// the n-th failure in a row) when opening fails or the subscription reports an error. The
/// subscription is dropped (unsubscribed) before this returns.
pub async fn keep_subscribed<S, E>(
    mut open: impl FnMut() -> Result<S, E>,
    stop: CancellationToken,
    delay: impl Fn(u32) -> Duration,
) where
    S: Subscription,
    E: std::fmt::Display,
{
    let mut failures = 0u32;
    loop {
        let wait = match open() {
            Ok(subscription) => {
                tracing::info!("watching for Modern Standby (Kernel-Power 506/507)");
                let opened = tokio::time::Instant::now();
                tokio::select! {
                    _ = subscription.failed() => {}
                    _ = stop.cancelled() => return,
                }
                drop(subscription);
                if opened.elapsed() >= STEADY {
                    failures = 0;
                }
                failures = failures.saturating_add(1);
                let wait = delay(failures);
                tracing::warn!(
                    "the Modern Standby subscription failed; subscribing again in {:.1} s",
                    wait.as_secs_f32()
                );
                wait
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                let wait = delay(failures);
                tracing::warn!(
                    "not watching for Modern Standby: {error}; trying again in {:.1} s",
                    wait.as_secs_f32()
                );
                wait
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.cancelled() => return,
        }
    }
}

mod standby {
    use super::{Signal, Subscription, parse_standby};
    use std::ffi::c_void;
    use std::sync::{Mutex, PoisonError};
    use tokio::sync::{Notify, mpsc};
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::System::EventLog::{
        EVT_HANDLE, EVT_SUBSCRIBE_NOTIFY_ACTION, EvtClose, EvtRender, EvtRenderEventXml,
        EvtSubscribe, EvtSubscribeActionDeliver, EvtSubscribeStartAtOldestRecord,
        EvtSubscribeToFutureEvents,
    };
    use windows::core::w;

    /// What the callback reaches through its context pointer.
    struct Context {
        /// Taken out when the subscription is closed, which closes the channel.
        sender: Mutex<Option<mpsc::Sender<Signal>>>,
        /// Told when the subscription reports an error.
        failed: Notify,
    }

    /// A live subscription to Kernel-Power 506 and 507 in the System log. Dropping it
    /// unsubscribes.
    pub struct Standby {
        handle: EVT_HANDLE,
        context: &'static Context,
    }

    // SAFETY: the handle is closed once, in `drop`, and the context is synchronised.
    unsafe impl Send for Standby {}
    unsafe impl Sync for Standby {}

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
            // The context is never freed. EvtClose's documentation doesn't promise to wait
            // for a callback already running, so there is no point after which freeing it
            // is known to be safe. A few dozen bytes per subscription (one per service
            // run; more only after the event log fails) buy soundness without that
            // promise. The sender inside is still released on close.
            let context: &'static Context = Box::leak(Box::new(Context {
                sender: Mutex::new(Some(sender)),
                failed: Notify::new(),
            }));
            let flags = if oldest {
                EvtSubscribeStartAtOldestRecord.0
            } else {
                EvtSubscribeToFutureEvents.0
            };
            // SAFETY: the query strings are static, the context lives for the rest of the
            // process, and `deliver` matches EVT_SUBSCRIBE_CALLBACK.
            let handle = unsafe {
                EvtSubscribe(
                    None,
                    None,
                    w!("System"),
                    w!(
                        "*[System[Provider[@Name='Microsoft-Windows-Kernel-Power'] and (EventID=506 or EventID=507)]]"
                    ),
                    None,
                    Some(std::ptr::from_ref(context).cast::<c_void>()),
                    Some(deliver),
                    flags,
                )
            }?;
            Ok(Self { handle, context })
        }
    }

    impl Subscription for Standby {
        fn failed(&self) -> impl Future<Output = ()> + Send + '_ {
            self.context.failed.notified()
        }
    }

    impl Drop for Standby {
        fn drop(&mut self) {
            // SAFETY: the handle came from EvtSubscribe and is closed only here.
            if let Err(error) = unsafe { EvtClose(self.handle) } {
                tracing::warn!("can't close the Modern Standby subscription: {error}");
            }
            // A callback still running, or one that comes after all, finds no sender.
            drop(
                self.context
                    .sender
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
            tracing::debug!("stopped watching for Modern Standby");
        }
    }

    /// Runs on a thread pool thread for each record; must not block.
    unsafe extern "system" fn deliver(
        action: EVT_SUBSCRIBE_NOTIFY_ACTION,
        context: *const c_void,
        event: EVT_HANDLE,
    ) -> u32 {
        // SAFETY: `context` is the leaked `Context` passed to EvtSubscribe, never freed.
        let context = unsafe { &*context.cast::<Context>() };
        if action != EvtSubscribeActionDeliver {
            // For an error, `event` carries the Win32 error code.
            tracing::warn!("Modern Standby subscription error {}", event.0);
            context.failed.notify_one();
            return 0;
        }
        // SAFETY: `event` is the record being delivered, valid during the callback.
        let Some(signal) = unsafe { render(event) }.as_deref().and_then(parse_standby) else {
            tracing::debug!("unreadable Kernel-Power record");
            return 0;
        };
        let sender = context
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = sender.as_ref()
            && sender.try_send(signal).is_err()
        {
            tracing::warn!("standby record dropped: {signal:?}");
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
        assert_eq!(sleep.on(jump(-3), t(850)), [false]);
        assert_eq!(sleep.on(exit(849), t(851)), Vec::<bool>::new());
    }

    /// A wall clock jump from the reading taken `secs` after t(0).
    fn jump(secs: i64) -> Signal {
        let base = t(0);
        let since = if secs >= 0 {
            base + Duration::from_secs(secs.unsigned_abs())
        } else {
            base - Duration::from_secs(secs.unsigned_abs())
        };
        Signal::ClockJump { since }
    }

    #[test]
    fn a_clock_jump_after_a_resume_is_the_same_sleep() {
        // S3: suspend, the automatic resume at once, the clock's jump a tick later.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(Signal::Suspend, t(0)), [true]);
        assert_eq!(sleep.on(Signal::Resume, t(600)), [false]);
        assert_eq!(sleep.on(jump(-2), t(603)), Vec::<bool>::new());
        // A later sleep the clock alone sees is still reported.
        assert_eq!(sleep.on(jump(700), t(1_000)), [true, false]);
    }

    #[test]
    fn a_clock_jump_after_a_standby_exit_is_the_same_sleep() {
        // Lid standby with the timers stopped: 506, 507 at once on wake, then the jump.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(enter(0), t(0)), [true]);
        assert_eq!(sleep.on(exit(850), t(850)), [false]);
        assert_eq!(sleep.on(jump(-4), t(854)), Vec::<bool>::new());
    }

    #[test]
    fn a_new_entry_after_a_lost_exit_is_reported() {
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(enter(0), t(0)), [true]);
        // The 507 at t(60) never arrives.
        assert_eq!(sleep.on(enter(300), t(300)), [true]);
        assert_eq!(sleep.on(exit(360), t(360)), [false]);
    }

    #[test]
    fn records_delivered_after_the_clock_jump_are_stale() {
        // The clock saw the sleep first; 506 and 507 arrive late, written before the wake.
        let mut sleep = Sleep::default();
        assert_eq!(sleep.on(jump(-5), t(850)), [true, false]);
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

    struct FakeSubscription {
        failed: std::sync::Arc<tokio::sync::Notify>,
        dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Subscription for FakeSubscription {
        fn failed(&self) -> impl Future<Output = ()> + Send + '_ {
            self.failed.notified()
        }
    }

    impl Drop for FakeSubscription {
        fn drop(&mut self) {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn keeps_trying_to_subscribe_and_resubscribes_after_a_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        let failed = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let stop = CancellationToken::new();
        let task = tokio::spawn(keep_subscribed(
            {
                let (failed, dropped, opens) = (failed.clone(), dropped.clone(), opens.clone());
                move || {
                    // The first two tries fail, like an event log service not up yet.
                    if opens.fetch_add(1, Ordering::SeqCst) < 2 {
                        return Err("unavailable");
                    }
                    Ok(FakeSubscription {
                        failed: failed.clone(),
                        dropped: dropped.clone(),
                    })
                }
            },
            stop.clone(),
            {
                let delays = delays.clone();
                move |n| {
                    delays.lock().unwrap().push(n);
                    Duration::from_millis(10)
                }
            },
        ));
        let until = |n: usize| {
            let opens = opens.clone();
            async move {
                while opens.load(Ordering::SeqCst) < n {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), until(3))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(opens.load(Ordering::SeqCst), 3, "open and holding");
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        // The subscription reports an error: it is closed and replaced.
        failed.notify_one();
        tokio::time::timeout(Duration::from_secs(5), until(4))
            .await
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        // Backoff grows with failures in a row.
        assert_eq!(*delays.lock().unwrap(), [1, 2, 3]);
        // Stop unsubscribes.
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
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
