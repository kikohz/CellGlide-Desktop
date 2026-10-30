//! The tunnel's fixed parts, identical to the Mac's TunnelConfiguration, and the
//! interface a platform implements to bring the tunnel up.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

pub use cg_ipc::StrictDns;

pub const INTERFACE_NAME: &str = "cellglide0";
pub const ADDRESS_V4: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
pub const ADDRESS_V6: Ipv6Addr = Ipv6Addr::new(0xfd6c, 0x6779, 0x6465, 0, 0, 0, 0, 1);
pub const MTU: u32 = 1500;
pub const IPV4_ROUTES: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];
pub const IPV6_ROUTES: [&str; 2] = ["::/1", "8000::/1"];
/// The local resolver systemd-resolved sends this link's queries to. Loopback is never
/// routed into the tunnel. (If Task 2's probe showed resolved refusing a port here,
/// this is 198.18.0.1:53 instead.)
pub const RESOLVER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1053);
/// With the phone's CellGlide DNS off: plain DNS to public resolvers through the tunnel.
/// IPv6 first, since some cellular networks only reach IPv4 literals through translation.
pub const FALLBACK_DNS: [IpAddr; 4] = [
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
    IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
];

/// Windows: the wintun adapter's fixed GUID, so the NLA profile and the
/// `Tcpip\Parameters\Interfaces\{GUID}` key exist once instead of piling up.
pub const WINDOWS_ADAPTER_GUID: u128 = 0xE490EEF2_2CB0_4A80_B341_BF929C2E56B3;
/// Windows: the wintun tunnel type (the adapter's driver description).
pub const WINDOWS_TUNNEL_TYPE: &str = "CellGlide";
/// Windows: our resolver listens on the tunnel's own address (UDP and TCP).
pub const WINDOWS_RESOLVER: SocketAddr = SocketAddr::new(IpAddr::V4(ADDRESS_V4), 53);

/// The forwarding engine's parameters, shared by hev and any engine that stands in for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineParams {
    pub mtu: u32,
    /// A TCP connection with no traffic either way for this long is closed.
    pub tcp_timeout: Duration,
    /// A UDP session with no traffic for this long is released.
    pub udp_idle: Duration,
}

/// Idle UDP sessions each hold a SOCKS connection on the phone, so they are released
/// after 30 s rather than hev's default 60 s.
pub const ENGINE: EngineParams = EngineParams {
    mtu: MTU,
    tcp_timeout: Duration::from_secs(1800),
    udp_idle: Duration::from_secs(30),
};

/// hev-socks5-tunnel settings, from `ENGINE`.
pub fn hev_config(socks_port: u16) -> String {
    let EngineParams {
        mtu,
        tcp_timeout,
        udp_idle,
    } = ENGINE;
    let (tcp, udp) = (tcp_timeout.as_millis(), udp_idle.as_millis());
    format!(
        "tunnel:\n  mtu: {mtu}\nsocks5:\n  port: {socks_port}\n  address: 127.0.0.1\n  udp: 'udp'\nmisc:\n  log-level: warn\n  log-file: stderr\n  tcp-read-write-timeout: {tcp}\n  udp-read-write-timeout: {udp}\n"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DnsPlan {
    /// Our resolver, which forwards to the phone's DNS-over-HTTPS.
    Resolver(SocketAddr),
    /// Public servers on port 53, reached through the tunnel.
    Servers(Vec<IpAddr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelPlan {
    pub socks_port: u16,
    pub dns: DnsPlan,
    /// Keep private, multicast and link-local ranges on the original network.
    pub bypass: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TunnelError(pub String);

/// When the controller starts our resolver relative to the tunnel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResolverOrder {
    /// Resolver first, then `start` brings up everything (Linux: the resolver is on
    /// loopback and must answer as soon as systemd-resolved is pointed at it).
    #[default]
    BeforeTunnel,
    /// `start` brings up only the interface and its addresses, then the resolver binds
    /// to an address on that interface, then `finish` sets the DNS (Windows: the
    /// resolver listens on 198.18.0.1, which exists only once the interface is up).
    AfterInterface,
}

/// Brings up the interface, routes, DNS and hev in one go, or leaves nothing behind.
/// Called from a blocking thread.
pub trait TunnelBackend: Send + Sync + 'static {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError>;
    fn resolver_order(&self) -> ResolverOrder {
        ResolverOrder::BeforeTunnel
    }
}

/// Something about a running tunnel worth a line in the connection timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelNotice {
    /// No default route outside the tunnel: local networks go through the phone for now.
    BypassUnavailable,
    /// The default route moved and the local-network routes couldn't follow it.
    BypassUpdateFailed,
}

pub trait RunningTunnel: Send + 'static {
    /// Undo everything `start` did. Called from a blocking thread.
    fn stop(self: Box<Self>) -> Result<(), TunnelError>;
    /// Whatever `start` left for once the resolver listens (with `AfterInterface`, the
    /// DNS settings). Called once from a blocking thread, after `start` succeeded; on
    /// failure the controller calls `stop`.
    fn finish(&self, _dns: &DnsPlan) -> Result<(), TunnelError> {
        Ok(())
    }
    /// hev returned on its own (a crash or a fatal error).
    fn has_exited(&self) -> bool;
    /// Whether the tunnel's DNS settings were applied (false when the system refused).
    fn dns_managed(&self) -> bool {
        true
    }
    /// Whether port 53 is blocked off the tunnel interface while it runs (Windows'
    /// strict DNS), and why not if it isn't; `NotApplicable` where the platform has no
    /// such rules (Linux).
    fn strict_dns(&self) -> StrictDns {
        StrictDns::NotApplicable
    }
    /// Notices since the last call, oldest first.
    fn take_notices(&self) -> Vec<TunnelNotice> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hev_config_matches_mac() {
        assert_eq!(
            hev_config(40000),
            "tunnel:\n  mtu: 1500\nsocks5:\n  port: 40000\n  address: 127.0.0.1\n  udp: 'udp'\nmisc:\n  log-level: warn\n  log-file: stderr\n  tcp-read-write-timeout: 1800000\n  udp-read-write-timeout: 30000\n"
        );
    }

    #[test]
    fn constants_match_mac() {
        assert_eq!(INTERFACE_NAME, "cellglide0");
        assert_eq!(ADDRESS_V4.to_string(), "198.18.0.1");
        assert_eq!(ADDRESS_V6.to_string(), "fd6c:6779:6465::1");
        assert_eq!(IPV4_ROUTES, ["0.0.0.0/1", "128.0.0.0/1"]);
        assert_eq!(IPV6_ROUTES, ["::/1", "8000::/1"]);
        assert_eq!(
            FALLBACK_DNS.map(|ip| ip.to_string()),
            [
                "2606:4700:4700::1111",
                "2001:4860:4860::8888",
                "1.1.1.1",
                "8.8.8.8"
            ]
        );
    }

    #[test]
    fn windows_constants() {
        assert_eq!(WINDOWS_TUNNEL_TYPE, "CellGlide");
        assert_eq!(WINDOWS_RESOLVER.to_string(), "198.18.0.1:53");
    }

    #[test]
    fn engine_params_match_hev() {
        assert_eq!(ENGINE.mtu, 1500);
        assert_eq!(ENGINE.tcp_timeout, Duration::from_secs(1800));
        assert_eq!(ENGINE.udp_idle, Duration::from_secs(30));
    }
}
