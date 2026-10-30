//! The local SOCKS5 endpoint hev talks to. Every client gets its own channel to the
//! phone's SOCKS port; UDP ASSOCIATE is turned into the phone's udp-over-usb-v1.

use crate::Result;
use crate::mux::MuxStream;
use crate::phone::Phone;
use crate::udp_bridge;
use cg_protocol::phone::port;
use cg_protocol::socks::address_rest_len;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Matches the phone's session limit.
pub const MAX_CLIENTS: usize = 256;

/// How long a client may take from its greeting to the end of its request; an idle
/// client would otherwise hold one of the `MAX_CLIENTS` slots forever.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Forwarder {
    port: u16,
    cancel: CancellationToken,
}

impl Forwarder {
    pub async fn start(phone: Phone) -> Result<Forwarder> {
        Forwarder::start_with(phone, HANDSHAKE_TIMEOUT).await
    }

    pub async fn start_with(phone: Phone, handshake_timeout: Duration) -> Result<Forwarder> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = token.cancelled() => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((client, _)) = accepted else {
                    // Out of descriptors: back off instead of giving up on the port.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let phone = phone.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    let _ = client.set_nodelay(true);
                    tokio::select! {
                        _ = token.cancelled() => {}
                        _ = handle(client, phone, handshake_timeout) => {}
                    }
                });
            }
        });
        Ok(Forwarder { port, cancel })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The address bytes after ATYP, port included.
pub(crate) async fn read_address<S: AsyncRead + Unpin + ?Sized>(
    stream: &mut S,
    atyp: u8,
) -> Result<Vec<u8>> {
    let first = stream.read_u8().await?;
    let mut address = vec![first];
    address.resize(1 + address_rest_len(atyp, first)?, 0);
    stream.read_exact(&mut address[1..]).await?;
    Ok(address)
}

/// What the client asked for once the handshake is done.
struct Request {
    peer: MuxStream,
    header: [u8; 4],
    address: Vec<u8>,
}

async fn handle(mut client: TcpStream, phone: Phone, handshake_timeout: Duration) -> Result<()> {
    let negotiated = tokio::time::timeout(handshake_timeout, negotiate(&mut client, &phone)).await;
    let Ok(negotiated) = negotiated else {
        return Ok(()); // Idle client: dropping it frees the slot.
    };
    let Some(Request {
        mut peer,
        header,
        address,
    }) = negotiated?
    else {
        return Ok(());
    };
    // No timeout from here on: forwarding lasts as long as both sides want it.
    if header[1] == 3 {
        return udp_bridge::associate(client, peer, &address).await;
    }
    peer.write_all(&header).await?;
    peer.write_all(&address).await?;
    tokio::io::copy_bidirectional(&mut client, &mut peer).await?;
    Ok(())
}

/// Greeting, method choice and request; `None` when the phone declined every method.
async fn negotiate(client: &mut TcpStream, phone: &Phone) -> Result<Option<Request>> {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    if greeting[0] != 5 || greeting[1] == 0 {
        return Err(cg_protocol::ProtocolError::Malformed("socks greeting").into());
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods).await?;

    let mut peer = phone.open(port::SOCKS).await?;
    peer.write_all(&greeting).await?;
    peer.write_all(&methods).await?;
    let mut method = [0u8; 2];
    peer.read_exact(&mut method).await?;
    client.write_all(&method).await?;
    if method != [5, 0] {
        return Ok(None);
    }

    let mut header = [0u8; 4];
    client.read_exact(&mut header).await?;
    if header[0] != 5 || header[2] != 0 {
        return Err(cg_protocol::ProtocolError::Malformed("socks request").into());
    }
    let address = read_address(client, header[3]).await?;
    Ok(Some(Request {
        peer,
        header,
        address,
    }))
}

/// A SOCKS5 greeting through the forwarder; only a live phone answers it.
pub async fn socks_answers(port: u16, timeout: Duration) -> bool {
    socks_rtt(port, timeout).await.is_some()
}

/// How long a SOCKS greeting through the forwarder on `port` took to be answered, from
/// connecting to the reply; `None` when it wasn't answered within `timeout`.
pub async fn socks_rtt(port: u16, timeout: Duration) -> Option<Duration> {
    let started = tokio::time::Instant::now();
    let greet = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await?;
        s.write_all(&[5, 1, 0]).await?;
        let mut reply = [0u8; 2];
        s.read_exact(&mut reply).await?;
        Ok::<bool, std::io::Error>(reply == [5, 0])
    };
    match tokio::time::timeout(timeout, greet).await {
        Ok(Ok(true)) => Some(started.elapsed()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, device};
    use std::time::Duration;
    use tokio::net::{TcpStream, UdpSocket};

    async fn setup(options: FakePhoneOptions) -> (FakeMux, FakePhone, Forwarder) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, options).await;
        let fwd = Forwarder::start(Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        })
        .await
        .unwrap();
        (mux, fake, fwd)
    }

    async fn socks_hello(port: u16) -> TcpStream {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut m = [0u8; 2];
        s.read_exact(&mut m).await.unwrap();
        assert_eq!(m, [5, 0]);
        s
    }

    #[tokio::test]
    async fn tcp_connect_relays_both_ways() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let mut s = socks_hello(fwd.port()).await;
        s.write_all(&[5, 1, 0, 1, 1, 2, 3, 4, 0, 80]).await.unwrap();
        let mut reply = [0u8; 10];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        s.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn udp_associate_bridges_frames_and_ignores_other_senders() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let mut control = socks_hello(fwd.port()).await;
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..8], &[5, 0, 0, 1, 127, 0, 0, 1]);
        let relay = u16::from_be_bytes([reply[8], reply[9]]);

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let packet = [0, 0, 0, 1, 1, 1, 1, 1, 0, 53, b'q', b'1'];
        client.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], &packet);

        let intruder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        intruder
            .send_to(&packet, ("127.0.0.1", relay))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), intruder.recv(&mut buf))
                .await
                .is_err()
        );

        // Zero-length payloads survive.
        let empty = [0, 0, 0, 1, 1, 1, 1, 1, 0, 53];
        client.send_to(&empty, ("127.0.0.1", relay)).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], &empty);

        drop(control);
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        let after = tokio::time::timeout(Duration::from_millis(300), client.recv(&mut buf)).await;
        // No datagram comes back. Windows reports the closed relay port's ICMP
        // unreachable as ConnectionReset on the next receive; Unix just times out.
        #[cfg(windows)]
        if let Ok(Err(error)) = &after {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            return;
        }
        assert!(
            after.is_err(),
            "association closed with its control connection"
        );
    }

    #[tokio::test]
    async fn stray_bytes_on_control_do_not_end_association() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let mut control = socks_hello(fwd.port()).await;
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        let relay = u16::from_be_bytes([reply[8], reply[9]]);

        control.write_all(&[1, 2, 3]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let packet = [0, 0, 0, 1, 1, 1, 1, 1, 0, 53, b'q', b'2'];
        client.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf))
            .await
            .expect("association still open")
            .unwrap();
        assert_eq!(&buf[..n], &packet);
    }

    #[tokio::test]
    async fn phone_refusing_udp_gets_command_failed() {
        let (_m, _p, fwd) = setup(FakePhoneOptions {
            udp_ok: false,
            ..FakePhoneOptions::default()
        })
        .await;
        let mut control = socks_hello(fwd.port()).await;
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, cg_protocol::socks::REPLY_COMMAND_FAILED);
    }

    #[tokio::test]
    async fn socks_rtt_times_the_greeting() {
        let (_m, fake, fwd) = setup(FakePhoneOptions::default()).await;
        let rtt = socks_rtt(fwd.port(), Duration::from_secs(2)).await.unwrap();
        assert!(rtt < Duration::from_secs(2), "{rtt:?}");
        fake.set_socks_dead(true);
        assert_eq!(
            socks_rtt(fwd.port(), Duration::from_millis(500)).await,
            None
        );
    }

    #[tokio::test]
    async fn socks_answers_follows_phone() {
        let (_m, fake, fwd) = setup(FakePhoneOptions::default()).await;
        assert!(socks_answers(fwd.port(), Duration::from_secs(2)).await);
        fake.set_socks_dead(true);
        assert!(!socks_answers(fwd.port(), Duration::from_secs(2)).await);
        fake.set_socks_dead(false);
        assert!(socks_answers(fwd.port(), Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn idle_client_is_dropped_after_handshake_timeout() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let _fake = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let phone = Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        };
        let fwd = Forwarder::start_with(phone, Duration::from_millis(200))
            .await
            .unwrap();

        // Silent from the start.
        let mut idle = TcpStream::connect(("127.0.0.1", fwd.port())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut buf))
            .await
            .expect("closed by the forwarder")
            .unwrap_or(0);
        assert_eq!(n, 0);

        // Silent after the greeting, before the request.
        let mut halfway = socks_hello(fwd.port()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let n = tokio::time::timeout(Duration::from_secs(1), halfway.read(&mut buf))
            .await
            .expect("closed by the forwarder")
            .unwrap_or(0);
        assert_eq!(n, 0);

        // A relayed connection outlives the handshake timeout.
        let mut s = socks_hello(fwd.port()).await;
        s.write_all(&[5, 1, 0, 1, 1, 2, 3, 4, 0, 80]).await.unwrap();
        let mut reply = [0u8; 10];
        s.read_exact(&mut reply).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.write_all(b"late").await.unwrap();
        let mut echo = [0u8; 4];
        s.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"late");
    }

    #[tokio::test]
    async fn stop_closes_listener() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let port = fwd.port();
        drop(fwd);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }
}
