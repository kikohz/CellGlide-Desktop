//! In-memory access point, discovery and TLS dialer (`wireless::ports`), scriptable per test,
//! with phones behind them: a dial that succeeds reads the request line first, then
//! `CG1 DATA <port>` is spliced to a `FakePhone`'s port (reached through its `FakeMux` routes)
//! and `CG1 PAIR` to a phone-side code pairing responder built from `code_message`.
//!
//! One `FakeWireless` plays all three ports (`FakeWireless::ports`). Discovery rounds are
//! numbered from 0 by the order of `browse` calls; each advert says in which rounds it is seen
//! (`Rounds`). Dials go to the phone registered at the dialled address; scripts
//! (`script_dials`) can override any dial first.

use super::FakeMux;
use crate::mux::MuxStream;
use crate::wireless::code_message::{self as message, Side, Transcript};
use crate::wireless::pairing::Key;
use crate::wireless::pairing::{
    LinkKey, PAIRING_IDENTITY, Role, Session, link_key, pairing_channel_key,
};
use crate::wireless::ports::{
    AccessPoint, ApFailure, BoxFuture, Browser, DialError, Dialer, FoundPhone, RunningAccessPoint,
    WirelessPorts,
};
use crate::wireless::request::{MAX_REQUEST_LENGTH, Request};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// The access point's own address, as Windows gives it.
pub const ACCESS_POINT_ADDRESS: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 137, 1));
/// The identity the fake phone hands out on a successful code pairing, unless set otherwise.
pub const PAIRED_IDENTITY: &str = "5C1D2E3F-0000-4000-8000-000000000001";
/// The fake phone's name on a code pairing, unless set otherwise.
pub const PHONE_NAME: &str = "Test iPhone";
/// The phone waits this long for the request line (the real one gives handshake and line 5 s).
const REQUEST_LINE_TIMEOUT: Duration = Duration::from_secs(5);
/// Once one direction of a data connection has ended, the phone ends the other this much later.
pub const RELAY_LINGER: Duration = Duration::from_secs(5);

/// The discovery rounds (0, 1, 2, … in the order of `browse` calls) in which an advert is seen.
#[derive(Clone)]
pub struct Rounds(Arc<dyn Fn(usize) -> bool + Send + Sync>);

impl Rounds {
    pub fn all() -> Rounds {
        Rounds::when(|_| true)
    }

    pub fn never() -> Rounds {
        Rounds::when(|_| false)
    }

    /// From round `first` on.
    pub fn starting(first: usize) -> Rounds {
        Rounds::when(move |r| r >= first)
    }

    /// Rounds in `range` (half-open).
    pub fn between(range: Range<usize>) -> Rounds {
        Rounds::when(move |r| range.contains(&r))
    }

    pub fn only(rounds: impl IntoIterator<Item = usize>) -> Rounds {
        let rounds: Vec<usize> = rounds.into_iter().collect();
        Rounds::when(move |r| rounds.contains(&r))
    }

    pub fn when(f: impl Fn(usize) -> bool + Send + Sync + 'static) -> Rounds {
        Rounds(Arc::new(f))
    }

    /// These rounds, minus `missed` (discovery misses about a third of the rounds on hardware).
    pub fn except(self, missed: impl IntoIterator<Item = usize>) -> Rounds {
        let missed: Vec<usize> = missed.into_iter().collect();
        Rounds::when(move |r| (self.0)(r) && !missed.contains(&r))
    }

    pub fn contains(&self, round: usize) -> bool {
        (self.0)(round)
    }
}

impl std::fmt::Debug for Rounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Rounds(..)")
    }
}

/// How the fake phone answers the end of code pairing (after its `P` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingAnswer {
    /// `OK <identity> <name> <proof>`; a valid `F` then makes the phone accept the identity.
    Allow,
    /// `NO`.
    Deny,
    /// Nothing at all, until the computer hangs up.
    NeverAnswer,
    /// Closes right after the request line, as a phone in its pairing lockout.
    CloseAtOnce,
    /// Closes right after `P` without an answer line, as a phone with another question on its
    /// screen.
    CloseAfterP,
}

/// The fake phone's side of code pairing.
#[derive(Clone, Debug)]
pub struct PhonePairing {
    pub answer: PairingAnswer,
    /// The identity it chooses for the new pairing.
    pub identity: String,
    pub phone_name: String,
    /// Names this phone id on the `C` line instead of its own instance name.
    pub claim_phone_id: Option<String>,
    /// Commits to another key than the one it reveals in `P`.
    pub wrong_commitment: bool,
    /// Waits this long after `P` before answering.
    pub answer_after: Duration,
    /// After a valid `F` the new identity is still refused (unknown) for this long, as the
    /// phone reloading its keys.
    pub accept_after: Duration,
}

impl Default for PhonePairing {
    fn default() -> Self {
        PhonePairing {
            answer: PairingAnswer::Allow,
            identity: PAIRED_IDENTITY.into(),
            phone_name: PHONE_NAME.into(),
            claim_phone_id: None,
            wrong_commitment: false,
            answer_after: Duration::ZERO,
            accept_after: Duration::ZERO,
        }
    }
}

/// A phone reachable at one address on the fake access point.
#[derive(Clone, Debug)]
pub struct WirelessPhone {
    instance: String,
    address: IpAddr,
    port: u16,
    routes: HashMap<u16, SocketAddr>,
    /// identity → (link key, accepted from)
    accepted: HashMap<String, (LinkKey, Instant)>,
    pairing: PhonePairing,
    impostor: bool,
}

impl WirelessPhone {
    pub fn new(instance: &str, address: IpAddr) -> WirelessPhone {
        WirelessPhone {
            instance: instance.into(),
            address,
            port: 56582,
            routes: HashMap::new(),
            accepted: HashMap::new(),
            pairing: PhonePairing::default(),
            impostor: false,
        }
    }

    /// Serves the ports of the fake phone behind `mux` (its routes as they are now).
    pub fn serving(mut self, mux: &FakeMux) -> WirelessPhone {
        self.routes = mux.routes();
        self
    }

    /// Already knows this pairing.
    pub fn paired(mut self, identity: &str, long_term: &Key) -> WirelessPhone {
        self.accepted
            .insert(identity.into(), (link_key(long_term), Instant::now()));
        self
    }

    pub fn pairing(mut self, pairing: PhonePairing) -> WirelessPhone {
        self.pairing = pairing;
        self
    }

    /// Something on the access point posing as this phone: every data dial gets
    /// `UnknownIdentity`, and it never finishes a code pairing.
    pub fn impostor(mut self) -> WirelessPhone {
        self.impostor = true;
        self.pairing.answer = PairingAnswer::NeverAnswer;
        self
    }

    pub fn port(mut self, port: u16) -> WirelessPhone {
        self.port = port;
        self
    }
}

/// Which dials a script applies to; `None` matches anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialMatch {
    pub address: Option<IpAddr>,
    pub identity: Option<String>,
}

impl DialMatch {
    pub fn any() -> DialMatch {
        DialMatch::default()
    }

    pub fn address(address: IpAddr) -> DialMatch {
        DialMatch {
            address: Some(address),
            identity: None,
        }
    }

    pub fn identity(identity: &str) -> DialMatch {
        DialMatch {
            address: None,
            identity: Some(identity.into()),
        }
    }

    fn matches(&self, to: SocketAddr, identity: &str) -> bool {
        self.address.is_none_or(|a| a == to.ip())
            && self.identity.as_deref().is_none_or(|i| i == identity)
    }
}

/// What one scripted dial does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialOutcome {
    Fail(DialError),
    /// Never completes (the caller's time limit ends it).
    Hang,
    /// Waits, then dials normally.
    Delay(Duration),
    /// Dials normally (lets a later outcome in the queue apply to a later dial).
    Normal,
}

/// One dial, as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dial {
    pub to: SocketAddr,
    pub identity: String,
    /// The first line the phone read (without its newline), once read.
    pub request_line: Option<String>,
}

/// What the fake phone saw of one code pairing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairingReport {
    /// The phone's instance name.
    pub phone: String,
    pub got_m: bool,
    pub computer_id: Option<String>,
    pub computer_name: Option<String>,
    /// The code the phone showed.
    pub code: Option<String>,
    /// `None`: no `F` came. `Some(true)`: a valid one (the phone then saved the pairing).
    pub f_valid: Option<bool>,
}

#[derive(Debug)]
struct Advert {
    phone: FoundPhone,
    rounds: Rounds,
}

#[derive(Default)]
struct State {
    // Access point.
    failures_once: VecDeque<ApFailure>,
    failure: Option<ApFailure>,
    start_delay: Duration,
    /// `None`: the default address.
    local_address: Option<Option<IpAddr>>,
    extra_clients: Vec<IpAddr>,
    start_attempts: usize,
    started_with: Vec<(String, String)>,
    starts: usize,
    stops: usize,
    lost: Vec<watch::Sender<bool>>,
    // Discovery.
    adverts: Vec<Advert>,
    browse_errors: Option<Rounds>,
    browse_delay: Option<Duration>,
    browses: Vec<IpAddr>,
    // Dialing.
    phones: Vec<WirelessPhone>,
    absent: Vec<IpAddr>,
    scripts: Vec<(DialMatch, VecDeque<DialOutcome>)>,
    dials: Vec<Dial>,
    connections: Vec<(IpAddr, JoinHandle<()>)>,
    pairings: Vec<PairingReport>,
}

/// See the module docs.
pub struct FakeWireless {
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|p| p.into_inner())
}

impl FakeWireless {
    pub fn new() -> Arc<FakeWireless> {
        Arc::new(FakeWireless {
            state: Arc::new(Mutex::new(State::default())),
        })
    }

    /// The three ports, all this fake.
    pub fn ports(self: &Arc<Self>) -> WirelessPorts {
        WirelessPorts {
            access_point: self.clone(),
            browser: self.clone(),
            dialer: self.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    // --- Access point: scripting.

    /// Every start fails this way until set back to `None`.
    pub fn fail_starts(&self, failure: Option<ApFailure>) {
        self.lock().failure = failure;
    }

    /// The next start fails this way (queued, before `fail_starts`).
    pub fn fail_next_start(&self, failure: ApFailure) {
        self.lock().failures_once.push_back(failure);
    }

    /// Each start takes this long (the real one takes up to 10 s).
    pub fn set_start_delay(&self, delay: Duration) {
        self.lock().start_delay = delay;
    }

    /// `local_address()` of access points started from now on (`ACCESS_POINT_ADDRESS` until
    /// set); `None` for not known yet.
    pub fn set_local_address(&self, address: Option<IpAddr>) {
        self.lock().local_address = Some(address);
    }

    /// Devices joined to the access point besides the advertised phones.
    pub fn add_client(&self, address: IpAddr) {
        self.lock().extra_clients.push(address);
    }

    /// The system aborts every running access point: their `lost()` completes.
    pub fn lose_access_point(&self) {
        for lost in &self.lock().lost {
            let _ = lost.send(true);
        }
    }

    // --- Access point: records.

    pub fn start_attempts(&self) -> usize {
        self.lock().start_attempts
    }

    /// Successful starts.
    pub fn starts(&self) -> usize {
        self.lock().starts
    }

    /// Running access points dropped (stopped).
    pub fn stops(&self) -> usize {
        self.lock().stops
    }

    /// Whether an access point is running (started and not dropped; a lost one still counts
    /// until it is dropped).
    pub fn is_up(&self) -> bool {
        let s = self.lock();
        s.starts > s.stops
    }

    /// The (network name, passphrase) of each start attempt.
    pub fn started_with(&self) -> Vec<(String, String)> {
        self.lock().started_with.clone()
    }

    // --- Discovery: scripting and records.

    /// `phone` answers dials at its address, and is advertised in `rounds`.
    pub fn add_phone(&self, phone: WirelessPhone, rounds: Rounds) {
        let mut s = self.lock();
        s.adverts.push(Advert {
            phone: FoundPhone {
                instance: phone.instance.clone(),
                address: phone.address,
                port: phone.port,
            },
            rounds,
        });
        s.phones.retain(|p| p.address != phone.address);
        s.phones.push(phone);
    }

    /// Something at `address` advertising `instance` (another phone's name) whose every data
    /// dial gets `UnknownIdentity`.
    pub fn add_impostor(&self, instance: &str, address: IpAddr, rounds: Rounds) {
        self.add_phone(WirelessPhone::new(instance, address).impostor(), rounds);
    }

    /// An advert with no phone behind it (dials to it time out unless a phone is added there).
    pub fn advertise(&self, phone: FoundPhone, rounds: Rounds) {
        self.lock().adverts.push(Advert { phone, rounds });
    }

    /// Stops advertising `instance` at `address`.
    pub fn stop_advertising(&self, instance: &str, address: IpAddr) {
        self.lock()
            .adverts
            .retain(|a| !(a.phone.instance == instance && a.phone.address == address));
    }

    /// Browsing fails (an I/O error) in these rounds.
    pub fn fail_browse(&self, rounds: Rounds) {
        self.lock().browse_errors = Some(rounds);
    }

    /// Each round takes this long; by default the timeout the caller passes.
    pub fn set_browse_delay(&self, delay: Duration) {
        self.lock().browse_delay = Some(delay);
    }

    /// Discovery rounds run so far.
    pub fn browse_count(&self) -> usize {
        self.lock().browses.len()
    }

    /// The `local` address each round was asked to browse on.
    pub fn browsed_on(&self) -> Vec<IpAddr> {
        self.lock().browses.clone()
    }

    // --- Dialing: scripting and records.

    /// The next dials matching `matching` take `outcomes`, one each, before anything else.
    pub fn script_dials(
        &self,
        matching: DialMatch,
        outcomes: impl IntoIterator<Item = DialOutcome>,
    ) {
        self.lock()
            .scripts
            .push((matching, outcomes.into_iter().collect()));
    }

    /// The phone at `address` leaves: its connections drop and dials to it time out.
    pub fn phone_leaves(&self, address: IpAddr) {
        self.lock().absent.push(address);
        self.drop_connections(address);
    }

    /// The phone at `address` is reachable again.
    pub fn phone_returns(&self, address: IpAddr) {
        self.lock().absent.retain(|a| *a != address);
    }

    /// Cuts every open connection to `address` (the computer's end reads EOF).
    pub fn drop_connections(&self, address: IpAddr) {
        let mut s = self.lock();
        for (_, task) in s.connections.iter().filter(|(a, _)| *a == address) {
            task.abort();
        }
        s.connections.retain(|(a, _)| *a != address);
    }

    /// Every dial so far, in order.
    pub fn dials(&self) -> Vec<Dial> {
        self.lock().dials.clone()
    }

    /// The identities dialled so far, in order.
    pub fn dialled_identities(&self) -> Vec<String> {
        self.lock()
            .dials
            .iter()
            .map(|d| d.identity.clone())
            .collect()
    }

    /// Every code pairing a fake phone took part in, in order.
    pub fn pairings(&self) -> Vec<PairingReport> {
        self.lock().pairings.clone()
    }

    /// Whether the phone at `address` now accepts `identity` (after its `accept_after`).
    pub fn accepts(&self, address: IpAddr, identity: &str) -> bool {
        let s = self.lock();
        s.phones
            .iter()
            .find(|p| p.address == address)
            .and_then(|p| p.accepted.get(identity))
            .is_some_and(|(_, from)| Instant::now() >= *from)
    }

    fn connect(
        &self,
        to: SocketAddr,
        identity: &str,
        psk: &LinkKey,
        index: usize,
    ) -> Result<MuxStream, DialError> {
        let mut s = self.lock();
        if s.absent.contains(&to.ip()) {
            return Err(DialError::Timeout);
        }
        let Some(phone) = s.phones.iter().find(|p| p.address == to.ip()).cloned() else {
            return Err(DialError::Timeout);
        };
        let mode = if identity == PAIRING_IDENTITY {
            if *psk != pairing_channel_key() {
                return Err(DialError::Io("handshake failed".into()));
            }
            Mode::Pair
        } else {
            if phone.impostor {
                return Err(DialError::UnknownIdentity);
            }
            match phone.accepted.get(identity) {
                Some((_, from)) if Instant::now() < *from => {
                    return Err(DialError::UnknownIdentity);
                }
                Some((key, _)) if key == psk => Mode::Data,
                Some(_) => return Err(DialError::Io("handshake failed".into())),
                None => return Err(DialError::UnknownIdentity),
            }
        };
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let state = self.state.clone();
        let task = tokio::spawn(serve(theirs, mode, phone, state, index));
        s.connections.push((to.ip(), task));
        Ok(Box::new(ours))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Data,
    Pair,
}

impl Dialer for FakeWireless {
    fn dial<'a>(
        &'a self,
        to: SocketAddr,
        identity: &'a str,
        psk: &'a LinkKey,
    ) -> BoxFuture<'a, Result<MuxStream, DialError>> {
        let (index, scripted) = {
            let mut s = self.lock();
            s.dials.push(Dial {
                to,
                identity: identity.into(),
                request_line: None,
            });
            let index = s.dials.len() - 1;
            let scripted = s
                .scripts
                .iter_mut()
                .find(|(m, outcomes)| !outcomes.is_empty() && m.matches(to, identity))
                .and_then(|(_, outcomes)| outcomes.pop_front());
            (index, scripted)
        };
        Box::pin(async move {
            match scripted {
                Some(DialOutcome::Fail(error)) => return Err(error),
                Some(DialOutcome::Hang) => std::future::pending::<()>().await,
                Some(DialOutcome::Delay(delay)) => tokio::time::sleep(delay).await,
                Some(DialOutcome::Normal) | None => {}
            }
            self.connect(to, identity, psk, index)
        })
    }
}

impl Browser for FakeWireless {
    fn browse(
        &self,
        local: IpAddr,
        timeout: Duration,
    ) -> BoxFuture<'_, io::Result<Vec<FoundPhone>>> {
        let (round, delay) = {
            let mut s = self.lock();
            s.browses.push(local);
            (s.browses.len() - 1, s.browse_delay.unwrap_or(timeout))
        };
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            let s = self.lock();
            if s.browse_errors.as_ref().is_some_and(|r| r.contains(round)) {
                return Err(io::Error::other("scripted discovery failure"));
            }
            Ok(s.adverts
                .iter()
                .filter(|a| a.rounds.contains(round))
                .map(|a| a.phone.clone())
                .collect())
        })
    }
}

impl AccessPoint for FakeWireless {
    fn start<'a>(
        &'a self,
        ssid: &'a str,
        passphrase: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn RunningAccessPoint>, ApFailure>> {
        let delay = {
            let mut s = self.lock();
            s.start_attempts += 1;
            s.started_with.push((ssid.into(), passphrase.into()));
            s.start_delay
        };
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            let mut s = self.lock();
            if let Some(failure) = s.failures_once.pop_front().or(s.failure) {
                return Err(failure);
            }
            s.starts += 1;
            let (lost, watching) = watch::channel(false);
            s.lost.push(lost);
            let local = s.local_address.unwrap_or(Some(ACCESS_POINT_ADDRESS));
            Ok(Box::new(FakeRunningAccessPoint {
                state: self.state.clone(),
                local,
                lost: watching,
            }) as Box<dyn RunningAccessPoint>)
        })
    }
}

struct FakeRunningAccessPoint {
    state: Arc<Mutex<State>>,
    local: Option<IpAddr>,
    lost: watch::Receiver<bool>,
}

impl RunningAccessPoint for FakeRunningAccessPoint {
    fn local_address(&self) -> Option<IpAddr> {
        self.local
    }

    fn clients(&self) -> Vec<IpAddr> {
        let s = lock(&self.state);
        let mut clients: Vec<IpAddr> = Vec::new();
        for address in s
            .adverts
            .iter()
            .map(|a| a.phone.address)
            .chain(s.extra_clients.iter().copied())
        {
            if !clients.contains(&address) {
                clients.push(address);
            }
        }
        clients
    }

    fn lost(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.lost.wait_for(|lost| *lost).await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}

impl Drop for FakeRunningAccessPoint {
    fn drop(&mut self) {
        lock(&self.state).stops += 1;
    }
}

/// Copies both ways, as the phone's relay: when one direction ends (its sender half-closed or
/// failed) the other may go on for `RELAY_LINGER`, then the whole connection is closed.
async fn relay(stream: DuplexStream, upstream: tokio::net::TcpStream) {
    let (mut from_computer, mut to_computer) = tokio::io::split(stream);
    let (mut from_phone, mut to_phone) = upstream.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut from_computer, &mut to_phone).await;
        let _ = to_phone.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut from_phone, &mut to_computer).await;
        let _ = to_computer.shutdown().await;
    };
    tokio::pin!(up, down);
    tokio::select! {
        () = &mut up => { let _ = tokio::time::timeout(RELAY_LINGER, &mut down).await; }
        () = &mut down => { let _ = tokio::time::timeout(RELAY_LINGER, &mut up).await; }
    }
}

/// The request line, byte by byte so nothing after it is consumed. `None` for anything else
/// than a line within `MAX_REQUEST_LENGTH`.
async fn request_line<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut line = Vec::new();
    while line.len() < MAX_REQUEST_LENGTH {
        let byte = stream.read_u8().await.ok()?;
        if byte == b'\n' {
            return String::from_utf8(line).ok();
        }
        line.push(byte);
    }
    None
}

async fn serve(
    mut stream: DuplexStream,
    mode: Mode,
    phone: WirelessPhone,
    state: Arc<Mutex<State>>,
    index: usize,
) {
    let Ok(Some(line)) =
        tokio::time::timeout(REQUEST_LINE_TIMEOUT, request_line(&mut stream)).await
    else {
        return;
    };
    lock(&state).dials[index].request_line = Some(line.clone());
    match (mode, Request::parse(&line)) {
        (Mode::Data, Some(Request::Data(port))) => {
            let Some(target) = phone.routes.get(&port).copied() else {
                return;
            };
            let Ok(upstream) = tokio::net::TcpStream::connect(target).await else {
                return;
            };
            relay(stream, upstream).await;
        }
        (Mode::Pair, Some(Request::Pair)) => pair(stream, phone, state).await,
        // The phone closes a request that does not fit the identity.
        _ => {}
    }
}

async fn read_line<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
    let mut line = String::new();
    let n = r.read_line(&mut line).await.ok()?;
    (n > 0 && line.ends_with('\n')).then_some(line)
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, text: &str) {
    let _ = w.write_all(text.as_bytes()).await;
    let _ = w.flush().await;
}

/// The phone's side of code pairing, after `CG1 PAIR`.
async fn pair(stream: DuplexStream, phone: WirelessPhone, state: Arc<Mutex<State>>) {
    let script = phone.pairing.clone();
    if script.answer == PairingAnswer::CloseAtOnce {
        return;
    }
    let report = {
        let mut s = lock(&state);
        s.pairings.push(PairingReport {
            phone: phone.instance.clone(),
            ..PairingReport::default()
        });
        s.pairings.len() - 1
    };
    let update = |f: &dyn Fn(&mut PairingReport)| f(&mut lock(&state).pairings[report]);

    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    let mut transcript = Transcript::new();
    let session = Session::new(Role::Phone);
    let commitment = if script.wrong_commitment {
        Session::new(Role::Phone).commitment()
    } else {
        session.commitment()
    };
    let claimed = script.claim_phone_id.as_deref().unwrap_or(&phone.instance);
    let c = message::commitment(&commitment, claimed);
    transcript.add(&c);
    send(&mut w, &c).await;

    let Ok(Some(m_line)) =
        tokio::time::timeout(message::FIRST_STEP_TIMEOUT, read_line(&mut r)).await
    else {
        return;
    };
    transcript.add(&m_line);
    let Some(m) = message::parse_computer(m_line.trim_end_matches('\n')) else {
        return;
    };
    update(&|p| {
        p.got_m = true;
        p.computer_id = Some(m.computer_id.clone());
        p.computer_name = Some(m.computer_name.clone());
    });
    let p_line = message::phone(&session.public_key(), &session.nonce());
    transcript.add(&p_line);
    send(&mut w, &p_line).await;
    let Ok(agreed) = session.agree(&m.public_key, &m.nonce) else {
        return;
    };
    update(&|p| p.code = Some(agreed.code.clone()));
    if script.answer == PairingAnswer::CloseAfterP {
        return;
    }

    tokio::time::sleep(script.answer_after).await;
    match script.answer {
        PairingAnswer::Allow => {}
        PairingAnswer::Deny => {
            send(&mut w, message::DENIED).await;
            return;
        }
        PairingAnswer::NeverAnswer | PairingAnswer::CloseAtOnce | PairingAnswer::CloseAfterP => {
            while read_line(&mut r).await.is_some() {}
            return;
        }
    }
    let signed = message::allowed_signed(&script.identity, &script.phone_name);
    transcript.add(&signed);
    let proof = message::proof(Side::Phone, &agreed.long_term_key, &transcript);
    send(
        &mut w,
        &message::allowed(&script.identity, &script.phone_name, &proof),
    )
    .await;
    let Some(f) = read_line(&mut r).await else {
        return;
    };
    let valid = message::parse_finished(f.trim_end_matches('\n')).is_some_and(|proof| {
        message::verify(&proof, Side::Computer, &agreed.long_term_key, &transcript)
    });
    update(&|p| p.f_valid = Some(valid));
    if valid {
        let mut s = lock(&state);
        if let Some(entry) = s.phones.iter_mut().find(|p| p.address == phone.address) {
            entry.accepted.insert(
                script.identity.clone(),
                (
                    link_key(&agreed.long_term_key),
                    Instant::now() + script.accept_after,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::{Phone, read_hello};
    use crate::testing::{FakePhone, FakePhoneOptions, SESSION_A};
    use crate::wireless::code_pairing::{self, CodePairingError};
    use crate::wireless::ports::{TlsLink, WirelessLink};

    const PHONE: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";
    const COMPUTER: &str = "7C1E5B52-2D6A-4A0F-8F3B-0A9D5E4C1B22";

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 168, 137, last])
    }

    fn at(last: u8) -> SocketAddr {
        SocketAddr::new(ip(last), 56582)
    }

    fn long_term() -> Key {
        Key::from_bytes([3; 32])
    }

    async fn phone_behind_mux() -> (FakeMux, FakePhone) {
        let mux = FakeMux::start().await;
        let phone = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        (mux, phone)
    }

    fn paired_phone(mux: &FakeMux) -> WirelessPhone {
        WirelessPhone::new(PHONE, ip(205))
            .serving(mux)
            .paired(IDENTITY, &long_term())
    }

    #[tokio::test]
    async fn the_access_point_counts_starts_and_stops() {
        let fake = FakeWireless::new();
        let ports = fake.ports();
        assert!(!fake.is_up());
        let running = ports
            .access_point
            .start("CellGlide-AB12", "pass")
            .await
            .unwrap();
        assert!(fake.is_up());
        assert_eq!(running.local_address(), Some(ACCESS_POINT_ADDRESS));
        assert_eq!(
            (fake.start_attempts(), fake.starts(), fake.stops()),
            (1, 1, 0)
        );
        drop(running);
        assert!(!fake.is_up());
        assert_eq!(fake.stops(), 1);
        assert_eq!(
            fake.started_with(),
            vec![("CellGlide-AB12".to_string(), "pass".to_string())]
        );
    }

    #[tokio::test]
    async fn start_failures_once_and_for_good() {
        let fake = FakeWireless::new();
        let ap = fake.ports().access_point;
        fake.fail_next_start(ApFailure::MobileHotspotOn);
        fake.fail_starts(Some(ApFailure::RadioOff));
        assert_eq!(
            ap.start("s", "p").await.err(),
            Some(ApFailure::MobileHotspotOn)
        );
        assert_eq!(ap.start("s", "p").await.err(), Some(ApFailure::RadioOff));
        assert_eq!(ap.start("s", "p").await.err(), Some(ApFailure::RadioOff));
        fake.fail_starts(None);
        let running = ap.start("s", "p").await.unwrap();
        assert_eq!((fake.start_attempts(), fake.starts()), (4, 1));
        assert!(fake.is_up());
        drop(running);
        assert_eq!(fake.stops(), 1, "a failed start leaves nothing to stop");
    }

    #[tokio::test(start_paused = true)]
    async fn lost_completes_only_when_the_access_point_is_lost() {
        let fake = FakeWireless::new();
        fake.set_local_address(None);
        fake.add_client(ip(40));
        let mut running = fake.ports().access_point.start("s", "p").await.unwrap();
        assert_eq!(running.local_address(), None);
        assert_eq!(running.clients(), vec![ip(40)]);
        assert!(
            tokio::time::timeout(Duration::from_secs(60), running.lost())
                .await
                .is_err()
        );
        fake.lose_access_point();
        tokio::time::timeout(Duration::from_secs(1), running.lost())
            .await
            .expect("lost");
        assert!(fake.is_up(), "still up until dropped");
        drop(running);
        assert!(!fake.is_up());
    }

    #[tokio::test(start_paused = true)]
    async fn browse_rounds_follow_the_script() {
        let fake = FakeWireless::new();
        let browser = fake.ports().browser;
        fake.add_phone(
            WirelessPhone::new(PHONE, ip(205)),
            Rounds::starting(2).except([3]),
        );
        fake.add_impostor(PHONE, ip(66), Rounds::only([4]));
        fake.fail_browse(Rounds::only([5]));
        let mut seen = Vec::new();
        for _ in 0..6 {
            let started = Instant::now();
            let round = browser
                .browse(ACCESS_POINT_ADDRESS, Duration::from_secs(2))
                .await;
            assert_eq!(
                started.elapsed(),
                Duration::from_secs(2),
                "takes the timeout"
            );
            seen.push(
                round
                    .ok()
                    .map(|found| found.iter().map(|p| p.address).collect::<Vec<_>>()),
            );
        }
        assert_eq!(
            seen,
            vec![
                Some(vec![]),
                Some(vec![]),
                Some(vec![ip(205)]),
                Some(vec![]),
                Some(vec![ip(205), ip(66)]),
                None,
            ]
        );
        assert_eq!(fake.browse_count(), 6);
        assert!(fake.browsed_on().iter().all(|a| *a == ACCESS_POINT_ADDRESS));
        fake.set_browse_delay(Duration::ZERO);
        let started = Instant::now();
        let found = browser
            .browse(ACCESS_POINT_ADDRESS, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(found[0].instance, PHONE);
        assert_eq!(found[0].port, 56582);
        fake.stop_advertising(PHONE, ip(205));
        let found = browser
            .browse(ACCESS_POINT_ADDRESS, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn a_data_dial_reaches_the_fake_phone_through_tls_link() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        fake.add_phone(paired_phone(&mux), Rounds::all());
        let link = TlsLink::new(
            PHONE,
            at(205),
            IDENTITY,
            link_key(&long_term()),
            fake.ports().dialer,
        );
        let phone = Phone::wireless(Arc::new(link));
        assert_eq!(phone.id(), format!("wireless:{PHONE}"));
        let hello = read_hello(&phone).await.unwrap();
        assert_eq!(hello.session_id, SESSION_A);
        assert_eq!(
            fake.dials(),
            vec![Dial {
                to: at(205),
                identity: IDENTITY.into(),
                request_line: Some("CG1 DATA 8088".into())
            }]
        );
        assert_eq!(fake.dialled_identities(), vec![IDENTITY.to_string()]);
    }

    #[tokio::test]
    async fn the_wrong_identity_an_impostor_and_an_absent_phone() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        let dialer = fake.ports().dialer;
        fake.add_phone(paired_phone(&mux), Rounds::all());
        fake.add_impostor(PHONE, ip(66), Rounds::all());
        let key = link_key(&long_term());
        let other = link_key(&Key::from_bytes([4; 32]));
        assert_eq!(
            dialer.dial(at(205), "SOMEONE-ELSE", &key).await.err(),
            Some(DialError::UnknownIdentity)
        );
        assert!(matches!(
            dialer.dial(at(205), IDENTITY, &other).await.err(),
            Some(DialError::Io(_))
        ));
        assert_eq!(
            dialer.dial(at(66), IDENTITY, &key).await.err(),
            Some(DialError::UnknownIdentity)
        );
        assert_eq!(
            dialer.dial(at(9), IDENTITY, &key).await.err(),
            Some(DialError::Timeout)
        );
        fake.phone_leaves(ip(205));
        assert_eq!(
            dialer.dial(at(205), IDENTITY, &key).await.err(),
            Some(DialError::Timeout)
        );
        fake.phone_returns(ip(205));
        assert!(dialer.dial(at(205), IDENTITY, &key).await.is_ok());
        assert_eq!(fake.dials().len(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn scripted_dials_come_first() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        let dialer = fake.ports().dialer;
        fake.add_phone(paired_phone(&mux), Rounds::all());
        fake.script_dials(
            DialMatch::address(ip(205)),
            [
                DialOutcome::Fail(DialError::UnknownIdentity),
                DialOutcome::Normal,
                DialOutcome::Delay(Duration::from_secs(3)),
                DialOutcome::Hang,
            ],
        );
        // Another identity's script does not apply.
        fake.script_dials(
            DialMatch::identity("SOMEONE-ELSE"),
            [DialOutcome::Fail(DialError::Timeout)],
        );
        let key = link_key(&long_term());
        assert_eq!(
            dialer.dial(at(205), IDENTITY, &key).await.err(),
            Some(DialError::UnknownIdentity)
        );
        assert!(dialer.dial(at(205), IDENTITY, &key).await.is_ok());
        let started = Instant::now();
        assert!(dialer.dial(at(205), IDENTITY, &key).await.is_ok());
        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert!(
            tokio::time::timeout(
                Duration::from_secs(30),
                dialer.dial(at(205), IDENTITY, &key)
            )
            .await
            .is_err()
        );
        // The script is used up.
        assert!(dialer.dial(at(205), IDENTITY, &key).await.is_ok());
    }

    #[tokio::test]
    async fn dropped_connections_read_eof() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        fake.add_phone(paired_phone(&mux), Rounds::all());
        let link = TlsLink::new(
            PHONE,
            at(205),
            IDENTITY,
            link_key(&long_term()),
            fake.ports().dialer,
        );
        // 9876 is the fake phone's SOCKS: it answers a greeting.
        let mut socks = link.open(9876).await.unwrap();
        socks.write_all(&[5, 1, 0]).await.unwrap();
        let mut reply = [0u8; 2];
        socks.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 0]);
        fake.drop_connections(ip(205));
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), socks.read_to_end(&mut rest)).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
    }

    async fn pair_with(
        fake: &Arc<FakeWireless>,
        confirm: bool,
    ) -> Result<code_pairing::Paired, CodePairingError> {
        let stream = fake
            .ports()
            .dialer
            .dial(at(205), PAIRING_IDENTITY, &pairing_channel_key())
            .await
            .unwrap();
        code_pairing::run(
            stream,
            PHONE,
            COMPUTER,
            "Desk PC",
            |_code| async move { confirm },
            |_paired| Ok(()),
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn code_pairing_against_the_fake_phone() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        fake.add_phone(
            WirelessPhone::new(PHONE, ip(205))
                .serving(&mux)
                .pairing(PhonePairing {
                    accept_after: Duration::from_secs(3),
                    ..PhonePairing::default()
                }),
            Rounds::all(),
        );
        let shown = Arc::new(Mutex::new(None));
        let stream = fake
            .ports()
            .dialer
            .dial(at(205), PAIRING_IDENTITY, &pairing_channel_key())
            .await
            .unwrap();
        let seen = shown.clone();
        let paired = code_pairing::run(
            stream,
            PHONE,
            COMPUTER,
            "Desk PC",
            move |code| {
                *seen.lock().unwrap() = Some(code);
                async { true }
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(paired.identity, PAIRED_IDENTITY);
        assert_eq!(paired.phone_name, PHONE_NAME);
        // The phone reads F after `run` has returned.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let report = fake.pairings().pop().unwrap();
        assert_eq!(report.phone, PHONE);
        assert_eq!(report.code, shown.lock().unwrap().clone());
        assert!(report.code.is_some());
        assert_eq!(report.computer_id.as_deref(), Some(COMPUTER));
        assert_eq!(report.computer_name.as_deref(), Some("Desk PC"));
        assert_eq!(report.f_valid, Some(true));
        assert_eq!(fake.dials()[0].request_line.as_deref(), Some("CG1 PAIR"));

        // The new identity works once the phone has reloaded its keys.
        let key = link_key(&paired.long_term_key);
        let dialer = fake.ports().dialer;
        assert!(!fake.accepts(ip(205), PAIRED_IDENTITY));
        assert_eq!(
            dialer.dial(at(205), PAIRED_IDENTITY, &key).await.err(),
            Some(DialError::UnknownIdentity)
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(fake.accepts(ip(205), PAIRED_IDENTITY));
        assert!(dialer.dial(at(205), PAIRED_IDENTITY, &key).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn the_fake_phone_can_refuse_a_pairing_in_every_way() {
        type Check = fn(&CodePairingError) -> bool;
        let cases: [(PhonePairing, Check); 5] = [
            (
                PhonePairing {
                    answer: PairingAnswer::Deny,
                    ..PhonePairing::default()
                },
                |e| *e == CodePairingError::Denied,
            ),
            (
                PhonePairing {
                    wrong_commitment: true,
                    ..PhonePairing::default()
                },
                |e| *e == CodePairingError::BadCommitment,
            ),
            (
                PhonePairing {
                    claim_phone_id: Some("SOMEONE-ELSE".into()),
                    ..PhonePairing::default()
                },
                |e| *e == CodePairingError::WrongPhone,
            ),
            (
                PhonePairing {
                    answer: PairingAnswer::NeverAnswer,
                    ..PhonePairing::default()
                },
                |e| *e == CodePairingError::Timeout,
            ),
            (
                PhonePairing {
                    answer: PairingAnswer::CloseAtOnce,
                    ..PhonePairing::default()
                },
                |e| matches!(e, CodePairingError::Io(_)),
            ),
        ];
        for (script, check) in cases {
            let (mux, _phone) = phone_behind_mux().await;
            let fake = FakeWireless::new();
            fake.add_phone(
                WirelessPhone::new(PHONE, ip(205))
                    .serving(&mux)
                    .pairing(script.clone()),
                Rounds::all(),
            );
            let error = pair_with(&fake, true).await.unwrap_err();
            assert!(check(&error), "{script:?}: {error:?}");
            assert!(!fake.accepts(ip(205), PAIRED_IDENTITY));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_pairing_this_computer_does_not_confirm_saves_nothing_on_the_phone() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        fake.add_phone(
            WirelessPhone::new(PHONE, ip(205)).serving(&mux),
            Rounds::all(),
        );
        let error = pair_with(&fake, false).await.unwrap_err();
        assert_eq!(error, CodePairingError::Cancelled);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(fake.pairings()[0].f_valid, None);
        assert!(!fake.accepts(ip(205), PAIRED_IDENTITY));
    }

    #[tokio::test]
    async fn a_request_that_does_not_fit_the_identity_is_closed() {
        let (mux, _phone) = phone_behind_mux().await;
        let fake = FakeWireless::new();
        let dialer = fake.ports().dialer;
        fake.add_phone(paired_phone(&mux), Rounds::all());
        // PAIR on a data identity, DATA on the pairing channel.
        for (identity, key, line) in [
            (IDENTITY, link_key(&long_term()), "CG1 PAIR\n"),
            (PAIRING_IDENTITY, pairing_channel_key(), "CG1 DATA 8088\n"),
        ] {
            let mut stream = dialer.dial(at(205), identity, &key).await.unwrap();
            stream.write_all(line.as_bytes()).await.unwrap();
            let mut rest = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
            assert!(rest.is_empty());
        }
        let lines: Vec<_> = fake.dials().into_iter().map(|d| d.request_line).collect();
        assert_eq!(
            lines,
            vec![
                Some("CG1 PAIR".to_string()),
                Some("CG1 DATA 8088".to_string())
            ]
        );
    }

    /// A phone service on `port` behind its own mux: reads the request until EOF, answers, and
    /// then holds the connection open.
    async fn answers_after_eof(port: u16) -> FakeMux {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mux = FakeMux::start().await;
        mux.route(port, listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            conn.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"request");
            conn.write_all(b"reply").await.unwrap();
            // Never closes on its own.
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(conn);
        });
        mux
    }

    #[tokio::test]
    async fn a_half_closed_data_connection_still_gets_its_reply_then_ends_after_five_seconds() {
        let mux = answers_after_eof(8090).await;
        let fake = FakeWireless::new();
        fake.add_phone(paired_phone(&mux), Rounds::all());
        let link = TlsLink::new(
            PHONE,
            at(205),
            IDENTITY,
            link_key(&long_term()),
            fake.ports().dialer,
        );
        let mut stream = link.open(8090).await.unwrap();
        stream.write_all(b"request").await.unwrap();
        stream.shutdown().await.unwrap();
        let half_closed = Instant::now();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut reply))
            .await
            .expect("the relay ends the connection")
            .unwrap();
        let ended = half_closed.elapsed();
        assert_eq!(reply, b"reply");
        assert!(
            ended >= RELAY_LINGER - Duration::from_millis(100),
            "{ended:?}"
        );
        assert!(ended < RELAY_LINGER + Duration::from_secs(2), "{ended:?}");
    }

    #[tokio::test]
    async fn a_data_connection_the_phone_half_closes_ends_after_five_seconds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mux = FakeMux::start().await;
        mux.route(8089, listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            conn.write_all(b"status").await.unwrap();
            conn.shutdown().await.unwrap();
            // Keeps reading; the relay closes the connection.
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
        });
        let fake = FakeWireless::new();
        fake.add_phone(paired_phone(&mux), Rounds::all());
        let link = TlsLink::new(
            PHONE,
            at(205),
            IDENTITY,
            link_key(&long_term()),
            fake.ports().dialer,
        );
        let mut stream = link.open(8089).await.unwrap();
        let mut status = Vec::new();
        stream.read_to_end(&mut status).await.unwrap();
        assert_eq!(status, b"status");
        let eof = Instant::now();
        // Writing still works for a while, then the connection is gone.
        stream.write_all(b"still here").await.unwrap();
        let gone = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if stream.write_all(b".").await.is_err() {
                    return;
                }
            }
        })
        .await;
        assert!(gone.is_ok(), "the relay closes the connection");
        let ended = eof.elapsed();
        assert!(
            ended >= RELAY_LINGER - Duration::from_millis(200),
            "{ended:?}"
        );
        assert!(ended < RELAY_LINGER + Duration::from_secs(2), "{ended:?}");
    }
}
