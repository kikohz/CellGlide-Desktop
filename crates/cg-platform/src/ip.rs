//! Interface addresses and routes through iproute2's `ip`, as the Mac helper uses
//! `ifconfig` and `route`. Everything here belongs to our interface only.

use cg_core::tunnel::{ADDRESS_V4, ADDRESS_V6, IPV4_ROUTES, IPV6_ROUTES, MTU, TunnelError};
use std::process::Command;

fn args(text: &str) -> Vec<String> {
    text.split(' ').map(String::from).collect()
}

pub fn link_commands(name: &str) -> Vec<Vec<String>> {
    vec![
        args(&format!("link set dev {name} mtu {MTU} multicast off up")),
        args(&format!("addr add {ADDRESS_V4}/32 dev {name}")),
    ]
}

/// Best effort: hosts with IPv6 disabled refuse these, and the tunnel still works for IPv4.
pub fn ipv6_commands(name: &str) -> Vec<Vec<String>> {
    let mut commands = vec![args(&format!("-6 addr add {ADDRESS_V6}/128 dev {name}"))];
    commands.extend(
        IPV6_ROUTES
            .iter()
            .map(|r| args(&format!("-6 route add {r} dev {name}"))),
    );
    commands
}

pub fn route_commands(name: &str) -> Vec<Vec<String>> {
    IPV4_ROUTES
        .iter()
        .map(|r| args(&format!("route add {r} dev {name}")))
        .collect()
}

pub fn run(arguments: &[String]) -> Result<(), TunnelError> {
    let output = Command::new("ip")
        .args(arguments)
        .output()
        .map_err(|e| TunnelError(format!("can't run ip: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(TunnelError(format!(
        "ip {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(commands: Vec<Vec<String>>) -> Vec<String> {
        commands.into_iter().map(|c| c.join(" ")).collect()
    }

    #[test]
    fn commands_match_mac_tunnel() {
        assert_eq!(
            joined(link_commands("cellglide0")),
            [
                "link set dev cellglide0 mtu 1500 multicast off up",
                "addr add 198.18.0.1/32 dev cellglide0"
            ]
        );
        assert_eq!(
            joined(ipv6_commands("cellglide0")),
            [
                "-6 addr add fd6c:6779:6465::1/128 dev cellglide0",
                "-6 route add ::/1 dev cellglide0",
                "-6 route add 8000::/1 dev cellglide0"
            ]
        );
        assert_eq!(
            joined(route_commands("cellglide0")),
            [
                "route add 0.0.0.0/1 dev cellglide0",
                "route add 128.0.0.0/1 dev cellglide0"
            ]
        );
    }
}
