//! Sends a prepared bundle to CellGlide's diagnostics endpoint; only ever on the user's say-so.
//! A small HTTP/1.1 client over tokio and rustls, with a 30-second limit on the whole exchange.

use crate::bundle::MAX_BYTES;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{CONTENT_TYPE, HOST, USER_AGENT};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub use cg_tools::net::user_agent;

pub const ENDPOINT: &str = "https://diag.cellglide.com/v1/reports";
const TIMEOUT: Duration = Duration::from_secs(30);
/// The reply is `{"code":"CG-XXXXX"}`; anything much bigger is not ours.
const MAX_REPLY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    /// 429: the server's per-minute limit.
    #[error("too many reports")]
    TooFrequent,
    #[error("rejected with status {0}")]
    Rejected(u16),
    /// Could not connect, TLS failed, or the exchange timed out.
    #[error("offline")]
    Offline,
    #[error("unreadable reply")]
    BadReply,
}

/// Where to POST. Production is always TLS; tests point at a local server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub path: String,
}

impl Target {
    pub fn production() -> Target {
        Target {
            host: "diag.cellglide.com".into(),
            port: 443,
            tls: true,
            path: "/v1/reports".into(),
        }
    }
}

/// POSTs `body` and returns the report code the server gives back.
pub async fn upload(body: &[u8], target: &Target) -> Result<String, UploadError> {
    upload_with(body, target, cg_tools::net::client_config()).await
}

/// `upload` with another TLS client config (tests trust a local certificate). Not public:
/// production code must not be able to hand in a config that skips verification.
pub(crate) async fn upload_with(
    body: &[u8],
    target: &Target,
    config: Arc<ClientConfig>,
) -> Result<String, UploadError> {
    if body.len() > MAX_BYTES {
        // Same answer the server would give; nothing is sent.
        return Err(UploadError::Rejected(413));
    }
    tokio::time::timeout(TIMEOUT, exchange(body, target, config))
        .await
        .unwrap_or(Err(UploadError::Offline))
}

async fn exchange(
    body: &[u8],
    target: &Target,
    config: Arc<ClientConfig>,
) -> Result<String, UploadError> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|_| UploadError::Offline)?;
    if target.tls {
        let name = ServerName::try_from(target.host.clone()).map_err(|_| UploadError::Offline)?;
        let stream = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|_| UploadError::Offline)?;
        send(TokioIo::new(stream), body, target).await
    } else {
        send(TokioIo::new(tcp), body, target).await
    }
}

async fn send<I>(io: I, body: &[u8], target: &Target) -> Result<String, UploadError>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|_| UploadError::Offline)?;
    let driver = tokio::spawn(connection);
    let result = async {
        let default_port = if target.tls { 443 } else { 80 };
        let host = if target.port == default_port {
            target.host.clone()
        } else {
            format!("{}:{}", target.host, target.port)
        };
        let request = Request::builder()
            .method(Method::POST)
            .uri(target.path.as_str())
            .header(HOST, host)
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, user_agent())
            .body(Full::new(Bytes::copy_from_slice(body)))
            .map_err(|_| UploadError::BadReply)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| UploadError::Offline)?;
        match response.status().as_u16() {
            429 => return Err(UploadError::TooFrequent),
            200 => {}
            other => return Err(UploadError::Rejected(other)),
        }
        let bytes = Limited::new(response.into_body(), MAX_REPLY)
            .collect()
            .await
            .map_err(|_| UploadError::BadReply)?
            .to_bytes();
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| UploadError::BadReply)?;
        match value.get("code").and_then(|c| c.as_str()) {
            Some(code) if code.starts_with("CG-") => Ok(code.to_string()),
            _ => Err(UploadError::BadReply),
        }
    }
    .await;
    driver.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::RootCertStore;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    /// The User-Agent each platform's build sends, written out independently of the code.
    fn expected_agent() -> String {
        #[cfg(windows)]
        let platform = "Windows";
        #[cfg(not(windows))]
        let platform = "Linux";
        format!("CellGlide-{platform}/{}", env!("CARGO_PKG_VERSION"))
    }

    const CERT: &str = include_str!("../tests/fixtures/server-cert.pem");
    // Throwaway test key generated for these tests only; it protects nothing.
    const KEY: &str = include_str!("../tests/fixtures/server-key.pem");

    /// Reads one request (head and body), sends `reply`, and returns the raw request text.
    async fn serve_one<S: AsyncReadExt + AsyncWriteExt + Unpin>(
        mut stream: S,
        reply: &str,
    ) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        let (status, body) = reply.split_once('|').unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.ok();
        String::from_utf8_lossy(&raw).to_string()
    }

    /// A plain-HTTP fake that answers each connection with the next reply.
    async fn fake_http(replies: &[&str]) -> (Target, std::sync::Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let replies: Vec<String> = replies.iter().map(|s| s.to_string()).collect();
        let log = seen.clone();
        tokio::spawn(async move {
            for reply in replies {
                let (stream, _) = listener.accept().await.unwrap();
                let request = serve_one(stream, &reply).await;
                log.lock().unwrap().push(request);
            }
        });
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            path: "/v1/reports".into(),
        };
        (target, seen)
    }

    #[tokio::test]
    async fn returns_the_code_and_sends_the_user_agent() {
        let (target, seen) = fake_http(&[r#"200 OK|{"code":"CG-7K3P9"}"#]).await;
        let code = upload(b"{\"a\":1}", &target).await.unwrap();
        assert_eq!(code, "CG-7K3P9");
        let request = seen.lock().unwrap()[0].clone();
        let lower = request.to_ascii_lowercase();
        assert!(
            request.starts_with("POST /v1/reports HTTP/1.1"),
            "{request}"
        );
        assert!(
            lower.contains(&format!("user-agent: {}", expected_agent()).to_ascii_lowercase()),
            "{request}"
        );
        assert!(
            lower.contains("content-type: application/json"),
            "{request}"
        );
        assert!(request.ends_with("{\"a\":1}"), "{request}");
    }

    #[tokio::test]
    async fn maps_errors() {
        let (target, _) = fake_http(&[
            "429 Too Many Requests|",
            "413 Payload Too Large|",
            "200 OK|nope",
            r#"200 OK|{"code":"XX-1"}"#,
        ])
        .await;
        assert_eq!(upload(b"{}", &target).await, Err(UploadError::TooFrequent));
        assert_eq!(
            upload(b"{}", &target).await,
            Err(UploadError::Rejected(413))
        );
        assert_eq!(upload(b"{}", &target).await, Err(UploadError::BadReply));
        assert_eq!(upload(b"{}", &target).await, Err(UploadError::BadReply));
        // Nobody listens.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            path: "/".into(),
        };
        assert_eq!(upload(b"{}", &target).await, Err(UploadError::Offline));
    }

    #[tokio::test]
    async fn refuses_a_body_over_the_limit_without_connecting() {
        // Nobody listens on this port; the size check must answer first.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            path: "/".into(),
        };
        let big = vec![b'x'; MAX_BYTES + 1];
        assert_eq!(upload(&big, &target).await, Err(UploadError::Rejected(413)));
        // Exactly the limit is allowed through (and then fails to connect).
        let ok = vec![b'x'; MAX_BYTES];
        assert_eq!(upload(&ok, &target).await, Err(UploadError::Offline));
    }

    #[test]
    fn production_target_is_the_endpoint() {
        let t = Target::production();
        assert_eq!(format!("https://{}{}", t.host, t.path), ENDPOINT,);
        assert!(t.tls && t.port == 443);
        assert_eq!(user_agent(), expected_agent());
    }

    fn server_cert() -> CertificateDer<'static> {
        CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()
    }

    async fn fake_https(reply: &str) -> (Target, tokio::task::JoinHandle<String>) {
        let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![server_cert()], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = reply.to_string();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            match acceptor.accept(tcp).await {
                Ok(stream) => serve_one(stream, &reply).await,
                Err(e) => format!("handshake failed: {e}"),
            }
        });
        (
            Target {
                host: "localhost".into(),
                port,
                tls: true,
                path: "/v1/reports".into(),
            },
            task,
        )
    }

    #[tokio::test]
    async fn speaks_tls_and_checks_the_certificate() {
        // Trusting the fake's certificate: the upload goes through.
        let (target, task) = fake_https(r#"200 OK|{"code":"CG-TLS01"}"#).await;
        let mut roots = RootCertStore::empty();
        roots.add(server_cert()).unwrap();
        let config = cg_tools::net::client_config_with_roots(roots);
        assert_eq!(
            upload_with(b"{}", &target, config).await.as_deref(),
            Ok("CG-TLS01")
        );
        assert!(
            task.await
                .unwrap()
                .to_ascii_lowercase()
                .contains(&format!("user-agent: {}", expected_agent()).to_ascii_lowercase())
        );
        // The normal public roots do not trust it: the upload refuses, as offline.
        let (target, task) = fake_https(r#"200 OK|{"code":"CG-TLS02"}"#).await;
        assert_eq!(upload(b"{}", &target).await, Err(UploadError::Offline));
        task.abort();
    }
}
