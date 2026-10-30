//! Test doubles for the engine: `FakeSocks`, a SOCKS5 server that echoes and records
//! what it was asked, and `PacketDriver`, a client that speaks raw IP packets (built and
//! parsed with etherparse, independently of smoltcp) on a packet device.
//!
//! Waits are bounded (`WAIT`) so a broken engine fails a test instead of hanging it,
//! except `FakeSocks::wait_control_closed`, which tests use under a paused clock.

use crate::PacketChannel;
use cg_protocol::socks::{address_rest_len, parse_address, parse_udp_datagram};
use etherparse::{NetSlice, PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Longest a test waits for anything (real time).
const WAIT: Duration = Duration::from_secs(5);
/// How long a probing target waits after EOF before it pokes the engine's socket.
const PROBE_DELAY: Duration = Duration::from_millis(300);
/// How long a released target waits, after shutting down its write side, before it reads.
const RELEASE_READ_DELAY: Duration = Duration::from_millis(300);

/// Byte `i` of a flood: a pattern that shows lost or reordered data.
pub fn flood_byte(i: usize) -> u8 {
    (i % 251) as u8
}

#[derive(Clone, Default)]
struct Record {
    connects: Vec<SocketAddr>,
    associates: usize,
    /// Bytes each CONNECT target has read from its client.
    received: HashMap<SocketAddr, usize>,
    /// CONNECT targets whose client sent EOF.
    eof: HashSet<SocketAddr>,
    /// CONNECT targets that found their connection gone (reset or write failure), and when.
    closed: HashMap<SocketAddr, Instant>,
    /// When the first association's control connection closed.
    control_closed: Option<Instant>,
}

#[derive(Default)]
struct Rules {
    refused: HashSet<SocketAddr>,
    deferred: HashSet<SocketAddr>,
    held: HashMap<SocketAddr, watch::Sender<bool>>,
    flood: HashMap<SocketAddr, watch::Sender<usize>>,
    probe: HashSet<SocketAddr>,
}

/// A SOCKS5 server on 127.0.0.1 shaped like the forwarder: no authentication, CONNECT
/// (echo, unless a rule for the target says otherwise) and UDP ASSOCIATE (each
/// datagram, header included, is sent back).
pub struct FakeSocks {
    addr: SocketAddr,
    record: watch::Sender<Record>,
    rules: Arc<Mutex<Rules>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeSocks {
    pub async fn start() -> FakeSocks {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let record = watch::Sender::new(Record::default());
        let rules = Arc::new(Mutex::new(Rules::default()));
        let (rec, rul) = (record.clone(), rules.clone());
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(serve(client, rec.clone(), rul.clone()));
            }
        });
        FakeSocks {
            addr,
            record,
            rules,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// CONNECTs to `target` get reply 5 (connection refused).
    pub fn refuse(&self, target: SocketAddr) {
        self.rules.lock().unwrap().refused.insert(target);
    }

    /// CONNECTs to `target` echo only after the client's EOF, then close.
    pub fn defer_echo(&self, target: SocketAddr) {
        self.rules.lock().unwrap().deferred.insert(target);
    }

    /// CONNECTs to `target` do not read (or echo) until `release`; on release they shut
    /// down their write side (unless flooding), wait `RELEASE_READ_DELAY` and then read
    /// everything.
    pub fn hold(&self, target: SocketAddr) {
        let (tx, _) = watch::channel(false);
        self.rules.lock().unwrap().held.insert(target, tx);
    }

    pub fn release(&self, target: SocketAddr) {
        if let Some(held) = self.rules.lock().unwrap().held.get(&target) {
            held.send_replace(true);
        }
    }

    /// CONNECTs to `target` send `bytes` of `flood_byte` at once (then shut down their
    /// write side), whatever the client does; they do not echo.
    pub fn flood(&self, target: SocketAddr, bytes: usize) {
        let (tx, _) = watch::channel(bytes);
        self.rules.lock().unwrap().flood.insert(target, tx);
    }

    /// Like `flood`, but the bytes go out only on `start_flood`.
    pub fn flood_later(&self, target: SocketAddr) {
        self.flood(target, 0);
    }

    pub fn start_flood(&self, target: SocketAddr, bytes: usize) {
        if let Some(flood) = self.rules.lock().unwrap().flood.get(&target) {
            flood.send_replace(bytes);
        }
    }

    /// CONNECTs to `target` neither echo nor close: after the client's EOF they wait
    /// `PROBE_DELAY`, write one byte and look whether the engine's socket is gone
    /// (it answers with RST then), recording it as closed.
    pub fn probe_after_eof(&self, target: SocketAddr) {
        self.rules.lock().unwrap().probe.insert(target);
    }

    /// CONNECT targets in arrival order.
    pub fn connects(&self) -> Vec<SocketAddr> {
        self.record.borrow().connects.clone()
    }

    pub fn associates(&self) -> usize {
        self.record.borrow().associates
    }

    pub fn control_closed(&self) -> bool {
        self.record.borrow().control_closed.is_some()
    }

    /// Waits until a CONNECT to `target` has read EOF from its client.
    pub async fn wait_eof(&self, target: SocketAddr) {
        self.wait(|r| r.eof.contains(&target), "server saw EOF")
            .await;
    }

    /// Waits until CONNECTs to `target` have read `bytes` in all.
    pub async fn wait_received(&self, target: SocketAddr, bytes: usize) {
        self.wait(
            |r| r.received.get(&target).copied().unwrap_or(0) >= bytes,
            "server received the data",
        )
        .await;
        assert_eq!(self.record.borrow().received[&target], bytes, "extra bytes");
    }

    /// Waits until a CONNECT to `target` found its connection gone; returns when.
    pub async fn wait_closed(&self, target: SocketAddr) -> Instant {
        self.wait(|r| r.closed.contains_key(&target), "server saw the close")
            .await;
        self.record.borrow().closed[&target]
    }

    async fn wait(&self, done: impl FnMut(&Record) -> bool, what: &str) {
        let mut rx = self.record.subscribe();
        tokio::time::timeout(WAIT, rx.wait_for(done))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out: {what}; received {:?}",
                    self.record.borrow().received
                )
            })
            .unwrap();
    }

    /// Waits (unbounded) until an association's control connection closes; returns when.
    pub async fn wait_control_closed(&self) -> Instant {
        let mut rx = self.record.subscribe();
        let record = rx.wait_for(|r| r.control_closed.is_some()).await.unwrap();
        record.control_closed.unwrap()
    }
}

impl Drop for FakeSocks {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut client: TcpStream, record: watch::Sender<Record>, rules: Arc<Mutex<Rules>>) {
    let mut greeting = [0u8; 2];
    if client.read_exact(&mut greeting).await.is_err() || greeting[0] != 5 {
        return;
    }
    let mut methods = vec![0u8; greeting[1].into()];
    if client.read_exact(&mut methods).await.is_err() || !methods.contains(&0) {
        return;
    }
    if client.write_all(&[5, 0]).await.is_err() {
        return;
    }
    let mut header = [0u8; 4];
    if client.read_exact(&mut header).await.is_err() {
        return;
    }
    let Ok(first) = client.read_u8().await else {
        return;
    };
    let Ok(rest) = address_rest_len(header[3], first) else {
        return;
    };
    let mut address = vec![first; 1 + rest];
    if client.read_exact(&mut address[1..]).await.is_err() {
        return;
    }
    let Ok(target) = parse_address(header[3], &address) else {
        return;
    };
    match header[1] {
        1 => connect(client, target, record, rules).await,
        3 => associate(client, record).await,
        _ => {}
    }
}

fn mark_closed(record: &watch::Sender<Record>, target: SocketAddr) {
    let now = Instant::now();
    record.send_modify(|r| {
        r.closed.entry(target).or_insert(now);
    });
}

async fn connect(
    mut client: TcpStream,
    target: SocketAddr,
    record: watch::Sender<Record>,
    rules: Arc<Mutex<Rules>>,
) {
    record.send_modify(|r| r.connects.push(target));
    let (refused, deferred, held, flood, probe) = {
        let rules = rules.lock().unwrap();
        (
            rules.refused.contains(&target),
            rules.deferred.contains(&target),
            rules.held.get(&target).map(|h| h.subscribe()),
            rules.flood.get(&target).map(|f| f.subscribe()),
            rules.probe.contains(&target),
        )
    };
    let code = if refused { 5 } else { 0 };
    if client
        .write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .is_err()
        || refused
    {
        return;
    }
    let (mut reader, writer) = client.into_split();
    let mut writer = Some(writer);
    let flooding = flood.is_some();
    if let Some(mut flood) = flood {
        let mut out = writer.take().unwrap();
        let record = record.clone();
        tokio::spawn(async move {
            let Ok(bytes) = flood.wait_for(|n| *n > 0).await.map(|n| *n) else {
                return;
            };
            let data: Vec<u8> = (0..bytes).map(flood_byte).collect();
            if out.write_all(&data).await.is_err() {
                return mark_closed(&record, target);
            }
            let _ = out.shutdown().await;
        });
    }
    let holding = held.is_some();
    if let Some(mut held) = held {
        // Not reading, but watching for an RST (peek reports it without taking data).
        let mut peek = [0u8; 1];
        loop {
            tokio::select! {
                _ = async { let _ = held.wait_for(|released| *released).await; } => break,
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    if reset_seen(&mut reader, &mut peek).await {
                        return mark_closed(&record, target);
                    }
                }
            }
        }
        if let Some(out) = writer.as_mut() {
            let _ = out.shutdown().await;
        }
        // The close can complete while the held data still waits in the engine.
        tokio::time::sleep(RELEASE_READ_DELAY).await;
    }
    let echo = !deferred && !probe && !flooding && !holding;
    let mut kept = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                record.send_modify(|r| *r.received.entry(target).or_insert(0) += n);
                if deferred {
                    kept.extend_from_slice(&buf[..n]);
                } else if echo
                    && let Some(out) = writer.as_mut()
                    && out.write_all(&buf[..n]).await.is_err()
                {
                    return mark_closed(&record, target);
                }
            }
            Err(_) => return mark_closed(&record, target),
        }
    }
    record.send_modify(|r| {
        r.eof.insert(target);
    });
    if probe {
        return probe_closed(reader, writer, &record, target).await;
    }
    if let Some(mut out) = writer {
        // Answer after EOF: the bytes cross a half-closed connection.
        let _ = out.write_all(&kept).await;
        let _ = out.shutdown().await;
    }
}

/// After EOF: one byte to the engine's socket. A socket still open takes it silently
/// (the next read is just EOF again); a closed one answers with RST.
async fn probe_closed(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    writer: Option<OwnedWriteHalf>,
    record: &watch::Sender<Record>,
    target: SocketAddr,
) {
    let Some(mut out) = writer else { return };
    tokio::time::sleep(PROBE_DELAY).await;
    if out.write_all(b"?").await.is_err() {
        return mark_closed(record, target);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut buf = [0u8; 16];
    if reader.read(&mut buf).await.is_err() || out.write_all(b"?").await.is_err() {
        mark_closed(record, target);
    }
}

async fn associate(mut control: TcpStream, record: watch::Sender<Record>) {
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    crate::udp::ignore_icmp_reset(&udp).unwrap();
    let [hi, lo] = udp.local_addr().unwrap().port().to_be_bytes();
    if control
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, hi, lo])
        .await
        .is_err()
    {
        return;
    }
    record.send_modify(|r| r.associates += 1);
    let mut buf = vec![0u8; 65535];
    let mut stray = [0u8; 64];
    loop {
        tokio::select! {
            received = udp.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                if parse_udp_datagram(&buf[..n]).is_ok() {
                    let _ = udp.send_to(&buf[..n], from).await;
                }
            }
            read = control.read(&mut stray) => {
                if !matches!(read, Ok(n) if n > 0) {
                    break;
                }
            }
        }
    }
    let now = Instant::now();
    record.send_modify(|r| {
        r.control_closed.get_or_insert(now);
    });
}

/// A TCP segment as the driver sees it.
#[derive(Debug)]
struct Segment {
    seq: u32,
    syn: bool,
    ack: bool,
    fin: bool,
    rst: bool,
    ack_number: u32,
    window: u16,
    payload: Vec<u8>,
}

type Flows = Arc<Mutex<HashMap<(SocketAddr, SocketAddr), mpsc::UnboundedSender<Segment>>>>;

/// Plays the client side on a packet device: TCP connections with a minimal, in-order
/// TCP (the in-memory link neither loses nor reorders; sends respect the engine's
/// window), and UDP datagrams.
pub struct PacketDriver {
    out: mpsc::UnboundedSender<Vec<u8>>,
    routes: Routes,
    udp: tokio::sync::Mutex<mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>>,
    task: tokio::task::JoinHandle<()>,
}

impl PacketDriver {
    pub fn new(mut device: PacketChannel) -> PacketDriver {
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (udp_tx, udp_rx) = mpsc::unbounded_channel();
        let routes = Routes::default();
        let owned = routes.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = device.recv() => {
                        let Some(packet) = packet else { return };
                        dispatch(&packet, &owned, &udp_tx);
                    }
                    packet = out_rx.recv() => {
                        let Some(packet) = packet else { return };
                        if device.send(packet).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        PacketDriver {
            out,
            routes,
            udp: tokio::sync::Mutex::new(udp_rx),
            task,
        }
    }

    /// Opens a connection (SYN, SYN-ACK, ACK) from `src` to `dst`.
    pub async fn tcp_connect(&self, src: SocketAddr, dst: SocketAddr) -> TcpFlow {
        let mut flow = self.tcp_syn(src, dst).await;
        flow.complete();
        flow
    }

    /// SYN and SYN-ACK only: the connection waits for `TcpFlow::complete`.
    pub async fn tcp_syn(&self, src: SocketAddr, dst: SocketAddr) -> TcpFlow {
        let (tx, rx) = mpsc::unbounded_channel();
        self.routes.flows.lock().unwrap().insert((src, dst), tx);
        let mut flow = TcpFlow {
            src,
            dst,
            out: self.out.clone(),
            rx,
            backlog: VecDeque::new(),
            snd_nxt: ISN,
            snd_una: ISN,
            peer_window: 0,
            rcv_nxt: 0,
            fin_pending: false,
        };
        flow.emit(Flags::SYN, &[]);
        flow.snd_nxt = ISN.wrapping_add(1);
        let reply = flow.segment().await;
        assert!(
            reply.syn && reply.ack && !reply.rst,
            "expected SYN-ACK, got {reply:?}"
        );
        assert_eq!(reply.ack_number, flow.snd_nxt);
        flow.snd_una = flow.snd_nxt;
        flow.peer_window = reply.window;
        flow.rcv_nxt = reply.seq.wrapping_add(1);
        flow
    }

    pub async fn udp_send(&self, src: SocketAddr, dst: SocketAddr, payload: &[u8]) {
        self.routes.udp_clients.lock().unwrap().insert(src);
        let packet = match (src.ip(), dst.ip()) {
            (IpAddr::V4(s), IpAddr::V4(d)) => {
                PacketBuilder::ipv4(s.octets(), d.octets(), 64).udp(src.port(), dst.port())
            }
            (IpAddr::V6(s), IpAddr::V6(d)) => {
                PacketBuilder::ipv6(s.octets(), d.octets(), 64).udp(src.port(), dst.port())
            }
            _ => panic!("address families differ"),
        };
        let mut bytes = Vec::new();
        packet.write_to_vec(&mut bytes, payload).unwrap();
        self.out.send(bytes).unwrap();
    }

    /// Packets from the engine that no flow of this driver owned (dropped).
    pub fn ignored(&self) -> usize {
        self.routes.ignored.load(Ordering::Relaxed)
    }

    /// The next UDP datagram for the client: its source and payload.
    pub async fn udp_recv(&self) -> (SocketAddr, Vec<u8>) {
        let mut rx = self.udp.lock().await;
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("udp datagram")
            .expect("driver running")
    }
}

impl Drop for PacketDriver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const ISN: u32 = 0x1000_0000;
/// Our receive window (no window scaling offered).
const WINDOW: u16 = 65535;
/// Payload per segment (announced in the SYN): fits a 1500-byte MTU with IPv6.
const MSS: usize = 1400;

struct Flags;
impl Flags {
    const SYN: u8 = 1;
    const ACK: u8 = 2;
    const PSH: u8 = 4;
    const FIN: u8 = 8;
    const RST: u8 = 16;
}

/// Where the driver's packets go: its TCP flows, the client addresses it sent UDP from,
/// and a count of packets it does not own.
#[derive(Clone, Default)]
struct Routes {
    flows: Flows,
    udp_clients: Arc<Mutex<HashSet<SocketAddr>>>,
    ignored: Arc<AtomicUsize>,
}

impl Routes {
    fn ignore(&self, what: &str) {
        self.ignored.fetch_add(1, Ordering::Relaxed);
        tracing::debug!("driver ignored a packet it does not own: {what}");
    }
}

/// Routes a packet to the flow it belongs to, checking its checksum (a bad one there is
/// a test failure). Packets no flow owns (ICMPv6 neighbour discovery and MLD, IGMP,
/// segments or datagrams for unknown flows, unparsable packets) are counted and dropped:
/// a real kernel stack sends those on its own.
fn dispatch(packet: &[u8], routes: &Routes, udp: &mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>) {
    let Ok(sliced) = SlicedPacket::from_ip(packet) else {
        return routes.ignore("unparsable");
    };
    let (src, dst, pseudo): (IpAddr, IpAddr, Pseudo) = match sliced.net {
        Some(NetSlice::Ipv4(ip)) => {
            let h = ip.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                Pseudo::V4(h.source(), h.destination()),
            )
        }
        Some(NetSlice::Ipv6(ip)) => {
            let h = ip.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                Pseudo::V6(h.source(), h.destination()),
            )
        }
        _ => return routes.ignore("not ip"),
    };
    match sliced.transport {
        Some(TransportSlice::Tcp(tcp)) => {
            let key = (
                SocketAddr::new(dst, tcp.destination_port()),
                SocketAddr::new(src, tcp.source_port()),
            );
            let Some(flow) = routes.flows.lock().unwrap().get(&key).cloned() else {
                return routes.ignore("tcp for an unknown flow");
            };
            let expected = match pseudo {
                Pseudo::V4(s, d) => tcp.calc_checksum_ipv4(s, d).unwrap(),
                Pseudo::V6(s, d) => tcp.calc_checksum_ipv6(s, d).unwrap(),
            };
            assert_eq!(tcp.checksum(), expected, "tcp checksum");
            let _ = flow.send(Segment {
                seq: tcp.sequence_number(),
                syn: tcp.syn(),
                ack: tcp.ack(),
                fin: tcp.fin(),
                rst: tcp.rst(),
                ack_number: tcp.acknowledgment_number(),
                window: tcp.window_size(),
                payload: tcp.payload().to_vec(),
            });
        }
        Some(TransportSlice::Udp(datagram)) => {
            let to = SocketAddr::new(dst, datagram.destination_port());
            if !routes.udp_clients.lock().unwrap().contains(&to) {
                return routes.ignore("udp for an unknown flow");
            }
            let header = datagram.to_header();
            let expected = match pseudo {
                Pseudo::V4(s, d) => header.calc_checksum_ipv4_raw(s, d, datagram.payload()),
                Pseudo::V6(s, d) => header.calc_checksum_ipv6_raw(s, d, datagram.payload()),
            }
            .unwrap();
            assert_eq!(header.checksum, expected, "udp checksum");
            let from = SocketAddr::new(src, datagram.source_port());
            let _ = udp.send((from, datagram.payload().to_vec()));
        }
        _ => routes.ignore("not tcp or udp"),
    }
}

enum Pseudo {
    V4([u8; 4], [u8; 4]),
    V6([u8; 16], [u8; 16]),
}

/// One client connection.
pub struct TcpFlow {
    src: SocketAddr,
    dst: SocketAddr,
    out: mpsc::UnboundedSender<Vec<u8>>,
    rx: mpsc::UnboundedReceiver<Segment>,
    /// Segments seen while sending (for their ACKs), not yet looked at for data.
    backlog: VecDeque<Segment>,
    snd_nxt: u32,
    /// Oldest byte the engine has not acknowledged.
    snd_una: u32,
    peer_window: u16,
    rcv_nxt: u32,
    /// A FIN arrived with the data last returned; the next `recv` reports it.
    fin_pending: bool,
}

/// What the server side did next.
enum Event {
    Data(Vec<u8>),
    Fin,
    Reset,
}

impl TcpFlow {
    fn emit(&self, flags: u8, payload: &[u8]) {
        let step = match (self.src.ip(), self.dst.ip()) {
            (IpAddr::V4(s), IpAddr::V4(d)) => PacketBuilder::ipv4(s.octets(), d.octets(), 64),
            (IpAddr::V6(s), IpAddr::V6(d)) => PacketBuilder::ipv6(s.octets(), d.octets(), 64),
            _ => panic!("address families differ"),
        };
        let mut tcp = step.tcp(self.src.port(), self.dst.port(), self.snd_nxt, WINDOW);
        if flags & Flags::SYN != 0 {
            tcp = tcp
                .syn()
                .options(&[TcpOptionElement::MaximumSegmentSize(MSS as u16)])
                .unwrap();
        }
        if flags & Flags::ACK != 0 {
            tcp = tcp.ack(self.rcv_nxt);
        }
        if flags & Flags::PSH != 0 {
            tcp = tcp.psh();
        }
        if flags & Flags::FIN != 0 {
            tcp = tcp.fin();
        }
        if flags & Flags::RST != 0 {
            tcp = tcp.rst();
        }
        let mut bytes = Vec::new();
        tcp.write_to_vec(&mut bytes, payload).unwrap();
        self.out.send(bytes).unwrap();
    }

    async fn segment(&mut self) -> Segment {
        tokio::time::timeout(WAIT, self.rx.recv())
            .await
            .expect("segment from the engine")
            .expect("driver running")
    }

    /// Takes the acknowledgement and window of `s`.
    fn note_ack(&mut self, s: &Segment) {
        let acked = s.ack_number.wrapping_sub(self.snd_una);
        let outstanding = self.snd_nxt.wrapping_sub(self.snd_una);
        if s.ack && !s.rst && acked <= outstanding {
            self.snd_una = s.ack_number;
            self.peer_window = s.window;
        }
    }

    /// Waits for one segment, keeping it for `event` if it carries anything.
    async fn absorb(&mut self) {
        let s = self.segment().await;
        self.note_ack(&s);
        if s.rst || s.fin || !s.payload.is_empty() {
            self.backlog.push_back(s);
        }
    }

    /// Finishes the handshake started by `PacketDriver::tcp_syn`.
    pub fn complete(&mut self) {
        self.emit(Flags::ACK, &[]);
    }

    /// The next data, FIN or RST, acknowledging data and FIN as they arrive.
    async fn event(&mut self) -> Event {
        if std::mem::take(&mut self.fin_pending) {
            return Event::Fin;
        }
        loop {
            let s = match self.backlog.pop_front() {
                Some(s) => s,
                None => {
                    let s = self.segment().await;
                    self.note_ack(&s);
                    s
                }
            };
            if s.rst {
                return Event::Reset;
            }
            if s.seq != self.rcv_nxt || (s.payload.is_empty() && !s.fin) {
                // A retransmission or a bare ACK: (re)acknowledge what we have.
                if !s.payload.is_empty() || s.fin {
                    self.emit(Flags::ACK, &[]);
                }
                continue;
            }
            self.rcv_nxt = self.rcv_nxt.wrapping_add(s.payload.len() as u32);
            if s.fin {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            }
            self.emit(Flags::ACK, &[]);
            match (s.payload.is_empty(), s.fin) {
                (false, fin) => {
                    self.fin_pending = fin;
                    return Event::Data(s.payload);
                }
                (true, _) => return Event::Fin,
            }
        }
    }

    /// Sends `data`, waiting for the engine's window when it is full.
    pub async fn send(&mut self, data: &[u8]) {
        for chunk in data.chunks(MSS) {
            while self.snd_nxt.wrapping_sub(self.snd_una) as usize + chunk.len()
                > usize::from(self.peer_window)
            {
                self.absorb().await;
            }
            self.emit(Flags::ACK | Flags::PSH, chunk);
            self.snd_nxt = self.snd_nxt.wrapping_add(chunk.len() as u32);
        }
    }

    /// Sends `flood_byte` data until the engine stops taking it as fast as it comes:
    /// once everything is acknowledged but the advertised window has shrunk, data sits
    /// unread in the engine because its SOCKS side is not reading. Returns bytes sent.
    pub async fn send_until_stalled(&mut self) -> usize {
        let mut sent = 0usize;
        loop {
            let chunk: Vec<u8> = (sent..sent + 16 * 1024).map(flood_byte).collect();
            self.send(&chunk).await;
            sent += chunk.len();
            while self.snd_una != self.snd_nxt {
                self.absorb().await;
            }
            if self.peer_window < WINDOW {
                return sent;
            }
            assert!(sent < 256 << 20, "the engine never stalled");
        }
    }

    /// Sends `data` with FIN set on its last segment.
    pub async fn send_fin(&mut self, data: &[u8]) {
        let (rest, last) = data.split_at(data.len().saturating_sub(MSS));
        self.send(rest).await;
        self.emit(Flags::ACK | Flags::PSH | Flags::FIN, last);
        self.snd_nxt = self.snd_nxt.wrapping_add(last.len() as u32 + 1);
    }

    /// Half-closes: FIN, then this side only receives.
    pub async fn shutdown_write(&mut self) {
        self.emit(Flags::ACK | Flags::FIN, &[]);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
    }

    /// Resets the connection (RST).
    pub fn reset(&mut self) {
        self.emit(Flags::RST, &[]);
    }

    /// The next data from the server; empty once it has sent FIN. Panics on RST.
    pub async fn recv(&mut self) -> Vec<u8> {
        match self.event().await {
            Event::Data(data) => data,
            Event::Fin => Vec::new(),
            Event::Reset => panic!("connection reset by the engine"),
        }
    }

    /// Exactly `n` bytes (panics on FIN or RST before that).
    pub async fn recv_exact(&mut self, n: usize) -> Vec<u8> {
        let mut data = Vec::new();
        while data.len() < n {
            let more = self.recv().await;
            assert!(!more.is_empty(), "FIN after {} of {n} bytes", data.len());
            data.extend_from_slice(&more);
        }
        assert_eq!(data.len(), n, "more data than expected");
        data
    }

    /// Everything up to the server's FIN.
    pub async fn recv_to_end(&mut self) -> Vec<u8> {
        let mut data = Vec::new();
        loop {
            let more = self.recv().await;
            if more.is_empty() {
                return data;
            }
            data.extend_from_slice(&more);
        }
    }

    /// Waits for an RST, with no data or FIN before it.
    pub async fn expect_reset(&mut self) {
        match self.event().await {
            Event::Reset => {}
            Event::Data(data) => panic!("expected RST, got {} bytes", data.len()),
            Event::Fin => panic!("expected RST, got FIN"),
        }
    }
}

/// Whether an RST has reached `reader`, without taking data. Windows drops queued data
/// on RST and `peek` fails; Linux keeps it readable (so `peek` succeeds) and reports
/// the reset as the socket's pending error.
async fn reset_seen(reader: &mut tokio::net::tcp::OwnedReadHalf, buf: &mut [u8]) -> bool {
    let is_reset = |e: &std::io::Error| e.kind() == std::io::ErrorKind::ConnectionReset;
    if let Ok(Some(error)) = AsRef::<TcpStream>::as_ref(reader).take_error() {
        return is_reset(&error);
    }
    matches!(
        tokio::time::timeout(Duration::from_millis(1), reader.peek(buf)).await,
        Ok(Err(error)) if is_reset(&error)
    )
}
