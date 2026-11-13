//! The phone app's control services: hello, session watch, status and metrics.

use crate::mux::{self, MuxEndpoint, MuxStream};
use crate::wireless::ports::WirelessLink;
use crate::{CoreError, Result};
use cg_protocol::ProtocolError;
use cg_protocol::mux::Device;
use cg_protocol::phone::{
    HELLO_REQUEST, Hello, MAX_HELLO_BODY, MAX_HTTP_HEAD, MAX_METRICS_LINE, MAX_STATUS_LINE,
    PhoneMetrics, SharingStatus, StatusSequence, parse_http_head, port, watch_request,
};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A phone that sends no metrics for this long (app in background, cable pulled) ends the stream.
pub const METRICS_SILENCE: Duration = Duration::from_secs(5);

pub type Ended = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// How the computer reaches the phone: usbmuxd over the cable, or the wireless link. Either
/// way `Phone::open` gives a stream to one of the phone's ports.
#[derive(Clone)]
pub enum Link {
    Usb {
        endpoint: MuxEndpoint,
        device: Device,
    },
    Wireless(Arc<dyn WirelessLink>),
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Link::Usb { endpoint, device } => f
                .debug_struct("Usb")
                .field("endpoint", endpoint)
                .field("device", device)
                .finish(),
            Link::Wireless(link) => f.debug_tuple("Wireless").field(&link.phone_id()).finish(),
        }
    }
}

/// One phone and the link to it. Cheap to clone (the wireless link is shared).
#[derive(Clone, Debug)]
pub struct Phone {
    pub link: Link,
}

impl Phone {
    pub fn usb(endpoint: MuxEndpoint, device: Device) -> Phone {
        Phone {
            link: Link::Usb { endpoint, device },
        }
    }

    pub fn wireless(link: Arc<dyn WirelessLink>) -> Phone {
        Phone {
            link: Link::Wireless(link),
        }
    }

    pub async fn open(&self, port: u16) -> Result<MuxStream> {
        match &self.link {
            Link::Usb { endpoint, device } => mux::connect(endpoint, device.mux_id, port).await,
            Link::Wireless(link) => link.open(port).await,
        }
    }

    /// What the phone is known by: its USB serial, or `wireless:<phoneID>`.
    pub fn id(&self) -> String {
        match &self.link {
            Link::Usb { device, .. } => device.serial.clone(),
            Link::Wireless(link) => format!("wireless:{}", link.phone_id()),
        }
    }

    /// The usbmuxd device, for what only works over the cable (lockdown, link speed, cable
    /// pairing); `None` on the wireless link.
    pub fn usb_device(&self) -> Option<&Device> {
        match &self.link {
            Link::Usb { device, .. } => Some(device),
            Link::Wireless(_) => None,
        }
    }

    pub fn is_wireless(&self) -> bool {
        matches!(self.link, Link::Wireless(_))
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
        FakeMux, FakePhone, FakePhoneOptions, FakeWirelessLink, SESSION_A, SESSION_B, device,
        hello_json,
    };
    use std::time::Duration;

    async fn setup(options: FakePhoneOptions) -> (FakeMux, FakePhone, Phone) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, options).await;
        let phone = Phone::usb(mux.endpoint(), device("aaaa", 1));
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

    #[test]
    fn a_usb_phone_is_known_by_its_serial() {
        let phone = Phone::usb(crate::testing::missing_mux(), device("aaaa", 7));
        assert_eq!(phone.id(), "aaaa");
        assert_eq!(phone.usb_device(), Some(&device("aaaa", 7)));
        assert!(!phone.is_wireless());
    }

    #[tokio::test]
    async fn a_wireless_phone_has_its_own_id_and_no_usb_device() {
        let phone = Phone::wireless(FakeWirelessLink::echo("ABC").await);
        assert_eq!(phone.id(), "wireless:ABC");
        assert_eq!(phone.usb_device(), None);
        assert!(phone.is_wireless());
        assert_eq!(phone.clone().id(), "wireless:ABC");
    }

    #[tokio::test]
    async fn open_on_a_wireless_phone_goes_through_its_link() {
        let link = FakeWirelessLink::echo("ABC").await;
        let phone = Phone::wireless(link.clone());
        let mut stream = phone.open(8088).await.unwrap();
        assert_eq!(link.opened(), [8088]);
        stream.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");
    }

    #[tokio::test]
    async fn open_on_a_usb_phone_goes_through_usbmuxd() {
        let (mux, _fake, phone) = setup(FakePhoneOptions::default()).await;
        assert!(phone.open(port::CONTROL).await.is_ok());
        let unknown = Phone::usb(mux.endpoint(), device("zzzz", 9));
        assert!(unknown.open(port::CONTROL).await.is_err(), "no such device");
    }
}
