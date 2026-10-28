//! Per-link DNS through systemd-resolved. Only our link is ever touched; when the
//! link disappears resolved forgets its settings, so a crash leaves nothing behind.

use cg_core::tunnel::{DnsPlan, TunnelError};
use std::net::IpAddr;

// resolved runs on Linux; these are Linux's address family numbers.
const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;

/// The `a(iayqs)` argument of SetLinkDNSEx: family, address bytes, port (0 = 53), server name.
pub fn dns_entries(plan: &DnsPlan) -> Vec<(i32, Vec<u8>, u16, String)> {
    let entry = |ip: IpAddr, port: u16| match ip {
        IpAddr::V4(v4) => (AF_INET, v4.octets().to_vec(), port, String::new()),
        IpAddr::V6(v6) => (AF_INET6, v6.octets().to_vec(), port, String::new()),
    };
    match plan {
        DnsPlan::Resolver(addr) => vec![entry(addr.ip(), addr.port())],
        DnsPlan::Servers(ips) => ips.iter().map(|ip| entry(*ip, 0)).collect(),
    }
}

#[derive(Debug)]
pub enum ApplyError {
    /// resolved (polkit) refused the caller; the tunnel can run without our DNS.
    NotAuthorized(String),
    /// resolved isn't running (or not installed), or the system bus can't be reached; the
    /// tunnel can run without our DNS, and retrying wouldn't change that.
    Unavailable(String),
    Failed(TunnelError),
}

/// Whether a D-Bus error name means polkit refused the caller. Only the name is matched:
/// the message text differs between distributions.
pub fn is_denied(name: &str) -> bool {
    name == "org.freedesktop.DBus.Error.AccessDenied"
        || name == "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired"
}

/// Whether a D-Bus error name means nobody owns resolved's bus name.
pub fn is_missing(name: &str) -> bool {
    name == "org.freedesktop.DBus.Error.ServiceUnknown"
        || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
}

/// What a D-Bus error from resolved means for the tunnel, by error name only.
pub fn classify_name(name: &str, text: String) -> ApplyError {
    if is_denied(name) {
        ApplyError::NotAuthorized(text)
    } else if is_missing(name) {
        ApplyError::Unavailable(text)
    } else {
        ApplyError::Failed(TunnelError(format!(
            "systemd-resolved refused the DNS settings: {text}"
        )))
    }
}

/// No system bus at all (containers, minimal systems): resolved can't be reached either.
pub fn no_system_bus(text: String) -> ApplyError {
    ApplyError::Unavailable(format!("system D-Bus unavailable: {text}"))
}

#[cfg(target_os = "linux")]
mod bus {
    use super::{ApplyError, classify_name, dns_entries, no_system_bus};
    use cg_core::tunnel::{DnsPlan, TunnelError};

    #[zbus::proxy(
        interface = "org.freedesktop.resolve1.Manager",
        default_service = "org.freedesktop.resolve1",
        default_path = "/org/freedesktop/resolve1"
    )]
    trait Resolve1Manager {
        #[zbus(name = "SetLinkDNSEx")]
        fn set_link_dns_ex(
            &self,
            ifindex: i32,
            addresses: &[(i32, Vec<u8>, u16, String)],
        ) -> zbus::Result<()>;
        fn set_link_domains(&self, ifindex: i32, domains: &[(&str, bool)]) -> zbus::Result<()>;
        fn set_link_default_route(&self, ifindex: i32, enable: bool) -> zbus::Result<()>;
        fn revert_link(&self, ifindex: i32) -> zbus::Result<()>;
    }

    fn manager() -> Result<Resolve1ManagerProxyBlocking<'static>, ApplyError> {
        let connection =
            zbus::blocking::Connection::system().map_err(|e| no_system_bus(e.to_string()))?;
        Resolve1ManagerProxyBlocking::new(&connection).map_err(classify)
    }

    fn classify(error: zbus::Error) -> ApplyError {
        let text = error.to_string();
        match &error {
            zbus::Error::MethodError(name, _, _) => classify_name(name.as_str(), text),
            _ => ApplyError::Failed(TunnelError(format!(
                "systemd-resolved refused the DNS settings: {text}"
            ))),
        }
    }

    /// All lookups go to our link: its DNS servers, the routing-only domain "~." and default route.
    pub fn apply(ifindex: i32, plan: &DnsPlan) -> Result<(), ApplyError> {
        let manager = manager()?;
        manager
            .set_link_dns_ex(ifindex, &dns_entries(plan))
            .map_err(classify)?;
        manager
            .set_link_domains(ifindex, &[(".", true)])
            .map_err(classify)?;
        manager
            .set_link_default_route(ifindex, true)
            .map_err(classify)?;
        Ok(())
    }

    pub fn revert(ifindex: i32) -> Result<(), TunnelError> {
        manager()
            .map_err(|e| TunnelError(format!("reverting DNS failed: {e:?}")))?
            .revert_link(ifindex)
            .map_err(|e| TunnelError(format!("reverting DNS failed: {e}")))
    }
}

#[cfg(target_os = "linux")]
pub use bus::{apply, revert};

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::tunnel::{DnsPlan, FALLBACK_DNS};

    #[test]
    fn denial_is_recognised_by_error_name() {
        assert!(is_denied("org.freedesktop.DBus.Error.AccessDenied"));
        assert!(is_denied(
            "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired"
        ));
        assert!(!is_denied("org.freedesktop.DBus.Error.ServiceUnknown"));
        assert!(!is_denied("Access denied"));
    }

    #[test]
    fn missing_resolved_is_unavailable_not_a_failure() {
        // resolved not installed or not running: the tunnel keeps working without our DNS.
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(
                matches!(classify_name(name, "x".into()), ApplyError::Unavailable(_)),
                "{name}"
            );
        }
        assert!(matches!(
            classify_name("org.freedesktop.DBus.Error.AccessDenied", "x".into()),
            ApplyError::NotAuthorized(_)
        ));
        assert!(matches!(
            classify_name(
                "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired",
                "x".into()
            ),
            ApplyError::NotAuthorized(_)
        ));
        // Anything else is a real failure; the message text is never parsed.
        assert!(matches!(
            classify_name(
                "org.freedesktop.DBus.Error.InvalidArgs",
                "ServiceUnknown".into()
            ),
            ApplyError::Failed(_)
        ));
        assert!(matches!(
            no_system_bus("No such file or directory".into()),
            ApplyError::Unavailable(_)
        ));
    }

    #[test]
    fn resolver_plan_carries_port() {
        let entries = dns_entries(&DnsPlan::Resolver("127.0.0.1:1053".parse().unwrap()));
        assert_eq!(entries, vec![(2, vec![127, 0, 0, 1], 1053, String::new())]);
    }

    #[test]
    fn public_servers_use_default_port() {
        let entries = dns_entries(&DnsPlan::Servers(FALLBACK_DNS.to_vec()));
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].0, 10, "AF_INET6 on Linux");
        assert_eq!(entries[0].1.len(), 16);
        assert_eq!(entries[2], (2, vec![1, 1, 1, 1], 0, String::new()));
    }
}
