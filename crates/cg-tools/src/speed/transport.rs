//! Connections to the speed test server, held to one interface with SO_BINDTODEVICE (Linux)
//! or IP_UNICAST_IF (Windows) so the test never changes a route, and the hand-written HTTP/1.1 the Mac speaks to Cloudflare
//! (SpeedTransport.swift): same headers, same refusal and length checks.

use crate::net;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// How long one address may take to accept the TCP connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// A response head bigger than this is not Cloudflare's.
pub const MAX_HEAD: usize = 64 * 1024;

/// Which way out a measurement goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The phone, through CellGlide's TUN (`cellglide0`).
    Phone,
    /// This computer's own Wi‑Fi or Ethernet, by name ("wlp2s0").
    Interface(String),
    /// Whatever the routing table says. Tests only: production code must never use it, since an
    /// unbound socket could measure either path (Review Focus 4).
    #[doc(hidden)]
    Unbound,
}

impl Route {
    /// The interface the socket is bound to, if any.
    pub fn device(&self) -> Option<&str> {
        match self {
            Route::Phone => Some(crate::uplink::OURS),
            Route::Interface(name) => Some(name),
            Route::Unbound => None,
        }
    }
}

/// The speed test server. Production uses `Server::cloudflare()`; tests a local plain-HTTP fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Server {
    /// `speed.cloudflare.com:443`, as on the Mac.
    pub fn cloudflare() -> Server {
        Server {
            host: "speed.cloudflare.com".into(),
            port: 443,
            tls: true,
        }
    }

    /// The `Host` header: the name, with the port when it isn't the scheme's default.
    pub(crate) fn host_header(&self) -> String {
        let default_port = if self.tls { 443 } else { 80 };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Why a measurement failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SpeedError {
    /// The named interface isn't there (bind_device answered ENODEV), or no Wi‑Fi/Ethernet
    /// default route exists to compare with (an empty name). Never measured unbound instead.
    #[error("no interface {0:?}")]
    NoInterface(String),
    /// `cellglide0` isn't there: CellGlide is not connected to a phone.
    #[error("not connected")]
    NotConnected,
    /// Cloudflare answered 429; try again in about this many minutes (at least 1).
    #[error("rate limited, retry in {minutes} min")]
    RateLimited { minutes: u32 },
    /// Cloudflare answered something other than 200 or 429.
    #[error("refused with HTTP {0}")]
    Refused(u16),
    /// Connecting or a latency request took too long.
    #[error("timed out")]
    Timeout,
    /// The connection closed before the whole response arrived.
    #[error("connection closed early")]
    ClosedEarly,
    /// The latency reply's length was not the one asked for.
    #[error("unexpected response length")]
    BadLength,
    /// Cloudflare switched to chunked replies, which this test doesn't read.
    #[error("chunked response")]
    Chunked,
    /// Not an HTTP response.
    #[error("unreadable response")]
    BadReply,
    /// Any other socket or TLS error, with its text.
    #[error("{0}")]
    Io(String),
}

impl SpeedError {
    /// One value of every variant (data-carrying ones with a sample), for completeness tests in
    /// this and the CLI crate; adding a variant fails to compile until the match names it.
    #[doc(hidden)]
    pub fn all() -> Vec<SpeedError> {
        let all = vec![
            SpeedError::NoInterface("x".into()),
            SpeedError::NotConnected,
            SpeedError::RateLimited { minutes: 2 },
            SpeedError::Refused(403),
            SpeedError::Timeout,
            SpeedError::ClosedEarly,
            SpeedError::BadLength,
            SpeedError::Chunked,
            SpeedError::BadReply,
            SpeedError::Io("reset".into()),
        ];
        for error in &all {
            match error {
                SpeedError::NoInterface(_)
                | SpeedError::NotConnected
                | SpeedError::RateLimited { .. }
                | SpeedError::Refused(_)
                | SpeedError::Timeout
                | SpeedError::ClosedEarly
                | SpeedError::BadLength
                | SpeedError::Chunked
                | SpeedError::BadReply
                | SpeedError::Io(_) => {}
            }
        }
        all
    }
}

impl From<io::Error> for SpeedError {
    fn from(error: io::Error) -> SpeedError {
        SpeedError::Io(error.to_string())
    }
}

/// A connection to the server, plain or TLS.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
    /// The local end of the TCP connection: which interface's address it went out from.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Stream::Plain(s) => s.local_addr(),
            Stream::Tls(s) => s.get_ref().0.local_addr(),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Connects to `server` along `route`: every resolved address in turn, each socket bound to
/// the route's interface before it connects, at most 4 s each; then TLS with the shared
/// config when the server speaks it.
pub async fn connect(route: &Route, server: &Server) -> Result<Stream, SpeedError> {
    connect_with(route, server, net::client_config()).await
}

/// `connect` with another TLS config; tests trust a local certificate with it.
pub(crate) async fn connect_with(
    route: &Route,
    server: &Server,
    config: Arc<ClientConfig>,
) -> Result<Stream, SpeedError> {
    let tcp = connect_tcp(route, server).await?;
    if !server.tls {
        return Ok(Stream::Plain(tcp));
    }
    let name = ServerName::try_from(server.host.clone()).map_err(|_| SpeedError::BadReply)?;
    let tls = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TlsConnector::from(config).connect(name, tcp),
    )
    .await
    .map_err(|_| SpeedError::Timeout)??;
    Ok(Stream::Tls(Box::new(tls)))
}

async fn connect_tcp(route: &Route, server: &Server) -> Result<TcpStream, SpeedError> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await?
        .collect();
    let mut last = SpeedError::Io(format!("{} has no address", server.host));
    for address in addresses {
        let attempt = async {
            let socket = bound_socket(route, address)?;
            Ok::<_, SpeedError>(socket.connect(address).await?)
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
            Ok(Ok(tcp)) => return Ok(tcp),
            // The interface is missing for every address alike: stop here.
            Ok(Err(e @ (SpeedError::NoInterface(_) | SpeedError::NotConnected))) => return Err(e),
            Ok(Err(e)) => last = e,
            Err(_) => last = SpeedError::Timeout,
        }
    }
    Err(last)
}

/// A fresh non-blocking TCP socket for `address`, bound to the route's interface.
fn bound_socket(route: &Route, address: SocketAddr) -> Result<tokio::net::TcpSocket, SpeedError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    socket.set_nonblocking(true)?;
    if let Some(device) = route.device() {
        bind_device(&socket, route, device, address)?;
    }
    Ok(tokio::net::TcpSocket::from_std_stream(socket.into()))
}

#[cfg(target_os = "linux")]
fn bind_device(
    socket: &socket2::Socket,
    route: &Route,
    device: &str,
    _address: SocketAddr,
) -> Result<(), SpeedError> {
    socket
        .bind_device(Some(device.as_bytes()))
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ENODEV) => missing(route, device),
            _ => e.into(),
        })
}

/// Windows: `IP_UNICAST_IF` (ifIndex in network byte order) or `IPV6_UNICAST_IF` (host byte
/// order) on the interface whose alias is `device` ("cellglide0", "Wi-Fi"). W0 measured that
/// this sends the connection out that interface past cellglide0's /1 routes.
#[cfg(windows)]
fn bind_device(
    socket: &socket2::Socket,
    route: &Route,
    device: &str,
    address: SocketAddr,
) -> Result<(), SpeedError> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceAliasToLuid, ConvertInterfaceLuidToIndex,
    };
    use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows::Win32::Networking::WinSock::{SOCKET, WSAGetLastError, setsockopt};
    use windows::core::PCWSTR;

    let alias: Vec<u16> = device.encode_utf16().chain([0]).collect();
    let mut luid = NET_LUID_LH::default();
    let mut index = 0u32;
    // SAFETY: `alias` is NUL-terminated and outlives the calls; the out pointers are valid.
    let found = unsafe {
        ConvertInterfaceAliasToLuid(PCWSTR(alias.as_ptr()), &mut luid).is_ok()
            && ConvertInterfaceLuidToIndex(&luid, &mut index).is_ok()
    };
    if !found || index == 0 {
        return Err(missing(route, device));
    }
    let (level, name, value) = unicast_if_option(index, address.is_ipv6());
    let raw = SOCKET(socket.as_raw_socket() as usize);
    // SAFETY: `raw` is the open socket `socket` owns; both options take a 4-byte DWORD.
    if unsafe { setsockopt(raw, level, name, Some(&value.to_ne_bytes())) } != 0 {
        // SAFETY: reads this thread's last Winsock error.
        let code = unsafe { WSAGetLastError() };
        return Err(io::Error::from_raw_os_error(code.0).into());
    }
    Ok(())
}

/// The `setsockopt` that holds a socket to interface `index`: `(level, optname, value)`. The
/// value is written as its native-endian bytes, so `IP_UNICAST_IF` gets the index in network
/// byte order (Winsock reads it that way) and `IPV6_UNICAST_IF` in host byte order.
#[cfg(windows)]
pub(crate) fn unicast_if_option(index: u32, ipv6: bool) -> (i32, i32, u32) {
    use windows::Win32::Networking::WinSock::{
        IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF,
    };
    if ipv6 {
        (IPPROTO_IPV6.0, IPV6_UNICAST_IF, index)
    } else {
        (IPPROTO_IP.0, IP_UNICAST_IF, index.to_be())
    }
}

/// Elsewhere a bound route can't be honoured, so it fails rather than measure an unbound
/// socket.
#[cfg(not(any(target_os = "linux", windows)))]
fn bind_device(
    _socket: &socket2::Socket,
    _route: &Route,
    device: &str,
    _address: SocketAddr,
) -> Result<(), SpeedError> {
    Err(SpeedError::NoInterface(device.to_string()))
}

#[cfg(any(target_os = "linux", windows))]
fn missing(route: &Route, device: &str) -> SpeedError {
    match route {
        Route::Phone => SpeedError::NotConnected,
        _ => SpeedError::NoInterface(device.to_string()),
    }
}

/// A request head with the Mac's headers.
pub(crate) fn request(line: &str, server: &Server, extra: &str) -> Vec<u8> {
    format!(
        "{line}\r\nHost: {}\r\nAccept-Encoding: identity\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n{extra}\r\n",
        server.host_header()
    )
    .into_bytes()
}

/// `GET /__down?bytes=<n>`.
pub(crate) fn download_request(server: &Server, bytes: u64) -> Vec<u8> {
    request(&format!("GET /__down?bytes={bytes} HTTP/1.1"), server, "")
}

/// `POST /__up` with a body of `length` bytes to follow.
pub(crate) fn upload_request(server: &Server, length: u64) -> Vec<u8> {
    request(
        "POST /__up HTTP/1.1",
        server,
        &format!("Content-Type: application/octet-stream\r\nContent-Length: {length}\r\n"),
    )
}

/// A parsed response head.
#[derive(Debug)]
pub(crate) struct Head {
    pub status: u16,
    pub content_length: Option<u64>,
    pub chunked: bool,
    text: String,
}

impl Head {
    fn parse(raw: &[u8]) -> Result<Head, SpeedError> {
        let text = String::from_utf8_lossy(raw).to_string();
        let mut lines = text.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        if !status_line.starts_with("HTTP/") {
            return Err(SpeedError::BadReply);
        }
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or(SpeedError::BadReply)?;
        let mut content_length = None;
        let mut chunked = false;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            if name == "content-length" {
                content_length = value.parse().ok();
            } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
        Ok(Head {
            status,
            content_length,
            chunked,
            text,
        })
    }

    /// The value of a header, by case-insensitive name.
    fn header(&self, wanted: &str) -> Option<&str> {
        self.text.split("\r\n").skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(wanted)
                .then_some(value.trim())
        })
    }

    /// Ok for 200; otherwise why Cloudflare refused, then chunked bodies, which aren't read.
    pub fn check(&self) -> Result<(), SpeedError> {
        if self.status != 200 {
            return Err(refusal(self));
        }
        if self.chunked {
            return Err(SpeedError::Chunked);
        }
        Ok(())
    }
}

/// A refused request: 429 asks us to wait `Retry-After` seconds, rounded up to whole minutes,
/// at least 1 (60 s when it doesn't say).
pub(crate) fn refusal(head: &Head) -> SpeedError {
    if head.status == 429 {
        let retry: u64 = head
            .header("retry-after")
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let minutes = retry.div_ceil(60).max(1);
        SpeedError::RateLimited {
            minutes: u32::try_from(minutes).unwrap_or(u32::MAX),
        }
    } else {
        SpeedError::Refused(head.status)
    }
}

/// Reads a response head; returns it with the body bytes that came in the same reads.
pub(crate) async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<(Head, Vec<u8>), SpeedError> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = Head::parse(&raw[..end])?;
            return Ok((head, raw[end + 4..].to_vec()));
        }
        if raw.len() > MAX_HEAD {
            return Err(SpeedError::BadReply);
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(SpeedError::ClosedEarly);
        }
        raw.extend_from_slice(&buf[..n]);
    }
}

/// One latency sample on an open connection: `GET /__down?bytes=0`, timed from sending the
/// request to the end of the (empty) response. The reply must say `Content-Length: 0`.
pub(crate) async fn ping(stream: &mut Stream, server: &Server) -> Result<Duration, SpeedError> {
    let started = tokio::time::Instant::now();
    stream.write_all(&download_request(server, 0)).await?;
    let (head, extra) = read_head(stream).await?;
    head.check()?;
    if head.content_length != Some(0) || !extra.is_empty() {
        return Err(SpeedError::BadLength);
    }
    Ok(started.elapsed())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::speed::engine::tests::{FakeServer, Mode};

    fn local_server() -> Server {
        Server {
            host: "127.0.0.1".into(),
            port: 9,
            tls: false,
        }
    }

    #[cfg(any(target_os = "linux", windows))]
    #[tokio::test]
    async fn missing_uplink_fails_without_falling_back() {
        let r = connect(&Route::Interface("cg-no-such0".into()), &local_server()).await;
        assert!(matches!(r, Err(SpeedError::NoInterface(name)) if name == "cg-no-such0"));
    }

    #[cfg(windows)]
    #[test]
    fn ipv4_index_is_network_order_and_ipv6_is_not() {
        use windows::Win32::Networking::WinSock::{
            IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF,
        };
        let (level, name, v4) = unicast_if_option(12, false);
        assert_eq!((level, name), (IPPROTO_IP.0, IP_UNICAST_IF));
        assert_eq!(v4.to_ne_bytes(), [0, 0, 0, 12]);
        let (level, name, v6) = unicast_if_option(12, true);
        assert_eq!((level, name, v6), (IPPROTO_IPV6.0, IPV6_UNICAST_IF, 12));
    }

    #[tokio::test]
    async fn local_addr_is_the_connection_s_own_end() {
        let fake = FakeServer::start(Mode::Normal).await;
        let stream = connect(&Route::Unbound, &fake.server()).await.unwrap();
        assert!(stream.local_addr().unwrap().ip().is_loopback());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn missing_tun_means_not_connected() {
        // Only meaningful where cellglide0 doesn't exist (Docker, CI, this test VM).
        if std::path::Path::new("/sys/class/net/cellglide0").exists() {
            return;
        }
        let r = connect(&Route::Phone, &local_server()).await;
        assert!(matches!(r, Err(SpeedError::NotConnected)), "{:?}", r.err());
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    #[tokio::test]
    async fn bound_routes_fail_off_linux_instead_of_going_unbound() {
        let r = connect(&Route::Interface("en0".into()), &local_server()).await;
        assert!(matches!(r, Err(SpeedError::NoInterface(name)) if name == "en0"));
        let r = connect(&Route::Phone, &local_server()).await;
        assert!(matches!(r, Err(SpeedError::NoInterface(name)) if name == "cellglide0"));
    }

    #[test]
    fn cloudflare_server_and_mac_headers() {
        let cf = Server::cloudflare();
        assert_eq!(
            (cf.host.as_str(), cf.port, cf.tls),
            ("speed.cloudflare.com", 443, true)
        );
        let head = String::from_utf8(download_request(&cf, 0)).unwrap();
        assert_eq!(
            head,
            "GET /__down?bytes=0 HTTP/1.1\r\nHost: speed.cloudflare.com\r\nAccept-Encoding: identity\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
        );
        let up = String::from_utf8(upload_request(&local_server(), 5)).unwrap();
        assert!(up.starts_with("POST /__up HTTP/1.1\r\nHost: 127.0.0.1:9\r\n"));
        assert!(up.contains("Content-Type: application/octet-stream\r\nContent-Length: 5\r\n"));
        assert!(up.ends_with("\r\n\r\n"));
    }

    fn head(text: &str) -> Head {
        Head::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn refusal_explains_rate_limit() {
        // SpeedEngineTests.testRefusalExplainsRateLimit: 90 s rounds up to 2 minutes.
        let limited = head("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 90");
        assert_eq!(limited.check(), Err(SpeedError::RateLimited { minutes: 2 }));
        let no_hint = head("HTTP/1.1 429 Too Many Requests");
        assert_eq!(refusal(&no_hint), SpeedError::RateLimited { minutes: 1 });
        let soon = head("HTTP/1.1 429 Too Many Requests\r\nretry-after: 0");
        assert_eq!(refusal(&soon), SpeedError::RateLimited { minutes: 1 });
        let forbidden = head("HTTP/1.1 403 Forbidden");
        assert_eq!(forbidden.check(), Err(SpeedError::Refused(403)));
    }

    #[test]
    fn chunked_and_garbage_replies_are_rejected() {
        let chunked = head("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked");
        assert_eq!(chunked.check(), Err(SpeedError::Chunked));
        assert!(matches!(
            Head::parse(b"SSH-2.0-OpenSSH"),
            Err(SpeedError::BadReply)
        ));
        let ok = head("HTTP/1.1 200 OK\r\nContent-Length: 12");
        assert_eq!((ok.check(), ok.content_length), (Ok(()), Some(12)));
    }

    #[tokio::test]
    async fn latency_needs_an_empty_reply() {
        let fake = FakeServer::start(Mode::WrongLength).await;
        let mut stream = connect(&Route::Unbound, &fake.server()).await.unwrap();
        assert_eq!(
            ping(&mut stream, &fake.server()).await,
            Err(SpeedError::BadLength)
        );
        let fake = FakeServer::start(Mode::Normal).await;
        let mut stream = connect(&Route::Unbound, &fake.server()).await.unwrap();
        for _ in 0..3 {
            // One kept-alive connection serves every sample, as on the Mac.
            ping(&mut stream, &fake.server()).await.unwrap();
        }
        assert_eq!(fake.accepted(), 1);
    }

    #[tokio::test]
    async fn speaks_tls_with_the_server_name() {
        use rustls::RootCertStore;
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        const CERT: &str = include_str!("../../../cg-diag/tests/fixtures/server-cert.pem");
        // Throwaway test key generated for these tests only; it protects nothing.
        const KEY: &str = include_str!("../../../cg-diag/tests/fixtures/server-key.pem");
        let cert = CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap();
        let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            let (_, _) = read_request(&mut tls).await;
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            tls.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let server = Server {
            host: "localhost".into(),
            port,
            tls: true,
        };
        let mut roots = RootCertStore::empty();
        roots.add(cert).unwrap();
        let config = net::client_config_with_roots(roots);
        let mut stream = connect_with(&Route::Unbound, &server, config)
            .await
            .unwrap();
        assert!(matches!(stream, Stream::Tls(_)));
        ping(&mut stream, &server).await.unwrap();
    }

    /// Reads one request head off a server-side stream; returns it and any body bytes read.
    pub(crate) async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> (String, Vec<u8>) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..end]).to_string();
                return (head, raw[end + 4..].to_vec());
            }
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return (String::new(), Vec::new());
            }
            raw.extend_from_slice(&buf[..n]);
        }
    }
}
