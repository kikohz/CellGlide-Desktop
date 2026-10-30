//! Test doubles for the engine: `FakeSocks`, a SOCKS5 server that echoes and records
//! what it was asked, and `PacketDriver`, a client that speaks raw IP packets (built and
//! parsed with etherparse, independently of smoltcp) on a packet device.
//!
//! Waits are bounded (`WAIT`) so a broken engine fails a test instead of hanging it,
//! except `FakeSocks::wait_control_closed`, which tests use under a paused clock.

use crate::PacketChannel;
use cg_protocol::socks::{address_rest_len, parse_address, parse_udp_datagram};
use etherparse::{NetSlice, PacketBuilder, SlicedPacket, TransportSlice};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Longest a test waits for anything (real time).
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct Record {
    connects: Vec<SocketAddr>,
    associates: usize,
    /// CONNECT targets whose client sent EOF.
    eof: HashSet<SocketAddr>,
    /// When the first association's control connection closed.
    control_closed: Option<Instant>,
}

#[derive(Default)]
struct Rules {
    refused: HashSet<SocketAddr>,
    deferred: HashSet<SocketAddr>,
}

/// A SOCKS5 server on 127.0.0.1 shaped like the forwarder: no authentication, CONNECT
/// (echo) and UDP ASSOCIATE (each datagram, header included, is sent back).
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
        let mut rx = self.record.subscribe();
        tokio::time::timeout(WAIT, rx.wait_for(|r| r.eof.contains(&target)))
            .await
            .expect("server saw EOF")
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

async fn connect(
    mut client: TcpStream,
    target: SocketAddr,
    record: watch::Sender<Record>,
    rules: Arc<Mutex<Rules>>,
) {
    record.send_modify(|r| r.connects.push(target));
    let (refused, deferred) = {
        let rules = rules.lock().unwrap();
        (
            rules.refused.contains(&target),
            rules.deferred.contains(&target),
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
    let mut held = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match client.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) if deferred => held.extend_from_slice(&buf[..n]),
            Ok(n) => {
                if client.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
    record.send_modify(|r| {
        r.eof.insert(target);
    });
    // Answer after EOF: the bytes cross a half-closed connection.
    let _ = client.write_all(&held).await;
    let _ = client.shutdown().await;
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
    payload: Vec<u8>,
}

type Flows = Arc<Mutex<HashMap<(SocketAddr, SocketAddr), mpsc::UnboundedSender<Segment>>>>;

/// Plays the client side on a packet device: TCP connections with a minimal, in-order
/// TCP (the in-memory link neither loses nor reorders), and UDP datagrams.
pub struct PacketDriver {
    out: mpsc::UnboundedSender<Vec<u8>>,
    flows: Flows,
    udp: tokio::sync::Mutex<mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>>,
    task: tokio::task::JoinHandle<()>,
}

impl PacketDriver {
    pub fn new(mut device: PacketChannel) -> PacketDriver {
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (udp_tx, udp_rx) = mpsc::unbounded_channel();
        let flows: Flows = Arc::default();
        let routes = flows.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    packet = device.recv() => {
                        let Some(packet) = packet else { return };
                        dispatch(&packet, &routes, &udp_tx);
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
            flows,
            udp: tokio::sync::Mutex::new(udp_rx),
            task,
        }
    }

    /// Opens a connection (SYN, SYN-ACK, ACK) from `src` to `dst`.
    pub async fn tcp_connect(&self, src: SocketAddr, dst: SocketAddr) -> TcpFlow {
        let (tx, rx) = mpsc::unbounded_channel();
        self.flows.lock().unwrap().insert((src, dst), tx);
        let mut flow = TcpFlow {
            src,
            dst,
            out: self.out.clone(),
            rx,
            snd_nxt: ISN,
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
        flow.rcv_nxt = reply.seq.wrapping_add(1);
        flow.emit(Flags::ACK, &[]);
        flow
    }

    pub async fn udp_send(&self, src: SocketAddr, dst: SocketAddr, payload: &[u8]) {
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
/// Payload per segment: fits a 1500-byte MTU with IPv6 and TCP headers.
const MSS: usize = 1400;

struct Flags;
impl Flags {
    const SYN: u8 = 1;
    const ACK: u8 = 2;
    const PSH: u8 = 4;
    const FIN: u8 = 8;
}

/// Checks checksums (a bad one is a test failure) and routes the packet.
fn dispatch(packet: &[u8], flows: &Flows, udp: &mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>) {
    let sliced = SlicedPacket::from_ip(packet).expect("engine sent a valid IP packet");
    let (src, dst, pseudo): (IpAddr, IpAddr, Pseudo) = match sliced.net.expect("ip header") {
        NetSlice::Ipv4(ip) => {
            let h = ip.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                Pseudo::V4(h.source(), h.destination()),
            )
        }
        NetSlice::Ipv6(ip) => {
            let h = ip.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                Pseudo::V6(h.source(), h.destination()),
            )
        }
        NetSlice::Arp(_) => panic!("arp on an ip link"),
    };
    match sliced.transport {
        Some(TransportSlice::Tcp(tcp)) => {
            let expected = match pseudo {
                Pseudo::V4(s, d) => tcp.calc_checksum_ipv4(s, d).unwrap(),
                Pseudo::V6(s, d) => tcp.calc_checksum_ipv6(s, d).unwrap(),
            };
            assert_eq!(tcp.checksum(), expected, "tcp checksum");
            let key = (
                SocketAddr::new(dst, tcp.destination_port()),
                SocketAddr::new(src, tcp.source_port()),
            );
            let segment = Segment {
                seq: tcp.sequence_number(),
                syn: tcp.syn(),
                ack: tcp.ack(),
                fin: tcp.fin(),
                rst: tcp.rst(),
                ack_number: tcp.acknowledgment_number(),
                payload: tcp.payload().to_vec(),
            };
            if let Some(flow) = flows.lock().unwrap().get(&key) {
                let _ = flow.send(segment);
            }
        }
        Some(TransportSlice::Udp(datagram)) => {
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
        other => panic!("unexpected packet from the engine: {other:?}"),
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
    snd_nxt: u32,
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
            tcp = tcp.syn();
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

    /// The next data, FIN or RST, acknowledging data and FIN as they arrive.
    async fn event(&mut self) -> Event {
        if std::mem::take(&mut self.fin_pending) {
            return Event::Fin;
        }
        loop {
            let s = self.segment().await;
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

    pub async fn send(&mut self, data: &[u8]) {
        for chunk in data.chunks(MSS) {
            self.emit(Flags::ACK | Flags::PSH, chunk);
            self.snd_nxt = self.snd_nxt.wrapping_add(chunk.len() as u32);
        }
    }

    /// Sends `data` with FIN set on its last segment.
    pub async fn send_fin(&mut self, data: &[u8]) {
        let (last, rest) = data.split_at(data.len().saturating_sub(MSS));
        let (rest, last) = (last, rest);
        self.send(rest).await;
        self.emit(Flags::ACK | Flags::PSH | Flags::FIN, last);
        self.snd_nxt = self.snd_nxt.wrapping_add(last.len() as u32 + 1);
    }

    /// Half-closes: FIN, then this side only receives.
    pub async fn shutdown_write(&mut self) {
        self.emit(Flags::ACK | Flags::FIN, &[]);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
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
