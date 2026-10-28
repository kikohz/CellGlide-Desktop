//! A small HTTPS/HTTP GET over tokio, hyper (HTTP/1.1) and the shared rustls config.

use crate::ToolError;
use crate::net;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::header::{ACCEPT, HOST, USER_AGENT};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Replies bigger than this are not ours.
pub const MAX_BODY: usize = 1024 * 1024;

/// Where to connect. Production targets are always TLS; tests point at a local server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// GETs `path` and returns the status and the body (at most 1 MiB). The whole exchange,
/// connecting included, is limited to `timeout`.
pub async fn get(
    target: &Target,
    path: &str,
    user_agent: &str,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), ToolError> {
    get_with(target, path, user_agent, timeout, net::client_config()).await
}

/// `get` with another TLS client config (tests trust a local certificate). Not public:
/// production code must not be able to hand in a config that skips verification.
pub(crate) async fn get_with(
    target: &Target,
    path: &str,
    user_agent: &str,
    timeout: Duration,
    config: Arc<ClientConfig>,
) -> Result<(u16, Vec<u8>), ToolError> {
    tokio::time::timeout(timeout, exchange(target, path, user_agent, config))
        .await
        .unwrap_or_else(|_| Err(ToolError::Offline("timed out".into())))
}

fn offline(error: impl std::fmt::Display) -> ToolError {
    ToolError::Offline(error.to_string())
}

async fn exchange(
    target: &Target,
    path: &str,
    user_agent: &str,
    config: Arc<ClientConfig>,
) -> Result<(u16, Vec<u8>), ToolError> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(offline)?;
    if target.tls {
        let name = ServerName::try_from(target.host.clone()).map_err(offline)?;
        let stream = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(offline)?;
        send(TokioIo::new(stream), target, path, user_agent).await
    } else {
        send(TokioIo::new(tcp), target, path, user_agent).await
    }
}

async fn send<I>(
    io: I,
    target: &Target,
    path: &str,
    user_agent: &str,
) -> Result<(u16, Vec<u8>), ToolError>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(offline)?;
    let driver = tokio::spawn(connection);
    let result = async {
        let default_port = if target.tls { 443 } else { 80 };
        let host = if target.port == default_port {
            target.host.clone()
        } else {
            format!("{}:{}", target.host, target.port)
        };
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(HOST, host)
            .header(USER_AGENT, user_agent)
            .header(ACCEPT, "application/json")
            .body(Empty::<Bytes>::new())
            .map_err(|_| ToolError::BadReply)?;
        let response = sender.send_request(request).await.map_err(offline)?;
        let status = response.status().as_u16();
        let body = Limited::new(response.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|_| ToolError::BadReply)?
            .to_bytes();
        Ok((status, body.to_vec()))
    }
    .await;
    driver.abort();
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rustls::RootCertStore;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    const CERT: &str = include_str!("../../cg-diag/tests/fixtures/server-cert.pem");
    // Throwaway test key generated for these tests only; it protects nothing.
    const KEY: &str = include_str!("../../cg-diag/tests/fixtures/server-key.pem");

    /// Reads one request head, answers with `status` and `body`, returns the raw head.
    pub(crate) async fn serve_one<S: AsyncReadExt + AsyncWriteExt + Unpin>(
        mut stream: S,
        status: &str,
        body: &[u8],
    ) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
        }
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.ok();
        stream.shutdown().await.ok();
        String::from_utf8_lossy(&raw).to_string()
    }

    /// A plain-HTTP fake answering one connection; the join handle yields the request head.
    pub(crate) async fn fake_http(
        status: &'static str,
        body: Vec<u8>,
    ) -> (Target, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve_one(stream, status, &body).await
        });
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
        };
        (target, task)
    }

    const SHORT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn returns_status_and_body_and_sends_the_user_agent() {
        let (target, task) = fake_http("200 OK", b"{\"ok\":1}".to_vec()).await;
        let (status, body) = get(&target, "/v1/abc", "CellGlide-Linux/9.9", SHORT)
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"{\"ok\":1}"[..]));
        let head = task.await.unwrap().to_ascii_lowercase();
        assert!(head.starts_with("get /v1/abc http/1.1"));
        assert!(head.contains("user-agent: cellglide-linux/9.9"));
        assert!(head.contains(&format!("host: 127.0.0.1:{}", target.port)));
    }

    #[tokio::test]
    async fn a_non_200_status_is_returned_not_an_error() {
        let (target, _task) = fake_http("503 Service Unavailable", b"no".to_vec()).await;
        let (status, _) = get(&target, "/", "ua", SHORT).await.unwrap();
        assert_eq!(status, 503);
    }

    #[tokio::test]
    async fn a_body_over_one_mebibyte_is_bad() {
        let (target, _task) = fake_http("200 OK", vec![b'x'; MAX_BODY + 1]).await;
        assert_eq!(
            get(&target, "/", "ua", SHORT).await,
            Err(ToolError::BadReply)
        );
        let (target, _task) = fake_http("200 OK", vec![b'x'; MAX_BODY]).await;
        assert_eq!(
            get(&target, "/", "ua", SHORT).await.unwrap().1.len(),
            MAX_BODY
        );
    }

    #[tokio::test]
    async fn nothing_listening_is_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
        };
        assert!(matches!(
            get(&target, "/", "ua", SHORT).await,
            Err(ToolError::Offline(_))
        ));
    }

    #[tokio::test]
    async fn a_silent_server_times_out_as_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _keep = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
        };
        assert_eq!(
            get(&target, "/", "ua", Duration::from_millis(200)).await,
            Err(ToolError::Offline("timed out".into()))
        );
    }

    fn server_cert() -> CertificateDer<'static> {
        CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()
    }

    async fn fake_https() -> (Target, tokio::task::JoinHandle<()>) {
        let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![server_cert()], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            if let Ok(stream) = acceptor.accept(tcp).await {
                serve_one(stream, "200 OK", b"secure").await;
            }
        });
        let target = Target {
            host: "localhost".into(),
            port,
            tls: true,
        };
        (target, task)
    }

    #[tokio::test]
    async fn speaks_tls_and_checks_the_certificate() {
        let (target, _task) = fake_https().await;
        let mut roots = RootCertStore::empty();
        roots.add(server_cert()).unwrap();
        let config = net::client_config_with_roots(roots);
        let (status, body) = get_with(&target, "/", "ua", SHORT, config).await.unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"secure"[..]));
        // The bundled public roots do not trust the fake.
        let (target, task) = fake_https().await;
        assert!(matches!(
            get(&target, "/", "ua", SHORT).await,
            Err(ToolError::Offline(_))
        ));
        task.abort();
    }
}
