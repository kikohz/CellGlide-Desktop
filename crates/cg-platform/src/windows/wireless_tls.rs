//! The computer's side of the wireless link's TLS (design §8.1): TLS 1.2 only, the single suite
//! `TLS_ECDHE_PSK_WITH_CHACHA20_POLY1305_SHA256`, a pre-shared key per identity, no session
//! cache and no tickets, so every connection is a full handshake (the phone relies on that to
//! tell which identity a connection uses). Neither SChannel nor rustls offer PSK suites, hence
//! OpenSSL. TCP keepalive 30 s idle, 10 s interval, 3 probes, like the phone.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use openssl::error::ErrorStack;
use openssl::ssl::{
    Ssl, SslAlert, SslContext, SslContextBuilder, SslMethod, SslOptions, SslSessionCacheMode,
    SslVerifyMode, SslVersion,
};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// OpenSSL's name for the one suite both sides offer.
pub const CIPHER: &str = "ECDHE-PSK-CHACHA20-POLY1305";

pub type TlsStream = tokio_openssl::SslStream<TcpStream>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    /// The phone answered the handshake with alert 115 (unknown_psk_identity): it does not
    /// know this identity (any more).
    UnknownIdentity,
    /// The handshake completed with a suite other than [`CIPHER`].
    WrongSuite,
    /// TCP connect and handshake did not complete within the time limit.
    Timeout,
    /// Anything else: connect, timeout, another alert, the peer closing.
    Io(String),
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::UnknownIdentity => f.write_str("the phone does not know this identity"),
            TlsError::WrongSuite => write!(f, "the handshake did not use {CIPHER}"),
            TlsError::Timeout => f.write_str("no handshake within the time limit"),
            TlsError::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TlsError {}

/// Connects to the phone at `address` and completes the handshake as `identity` with `psk`,
/// TCP connect and handshake together within `timeout`. The caller sends its request line right
/// away (the phone allows 5 s for handshake and request line).
///
/// The returned `TlsStream`'s own `poll_shutdown` sends close_notify and shuts the TCP write
/// side; a session hands the stream on only inside [`HalfCloseSafe`] (design §8.1).
pub async fn connect(
    address: SocketAddr,
    identity: &str,
    psk: &[u8; 32],
    timeout: Duration,
) -> Result<TlsStream, TlsError> {
    let context = client_context(identity, psk).map_err(|e| TlsError::Io(e.to_string()))?;
    match tokio::time::timeout(timeout, handshake(address, &context)).await {
        Ok(result) => result,
        Err(_) => Err(TlsError::Timeout),
    }
}

/// The TLS stream as the session uses it. The phone treats close_notify or a TCP FIN as the end
/// of the whole connection, so a user that closes only its write side (an HTTP client after its
/// request, `copy_bidirectional` when one side is done) must change nothing on the wire:
///
/// - `poll_shutdown` flushes, records that writing is over and returns `Ok`; it sends no
///   close_notify (no `SSL_shutdown`) and does not shut the TCP write side, so the phone's reply
///   can still be read to the end. Writes after it fail with `BrokenPipe`.
/// - Dropping it sets `SO_LINGER` 0, so the close is a reset: the connection ends at once, also
///   for the phone, and never sends close_notify either.
pub struct HalfCloseSafe {
    inner: TlsStream,
    write_done: bool,
}

impl HalfCloseSafe {
    pub fn new(inner: TlsStream) -> HalfCloseSafe {
        HalfCloseSafe {
            inner,
            write_done: false,
        }
    }

    /// The underlying connection (version and suite, for logs).
    pub fn ssl(&self) -> &openssl::ssl::SslRef {
        self.inner.ssl()
    }
}

impl fmt::Debug for HalfCloseSafe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HalfCloseSafe")
            .field("peer", &self.inner.get_ref().peer_addr().ok())
            .field("write_done", &self.write_done)
            .finish()
    }
}

impl AsyncRead for HalfCloseSafe {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for HalfCloseSafe {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.write_done {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.write_done {
            std::task::ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
            self.write_done = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for HalfCloseSafe {
    fn drop(&mut self) {
        // The TlsStream's own drop frees the SSL without SSL_shutdown; the socket then closes
        // with a reset instead of a FIN.
        if let Err(error) = SockRef::from(self.inner.get_ref()).set_linger(Some(Duration::ZERO)) {
            tracing::debug!("wireless: SO_LINGER 0 not set: {error}");
        }
    }
}

async fn handshake(address: SocketAddr, context: &SslContext) -> Result<TlsStream, TlsError> {
    let tcp = TcpStream::connect(address)
        .await
        .map_err(|e| TlsError::Io(format!("connect {address}: {e}")))?;
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10))
        .with_retries(3);
    SockRef::from(&tcp)
        .set_tcp_keepalive(&keepalive)
        .map_err(|e| TlsError::Io(format!("keepalive: {e}")))?;
    let ssl = Ssl::new(context).map_err(|e| TlsError::Io(e.to_string()))?;
    let mut stream = TlsStream::new(ssl, tcp).map_err(|e| TlsError::Io(e.to_string()))?;
    if let Err(error) = Pin::new(&mut stream).connect().await {
        let reasons = error
            .ssl_error()
            .map(|stack| stack.errors().iter().map(|e| e.reason_code()).collect());
        return Err(classify_failure(
            reasons.unwrap_or_default(),
            error.to_string(),
        ));
    }
    check_suite(stream.ssl().current_cipher().map(|cipher| cipher.name()))?;
    Ok(stream)
}

/// A failed handshake: `UnknownIdentity` when the error stack holds the reason OpenSSL records
/// for a received alert 115 (`SSL_R_TLSV1_ALERT_UNKNOWN_PSK_IDENTITY`, 1115), else `Io`.
fn classify_failure(reasons: Vec<i32>, message: String) -> TlsError {
    let unknown = SslAlert::UNKNOWN_PSK_IDENTITY.reason_code();
    if reasons.contains(&unknown) {
        TlsError::UnknownIdentity
    } else {
        TlsError::Io(format!("handshake: {message}"))
    }
}

fn check_suite(negotiated: Option<&str>) -> Result<(), TlsError> {
    if negotiated == Some(CIPHER) {
        Ok(())
    } else {
        Err(TlsError::WrongSuite)
    }
}

/// A context for one connection: the PSK callback carries this identity and key only.
fn client_context(identity: &str, psk: &[u8; 32]) -> Result<SslContext, ErrorStack> {
    let mut builder = SslContextBuilder::new(SslMethod::tls_client())?;
    restrict(&mut builder)?;
    // A PSK suite has no certificates; nothing to verify.
    builder.set_verify(SslVerifyMode::NONE);
    let identity = identity.as_bytes().to_vec();
    let key = Zeroizing::new(*psk);
    builder.set_psk_client_callback(move |_ssl, _hint, identity_out, psk_out| {
        // The identity goes out NUL-terminated; OpenSSL's buffer holds 128 bytes and more.
        if identity.len() + 1 > identity_out.len() || key.len() > psk_out.len() {
            return Ok(0);
        }
        identity_out[..identity.len()].copy_from_slice(&identity);
        identity_out[identity.len()] = 0;
        psk_out[..key.len()].copy_from_slice(key.as_slice());
        Ok(key.len())
    });
    Ok(builder.build())
}

/// TLS 1.2 only, the one suite, no session cache, no tickets. Shared with the test server.
fn restrict(builder: &mut SslContextBuilder) -> Result<(), ErrorStack> {
    builder.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_cipher_list(CIPHER)?;
    builder.set_session_cache_mode(SslSessionCacheMode::OFF);
    builder.set_options(SslOptions::NO_TICKET);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio_openssl::SslStream;

    const KEY: [u8; 32] = [7; 32];
    const LIMIT: Duration = Duration::from_secs(10);

    /// A server like the phone's: TLS 1.2, `cipher`, one known identity; for any other identity
    /// the PSK callback finds no key.
    fn server_context(cipher: &str) -> SslContext {
        server_context_for(cipher, "known", KEY)
    }

    /// As [`server_context`], knowing `identity` with `key`.
    pub(crate) fn server_context_for(cipher: &str, identity: &str, key: [u8; 32]) -> SslContext {
        let mut builder = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
        restrict(&mut builder).unwrap();
        builder.set_cipher_list(cipher).unwrap();
        let known = identity.as_bytes().to_vec();
        builder.set_psk_server_callback(move |_ssl, identity, psk_out| {
            if identity == Some(known.as_slice()) {
                psk_out[..32].copy_from_slice(&key);
                Ok(32)
            } else {
                Ok(0)
            }
        });
        builder.build()
    }

    /// Accepts one connection, completes the handshake and echoes one line. The server-side
    /// result (the handshake's error, if any) comes back through the handle.
    pub(crate) async fn serve_one(
        cipher: &str,
    ) -> (SocketAddr, tokio::task::JoinHandle<Result<(), String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let context = server_context(cipher);
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut stream = SslStream::new(Ssl::new(&context).unwrap(), tcp).unwrap();
            if let Err(error) = Pin::new(&mut stream).accept().await {
                // Close the way a real server does: the alert is already sent; read what the
                // client sent after it (its Finished) so the close is a FIN, not an RST that
                // could overtake the alert.
                let tcp = stream.get_mut();
                let _ = tcp.shutdown().await;
                let mut rest = [0u8; 1024];
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    while matches!(tcp.read(&mut rest).await, Ok(n) if n > 0) {}
                })
                .await;
                return Err(error.to_string());
            }
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            reader
                .get_mut()
                .write_all(line.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            reader
                .get_mut()
                .shutdown()
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        });
        (address, task)
    }

    #[tokio::test]
    async fn handshakes_and_echoes_a_line() {
        let (address, server) = serve_one(CIPHER).await;
        let stream = connect(address, "known", &KEY, LIMIT).await.unwrap();
        assert_eq!(stream.ssl().current_cipher().unwrap().name(), CIPHER);
        assert_eq!(stream.ssl().version_str(), "TLSv1.2");
        assert!(!stream.ssl().session_reused());
        let mut reader = BufReader::new(stream);
        reader.get_mut().write_all(b"CG1 PAIR\n").await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "CG1 PAIR\n");
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_unknown_identity_is_reported_as_such() {
        let (address, server) = serve_one(CIPHER).await;
        let error = connect(address, "stranger", &KEY, LIMIT).await.unwrap_err();
        assert_eq!(error, TlsError::UnknownIdentity);
        // OpenSSL's server sends the same alert the phone does (115) when it has no key.
        let server_error = server.await.unwrap().unwrap_err();
        assert!(
            server_error.contains("psk identity not found"),
            "{server_error}"
        );
    }

    #[tokio::test]
    async fn the_wrong_key_is_not_an_unknown_identity() {
        let (address, server) = serve_one(CIPHER).await;
        let error = connect(address, "known", &[8; 32], LIMIT)
            .await
            .unwrap_err();
        assert!(matches!(error, TlsError::Io(_)), "{error:?}");
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_server_without_the_suite_fails_the_handshake() {
        let (address, server) = serve_one("ECDHE-PSK-AES128-CBC-SHA256").await;
        let error = connect(address, "known", &KEY, LIMIT).await.unwrap_err();
        assert!(matches!(error, TlsError::Io(_)), "{error:?}");
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn nothing_listening_is_an_io_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(matches!(
            connect(address, "known", &KEY, LIMIT).await,
            Err(TlsError::Io(_))
        ));
    }

    /// What the server saw of a client that half-closed and then dropped its stream. Through
    /// `tokio-openssl` both close_notify and a bare TCP FIN read as `Ok(0)`; a reset is
    /// `ConnectionReset`. Whether close_notify came is told by the raw bytes: after the
    /// handshake the client sends an alert record (content type 21) only to close or to fail.
    #[derive(Debug)]
    pub(crate) struct ServerView {
        pub(crate) request: String,
        /// The read in the 300 ms after the half-close, if it completed.
        pub(crate) after_half_close: Option<io::Result<usize>>,
        /// The read once the client dropped the stream.
        pub(crate) after_drop: io::Result<usize>,
        /// TLS alert records among everything the client sent.
        pub(crate) client_alerts: usize,
    }

    /// The server's TCP stream, keeping a copy of every byte received.
    struct Recorded {
        inner: TcpStream,
        received: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl AsyncRead for Recorded {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let before = buf.filled().len();
            let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
            let new = buf.filled()[before..].to_vec();
            self.received.lock().unwrap().extend_from_slice(&new);
            polled
        }
    }

    impl AsyncWrite for Recorded {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// The number of alert records (type 21) in a TLS record stream.
    fn alert_records(mut bytes: &[u8]) -> usize {
        let mut alerts = 0;
        while bytes.len() >= 5 {
            let length = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
            if bytes[0] == 21 {
                alerts += 1;
            }
            bytes = &bytes[(5 + length).min(bytes.len())..];
        }
        alerts
    }

    pub(crate) const REPLY_LEN: usize = 100 * 1024;

    pub(crate) fn reply_byte(i: usize) -> u8 {
        (i % 251) as u8
    }

    /// A phone-like server for one connection over `context`: reads the request line, waits
    /// for `half_closed`, listens 300 ms for anything more, writes a 100 KiB reply, then reads
    /// until the client is gone.
    pub(crate) async fn serve_reply(
        context: SslContext,
        half_closed: tokio::sync::oneshot::Receiver<()>,
    ) -> (SocketAddr, tokio::task::JoinHandle<ServerView>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let received = std::sync::Arc::default();
            let tcp = Recorded {
                inner: tcp,
                received: std::sync::Arc::clone(&received),
            };
            let mut stream = SslStream::new(Ssl::new(&context).unwrap(), tcp).unwrap();
            Pin::new(&mut stream).accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while request.last() != Some(&b'\n') {
                assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
                request.push(byte[0]);
            }
            half_closed.await.unwrap();
            let mut more = [0u8; 16];
            let after_half_close =
                tokio::time::timeout(Duration::from_millis(300), stream.read(&mut more))
                    .await
                    .ok();
            let reply: Vec<u8> = (0..REPLY_LEN).map(reply_byte).collect();
            // A peer that already closed may refuse the reply; the view says what it heard.
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                stream.write_all(&reply).await?;
                stream.flush().await
            })
            .await;
            let after_drop = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut more))
                .await
                .expect("the client's drop reached the server");
            let client_alerts = alert_records(&received.lock().unwrap());
            ServerView {
                request: String::from_utf8(request).unwrap(),
                after_half_close,
                after_drop,
                client_alerts,
            }
        });
        (address, task)
    }

    /// Checks a view of a `HalfCloseSafe` client: request seen, nothing heard after the
    /// half-close, never close_notify, and a reset at the drop (SO_LINGER 0).
    pub(crate) fn assert_half_close_safe(view: &ServerView, request: &str) {
        assert_eq!(view.request, request);
        assert!(view.after_half_close.is_none(), "{view:?}");
        assert_eq!(view.client_alerts, 0, "close_notify sent: {view:?}");
        // A reset, not an orderly close (FIN).
        match &view.after_drop {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset, "{view:?}"),
            Ok(n) => panic!("read {n} bytes instead of a reset: {view:?}"),
        }
    }

    /// Review Focus 4: the user shuts its write side after the request; the reply is still read
    /// in full, the server never gets close_notify, and dropping the stream resets the TCP
    /// connection.
    #[tokio::test]
    async fn a_half_close_keeps_the_connection_and_the_drop_resets_it() {
        let (half_closed, wait) = tokio::sync::oneshot::channel();
        let (address, server) = serve_reply(server_context(CIPHER), wait).await;
        let mut stream = HalfCloseSafe::new(connect(address, "known", &KEY, LIMIT).await.unwrap());
        stream.write_all(b"GET / HTTP/1.1\n").await.unwrap();
        stream.shutdown().await.unwrap();
        // Shutting down again is harmless; writing after it is not allowed.
        let limit = Duration::from_secs(2);
        tokio::time::timeout(limit, stream.shutdown())
            .await
            .unwrap()
            .unwrap();
        let written = tokio::time::timeout(limit, stream.write_all(b"x")).await;
        assert_eq!(
            written.unwrap().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        half_closed.send(()).unwrap();
        let mut reply = vec![0u8; REPLY_LEN];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut reply))
            .await
            .expect("the reply arrived")
            .unwrap();
        assert!(reply.iter().enumerate().all(|(i, b)| *b == reply_byte(i)));
        drop(stream);
        assert_half_close_safe(&server.await.unwrap(), "GET / HTTP/1.1\n");
    }

    /// The plain `TlsStream`, for contrast: its shutdown is heard at once (close_notify), which
    /// is what `HalfCloseSafe` exists to prevent.
    #[tokio::test]
    async fn without_the_wrapper_a_shutdown_sends_close_notify() {
        let (half_closed, wait) = tokio::sync::oneshot::channel();
        let (address, server) = serve_reply(server_context(CIPHER), wait).await;
        let mut stream = connect(address, "known", &KEY, LIMIT).await.unwrap();
        stream.write_all(b"GET / HTTP/1.1\n").await.unwrap();
        stream.shutdown().await.unwrap();
        half_closed.send(()).unwrap();
        let mut reply = vec![0u8; REPLY_LEN];
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut reply)).await;
        drop(stream);
        let view = server.await.unwrap();
        assert!(
            matches!(view.after_half_close, Some(Ok(0))),
            "heard at once: {view:?}"
        );
        assert_eq!(view.client_alerts, 1, "close_notify: {view:?}");
    }

    #[tokio::test]
    async fn the_time_limit_is_the_callers() {
        // Accepts TCP and never answers the handshake.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let holder = tokio::spawn(async move {
            let (_tcp, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let started = std::time::Instant::now();
        let error = connect(address, "known", &KEY, Duration::from_millis(300))
            .await
            .unwrap_err();
        let took = started.elapsed();
        assert_eq!(error, TlsError::Timeout);
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        holder.abort();
    }

    #[test]
    fn alert_records_are_counted_by_content_type() {
        // handshake (22), alert (21), application data (23), a truncated alert header.
        let stream = [
            22, 3, 3, 0, 2, 1, 1, 21, 3, 3, 0, 2, 1, 0, 23, 3, 3, 0, 1, 9, 21, 3,
        ];
        assert_eq!(alert_records(&stream), 1);
        assert_eq!(alert_records(&[]), 0);
    }

    #[test]
    fn failure_classification() {
        // 1115 = SSL_AD_REASON_OFFSET + 115, SSL_R_TLSV1_ALERT_UNKNOWN_PSK_IDENTITY.
        assert_eq!(
            classify_failure(vec![1115], String::new()),
            TlsError::UnknownIdentity
        );
        assert_eq!(
            classify_failure(vec![10, 1115], String::new()),
            TlsError::UnknownIdentity
        );
        // 1040 = handshake_failure, 1051 = decrypt_error: not this.
        assert!(matches!(
            classify_failure(vec![1040], "x".into()),
            TlsError::Io(_)
        ));
        assert!(matches!(
            classify_failure(vec![1051], "x".into()),
            TlsError::Io(_)
        ));
        assert!(matches!(
            classify_failure(vec![], "x".into()),
            TlsError::Io(_)
        ));
    }

    #[test]
    fn suite_check() {
        assert_eq!(check_suite(Some(CIPHER)), Ok(()));
        assert_eq!(
            check_suite(Some("ECDHE-PSK-AES128-CBC-SHA256")),
            Err(TlsError::WrongSuite)
        );
        assert_eq!(check_suite(None), Err(TlsError::WrongSuite));
    }

    /// Code pairing end to end over this TLS on the pairing channel, against a stand-in phone
    /// built from cg-core's phone-side helpers (its `testing` feature): both sides show the same
    /// code, the phone gets a valid F, and the computer keeps the key the phone derived.
    #[tokio::test]
    async fn code_pairing_runs_over_the_pairing_channel() {
        use cg_core::wireless::code_message::{self as message, Side, Transcript};
        use cg_core::wireless::code_pairing;
        use cg_core::wireless::pairing::{PAIRING_IDENTITY, Role, Session, pairing_channel_key};

        const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
        const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";
        let channel_key = *pairing_channel_key().as_bytes();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut builder = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
        restrict(&mut builder).unwrap();
        builder.set_psk_server_callback(move |_ssl, identity, psk_out| {
            if identity == Some(PAIRING_IDENTITY.as_bytes()) {
                psk_out[..32].copy_from_slice(&channel_key);
                Ok(32)
            } else {
                Ok(0)
            }
        });
        let context = builder.build();
        let phone = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = SslStream::new(Ssl::new(&context).unwrap(), tcp).unwrap();
            Pin::new(&mut stream).accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "CG1 PAIR\n");

            let session = Session::fixed(Role::Phone, [0x11; 32], [0x22; 16]);
            let mut transcript = Transcript::new();
            let c = message::commitment(&session.commitment(), PHONE_ID);
            transcript.add(&c);
            reader.get_mut().write_all(c.as_bytes()).await.unwrap();

            let mut m = String::new();
            reader.read_line(&mut m).await.unwrap();
            transcript.add(&m);
            let computer = message::parse_computer(m.trim_end_matches('\n')).unwrap();
            let p = message::phone(&session.public_key(), &session.nonce());
            transcript.add(&p);
            reader.get_mut().write_all(p.as_bytes()).await.unwrap();
            let agreed = session
                .agree(&computer.public_key, &computer.nonce)
                .unwrap();

            let signed = message::allowed_signed(IDENTITY, "Eve iPhone");
            transcript.add(&signed);
            let proof = message::proof(Side::Phone, &agreed.long_term_key, &transcript);
            let ok = message::allowed(IDENTITY, "Eve iPhone", &proof);
            reader.get_mut().write_all(ok.as_bytes()).await.unwrap();

            let mut f = String::new();
            tokio::time::timeout(message::FIRST_STEP_TIMEOUT, reader.read_line(&mut f))
                .await
                .unwrap()
                .unwrap();
            let computer_proof = message::parse_finished(f.trim_end_matches('\n')).unwrap();
            assert!(message::verify(
                &computer_proof,
                Side::Computer,
                &agreed.long_term_key,
                &transcript
            ));
            (computer.computer_name, agreed.code, agreed.long_term_key)
        });

        let stream = connect(address, PAIRING_IDENTITY, &channel_key, LIMIT)
            .await
            .unwrap();
        let shown = std::sync::Mutex::new(None);
        let paired = code_pairing::run(
            stream,
            PHONE_ID,
            "7C1E5B52-2D6A-4A0F-8F3B-0A9D5E4C1B22",
            "Desk PC",
            |code| {
                *shown.lock().unwrap() = Some(code);
                async { true }
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
        let (computer_name, phone_code, phone_key) = phone.await.unwrap();
        assert_eq!(computer_name, "Desk PC");
        assert_eq!(shown.into_inner().unwrap(), Some(phone_code));
        assert_eq!(paired.phone_id, PHONE_ID);
        assert_eq!(paired.identity, IDENTITY);
        assert_eq!(paired.long_term_key, phone_key);
    }
}
