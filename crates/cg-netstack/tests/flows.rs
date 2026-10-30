//! End-to-end flows through the engine: a PacketDriver plays the client on one end of a
//! PacketChannel, `run` owns the other end, and FakeSocks stands in for the forwarder.

use cg_netstack::testkit::{FakeSocks, PacketDriver};
use cg_netstack::{Config, PacketChannel, run};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn config(socks: SocketAddr) -> Config {
    // The values cg_core::tunnel::ENGINE holds (cg-netstack does not depend on cg-core).
    Config {
        mtu: 1500,
        socks,
        tcp_timeout: Duration::from_secs(1800),
        udp_idle: Duration::from_secs(30),
    }
}

struct Engine {
    driver: PacketDriver,
    stop: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}

fn start(fake: &FakeSocks) -> Engine {
    let (ours, theirs) = PacketChannel::pair(256);
    let stop = CancellationToken::new();
    let task = tokio::spawn(run(theirs, config(fake.addr()), stop.clone()));
    Engine {
        driver: PacketDriver::new(ours),
        stop,
        task,
    }
}

#[tokio::test]
async fn tcp_round_trip_goes_through_socks_connect() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let target = addr("93.184.216.34:443");
    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40000"), target)
        .await;
    flow.send(b"hello").await;
    assert_eq!(flow.recv_exact(5).await, b"hello");
    assert_eq!(fake.connects(), vec![target]);

    // A larger exchange, several segments each way.
    let big: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
    flow.send(&big).await;
    assert_eq!(flow.recv_exact(big.len()).await, big);
}

#[tokio::test]
async fn tcp_half_close_reaches_the_server() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let target = addr("93.184.216.34:80");
    // This target answers only after it has read EOF, so the answer has to cross the
    // connection after the client's FIN.
    fake.defer_echo(target);
    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40001"), target)
        .await;
    flow.send(b"hello").await;
    flow.shutdown_write().await;
    fake.wait_eof(target).await;
    assert_eq!(flow.recv_to_end().await, b"hello");
}

#[tokio::test]
async fn tcp_data_on_the_fin_segment_is_delivered() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let target = addr("93.184.216.34:81");
    fake.defer_echo(target);
    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40002"), target)
        .await;
    flow.send_fin(b"last words").await;
    fake.wait_eof(target).await;
    assert_eq!(flow.recv_to_end().await, b"last words");
}

#[tokio::test]
async fn ipv6_tcp_round_trip() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let target = addr("[2606:4700::1111]:443");
    let mut flow = engine
        .driver
        .tcp_connect(addr("[fd00::2]:40000"), target)
        .await;
    flow.send(b"hello6").await;
    assert_eq!(flow.recv_exact(6).await, b"hello6");
    assert_eq!(fake.connects(), vec![target]);
}

/// A DNS query for example.com, as a client would send it.
const DNS_QUERY: &[u8] = &[
    0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c',
    b'o', b'm', 0, 0, 1, 0, 1,
];

#[tokio::test]
async fn udp_round_trip_uses_associate() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let (client, server) = (addr("10.0.0.2:5353"), addr("1.1.1.1:53"));
    engine.driver.udp_send(client, server, DNS_QUERY).await;
    assert_eq!(engine.driver.udp_recv().await, (server, DNS_QUERY.to_vec()));
    // The same flow keeps its association.
    engine.driver.udp_send(client, server, b"again").await;
    assert_eq!(engine.driver.udp_recv().await, (server, b"again".to_vec()));
    assert_eq!(fake.associates(), 1);

    let (client6, server6) = (addr("[fd00::2]:5353"), addr("[2606:4700::1111]:53"));
    engine.driver.udp_send(client6, server6, DNS_QUERY).await;
    assert_eq!(
        engine.driver.udp_recv().await,
        (server6, DNS_QUERY.to_vec())
    );
    assert_eq!(fake.associates(), 2);
}

/// Paused time from the moment the association is idle: with `start_paused` the clock
/// would auto-advance past `udp_idle` while the SOCKS handshake waits on real sockets.
#[tokio::test]
async fn idle_udp_is_released_after_udp_idle() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let (client, server) = (addr("10.0.0.2:5353"), addr("1.1.1.1:53"));
    engine.driver.udp_send(client, server, DNS_QUERY).await;
    assert_eq!(engine.driver.udp_recv().await, (server, DNS_QUERY.to_vec()));

    tokio::time::pause();
    let idle_from = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(!fake.control_closed(), "released before udp_idle");
    // No timeout around this wait: a paused clock would jump straight to it.
    let closed_at = fake.wait_control_closed().await;
    let idle = closed_at - idle_from;
    assert!(
        idle >= Duration::from_secs(29) && idle <= Duration::from_secs(31),
        "{idle:?}"
    );
}

#[tokio::test]
async fn socks_refusal_resets_only_that_flow() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let refused = addr("93.184.216.34:443");
    fake.refuse(refused);
    let mut good = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40010"), addr("93.184.216.35:443"))
        .await;
    let mut bad = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40011"), refused)
        .await;
    bad.expect_reset().await;
    good.send(b"still here").await;
    assert_eq!(good.recv_exact(10).await, b"still here");
}

#[tokio::test]
async fn stop_ends_run() {
    let fake = FakeSocks::start().await;
    let engine = start(&fake);
    let mut flow = engine
        .driver
        .tcp_connect(addr("10.0.0.2:40020"), addr("93.184.216.34:443"))
        .await;
    flow.send(b"x").await;
    assert_eq!(flow.recv_exact(1).await, b"x");
    engine.stop.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(1), engine.task)
        .await
        .expect("run returns within 1 s");
    ended.unwrap().unwrap();
}
