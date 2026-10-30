//! hev-socks5-tunnel and cg-netstack side by side: the same client (`PacketDriver`) and
//! the same SOCKS5 server (`FakeSocks`) against each engine, and what the client and the
//! server see must match. hev gets one end of a `socketpair(AF_UNIX, SOCK_DGRAM)`: on
//! Linux it reads and writes its fd as raw IP packets (no packet-info header) and only
//! sets it non-blocking, so any fd that keeps packet boundaries will do. No TUN, no root.
#![cfg(target_os = "linux")]

use cg_core::tunnel::{ENGINE, hev_config};
use cg_netstack::testkit::{FakeSocks, PacketDriver};
use cg_netstack::{Config, PacketChannel, run};
use cg_platform::hev::Hev;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::net::UnixDatagram;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

/// hev keeps global state, so the tests take turns with it.
static HEV_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Copy, Debug)]
enum Kind {
    Hev,
    Netstack,
}

/// An engine with its client on the other end of the packet link.
struct Engine {
    driver: PacketDriver,
    // Held for their drops: hev stops, or cg-netstack is cancelled.
    _hev: Option<HevRun>,
    _netstack: Option<DropGuard>,
}

/// A running hev, stopped on drop (also when a test fails), before its fd is closed
/// (hev does not close an fd it was given) and before the next test may start one.
struct HevRun {
    hev: Option<Hev>,
    bridge: JoinHandle<()>,
    _fd: OwnedFd,
    _turn: MutexGuard<'static, ()>,
}

impl Drop for HevRun {
    fn drop(&mut self) {
        if let Some(hev) = self.hev.take() {
            hev.stop();
        }
        self.bridge.abort();
    }
}

async fn start(kind: Kind, fake: &FakeSocks) -> Engine {
    let (ours, theirs) = PacketChannel::pair(256);
    let (hev, netstack) = match kind {
        Kind::Netstack => {
            let config = Config {
                mtu: u16::try_from(ENGINE.mtu).unwrap(),
                socks: fake.addr(),
                tcp_timeout: ENGINE.tcp_timeout,
                udp_idle: ENGINE.udp_idle,
            };
            let stop = CancellationToken::new();
            tokio::spawn(run(theirs, config, stop.clone()));
            (None, Some(stop.drop_guard()))
        }
        Kind::Hev => (Some(start_hev(theirs, fake.addr().port()).await), None),
    };
    Engine {
        driver: PacketDriver::new(ours),
        _hev: hev,
        _netstack: netstack,
    }
}

/// hev on one end of a datagram socketpair; the other end is bridged to `device`.
async fn start_hev(device: PacketChannel, socks_port: u16) -> HevRun {
    let turn = HEV_LOCK.lock().await;
    let (hev_end, test_end) = std::os::unix::net::UnixDatagram::pair().unwrap();
    test_end.set_nonblocking(true).unwrap();
    let test_end = UnixDatagram::from_std(test_end).unwrap();
    let fd = OwnedFd::from(hev_end);
    let raw = fd.as_raw_fd();
    let hev = tokio::task::spawn_blocking(move || Hev::spawn(hev_config(socks_port), raw))
        .await
        .unwrap()
        .expect("hev starts");
    HevRun {
        hev: Some(hev),
        bridge: tokio::spawn(bridge(device, test_end)),
        _fd: fd,
        _turn: turn,
    }
}

/// Copies packets both ways between the driver's link and hev's socket, one datagram
/// per packet.
async fn bridge(mut device: PacketChannel, socket: UnixDatagram) {
    let mut buf = vec![0u8; 65535];
    loop {
        tokio::select! {
            packet = device.recv() => {
                let Some(packet) = packet else { return };
                if socket.send(&packet).await.is_err() {
                    return;
                }
            }
            received = socket.recv(&mut buf) => {
                let Ok(n) = received else { return };
                if device.send(buf[..n].to_vec()).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct TcpOutcome {
    connects: Vec<SocketAddr>,
    echoed: Vec<Vec<u8>>,
}

async fn tcp_echo(kind: Kind) -> TcpOutcome {
    let fake = FakeSocks::start().await;
    let engine = start(kind, &fake).await;
    let big: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
    let mut echoed = Vec::new();

    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40000"), addr("93.184.216.34:443"))
        .await;
    flow.send(b"hello").await;
    echoed.push(flow.recv_exact(5).await);
    // Several segments each way.
    flow.send(&big).await;
    echoed.push(flow.recv_exact(big.len()).await);

    let mut flow6 = engine
        .driver
        .tcp_connect(addr("[fd00::2]:40000"), addr("[2606:4700::1111]:443"))
        .await;
    flow6.send(b"hello6").await;
    echoed.push(flow6.recv_exact(6).await);

    assert_eq!(echoed, vec![b"hello".to_vec(), big, b"hello6".to_vec()]);
    TcpOutcome {
        connects: fake.connects(),
        echoed,
    }
}

#[tokio::test]
async fn both_engines_echo_tcp_alike() {
    let hev = tcp_echo(Kind::Hev).await;
    assert_eq!(
        hev.connects,
        vec![addr("93.184.216.34:443"), addr("[2606:4700::1111]:443")]
    );
    let ours = tcp_echo(Kind::Netstack).await;
    assert_eq!(ours, hev);
}

/// A DNS query for example.com, as a client would send it.
const DNS_QUERY: &[u8] = &[
    0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c',
    b'o', b'm', 0, 0, 1, 0, 1,
];

#[derive(Debug, PartialEq, Eq)]
struct UdpOutcome {
    replies: Vec<(SocketAddr, Vec<u8>)>,
    /// UDP ASSOCIATE requests after each reply.
    associates: Vec<usize>,
}

async fn udp_echo(kind: Kind) -> UdpOutcome {
    let fake = FakeSocks::start().await;
    let engine = start(kind, &fake).await;
    let flows = [
        (addr("10.0.0.2:5353"), addr("1.1.1.1:53"), DNS_QUERY),
        // The same flow again.
        (
            addr("10.0.0.2:5353"),
            addr("1.1.1.1:53"),
            b"again".as_slice(),
        ),
        (
            addr("[fd00::2]:5353"),
            addr("[2606:4700::1111]:53"),
            DNS_QUERY,
        ),
    ];
    let mut outcome = UdpOutcome {
        replies: Vec::new(),
        associates: Vec::new(),
    };
    for (client, server, payload) in flows {
        engine.driver.udp_send(client, server, payload).await;
        let reply = engine.driver.udp_recv().await;
        assert_eq!(reply, (server, payload.to_vec()));
        outcome.replies.push(reply);
        outcome.associates.push(fake.associates());
    }
    assert!(fake.connects().is_empty(), "UDP went over CONNECT");
    outcome
}

#[tokio::test]
async fn both_engines_echo_udp_alike() {
    let hev = udp_echo(Kind::Hev).await;
    assert!(hev.associates[0] >= 1, "hev did not use ASSOCIATE");
    let ours = udp_echo(Kind::Netstack).await;
    assert_eq!(ours, hev);
}

async fn tcp_half_close(kind: Kind) -> Vec<u8> {
    let fake = FakeSocks::start().await;
    let engine = start(kind, &fake).await;
    let target = addr("93.184.216.34:80");
    // This target answers only after it has read EOF, so the answer crosses the
    // connection after the client's FIN.
    fake.defer_echo(target);
    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40001"), target)
        .await;
    flow.send(b"hello").await;
    flow.shutdown_write().await;
    fake.wait_eof(target).await;
    flow.recv_to_end().await
}

#[tokio::test]
async fn both_engines_pass_tcp_half_close() {
    let hev = tcp_half_close(Kind::Hev).await;
    assert_eq!(hev, b"hello");
    let ours = tcp_half_close(Kind::Netstack).await;
    assert_eq!(ours, hev);
}
