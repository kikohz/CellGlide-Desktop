//! The computer's side of the wireless link's TLS (design §8.1): TLS 1.2 only, the single suite
//! `TLS_ECDHE_PSK_WITH_CHACHA20_POLY1305_SHA256`, a pre-shared key per identity, no session
//! cache and no tickets, so every connection is a full handshake (the phone relies on that to
//! tell which identity a connection uses). Neither SChannel nor rustls offer PSK suites, hence
//! OpenSSL. TCP keepalive 30 s idle, 10 s interval, 3 probes, like the phone.

use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use openssl::error::ErrorStack;
use openssl::ssl::{
    Ssl, SslAlert, SslContext, SslContextBuilder, SslMethod, SslOptions, SslSessionCacheMode,
    SslVerifyMode, SslVersion,
};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// OpenSSL's name for the one suite both sides offer.
pub const CIPHER: &str = "ECDHE-PSK-CHACHA20-POLY1305";

/// TCP connect plus handshake. The phone itself allows 5 s for handshake and request line.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub type TlsStream = tokio_openssl::SslStream<TcpStream>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    /// The phone answered the handshake with alert 115 (unknown_psk_identity): it does not
    /// know this identity (any more).
    UnknownIdentity,
    /// The handshake completed with a suite other than [`CIPHER`].
    WrongSuite,
    /// Anything else: connect, timeout, another alert, the peer closing.
    Io(String),
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::UnknownIdentity => f.write_str("the phone does not know this identity"),
            TlsError::WrongSuite => write!(f, "the handshake did not use {CIPHER}"),
            TlsError::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TlsError {}

/// Note on the returned `TlsStream`: its `poll_shutdown` sends close_notify and shuts the TCP write
/// side; the session layer must not let a half-closing user trigger it (design §8.1).
///
/// Connects to the phone at `address` and completes the handshake as `identity` with `psk`.
/// The caller sends its request line right away (the phone's 5 s budget).
pub async fn connect(
    address: SocketAddr,
    identity: &str,
    psk: &[u8; 32],
) -> Result<TlsStream, TlsError> {
    let context = client_context(identity, psk).map_err(|e| TlsError::Io(e.to_string()))?;
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(address, &context)).await {
        Ok(result) => result,
        Err(_) => Err(TlsError::Io(format!(
            "no handshake with {address} within 10 s"
        ))),
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
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio_openssl::SslStream;

    const KEY: [u8; 32] = [7; 32];

    /// A server like the phone's: TLS 1.2, `cipher`, one known identity; for any other identity
    /// the PSK callback finds no key.
    fn server_context(cipher: &str) -> SslContext {
        let mut builder = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
        restrict(&mut builder).unwrap();
        builder.set_cipher_list(cipher).unwrap();
        builder.set_psk_server_callback(|_ssl, identity, psk_out| {
            if identity == Some(b"known".as_slice()) {
                psk_out[..32].copy_from_slice(&KEY);
                Ok(32)
            } else {
                Ok(0)
            }
        });
        builder.build()
    }

    /// Accepts one connection, completes the handshake and echoes one line. The server-side
    /// result (the handshake's error, if any) comes back through the handle.
    async fn serve_one(cipher: &str) -> (SocketAddr, tokio::task::JoinHandle<Result<(), String>>) {
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
        let stream = connect(address, "known", &KEY).await.unwrap();
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
        let error = connect(address, "stranger", &KEY).await.unwrap_err();
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
        let error = connect(address, "known", &[8; 32]).await.unwrap_err();
        assert!(matches!(error, TlsError::Io(_)), "{error:?}");
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_server_without_the_suite_fails_the_handshake() {
        let (address, server) = serve_one("ECDHE-PSK-AES128-CBC-SHA256").await;
        let error = connect(address, "known", &KEY).await.unwrap_err();
        assert!(matches!(error, TlsError::Io(_)), "{error:?}");
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn nothing_listening_is_an_io_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(matches!(
            connect(address, "known", &KEY).await,
            Err(TlsError::Io(_))
        ));
    }

    /// TEMP diagnostics: our client against `openssl s_server -rev` on CG_TLS_EXTERNAL_PORT.
    #[tokio::test]
    #[ignore]
    async fn temp_external_server() {
        let port: u16 = std::env::var("CG_TLS_EXTERNAL_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let stream = connect(SocketAddr::from(([127, 0, 0, 1], port)), "known", &KEY)
            .await
            .unwrap();
        println!(
            "EXTERNAL handshake ok: {} {}",
            stream.ssl().current_cipher().unwrap().name(),
            stream.ssl().version_str()
        );
        let mut reader = BufReader::new(stream);
        reader.get_mut().write_all(b"CG1 PAIR\n").await.unwrap();
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
        println!("EXTERNAL reply: {read:?} {line:?}");
        assert_eq!(line.trim_end(), "RIAP 1GC");
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
}
