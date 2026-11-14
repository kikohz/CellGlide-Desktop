//! The connection state machine. One task owns all state; usbmuxd events, user
//! commands and session events arrive as messages. Every change is published as a
//! Snapshot on a watch channel.

use crate::battery::{self, Reporter};
use crate::doh::DohUpstream;
use crate::exit_check::{CaptiveProbe, ExitProbe, HttpsProbe, TunnelProbe};
use crate::forwarder::{self, Forwarder};
use crate::health::judge::JudgeConfig;
use crate::health::log::HealthLog;
use crate::health::monitor::HealthMonitor;
use crate::health::weak::Handshakes;
use crate::lockdown;
use crate::mux::{self, MuxEndpoint};
use crate::phone::{self, Ended, Phone};
use crate::phone_feed::{PhoneFeed, sanitize};
use crate::resolver::{self, Resolver};
use crate::retry::backoff;
use crate::settings::{LastVersions, SettingsStore, Stored};
use crate::tunnel::{
    DnsPlan, FALLBACK_DNS, RESOLVER, ResolverOrder, RunningTunnel, TunnelBackend, TunnelNotice,
    TunnelPlan,
};
use crate::wireless::cable_pairing;
use crate::wireless::ports::{DialError, WirelessPorts};
use crate::wireless::store::WirelessStore;
use crate::{CoreError, Result};
use cg_ipc::wireless::{WirelessFailure, WirelessStatus};
use wireless_session::{NotEstablished, WirelessEvent};
pub use wireless_session::{WirelessPhase, WirelessTiming};

mod wireless_session;
use cg_ipc::health::{DayStart, EventKind, HistoryDay};
use cg_ipc::phone::{PhoneLabel, UsbPhone};
use cg_ipc::{ConnState, DnsMode, PhoneChoice, Reason, Snapshot, StrictDns};
use cg_protocol::mux::{Device, MuxEvent};
use cg_protocol::phone::{Hello, PhoneMetrics, capability};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub struct ControllerConfig {
    pub endpoint: MuxEndpoint,
    pub backend: Arc<dyn TunnelBackend>,
    pub resolver_addr: SocketAddr,
    /// How long the resolver waits for the phone before answering SERVFAIL.
    pub resolver_deadline: Duration,
    pub computer_name: Option<String>,
    pub health: HealthTiming,
    pub exit_probe: Arc<dyn ExitProbe>,
    pub relisten_delay: Duration,
    pub settings: SettingsSource,
    /// Where sysfs is read for the link speed; `None` leaves the speed out.
    pub sysfs_root: Option<std::path::PathBuf>,
    /// Keep private, multicast and link-local ranges out of the tunnel.
    pub bypass: bool,
    /// The connection timeline's checks (M3), beside the M1 checks in `health`.
    pub health_watch: HealthWatchTiming,
    /// The computer's own traffic through the tunnel.
    pub tunnel_probe: Arc<dyn TunnelProbe>,
    /// Where the seven-day timeline is written; `None` writes nothing.
    pub health_dir: Option<PathBuf>,
    /// The zone whose local days name the timeline's files.
    pub time_zone: TimeZone,
    /// Where wireless pairings are kept; `None` means the feature is absent and nothing wireless
    /// is attempted.
    pub wireless: Option<Arc<WirelessStore>>,
    /// The service forces the wireless switch on, whatever the stored setting says.
    pub wireless_forced: bool,
    /// The platform's access point, discovery and TLS dialer; `None` where wireless sessions
    /// are not available.
    pub wireless_ports: Option<WirelessPorts>,
    /// The wireless attempt's windows (`wireless::policy`'s values by default).
    pub wireless_timing: WirelessTiming,
    /// Health checks of a wireless session: `health` and `health_watch` stay with USB.
    pub wireless_health: HealthTiming,
    pub wireless_health_watch: HealthWatchTiming,
}

/// How often the timeline's checks run, as the Mac's `HealthController`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthWatchTiming {
    /// SOCKS greeting through the check forwarder, timed.
    pub channel: Duration,
    /// The captive check through the tunnel while all is well…
    pub tunnel: Duration,
    /// …and while it fails or an issue is open.
    pub fast: Duration,
    /// A phone that sent nothing for this long no longer vouches for its own legs.
    pub frame_stale: Duration,
    pub channel_timeout: Duration,
    /// The whole captive check through the tunnel (`TunnelProbe::check_within`).
    pub tunnel_timeout: Duration,
    pub judge: JudgeConfig,
    /// The median handshake time above which the link counts as weak (`health::weak`); `None`
    /// (USB) never calls it weak.
    pub weak_median: Option<Duration>,
}

impl Default for HealthWatchTiming {
    fn default() -> Self {
        HealthWatchTiming {
            channel: Duration::from_secs(3),
            tunnel: Duration::from_secs(15),
            fast: Duration::from_secs(3),
            frame_stale: Duration::from_secs(6),
            channel_timeout: Duration::from_secs(2),
            tunnel_timeout: crate::exit_check::CAPTIVE_TIMEOUT,
            judge: JudgeConfig::default(),
            weak_median: None,
        }
    }
}

/// The wireless tier's SOCKS handshake timeout, for the quick check and the timeline's check.
pub const WIRELESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(2500);
/// The wireless tier's limit for the small request through the tunnel.
pub const WIRELESS_TUNNEL_TIMEOUT: Duration = Duration::from_millis(5500);

impl HealthWatchTiming {
    /// The wireless tier (as the Mac): SOCKS handshake 2.5 s, the small request through the
    /// tunnel 5.5 s, a leg fails after 3 failures in a row, and a median handshake above
    /// 1150 ms is weak.
    pub fn wireless() -> Self {
        HealthWatchTiming {
            channel_timeout: WIRELESS_HANDSHAKE_TIMEOUT,
            tunnel_timeout: WIRELESS_TUNNEL_TIMEOUT,
            judge: JudgeConfig::wireless(),
            weak_median: Some(crate::health::weak::WEAK_MEDIAN),
            ..HealthWatchTiming::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HealthTiming {
    /// SOCKS greeting through the check forwarder.
    pub quick: Duration,
    pub quick_timeout: Duration,
    /// Hello again plus the HTTPS exit check.
    pub full: Duration,
    /// After one failed exit check.
    pub retry: Duration,
    /// Minimum spacing of checks triggered by the phone reporting no exit.
    pub early_spacing: Duration,
    /// This many failed quick checks in a row run a full check at once.
    pub quick_fail_after: u32,
}

impl Default for HealthTiming {
    fn default() -> Self {
        HealthTiming {
            quick: Duration::from_secs(5),
            quick_timeout: Duration::from_secs(2),
            full: Duration::from_secs(60),
            retry: Duration::from_secs(5),
            early_spacing: Duration::from_secs(30),
            quick_fail_after: 2,
        }
    }
}

impl HealthTiming {
    /// The wireless tier: the quick SOCKS check waits 2.5 s, and 3 failures in a row (not 2)
    /// run a full check.
    pub fn wireless() -> Self {
        HealthTiming {
            quick_timeout: WIRELESS_HANDSHAKE_TIMEOUT,
            quick_fail_after: 3,
            ..HealthTiming::default()
        }
    }
}

/// Counts one quick check result; true when `fail_after` failures in a row are reached (the
/// count then starts again).
fn quick_check_counted(failures: &mut u32, ok: bool, fail_after: u32) -> bool {
    if ok {
        *failures = 0;
        return false;
    }
    *failures += 1;
    if *failures >= fail_after {
        *failures = 0;
        return true;
    }
    false
}

/// Where the persistent settings live.
pub enum SettingsSource {
    Memory(Stored),
    File(std::path::PathBuf),
}

impl ControllerConfig {
    pub fn new(endpoint: MuxEndpoint, backend: Arc<dyn TunnelBackend>) -> Self {
        Self {
            endpoint,
            backend,
            resolver_addr: RESOLVER,
            resolver_deadline: resolver::UPSTREAM_TIMEOUT,
            computer_name: None,
            health: HealthTiming::default(),
            exit_probe: Arc::new(HttpsProbe::cloudflare()),
            relisten_delay: Duration::from_secs(2),
            settings: SettingsSource::Memory(Stored::default()),
            sysfs_root: None,
            bypass: true,
            health_watch: HealthWatchTiming::default(),
            tunnel_probe: Arc::new(CaptiveProbe::apple()),
            health_dir: None,
            time_zone: TimeZone::system(),
            wireless: None,
            wireless_forced: false,
            wireless_ports: None,
            wireless_timing: WirelessTiming::default(),
            wireless_health: HealthTiming::wireless(),
            wireless_health_watch: HealthWatchTiming::wireless(),
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Connect {
        serial: Option<String>,
    },
    Disconnect,
    SetSettings {
        auto_connect: Option<bool>,
        phone: Option<PhoneChoice>,
        /// Windows' strict DNS; applied at once while connected.
        strict_dns: Option<bool>,
    },
    /// The computer is about to suspend (`true`) or has resumed (`false`): the health
    /// judgement starts over. The session is left to the exit checks. On resume, a
    /// connection that is wanted but not up is tried at once, from a fresh backoff, and a
    /// lost usbmuxd is rejoined at once.
    Sleep {
        sleeping: bool,
    },
    /// Starts the access point and connects to a paired iPhone over it. Refused (`BadRequest`)
    /// unless the wireless switch is on, the platform and the pairing store are there, no session
    /// runs and no iPhone is on USB; while an attempt already runs it does nothing.
    ConnectWireless,
    /// Ends the wireless attempt (and its session), closing the access point.
    CancelWireless,
    /// The wireless switch. Turning it off ends a wireless attempt (`TurnedOff`).
    SetWireless {
        enabled: bool,
    },
    /// The user's answer to the code shown in the wireless phase `Pairing`: whether the iPhone
    /// shows the same code. Refused (`BadRequest`, nothing changes) when no code is shown.
    ConfirmCode {
        matches: bool,
    },
}

enum Message {
    Command(Command, tokio::sync::oneshot::Sender<Result<()>>),
    Shutdown(tokio::sync::oneshot::Sender<()>),
}

/// Whether usbmuxd counts as reachable before the first look at it. On Windows, Apple Devices'
/// usbmuxd runs from sign-in, and the app takes the iPhones listed once usbmuxd is reachable
/// again for ones plugged in all along (`cg_ipc::describe::ui::MuxLife::FromSignIn`): until
/// the first look has an answer the service says "not reachable", not "no iPhone", so an app
/// that connects right after the service (re)starts doesn't take the empty list for its
/// baseline and the iPhone then listed for one just plugged in (W4 item i review). On Linux
/// usbmuxd starts on plug-in and nothing changes.
const MUX_UP_AT_START: bool = !cfg!(windows);

/// The snapshot published before the actor has looked at anything.
fn first_snapshot() -> Snapshot {
    Snapshot {
        state: if MUX_UP_AT_START {
            ConnState::NoPhone
        } else {
            ConnState::NoUsbService
        },
        ..Snapshot::default()
    }
}

#[derive(Clone)]
pub struct Controller {
    messages: mpsc::Sender<Message>,
    snapshot: watch::Receiver<Snapshot>,
    /// The timeline, read here so `history` never waits for the actor.
    history: Option<Arc<HealthLog>>,
    time_zone: TimeZone,
}

impl Controller {
    pub fn spawn(config: ControllerConfig) -> Controller {
        let (tx, rx) = mpsc::channel(16);
        let (snapshot_tx, snapshot) = watch::channel(first_snapshot());
        let time_zone = config.time_zone.clone();
        let history = config
            .health_dir
            .clone()
            .map(|dir| Arc::new(HealthLog::new(dir, time_zone.clone())));
        tokio::spawn(Actor::new(config, snapshot_tx).run(rx));
        Controller {
            messages: tx,
            snapshot,
            history,
            time_zone,
        }
    }

    /// One day of the timeline (`None`: today), read straight from the files. Empty when
    /// nothing is written or the directory can't be read.
    pub fn history(&self, day: Option<Date>) -> HistoryDay {
        history_day(
            self.history.as_deref(),
            &self.time_zone,
            day,
            Timestamp::now(),
        )
    }

    /// Returns once the command has taken effect (a disconnect: once the tunnel is down), or
    /// `CoreError::BadRequest` for one that does not apply now.
    pub async fn send(&self, command: Command) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.messages
            .send(Message::Command(command, tx))
            .await
            .map_err(|_| CoreError::ShuttingDown)?;
        rx.await.map_err(|_| CoreError::ShuttingDown)?
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    /// Tears the tunnel down and stops the state machine.
    pub async fn shutdown(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.messages.send(Message::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

enum FullCheck {
    Ok,
    SessionChanged,
    NoExit,
    Failed(Reason),
}

/// Events from a session's helper tasks, tagged with the session they belong to.
enum Event {
    Ended(u64),
    DnsChanged(u64),
    Metrics(u64, Box<PhoneMetrics>),
    /// Whether the check forwarder answered a SOCKS greeting.
    QuickCheck(u64, bool),
    FullCheck(u64, FullCheck),
    /// What the battery reader found: `None` while it cannot read the phone.
    Usb(u64, Option<Box<UsbPhone>>),
    /// The name and model of an attached phone, by serial.
    Label(String, u64, PhoneLabel),
    /// The timeline's channel check: the greeting's round trip, `None` if unanswered.
    HealthChannel(u64, Option<Duration>),
    /// The timeline's tunnel check.
    HealthTunnel(u64, bool),
    /// From the wireless attempt's task, tagged with the attempt.
    Wireless(u64, WirelessEvent),
    /// Whether the USB device (serial, mux id) answered the phone app's hello.
    UsbProbe(String, u32, bool),
}

/// Aborts the session's helper tasks unless the session is handed over.
struct TaskGuard(Vec<JoinHandle<()>>);

impl TaskGuard {
    fn spawn(&mut self, task: impl Future<Output = ()> + Send + 'static) {
        // Health checks come and go for the whole session: forget the finished ones.
        self.0.retain(|t| !t.is_finished());
        self.0.push(tokio::spawn(task));
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.iter().for_each(JoinHandle::abort);
    }
}

/// What one established session holds. Dropping it stops helper tasks (health checks
/// included) and relays; the tunnel itself is stopped explicitly first (see `teardown`).
struct Active {
    /// First, so the helper tasks are aborted before anything else is dropped.
    tasks: TaskGuard,
    /// Sends the battery to the phone app; its connection closes with the session.
    reporter: Reporter,
    /// The phone and the link this session uses.
    phone: Phone,
    hello: Hello,
    dns: DnsMode,
    /// As the tunnel reported it once finished.
    strict_dns: StrictDns,
    tunnel: Option<Box<dyn RunningTunnel>>,
    forwarder: Forwarder,
    /// A second forwarder only the health checks use, like the Mac's checkForwarder.
    checks: Forwarder,
    _resolver: Option<Resolver>,
    next_quick: Instant,
    quick_check_running: bool,
    quick_failures: u32,
    next_full: Instant,
    /// Health checks run as tasks so commands and events are never held up by them.
    full_check_running: bool,
    exit_retried: bool,
    last_early: Option<Instant>,
    last_exit_ok: Option<bool>,
    next_channel: Instant,
    next_tunnel: Instant,
    channel_running: bool,
    tunnel_running: bool,
    /// The checks' tier: the controller's for USB, the wireless one for a wireless session.
    health: HealthTiming,
    watch: HealthWatchTiming,
    /// The latest SOCKS handshake times, for "weak".
    handshakes: Handshakes,
    /// A wireless session's task waits on the other end: dropping this (with the session,
    /// after the tunnel is stopped) tells it the session is over.
    ended: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Active {
    fn is_wireless(&self) -> bool {
        self.phone.is_wireless()
    }

    /// The median of the last handshakes is above this tier's limit (never for USB).
    fn weak(&self) -> bool {
        self.watch
            .weak_median
            .is_some_and(|limit| self.handshakes.median().is_some_and(|m| m > limit))
    }
}

/// The wireless attempt, as the actor sees it.
struct Wireless {
    phase: WirelessPhase,
    /// Tags the task's events; bumped whenever an attempt ends, so a late event is ignored.
    attempt: u64,
    task: Option<wireless_session::Task>,
    /// The access point's network name, once it is up.
    ssid: Option<String>,
    /// Why the last try to build the session failed, while the attempt keeps trying.
    last_error: Option<Reason>,
    /// USB devices asked whether the phone app answers (by serial): when last, and whether
    /// they are due again (they appeared since).
    probes: HashMap<String, UsbProbe>,
    /// The probe in flight, if any: (serial, mux id) and its task.
    probing: Option<(String, u32, JoinHandle<()>)>,
    /// While the phase is `Pairing`: where the user's answer goes. Dropped (the question
    /// withdrawn) whenever the attempt ends.
    confirm: Option<tokio::sync::oneshot::Sender<bool>>,
}

#[derive(Clone, Copy, Debug)]
struct UsbProbe {
    last: Instant,
    due: bool,
}

enum Choice {
    Waiting,
    Choose,
    Selected(Device),
}

/// The id `Phone::id` gives a wireless phone.
fn is_wireless_id(serial: &str) -> bool {
    serial.starts_with("wireless:")
}

/// A phone chosen by serial is never replaced; otherwise only a single phone is used.
fn choose(devices: &[Device], chosen: Option<&str>) -> Choice {
    if let Some(serial) = chosen {
        return devices
            .iter()
            .find(|d| d.serial == serial)
            .cloned()
            .map_or(Choice::Waiting, Choice::Selected);
    }
    match devices {
        [] => Choice::Waiting,
        [one] => Choice::Selected(one.clone()),
        _ => Choice::Choose,
    }
}

struct Actor {
    config: ControllerConfig,
    snapshot: watch::Sender<Snapshot>,
    mux_events: Option<mpsc::Receiver<MuxEvent>>,
    next_listen: Instant,
    mux_up: bool,
    /// usbmuxd's last device list was empty and that was logged: an empty list is logged
    /// once, not on every relisten.
    mux_listed_none: bool,
    devices: Vec<Device>,
    settings: SettingsStore,
    /// The user pressed Connect since the last session ended; not persisted.
    manual: bool,
    connecting: bool,
    active: Option<Active>,
    last_error: Option<Reason>,
    failures: u32,
    retry_at: Option<Instant>,
    generation: u64,
    feed: PhoneFeed,
    /// What USB reveals about the phone (filled in by the battery reader).
    usb: Option<UsbPhone>,
    /// Name and model of each attached phone, read once when it appears.
    labels: BTreeMap<String, PhoneLabel>,
    label_tasks: HashMap<String, (u64, JoinHandle<()>)>,
    label_seq: u64,
    monitor: HealthMonitor,
    wireless: Wireless,
}

impl Actor {
    fn new(mut config: ControllerConfig, snapshot: watch::Sender<Snapshot>) -> Self {
        let settings = match std::mem::replace(
            &mut config.settings,
            SettingsSource::Memory(Stored::default()),
        ) {
            SettingsSource::Memory(stored) => SettingsStore::memory(stored),
            SettingsSource::File(path) => SettingsStore::load(path),
        };
        let log = config
            .health_dir
            .clone()
            .map(|dir| HealthLog::new(dir, config.time_zone.clone()));
        let monitor = HealthMonitor::new(
            log,
            config.health_watch,
            env!("CARGO_PKG_VERSION").to_string(),
        );
        Self {
            config,
            snapshot,
            mux_events: None,
            next_listen: Instant::now(),
            mux_up: MUX_UP_AT_START,
            mux_listed_none: false,
            devices: Vec::new(),
            settings,
            manual: false,
            connecting: false,
            active: None,
            last_error: None,
            failures: 0,
            retry_at: None,
            generation: 0,
            feed: PhoneFeed::default(),
            usb: None,
            labels: BTreeMap::new(),
            label_tasks: HashMap::new(),
            label_seq: 0,
            monitor,
            wireless: Wireless {
                phase: WirelessPhase::Off,
                attempt: 0,
                task: None,
                ssid: None,
                last_error: None,
                probes: HashMap::new(),
                probing: None,
                confirm: None,
            },
        }
    }

    async fn run(mut self, mut messages: mpsc::Receiver<Message>) {
        self.monitor.service_started(Timestamp::now());
        let (events_tx, mut events) = mpsc::channel::<Event>(64);
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        // Ticks missed while connecting are not fired back to back afterwards.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                message = messages.recv() => match message {
                    Some(Message::Command(command, done)) => {
                        let result = self.command(command, &events_tx).await;
                        self.publish();
                        let _ = done.send(result);
                    }
                    Some(Message::Shutdown(done)) => {
                        self.end_wireless(WirelessPhase::Off).await;
                        self.teardown().await;
                        self.publish();
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.end_wireless(WirelessPhase::Off).await;
                        self.teardown().await;
                        return;
                    }
                },
                event = async { self.mux_events.as_mut().unwrap().recv().await }, if self.mux_events.is_some() => {
                    self.mux_event(event, &events_tx).await;
                }
                Some(event) = events.recv() => self.session_event(event, &events_tx).await,
                _ = tick.tick() => self.tick(&events_tx).await,
            }
            self.publish();
        }
    }

    fn wants(&self) -> bool {
        let stored = self.settings.get();
        if stored.auto_connect {
            !stored.user_disconnected
        } else {
            self.manual
        }
    }

    /// The current USB session uses a different phone than `serial`. A wireless session is
    /// never replaced by choosing a USB phone (an iPhone that shows up on USB is).
    fn active_is_other(&self, serial: &str) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| !a.is_wireless() && a.phone.id() != serial)
    }

    fn active_is_wireless(&self) -> bool {
        self.active.as_ref().is_some_and(Active::is_wireless)
    }

    /// The wireless switch as it applies now.
    fn wireless_enabled(&self) -> bool {
        self.settings.get().wireless_enabled || self.config.wireless_forced
    }

    fn chosen(&self) -> Option<&str> {
        self.settings.get().chosen_serial.as_deref()
    }

    /// A session ended because of the phone (stopped sharing, unplugged).
    fn phone_ended_session(&mut self) {
        if !self.settings.get().auto_connect {
            self.manual = false;
        }
    }

    async fn command(&mut self, command: Command, events: &mpsc::Sender<Event>) -> Result<()> {
        match command {
            // A wireless phone is never chosen by serial (a client echoing `selected` back);
            // it is reached with `ConnectWireless` only.
            Command::Connect {
                serial: Some(serial),
            } if is_wireless_id(&serial) => return Err(CoreError::BadRequest),
            Command::Connect { serial } => {
                if let Some(serial) = &serial
                    && self.active_is_other(serial)
                {
                    self.teardown().await;
                }
                self.settings.update(|s| {
                    s.user_disconnected = false;
                    if serial.is_some() {
                        s.chosen_serial = serial.clone();
                    }
                });
                self.manual = true;
                self.failures = 0;
                self.last_error = None;
                self.retry_at = Some(Instant::now());
            }
            Command::Sleep { sleeping } => {
                tracing::info!(
                    "health judge reset on {}",
                    if sleeping { "suspend" } else { "resume" }
                );
                self.monitor.sleep_changed(Timestamp::now());
                // Suspending ends a wireless attempt; resuming does not start it again.
                if sleeping && self.wireless.phase.owns() {
                    tracing::info!("wireless: ending for suspend");
                    self.end_wireless(WirelessPhase::Off).await;
                }
                if !sleeping {
                    self.next_listen = Instant::now();
                    if self.wants() && self.active.is_none() {
                        self.failures = 0;
                        self.retry_soon();
                    }
                }
            }
            Command::Disconnect => {
                self.settings.update(|s| {
                    s.user_disconnected = true;
                    s.chosen_serial = None;
                });
                self.manual = false;
                self.retry_at = None;
                self.last_error = None;
                self.end_wireless(WirelessPhase::Off).await;
                self.teardown().await;
            }
            Command::ConnectWireless => return self.connect_wireless(events).await,
            Command::CancelWireless => self.end_wireless(WirelessPhase::Off).await,
            Command::ConfirmCode { matches } => return self.confirm_code(matches).await,
            Command::SetWireless { enabled } => {
                self.settings.update(|s| s.wireless_enabled = enabled);
                if !self.wireless_enabled() && self.wireless.phase.owns() {
                    tracing::info!("wireless: switched off; ending");
                    self.end_wireless(WirelessPhase::Failed(WirelessFailure::TurnedOff))
                        .await;
                }
            }
            Command::SetSettings {
                auto_connect,
                phone,
                strict_dns,
            } => {
                if let Some(on) = strict_dns {
                    self.settings.update(|s| s.strict_dns = on);
                    self.apply_strict_dns(on).await;
                }
                let chosen = match phone {
                    None => self.settings.get().chosen_serial.clone(),
                    Some(PhoneChoice::Auto) => None,
                    // A wireless phone is not a USB choice: the choice stays as it was.
                    Some(PhoneChoice::Serial { serial }) if is_wireless_id(&serial) => {
                        self.settings.get().chosen_serial.clone()
                    }
                    Some(PhoneChoice::Serial { serial }) => Some(serial),
                };
                if let Some(serial) = &chosen
                    && self.active_is_other(serial)
                {
                    self.teardown().await;
                }
                let was_auto = self.settings.get().auto_connect;
                let was_wanted = self.wants();
                self.settings.update(|s| {
                    if let Some(auto) = auto_connect {
                        s.auto_connect = auto;
                    }
                    s.chosen_serial = chosen;
                });
                if was_auto && auto_connect == Some(false) {
                    // Keep the user's session (connected, or retrying after a failure) when
                    // auto connect goes off; with nothing going on, auto off means "wait
                    // for a Connect".
                    let in_progress = self.active.as_ref().is_some_and(|a| !a.is_wireless())
                        || self.retry_at.is_some()
                        || self.last_error.is_some();
                    self.manual = was_wanted && in_progress;
                }
                self.last_error = None;
                self.retry_soon();
            }
        }
        Ok(())
    }

    async fn connect_wireless(&mut self, events: &mpsc::Sender<Event>) -> Result<()> {
        if self.wireless.phase.owns() {
            return Ok(());
        }
        let refused = if !self.wireless_enabled() {
            Some("the switch is off")
        } else if self.config.wireless_ports.is_none() || self.config.wireless.is_none() {
            Some("not available here")
        } else if self.active.is_some() {
            Some("a session runs")
        } else if self.usb_phone_answers().await {
            Some("an iPhone shares over USB")
        } else {
            None
        };
        if let Some(why) = refused {
            tracing::info!("wireless: not starting: {why}");
            return Err(CoreError::BadRequest);
        }
        let (Some(ports), Some(store)) = (
            self.config.wireless_ports.clone(),
            self.config.wireless.clone(),
        ) else {
            return Err(CoreError::BadRequest);
        };
        self.wireless.attempt += 1;
        self.wireless.phase = WirelessPhase::Starting;
        self.wireless.last_error = None;
        let context = wireless_session::Context {
            attempt: self.wireless.attempt,
            ports,
            store,
            timing: self.config.wireless_timing,
            events: events.clone(),
        };
        let computer_name = self.config.computer_name.clone();
        self.wireless.task = Some(wireless_session::Task::spawn(context, computer_name));
        tracing::info!("wireless: starting");
        Ok(())
    }

    /// The user's answer to the code shown. A match goes to the task, which waits for the
    /// phone's answer too and only then saves (the code stays shown, `confirmed`, until then;
    /// asking again is refused); a mismatch ends the attempt at once (the task,
    /// and with it the pairing channel, is gone before this returns; nothing was saved).
    async fn confirm_code(&mut self, matches: bool) -> Result<()> {
        if !matches!(self.wireless.phase, WirelessPhase::Pairing { .. }) {
            return Err(CoreError::BadRequest);
        }
        let Some(reply) = self.wireless.confirm.take() else {
            return Err(CoreError::BadRequest);
        };
        if !matches {
            tracing::info!("wireless: the code was not confirmed");
            drop(reply);
            self.end_wireless(WirelessPhase::Failed(WirelessFailure::PairingFailed))
                .await;
            return Ok(());
        }
        tracing::info!("wireless: the code was confirmed; waiting for the phone");
        let _ = reply.send(true);
        // The code stays shown until the phone answers (the task then moves the phase on).
        if let WirelessPhase::Pairing { confirmed, .. } = &mut self.wireless.phase {
            *confirmed = true;
        }
        Ok(())
    }

    /// Ends the wireless attempt, if one runs or failed, and leaves `then`: the task (and with
    /// it the access point) is gone before its session's tunnel is stopped and this returns.
    async fn end_wireless(&mut self, then: WirelessPhase) {
        if self.wireless.phase == WirelessPhase::Off && self.wireless.task.is_none() {
            return;
        }
        self.wireless.attempt += 1;
        if let Some(task) = self.wireless.task.take() {
            task.stop().await;
        }
        if let Some((_, _, probe)) = self.wireless.probing.take() {
            probe.abort();
        }
        // A question still on screen is withdrawn with the task (which held the other end).
        self.wireless.confirm = None;
        self.wireless.last_error = None;
        if self.active_is_wireless() {
            self.teardown().await;
        }
        self.wireless.phase = then;
        self.wireless.ssid = None;
    }

    /// What the wireless attempt's task asks or reports.
    async fn wireless_event(&mut self, event: WirelessEvent, events: &mpsc::Sender<Event>) {
        match event {
            WirelessEvent::Phase(phase) => {
                if self.wireless.phase.owns() {
                    if let WirelessPhase::Waiting { ssid, .. } = &phase {
                        self.wireless.ssid = Some(ssid.clone());
                    }
                    // The task has moved on from any question it asked.
                    self.wireless.confirm = None;
                    self.wireless.phase = phase;
                }
            }
            WirelessEvent::Establish { link, reply } => {
                if !self.wireless.phase.owns() || self.active.is_some() {
                    return;
                }
                match self.establish(Phone::wireless(link), events).await {
                    Ok(mut active) => {
                        let (ended, end) = tokio::sync::oneshot::channel();
                        active.ended = Some(ended);
                        self.begin_session(active);
                        if reply.send(Ok(end)).is_ok() {
                            self.wireless.phase = WirelessPhase::Connected;
                            self.wireless.last_error = None;
                            tracing::info!("wireless: connected");
                        } else {
                            // Nobody waits for this session any more.
                            self.teardown().await;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("wireless: connect failed: {error}");
                        let reason = error.reason();
                        let why = match error {
                            CoreError::Dial(DialError::UnknownIdentity) => {
                                NotEstablished::UnknownIdentity
                            }
                            // Over USB too this is the one failure that asks the user to act
                            // (update the app) rather than wait: no point dialling again.
                            _ if reason == Reason::PhoneTooOld => {
                                NotEstablished::Permanent(reason.clone())
                            }
                            _ => NotEstablished::Other,
                        };
                        self.wireless.last_error = Some(reason);
                        let _ = reply.send(Err(why));
                    }
                }
            }
            WirelessEvent::Confirm {
                code,
                phone_name,
                reply,
            } => {
                // Otherwise the reply is dropped: a "no" to a task that is going away anyway.
                if self.wireless.phase.owns() {
                    self.wireless.phase = WirelessPhase::Pairing {
                        code,
                        phone_name,
                        confirmed: false,
                    };
                    self.wireless.confirm = Some(reply);
                }
            }
            WirelessEvent::Done(failure) => {
                // The task has dropped the access point and is ending on its own.
                self.wireless.task = None;
                self.end_wireless(WirelessPhase::Failed(failure)).await;
            }
        }
    }

    /// The USB device a connection would use now, if there is exactly that one.
    fn selected_usb(&self) -> Option<Device> {
        match choose(&self.devices, self.chosen()) {
            Choice::Selected(device) => Some(device),
            Choice::Waiting | Choice::Choose => None,
        }
    }

    /// Whether the USB device a connection would use answers the phone app's hello (CellGlide
    /// sharing over the cable), asked here and now (bounded by the hello's own time limit).
    async fn usb_phone_answers(&mut self) -> bool {
        let Some(device) = self.selected_usb() else {
            return false;
        };
        self.wireless.probes.insert(
            device.serial.clone(),
            UsbProbe {
                last: Instant::now(),
                due: false,
            },
        );
        let phone = Phone::usb(self.config.endpoint.clone(), device);
        phone::read_hello(&phone).await.is_ok()
    }

    /// While a wireless attempt runs, asks the USB device a connection would use whether the
    /// phone app answers, in the background: once after it appears (attach or usbmuxd joined
    /// again), and at most once per `usb_probe_every`. An iPad, or an iPhone only charging, is
    /// left alone and the wireless session goes on.
    fn probe_usb(&mut self, events: &mpsc::Sender<Event>) {
        if !self.wireless.phase.owns()
            || self
                .wireless
                .probing
                .as_ref()
                .is_some_and(|(_, _, task)| !task.is_finished())
        {
            return;
        }
        let Some(device) = self.selected_usb() else {
            return;
        };
        let now = Instant::now();
        let every = self.config.wireless_timing.usb_probe_every;
        if let Some(probe) = self.wireless.probes.get(&device.serial)
            && (!probe.due || now < probe.last + every)
        {
            return;
        }
        self.wireless.probes.insert(
            device.serial.clone(),
            UsbProbe {
                last: now,
                due: false,
            },
        );
        let (serial, mux_id) = (device.serial.clone(), device.mux_id);
        let phone = Phone::usb(self.config.endpoint.clone(), device);
        let tx = events.clone();
        let task = tokio::spawn({
            let serial = serial.clone();
            async move {
                let answers = phone::read_hello(&phone).await.is_ok();
                let _ = tx.send(Event::UsbProbe(serial, mux_id, answers)).await;
            }
        });
        self.wireless.probing = Some((serial, mux_id, task));
    }

    /// A device appeared on USB (attached, or listed when usbmuxd was joined again): it may be
    /// asked again.
    fn usb_probe_due(&mut self, serial: &str) {
        if let Some(probe) = self.wireless.probes.get_mut(serial) {
            probe.due = true;
        }
    }

    /// An iPhone that shares over USB ends the wireless attempt and connects over USB, even
    /// with auto connect off: the user asked for a connection.
    async fn switch_to_usb(&mut self) {
        if !self.wireless.phase.owns() {
            return;
        }
        tracing::info!("wireless: an iPhone is on USB; switching to it");
        self.end_wireless(WirelessPhase::Off).await;
        self.settings.update(|s| s.user_disconnected = false);
        self.manual = true;
        self.failures = 0;
        self.last_error = None;
        self.retry_at = Some(Instant::now());
    }

    /// Reads the name and model of each attached phone that has not been asked yet. These
    /// come from lockdown without a session, so they work before the phone is trusted.
    fn describe_new_phones(&mut self, events: &mpsc::Sender<Event>) {
        for device in &self.devices {
            if self.labels.contains_key(&device.serial)
                || self.label_tasks.contains_key(&device.serial)
            {
                continue;
            }
            let phone = Phone::usb(self.config.endpoint.clone(), device.clone());
            let serial = device.serial.clone();
            let tx = events.clone();
            // Tagged, so a slow answer about an earlier plug-in of this serial is ignored.
            self.label_seq += 1;
            let seq = self.label_seq;
            self.label_tasks.insert(
                serial.clone(),
                (
                    seq,
                    tokio::spawn({
                        let serial = serial.clone();
                        async move {
                            let label = lockdown::describe(&phone).await;
                            let _ = tx.send(Event::Label(serial, seq, label)).await;
                        }
                    }),
                ),
            );
        }
    }

    fn forget_label(&mut self, serial: &str) {
        self.labels.remove(serial);
        if let Some((_, task)) = self.label_tasks.remove(serial) {
            task.abort();
        }
    }

    async fn mux_event(&mut self, event: Option<MuxEvent>, events: &mpsc::Sender<Event>) {
        match event {
            Some(MuxEvent::Attached(device)) => {
                // The device list changed: the next empty list is news again.
                self.mux_listed_none = false;
                if !self.devices.iter().any(|d| d.mux_id == device.mux_id) {
                    // A phone that is plugged in again is asked again.
                    self.forget_label(&device.serial);
                    self.monitor.usb(true, Timestamp::now());
                    self.usb_probe_due(&device.serial);
                    self.devices.push(device);
                    self.devices.sort_by(|a, b| a.serial.cmp(&b.serial));
                    self.describe_new_phones(events);
                }
                self.retry_soon();
            }
            Some(MuxEvent::Detached(id)) => {
                self.mux_listed_none = false;
                if let Some(gone) = self.devices.iter().find(|d| d.mux_id == id) {
                    let serial = gone.serial.clone();
                    self.forget_label(&serial);
                    self.monitor.usb(false, Timestamp::now());
                }
                self.devices.retain(|d| d.mux_id != id);
                if self
                    .active
                    .as_ref()
                    .and_then(|a| a.phone.usb_device())
                    .is_some_and(|d| d.mux_id == id)
                {
                    self.teardown().await;
                    self.phone_ended_session();
                    self.last_error = Some(Reason::PhoneGone);
                    // Another phone may be attached: let auto connect move to it.
                    self.retry_soon();
                }
            }
            Some(MuxEvent::Other) => {}
            None => {
                // usbmuxd went away: treat every phone as gone and listen again later.
                tracing::warn!("usbmuxd connection closed");
                self.mux_events = None;
                self.mux_listed_none = false;
                for _ in self.devices.drain(..) {
                    self.monitor.usb(false, Timestamp::now());
                }
                self.labels.clear();
                self.label_tasks
                    .drain()
                    .for_each(|(_, (_, task))| task.abort());
                // A wireless session does not depend on usbmuxd.
                if !self.active_is_wireless() {
                    self.teardown().await;
                }
                self.next_listen = Instant::now() + self.config.relisten_delay;
            }
        }
    }

    async fn session_event(&mut self, event: Event, events: &mpsc::Sender<Event>) {
        match event {
            Event::Ended(g) if g == self.generation && self.active.is_some() => {
                tracing::info!("phone session ended; rebuilding");
                let wireless = self.active_is_wireless();
                self.teardown().await;
                // A wireless session is rebuilt by its own task; USB's wishes stay as they were.
                if !wireless {
                    self.phone_ended_session();
                }
                self.retry_soon();
            }
            Event::DnsChanged(g) if g == self.generation && self.active.is_some() => {
                // The phone is still sharing: rebuild with the new DNS setting.
                tracing::info!("phone DNS setting changed; rebuilding");
                self.teardown().await;
                self.retry_soon();
            }
            Event::Metrics(g, m) if g == self.generation && self.active.is_some() => {
                let exit_ok = m.exit_ok;
                let at = std::time::Instant::now();
                self.feed.receive(&m, at);
                self.monitor
                    .phone_frame(&sanitize(&m), Timestamp::now(), at);
                if let Some(active) = self.active.as_mut() {
                    let spacing = active.health.early_spacing;
                    let now = Instant::now();
                    let fell = exit_ok == Some(false) && active.last_exit_ok != Some(false);
                    if fell
                        && active
                            .last_early
                            .is_none_or(|t| now.duration_since(t) >= spacing)
                    {
                        active.last_early = Some(now);
                        active.next_full = now;
                    }
                    active.last_exit_ok = exit_ok;
                }
            }
            Event::QuickCheck(g, ok) if g == self.generation => {
                if let Some(active) = self.active.as_mut() {
                    let fail_after = active.health.quick_fail_after;
                    active.quick_check_running = false;
                    if quick_check_counted(&mut active.quick_failures, ok, fail_after) {
                        active.next_full = Instant::now();
                    }
                }
            }
            Event::FullCheck(g, outcome) if g == self.generation && self.active.is_some() => {
                self.full_check_done(outcome).await;
            }
            Event::Usb(g, usb) if g == self.generation => {
                if let Some(active) = self.active.as_ref() {
                    self.usb = usb.map(|usb| *usb);
                    if let Some(report) = self.usb.as_ref().and_then(battery::report) {
                        active.reporter.send(report);
                    }
                }
            }
            Event::HealthChannel(g, rtt) if g == self.generation => {
                if let Some(active) = self.active.as_mut() {
                    active.channel_running = false;
                    // A greeting that went unanswered took at least the time limit.
                    let took = rtt.unwrap_or(active.watch.channel_timeout);
                    active.handshakes.push(took);
                    self.monitor.channel(rtt, Timestamp::now());
                }
            }
            Event::HealthTunnel(g, ok) if g == self.generation => {
                if let Some(active) = self.active.as_mut() {
                    active.tunnel_running = false;
                    self.monitor.tunnel(ok, Timestamp::now());
                    let watch = active.watch;
                    let wait = if !ok || self.monitor.wants_fast_tunnel() {
                        watch.fast
                    } else {
                        watch.tunnel
                    };
                    active.next_tunnel = Instant::now() + wait;
                }
            }
            Event::Label(serial, seq, label) => {
                let current = self
                    .label_tasks
                    .get(&serial)
                    .is_some_and(|(s, _)| *s == seq);
                if current {
                    self.label_tasks.remove(&serial);
                }
                if current && self.devices.iter().any(|d| d.serial == serial) {
                    self.labels.insert(serial, label);
                }
            }
            Event::Wireless(attempt, event) if attempt == self.wireless.attempt => {
                self.wireless_event(event, events).await;
            }
            Event::UsbProbe(serial, mux_id, answers) => {
                if self
                    .wireless
                    .probing
                    .as_ref()
                    .is_some_and(|(s, m, _)| *s == serial && *m == mux_id)
                {
                    self.wireless.probing = None;
                }
                // Still the device a connection would use, and the attempt still runs.
                if answers
                    && self
                        .selected_usb()
                        .is_some_and(|d| d.serial == serial && d.mux_id == mux_id)
                {
                    self.switch_to_usb().await;
                } else if !answers {
                    tracing::info!("wireless: the device on USB does not share; staying wireless");
                }
            }
            // Anything else belongs to a session (or a wireless attempt) that has already
            // been torn down.
            _ => {}
        }
    }

    async fn tick(&mut self, events: &mpsc::Sender<Event>) {
        let now = Instant::now();
        if self.mux_events.is_none() && now >= self.next_listen {
            self.relisten(events).await;
        }
        if self
            .active
            .as_ref()
            .and_then(|a| a.tunnel.as_ref())
            .is_some_and(|t| t.has_exited())
        {
            tracing::warn!("hev exited; tearing the tunnel down");
            let wireless = self.active_is_wireless();
            self.teardown().await;
            if !wireless {
                self.last_error = Some(Reason::ForwardingStopped);
            }
            self.retry_soon();
        }
        self.take_notices();
        self.health_checks(events);
        if let Some(task) = self.wireless.task.as_mut()
            && task.panicked().await
        {
            tracing::error!("wireless: the attempt's task failed");
            self.end_wireless(WirelessPhase::Failed(WirelessFailure::Lost))
                .await;
        }
        self.probe_usb(events);
        // While a wireless attempt runs, USB connects only through `switch_to_usb`.
        if self.wants()
            && self.active.is_none()
            && !self.wireless.phase.owns()
            && self.mux_events.is_some()
            && self.retry_at.is_some_and(|t| now >= t)
        {
            self.attempt(events).await;
        }
    }

    /// What the tunnel has had to say since the last look, into the timeline.
    fn take_notices(&mut self) {
        let Some(tunnel) = self.active.as_ref().and_then(|a| a.tunnel.as_ref()) else {
            return;
        };
        for notice in tunnel.take_notices() {
            let kind = match notice {
                TunnelNotice::BypassUnavailable => EventKind::BypassUnavailable,
                TunnelNotice::BypassUpdateFailed => EventKind::BypassUpdateFailed,
            };
            self.monitor.note(kind, None, Timestamp::now());
        }
    }

    /// Starts the checks that are due; their results come back as events.
    fn health_checks(&mut self, events: &mpsc::Sender<Event>) {
        let now = Instant::now();
        let generation = self.generation;
        let full = self
            .active
            .as_ref()
            .is_some_and(|a| !a.full_check_running && now >= a.next_full)
            .then(|| self.full_check());
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let (timing, watch) = (active.health, active.watch);
        if !active.quick_check_running && now >= active.next_quick {
            active.next_quick = now + timing.quick;
            active.quick_check_running = true;
            let port = active.checks.port();
            let tx = events.clone();
            active.tasks.spawn(async move {
                let ok = forwarder::socks_answers(port, timing.quick_timeout).await;
                let _ = tx.send(Event::QuickCheck(generation, ok)).await;
            });
        }
        if let Some(check) = full {
            active.full_check_running = true;
            // Provisional: a check asked for while this one runs moves it back to now.
            active.next_full = now + timing.full;
            let tx = events.clone();
            active.tasks.spawn(async move {
                let outcome = check.await;
                let _ = tx.send(Event::FullCheck(generation, outcome)).await;
            });
        }
        // The timeline's checks: only results come back to the actor.
        if !active.channel_running && now >= active.next_channel {
            self.monitor
                .frames_stale(std::time::Instant::now(), Timestamp::now());
            active.next_channel = now + watch.channel;
            active.channel_running = true;
            let port = active.checks.port();
            let tx = events.clone();
            active.tasks.spawn(async move {
                let rtt = forwarder::socks_rtt(port, watch.channel_timeout).await;
                let _ = tx.send(Event::HealthChannel(generation, rtt)).await;
            });
        }
        // Spec §3.6: every `fast` while there is a problem, from the moment it opens.
        if self.monitor.wants_fast_tunnel() {
            active.next_tunnel = active.next_tunnel.min(now + watch.fast);
        }
        if !active.tunnel_running && now >= active.next_tunnel {
            active.tunnel_running = true;
            let probe = self.config.tunnel_probe.clone();
            let tx = events.clone();
            active.tasks.spawn(async move {
                let ok = probe.check_within(watch.tunnel_timeout).await;
                let _ = tx.send(Event::HealthTunnel(generation, ok)).await;
            });
        }
    }

    async fn full_check_done(&mut self, outcome: FullCheck) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let timing = active.health;
        active.full_check_running = false;
        let now = Instant::now();
        match outcome {
            FullCheck::Ok => {
                active.next_full = active.next_full.min(now + timing.full);
                active.exit_retried = false;
            }
            FullCheck::NoExit if !active.exit_retried => {
                tracing::info!("exit check through the phone failed once; checking again");
                active.exit_retried = true;
                active.next_full = active.next_full.min(now + timing.retry);
                // Nothing got out through the phone: evidence against the tunnel too.
                self.monitor.tunnel(false, Timestamp::now());
            }
            FullCheck::SessionChanged => {
                tracing::info!("phone session changed; rebuilding");
                self.teardown().await;
                self.retry_soon();
            }
            FullCheck::NoExit => {
                // The second one fails the tunnel leg before the release ends the issue.
                self.monitor.tunnel(false, Timestamp::now());
                self.release(Reason::NoExit).await
            }
            FullCheck::Failed(reason) => self.release(reason).await,
        }
    }

    fn full_check(&self) -> impl Future<Output = FullCheck> + Send + use<> {
        // Copy what the awaits need: the actor holds a tunnel that is not Sync.
        let target = self
            .active
            .as_ref()
            .map(|a| (a.phone.clone(), a.hello.session_id.clone(), a.checks.port()));
        let probe = self.config.exit_probe.clone();
        async move {
            let Some((phone, session_id, port)) = target else {
                return FullCheck::Ok;
            };
            match phone::read_hello(&phone).await {
                Ok(hello) if hello.validate().is_err() => FullCheck::Failed(Reason::PhoneTooOld),
                Ok(hello) if hello.session_id != session_id => FullCheck::SessionChanged,
                Ok(_) if probe.check(port).await => FullCheck::Ok,
                Ok(_) => FullCheck::NoExit,
                Err(error) => FullCheck::Failed(error.reason()),
            }
        }
    }

    /// A failed check: back to the original network, retry with backoff.
    async fn release(&mut self, reason: Reason) {
        tracing::warn!("health check failed: {reason:?}; releasing the session");
        let wireless = self.active_is_wireless();
        self.teardown_because(Some(release_detail(&reason))).await;
        // The wireless attempt reconnects on its own clock (`wireless::policy`), not USB's.
        if wireless {
            return;
        }
        self.failures = (self.failures + 1).min(5);
        self.last_error = Some(reason);
        self.retry_at = Some(Instant::now() + backoff(self.failures));
    }

    async fn relisten(&mut self, session_events: &mpsc::Sender<Event>) {
        let listened = async {
            let events = mux::listen(&self.config.endpoint).await?;
            let devices = mux::list_devices(&self.config.endpoint).await?;
            Ok::<_, CoreError>((events, devices))
        }
        .await;
        match listened {
            Ok((events, devices)) => {
                self.mux_events = Some(events);
                // usbmuxd answered but sees no phone, which is not "not reachable".
                if devices.is_empty() && !self.mux_listed_none {
                    tracing::debug!("usbmuxd lists no iPhone");
                }
                self.mux_listed_none = devices.is_empty();
                // Phones already plugged in when usbmuxd is (re)joined.
                for device in &devices {
                    if !self.devices.iter().any(|d| d.mux_id == device.mux_id) {
                        self.monitor.usb(true, Timestamp::now());
                    }
                    if let Some(probe) = self.wireless.probes.get_mut(&device.serial) {
                        probe.due = true;
                    }
                }
                self.devices = devices;
                let serials: Vec<_> = self.devices.iter().map(|d| d.serial.clone()).collect();
                let stale: Vec<_> = self
                    .labels
                    .keys()
                    .chain(self.label_tasks.keys())
                    .filter(|serial| !serials.contains(serial))
                    .cloned()
                    .collect();
                stale.iter().for_each(|serial| self.forget_label(serial));
                self.describe_new_phones(session_events);
                self.mux_up = true;
                self.retry_soon();
            }
            Err(error) => {
                tracing::debug!("usbmuxd not reachable: {error}");
                self.mux_up = false;
                // The next empty list after this is news again.
                self.mux_listed_none = false;
                self.next_listen = Instant::now() + self.config.relisten_delay;
            }
        }
    }

    fn retry_soon(&mut self) {
        if self.wants() && self.active.is_none() {
            self.retry_at = Some(Instant::now());
        }
    }

    async fn attempt(&mut self, events: &mpsc::Sender<Event>) {
        let device = match choose(&self.devices, self.chosen()) {
            Choice::Selected(device) => device,
            Choice::Waiting | Choice::Choose => {
                self.retry_at = None; // an attach or a choice schedules the next try
                return;
            }
        };
        self.connecting = true;
        self.publish();
        let phone = Phone::usb(self.config.endpoint.clone(), device);
        let result = self.establish(phone, events).await;
        self.connecting = false;
        match result {
            Ok(mut active) => {
                self.start_cable_pairing(&mut active);
                self.begin_session(active);
                // A wireless failure shown until now is old news once USB is up.
                if matches!(self.wireless.phase, WirelessPhase::Failed(_)) {
                    self.wireless.phase = WirelessPhase::Off;
                }
                self.failures = 0;
                self.last_error = None;
                self.retry_at = None;
            }
            Err(error) => {
                tracing::warn!("connect failed: {error}");
                self.failures = (self.failures + 1).min(5);
                self.last_error = Some(error.reason());
                self.retry_at = Some(Instant::now() + backoff(self.failures));
            }
        }
    }

    /// What every new session does once established, USB or wireless: the timeline (on the
    /// session's tier), and the versions.
    fn begin_session(&mut self, active: Active) {
        let now = Timestamp::now();
        self.monitor.session_started(
            &active.hello.session_id,
            active.is_wireless(),
            active.watch,
            now,
        );
        if active.dns == DnsMode::Unmanaged {
            self.monitor.note(EventKind::DnsNotAuthorized, None, now);
        }
        if let StrictDns::Off { .. } = active.strict_dns {
            self.monitor
                .note(EventKind::StrictDnsUnavailable, None, now);
        }
        let stored = self.settings.get().last_versions.clone();
        let previous = stored.computer.zip(stored.phone);
        let (computer, phone) = self.monitor.versions(previous, &active.hello.version, now);
        self.settings.update(|s| {
            s.last_versions = LastVersions {
                computer: Some(computer),
                phone: Some(phone),
            }
        });
        self.active = Some(active);
    }

    /// Builds a session on `phone`, the same way for both links. A wireless one gets the
    /// wireless health tier, and none of what only USB has (link speed, lockdown battery
    /// readings, cable pairing).
    async fn establish(&mut self, phone: Phone, events: &mpsc::Sender<Event>) -> Result<Active> {
        let (health, watch) = if phone.is_wireless() {
            (
                self.config.wireless_health,
                self.config.wireless_health_watch,
            )
        } else {
            (self.config.health, self.config.health_watch)
        };
        let hello = phone::read_hello(&phone).await?;
        hello.validate()?;
        if !hello.has(capability::UDP_OVER_USB) {
            return Err(CoreError::PhoneTooOld);
        }
        self.generation += 1;
        let generation = self.generation;
        let mut tasks = TaskGuard(Vec::new());

        if hello.has(capability::SESSION_WATCH) {
            let link_speed = self
                .config
                .sysfs_root
                .as_deref()
                .zip(phone.usb_device())
                .and_then(|(root, device)| crate::usb_speed::link_speed(root, &device.serial));
            let ended: Ended = phone::watch(
                &phone,
                &hello.session_id,
                self.config.computer_name.as_deref(),
                link_speed,
            )
            .await?;
            let tx = events.clone();
            tasks.spawn(async move {
                ended.await;
                let _ = tx.send(Event::Ended(generation)).await;
            });
        }
        let forwarder = Forwarder::start(phone.clone()).await?;
        let checks = Forwarder::start(phone.clone()).await?;
        if !self.config.exit_probe.check(checks.port()).await {
            return Err(CoreError::NoExit);
        }
        let phone_dns = hello.dns_enabled() && hello.has(capability::DNS_FORWARD);
        // BeforeTunnel (Linux): resolver, then the whole tunnel. AfterInterface (Windows):
        // the interface and its addresses, then the resolver on one of them, then `finish`
        // sets the DNS. Either way `finish` runs once, and a failure after `start` stops
        // the tunnel before the error is returned.
        let order = self.config.backend.resolver_order();
        let (resolver_addr, deadline) = (self.config.resolver_addr, self.config.resolver_deadline);
        let start_resolver = || {
            let upstream = Arc::new(DohUpstream::new(phone.clone()));
            Resolver::start(resolver_addr, upstream, deadline)
        };
        let mut resolver = None;
        if phone_dns && order == ResolverOrder::BeforeTunnel {
            resolver = Some(start_resolver().await?);
        }
        let plan = TunnelPlan {
            socks_port: forwarder.port(),
            dns: match &resolver {
                Some(r) => DnsPlan::Resolver(r.local_addr()),
                None if phone_dns => DnsPlan::Resolver(resolver_addr),
                None => DnsPlan::Servers(FALLBACK_DNS.to_vec()),
            },
            bypass: self.config.bypass,
            strict_dns: self.settings.get().strict_dns,
        };
        let planned_dns = plan.dns.clone();
        let backend = self.config.backend.clone();
        let started = tokio::task::spawn_blocking(move || backend.start(&plan)).await;
        let tunnel = match started {
            Ok(Ok(tunnel)) => tunnel,
            Ok(Err(error)) => return Err(CoreError::Tunnel(error.0)),
            Err(join) => return Err(CoreError::Tunnel(join.to_string())),
        };
        if phone_dns && order == ResolverOrder::AfterInterface {
            match start_resolver().await {
                Ok(r) => resolver = Some(r),
                Err(error) => {
                    stop_tunnel(tunnel).await;
                    return Err(error);
                }
            }
        }
        let dns_plan = match &resolver {
            Some(r) => DnsPlan::Resolver(r.local_addr()),
            None => planned_dns,
        };
        let finished = tokio::task::spawn_blocking(move || {
            let result = tunnel.finish(&dns_plan);
            (tunnel, result)
        })
        .await;
        let tunnel = match finished {
            Ok((tunnel, Ok(()))) => tunnel,
            Ok((tunnel, Err(error))) => {
                stop_tunnel(tunnel).await;
                return Err(CoreError::Tunnel(error.0));
            }
            Err(join) => return Err(CoreError::Tunnel(join.to_string())),
        };

        let dns = if !tunnel.dns_managed() {
            DnsMode::Unmanaged
        } else if resolver.is_some() {
            DnsMode::Phone
        } else {
            DnsMode::Public
        };
        let strict_dns = tunnel.strict_dns();
        let (usb_tx, usb_rx) = mpsc::channel(4);
        tasks
            .0
            .push(tokio::spawn(battery::watch(phone.clone(), usb_tx)));
        tasks
            .0
            .push(spawn_usb_forward(usb_rx, generation, events.clone()));
        let reporter = battery::reporter_for(phone.clone());
        tasks
            .0
            .push(spawn_metrics(phone.clone(), generation, events.clone()));
        tasks.0.push(spawn_status(
            phone.clone(),
            generation,
            hello.dns_enabled(),
            events.clone(),
        ));
        self.feed.reset();
        self.usb = None;
        Ok(Active {
            tasks,
            reporter,
            phone,
            hello,
            dns,
            strict_dns,
            tunnel: Some(tunnel),
            forwarder,
            checks,
            _resolver: resolver,
            next_quick: Instant::now() + health.quick,
            quick_check_running: false,
            quick_failures: 0,
            next_full: Instant::now() + health.full,
            full_check_running: false,
            exit_retried: false,
            last_early: None,
            last_exit_ok: None,
            next_channel: Instant::now() + watch.channel,
            next_tunnel: Instant::now(),
            channel_running: false,
            tunnel_running: false,
            health,
            watch,
            handshakes: Handshakes::new(),
            ended: None,
        })
    }

    /// Wireless pairing over the cable, once for this session, in a task of its own that
    /// belongs to the session (it ends with it, so a phone that never answers is not left
    /// holding a connection). Nothing here can delay or fail the session: the result is only
    /// logged, and never the key or the reply.
    fn start_cable_pairing(&self, active: &mut Active) {
        let Some(store) = self.config.wireless.clone() else {
            return;
        };
        let enabled = self.settings.get().wireless_enabled || self.config.wireless_forced;
        // Cable pairing is for the USB link only.
        let Some(device) = active.phone.usb_device() else {
            return;
        };
        let serial = device.serial.clone();
        let phone = active.phone.clone();
        let hello = active.hello.clone();
        let name = crate::wireless::computer_name(self.config.computer_name.as_deref());
        active.tasks.spawn(async move {
            match cable_pairing::pair_if_needed(&phone, &hello, &serial, enabled, &name, &store)
                .await
            {
                Ok(cable_pairing::Outcome::Paired(_)) => {
                    tracing::info!("wireless: paired over the cable");
                }
                Ok(cable_pairing::Outcome::AlreadyPaired) => {
                    tracing::info!("wireless: already paired");
                }
                Ok(cable_pairing::Outcome::KnownButNoKey) => {
                    tracing::warn!(
                        "wireless: the phone has a pairing this computer lacks; remove it on \
                         the phone to pair again"
                    );
                }
                Ok(cable_pairing::Outcome::Skipped) => {
                    tracing::info!("wireless: cable pairing skipped");
                }
                Err(error) => tracing::warn!("wireless: cable pairing failed: {error}"),
            }
        });
    }

    /// The strict DNS switch while connected: the running tunnel turns its rules on or off
    /// (on a blocking thread) and the new state is published with the next snapshot. Turning
    /// it on and failing is logged like a connection that couldn't turn it on; turning it
    /// off logs nothing.
    async fn apply_strict_dns(&mut self, on: bool) {
        let Some(tunnel) = self.active.as_mut().and_then(|a| a.tunnel.take()) else {
            return;
        };
        let applied = tokio::task::spawn_blocking(move || {
            let state = tunnel.apply_strict_dns(on);
            (tunnel, state)
        })
        .await;
        let Some(active) = self.active.as_mut() else {
            return;
        };
        match applied {
            Ok((tunnel, state)) => {
                active.tunnel = Some(tunnel);
                if on && matches!(state, StrictDns::Off { .. }) {
                    self.monitor
                        .note(EventKind::StrictDnsUnavailable, None, Timestamp::now());
                }
                active.strict_dns = state;
            }
            // The tunnel went with the panicked thread (its `Drop` cleans up): the rest of
            // the session goes too, and the connection is tried again like after hev exits.
            Err(error) => {
                tracing::error!("switching strict DNS: {error}; tearing the tunnel down");
                self.teardown().await;
                self.last_error = Some(Reason::ForwardingStopped);
                self.retry_soon();
            }
        }
    }

    /// Network first: the tunnel goes before the relays it depends on.
    async fn teardown(&mut self) {
        self.teardown_because(None).await;
    }

    /// `detail` goes on the timeline's `tunnelStopped`, e.g. why a check released it.
    async fn teardown_because(&mut self, detail: Option<String>) {
        self.take_notices();
        let Some(mut active) = self.active.take() else {
            return;
        };
        self.monitor.tunnel_stopped(detail, Timestamp::now());
        if let Some(tunnel) = active.tunnel.take() {
            stop_tunnel(tunnel).await;
        }
        // A wireless session that ends on its own is brought back by its task, which learns
        // of the end from this drop, after the tunnel is down.
        if active.is_wireless() && self.wireless.phase == WirelessPhase::Connected {
            self.wireless.phase = WirelessPhase::Reconnecting {
                since: Instant::now(),
                name_clash: false,
            };
        }
        drop(active);
        self.feed.reset();
        self.usb = None;
    }

    fn state(&self) -> ConnState {
        // A wireless attempt does not depend on usbmuxd: whatever usbmuxd does, the state is
        // the attempt's.
        if self.wireless.phase.owns() {
            return match &self.active {
                Some(active) => ConnState::Connected {
                    phone_version: active.hello.version.clone(),
                    dns: active.dns,
                },
                None => ConnState::Connecting,
            };
        }
        if !self.mux_up {
            return ConnState::NoUsbService;
        }
        if let Some(active) = &self.active {
            return ConnState::Connected {
                phone_version: active.hello.version.clone(),
                dns: active.dns,
            };
        }
        if self.connecting {
            return ConnState::Connecting;
        }
        if self.devices.is_empty() {
            return ConnState::NoPhone;
        }
        if self.wants() {
            if let Choice::Choose = choose(&self.devices, self.chosen()) {
                return ConnState::ChoosePhone;
            }
            if self.chosen().is_some()
                && matches!(choose(&self.devices, self.chosen()), Choice::Waiting)
            {
                return ConnState::Retrying {
                    reason: Reason::ChosenPhoneMissing,
                };
            }
            return match &self.last_error {
                Some(reason) => ConnState::Retrying {
                    reason: reason.clone(),
                },
                None => ConnState::Connecting,
            };
        }
        ConnState::Idle
    }

    /// The wireless attempt for the snapshot; never the passphrase.
    fn wireless_status(&self) -> Option<WirelessStatus> {
        let phase = &self.wireless.phase;
        Some(WirelessStatus {
            phase: phase.view()?,
            ssid: self.wireless.ssid.clone().filter(|_| phase.owns()),
            weak: self
                .active
                .as_ref()
                .is_some_and(|a| a.is_wireless() && a.weak()),
            name_clash: phase.name_clash(),
            last_error: self.wireless.last_error.clone().filter(|_| phase.owns()),
            hint: phase.hint(),
        })
    }

    fn publish(&self) {
        let stored = self.settings.get();
        let next = Snapshot {
            state: self.state(),
            devices: self.devices.iter().map(|d| d.serial.clone()).collect(),
            selected: self
                .active
                .as_ref()
                .map(|a| a.phone.id())
                .or_else(|| stored.chosen_serial.clone()),
            wants_connection: self.wants(),
            phone: self.feed.card(std::time::Instant::now(), self.usb.as_ref()),
            labels: self.labels.clone(),
            health: self.monitor.status(),
            socks_port: self
                .active
                .as_ref()
                .filter(|a| a.tunnel.is_some())
                .map(|a| a.forwarder.port()),
            strict_dns: self
                .active
                .as_ref()
                .map(|a| a.strict_dns.clone())
                .unwrap_or_default(),
            settings: cg_ipc::Settings {
                auto_connect: stored.auto_connect,
                chosen_serial: stored.chosen_serial.clone(),
                strict_dns: stored.strict_dns,
            },
            wireless: self.wireless_status(),
        };
        self.snapshot.send_if_modified(|current| {
            let changed = *current != next;
            *current = next;
            changed
        });
    }
}

/// `today` and the six days before it, oldest first.
fn last_seven(today: Date) -> Vec<Date> {
    (0..7)
        .rev()
        .filter_map(|back| today.checked_sub(back.days()).ok())
        .collect()
}

/// The `tunnelStopped` detail when a check releases the session: a short name per reason.
fn release_detail(reason: &Reason) -> String {
    let name = match reason {
        Reason::NoUsbService => "noUsbService",
        Reason::PhoneNotReady => "phoneNotReady",
        Reason::PhoneTooOld => "phoneTooOld",
        Reason::SessionChanged => "sessionChanged",
        Reason::NoExit => "noExit",
        Reason::PhoneGone => "phoneGone",
        Reason::ResolverPortBusy { .. } => "resolverPortBusy",
        Reason::ChosenPhoneMissing => "chosenPhoneMissing",
        Reason::ForwardingStopped => "forwardingStopped",
        Reason::Timeout => "timeout",
        Reason::Tunnel { .. } => "tunnel",
        Reason::Internal { .. } => "internal",
        Reason::NotInGroup => "notInGroup",
        Reason::NotAllowed => "notAllowed",
        Reason::BadRequest => "badRequest",
        Reason::ShuttingDown => "shuttingDown",
        Reason::BadDay => "badDay",
    };
    format!("released {name}")
}

fn history_day(
    log: Option<&HealthLog>,
    tz: &TimeZone,
    day: Option<Date>,
    now: Timestamp,
) -> HistoryDay {
    let today = now.to_zoned(tz.clone()).date();
    let day = day.unwrap_or(today);
    let midnight = |d: Date| d.to_zoned(tz.clone()).ok().map(|z| z.timestamp());
    let begin = midnight(day).unwrap_or(now);
    let next = day.tomorrow().ok().and_then(midnight).unwrap_or(now);
    let (events, start) = match log {
        Some(log) => (log.events_on(day), log.start_of(day)),
        None => (Vec::new(), DayStart::default()),
    };
    HistoryDay {
        day: day.to_string(),
        start,
        events,
        days: last_seven(today).iter().map(Date::to_string).collect(),
        begin,
        end: next.min(now).max(begin),
    }
}

/// Undoes the tunnel on a blocking thread; a failure is logged, there is nothing else to do.
async fn stop_tunnel(tunnel: Box<dyn RunningTunnel>) {
    match tokio::task::spawn_blocking(move || tunnel.stop()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("stopping tunnel: {error}"),
        Err(error) => tracing::error!("stopping tunnel: {error}"),
    }
}

fn spawn_usb_forward(
    mut readings: mpsc::Receiver<Option<UsbPhone>>,
    generation: u64,
    events: mpsc::Sender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(usb) = readings.recv().await {
            if events
                .send(Event::Usb(generation, usb.map(Box::new)))
                .await
                .is_err()
            {
                return;
            }
        }
    })
}

fn spawn_metrics(phone: Phone, generation: u64, events: mpsc::Sender<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (tx, mut rx) = mpsc::channel(4);
            let forward = async {
                while let Some(metrics) = rx.recv().await {
                    if events
                        .send(Event::Metrics(generation, Box::new(metrics)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            };
            // Both stop together when the session's tasks are aborted.
            tokio::select! {
                _ = phone::metrics_stream(phone.clone(), tx) => {}
                _ = forward => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn spawn_status(
    phone: Phone,
    generation: u64,
    dns_enabled: bool,
    events: mpsc::Sender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<cg_protocol::phone::SharingStatus>(4);
        let watch_dns = async {
            while let Some(status) = rx.recv().await {
                if status.dns_enabled != dns_enabled {
                    let _ = events.send(Event::DnsChanged(generation)).await;
                    return;
                }
            }
        };
        tokio::select! {
            _ = phone::status_stream(phone, tx) => {}
            _ = watch_dns => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Stored;
    use crate::testing::{
        FakeBackend, FakeMux, FakePhone, FakePhoneOptions, FakeProbe, FakeTunnelProbe, SESSION_A,
        SESSION_B, SharedFake, SharedProbe, device, hello_json,
    };
    use cg_ipc::{ConnState, DnsMode, PhoneChoice};

    struct Rig {
        mux: FakeMux,
        phone: FakePhone,
        backend: Arc<FakeBackend>,
        controller: Controller,
        probe: Arc<FakeProbe>,
        tunnel_probe: Arc<FakeTunnelProbe>,
    }

    fn quiet_timing() -> HealthTiming {
        // Health checks stay out of the way of tests that are about something else.
        HealthTiming {
            quick: Duration::from_secs(30),
            quick_timeout: Duration::from_millis(500),
            full: Duration::from_secs(30),
            retry: Duration::from_secs(5),
            early_spacing: Duration::from_millis(300),
            quick_fail_after: 2,
        }
    }

    fn fast_timing() -> HealthTiming {
        HealthTiming {
            quick: Duration::from_millis(100),
            quick_timeout: Duration::from_millis(300),
            full: Duration::from_millis(300),
            retry: Duration::from_millis(400),
            early_spacing: Duration::from_millis(300),
            quick_fail_after: 2,
        }
    }

    /// Starts with auto connect off: tests press Connect themselves.
    async fn rig(options: FakePhoneOptions, resolver_addr: SocketAddr) -> Rig {
        rig_with(options, resolver_addr, false, quiet_timing()).await
    }

    async fn rig_auto(options: FakePhoneOptions, resolver_addr: SocketAddr) -> Rig {
        rig_with(options, resolver_addr, true, quiet_timing()).await
    }

    async fn rig_with(
        options: FakePhoneOptions,
        resolver_addr: SocketAddr,
        auto: bool,
        timing: HealthTiming,
    ) -> Rig {
        rig_config(options, resolver_addr, timing, |config| {
            config.settings = SettingsSource::Memory(Stored {
                auto_connect: auto,
                ..Stored::default()
            });
        })
        .await
    }

    /// A rig whose config the caller adjusts last.
    async fn rig_config(
        options: FakePhoneOptions,
        resolver_addr: SocketAddr,
        timing: HealthTiming,
        adjust: impl FnOnce(&mut ControllerConfig),
    ) -> Rig {
        rig_backend(options, resolver_addr, timing, FakeBackend::new(), adjust).await
    }

    /// A rig on the given backend, auto connect off.
    async fn rig_on(
        backend: Arc<FakeBackend>,
        options: FakePhoneOptions,
        resolver_addr: SocketAddr,
    ) -> Rig {
        rig_backend(options, resolver_addr, quiet_timing(), backend, |config| {
            config.settings = SettingsSource::Memory(Stored {
                auto_connect: false,
                ..Stored::default()
            });
        })
        .await
    }

    async fn rig_backend(
        options: FakePhoneOptions,
        resolver_addr: SocketAddr,
        timing: HealthTiming,
        backend: Arc<FakeBackend>,
        adjust: impl FnOnce(&mut ControllerConfig),
    ) -> Rig {
        let probe = FakeProbe::new();
        let tunnel_probe = FakeTunnelProbe::new();
        let mux = FakeMux::start().await;
        let phone = FakePhone::start(&mux, options).await;
        let mut config =
            ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(backend.clone())));
        config.resolver_addr = resolver_addr;
        config.relisten_delay = Duration::from_millis(200);
        config.health = timing;
        config.exit_probe = Arc::new(SharedProbe(probe.clone()));
        // Never the real captive check from a unit test.
        config.tunnel_probe = tunnel_probe.clone();
        adjust(&mut config);
        let controller = Controller::spawn(config);
        Rig {
            mux,
            phone,
            backend,
            controller,
            probe,
            tunnel_probe,
        }
    }

    fn any_port() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    /// A fixed resolver address no other socket can hold: a loopback address of its own
    /// (Linux and Windows answer on all of 127/8), unique within this test process, on a
    /// port below both systems' ephemeral ranges. Other sockets bind 127.0.0.1 or
    /// ephemeral ports, so nothing can take it between picking it and the resolver
    /// binding it, unlike a probed-then-released port.
    #[cfg(any(target_os = "linux", windows))]
    fn private_resolver_addr() -> SocketAddr {
        use std::sync::atomic::{AtomicU16, Ordering};
        static NEXT: AtomicU16 = AtomicU16::new(1);
        let [hi, lo] = NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes();
        SocketAddr::from(([127, 77, hi, lo], 25353))
    }

    /// Elsewhere (macOS has only 127.0.0.1 without aliases): a probed free port, which
    /// another socket may take before the resolver binds it.
    #[cfg(not(any(target_os = "linux", windows)))]
    fn private_resolver_addr() -> SocketAddr {
        loop {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let addr = udp.local_addr().unwrap();
            if std::net::TcpListener::bind(addr).is_ok() {
                return addr;
            }
        }
    }

    async fn connect_and_wait(r: &Rig, what: &str, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, what, pred).await
    }

    fn retrying(s: &Snapshot) -> bool {
        matches!(s.state, ConnState::Retrying { .. })
    }

    #[tokio::test]
    async fn after_interface_starts_resolver_between_start_and_finish() {
        let addr = private_resolver_addr();
        let backend = FakeBackend::with_order(ResolverOrder::AfterInterface);
        let r = rig_on(backend, FakePhoneOptions::default(), addr).await;
        let snap = connect_and_wait(&r, "connected", connected).await;
        assert!(matches!(
            snap.state,
            ConnState::Connected {
                dns: DnsMode::Phone,
                ..
            }
        ));
        assert_eq!(r.backend.started(), 1);
        assert_eq!(r.backend.last_plan().unwrap().dns, DnsPlan::Resolver(addr));
        assert_eq!(
            r.backend.listening_at_start(),
            Some(false),
            "the interface comes up before the resolver"
        );
        let finish = r.backend.last_finish().unwrap();
        assert_eq!(finish.dns, DnsPlan::Resolver(addr));
        assert!(finish.resolver_listening, "resolver listens before finish");
        assert_eq!(r.backend.finished(), 1);
        assert_eq!(r.backend.stopped(), 0);
    }

    #[tokio::test]
    async fn after_interface_rolls_back_when_resolver_cannot_bind() {
        let taken = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = taken.local_addr().unwrap();
        let backend = FakeBackend::with_order(ResolverOrder::AfterInterface);
        let r = rig_on(backend, FakePhoneOptions::default(), addr).await;
        let snap = connect_and_wait(&r, "retrying", retrying).await;
        assert_eq!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::ResolverPortBusy { port: addr.port() }
            }
        );
        assert_eq!(r.backend.started(), 1);
        assert_eq!(r.backend.stopped(), 1);
        assert_eq!(r.backend.finished(), 0);
    }

    #[tokio::test]
    async fn after_interface_rolls_back_when_finish_fails() {
        let backend = FakeBackend::with_order(ResolverOrder::AfterInterface);
        backend.set_finish_fails(true);
        let r = rig_on(
            backend,
            FakePhoneOptions::default(),
            private_resolver_addr(),
        )
        .await;
        let snap = connect_and_wait(&r, "retrying", retrying).await;
        assert!(matches!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::Tunnel { .. }
            }
        ));
        assert_eq!(r.backend.finished(), 1);
        assert_eq!(r.backend.stopped(), 1);
    }

    #[tokio::test]
    async fn before_tunnel_order_is_unchanged() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        connect_and_wait(&r, "connected", connected).await;
        let plan = r.backend.last_plan().unwrap();
        assert!(matches!(plan.dns, DnsPlan::Resolver(addr) if addr.port() != 0));
        assert_eq!(
            r.backend.listening_at_start(),
            Some(true),
            "the resolver listens before the tunnel starts"
        );
        assert_eq!(r.backend.finished(), 1);
        let finish = r.backend.last_finish().unwrap();
        assert_eq!(finish.dns, plan.dns);
        assert!(finish.resolver_listening);
    }

    async fn wait_for(c: &Controller, what: &str, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let mut rx = c.subscribe();
        let found = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let snap = rx.borrow_and_update().clone();
                if pred(&snap) {
                    return snap;
                }
                rx.changed().await.unwrap();
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("timed out waiting for {what}; last: {:?}", c.snapshot()))
    }

    fn connected(s: &Snapshot) -> bool {
        matches!(s.state, ConnState::Connected { .. })
    }

    #[tokio::test]
    async fn watch_reports_link_speed() {
        let sysfs = tempfile::tempdir().unwrap();
        let dir = sysfs.path().join("sys/bus/usb/devices/1-2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("serial"), "AAAA\n").unwrap();
        std::fs::write(dir.join("speed"), "480\n").unwrap();
        let mux = FakeMux::start().await;
        let phone = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let backend = FakeBackend::new();
        let mut config = ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(backend)));
        config.resolver_addr = any_port();
        config.health = quiet_timing();
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.tunnel_probe = FakeTunnelProbe::new();
        config.sysfs_root = Some(sysfs.path().to_path_buf());
        let controller = Controller::spawn(config);
        mux.attach(device("AAAA", 1));
        wait_for(&controller, "connected", connected).await;
        let head = phone.last_watch_head().unwrap();
        assert!(
            head.contains("X-CellGlide-Link: usb; speed=480000000\r\n"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn connects_one_phone() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(
            snap.state,
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone
            }
        );
        assert_eq!(snap.devices, ["aaaa"]);
        assert_eq!(r.backend.started(), 1);
        let plan = r.backend.last_plan().unwrap();
        assert!(matches!(plan.dns, DnsPlan::Resolver(addr) if addr.port() != 0));
        assert_ne!(plan.socks_port, 0);
    }

    #[tokio::test]
    async fn snapshot_names_the_socks_port_only_while_connected() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        let idle = wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        assert_eq!(idle.socks_port, None);
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        let port = snap.socks_port.expect("port while connected");
        assert_ne!(port, 0);
        assert_eq!(port, r.backend.last_plan().unwrap().socks_port);
        r.controller.send(Command::Disconnect).await.unwrap();
        let snap = wait_for(&r.controller, "disconnected", |s| !connected(s)).await;
        assert_eq!(snap.socks_port, None);
    }

    #[tokio::test]
    async fn session_end_tears_down_and_reconnects() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.phone.end_session();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
    }

    #[tokio::test]
    async fn detach_tears_down() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.mux.detach(1);
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        assert_eq!(r.backend.stopped(), 1);
    }

    #[tokio::test]
    async fn detach_moves_auto_connect_to_the_remaining_phone() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        r.mux.attach(device("bbbb", 2));
        r.mux.detach(1);
        wait_for(&r.controller, "moved to the other phone", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
    }

    #[tokio::test]
    async fn auto_off_after_a_health_release_keeps_the_session_wanted() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        r.probe.set_ok(false);
        wait_for(&r.controller, "released", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        r.controller
            .send(Command::SetSettings {
                auto_connect: Some(false),
                phone: None,
                strict_dns: None,
            })
            .await
            .unwrap();
        assert!(r.controller.snapshot().wants_connection);
    }

    #[tokio::test]
    async fn full_check_rejects_an_invalid_hello() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        let mut hello = hello_json(&["socks5-tcp"], true, SESSION_A);
        hello["service"] = "Other".into();
        r.phone.set_hello(hello);
        let snap = wait_for(&r.controller, "released", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert_eq!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::PhoneTooOld
            }
        );
    }

    #[tokio::test]
    async fn disconnect_stops_and_stays_idle() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| {
            s.state == ConnState::Idle && !s.wants_connection
        })
        .await;
        assert_eq!(r.backend.stopped(), 1);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 1);
    }

    #[tokio::test]
    async fn old_phone_is_told_to_update() {
        let options = FakePhoneOptions {
            hello: hello_json(&["socks5-tcp", "session-watch-v1"], true, SESSION_A),
            ..FakePhoneOptions::default()
        };
        let r = rig(options, any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert!(matches!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::PhoneTooOld
            }
        ));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn dns_unauthorized_keeps_tunnel_without_dns() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.backend.set_dns_unmanaged(true);
        r.mux.attach(device("aaaa", 1));
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(
            snap.state,
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Unmanaged
            }
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 1, "no retry loop");
        assert!(connected(&r.controller.snapshot()));
    }

    #[tokio::test]
    async fn phone_dns_off_uses_public_servers() {
        let caps = [
            "socks5-tcp",
            "udp-over-usb-v1",
            "dns-forward-v1",
            "session-watch-v1",
        ];
        let options = FakePhoneOptions {
            hello: hello_json(&caps, false, SESSION_A),
            ..FakePhoneOptions::default()
        };
        let r = rig(options, any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(
            snap.state,
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Public
            }
        );
        assert_eq!(
            r.backend.last_plan().unwrap().dns,
            DnsPlan::Servers(FALLBACK_DNS.to_vec())
        );
    }

    #[tokio::test]
    async fn no_exit_does_not_start_tunnel() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.probe.set_ok(false);
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert!(matches!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::NoExit
            }
        ));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn two_phones_require_choice() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.mux.attach(device("bbbb", 2));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "choose", |s| {
            s.state == ConnState::ChoosePhone
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 0);
        r.controller
            .send(Command::Connect {
                serial: Some("bbbb".into()),
            })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.selected.as_deref(), Some("bbbb"));
    }

    #[tokio::test]
    async fn resolver_port_busy_fails_cleanly() {
        let taken = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let r = rig(FakePhoneOptions::default(), taken.local_addr().unwrap()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert!(matches!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::ResolverPortBusy { .. }
            }
        ));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn usbmuxd_restart_tears_down_and_relistens() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.mux.drop_listeners();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
    }

    /// An empty device list is logged apart from "not reachable", once per change: not on
    /// every relisten, but again after a phone came and went and usbmuxd restarted.
    #[tokio::test]
    async fn an_empty_device_list_is_logged_once_per_change() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _log = tracing::subscriber::set_default(subscriber);
        let message = "usbmuxd lists no iPhone";
        let logged = |count: usize| {
            let captured = captured.clone();
            async move {
                for _ in 0..80 {
                    if captured.lines("DEBUG", message) >= count {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                panic!("{message} logged fewer than {count} times");
            }
        };

        let r = rig(FakePhoneOptions::default(), any_port()).await;
        logged(1).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.mux.detach(1);
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        // usbmuxd restarts while no phone is plugged in: the list is empty again.
        r.mux.drop_listeners();
        logged(2).await;
        assert_eq!(captured.lines("DEBUG", message), 2);
        assert_eq!(captured.lines("DEBUG", "usbmuxd not reachable"), 0);
    }

    #[tokio::test]
    async fn hev_exit_tears_down_and_reconnects() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.backend.exit_running();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
    }

    #[test]
    fn before_the_first_look_usbmuxd_is_unknown_on_windows_only() {
        let first = first_snapshot();
        if cfg!(windows) {
            assert_eq!(first.state, ConnState::NoUsbService);
        } else {
            assert_eq!(first, Snapshot::default());
        }
        assert!(first.devices.is_empty());
    }

    #[tokio::test]
    async fn missing_usbmuxd_is_reported() {
        let backend = FakeBackend::new();
        let mut config =
            ControllerConfig::new(crate::testing::missing_mux(), Arc::new(SharedFake(backend)));
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.health = quiet_timing();
        let controller = Controller::spawn(config);
        wait_for(&controller, "no usb service", |s| {
            s.state == ConnState::NoUsbService
        })
        .await;
    }

    #[tokio::test]
    async fn metrics_reach_the_snapshot() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        r.phone.send_metrics(
            r#"{"service":"CellGlide-metrics","version":1,"carrier":"中国移动","battery":0.6}"#,
        );
        let card = wait_for(&r.controller, "card", |s| {
            s.phone.as_ref().is_some_and(|c| c.frame.is_some())
        })
        .await
        .phone
        .unwrap();
        let m = card.frame.unwrap();
        assert_eq!(m.carrier.as_deref(), Some("中国移动"));
        assert_eq!(m.battery, Some(0.6));
    }

    #[tokio::test]
    async fn disconnect_forgets_chosen_phone() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.mux.attach(device("bbbb", 2));
        r.controller
            .send(Command::Connect {
                serial: Some("bbbb".into()),
            })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.selected.as_deref(), Some("bbbb"));
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.mux.detach(2);
        wait_for(&r.controller, "one phone", |s| s.devices == ["aaaa"]).await;
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.selected.as_deref(), Some("aaaa"));
    }

    #[tokio::test]
    async fn chosen_phone_missing_is_reported() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect {
                serial: Some("zzzz".into()),
            })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert!(matches!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::ChosenPhoneMissing
            }
        ));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn metrics_are_cleared_on_disconnect_and_stay_cleared() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let line =
            r#"{"service":"CellGlide-metrics","version":1,"carrier":"中国移动","battery":0.6}"#;
        r.phone.send_metrics(line);
        wait_for(&r.controller, "card", |s| {
            s.phone.as_ref().is_some_and(|c| c.frame.is_some())
        })
        .await;
        r.phone.send_metrics(line);
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(r.controller.snapshot().phone.is_none());
    }

    #[tokio::test]
    async fn auto_connect_connects_without_command() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert!(snap.wants_connection);
        assert!(snap.settings.auto_connect);
    }

    #[tokio::test]
    async fn disconnect_replies_after_teardown() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        r.controller.send(Command::Disconnect).await.unwrap();
        assert_eq!(
            r.backend.stopped(),
            1,
            "the reply comes after the tunnel is down"
        );
        let snap = r.controller.snapshot();
        assert!(!snap.wants_connection);
        assert_eq!(
            snap.state,
            ConnState::Idle,
            "the snapshot is published before the reply"
        );
    }

    #[tokio::test]
    async fn auto_off_with_no_phone_waits_for_connect() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        // The first listen schedules a try, which the next tick (250 ms) drops for want of a
        // phone; until then a try is in progress and auto off keeps it wanted. (Before the
        // first listen published anything, the default snapshot already said NoPhone.)
        tokio::time::sleep(Duration::from_millis(600)).await;
        r.controller
            .send(Command::SetSettings {
                auto_connect: Some(false),
                phone: None,
                strict_dns: None,
            })
            .await
            .unwrap();
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 0);
        assert!(!r.controller.snapshot().wants_connection);
    }

    #[tokio::test]
    async fn auto_off_keeps_the_running_session_wanted() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        r.controller
            .send(Command::SetSettings {
                auto_connect: Some(false),
                phone: None,
                strict_dns: None,
            })
            .await
            .unwrap();
        let snap = r.controller.snapshot();
        assert!(snap.wants_connection);
        assert!(connected(&snap));
        r.backend.exit_running();
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
    }

    #[tokio::test]
    async fn disconnect_is_remembered_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let mux = FakeMux::start().await;
        let _phone = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        mux.attach(device("aaaa", 1));
        let spawn = |backend: Arc<FakeBackend>| {
            let mut config = ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(backend)));
            config.resolver_addr = any_port();
            config.relisten_delay = Duration::from_millis(200);
            config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
            config.health = quiet_timing();
            config.settings = SettingsSource::File(file.clone());
            Controller::spawn(config)
        };
        let first = FakeBackend::new();
        let c1 = spawn(first.clone());
        wait_for(&c1, "connected", connected).await;
        c1.send(Command::Disconnect).await.unwrap();
        c1.shutdown().await;

        let second = FakeBackend::new();
        let c2 = spawn(second.clone());
        wait_for(&c2, "idle", |s| s.state == ConnState::Idle).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            second.started(),
            0,
            "a remembered disconnect keeps auto connect off"
        );
        c2.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&c2, "connected again", connected).await;
    }

    #[tokio::test]
    async fn auto_off_forgets_after_unplug() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.mux.detach(1);
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        r.mux.attach(device("aaaa", 2));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 1);
    }

    #[tokio::test]
    async fn set_settings_switches_phone_and_auto() {
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.mux.attach(device("bbbb", 2));
        wait_for(&r.controller, "choose", |s| {
            s.state == ConnState::ChoosePhone
        })
        .await;
        r.controller
            .send(Command::SetSettings {
                auto_connect: None,
                phone: Some(PhoneChoice::Serial {
                    serial: "bbbb".into(),
                }),
                strict_dns: None,
            })
            .await
            .unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.selected.as_deref(), Some("bbbb"));
        assert_eq!(snap.settings.chosen_serial.as_deref(), Some("bbbb"));
        r.controller
            .send(Command::SetSettings {
                auto_connect: Some(false),
                phone: None,
                strict_dns: None,
            })
            .await
            .unwrap();
        assert!(!r.controller.snapshot().settings.auto_connect);
    }

    #[tokio::test]
    async fn exit_failure_once_is_retried() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        let before = r.probe.calls();
        r.probe.set_ok(false);
        // Polled: a failed first check changes no snapshot.
        tokio::time::timeout(Duration::from_secs(3), async {
            while r.probe.calls() == before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a full check runs");
        r.probe.set_ok(true);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(connected(&r.controller.snapshot()));
        assert_eq!(r.backend.stopped(), 0);
    }

    #[tokio::test]
    async fn exit_failure_twice_releases_session() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        r.probe.set_ok(false);
        let snap = wait_for(&r.controller, "released", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        assert_eq!(
            snap.state,
            ConnState::Retrying {
                reason: Reason::NoExit
            }
        );
        assert_eq!(r.backend.stopped(), 1);
        assert_eq!(
            r.backend.started(),
            1,
            "reconnects keep failing the exit check"
        );
    }

    #[tokio::test]
    async fn quick_failures_force_a_full_check() {
        let timing = HealthTiming {
            full: Duration::from_secs(30),
            ..fast_timing()
        };
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, timing).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        let before = r.probe.calls();
        r.phone.set_socks_dead(true);
        tokio::time::timeout(Duration::from_secs(3), async {
            while r.probe.calls() == before {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("two failed quick checks run a full check");
    }

    #[tokio::test]
    async fn phone_reporting_no_exit_checks_early() {
        let timing = HealthTiming {
            full: Duration::from_secs(30),
            ..fast_timing()
        };
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, timing).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let before = r.probe.calls();
        r.phone
            .send_metrics(r#"{"service":"CellGlide-metrics","version":1,"exitOK":false}"#);
        tokio::time::timeout(Duration::from_secs(2), async {
            while r.probe.calls() == before {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("exitOK=false triggers a full check");
    }

    #[tokio::test]
    async fn disconnect_is_not_blocked_by_a_running_full_check() {
        let timing = HealthTiming {
            quick: Duration::from_secs(30),
            ..fast_timing()
        };
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, timing).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        let before = r.probe.calls();
        r.probe.set_delay(Duration::from_secs(3));
        tokio::time::timeout(Duration::from_secs(3), async {
            while r.probe.calls() == before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a full check starts");

        let started = tokio::time::Instant::now();
        r.controller.send(Command::Disconnect).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "disconnect waited {:?} for the full check",
            started.elapsed()
        );
        assert_eq!(r.backend.stopped(), 1);
        assert_eq!(r.controller.snapshot().state, ConnState::Idle);
    }

    #[tokio::test]
    async fn stale_full_check_result_is_ignored() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        // A slow failing check is running when the user disconnects and reconnects.
        let before = r.probe.calls();
        r.probe.set_delay(Duration::from_millis(600));
        r.probe.set_ok(false);
        tokio::time::timeout(Duration::from_secs(3), async {
            while r.probe.calls() == before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a full check starts");
        r.controller.send(Command::Disconnect).await.unwrap();
        r.probe.set_delay(Duration::ZERO);
        r.probe.set_ok(true);
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert!(connected(&r.controller.snapshot()));
        assert_eq!(r.backend.stopped(), 1);
    }

    #[tokio::test]
    async fn session_change_found_by_full_check_rebuilds() {
        let r = rig_with(FakePhoneOptions::default(), any_port(), true, fast_timing()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        let caps = [
            "socks5-tcp",
            "udp-over-usb-v1",
            "dns-forward-v1",
            "session-watch-v1",
        ];
        r.phone.set_hello(hello_json(&caps, true, SESSION_B));
        wait_for(&r.controller, "rebuilt", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
        assert_eq!(r.backend.stopped(), 1);
    }

    // Battery over USB, and the names of attached phones.

    use crate::testing::battery_fixtures::charging;
    use crate::testing::{FakeLockdown, fixture_pair_record};

    fn lockdown_phone(tls: bool, stalls: bool) -> FakePhoneOptions {
        FakePhoneOptions {
            lockdown: Some(FakeLockdown {
                name: Some("测试手机".into()),
                model: Some("iPhone18,4".into()),
                registry: Some(charging()),
                session_tls: tls,
                registry_stalls: stalls,
                ..FakeLockdown::default()
            }),
            ..FakePhoneOptions::default()
        }
    }

    fn has_battery(s: &Snapshot) -> bool {
        s.phone
            .as_ref()
            .and_then(|c| c.usb.as_ref())
            .and_then(|u| u.battery.as_ref())
            .is_some()
    }

    /// Polls a condition that is not part of the snapshot.
    async fn eventually(what: &str, within: Duration, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + within;
        while !ok() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn connect_phone(r: &Rig) {
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
    }

    #[tokio::test]
    async fn usb_battery_reaches_the_card_and_the_phone() {
        let r = rig(lockdown_phone(false, false), any_port()).await;
        r.mux.add_pair_record_with("aaaa", fixture_pair_record());
        connect_phone(&r).await;
        let snap = wait_for(&r.controller, "battery", has_battery).await;
        let usb = snap.phone.unwrap().usb.unwrap();
        assert_eq!(usb.model.as_deref(), Some("iPhone18,4"));
        assert_eq!(usb.battery.unwrap().level, Some(75));
        eventually("a report on 8091", Duration::from_secs(5), || {
            !r.phone.battery_lines().is_empty()
        })
        .await;
        let line = r.phone.battery_lines().remove(0);
        let report: cg_protocol::phone::BatteryReport = serde_json::from_str(&line).unwrap();
        assert_eq!(report.service, "CellGlide-battery");
        assert_eq!(report.level, Some(75));
    }

    #[tokio::test]
    async fn unreadable_battery_leaves_usb_empty() {
        // No pairing record: the phone was never trusted on this computer.
        let r = rig(lockdown_phone(false, false), any_port()).await;
        connect_phone(&r).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let metrics = r#"{"service":"CellGlide-metrics","version":1,"sharing":true,"download":2048,"upload":1024}"#;
        let snap = {
            let mut rx = r.controller.subscribe();
            let found = tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    r.phone.send_metrics(metrics);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let snap = rx.borrow_and_update().clone();
                    if snap.phone.as_ref().is_some_and(|c| c.frame.is_some()) {
                        return snap;
                    }
                }
            })
            .await;
            found.expect("a phone frame")
        };
        let card = snap.phone.clone().unwrap();
        assert!(card.frame.is_some());
        assert_eq!(card.usb, None);
        assert!(connected(&snap), "the connection does not depend on it");
        assert!(r.phone.battery_lines().is_empty());
        assert_eq!(r.phone.relay_requests(), 0);
    }

    #[tokio::test]
    async fn labels_follow_attach_and_detach() {
        let r = rig(lockdown_phone(false, false), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        let snap = wait_for(&r.controller, "label", |s| s.labels.contains_key("aaaa")).await;
        let label = &snap.labels["aaaa"];
        assert_eq!(label.model.as_deref(), Some("iPhone18,4"));
        assert_eq!(label.name.as_deref(), Some("测试手机"));
        r.mux.detach(1);
        wait_for(&r.controller, "label gone", |s| s.labels.is_empty()).await;
    }

    #[tokio::test]
    async fn battery_tasks_stop_with_the_session() {
        let r = rig(lockdown_phone(true, false), any_port()).await;
        r.mux.add_pair_record_with("aaaa", fixture_pair_record());
        connect_phone(&r).await;
        wait_for(&r.controller, "battery", has_battery).await;
        eventually("8091 connected", Duration::from_secs(5), || {
            r.phone.battery_connections() == 1
        })
        .await;
        assert_eq!(r.phone.open_relays(), 1, "one relay, kept between reads");

        r.controller.send(Command::Disconnect).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(r.phone.open_relays(), 0, "relay closed");
        assert_eq!(r.phone.open_lockdown_sessions(), 0, "session closed");
        assert_eq!(r.phone.battery_connections(), 0, "8091 closed");
        let snap = r.controller.snapshot();
        assert!(snap.phone.is_none(), "the card is gone with the session");

        // The next connection starts fresh and the battery comes back.
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "battery again", has_battery).await;
        assert_eq!(r.phone.open_relays(), 1);
    }

    #[tokio::test]
    async fn unplugging_during_a_stalled_battery_read_closes_everything() {
        let r = rig(lockdown_phone(true, true), any_port()).await;
        r.mux.add_pair_record_with("aaaa", fixture_pair_record());
        connect_phone(&r).await;
        // The relay has the request and is not answering: the read is in flight.
        eventually("a stalled read", Duration::from_secs(5), || {
            r.phone.relay_requests() == 1 && r.phone.open_relays() == 1
        })
        .await;
        r.mux.detach(1);
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(r.phone.open_relays(), 0);
        assert_eq!(r.phone.open_lockdown_sessions(), 0);
        assert_eq!(r.phone.battery_connections(), 0);
        assert_eq!(r.backend.stopped(), 1, "the state machine kept going");
    }

    #[tokio::test]
    async fn replugging_a_different_phone_starts_fresh() {
        let r = rig_auto(lockdown_phone(true, false), any_port()).await;
        r.mux.add_pair_record_with("aaaa", fixture_pair_record());
        r.mux.add_pair_record_with("bbbb", fixture_pair_record());
        r.mux.attach(device("aaaa", 1));
        let snap = wait_for(&r.controller, "A's battery", has_battery).await;
        assert_eq!(battery_level(&snap), Some(75));
        eventually("A's 8091 report", Duration::from_secs(5), || {
            !r.phone.battery_lines().is_empty() && r.phone.battery_connections() == 1
        })
        .await;

        r.mux.detach(1);
        // B is another phone app: its services replace A's routes on the mux.
        let mut registry = charging();
        registry.insert(
            "CurrentCapacity".into(),
            plist::Value::Integer(40_i64.into()),
        );
        let b = FakePhone::start(
            &r.mux,
            FakePhoneOptions {
                lockdown: Some(FakeLockdown {
                    name: Some("B".into()),
                    model: Some("iPhone17,1".into()),
                    registry: Some(registry),
                    session_tls: true,
                    ..FakeLockdown::default()
                }),
                ..FakePhoneOptions::default()
            },
        )
        .await;
        r.mux.attach(device("bbbb", 2));
        let a_lines = r.phone.battery_lines().len();

        let snap = wait_for(&r.controller, "B's battery", |s| {
            has_battery(s) && s.labels.contains_key("bbbb")
        })
        .await;
        assert_eq!(battery_level(&snap), Some(40));
        assert_eq!(snap.labels.keys().collect::<Vec<_>>(), ["bbbb"]);
        eventually("B's 8091 report", Duration::from_secs(5), || {
            !b.battery_lines().is_empty()
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(r.phone.open_relays(), 0, "A's relay closed");
        assert_eq!(r.phone.open_lockdown_sessions(), 0);
        assert_eq!(r.phone.battery_connections(), 0, "A's 8091 closed");
        assert_eq!(r.phone.battery_lines().len(), a_lines, "A got nothing new");
        assert_eq!(b.open_relays(), 1);
    }

    fn battery_level(s: &Snapshot) -> Option<u8> {
        s.phone.as_ref()?.usb.as_ref()?.battery.as_ref()?.level
    }

    // The connection timeline (M3 Task 7).

    use crate::health::log::HealthLog;
    use crate::settings::LastVersions;
    use cg_ipc::health::{EventKind, EventPhase, IssueCause};

    fn watch_timing() -> HealthWatchTiming {
        HealthWatchTiming {
            channel: Duration::from_millis(200),
            tunnel: Duration::from_millis(200),
            fast: Duration::from_millis(100),
            frame_stale: Duration::from_secs(1),
            channel_timeout: Duration::from_millis(500),
            tunnel_timeout: crate::exit_check::CAPTIVE_TIMEOUT,
            judge: JudgeConfig {
                warmup: Duration::ZERO,
                ..JudgeConfig::default()
            },
            weak_median: None,
        }
    }

    /// Fast timeline checks writing to `dir` in UTC; auto connect off.
    async fn rig_health_at(dir: std::path::PathBuf, stored: Stored) -> Rig {
        rig_config(
            FakePhoneOptions::default(),
            any_port(),
            quiet_timing(),
            |config| {
                config.health_watch = watch_timing();
                config.health_dir = Some(dir);
                config.time_zone = TimeZone::UTC;
                config.settings = SettingsSource::Memory(stored);
            },
        )
        .await
    }

    fn manual() -> Stored {
        Stored {
            auto_connect: false,
            ..Stored::default()
        }
    }

    async fn rig_health() -> (Rig, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let r = rig_health_at(dir.path().join("health"), manual()).await;
        (r, dir)
    }

    fn kinds(c: &Controller) -> Vec<(EventKind, Option<EventPhase>)> {
        c.history(None)
            .events
            .iter()
            .map(|e| (e.kind, e.phase))
            .collect()
    }

    const NO_EXIT: &str =
        r#"{"service":"CellGlide-metrics","version":1,"exitOK":false,"radio":"5G","thermal":2}"#;

    #[tokio::test]
    async fn tunnel_failure_opens_and_closes_a_computer_relay_issue() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        r.tunnel_probe.set_ok(false);
        let snap = wait_for(&r.controller, "issue", |s| {
            s.health.cause == Some(IssueCause::ComputerRelay)
        })
        .await;
        assert!(snap.health.since.is_some());
        r.tunnel_probe.set_ok(true);
        let snap = wait_for(&r.controller, "recovered", |s| s.health.cause.is_none()).await;
        let last = snap.health.last_issue.unwrap();
        assert_eq!(last.cause, IssueCause::ComputerRelay);
        assert!(last.duration.is_some());
        let kinds = kinds(&r.controller);
        assert!(
            kinds.starts_with(&[
                (EventKind::ServiceStarted, None),
                (EventKind::UsbAttached, None),
                (EventKind::TunnelStarted, None)
            ]),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&(EventKind::Issue, Some(EventPhase::Start)))
                && kinds.contains(&(EventKind::Issue, Some(EventPhase::End))),
            "{kinds:?}"
        );
        assert!(r.tunnel_probe.calls() >= 5);
        // Judging never releases the session: that stays with the M1 checks.
        assert!(connected(&r.controller.snapshot()));
        assert_eq!(r.backend.started(), 1);
    }

    /// `watch_timing` with a warmup that is long enough to see.
    fn warm_timing() -> HealthWatchTiming {
        HealthWatchTiming {
            judge: JudgeConfig {
                warmup: Duration::from_secs(2),
                ..watch_timing().judge
            },
            ..watch_timing()
        }
    }

    #[tokio::test]
    async fn resume_retries_at_once_instead_of_waiting_out_the_backoff() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.probe.set_ok(false);
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        // The next try is two seconds away; a resume brings it forward.
        r.probe.set_ok(true);
        r.controller
            .send(Command::Sleep { sleeping: false })
            .await
            .unwrap();
        // Without the resume, the try comes 2 s after the failure; allow 1.5 s for slow CI.
        let mut rx = r.controller.subscribe();
        tokio::time::timeout(Duration::from_millis(1500), async {
            while !connected(&rx.borrow_and_update()) {
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("connected before the two second backoff");
    }

    #[tokio::test]
    async fn resume_after_a_disconnect_does_not_connect() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| {
            s.state == ConnState::Idle && !s.wants_connection
        })
        .await;
        r.controller
            .send(Command::Sleep { sleeping: false })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let snap = r.controller.snapshot();
        assert_eq!(snap.state, ConnState::Idle);
        assert!(!snap.wants_connection);
        assert_eq!(r.backend.started(), 1, "no new session after the resume");
    }

    #[tokio::test]
    async fn resume_without_a_phone_does_not_loop() {
        // Auto connect wants a phone, but none is plugged in.
        let r = rig_auto(FakePhoneOptions::default(), any_port()).await;
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        r.controller
            .send(Command::Sleep { sleeping: false })
            .await
            .unwrap();
        // Count every state the snapshot passes through for a while: the resume's one try
        // finds no phone and stops, with no connecting in between.
        let mut rx = r.controller.subscribe();
        let mut states = Vec::new();
        let _ = tokio::time::timeout(Duration::from_millis(1500), async {
            loop {
                states.push(rx.borrow_and_update().state.clone());
                rx.changed().await.unwrap();
            }
        })
        .await;
        assert!(
            states.iter().all(|s| *s == ConnState::NoPhone),
            "{states:?}"
        );
        assert_eq!(r.probe.calls(), 0, "no exit check without a phone");
        // The schedule is not stuck: a phone plugged in now connects, once.
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "connected", connected).await;
        assert_eq!(r.backend.started(), 1);
    }

    #[tokio::test]
    async fn resume_with_a_failing_phone_tries_once() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.probe.set_ok(false);
        r.mux.attach(device("aaaa", 1));
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        let before = r.probe.calls();
        r.controller
            .send(Command::Sleep { sleeping: false })
            .await
            .unwrap();
        // One try at once; the next waits out the fresh two second backoff.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(r.probe.calls(), before + 1);
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn sleep_command_resets_health_without_dropping_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let r = rig_config(
            FakePhoneOptions::default(),
            any_port(),
            quiet_timing(),
            |config| {
                config.health_watch = warm_timing();
                config.health_dir = Some(dir.path().join("health"));
                config.time_zone = TimeZone::UTC;
                config.settings = SettingsSource::Memory(manual());
            },
        )
        .await;
        connect_phone(&r).await;
        let port = r.controller.snapshot().socks_port;
        r.tunnel_probe.set_ok(false);
        wait_for(&r.controller, "issue", |s| {
            s.health.cause == Some(IssueCause::ComputerRelay)
        })
        .await;

        r.controller
            .send(Command::Sleep { sleeping: true })
            .await
            .unwrap();
        // The judge reset: the open issue ended, and the still failing probe is not
        // judged until the warmup has passed again.
        assert_eq!(r.controller.snapshot().health.cause, None);
        r.controller
            .send(Command::Sleep { sleeping: false })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(r.controller.snapshot().health.cause, None);
        // The session itself stays.
        let snap = r.controller.snapshot();
        assert!(connected(&snap));
        assert_eq!(snap.socks_port, port);
        assert!(
            kinds(&r.controller)
                .iter()
                .all(|(k, _)| *k != EventKind::TunnelStopped)
        );
        assert_eq!(r.backend.started(), 1);
        // Judging resumes once the warmup is over.
        wait_for(&r.controller, "issue again", |s| {
            s.health.cause == Some(IssueCause::ComputerRelay)
        })
        .await;
    }

    #[tokio::test]
    async fn phone_reporting_no_exit_with_tunnel_down_is_phone_network() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        r.tunnel_probe.set_ok(false);
        let frames = async {
            loop {
                r.phone.send_metrics(NO_EXIT);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        let snap = tokio::select! {
            snap = wait_for(&r.controller, "phone network", |s| {
                s.health.cause == Some(IssueCause::PhoneNetwork)
            }) => snap,
            _ = frames => unreachable!(),
        };
        assert!(snap.health.since.is_some());
        let events = r.controller.history(None).events;
        let named = events
            .iter()
            .find(|e| e.kind == EventKind::Issue && e.cause == Some(IssueCause::PhoneNetwork))
            .expect("an issue event naming the phone's network");
        let snapshot = named.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.radio.as_deref(), Some("5G"));
        assert_eq!(snapshot.thermal, Some(2));
        assert_eq!(snapshot.phone_version.as_deref(), Some("2.2.0"));
        assert_eq!(
            snapshot.computer_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// A rig for the airplane-mode cases: fast M1 checks, the timeline in `dir`, and a
    /// tunnel probe whose period the caller picks.
    async fn rig_airplane(dir: std::path::PathBuf, tunnel: Duration, fast: Duration) -> Rig {
        rig_config(
            FakePhoneOptions::default(),
            any_port(),
            fast_timing(),
            |config| {
                config.health_watch = HealthWatchTiming {
                    tunnel,
                    fast,
                    ..watch_timing()
                };
                config.health_dir = Some(dir);
                config.time_zone = TimeZone::UTC;
                config.settings = SettingsSource::Memory(manual());
            },
        )
        .await
    }

    /// Final review Important 1: airplane mode on the phone. The phone reports no exit, the
    /// exit check through it fails and the M1 checks release the session before the slow
    /// tunnel probe runs twice. The failed exit checks count against the tunnel leg, so the
    /// issue escalates to the phone's network and ends with that cause.
    #[tokio::test]
    async fn airplane_mode_release_is_logged_as_phone_network() {
        let dir = tempfile::tempdir().unwrap();
        // The tunnel probe stays out of it: only the exit checks speak for the tunnel.
        let long = Duration::from_secs(30);
        let r = rig_airplane(dir.path().join("health"), long, long).await;
        connect_phone(&r).await;
        let frames = async {
            loop {
                r.phone.send_metrics(NO_EXIT);
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        };
        let released = async {
            wait_for(&r.controller, "phone unstable", |s| {
                s.health.cause == Some(IssueCause::PhoneUnstable)
            })
            .await;
            r.probe.set_ok(false);
            wait_for(&r.controller, "released", |s| {
                matches!(
                    s.state,
                    ConnState::Retrying {
                        reason: Reason::NoExit
                    }
                )
            })
            .await
        };
        tokio::select! {
            _ = released => {}
            _ = frames => unreachable!(),
        }
        let events = r.controller.history(None).events;
        let stopped = events
            .iter()
            .position(|e| e.kind == EventKind::TunnelStopped)
            .expect("tunnelStopped");
        let first_session: Vec<_> = events[..=stopped]
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Issue | EventKind::TunnelStopped))
            .map(|e| (e.kind, e.phase, e.cause, e.detail.as_deref()))
            .collect();
        assert_eq!(
            first_session,
            [
                (
                    EventKind::Issue,
                    Some(EventPhase::Start),
                    Some(IssueCause::PhoneUnstable),
                    None
                ),
                (
                    EventKind::Issue,
                    Some(EventPhase::Escalate),
                    Some(IssueCause::PhoneNetwork),
                    None
                ),
                (
                    EventKind::Issue,
                    Some(EventPhase::End),
                    Some(IssueCause::PhoneNetwork),
                    None
                ),
                (
                    EventKind::TunnelStopped,
                    None,
                    None,
                    Some("released noExit")
                ),
            ]
        );
    }

    /// Spec §3.6: while there is a problem the tunnel is probed every `fast`, from the
    /// moment the issue opens rather than at the end of the slow period.
    #[tokio::test]
    async fn an_open_issue_probes_the_tunnel_fast() {
        let dir = tempfile::tempdir().unwrap();
        let r = rig_airplane(
            dir.path().join("health"),
            Duration::from_secs(30),
            Duration::from_millis(100),
        )
        .await;
        connect_phone(&r).await;
        eventually("the first tunnel probe", Duration::from_secs(2), || {
            r.tunnel_probe.calls() >= 1
        })
        .await;
        let frames = async {
            loop {
                r.phone.send_metrics(NO_EXIT);
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        };
        let probed = async {
            wait_for(&r.controller, "phone unstable", |s| {
                s.health.cause == Some(IssueCause::PhoneUnstable)
            })
            .await;
            let before = r.tunnel_probe.calls();
            eventually("a fast tunnel probe", Duration::from_secs(2), || {
                r.tunnel_probe.calls() > before
            })
            .await;
        };
        tokio::select! {
            _ = probed => {}
            _ = frames => unreachable!(),
        }
    }

    #[tokio::test]
    async fn silent_phone_turns_its_legs_unknown() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        // The metrics stream opens just after the session is up.
        tokio::time::sleep(Duration::from_millis(200)).await;
        for _ in 0..3 {
            r.phone.send_metrics(NO_EXIT);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        wait_for(&r.controller, "phone unstable", |s| {
            s.health.cause == Some(IssueCause::PhoneUnstable)
        })
        .await;
        // No more frames: after frame_stale, five unknowns in a row end the issue.
        let started = std::time::Instant::now();
        let snap = wait_for(&r.controller, "ended", |s| s.health.cause.is_none()).await;
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(
            snap.health.last_issue.unwrap().cause,
            IssueCause::PhoneUnstable
        );
        let kinds = kinds(&r.controller);
        assert!(kinds.contains(&(EventKind::Issue, Some(EventPhase::End))));
    }

    #[tokio::test]
    async fn bypass_and_dns_notices_are_logged() {
        let (r, _dir) = rig_health().await;
        r.backend.set_dns_unmanaged(true);
        connect_phone(&r).await;
        r.backend.push_notice(TunnelNotice::BypassUnavailable);
        r.backend.push_notice(TunnelNotice::BypassUpdateFailed);
        eventually("notices in the timeline", Duration::from_secs(3), || {
            let kinds = kinds(&r.controller);
            kinds.contains(&(EventKind::BypassUnavailable, None))
                && kinds.contains(&(EventKind::BypassUpdateFailed, None))
        })
        .await;
        let kinds = kinds(&r.controller);
        let dns = kinds
            .iter()
            .position(|k| *k == (EventKind::DnsNotAuthorized, None))
            .expect("dnsNotAuthorized");
        let started = kinds
            .iter()
            .position(|k| *k == (EventKind::TunnelStarted, None))
            .unwrap();
        assert!(started < dns, "{kinds:?}");
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == (EventKind::BypassUnavailable, None))
                .count(),
            1,
            "taken once"
        );
    }

    #[tokio::test]
    async fn strict_dns_off_is_published_and_logged() {
        let (r, _dir) = rig_health().await;
        let off = StrictDns::Off {
            reason: "BFE unavailable".into(),
        };
        r.backend.set_strict_dns(off.clone());
        connect_phone(&r).await;
        assert_eq!(r.controller.snapshot().strict_dns, off);
        let kinds = kinds(&r.controller);
        let strict = kinds
            .iter()
            .position(|k| *k == (EventKind::StrictDnsUnavailable, None))
            .expect("strictDnsUnavailable");
        let started = kinds
            .iter()
            .position(|k| *k == (EventKind::TunnelStarted, None))
            .unwrap();
        assert!(started < strict, "{kinds:?}");
        // Not connected: nothing to report.
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        assert_eq!(r.controller.snapshot().strict_dns, StrictDns::NotApplicable);
    }

    fn set_strict_dns(on: bool) -> Command {
        Command::SetSettings {
            auto_connect: None,
            phone: None,
            strict_dns: Some(on),
        }
    }

    /// The settings switch while connected: stored, applied to the running tunnel at once
    /// and published; turning it off logs nothing.
    #[tokio::test]
    async fn strict_dns_switch_applies_while_connected() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        r.controller.send(set_strict_dns(false)).await.unwrap();
        let snapshot = r.controller.snapshot();
        assert_eq!(snapshot.strict_dns, StrictDns::Disabled);
        assert!(!snapshot.settings.strict_dns);
        assert!(matches!(snapshot.state, ConnState::Connected { .. }));
        assert_eq!(r.backend.strict_calls(), vec![false]);
        assert!(
            !kinds(&r.controller)
                .iter()
                .any(|(kind, _)| *kind == EventKind::StrictDnsUnavailable)
        );
        r.controller.send(set_strict_dns(true)).await.unwrap();
        let snapshot = r.controller.snapshot();
        assert_eq!(snapshot.strict_dns, StrictDns::On);
        assert!(snapshot.settings.strict_dns);
        assert_eq!(r.backend.strict_calls(), vec![false, true]);
        assert_eq!(r.backend.started(), 1, "no reconnect");
    }

    /// Turning it on and failing is logged, like a connection that couldn't turn it on.
    #[tokio::test]
    async fn strict_dns_switched_on_but_unavailable_is_logged() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        let off = StrictDns::Off {
            reason: "BFE unavailable".into(),
        };
        r.backend.set_strict_dns(off.clone());
        r.controller.send(set_strict_dns(true)).await.unwrap();
        assert_eq!(r.controller.snapshot().strict_dns, off);
        assert!(
            kinds(&r.controller)
                .iter()
                .any(|(kind, _)| *kind == EventKind::StrictDnsUnavailable)
        );
    }

    /// Not connected: the switch is only stored, and the next connection's plan carries it.
    #[tokio::test]
    async fn strict_dns_setting_reaches_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let r = rig_health_at(
            dir.path().join("health"),
            Stored {
                strict_dns: false,
                ..manual()
            },
        )
        .await;
        wait_for(&r.controller, "setting off", |s| !s.settings.strict_dns).await;
        connect_phone(&r).await;
        assert!(!r.backend.last_plan().unwrap().strict_dns);
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.controller.send(set_strict_dns(true)).await.unwrap();
        assert!(r.backend.strict_calls().is_empty(), "nothing to apply to");
        assert!(r.controller.snapshot().settings.strict_dns);
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "connected", connected).await;
        assert!(r.backend.last_plan().unwrap().strict_dns);
    }

    /// Where the platform has no strict DNS (Linux), a connection says so and logs nothing.
    #[tokio::test]
    async fn strict_dns_not_applicable_logs_nothing() {
        let (r, _dir) = rig_health().await;
        connect_phone(&r).await;
        let snapshot = r.controller.snapshot();
        assert!(matches!(snapshot.state, ConnState::Connected { .. }));
        assert_eq!(snapshot.strict_dns, StrictDns::NotApplicable);
        assert!(
            !kinds(&r.controller)
                .iter()
                .any(|(kind, _)| *kind == EventKind::StrictDnsUnavailable)
        );
    }

    /// Collects what the actor logs (it runs on the test's own thread).
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn lines(&self, level: &str, text: &str) -> usize {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .filter(|l| l.contains(level) && l.contains(text))
                .count()
        }
    }

    /// Review Focus 3: today's file can't be written (it is a directory, which stops root
    /// too). Writes fail with one warning, the connection is untouched, and earlier days
    /// still read.
    #[tokio::test]
    async fn unwritable_log_dir_keeps_connection_working() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _log = tracing::subscriber::set_default(subscriber);

        let dir = tempfile::tempdir().unwrap();
        let utc = TimeZone::UTC;
        let today = Timestamp::now().to_zoned(utc.clone()).date();
        let yesterday = today.yesterday().unwrap();
        let earlier = HealthLog::new(dir.path().to_path_buf(), utc.clone());
        let at = |h| {
            yesterday
                .at(h, 0, 0, 0)
                .to_zoned(utc.clone())
                .unwrap()
                .timestamp()
        };
        earlier
            .append(&cg_ipc::health::HealthEvent::new(
                at(9),
                EventKind::TunnelStarted,
            ))
            .unwrap();
        earlier
            .append(&cg_ipc::health::HealthEvent::new(
                at(10),
                EventKind::TunnelStopped,
            ))
            .unwrap();
        std::fs::create_dir(dir.path().join(format!("{today}.jsonl"))).unwrap();

        let r = rig_health_at(dir.path().to_path_buf(), manual()).await;
        connect_phone(&r).await;
        r.tunnel_probe.set_ok(false);
        wait_for(&r.controller, "an issue, written nowhere", |s| {
            s.health.cause == Some(IssueCause::ComputerRelay)
        })
        .await;
        let message = "can't write the connection history";
        assert_eq!(captured.lines("WARN", message), 1);
        // serviceStarted, usbAttached, tunnelStarted and the issue all failed.
        assert!(captured.lines("DEBUG", message) >= 3);
        assert!(connected(&r.controller.snapshot()));
        // The actor still answers commands.
        r.controller.send(Command::Disconnect).await.unwrap();
        assert_eq!(r.backend.stopped(), 1);

        let today_page = r.controller.history(None);
        assert!(today_page.events.is_empty());
        assert_eq!(today_page.days.len(), 7);
        let before = r.controller.history(Some(yesterday));
        assert_eq!(
            before.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::TunnelStarted, EventKind::TunnelStopped]
        );
        assert_eq!(before.day, yesterday.to_string());
    }

    #[tokio::test]
    async fn log_dir_that_is_a_file_reads_empty() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let r = rig_health_at(file.path().to_path_buf(), manual()).await;
        connect_phone(&r).await;
        let page = r.controller.history(None);
        assert!(page.events.is_empty());
        assert_eq!(page.days.len(), 7);
        assert!(page.begin <= page.end);
        assert!(connected(&r.controller.snapshot()));
    }

    #[tokio::test]
    async fn version_change_is_logged_once() {
        let dir = tempfile::tempdir().unwrap();
        let stored = Stored {
            last_versions: LastVersions {
                computer: Some("0.9.0".into()),
                phone: Some("2.2.0".into()),
            },
            ..manual()
        };
        let r = rig_health_at(dir.path().to_path_buf(), stored).await;
        connect_phone(&r).await;
        let changes = |c: &Controller| -> Vec<Option<String>> {
            c.history(None)
                .events
                .into_iter()
                .filter(|e| e.kind == EventKind::VersionChanged)
                .map(|e| e.detail)
                .collect()
        };
        let expected = format!("0.9.0 | 2.2.0 → {} | 2.2.0", env!("CARGO_PKG_VERSION"));
        assert_eq!(changes(&r.controller), [Some(expected.clone())]);
        r.controller.send(Command::Disconnect).await.unwrap();
        r.controller
            .send(Command::Connect { serial: None })
            .await
            .unwrap();
        wait_for(&r.controller, "reconnected", |s| {
            connected(s) && r.backend.started() == 2
        })
        .await;
        assert_eq!(changes(&r.controller), [Some(expected)]);
    }

    #[tokio::test]
    async fn history_without_a_directory_is_empty() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        let page = r.controller.history(None);
        assert!(page.events.is_empty());
        assert_eq!(page.start, DayStart::default());
        assert_eq!(page.days.len(), 7);
        assert_eq!(page.days.last(), Some(&page.day));
    }

    /// Wireless sessions (`controller/tests/wireless.rs`).
    mod wireless;

    mod cable_pairing {
        use super::*;
        use crate::testing::CableBehaviour;
        use crate::wireless::store::{PlainSecrets, WirelessStore};
        use base64::Engine as _;

        const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
        const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

        fn new_line() -> String {
            format!(
                "{PHONE_ID} {IDENTITY} {} {}",
                base64::engine::general_purpose::STANDARD.encode([7u8; 32]),
                crate::wireless::encoding::encode_name("Eve iPhone")
            )
        }

        fn options(cable: CableBehaviour, caps: &[&str]) -> FakePhoneOptions {
            FakePhoneOptions {
                hello: hello_json(caps, true, SESSION_A),
                cable,
                ..FakePhoneOptions::default()
            }
        }

        const WIRELESS_CAPS: &[&str] = &[
            "socks5-tcp",
            "udp-over-usb-v1",
            "dns-forward-v1",
            "session-watch-v1",
            "wireless-v2",
        ];

        struct Wireless {
            r: Rig,
            store: Arc<WirelessStore>,
            _dir: tempfile::TempDir,
        }

        /// A rig with a store; `setting` is the stored switch, `forced` the service's override.
        async fn wireless_rig(
            options: FakePhoneOptions,
            setting: bool,
            forced: bool,
            with_store: bool,
        ) -> Wireless {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(WirelessStore::open(dir.path(), Arc::new(PlainSecrets)).unwrap());
            let shared = store.clone();
            let r = rig_config(options, any_port(), quiet_timing(), |config| {
                config.settings = SettingsSource::Memory(Stored {
                    auto_connect: false,
                    wireless_enabled: setting,
                    ..Stored::default()
                });
                config.wireless = with_store.then_some(shared);
                config.wireless_forced = forced;
                config.computer_name = Some("Desk PC".into());
            })
            .await;
            Wireless {
                r,
                store,
                _dir: dir,
            }
        }

        async fn until(what: &str, done: impl FnMut() -> bool) {
            eventually(what, Duration::from_secs(8), done).await;
        }

        /// Captures the actor's log on this thread: the cable pairing task logs its outcome
        /// last, so a line here means the attempt has finished.
        fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
            let captured = Captured::default();
            let writer = captured.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .finish();
            (captured, tracing::subscriber::set_default(subscriber))
        }

        async fn logged(captured: &Captured, level: &str, text: &str, count: usize) {
            until(&format!("{count} × {level} {text:?}"), || {
                captured.lines(level, text) >= count
            })
            .await;
        }

        #[tokio::test]
        async fn a_connected_session_pairs_once_over_the_cable() {
            let (log, _guard) = capture();
            let w = wireless_rig(
                options(CableBehaviour::Reply(new_line()), WIRELESS_CAPS),
                true,
                false,
                true,
            )
            .await;
            connect_and_wait(&w.r, "connected", connected).await;
            until("the pairing", || w.store.records().len() == 1).await;
            let record = &w.store.records()[0];
            assert_eq!(record.method, crate::wireless::store::Method::Cable);
            assert_eq!(w.store.phone_id("aaaa"), Some(PHONE_ID.to_string()));
            assert_eq!(
                w.r.phone.cable_requests().len(),
                1,
                "one connection for the session"
            );
            // Another session on the same cable asks nothing.
            w.r.controller.send(Command::Disconnect).await.unwrap();
            w.r.controller
                .send(Command::Connect { serial: None })
                .await
                .unwrap();
            wait_for(&w.r.controller, "reconnected", |s| {
                connected(s) && w.r.backend.started() == 2
            })
            .await;
            // The second session's attempt has decided.
            logged(&log, "INFO", "wireless: already paired", 1).await;
            assert_eq!(w.r.phone.cable_connections(), 1);
        }

        #[tokio::test]
        async fn the_service_override_enables_it_when_the_setting_is_off() {
            let w = wireless_rig(
                options(CableBehaviour::Reply(new_line()), WIRELESS_CAPS),
                false,
                true,
                true,
            )
            .await;
            connect_and_wait(&w.r, "connected", connected).await;
            until("the pairing", || w.store.records().len() == 1).await;
        }

        #[tokio::test]
        async fn nothing_is_attempted_without_the_switch_the_store_or_the_capability() {
            let cases = [
                // Setting off, not forced.
                (WIRELESS_CAPS, false, false, true),
                // No store: the feature is absent.
                (WIRELESS_CAPS, true, true, false),
                // A phone without wireless-v2.
                (
                    &["socks5-tcp", "udp-over-usb-v1", "session-watch-v1"][..],
                    true,
                    false,
                    true,
                ),
            ];
            for (caps, setting, forced, with_store) in cases {
                let (log, _guard) = capture();
                let w = wireless_rig(
                    options(CableBehaviour::Reply(new_line()), caps),
                    setting,
                    forced,
                    with_store,
                )
                .await;
                connect_and_wait(&w.r, "connected", connected).await;
                if with_store {
                    logged(&log, "INFO", "wireless: cable pairing skipped", 1).await;
                }
                // Without a store no attempt is started at all, and that is decided before the
                // session is published as connected.
                assert_eq!(w.r.phone.cable_connections(), 0);
                assert!(w.store.records().is_empty());
            }
        }

        #[tokio::test]
        async fn failures_leave_the_usb_session_connected_and_save_nothing() {
            for cable in [
                CableBehaviour::Reply("garbage".into()),
                CableBehaviour::Reply(format!("{PHONE_ID} {IDENTITY} AAAA")),
                CableBehaviour::Refuse,
            ] {
                let (log, _guard) = capture();
                let refused = cable == CableBehaviour::Refuse;
                let w = wireless_rig(options(cable, WIRELESS_CAPS), true, false, true).await;
                connect_and_wait(&w.r, "connected", connected).await;
                if !refused {
                    until("the attempt", || w.r.phone.cable_connections() == 1).await;
                }
                logged(&log, "WARN", "wireless: cable pairing failed", 1).await;
                let snapshot = w.r.controller.snapshot();
                assert!(connected(&snapshot), "{:?}", snapshot.state);
                assert!(w.store.records().is_empty());
                assert_eq!(w.store.phone_id("aaaa"), None);
                assert_eq!(w.r.backend.started(), 1, "the session was not rebuilt");
            }
        }

        #[tokio::test]
        async fn a_silent_phone_does_not_hold_the_session_and_the_attempt_ends_with_it() {
            let w = wireless_rig(
                options(CableBehaviour::Silent, WIRELESS_CAPS),
                true,
                false,
                true,
            )
            .await;
            connect_and_wait(&w.r, "connected", connected).await;
            until("the attempt", || w.r.phone.open_cable_connections() == 1).await;
            // Connected while the attempt waits on the phone.
            assert!(connected(&w.r.controller.snapshot()));
            // The session ends: the attempt's connection goes with it.
            w.r.controller.send(Command::Disconnect).await.unwrap();
            until("the connection to close", || {
                w.r.phone.open_cable_connections() == 0
            })
            .await;
            assert!(w.store.records().is_empty());
        }

        #[tokio::test]
        async fn a_known_reply_without_a_pairing_here_is_logged_as_such() {
            let (log, _guard) = capture();
            let w = wireless_rig(
                options(
                    CableBehaviour::Reply(format!("CG1 KNOWN {PHONE_ID}")),
                    WIRELESS_CAPS,
                ),
                true,
                false,
                true,
            )
            .await;
            connect_and_wait(&w.r, "connected", connected).await;
            logged(&log, "WARN", "has a pairing this computer lacks", 1).await;
            assert_eq!(log.lines("INFO", "already paired"), 0);
            assert!(connected(&w.r.controller.snapshot()));
            assert!(w.store.records().is_empty());
        }
    }
}

#[cfg(test)]
mod tier_tests {
    use super::*;

    #[test]
    fn the_cable_tier_is_unchanged() {
        let quick = HealthTiming::default();
        assert_eq!(quick.quick_timeout, Duration::from_secs(2));
        assert_eq!(quick.quick_fail_after, 2);
        let watch = HealthWatchTiming::default();
        assert_eq!(watch.channel_timeout, Duration::from_secs(2));
        assert_eq!(watch.tunnel_timeout, Duration::from_secs(5));
        assert_eq!(watch.judge, JudgeConfig::default());
        assert_eq!(watch.judge.fail_after, 2);
        assert_eq!(watch.weak_median, None);
    }

    #[test]
    fn the_wireless_tier() {
        let quick = HealthTiming::wireless();
        assert_eq!(quick.quick_timeout, Duration::from_millis(2500));
        assert_eq!(quick.quick_fail_after, 3);
        let cable = HealthTiming::default();
        assert_eq!(
            (quick.quick, quick.full, quick.retry, quick.early_spacing),
            (cable.quick, cable.full, cable.retry, cable.early_spacing)
        );
        let watch = HealthWatchTiming::wireless();
        assert_eq!(watch.channel_timeout, Duration::from_millis(2500));
        assert_eq!(watch.tunnel_timeout, Duration::from_millis(5500));
        assert_eq!(watch.judge, JudgeConfig::wireless());
        assert_eq!(watch.judge.fail_after, 3);
        assert_eq!(watch.weak_median, Some(Duration::from_millis(1150)));
        let cable = HealthWatchTiming::default();
        assert_eq!(
            (watch.channel, watch.tunnel, watch.fast, watch.frame_stale),
            (cable.channel, cable.tunnel, cable.fast, cable.frame_stale)
        );
    }

    #[test]
    fn quick_failures_count_up_to_the_configured_threshold() {
        for fail_after in [2, 3] {
            let mut failures = 0;
            for _ in 1..fail_after {
                assert!(!quick_check_counted(&mut failures, false, fail_after));
            }
            assert!(quick_check_counted(&mut failures, false, fail_after));
            assert_eq!(failures, 0, "counting starts again");
            // A success in between resets the run.
            for _ in 1..fail_after {
                quick_check_counted(&mut failures, false, fail_after);
            }
            assert!(!quick_check_counted(&mut failures, true, fail_after));
            assert!(!quick_check_counted(&mut failures, false, fail_after));
        }
    }
}
