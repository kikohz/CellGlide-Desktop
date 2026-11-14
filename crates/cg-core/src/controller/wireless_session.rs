//! The wireless attempt's own task: it owns the access point, finds the paired phone, dials it
//! and hands the link to the actor, which builds the session exactly as for USB
//! (`Actor::establish`). One task per `ConnectWireless`; the actor aborts it to end the attempt.
//!
//! The running access point is a local of `drive`, so every way the task stops drops it: its
//! own failures (dropped before `Done` is sent), being aborted (the actor awaits the abort, so
//! the access point is gone once the actor moves on), the actor itself going away (`Task`
//! aborts on drop) and a panic (unwinding).
//!
//! The session's end reaches the task as the drop of a oneshot sender the actor keeps in the
//! session (`Active::ended`): the tunnel is stopped before that, so a reconnect always starts
//! from the original network.
//!
//! A phone without a usable pairing, alone on the access point while it waits, is paired by
//! code (`pair`). Everything of a pairing in progress lives in that future: the pairing channel,
//! and the question to the user, which the actor holds only as the reply sender of
//! `WirelessEvent::Confirm`. Whatever ends the attempt drops both, and nothing is saved until
//! both users agreed and the phone's proof checked out (`code_pairing`).

use super::Event;
use crate::wireless::code_pairing::{self, CodePairingError, Paired};
use crate::wireless::pairing::{PAIRING_IDENTITY, link_key, pairing_channel_key};
use crate::wireless::policy::{
    self, DialContext, Next, PAIRING_BUSY_WAIT, Pick, RETRY_EVERY, RETRY_FOR, UnknownTracker,
    WAIT_FOR_PHONE,
};
use crate::wireless::ports::{DialError, FoundPhone, RunningAccessPoint, TlsLink, WirelessPorts};
use crate::wireless::store::{Method, Record, WirelessStore};
use cg_ipc::Reason;
use cg_ipc::wireless::{WirelessFailure, WirelessPhaseView};
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// The wireless attempt's windows and limits; the defaults are `wireless::policy`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WirelessTiming {
    /// How long the access point waits for a paired phone.
    pub wait_for_phone: Duration,
    /// Between discovery rounds (a round that takes longer is not waited for again), and
    /// before the first round after a session ended.
    pub retry_every: Duration,
    /// How long a session that ended is tried again.
    pub retry_for: Duration,
    /// One discovery round.
    pub browse: Duration,
    /// The dial that checks the pairing before the session is built.
    pub dial_timeout: Duration,
    /// This many rounds in a row with the paired phone's name at several addresses are shown
    /// as a name clash (and end as one when the window runs out).
    pub clash_rounds: u32,
    /// A device on USB is asked for the phone app's hello (to see whether it can take over
    /// from the wireless session) at most this often.
    pub usb_probe_every: Duration,
}

impl Default for WirelessTiming {
    fn default() -> Self {
        WirelessTiming {
            wait_for_phone: WAIT_FOR_PHONE,
            retry_every: RETRY_EVERY,
            retry_for: RETRY_FOR,
            browse: Duration::from_secs(2),
            dial_timeout: policy::DIAL_TIMEOUT,
            clash_rounds: 3,
            usb_probe_every: Duration::from_secs(10),
        }
    }
}

/// Where the wireless attempt is. Everything but `Off` and `Failed` owns the controller: the
/// state comes from here and not from usbmuxd, and no USB connection is attempted.
/// `Debug` never prints the pairing code.
#[derive(Clone, PartialEq, Eq)]
pub enum WirelessPhase {
    Off,
    /// Reading the access point's name and starting it.
    Starting,
    /// The access point is up since `since`; looking for the paired phone.
    Waiting {
        since: Instant,
        ssid: String,
        /// The paired phone's name has been at several addresses for a few rounds.
        name_clash: bool,
        /// What holds the attempt up meanwhile (`SeveralPhones`, `PairingBusy`); the window
        /// ends with it.
        hint: Option<WirelessFailure>,
    },
    /// Found; checking the pairing and building the session.
    Dialing,
    /// Code pairing: `code` is shown until this computer's user answers (`ConfirmCode`), and
    /// after a match (`confirmed`) until the phone answers too.
    Pairing {
        code: String,
        phone_name: Option<String>,
        confirmed: bool,
    },
    Connected,
    /// The session ended at `since`; trying to get it back.
    Reconnecting {
        since: Instant,
        name_clash: bool,
    },
    Failed(WirelessFailure),
}

impl WirelessPhase {
    pub fn owns(&self) -> bool {
        !matches!(self, WirelessPhase::Off | WirelessPhase::Failed(_))
    }

    pub fn view(&self) -> Option<WirelessPhaseView> {
        Some(match self {
            WirelessPhase::Off => return None,
            WirelessPhase::Starting => WirelessPhaseView::Starting,
            WirelessPhase::Waiting { .. } => WirelessPhaseView::Waiting,
            WirelessPhase::Dialing => WirelessPhaseView::Connecting,
            WirelessPhase::Connected => WirelessPhaseView::Connected,
            WirelessPhase::Reconnecting { .. } => WirelessPhaseView::Reconnecting,
            WirelessPhase::Pairing {
                code,
                phone_name,
                confirmed,
            } => WirelessPhaseView::Pairing {
                code: code.clone(),
                phone_name: phone_name.clone(),
                confirmed: *confirmed,
            },
            WirelessPhase::Failed(reason) => WirelessPhaseView::Failed {
                reason: reason.clone(),
            },
        })
    }

    pub fn hint(&self) -> Option<WirelessFailure> {
        match self {
            WirelessPhase::Waiting { hint, .. } => hint.clone(),
            _ => None,
        }
    }

    pub fn name_clash(&self) -> bool {
        matches!(
            self,
            WirelessPhase::Waiting {
                name_clash: true,
                ..
            } | WirelessPhase::Reconnecting {
                name_clash: true,
                ..
            }
        )
    }
}

impl std::fmt::Debug for WirelessPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WirelessPhase::Off => f.write_str("Off"),
            WirelessPhase::Starting => f.write_str("Starting"),
            WirelessPhase::Waiting {
                since,
                ssid,
                name_clash,
                hint,
            } => f
                .debug_struct("Waiting")
                .field("since", since)
                .field("ssid", ssid)
                .field("name_clash", name_clash)
                .field("hint", hint)
                .finish(),
            WirelessPhase::Dialing => f.write_str("Dialing"),
            WirelessPhase::Pairing {
                phone_name,
                confirmed,
                ..
            } => f
                .debug_struct("Pairing")
                .field("code", &"<hidden>")
                .field("phone_name", phone_name)
                .field("confirmed", confirmed)
                .finish(),
            WirelessPhase::Connected => f.write_str("Connected"),
            WirelessPhase::Reconnecting { since, name_clash } => f
                .debug_struct("Reconnecting")
                .field("since", since)
                .field("name_clash", name_clash)
                .finish(),
            WirelessPhase::Failed(failure) => f.debug_tuple("Failed").field(failure).finish(),
        }
    }
}

/// Why the actor could not build the session on a link the task had checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NotEstablished {
    /// A dial inside it was refused with an unknown identity.
    UnknownIdentity,
    /// The phone can't be used and trying again won't help (`Reason::PhoneTooOld`).
    Permanent(Reason),
    /// Anything else; tried again.
    Other,
}

/// What the task asks of, or tells, the actor; tagged with its attempt (`Event::Wireless`).
pub(super) enum WirelessEvent {
    Phase(WirelessPhase),
    /// Build the session on `link`. The answer carries a receiver that completes (with an
    /// error) when the session is over and its tunnel is down.
    Establish {
        link: Arc<TlsLink>,
        reply: oneshot::Sender<Result<oneshot::Receiver<()>, NotEstablished>>,
    },
    /// Code pairing: show `code` (phase `Pairing`) and answer whether this computer's user says
    /// it matches the phone's. Dropping `reply` unanswered withdraws the question (the task
    /// takes it as "no"); the task dropping the receiver means it no longer waits.
    Confirm {
        code: String,
        phone_name: Option<String>,
        reply: oneshot::Sender<bool>,
    },
    /// The attempt is over; the access point is already down.
    Done(WirelessFailure),
}

/// The attempt's task; aborted when dropped.
pub(super) struct Task(Option<JoinHandle<()>>);

impl Task {
    /// `computer_name` as configured (`ControllerConfig::computer_name`); the code pairing's
    /// `M` line gets it through `wireless::computer_name`.
    pub(super) fn spawn(context: Context, computer_name: Option<String>) -> Task {
        Task(Some(tokio::spawn(run_as(context, computer_name))))
    }

    /// Aborts the task and waits until it is gone (and with it the access point).
    pub(super) async fn stop(mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// The task panicked (it never ends otherwise without sending `Done`).
    pub(super) async fn panicked(&mut self) -> bool {
        match self.0.as_ref() {
            Some(handle) if handle.is_finished() => {}
            _ => return false,
        }
        let handle = self.0.take().expect("checked above");
        handle.await.is_err_and(|e| e.is_panic())
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

pub(super) struct Context {
    pub attempt: u64,
    pub ports: WirelessPorts,
    pub store: Arc<WirelessStore>,
    pub timing: WirelessTiming,
    pub events: mpsc::Sender<Event>,
}

impl Context {
    async fn tell(&self, event: WirelessEvent) -> bool {
        self.events
            .send(Event::Wireless(self.attempt, event))
            .await
            .is_ok()
    }
}

/// The attempt without a computer name of its own (the phone is then told the fallback); for
/// tests that drive the task directly.
#[cfg(test)]
pub(super) async fn run(context: Context) {
    run_as(context, None).await;
}

async fn run_as(context: Context, computer_name: Option<String>) {
    let failure = drive(&context, computer_name).await;
    tracing::info!("wireless: attempt ended: {failure:?}");
    context.tell(WirelessEvent::Done(failure)).await;
}

/// The attempt from start to failure; the access point lives here and nowhere else.
async fn drive(context: &Context, computer_name: Option<String>) -> WirelessFailure {
    let timing = context.timing;
    context
        .tell(WirelessEvent::Phase(WirelessPhase::Starting))
        .await;
    let store = context.store.clone();
    let (ssid, passphrase) = match tokio::task::spawn_blocking(move || store.access_point()).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => {
            tracing::warn!("wireless: no access point name: {error}");
            return WirelessFailure::StoreUnavailable;
        }
        Err(error) => {
            tracing::warn!("wireless: no access point name: {error}");
            return WirelessFailure::StoreUnavailable;
        }
    };
    let started = context.ports.access_point.start(&ssid, &passphrase).await;
    drop(passphrase);
    let mut access_point = match started {
        Ok(access_point) => access_point,
        Err(cause) => {
            tracing::warn!("wireless: access point not started: {cause}");
            return WirelessFailure::AccessPoint { cause };
        }
    };
    tracing::info!("wireless: access point {ssid} started");
    let mut attempt = Attempt::new(context, computer_name);
    let mut window = Window {
        kind: WindowKind::Waiting { ssid },
        since: Instant::now(),
        deadline: Instant::now() + timing.wait_for_phone,
    };
    loop {
        let ended = match attempt.connect(access_point.as_mut(), &mut window).await {
            Ok(ended) => ended,
            Err(failure) => return failure,
        };
        tokio::select! {
            _ = ended => tracing::info!("wireless: session ended; reconnecting"),
            () = access_point.lost() => return lost(),
        }
        let since = Instant::now();
        tokio::select! {
            () = tokio::time::sleep(timing.retry_every) => {}
            () = access_point.lost() => return lost(),
        }
        window = Window {
            kind: WindowKind::Reconnecting,
            since,
            deadline: since + timing.retry_for,
        };
    }
}

fn lost() -> WirelessFailure {
    tracing::warn!("wireless: the system stopped the access point");
    WirelessFailure::Lost
}

enum WindowKind {
    Waiting { ssid: String },
    Reconnecting,
}

/// One stretch of looking for the phone: the wait after the start, or a reconnect.
struct Window {
    kind: WindowKind,
    since: Instant,
    deadline: Instant,
}

impl Window {
    fn phase(&self, name_clash: bool, hint: Option<WirelessFailure>) -> WirelessPhase {
        match &self.kind {
            WindowKind::Waiting { ssid } => WirelessPhase::Waiting {
                since: self.since,
                ssid: ssid.clone(),
                name_clash,
                hint,
            },
            WindowKind::Reconnecting => WirelessPhase::Reconnecting {
                since: self.since,
                name_clash,
            },
        }
    }

    /// The phase while dialling: the reconnect keeps showing that it reconnects.
    fn dialing(&self, name_clash: bool) -> WirelessPhase {
        match self.kind {
            WindowKind::Waiting { .. } => WirelessPhase::Dialing,
            WindowKind::Reconnecting => self.phase(name_clash, None),
        }
    }

    /// Code pairing happens only while waiting for a phone after the user asked: a reconnect
    /// brings back the session it lost, not another phone.
    fn pairs(&self) -> bool {
        matches!(self.kind, WindowKind::Waiting { .. })
    }

    fn ran_out(
        &self,
        name_clash: bool,
        hint: Option<WirelessFailure>,
        unknown_seen: bool,
    ) -> WirelessFailure {
        if name_clash {
            WirelessFailure::NameClash
        } else if let Some(hint) = hint {
            hint
        } else if unknown_seen {
            WirelessFailure::PairingRefused
        } else {
            match self.kind {
                WindowKind::Waiting { .. } => WirelessFailure::NoPhone,
                WindowKind::Reconnecting => WirelessFailure::Lost,
            }
        }
    }
}

enum Dialled {
    Connected(oneshot::Receiver<()>),
    Retry,
    End(WirelessFailure),
}

/// How one code pairing went.
enum Pairing {
    /// Saved (and `F` sent): dial the phone with `record` at once.
    Paired(Record),
    /// The phone turned the pairing channel away before its first line.
    Busy,
    /// The pairing channel did not come up: try again next round.
    Retry,
    End(WirelessFailure),
}

/// What one `ConnectWireless` remembers between its windows.
struct Attempt<'a> {
    context: &'a Context,
    computer_name: Option<String>,
    /// Phone id → the address a handshake with its pairing completed at, in this attempt.
    verified: HashMap<String, IpAddr>,
    /// Pairings that were good when the attempt began and that it marked stale.
    marked_stale: HashSet<String>,
    /// Runs of unknown identities, for the phone last picked.
    unknowns: UnknownTracker,
    picked: Option<String>,
    /// The pairing made by code in this attempt, if any: the phone and when.
    fresh: Option<(String, Instant)>,
    /// The phone turned pairing away; not asked again before this.
    busy_until: Option<Instant>,
}

impl<'a> Attempt<'a> {
    fn new(context: &'a Context, computer_name: Option<String>) -> Attempt<'a> {
        Attempt {
            context,
            computer_name,
            verified: HashMap::new(),
            marked_stale: HashSet::new(),
            unknowns: UnknownTracker::new(),
            picked: None,
            fresh: None,
            busy_until: None,
        }
    }

    async fn tell(&self, phase: WirelessPhase) {
        self.context.tell(WirelessEvent::Phase(phase)).await;
    }

    /// Rounds of discovery and dialling (or code pairing) until the session is built (its end
    /// receiver), the window runs out, or something ends the attempt.
    async fn connect(
        &mut self,
        access_point: &mut dyn RunningAccessPoint,
        window: &mut Window,
    ) -> Result<oneshot::Receiver<()>, WirelessFailure> {
        let timing = self.context.timing;
        let mut clash_rounds = 0u32;
        let mut unknown_seen = false;
        // What holds the attempt up while it waits (several unpaired phones, a busy phone).
        let mut hint: Option<WirelessFailure> = None;
        // The (name clash, hint) last shown with the window's phase.
        let mut shown = (false, None);
        self.tell(window.phase(false, None)).await;
        loop {
            let round = Instant::now();
            let name_clash = clash_rounds >= timing.clash_rounds;
            if round >= window.deadline {
                return Err(window.ran_out(name_clash, hint, unknown_seen));
            }
            if (name_clash, &hint) != (shown.0, &shown.1) {
                shown = (name_clash, hint.clone());
                self.tell(window.phase(name_clash, hint.clone())).await;
            }
            let local = access_point.local_address();
            let found = tokio::select! {
                found = self.browse(local) => found,
                () = access_point.lost() => return Err(lost()),
            };
            let records = self.context.store.records();
            let pick = policy::pick(&found, &records, &self.verified, &self.marked_stale);
            // A phone to dial with a pairing (one found paired, or one just paired by code).
            let mut to_dial: Option<(FoundPhone, Record)> = None;
            match pick {
                Pick::Paired { phone, record } => {
                    clash_rounds = 0;
                    hint = None;
                    to_dial = Some((phone, record));
                }
                Pick::Ambiguous { instance } => {
                    clash_rounds += 1;
                    tracing::debug!("wireless: {instance} is advertised from several addresses");
                }
                Pick::Unpaired(phone) if window.pairs() => {
                    clash_rounds = 0;
                    if self.busy_until.is_some_and(|until| round < until) {
                        hint = Some(WirelessFailure::PairingBusy);
                    } else {
                        hint = None;
                        self.tell(WirelessPhase::Dialing).await;
                        let paired = tokio::select! {
                            paired = self.pair(&phone) => paired,
                            () = access_point.lost() => return Err(lost()),
                        };
                        match paired {
                            Pairing::Paired(record) => {
                                // However late in the window, the new pairing gets its chance.
                                window.deadline =
                                    window.deadline.max(Instant::now() + timing.retry_for);
                                to_dial = Some((phone, record));
                            }
                            Pairing::Busy => {
                                tracing::info!(
                                    "wireless: the phone turned pairing away; asking again later"
                                );
                                self.busy_until = Some(Instant::now() + PAIRING_BUSY_WAIT);
                                hint = Some(WirelessFailure::PairingBusy);
                            }
                            Pairing::Retry => {}
                            Pairing::End(failure) => return Err(failure),
                        }
                        if to_dial.is_none() {
                            shown = (false, hint.clone());
                            self.tell(window.phase(false, hint.clone())).await;
                        }
                    }
                }
                Pick::SeveralUnpaired if window.pairs() => {
                    clash_rounds = 0;
                    hint = Some(WirelessFailure::SeveralPhones);
                }
                // Reconnecting brings back the phone it lost, never pairs another one.
                Pick::Unpaired(_) | Pick::SeveralUnpaired => clash_rounds = 0,
                // A round that finds nothing proves nothing (discovery misses about a third).
                Pick::None => {}
            }
            if let Some((phone, record)) = to_dial {
                shown = (false, None);
                self.tell(window.dialing(false)).await;
                let dialled = tokio::select! {
                    dialled = self.dial(&phone, &record, window, &mut unknown_seen) => dialled,
                    () = access_point.lost() => return Err(lost()),
                };
                match dialled {
                    Dialled::Connected(ended) => return Ok(ended),
                    Dialled::End(failure) => return Err(failure),
                    Dialled::Retry => self.tell(window.phase(false, None)).await,
                }
            }
            tokio::select! {
                () = tokio::time::sleep_until(round + timing.retry_every) => {}
                () = access_point.lost() => return Err(lost()),
            }
        }
    }

    async fn browse(&self, local: Option<IpAddr>) -> Vec<FoundPhone> {
        let timing = self.context.timing;
        let Some(local) = local else {
            // The access point has no address yet: nothing can be found this round.
            tokio::time::sleep(timing.browse).await;
            return Vec::new();
        };
        match self
            .context
            .ports
            .browser
            .browse(local, timing.browse)
            .await
        {
            Ok(found) => found,
            Err(error) => {
                tracing::debug!("wireless: discovery failed: {error}");
                Vec::new()
            }
        }
    }

    /// Code pairing with `phone` over the pairing channel (the public identity). The code goes
    /// to the actor (phase `Pairing`) and the user's answer comes back through
    /// `WirelessEvent::Confirm`; the pairing is saved, on a blocking thread, only once both
    /// users agreed and the phone's proof checked out, and before `F` is sent. Dropping this
    /// future (the attempt ends) drops the channel and the question, and saves nothing.
    async fn pair(&mut self, phone: &FoundPhone) -> Pairing {
        let context = self.context;
        let to = SocketAddr::new(phone.address, phone.port);
        let key = pairing_channel_key();
        let dialer = context.ports.dialer.clone();
        let opened = tokio::time::timeout(
            context.timing.dial_timeout,
            dialer.dial(to, PAIRING_IDENTITY, &key),
        )
        .await
        .unwrap_or(Err(DialError::Timeout));
        let stream = match opened {
            Ok(stream) => stream,
            Err(error) => {
                tracing::debug!("wireless: pairing channel not opened: {error}");
                return Pairing::Retry;
            }
        };
        let heard = Arc::new(Heard::default());
        let stream = Watched {
            inner: stream,
            heard: heard.clone(),
        };
        // A phone whose pairing went stale is shown by the name it had.
        let known_name = context
            .store
            .records()
            .into_iter()
            .find(|r| r.peer_id == phone.instance)
            .map(|r| r.name);
        let local_id = context.store.local_id();
        let computer_name = crate::wireless::computer_name(self.computer_name.as_deref());
        tracing::info!("wireless: pairing by code with {}", phone.instance);
        let confirm = |code: String| async move {
            let (reply, answer) = oneshot::channel();
            let event = WirelessEvent::Confirm {
                code,
                phone_name: known_name,
                reply,
            };
            if !context.tell(event).await {
                return false;
            }
            // The actor dropping the question (the attempt is ending) is a "no".
            answer.await.unwrap_or(false)
        };
        let store = context.store.clone();
        let save = move |paired: &Paired| {
            let mut name = paired.phone_name.clone();
            if name.is_empty() {
                name = crate::wireless::cable_pairing::FALLBACK_PHONE_NAME.to_string();
            }
            let record = Record {
                peer_id: paired.phone_id.clone(),
                name,
                identity: paired.identity.clone(),
                created_at: unix_now(),
                method: Method::Code,
                stale: false,
            };
            let key = paired.long_term_key.clone();
            async move {
                // Replaces any earlier pairing with this phone (record and key).
                tokio::task::spawn_blocking(move || store.save(record, key))
                    .await
                    .unwrap_or_else(|error| Err(io::Error::other(error.to_string())))
            }
        };
        let result = code_pairing::run_saving(
            stream,
            &phone.instance,
            &local_id,
            &computer_name,
            confirm,
            save,
        )
        .await;
        match result {
            Ok(paired) => {
                tracing::info!("wireless: paired by code with {}", paired.phone_id);
                self.fresh = Some((paired.phone_id.clone(), Instant::now()));
                match context
                    .store
                    .records()
                    .into_iter()
                    .find(|r| r.peer_id == paired.phone_id && !r.stale)
                {
                    Some(record) => Pairing::Paired(record),
                    // Saved, yet not there: the store is not usable.
                    None => Pairing::End(WirelessFailure::PairingFailed),
                }
            }
            Err(CodePairingError::Denied) => {
                tracing::info!("wireless: pairing not allowed on the phone");
                Pairing::End(WirelessFailure::PairingDenied)
            }
            // Turned away: closed before its first line (the phone's lockout, or another pairing
            // on it), or closed cleanly after `P` without an answer (another question is on the
            // phone's screen; no strike there).
            Err(CodePairingError::Io(_)) if heard.turned_away() => Pairing::Busy,
            Err(error) => {
                tracing::warn!("wireless: code pairing failed: {error}");
                Pairing::End(WirelessFailure::PairingFailed)
            }
        }
    }

    /// Checks the pairing with one dial, then has the actor build the session over it.
    async fn dial(
        &mut self,
        phone: &FoundPhone,
        record: &Record,
        window: &Window,
        unknown_seen: &mut bool,
    ) -> Dialled {
        let context = self.context;
        let Some(key) = context.store.key(&record.identity) else {
            return Dialled::Retry;
        };
        if self.picked.as_deref() != Some(record.peer_id.as_str()) {
            self.unknowns.clear();
            self.picked = Some(record.peer_id.clone());
        }
        // How long ago this pairing was made by code in this attempt.
        let since_pairing = self
            .fresh
            .as_ref()
            .filter(|(peer, _)| *peer == record.peer_id)
            .map(|(_, at)| at.elapsed());
        let key = link_key(&key);
        let to = SocketAddr::new(phone.address, phone.port);
        let dialer = context.ports.dialer.clone();
        let checked = tokio::time::timeout(
            context.timing.dial_timeout,
            dialer.dial(to, &record.identity, &key),
        )
        .await
        .unwrap_or(Err(DialError::Timeout));
        let error = match checked {
            Ok(stream) => {
                drop(stream);
                self.unknowns.on_other(phone.address);
                // Verified only by a handshake with this pairing's own identity (never by the
                // pairing channel's public one).
                self.verified.insert(record.peer_id.clone(), phone.address);
                let link = Arc::new(TlsLink::new(
                    &record.peer_id,
                    to,
                    &record.identity,
                    key,
                    dialer,
                ));
                let (reply, answer) = oneshot::channel();
                if !context.tell(WirelessEvent::Establish { link, reply }).await {
                    return Dialled::End(WirelessFailure::Lost);
                }
                match answer.await {
                    Ok(Ok(ended)) => return Dialled::Connected(ended),
                    Ok(Err(NotEstablished::UnknownIdentity)) => DialError::UnknownIdentity,
                    Ok(Err(NotEstablished::Permanent(reason))) => {
                        return Dialled::End(WirelessFailure::PhoneRefused { reason });
                    }
                    Ok(Err(NotEstablished::Other)) => return Dialled::Retry,
                    // The actor dropped the question: it is ending the attempt.
                    Err(_) => return Dialled::Retry,
                }
            }
            Err(error) => error,
        };
        let now = Instant::now();
        if error == DialError::UnknownIdentity {
            *unknown_seen = true;
            self.unknowns.on_unknown(phone.address, now, since_pairing);
        } else {
            self.unknowns.on_other(phone.address);
        }
        tracing::debug!("wireless: dial failed: {error}");
        let dial = DialContext {
            since_pairing,
            verified: self.verified.get(&record.peer_id) == Some(&phone.address),
            unknown_span: self.unknowns.span(phone.address, now),
            window_over: now >= window.deadline,
        };
        match policy::after_dial_failure(&error, &dial) {
            Next::Retry => Dialled::Retry,
            Next::EndStale => {
                tracing::warn!("wireless: the phone no longer knows this pairing; marked stale");
                let store = context.store.clone();
                let peer = record.peer_id.clone();
                match tokio::task::spawn_blocking(move || store.set_stale(&peer, true)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!("wireless: marking stale: {error}"),
                    Err(error) => tracing::warn!("wireless: marking stale: {error}"),
                }
                self.marked_stale.insert(record.peer_id.clone());
                Dialled::End(WirelessFailure::PairingStale)
            }
            // The pairing made by code in this attempt did not take on the phone (refused at
            // an address verified for it, past its grace). The record is left as it is.
            Next::EndFailed => Dialled::End(WirelessFailure::PairingFailed),
            Next::EndRefused => Dialled::End(WirelessFailure::PairingRefused),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the phone sent on the pairing channel: whether anything, how many lines, and whether it
/// ended the stream cleanly.
#[derive(Default)]
struct Heard {
    anything: AtomicBool,
    lines: AtomicUsize,
    ended: AtomicBool,
}

impl Heard {
    /// The phone turned the pairing away rather than failing it: it closed before its first
    /// byte (busy or locked out), or closed cleanly right after its `P` line (the second) with
    /// no answer line (another question was showing).
    fn turned_away(&self) -> bool {
        !self.anything.load(Ordering::SeqCst)
            || (self.ended.load(Ordering::SeqCst) && self.lines.load(Ordering::SeqCst) == 2)
    }
}

/// The pairing channel, noting in `heard` what the phone sent.
struct Watched {
    inner: crate::mux::MuxStream,
    heard: Arc<Heard>,
}

impl AsyncRead for Watched {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let room = buf.remaining() > 0;
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            let new = &buf.filled()[before..];
            if new.is_empty() {
                if room {
                    this.heard.ended.store(true, Ordering::SeqCst);
                }
            } else {
                this.heard.anything.store(true, Ordering::SeqCst);
                let lines = new.iter().filter(|b| **b == b'\n').count();
                this.heard.lines.fetch_add(lines, Ordering::SeqCst);
            }
        }
        polled
    }
}

impl AsyncWrite for Watched {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
