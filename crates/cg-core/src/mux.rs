//! usbmuxd client: list devices, follow attach/detach, and open a TCP channel to a
//! port on the phone. On Linux and macOS usbmuxd is a Unix socket; on Windows,
//! Apple Mobile Device Service listens on TCP 127.0.0.1:27015.

use crate::{CoreError, Result};
use cg_protocol::mux::{self, Device, MuxEvent, MuxRequest};
use plist::Dictionary;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type MuxStream = Box<dyn Stream>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxEndpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(SocketAddr),
}

fn open_error(error: std::io::Error) -> CoreError {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    match error.kind() {
        NotFound | ConnectionRefused => CoreError::NoUsbService,
        _ => error.into(),
    }
}

impl MuxEndpoint {
    pub fn system() -> Self {
        #[cfg(unix)]
        {
            Self::Unix(PathBuf::from("/var/run/usbmuxd"))
        }
        #[cfg(not(unix))]
        {
            Self::Tcp(SocketAddr::from(([127, 0, 0, 1], 27015)))
        }
    }

    pub async fn open(&self) -> Result<MuxStream> {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(open_error)?;
                Ok(Box::new(stream))
            }
            Self::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .map_err(open_error)?;
                stream.set_nodelay(true)?;
                Ok(Box::new(stream))
            }
        }
    }
}

pub async fn read_message<S: AsyncRead + Unpin + ?Sized>(stream: &mut S) -> Result<Dictionary> {
    let mut header = [0u8; mux::HEADER_LEN];
    stream.read_exact(&mut header).await?;
    let mut body = vec![0u8; mux::body_len(&header)?];
    stream.read_exact(&mut body).await?;
    Ok(mux::decode_body(&body)?)
}

async fn exchange(stream: &mut MuxStream, request: &MuxRequest) -> Result<Dictionary> {
    let bytes = request.encode(1)?;
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        stream.write_all(&bytes).await?;
        read_message(stream.as_mut()).await
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}

async fn request(endpoint: &MuxEndpoint, request: &MuxRequest) -> Result<(MuxStream, Dictionary)> {
    let mut stream = endpoint.open().await?;
    let reply = exchange(&mut stream, request).await?;
    Ok((stream, reply))
}

pub async fn list_devices(endpoint: &MuxEndpoint) -> Result<Vec<Device>> {
    let (_, reply) = request(endpoint, &MuxRequest::ListDevices).await?;
    Ok(mux::device_list(&reply)?)
}

/// A byte stream to `port` on the phone. No timeouts stay on it.
pub async fn connect(endpoint: &MuxEndpoint, mux_id: u32, port: u16) -> Result<MuxStream> {
    let (stream, reply) = request(endpoint, &MuxRequest::Connect { mux_id, port }).await?;
    match mux::result_number(&reply) {
        Some(0) => Ok(stream),
        _ => Err(CoreError::PhoneNotReady),
    }
}

/// Whether this computer was trusted on the phone (usbmuxd holds a pairing record).
pub async fn has_pair_record(endpoint: &MuxEndpoint, serial: &str) -> Result<bool> {
    let (_, reply) = request(
        endpoint,
        &MuxRequest::ReadPairRecord {
            serial: serial.into(),
        },
    )
    .await?;
    Ok(mux::has_pair_record(&reply))
}

/// The pairing record usbmuxd keeps for this phone (a plist), if the computer was trusted.
pub async fn pair_record(endpoint: &MuxEndpoint, serial: &str) -> Result<Option<Vec<u8>>> {
    let (_, reply) = request(
        endpoint,
        &MuxRequest::ReadPairRecord {
            serial: serial.into(),
        },
    )
    .await?;
    Ok(reply
        .get("PairRecordData")
        .and_then(plist::Value::as_data)
        .map(<[u8]>::to_vec))
}

/// Attach and detach events; the receiver closes when usbmuxd goes away.
pub async fn listen(endpoint: &MuxEndpoint) -> Result<mpsc::Receiver<MuxEvent>> {
    let (mut stream, reply) = request(endpoint, &MuxRequest::Listen).await?;
    if mux::result_number(&reply) != Some(0) {
        return Err(cg_protocol::ProtocolError::Malformed("usbmuxd refused Listen").into());
    }
    let (tx, rx) = mpsc::channel(32);
    // Ends (closing the Listen connection) when the receiver is dropped, even if
    // usbmuxd stays quiet.
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tx.closed() => break,
                message = read_message(stream.as_mut()) => match message {
                    Ok(message) => {
                        if tx.send(mux::parse_event(&message)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, device, echo_server, missing_mux};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn lists_and_connects() {
        let mux = FakeMux::start().await;
        mux.attach(device("bbbb", 2));
        mux.attach(device("aaaa", 1));
        mux.route(9876, echo_server().await);
        let ep = mux.endpoint();
        let devices = list_devices(&ep).await.unwrap();
        assert_eq!(
            devices
                .iter()
                .map(|d| d.serial.as_str())
                .collect::<Vec<_>>(),
            ["aaaa", "bbbb"]
        );

        let mut stream = connect(&ep, 1, 9876).await.unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        assert!(matches!(
            connect(&ep, 1, 1234).await,
            Err(CoreError::PhoneNotReady)
        ));
        assert!(matches!(
            connect(&ep, 99, 9876).await,
            Err(CoreError::PhoneNotReady)
        ));
    }

    #[tokio::test]
    async fn listen_reports_attach_detach_and_closes_on_restart() {
        let mux = FakeMux::start().await;
        let mut events = listen(&mux.endpoint()).await.unwrap();
        mux.attach(device("aaaa", 1));
        assert_eq!(
            events.recv().await,
            Some(MuxEvent::Attached(device("aaaa", 1)))
        );
        mux.detach(1);
        assert_eq!(events.recv().await, Some(MuxEvent::Detached(1)));
        mux.drop_listeners();
        assert_eq!(events.recv().await, None);
    }

    #[tokio::test]
    async fn listen_task_ends_when_receiver_is_dropped() {
        let mux = FakeMux::start().await;
        let events = listen(&mux.endpoint()).await.unwrap();
        assert_eq!(mux.listener_count(), 1);
        drop(events);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while mux.listener_count() > 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "Listen connection still open"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn missing_usbmuxd_and_pair_records() {
        let gone = missing_mux();
        assert!(matches!(
            list_devices(&gone).await,
            Err(CoreError::NoUsbService)
        ));
        let mux = FakeMux::start().await;
        assert!(!has_pair_record(&mux.endpoint(), "aaaa").await.unwrap());
        mux.add_pair_record("aaaa");
        assert!(has_pair_record(&mux.endpoint(), "aaaa").await.unwrap());
    }
}
