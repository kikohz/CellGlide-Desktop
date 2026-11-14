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

use super::Event;
use crate::wireless::pairing::link_key;
use crate::wireless::policy::{
    self, DialContext, Next, Pick, RETRY_EVERY, RETRY_FOR, UnknownTracker, WAIT_FOR_PHONE,
};
use crate::wireless::ports::{DialError, FoundPhone, RunningAccessPoint, TlsLink, WirelessPorts};
use crate::wireless::store::{Record, WirelessStore};
use cg_ipc::wireless::{WirelessFailure, WirelessPhaseView};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
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
        }
    }
}

/// Where the wireless attempt is. Everything but `Off` and `Failed` owns the controller: the
/// state comes from here and not from usbmuxd, and no USB connection is attempted.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    },
    /// Found; checking the pairing and building the session.
    Dialing,
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
            WirelessPhase::Failed(reason) => WirelessPhaseView::Failed { reason: *reason },
        })
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

/// Why the actor could not build the session on a link the task had checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotEstablished {
    /// A dial inside it was refused with an unknown identity.
    UnknownIdentity,
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
    /// The attempt is over; the access point is already down.
    Done(WirelessFailure),
}

/// The attempt's task; aborted when dropped.
pub(super) struct Task(Option<JoinHandle<()>>);

impl Task {
    pub(super) fn spawn(context: Context) -> Task {
        Task(Some(tokio::spawn(run(context))))
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

pub(super) async fn run(context: Context) {
    let failure = drive(&context).await;
    tracing::info!("wireless: attempt ended: {failure:?}");
    context.tell(WirelessEvent::Done(failure)).await;
}

/// The attempt from start to failure; the access point lives here and nowhere else.
async fn drive(context: &Context) -> WirelessFailure {
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
    let mut attempt = Attempt::new(context);
    let mut window = Window {
        kind: WindowKind::Waiting { ssid },
        since: Instant::now(),
        deadline: Instant::now() + timing.wait_for_phone,
    };
    loop {
        let ended = match attempt.connect(access_point.as_mut(), &window).await {
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
    fn phase(&self, name_clash: bool) -> WirelessPhase {
        match &self.kind {
            WindowKind::Waiting { ssid } => WirelessPhase::Waiting {
                since: self.since,
                ssid: ssid.clone(),
                name_clash,
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
            WindowKind::Reconnecting => self.phase(name_clash),
        }
    }

    fn ran_out(&self, name_clash: bool, unknown_seen: bool) -> WirelessFailure {
        if name_clash {
            WirelessFailure::NameClash
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

/// What one `ConnectWireless` remembers between its windows.
struct Attempt<'a> {
    context: &'a Context,
    /// Phone id → the address a handshake with its pairing completed at, in this attempt.
    verified: HashMap<String, IpAddr>,
    /// Pairings that were good when the attempt began and that it marked stale.
    marked_stale: HashSet<String>,
    /// Runs of unknown identities, for the phone last picked.
    unknowns: UnknownTracker,
    picked: Option<String>,
}

impl<'a> Attempt<'a> {
    fn new(context: &'a Context) -> Attempt<'a> {
        Attempt {
            context,
            verified: HashMap::new(),
            marked_stale: HashSet::new(),
            unknowns: UnknownTracker::new(),
            picked: None,
        }
    }

    async fn tell(&self, phase: WirelessPhase) {
        self.context.tell(WirelessEvent::Phase(phase)).await;
    }

    /// Rounds of discovery and dialling until the session is built (its end receiver), the
    /// window runs out, or something ends the attempt.
    async fn connect(
        &mut self,
        access_point: &mut dyn RunningAccessPoint,
        window: &Window,
    ) -> Result<oneshot::Receiver<()>, WirelessFailure> {
        let timing = self.context.timing;
        let mut clash_rounds = 0u32;
        let mut unknown_seen = false;
        let mut shown_clash = false;
        self.tell(window.phase(false)).await;
        loop {
            let round = Instant::now();
            let name_clash = clash_rounds >= timing.clash_rounds;
            if round >= window.deadline {
                return Err(window.ran_out(name_clash, unknown_seen));
            }
            if name_clash != shown_clash {
                shown_clash = name_clash;
                self.tell(window.phase(name_clash)).await;
            }
            let local = access_point.local_address();
            let found = tokio::select! {
                found = self.browse(local) => found,
                () = access_point.lost() => return Err(lost()),
            };
            let records = self.context.store.records();
            let pick = policy::pick(&found, &records, &self.verified, &self.marked_stale);
            match pick {
                Pick::Paired { phone, record } => {
                    clash_rounds = 0;
                    shown_clash = false;
                    self.tell(window.dialing(false)).await;
                    let dialled = tokio::select! {
                        dialled = self.dial(&phone, &record, window, &mut unknown_seen) => dialled,
                        () = access_point.lost() => return Err(lost()),
                    };
                    match dialled {
                        Dialled::Connected(ended) => return Ok(ended),
                        Dialled::End(failure) => return Err(failure),
                        Dialled::Retry => self.tell(window.phase(false)).await,
                    }
                }
                Pick::Ambiguous { instance } => {
                    clash_rounds += 1;
                    tracing::debug!("wireless: {instance} is advertised from several addresses");
                }
                // Code pairing is not part of this build yet: keep waiting for a paired phone.
                Pick::Unpaired(_) | Pick::SeveralUnpaired => clash_rounds = 0,
                // A round that finds nothing proves nothing (discovery misses about a third).
                Pick::None => {}
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
            self.unknowns.on_unknown(phone.address, now, None);
        } else {
            self.unknowns.on_other(phone.address);
        }
        tracing::debug!("wireless: dial failed: {error}");
        let dial = DialContext {
            since_pairing: None,
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
            // `EndFailed` needs a pairing made by code in this attempt, which this build does
            // not make; either way the record is left alone.
            Next::EndFailed | Next::EndRefused => Dialled::End(WirelessFailure::PairingRefused),
        }
    }
}
