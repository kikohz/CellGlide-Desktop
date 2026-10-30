//! UDP: one SOCKS5 UDP ASSOCIATE per (client, destination) flow, as hev does with
//! `udp: 'udp'`. These packets never enter smoltcp: the poll loop hands each datagram to
//! its session task, which answers by building IP/UDP packets itself.

use crate::socks;
use cg_protocol::socks::{parse_udp_datagram, udp_datagram};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    IpAddress, IpProtocol, Ipv4Packet, Ipv4Repr, Ipv6Packet, Ipv6Repr, UdpPacket, UdpRepr,
};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Datagrams waiting for their session (during the SOCKS handshake, say); more are
/// dropped, as a full socket buffer would.
pub(crate) const QUEUE: usize = 64;
/// Hop limit of the packets we build (what Windows uses for its own).
const HOP_LIMIT: u8 = 128;
const UDP_HEADER: usize = 8;

pub(crate) struct Session {
    pub socks: SocketAddr,
    pub client: SocketAddr,
    pub target: SocketAddr,
    pub idle: Duration,
    pub mtu: u16,
}

impl Session {
    /// Runs until the association fails, its control connection closes, nothing has
    /// passed either way for `idle`, the loop drops `datagrams`, or `cancel`.
    pub async fn run(
        self,
        mut datagrams: mpsc::Receiver<Vec<u8>>,
        out: mpsc::Sender<Vec<u8>>,
        cancel: CancellationToken,
    ) {
        let associated = tokio::select! {
            _ = cancel.cancelled() => return,
            associated = socks::associate(self.socks) => associated,
        };
        let (mut control, relay) = match associated {
            Ok(associated) => associated,
            Err(error) => {
                tracing::debug!(client = %self.client, target = %self.target, %error, "udp associate failed");
                return;
            }
        };
        let udp = match bind_for(self.socks, relay).await {
            Ok(udp) => udp,
            Err(error) => {
                tracing::debug!(%error, "udp relay socket");
                return;
            }
        };

        let idle = tokio::time::sleep(self.idle);
        tokio::pin!(idle);
        let mut buf = vec![0u8; 65535];
        let mut stray = [0u8; 64];
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = &mut idle => {
                    tracing::debug!(client = %self.client, target = %self.target, "udp session idle");
                    return;
                }
                datagram = datagrams.recv() => {
                    let Some(datagram) = datagram else { return };
                    // A failed send (relay gone for a moment) loses this datagram only.
                    let _ = udp.send(&udp_datagram(self.target, &datagram)).await;
                    idle.as_mut().reset(tokio::time::Instant::now() + self.idle);
                }
                received = udp.recv(&mut buf) => {
                    let n = match received {
                        Ok(n) => n,
                        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => continue,
                        Err(_) => return,
                    };
                    idle.as_mut().reset(tokio::time::Instant::now() + self.idle);
                    let Ok((from, payload)) = parse_udp_datagram(&buf[..n]) else { continue };
                    if let Some(packet) = build_packet(from, self.client, payload, self.mtu)
                        && out.send(packet).await.is_err()
                    {
                        return;
                    }
                }
                // RFC 1928: the association ends with its control connection; stray
                // bytes on it are not an end.
                read = control.read(&mut stray) => {
                    if !matches!(read, Ok(n) if n > 0) {
                        return;
                    }
                }
            }
        }
    }
}

/// A local socket connected to the relay, from the address family and (for a loopback
/// relay) the loopback address the forwarder expects its client on.
async fn bind_for(socks: SocketAddr, relay: SocketAddr) -> io::Result<UdpSocket> {
    let local: IpAddr = match (socks.ip(), relay.ip()) {
        (ip, _) if ip.is_loopback() && ip.is_ipv4() == relay.is_ipv4() => ip,
        (_, IpAddr::V4(_)) => IpAddr::from([0u8; 4]),
        (_, IpAddr::V6(_)) => IpAddr::from([0u8; 16]),
    };
    let udp = UdpSocket::bind((local, 0)).await?;
    ignore_icmp_reset(&udp)?;
    udp.connect(relay).await?;
    Ok(udp)
}

/// An IP/UDP packet from `from` to `to`; `None` when the families differ or it would
/// not fit in `mtu` (the client would have to reassemble fragments we do not make).
pub(crate) fn build_packet(
    from: SocketAddr,
    to: SocketAddr,
    payload: &[u8],
    mtu: u16,
) -> Option<Vec<u8>> {
    let caps = ChecksumCapabilities::default();
    let udp = UdpRepr {
        src_port: from.port(),
        dst_port: to.port(),
    };
    let udp_len = UDP_HEADER + payload.len();
    let (src, dst) = (IpAddress::from(from.ip()), IpAddress::from(to.ip()));
    let mut packet = match (from.ip(), to.ip()) {
        (IpAddr::V4(src_addr), IpAddr::V4(dst_addr)) => {
            let ip = Ipv4Repr {
                src_addr,
                dst_addr,
                next_header: IpProtocol::Udp,
                payload_len: udp_len,
                hop_limit: HOP_LIMIT,
            };
            let mut packet = vec![0u8; ip.buffer_len() + udp_len];
            let mut view = Ipv4Packet::new_unchecked(&mut packet[..]);
            ip.emit(&mut view, &caps);
            let mut datagram = UdpPacket::new_unchecked(view.payload_mut());
            udp.emit(
                &mut datagram,
                &src,
                &dst,
                payload.len(),
                |b| b.copy_from_slice(payload),
                &caps,
            );
            packet
        }
        (IpAddr::V6(src_addr), IpAddr::V6(dst_addr)) => {
            let ip = Ipv6Repr {
                src_addr,
                dst_addr,
                next_header: IpProtocol::Udp,
                payload_len: udp_len,
                hop_limit: HOP_LIMIT,
            };
            let mut packet = vec![0u8; ip.buffer_len() + udp_len];
            let mut view = Ipv6Packet::new_unchecked(&mut packet[..]);
            ip.emit(&mut view);
            let mut datagram = UdpPacket::new_unchecked(view.payload_mut());
            udp.emit(
                &mut datagram,
                &src,
                &dst,
                payload.len(),
                |b| b.copy_from_slice(payload),
                &caps,
            );
            packet
        }
        _ => return None,
    };
    if packet.len() > usize::from(mtu) {
        tracing::debug!(len = packet.len(), "udp reply larger than the mtu, dropped");
        return None;
    }
    packet.shrink_to_fit();
    Some(packet)
}

/// Stops ICMP port unreachable from surfacing as `ConnectionReset` on receive
/// (Windows `SIO_UDP_CONNRESET = FALSE`). The twin of `cg_core::udp::ignore_icmp_reset`,
/// repeated here because cg-netstack does not depend on cg-core.
#[cfg(windows)]
pub(crate) fn ignore_icmp_reset(socket: &UdpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET, WSAIoctl};

    let off: u32 = 0; // BOOL FALSE
    let mut returned = 0u32;
    // SAFETY: the socket stays open for the call (we hold a reference); the input is a
    // live 4-byte BOOL, there is no output buffer, and no OVERLAPPED, so the call is
    // synchronous and keeps no pointer.
    let rc = unsafe {
        WSAIoctl(
            SOCKET(socket.as_raw_socket() as usize),
            SIO_UDP_CONNRESET,
            Some(std::ptr::from_ref(&off).cast()),
            std::mem::size_of_val(&off) as u32,
            None,
            0,
            &mut returned,
            None,
            None,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Nothing to do elsewhere: Linux reports no such error on these sockets.
#[cfg(not(windows))]
pub(crate) fn ignore_icmp_reset(_socket: &UdpSocket) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_v4(packet: &[u8]) -> (SocketAddr, SocketAddr, Vec<u8>) {
        let caps = ChecksumCapabilities::default();
        let ip = Ipv4Packet::new_checked(packet).unwrap();
        let repr = Ipv4Repr::parse(&ip, &caps).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        let (src, dst) = (repr.src_addr.into(), repr.dst_addr.into());
        UdpRepr::parse(&udp, &src, &dst, &caps).expect("valid checksum");
        (
            SocketAddr::new(IpAddr::V4(repr.src_addr), udp.src_port()),
            SocketAddr::new(IpAddr::V4(repr.dst_addr), udp.dst_port()),
            udp.payload().to_vec(),
        )
    }

    #[test]
    fn built_packets_parse_back_with_valid_checksums() {
        let (from, to): (SocketAddr, SocketAddr) = (
            "1.1.1.1:53".parse().unwrap(),
            "10.0.0.2:5353".parse().unwrap(),
        );
        let packet = build_packet(from, to, b"answer", 1500).unwrap();
        assert_eq!(packet.len(), 20 + 8 + 6);
        assert_eq!(parse_v4(&packet), (from, to, b"answer".to_vec()));

        let (from6, to6): (SocketAddr, SocketAddr) = (
            "[2606:4700::1111]:53".parse().unwrap(),
            "[fd00::2]:5353".parse().unwrap(),
        );
        let packet = build_packet(from6, to6, b"", 1500).unwrap();
        assert_eq!(packet.len(), 40 + 8);
        let ip = Ipv6Packet::new_checked(&packet[..]).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        let (src, dst) = (ip.src_addr().into(), ip.dst_addr().into());
        UdpRepr::parse(&udp, &src, &dst, &ChecksumCapabilities::default()).unwrap();
        assert_eq!(udp.dst_port(), 5353);
    }

    #[test]
    fn mismatched_families_and_oversize_are_dropped() {
        let v4: SocketAddr = "1.1.1.1:53".parse().unwrap();
        let v6: SocketAddr = "[fd00::2]:5353".parse().unwrap();
        assert!(build_packet(v4, v6, b"x", 1500).is_none());
        let to: SocketAddr = "10.0.0.2:5353".parse().unwrap();
        assert!(build_packet(v4, to, &[0; 1472], 1500).is_some());
        assert!(build_packet(v4, to, &[0; 1473], 1500).is_none());
    }
}
