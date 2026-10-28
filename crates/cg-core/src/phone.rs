//! The phone app's control services: hello, session watch, status and metrics.

use crate::mux::{self, MuxEndpoint, MuxStream};
use crate::{CoreError, Result};
use cg_protocol::ProtocolError;
use cg_protocol::mux::Device;
use cg_protocol::phone::{
    HELLO_REQUEST, Hello, MAX_HELLO_BODY, MAX_HTTP_HEAD, MAX_METRICS_LINE, MAX_STATUS_LINE,
    PhoneMetrics, SharingStatus, StatusSequence, parse_http_head, port, watch_request,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A phone that sends no metrics for this long (app in background, cable pulled) ends the stream.
pub const METRICS_SILENCE: Duration = Duration::from_secs(5);

pub type Ended = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Debug)]
pub struct Phone {
    pub endpoint: MuxEndpoint,
    pub device: Device,
}

impl Phone {
    pub async fn open(&self, port: u16) -> Result<MuxStream> {
        mux::connect(&self.endpoint, self.device.mux_id, port).await
    }
}

async fn read_head<S: AsyncRead + Unpin + ?Sized>(stream: &mut S) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HTTP_HEAD {
            return Err(ProtocolError::TooLarge.into());
        }
        head.push(stream.read_u8().await?);
    }
    Ok(head)
}

pub async fn read_hello(phone: &Phone) -> Result<Hello> {
    timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = phone.open(port::CONTROL).await?;
        stream.write_all(HELLO_REQUEST).await?;
        let head = parse_http_head(&read_head(stream.as_mut()).await?)?;
        let len = match (head.status, head.content_length) {
            (200, Some(n)) if (1..=MAX_HELLO_BODY).contains(&n) => n,
            _ => return Err(CoreError::PhoneTooOld),
        };
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body).map_err(ProtocolError::from)?)
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}

/// Subscribes to the phone's session. The returned future completes when the phone
/// ends the session (stops sharing, quits, or the cable goes).
pub async fn watch(
    phone: &Phone,
    session_id: &str,
    computer_name: Option<&str>,
    link_speed: Option<u64>,
) -> Result<Ended> {
    let mut stream = phone.open(port::CONTROL).await?;
    stream
        .write_all(watch_request(session_id, computer_name, link_speed).as_bytes())
        .await?;
    let head = timeout(HANDSHAKE_TIMEOUT, read_head(stream.as_mut()))
        .await
        .map_err(|_| CoreError::Timeout)??;
    if !head.starts_with(b"HTTP/1.1 200 OK\r\n") {
        return Err(CoreError::SessionChanged);
    }
    Ok(Box::pin(async move {
        let mut byte = [0u8; 1];
        let _ = stream.read(&mut byte).await;
    }))
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = reader.read_u8().await?;
        if byte == b'\n' {
            return Ok(line);
        }
        line.push(byte);
        if line.len() >= max {
            return Err(ProtocolError::TooLarge.into());
        }
    }
}

pub async fn status_stream(phone: Phone, tx: mpsc::Sender<SharingStatus>) -> Result<()> {
    let mut reader = BufReader::new(phone.open(port::STATUS).await?);
    let mut sequence = StatusSequence::default();
    loop {
        let line = read_line(&mut reader, MAX_STATUS_LINE).await?;
        let status: SharingStatus = serde_json::from_slice(&line).map_err(ProtocolError::from)?;
        sequence.accept(&status)?;
        if tx.send(status).await.is_err() {
            return Ok(());
        }
    }
}

pub async fn metrics_stream(phone: Phone, tx: mpsc::Sender<PhoneMetrics>) -> Result<()> {
    let mut reader = BufReader::new(phone.open(port::METRICS).await?);
    loop {
        let line = timeout(METRICS_SILENCE, read_line(&mut reader, MAX_METRICS_LINE))
            .await
            .map_err(|_| CoreError::Timeout)??;
        let metrics: PhoneMetrics = serde_json::from_slice(&line).map_err(ProtocolError::from)?;
        if !metrics.is_valid() {
            return Err(ProtocolError::Malformed("metrics frame").into());
        }
        if tx.send(metrics).await.is_err() {
            return Ok(());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        FakeMux, FakePhone, FakePhoneOptions, SESSION_A, SESSION_B, device, hello_json,
    };
    use std::time::Duration;

    async fn setup(options: FakePhoneOptions) -> (FakeMux, FakePhone, Phone) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, options).await;
        let phone = Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        };
        (mux, fake, phone)
    }

    #[tokio::test]
    async fn hello_reads_capabilities() {
        let (_mux, _fake, phone) = setup(FakePhoneOptions::default()).await;
        let hello = read_hello(&phone).await.unwrap();
        hello.validate().unwrap();
        assert_eq!(hello.session_id, SESSION_A);
        assert!(hello.has(cg_protocol::phone::capability::UDP_OVER_USB));
    }

    #[tokio::test]
    async fn watch_ends_when_phone_ends_session() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let ended = watch(&phone, SESSION_A, Some("测试电脑"), None)
            .await
            .unwrap();
        let mut ended = Box::pin(ended);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut ended)
                .await
                .is_err(),
            "still running"
        );
        fake.end_session();
        tokio::time::timeout(Duration::from_secs(2), ended)
            .await
            .expect("watch should end");
    }

    #[tokio::test]
    async fn watch_with_stale_session_is_rejected() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        fake.set_hello(hello_json(&["socks5-tcp"], true, SESSION_B));
        assert!(matches!(
            watch(&phone, SESSION_A, None, None).await,
            Err(CoreError::SessionChanged)
        ));
    }

    #[tokio::test]
    async fn status_stream_rejects_out_of_order_frames() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(status_stream(phone, tx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let line = |rev: u64| {
            format!(
                r#"{{"service":"CellGlide-status","version":1,"instance":"{SESSION_A}","revision":{rev},"running":true,"starting":false,"background":true,"dnsEnabled":false}}"#
            )
        };
        fake.send_status(&line(1));
        assert!(!rx.recv().await.unwrap().dns_enabled);
        fake.send_status(&line(1));
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn metrics_stream_delivers_frames() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(metrics_stream(phone, tx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        fake.send_metrics(
            r#"{"service":"CellGlide-metrics","version":1,"battery":0.5,"download":2048}"#,
        );
        let m = rx.recv().await.unwrap();
        assert_eq!(m.battery, Some(0.5));
        assert_eq!(m.download, Some(2048.0));
    }
}
