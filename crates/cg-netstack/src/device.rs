//! Packet devices: `PacketChannel`, an in-memory pair of them, and `Phy`, the smoltcp side
//! of the engine (packets handed in by the poll loop, packets collected for the device).

use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;

/// One end of an in-memory packet link. Each write is one IP packet, each read returns
/// one (truncated if the buffer is smaller, like a datagram socket). Reading returns 0
/// bytes once the other end is gone and its packets are drained.
pub struct PacketChannel {
    tx: PollSender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl PacketChannel {
    /// Two connected ends; each direction holds up to `capacity` packets.
    pub fn pair(capacity: usize) -> (PacketChannel, PacketChannel) {
        let (a_tx, b_rx) = mpsc::channel(capacity.max(1));
        let (b_tx, a_rx) = mpsc::channel(capacity.max(1));
        (
            PacketChannel {
                tx: PollSender::new(a_tx),
                rx: a_rx,
            },
            PacketChannel {
                tx: PollSender::new(b_tx),
                rx: b_rx,
            },
        )
    }

    /// The next packet from the other end; `None` once it is gone.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }

    /// Sends one packet, waiting for room.
    pub async fn send(&mut self, packet: Vec<u8>) -> io::Result<()> {
        poll_fn(|cx| self.tx.poll_reserve(cx))
            .await
            .map_err(|_| closed())?;
        self.tx.send_item(packet).map_err(|_| closed())
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "packet channel closed")
}

impl AsyncRead for PacketChannel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(packet) = ready!(self.rx.poll_recv(cx)) {
            let n = packet.len().min(buf.remaining());
            buf.put_slice(&packet[..n]);
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for PacketChannel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(self.tx.poll_reserve(cx)).map_err(|_| closed())?;
        self.tx.send_item(buf.to_vec()).map_err(|_| closed())?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.tx.close();
        Poll::Ready(Ok(()))
    }
}

/// The smoltcp device: `rx` is filled by the poll loop (TCP packets only), `tx`
/// collects what smoltcp sends, for the loop to write to the real device.
pub(crate) struct Phy {
    pub rx: VecDeque<Vec<u8>>,
    pub tx: Vec<Vec<u8>>,
    mtu: usize,
}

impl Phy {
    pub fn new(mtu: u16) -> Phy {
        Phy {
            rx: VecDeque::new(),
            tx: Vec::new(),
            mtu: mtu.into(),
        }
    }
}

pub(crate) struct RxToken(Vec<u8>);
pub(crate) struct TxToken<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push(packet);
        result
    }
}

impl phy::Device for Phy {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(RxToken, TxToken<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((RxToken(packet), TxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _now: Instant) -> Option<TxToken<'_>> {
        Some(TxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn each_write_is_one_packet_and_close_reads_as_eof() {
        let (mut a, mut b) = PacketChannel::pair(4);
        a.write_all(b"one").await.unwrap();
        a.send(b"two".to_vec()).await.unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(b.read(&mut buf).await.unwrap(), 3);
        assert_eq!(&buf[..3], b"one");
        assert_eq!(b.recv().await.unwrap(), b"two");
        // A short buffer truncates the packet; the rest is not delivered.
        a.send(b"truncated".to_vec()).await.unwrap();
        let mut small = [0u8; 4];
        assert_eq!(b.read(&mut small).await.unwrap(), 4);
        a.send(b"next".to_vec()).await.unwrap();
        assert_eq!(b.recv().await.unwrap(), b"next");
        drop(a);
        assert_eq!(b.read(&mut buf).await.unwrap(), 0);
        assert!(b.send(b"x".to_vec()).await.is_err());
    }
}
