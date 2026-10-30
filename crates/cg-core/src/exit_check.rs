//! The full check's way-out probe: a real HTTPS request through the phone, as the Mac
//! runs `curl --proxy socks5h://…` against Cloudflare's trace page. And the health
//! monitor's tunnel probe: a plain request that goes the way the computer's own traffic
//! goes (the TUN device, hev, the phone), as the Mac's `HealthProbes.tunnel()`.

use crate::forwarder::read_address;
use crate::resolver::BoxFuture;
use crate::{CoreError, Result};
use cg_protocol::socks::connect_domain_request;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::rustls::pki_types::ServerName;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(8);

pub trait ExitProbe: Send + Sync + 'static {
    /// Whether the phone reaches the internet, through the SOCKS forwarder on `socks_port`.
    fn check(&self, socks_port: u16) -> BoxFuture<'_, bool>;
}

pub struct HttpsProbe {
    host: String,
    path: String,
    tls: TlsConnector,
}

impl HttpsProbe {
    pub fn new(host: &str, path: &str) -> HttpsProbe {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = crate::tls::client_builder(crate::tls::provider())
            .with_root_certificates(roots)
            .with_no_client_auth();
        HttpsProbe {
            host: host.into(),
            path: path.into(),
            tls: TlsConnector::from(Arc::new(config)),
        }
    }

    pub fn cloudflare() -> HttpsProbe {
        HttpsProbe::new("www.cloudflare.com", "/cdn-cgi/trace")
    }

    /// A SOCKS5 CONNECT to `host:443`; None when the phone refused it.
    async fn connect(&self, socks_port: u16) -> Result<Option<TcpStream>> {
        let mut stream = TcpStream::connect(("127.0.0.1", socks_port)).await?;
        stream.write_all(&[5, 1, 0]).await?;
        let mut method = [0u8; 2];
        stream.read_exact(&mut method).await?;
        if method != [5, 0] {
            return Ok(None);
        }
        stream
            .write_all(&connect_domain_request(&self.host, 443)?)
            .await?;
        let mut reply = [0u8; 4];
        stream.read_exact(&mut reply).await?;
        read_address(&mut stream, reply[3]).await?;
        Ok((reply[1] == 0).then_some(stream))
    }

    async fn run(&self, socks_port: u16) -> Result<bool> {
        let Some(tcp) = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(socks_port))
            .await
            .map_err(|_| CoreError::Timeout)??
        else {
            return Ok(false);
        };
        let name = ServerName::try_from(self.host.clone())
            .map_err(|_| CoreError::Tunnel("bad probe host".into()))?;
        let mut tls = self.tls.connect(name, tcp).await?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            cg_ipc::user_agent()
        );
        tls.write_all(request.as_bytes()).await?;
        let mut status = [0u8; 12];
        tls.read_exact(&mut status).await?;
        Ok(status.starts_with(b"HTTP/1.1 200"))
    }
}

impl ExitProbe for HttpsProbe {
    fn check(&self, socks_port: u16) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            matches!(
                tokio::time::timeout(TOTAL_TIMEOUT, self.run(socks_port)).await,
                Ok(Ok(true))
            )
        })
    }
}

pub trait TunnelProbe: Send + Sync + 'static {
    /// Whether the computer's own traffic gets out, through the tunnel.
    fn check(&self) -> BoxFuture<'_, bool>;
}

/// `GET http://captive.apple.com/hotspot-detect.html` without a proxy: 200 and `Success` in
/// the first 1024 bytes of the body. Redirects (a captive portal) are not followed and fail.
pub struct CaptiveProbe {
    host: String,
    port: u16,
    path: String,
    timeout: Duration,
}

/// The whole captive check, lookup included.
pub const CAPTIVE_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTIVE_BODY: usize = 1024;
/// Headers plus the part of the body that is looked at.
const CAPTIVE_READ_LIMIT: usize = 16 * 1024;

impl CaptiveProbe {
    pub fn new(host: &str, port: u16, path: &str, timeout: Duration) -> CaptiveProbe {
        CaptiveProbe {
            host: host.into(),
            port,
            path: path.into(),
            timeout,
        }
    }

    pub fn apple() -> CaptiveProbe {
        CaptiveProbe::new(
            "captive.apple.com",
            80,
            "/hotspot-detect.html",
            CAPTIVE_TIMEOUT,
        )
    }

    async fn run(&self) -> std::io::Result<bool> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
        // HTTP/1.0: no chunked body to unwrap before looking for the word.
        let request = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            cg_ipc::user_agent()
        );
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        let mut chunk = [0u8; 2048];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..n]);
            if let Some(head) = header_end(&response)
                && response.len() >= head + CAPTIVE_BODY
            {
                break;
            }
            if response.len() >= CAPTIVE_READ_LIMIT {
                break;
            }
        }
        Ok(captive_success(&response))
    }
}

fn header_end(response: &[u8]) -> Option<usize> {
    response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
}

/// Status 200 and `Success` within the first 1024 bytes of the body.
fn captive_success(response: &[u8]) -> bool {
    let Some(head) = header_end(response) else {
        return false;
    };
    let status_ok = response[..head]
        .split(|&b| b == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .and_then(|line| line.split_whitespace().nth(1))
        == Some("200");
    let body = &response[head..response.len().min(head + CAPTIVE_BODY)];
    status_ok && body.windows(7).any(|w| w == b"Success")
}

impl TunnelProbe for CaptiveProbe {
    fn check(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            matches!(
                tokio::time::timeout(self.timeout, self.run()).await,
                Ok(Ok(true))
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::Forwarder;
    use crate::phone::Phone;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, device};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn fails_fast_when_the_phone_refuses() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let _phone = FakePhone::start(
            &mux,
            FakePhoneOptions {
                exit_ok: false,
                ..FakePhoneOptions::default()
            },
        )
        .await;
        let fwd = Forwarder::start(Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        })
        .await
        .unwrap();
        let started = std::time::Instant::now();
        assert!(!HttpsProbe::cloudflare().check(fwd.port()).await);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn fails_when_nothing_listens() {
        let unused = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(!HttpsProbe::cloudflare().check(unused).await);
    }

    /// A one-shot HTTP server that answers with `response` (after `delay`).
    async fn http_once(response: Vec<u8>, delay: Duration) -> CaptiveProbe {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 1024];
            let _ = client.read(&mut request).await;
            tokio::time::sleep(delay).await;
            let _ = client.write_all(&response).await;
        });
        CaptiveProbe::new(
            "127.0.0.1",
            port,
            "/hotspot-detect.html",
            Duration::from_millis(500),
        )
    }

    const SUCCESS: &str = "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>";

    #[tokio::test]
    async fn captive_probe_wants_200_and_success() {
        let ok = format!("HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n{SUCCESS}");
        assert!(http_once(ok.into(), Duration::ZERO).await.check().await);
        // A captive portal's redirect is not followed.
        let redirect =
            "HTTP/1.1 302 Found\r\nLocation: http://portal.example/\r\n\r\nSuccess".to_string();
        assert!(
            !http_once(redirect.into(), Duration::ZERO)
                .await
                .check()
                .await
        );
        // 200 from a portal that rewrote the page.
        let rewritten = "HTTP/1.1 200 OK\r\n\r\n<html>Sign in</html>".to_string();
        assert!(
            !http_once(rewritten.into(), Duration::ZERO)
                .await
                .check()
                .await
        );
        // `Success` only after the first 1024 bytes of the body.
        let late = format!("HTTP/1.1 200 OK\r\n\r\n{}{SUCCESS}", " ".repeat(1024));
        assert!(!http_once(late.into(), Duration::ZERO).await.check().await);
    }

    /// The probes name the platform they run on: CellGlide-Windows on Windows.
    #[tokio::test]
    async fn captive_probe_sends_the_platform_user_agent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 1024];
            let n = client.read(&mut request).await.unwrap();
            let ok = format!("HTTP/1.0 200 OK\r\n\r\n{SUCCESS}");
            let _ = client.write_all(ok.as_bytes()).await;
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        let probe = CaptiveProbe::new(
            "127.0.0.1",
            port,
            "/hotspot-detect.html",
            Duration::from_millis(500),
        );
        assert!(probe.check().await);
        let request = seen.await.unwrap();
        let platform = if cfg!(windows) { "Windows" } else { "Linux" };
        let expected = format!(
            "User-Agent: CellGlide-{platform}/{}\r\n",
            env!("CARGO_PKG_VERSION")
        );
        assert!(request.contains(&expected), "{request}");
    }

    #[tokio::test]
    async fn captive_probe_gives_up_at_its_timeout() {
        let ok = format!("HTTP/1.0 200 OK\r\n\r\n{SUCCESS}");
        let probe = http_once(ok.into(), Duration::from_secs(3)).await;
        let started = std::time::Instant::now();
        assert!(!probe.check().await);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(CaptiveProbe::apple().timeout, Duration::from_secs(5));
    }

    /// Needs internet access: run with `cargo test -p cg-core exit_check -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn reaches_cloudflare_through_a_plain_socks_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            client.read_exact(&mut greeting).await.unwrap();
            client.write_all(&[5, 0]).await.unwrap();
            let mut head = [0u8; 5];
            client.read_exact(&mut head).await.unwrap();
            let mut rest = vec![0u8; head[4] as usize + 2];
            client.read_exact(&mut rest).await.unwrap();
            let host = String::from_utf8(rest[..rest.len() - 2].to_vec()).unwrap();
            let port = u16::from_be_bytes([rest[rest.len() - 2], rest[rest.len() - 1]]);
            let mut upstream = TcpStream::connect((host.as_str(), port)).await.unwrap();
            client
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });
        assert!(HttpsProbe::cloudflare().check(port).await);
    }
}
