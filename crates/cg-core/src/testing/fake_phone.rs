use super::FakeMux;
use super::fake_lockdown::{self, FakeLockdown, Observed};
use cg_protocol::phone::port;
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};

pub const SESSION_A: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
pub const SESSION_B: &str = "7F9619FF-8B86-D011-B42D-00C04FC964FF";

pub fn hello_json(caps: &[&str], dns_enabled: bool, session_id: &str) -> Value {
    json!({
        "service": "CellProxy", "protocolVersion": 1, "version": "2.2.0",
        "sessionID": session_id, "capabilities": caps,
        "dns": {"enabled": dns_enabled, "endpoint": "https://1.1.1.1/dns-query", "cacheEnabled": true}
    })
}

pub struct FakePhoneOptions {
    pub hello: Value,
    /// Whether SOCKS CONNECT succeeds (the phone's own way out works).
    pub exit_ok: bool,
    /// Whether the phone accepts udp-over-usb-v1 (command 0xF0).
    pub udp_ok: bool,
    /// Serve lockdownd on 62078 and its diagnostics relay.
    pub lockdown: Option<FakeLockdown>,
    /// Whether the phone app listens on 8091 for the computer's battery reports.
    pub battery_port_open: bool,
}

impl Default for FakePhoneOptions {
    fn default() -> Self {
        Self {
            hello: hello_json(
                &[
                    "socks5-tcp",
                    "socks5-udp",
                    "udp-over-usb-v1",
                    "http-connect",
                    "dns-forward-v1",
                    "session-watch-v1",
                ],
                true,
                SESSION_A,
            ),
            exit_ok: true,
            udp_ok: true,
            lockdown: None,
            battery_port_open: true,
        }
    }
}

struct Shared {
    hello: Mutex<Value>,
    exit_ok: bool,
    udp_ok: bool,
    /// Bumped by `end_session`; watch connections close when it changes.
    generation: watch::Sender<u64>,
    statuses: broadcast::Sender<String>,
    metrics: broadcast::Sender<String>,
    doh_queries: AtomicUsize,
    socks_dead: AtomicBool,
    /// The head of the most recent session watch request.
    last_watch: Mutex<Option<String>>,
    /// Every line received on 8091.
    battery_lines: Mutex<Vec<String>>,
    /// 8091 connections that are still open.
    battery_connections: AtomicUsize,
}

/// The phone app's loopback services behind a FakeMux.
pub struct FakePhone {
    shared: Arc<Shared>,
    lockdown: Option<Arc<Observed>>,
}

async fn serve_on(
    listener: TcpListener,
    shared: Arc<Shared>,
    handler: fn(TcpStream, Arc<Shared>) -> tokio::task::JoinHandle<()>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        handler(stream, shared.clone());
    }
}

async fn bind() -> (TcpListener, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

impl FakePhone {
    pub async fn start(mux: &FakeMux, options: FakePhoneOptions) -> Self {
        let shared = Arc::new(Shared {
            hello: Mutex::new(options.hello),
            exit_ok: options.exit_ok,
            udp_ok: options.udp_ok,
            generation: watch::channel(0).0,
            statuses: broadcast::channel(16).0,
            metrics: broadcast::channel(16).0,
            doh_queries: AtomicUsize::new(0),
            socks_dead: AtomicBool::new(false),
            last_watch: Mutex::new(None),
            battery_lines: Mutex::new(Vec::new()),
            battery_connections: AtomicUsize::new(0),
        });
        for (port, handler) in [
            (
                port::CONTROL,
                control as fn(TcpStream, Arc<Shared>) -> tokio::task::JoinHandle<()>,
            ),
            (port::SOCKS, socks),
            (port::STATUS, status_lines),
            (port::METRICS, metric_lines),
        ] {
            let (listener, addr) = bind().await;
            mux.route(port, addr);
            tokio::spawn(serve_on(listener, shared.clone(), handler));
        }
        if options.battery_port_open {
            let (listener, addr) = bind().await;
            mux.route(port::BATTERY, addr);
            tokio::spawn(serve_on(listener, shared.clone(), battery_sink));
        }
        let lockdown = match options.lockdown {
            Some(lockdown) => Some(fake_lockdown::start(mux, lockdown).await),
            None => None,
        };
        Self { shared, lockdown }
    }

    /// The client certificate presented in each completed lockdown or relay TLS handshake.
    pub fn lockdown_client_certs(&self) -> Vec<CertificateDer<'static>> {
        self.lockdown
            .as_ref()
            .map(|o| o.client_certs.lock().unwrap().clone())
            .unwrap_or_default()
    }

    /// IORegistry requests the fake diagnostics relay answered.
    pub fn relay_requests(&self) -> usize {
        self.lockdown
            .as_ref()
            .map_or(0, |o| o.relay_requests.load(Ordering::SeqCst))
    }

    /// Lines the phone app received on 8091, oldest first.
    pub fn battery_lines(&self) -> Vec<String> {
        self.shared.battery_lines.lock().unwrap().clone()
    }

    /// 8091 connections still open.
    pub fn battery_connections(&self) -> usize {
        self.shared.battery_connections.load(Ordering::SeqCst)
    }

    /// Diagnostics relay connections still open.
    pub fn open_relays(&self) -> usize {
        self.lockdown
            .as_ref()
            .map_or(0, |o| o.open_relays.load(Ordering::SeqCst))
    }

    /// Lockdownd connections (sessions) still open.
    pub fn open_lockdown_sessions(&self) -> usize {
        self.lockdown
            .as_ref()
            .map_or(0, |o| o.open_sessions.load(Ordering::SeqCst))
    }

    /// Ends every session watch, as when the user stops sharing on the phone.
    pub fn end_session(&self) {
        self.shared.generation.send_modify(|g| *g += 1);
    }

    pub fn set_hello(&self, hello: Value) {
        *self.shared.hello.lock().unwrap() = hello;
    }

    /// Closes every new SOCKS connection at once, as a phone whose proxy has died.
    pub fn set_socks_dead(&self, dead: bool) {
        self.shared.socks_dead.store(dead, Ordering::SeqCst);
    }

    pub fn send_status(&self, line: &str) {
        let _ = self.shared.statuses.send(line.into());
    }

    pub fn send_metrics(&self, line: &str) {
        let _ = self.shared.metrics.send(line.into());
    }

    pub fn last_watch_head(&self) -> Option<String> {
        self.shared.last_watch.lock().unwrap().clone()
    }

    pub fn doh_queries(&self) -> usize {
        self.shared.doh_queries.load(Ordering::SeqCst)
    }
}

async fn read_head<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        head.push_str(&line);
        if line == "\r\n" {
            return Some(head);
        }
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

fn control(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        while let Some(head) = read_head(&mut r).await {
            let first = head.lines().next().unwrap_or_default().to_string();
            if first.starts_with("GET /cellproxy/hello ") {
                let body = serde_json::to_vec(&*shared.hello.lock().unwrap()).unwrap();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = w.write_all(reply.as_bytes()).await;
                let _ = w.write_all(&body).await;
                return;
            }
            if let Some(rest) = first.strip_prefix("GET /cellproxy/watch?session=") {
                *shared.last_watch.lock().unwrap() = Some(head.clone());
                let wanted = rest.split(' ').next().unwrap_or_default();
                let current = shared.hello.lock().unwrap()["sessionID"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if wanted != current {
                    let _ = w
                        .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return;
                }
                let mut generation = shared.generation.subscribe();
                let _ = w
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n",
                    )
                    .await;
                let _ = generation.changed().await;
                return;
            }
            if first.starts_with("POST /dns-query ") {
                let mut query = vec![0u8; content_length(&head)];
                if r.read_exact(&mut query).await.is_err() {
                    return;
                }
                shared.doh_queries.fetch_add(1, Ordering::SeqCst);
                if query.len() > 2 {
                    query[2] |= 0x80; // answer = the query with QR set
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\n\r\n",
                    query.len()
                );
                if w.write_all(reply.as_bytes()).await.is_err()
                    || w.write_all(&query).await.is_err()
                {
                    return;
                }
                continue;
            }
            let _ = w
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            return;
        }
    })
}

fn socks(mut stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if shared.socks_dead.load(Ordering::SeqCst) {
            return;
        }
        let mut greeting = [0u8; 2];
        if stream.read_exact(&mut greeting).await.is_err() {
            return;
        }
        let mut methods = vec![0u8; greeting[1] as usize];
        if stream.read_exact(&mut methods).await.is_err()
            || stream.write_all(&[5, 0]).await.is_err()
        {
            return;
        }
        let mut header = [0u8; 4];
        if stream.read_exact(&mut header).await.is_err() {
            return;
        }
        let mut first = [0u8; 1];
        if stream.read_exact(&mut first).await.is_err() {
            return;
        }
        let Ok(rest) = cg_protocol::socks::address_rest_len(header[3], first[0]) else {
            return;
        };
        let mut address = vec![0u8; rest];
        if stream.read_exact(&mut address).await.is_err() {
            return;
        }
        match header[1] {
            1 if shared.exit_ok => {
                if stream
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .is_ok()
                {
                    let (mut r, mut w) = stream.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                }
            }
            0xf0 if shared.udp_ok => {
                if stream
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .is_err()
                {
                    return;
                }
                // Echo every frame back unchanged.
                loop {
                    let mut len = [0u8; 2];
                    if stream.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut payload = vec![0u8; u16::from_be_bytes(len) as usize];
                    if stream.read_exact(&mut payload).await.is_err()
                        || stream.write_all(&len).await.is_err()
                        || stream.write_all(&payload).await.is_err()
                    {
                        return;
                    }
                }
            }
            _ => {
                let _ = stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            }
        }
    })
}

fn lines_from(
    mut stream: TcpStream,
    mut rx: broadcast::Receiver<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(line) = rx.recv().await {
            if stream
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
    })
}

fn status_lines(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    lines_from(stream, shared.statuses.subscribe())
}

fn metric_lines(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    lines_from(stream, shared.metrics.subscribe())
}

fn battery_sink(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        shared.battery_connections.fetch_add(1, Ordering::SeqCst);
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            shared.battery_lines.lock().unwrap().push(line);
        }
        shared.battery_connections.fetch_sub(1, Ordering::SeqCst);
    })
}
