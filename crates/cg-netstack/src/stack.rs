//! The poll loop: one task owns the smoltcp interface and its sockets. It is woken by
//! packets from the device, by flow tasks (`kick`) and by smoltcp's own timers.
//!
//! TCP goes through smoltcp with AnyIP: on a SYN for a connection we do not know, a
//! socket starts listening on that exact destination, so smoltcp completes the
//! handshake as if it were the destination. UDP bypasses smoltcp (see `udp`). Anything
//! else is dropped, so nothing on the tunnel answers pings it cannot forward.

use crate::device::Phy;
use crate::tcp::{self, Flow, Session};
use crate::{Config, udp};
use smoltcp::iface::{Config as IfaceConfig, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::tcp::Socket as TcpSocket;
use smoltcp::wire::{
    HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, IpProtocol, Ipv4Address,
    Ipv4Packet, Ipv6Address, Ipv6Packet, TcpPacket, UdpPacket, UdpRepr,
};
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The interface's own addresses (documentation ranges). With AnyIP they only anchor the
/// default routes smoltcp needs to answer any client; they never appear on the wire.
const OWN_V4: Ipv4Address = Ipv4Address::new(192, 0, 2, 1);
const OWN_V6: Ipv6Address = Ipv6Address::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
/// Passes of poll-and-pump before the loop flushes and waits again.
const MAX_PASSES: usize = 64;

/// (client, destination) as the client sees the connection.
type Key = (SocketAddr, SocketAddr);

enum Inbound<'a> {
    Tcp { key: Key, syn: bool },
    Udp { key: Key, payload: &'a [u8] },
}

/// Whether traffic to `dst` can mean anything on the phone's network. Multicast,
/// broadcast, link-local, loopback and unspecified destinations belong to the local link
/// (mDNS, SSDP, DHCP...) and never get a SOCKS session. Subnet-directed broadcast cannot
/// be recognised without the client's prefix; the routes only send 0/1 and 128/1 here.
fn forwardable(dst: IpAddr) -> bool {
    match dst {
        IpAddr::V4(ip) => {
            !(ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_link_local()
                || ip.is_loopback()
                || ip.is_unspecified())
        }
        IpAddr::V6(ip) => {
            !(ip.is_multicast()
                || ip.is_unicast_link_local()
                || ip.is_loopback()
                || ip.is_unspecified())
        }
    }
}

/// What a packet from the device is, if we carry it at all.
fn classify(packet: &[u8]) -> Option<Inbound<'_>> {
    let caps = ChecksumCapabilities::default();
    let (src, dst, protocol, payload) = match packet.first()? >> 4 {
        4 => {
            let ip = Ipv4Packet::new_checked(packet).ok()?;
            // Fragments are not reassembled (clients use PMTU discovery for TCP; DNS
            // answers over 1472 bytes are rare).
            if !ip.verify_checksum() || ip.more_frags() || ip.frag_offset() != 0 {
                return None;
            }
            let (src, dst) = (IpAddr::V4(ip.src_addr()), IpAddr::V4(ip.dst_addr()));
            (src, dst, ip.next_header(), ip.payload())
        }
        6 => {
            let ip = Ipv6Packet::new_checked(packet).ok()?;
            let (src, dst) = (IpAddr::V6(ip.src_addr()), IpAddr::V6(ip.dst_addr()));
            (src, dst, ip.next_header(), ip.payload())
        }
        _ => return None,
    };
    if !forwardable(dst) {
        return None;
    }
    match protocol {
        IpProtocol::Tcp => {
            let tcp = TcpPacket::new_checked(payload).ok()?;
            let key = (
                SocketAddr::new(src, tcp.src_port()),
                SocketAddr::new(dst, tcp.dst_port()),
            );
            Some(Inbound::Tcp {
                key,
                syn: tcp.syn() && !tcp.ack(),
            })
        }
        IpProtocol::Udp => {
            let datagram = UdpPacket::new_checked(payload).ok()?;
            UdpRepr::parse(&datagram, &src.into(), &dst.into(), &caps).ok()?;
            let key = (
                SocketAddr::new(src, datagram.src_port()),
                SocketAddr::new(dst, datagram.dst_port()),
            );
            Some(Inbound::Udp {
                key,
                payload: datagram.payload(),
            })
        }
        _ => None,
    }
}

fn socket_addr(endpoint: IpEndpoint) -> SocketAddr {
    SocketAddr::new(endpoint.addr.into(), endpoint.port)
}

pub(crate) struct Stack {
    config: Config,
    iface: Interface,
    sockets: SocketSet<'static>,
    phy: Phy,
    origin: Instant,
    /// Connections handed to a flow task.
    sessions: HashMap<SocketHandle, Session>,
    keys: HashMap<Key, SocketHandle>,
    /// Sockets put in LISTEN for SYNs not yet polled, with the SYN's key.
    listeners: Vec<(SocketHandle, Key)>,
    udp: HashMap<Key, mpsc::Sender<Vec<u8>>>,
    pub kick: Arc<Notify>,
    out: mpsc::Sender<Vec<u8>>,
    cancel: CancellationToken,
}

impl Stack {
    pub fn new(config: Config, out: mpsc::Sender<Vec<u8>>, cancel: CancellationToken) -> Stack {
        let mut phy = Phy::new(config.mtu);
        let mut iface_config = IfaceConfig::new(HardwareAddress::Ip);
        // Seeds smoltcp's initial sequence numbers; need not be cryptographic.
        iface_config.random_seed = RandomState::new().hash_one(std::process::id());
        let origin = Instant::now();
        let mut iface = Interface::new(iface_config, &mut phy, smoltcp::time::Instant::ZERO);
        iface.set_any_ip(true);
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(OWN_V4), 32));
            let _ = addrs.push(IpCidr::new(IpAddress::Ipv6(OWN_V6), 128));
        });
        let routes = iface.routes_mut();
        let _ = routes.add_default_ipv4_route(OWN_V4);
        let _ = routes.add_default_ipv6_route(OWN_V6);
        Stack {
            config,
            iface,
            sockets: SocketSet::new(Vec::new()),
            phy,
            origin,
            sessions: HashMap::new(),
            keys: HashMap::new(),
            listeners: Vec::new(),
            udp: HashMap::new(),
            kick: Arc::new(Notify::new()),
            out,
            cancel,
        }
    }

    fn now(&self) -> smoltcp::time::Instant {
        let micros = (Instant::now() - self.origin).as_micros();
        smoltcp::time::Instant::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
    }

    /// How long the loop may sleep before smoltcp needs polling; `None` for no timer.
    pub fn delay(&mut self) -> Option<std::time::Duration> {
        let now = self.now();
        self.iface
            .poll_delay(now, &self.sockets)
            .map(|d| std::time::Duration::from_micros(d.total_micros()))
    }

    /// Takes one packet from the device.
    pub fn input(&mut self, packet: Vec<u8>) {
        match classify(&packet) {
            Some(Inbound::Tcp { key, syn }) => {
                if syn {
                    self.listen_for(key);
                }
                self.phy.rx.push_back(packet);
            }
            Some(Inbound::Udp { key, payload }) => {
                let payload = payload.to_vec();
                self.udp_datagram(key, payload);
            }
            None => {}
        }
    }

    /// Makes sure a socket will take the SYN for `key`.
    fn listen_for(&mut self, key: Key) {
        if let Some(&handle) = self.keys.get(&key) {
            let socket = self.sockets.get::<TcpSocket>(handle);
            if socket.state() != smoltcp::socket::tcp::State::TimeWait {
                return; // A retransmitted SYN: the connection's own socket answers it.
            }
            // The client reuses the 4-tuple of a connection that is only lingering.
            self.remove(handle);
        }
        if self.listeners.iter().any(|(_, pending)| *pending == key) {
            return;
        }
        if self.sessions.len() + self.listeners.len() >= tcp::MAX_SESSIONS {
            tracing::debug!(client = %key.0, target = %key.1, "too many tcp sessions");
            return; // No socket takes the SYN: smoltcp answers with RST.
        }
        let mut socket = tcp::new_socket(self.config.tcp_timeout);
        let endpoint = IpListenEndpoint {
            addr: Some(key.1.ip().into()),
            port: key.1.port(),
        };
        if socket.listen(endpoint).is_err() {
            return; // Port 0: smoltcp answers with RST.
        }
        let handle = self.sockets.add(socket);
        self.listeners.push((handle, key));
    }

    fn udp_datagram(&mut self, key: Key, mut payload: Vec<u8>) {
        if let Some(session) = self.udp.get(&key) {
            match session.try_send(payload) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Full(_)) => return,
                Err(mpsc::error::TrySendError::Closed(back)) => payload = back,
            }
            self.udp.remove(&key);
        }
        self.udp.retain(|_, session| !session.is_closed());
        let (tx, rx) = mpsc::channel(udp::QUEUE);
        let _ = tx.try_send(payload);
        let session = udp::Session {
            socks: self.config.socks,
            client: key.0,
            target: key.1,
            idle: self.config.udp_idle,
            mtu: self.config.mtu,
        };
        tokio::spawn(session.run(rx, self.out.clone(), self.cancel.clone()));
        self.udp.insert(key, tx);
    }

    /// Polls smoltcp and moves data until nothing changes (or `MAX_PASSES`).
    pub fn process(&mut self) {
        for _ in 0..MAX_PASSES {
            let now = self.now();
            let polled = self.iface.poll(now, &mut self.phy, &mut self.sockets);
            let mut progress = matches!(polled, PollResult::SocketStateChanged);
            self.promote_listeners();
            progress |= self.pump();
            if !progress && self.phy.rx.is_empty() {
                break;
            }
        }
    }

    /// Listeners that took a SYN become sessions (keyed by the connection they actually
    /// took); the rest are removed.
    fn promote_listeners(&mut self) {
        for (handle, _) in std::mem::take(&mut self.listeners) {
            let socket = self.sockets.get::<TcpSocket>(handle);
            let (Some(remote), Some(local)) = (socket.remote_endpoint(), socket.local_endpoint())
            else {
                self.sockets.remove(handle);
                continue;
            };
            let (client, target) = (socket_addr(remote), socket_addr(local));
            let flow = Flow {
                socks: self.config.socks,
                target,
                idle: self.config.tcp_timeout,
                kick: self.kick.clone(),
                cancel: self.cancel.clone(),
            };
            self.sessions.insert(handle, Session::new(client, flow));
            self.keys.insert((client, target), handle);
        }
    }

    fn pump(&mut self) -> bool {
        let mut progress = false;
        let mut finished = Vec::new();
        for (&handle, session) in &mut self.sessions {
            let socket = self.sockets.get_mut::<TcpSocket>(handle);
            progress |= session.pump(socket);
            if session.finished(socket) {
                finished.push(handle);
            }
        }
        for handle in finished {
            self.remove(handle);
        }
        progress
    }

    fn remove(&mut self, handle: SocketHandle) {
        if let Some(session) = self.sessions.remove(&handle) {
            let key = (session.client, session.target);
            if self.keys.get(&key) == Some(&handle) {
                self.keys.remove(&key);
            }
        }
        self.sockets.remove(handle);
    }

    /// Packets smoltcp produced, for the device.
    pub fn take_output(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.phy.tx)
    }

    /// Resets every connection (the RSTs are in `take_output` afterwards).
    pub fn abort_all(&mut self) {
        for &handle in self.sessions.keys() {
            self.sockets.get_mut::<TcpSocket>(handle).abort();
        }
        let now = self.now();
        self.iface.poll(now, &mut self.phy, &mut self.sockets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_scope_destinations_are_not_forwarded() {
        for local in [
            "224.0.0.251",
            "239.255.255.250",
            "255.255.255.255",
            "169.254.1.1",
            "127.0.0.1",
            "0.0.0.0",
            "ff02::fb",
            "ff05::c",
            "fe80::1",
            "::1",
            "::",
        ] {
            assert!(!forwardable(local.parse().unwrap()), "{local}");
        }
        for remote in [
            "1.1.1.1",
            "10.0.0.1",
            "198.18.0.1",
            "2606:4700::1111",
            "fd00::1",
        ] {
            assert!(forwardable(remote.parse().unwrap()), "{remote}");
        }
    }

    #[test]
    fn classify_finds_syns_and_udp_payloads() {
        let syn = [
            0x45, 0, 0, 40, 0, 0, 0x40, 0, 64, 6, 0, 0, 10, 0, 0, 2, 1, 2, 3, 4, // IPv4
            0x9c, 0x40, 0x01, 0xbb, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0,
            0, // TCP SYN 40000 -> 443
        ];
        let mut syn = syn.to_vec();
        let mut ip = Ipv4Packet::new_unchecked(&mut syn[..]);
        ip.fill_checksum();
        let Some(Inbound::Tcp { key, syn: true }) = classify(&syn) else {
            panic!("not a SYN");
        };
        assert_eq!(
            key,
            (
                "10.0.0.2:40000".parse().unwrap(),
                "1.2.3.4:443".parse().unwrap()
            )
        );
        // A bad IP checksum is dropped.
        syn[10] ^= 0xff;
        assert!(classify(&syn).is_none());

        let from: SocketAddr = "10.0.0.2:5353".parse().unwrap();
        let to: SocketAddr = "1.1.1.1:53".parse().unwrap();
        let packet = udp::build_packet(from, to, b"query", 1500).unwrap();
        let Some(Inbound::Udp { key, payload }) = classify(&packet) else {
            panic!("not udp");
        };
        assert_eq!((key, payload), ((from, to), &b"query"[..]));
        assert!(classify(&[]).is_none());
        assert!(classify(&[0x10, 0, 0]).is_none());
    }
}
