//! Local DNS server for the tunnel link. Takes queries over UDP and TCP and hands
//! each to the upstream (DNS-over-HTTPS to the phone). Identical questions in flight
//! share one exchange; answers pass through unchanged apart from the client's ID.

use crate::{CoreError, Result};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_IN_FLIGHT: usize = 128;
/// How long a query waits for the upstream before it gets SERVFAIL, unless the caller
/// sets its own deadline.
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_TCP_CLIENTS: usize = 16;
const TCP_IDLE: Duration = Duration::from_secs(10);
/// Pause after a failed receive or accept (out of descriptors) instead of spinning.
const ERROR_BACKOFF: Duration = Duration::from_millis(100);

pub trait Upstream: Send + Sync + 'static {
    fn exchange(&self, query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>>;
}

type Waiters = Arc<Mutex<HashMap<Vec<u8>, Vec<oneshot::Sender<Option<Vec<u8>>>>>>>;

#[derive(Debug)]
pub struct Resolver {
    addr: SocketAddr,
    cancel: CancellationToken,
}

impl Resolver {
    /// Serves `addr` over UDP and TCP; a query the upstream has not answered within
    /// `deadline` gets SERVFAIL.
    pub async fn start(
        addr: SocketAddr,
        upstream: Arc<dyn Upstream>,
        deadline: Duration,
    ) -> Result<Resolver> {
        let (udp, tcp) = bind_pair(addr).await?;
        let udp = Arc::new(udp);
        let addr = udp.local_addr()?;
        let cancel = CancellationToken::new();
        let waiters: Waiters = Arc::default();

        let token = cancel.clone();
        let (up, w, sock) = (upstream.clone(), waiters.clone(), udp.clone());
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = serve_udp(sock, up, w, deadline, token.clone()) => {}
            }
        });
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = serve_tcp(tcp, upstream, waiters, deadline, token.clone()) => {}
            }
        });
        Ok(Resolver { addr, cancel })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Tries for a port that is free for both UDP and TCP when the caller asks for any port (0).
const ANY_PORT_TRIES: usize = 8;

/// UDP and TCP on the same port. With port 0 the kernel picks the UDP port, which may be in use
/// for TCP (the two have separate ephemeral ranges, e.g. as some connection's local port), so
/// another port is tried. A fixed port (production uses 1053) is tried once.
async fn bind_pair(addr: SocketAddr) -> Result<(UdpSocket, TcpListener)> {
    let tries = if addr.port() == 0 { ANY_PORT_TRIES } else { 1 };
    let mut busy = CoreError::ResolverPortBusy(addr.port());
    for _ in 0..tries {
        let udp = UdpSocket::bind(addr)
            .await
            .map_err(|_| CoreError::ResolverPortBusy(addr.port()))?;
        let port = udp.local_addr()?;
        match TcpListener::bind(port).await {
            Ok(tcp) => return Ok((udp, tcp)),
            Err(_) => busy = CoreError::ResolverPortBusy(port.port()),
        }
    }
    Err(busy)
}

/// A SERVFAIL reply to `query`, keeping its question; None for a response or a runt.
pub fn servfail(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None;
    }
    let mut end = 12;
    if query[4] == 0 && query[5] == 1 {
        while end < query.len() && query[end] != 0 && query[end] & 0xc0 == 0 {
            end += query[end] as usize + 1;
        }
        end += 5; // root label, QTYPE, QCLASS
    }
    let has_question = end > 12 && end <= query.len();
    let mut reply = query[..if has_question { end } else { 12 }].to_vec();
    reply[2] = 0x80 | (query[2] & 0x79); // QR, keep opcode and RD
    reply[3] = 0x80 | 2; // RA, SERVFAIL
    reply[4] = 0;
    reply[5] = u8::from(has_question);
    reply[6..12].fill(0);
    Some(reply)
}

async fn resolve(
    query: Vec<u8>,
    upstream: &Arc<dyn Upstream>,
    waiters: &Waiters,
    deadline: Duration,
) -> Option<Vec<u8>> {
    let mut key = query.clone();
    key[0] = 0;
    key[1] = 0;
    let (tx, rx) = oneshot::channel();
    let first = {
        let mut map = waiters.lock().unwrap();
        if let Some(list) = map.get_mut(&key) {
            list.push(tx);
            false
        } else if map.len() >= MAX_IN_FLIGHT {
            return servfail(&query);
        } else {
            map.insert(key.clone(), vec![tx]);
            true
        }
    };
    if first {
        let upstream = upstream.clone();
        let waiters = waiters.clone();
        let q = query.clone();
        tokio::spawn(async move {
            let reply = tokio::time::timeout(deadline, upstream.exchange(q))
                .await
                .ok()
                .flatten();
            let clients = waiters.lock().unwrap().remove(&key).unwrap_or_default();
            for client in clients {
                let _ = client.send(reply.clone());
            }
        });
    }
    match rx.await.ok().flatten() {
        Some(mut reply) if reply.len() >= 12 => {
            reply[0] = query[0];
            reply[1] = query[1];
            Some(reply)
        }
        _ => servfail(&query),
    }
}

async fn serve_udp(
    socket: Arc<UdpSocket>,
    upstream: Arc<dyn Upstream>,
    waiters: Waiters,
    deadline: Duration,
    cancel: CancellationToken,
) {
    let mut buf = vec![0u8; 65535];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf).await else {
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        if len < 12 {
            continue;
        }
        let query = buf[..len].to_vec();
        let (socket, upstream, waiters) = (socket.clone(), upstream.clone(), waiters.clone());
        let cancel = cancel.clone();
        // Cancelled on stop so no task keeps the socket bound or replies afterwards.
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                reply = resolve(query, &upstream, &waiters, deadline) => {
                    if let Some(reply) = reply {
                        let _ = socket.send_to(&reply, from).await;
                    }
                }
            }
        });
    }
}

async fn serve_tcp(
    listener: TcpListener,
    upstream: Arc<dyn Upstream>,
    waiters: Waiters,
    deadline: Duration,
    cancel: CancellationToken,
) {
    let slots = Arc::new(Semaphore::new(MAX_TCP_CLIENTS));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (upstream, waiters, cancel) = (upstream.clone(), waiters.clone(), cancel.clone());
        tokio::spawn(async move {
            let _slot = slot;
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = serve_tcp_client(stream, upstream, waiters, deadline) => {}
            }
        });
    }
}

async fn serve_tcp_client(
    mut stream: TcpStream,
    upstream: Arc<dyn Upstream>,
    waiters: Waiters,
    deadline: Duration,
) -> Option<()> {
    loop {
        let len = tokio::time::timeout(TCP_IDLE, stream.read_u16())
            .await
            .ok()?
            .ok()? as usize;
        if len < 12 {
            return None;
        }
        let mut query = vec![0u8; len];
        tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut query))
            .await
            .ok()?
            .ok()?;
        let reply = resolve(query, &upstream, &waiters, deadline).await?;
        let len = u16::try_from(reply.len()).ok()?;
        stream.write_all(&len.to_be_bytes()).await.ok()?;
        stream.write_all(&reply).await.ok()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UdpSocket};

    /// Answers with the query plus QR, after `delay`; counts calls.
    struct Echo {
        calls: AtomicUsize,
        delay: Duration,
        fail: bool,
    }

    impl Upstream for Echo {
        fn exchange(&self, mut query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                if self.fail {
                    return None;
                }
                query[2] |= 0x80;
                // Upstream answers with its own ID; the resolver must restore the client's.
                query[0] = 0xAB;
                query[1] = 0xCD;
                Some(query)
            })
        }
    }

    fn query(id: u16) -> Vec<u8> {
        let mut q = id.to_be_bytes().to_vec();
        q.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        q.extend_from_slice(&[
            7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o', b'm', 0, 0, 1, 0, 1,
        ]);
        q
    }

    async fn start(echo: Arc<Echo>) -> Resolver {
        Resolver::start("127.0.0.1:0".parse().unwrap(), echo, UPSTREAM_TIMEOUT)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn udp_answers_with_client_id() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            fail: false,
        });
        let resolver = start(echo).await;
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&query(0x1234), resolver.local_addr())
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let n = client.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0x12, 0x34]);
        assert_eq!(buf[2] & 0x80, 0x80);
        assert_eq!(n, query(0).len());
    }

    #[tokio::test]
    async fn identical_questions_share_one_upstream_exchange() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(200),
            fail: false,
        });
        let resolver = start(echo.clone()).await;
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        a.send_to(&query(1), resolver.local_addr()).await.unwrap();
        b.send_to(&query(2), resolver.local_addr()).await.unwrap();
        let mut buf = [0u8; 512];
        a.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0, 1]);
        b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0, 2]);
        assert_eq!(echo.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_upstream_gives_servfail() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            fail: true,
        });
        let resolver = start(echo).await;
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&query(7), resolver.local_addr())
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let n = client.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &servfail(&query(7)).unwrap()[..]);
        assert_eq!(buf[3] & 0x0f, 2);
    }

    #[tokio::test]
    async fn tcp_queries_are_length_prefixed() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            fail: false,
        });
        let resolver = start(echo).await;
        let mut s = TcpStream::connect(resolver.local_addr()).await.unwrap();
        let q = query(9);
        s.write_all(&(q.len() as u16).to_be_bytes()).await.unwrap();
        s.write_all(&q).await.unwrap();
        let len = s.read_u16().await.unwrap() as usize;
        let mut reply = vec![0u8; len];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], &[0, 9]);
    }

    #[tokio::test]
    async fn busy_port_is_reported() {
        let taken = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            fail: false,
        });
        let err = Resolver::start(taken.local_addr().unwrap(), echo, UPSTREAM_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::ResolverPortBusy(_)));
        assert!(err.to_string().contains("in use"));
    }

    #[tokio::test]
    async fn a_fixed_port_taken_for_tcp_only_is_busy() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();
        // Only if UDP happens to be free on that port does this test what it says.
        if std::net::UdpSocket::bind(addr).is_err() {
            return;
        }
        let err = bind_pair(addr).await.unwrap_err();
        assert!(matches!(err, CoreError::ResolverPortBusy(p) if p == addr.port()));
    }

    #[tokio::test]
    async fn any_port_gets_the_same_port_for_udp_and_tcp() {
        for _ in 0..50 {
            let (udp, tcp) = bind_pair("127.0.0.1:0".parse().unwrap()).await.unwrap();
            assert_eq!(udp.local_addr().unwrap(), tcp.local_addr().unwrap());
        }
    }

    #[tokio::test]
    async fn stop_releases_the_port_while_queries_are_pending() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::from_secs(5),
            fail: false,
        });
        let resolver = start(echo.clone()).await;
        let addr = resolver.local_addr();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(&query(3), addr).await.unwrap();
        while echo.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        resolver.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        UdpSocket::bind(addr)
            .await
            .expect("UDP port must be free after stop");
    }

    #[test]
    fn servfail_keeps_question_and_flags() {
        let q = query(0x0102);
        let r = servfail(&q).unwrap();
        assert_eq!(&r[..2], &[1, 2]);
        assert_eq!(r[2], 0x80 | 0x01, "QR set, RD kept");
        assert_eq!(r[3], 0x82, "RA, SERVFAIL");
        assert_eq!(&r[4..6], &[0, 1]);
        assert_eq!(&r[6..12], &[0; 6]);
        assert_eq!(&r[12..], &q[12..]);
        let mut response = q.clone();
        response[2] |= 0x80;
        assert!(servfail(&response).is_none(), "never answer a response");
        assert!(servfail(&q[..5]).is_none());
    }

    #[tokio::test]
    async fn servfail_after_the_configured_deadline() {
        let echo = Arc::new(Echo {
            calls: AtomicUsize::new(0),
            delay: Duration::from_secs(3600),
            fail: false,
        });
        let resolver = Resolver::start(
            "127.0.0.1:0".parse().unwrap(),
            echo,
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&query(5), resolver.local_addr())
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let n = tokio::time::timeout(Duration::from_millis(400), client.recv(&mut buf))
            .await
            .expect("SERVFAIL within 400 ms")
            .unwrap();
        assert_eq!(&buf[..n], &servfail(&query(5)).unwrap()[..]);
    }
}
