//! Needs an elevated shell and wintun.dll. Take network snapshots before and after:
//!   scripts\fetch-wintun.ps1
//!   $env:CELLGLIDE_WINTUN_DLL = "$PWD\target\debug\wintun.dll"
//!   cargo test -p cg-platform --test windows_privileged -- --ignored --test-threads=1
#![cfg(windows)]

use cg_core::tunnel::{
    ADDRESS_V4, ADDRESS_V6, FALLBACK_DNS, INTERFACE_NAME, IPV6_ROUTES, WINDOWS_ADAPTER_GUID,
    WINDOWS_TUNNEL_TYPE,
};
use cg_platform::windows::dns;
use cg_platform::windows::ip::{self, Family};
use cg_platform::windows::wintun::{Adapter, SESSION_CAPACITY, Wintun};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GONE_WITHIN: Duration = Duration::from_secs(5);

fn dll() -> Arc<Wintun> {
    Wintun::load().expect(r"set CELLGLIDE_WINTUN_DLL to target\debug\wintun.dll")
}

/// Whether Windows lists the adapter, hidden ones included.
fn adapter_listed() -> bool {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "if (Get-NetAdapter -IncludeHidden -Name {INTERFACE_NAME} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
        ))
        .status()
        .unwrap()
        .success()
}

/// Polls until `done` holds, up to GONE_WITHIN; returns how long it took.
fn wait_for(mut done: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < GONE_WITHIN {
        if done() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Runs a PowerShell command and returns its trimmed standard output, lines joined by `\n`.
fn ps(command: &str) -> String {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Our adapter's DNS servers for one family, joined with commas.
fn dns_servers(family: &str) -> String {
    ps(&format!(
        "(Get-DnsClientServerAddress -InterfaceAlias {INTERFACE_NAME} -AddressFamily {family}).ServerAddresses -join ','"
    ))
}

/// The manually added addresses on our adapter, as sorted `address/prefix` lines.
fn manual_addresses() -> String {
    ps(&format!(
        "$addresses = Get-NetIPAddress -InterfaceAlias {INTERFACE_NAME} -ErrorAction SilentlyContinue; $addresses | Where-Object PrefixOrigin -eq Manual | Sort-Object IPAddress | ForEach-Object {{ \"$($_.IPAddress)/$($_.PrefixLength)\" }}"
    ))
}

/// With no IPv6 servers set, Windows lists either nothing or its fec0:0:0:ffff::1-3
/// placeholders, depending on the interface's history.
fn assert_no_ipv6_servers() {
    let listed = dns_servers("IPv6");
    assert!(
        listed.is_empty() || listed == "fec0:0:0:ffff::1,fec0:0:0:ffff::2,fec0:0:0:ffff::3",
        "IPv6 servers: {listed}"
    );
}

/// Our adapter's `Tcpip` or `Tcpip6` interface key, as a PowerShell path.
fn interface_key(service: &str) -> String {
    format!(
        r"HKLM:\SYSTEM\CurrentControlSet\Services\{service}\Parameters\Interfaces\{{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}}"
    )
}

fn create(dll: &Arc<Wintun>) -> Adapter {
    assert!(
        Adapter::open(dll, INTERFACE_NAME).unwrap().is_none(),
        "{INTERFACE_NAME} already exists; remove the leftover first"
    );
    Adapter::create(
        dll,
        INTERFACE_NAME,
        WINDOWS_TUNNEL_TYPE,
        WINDOWS_ADAPTER_GUID,
    )
    .unwrap()
}

#[test]
#[ignore]
fn adapter_appears_and_disappears() {
    let dll = dll();
    let adapter = create(&dll);
    assert_ne!(adapter.luid(), 0);
    assert!(adapter_listed(), "{INTERFACE_NAME} not listed after create");
    drop(adapter);
    let took = wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
    eprintln!("adapter gone {took:?} after drop");
}

#[test]
#[ignore]
fn open_finds_an_existing_adapter() {
    let dll = dll();
    let created = create(&dll);
    let opened = Adapter::open(&dll, INTERFACE_NAME)
        .unwrap()
        .expect("open found nothing");
    assert_eq!(opened.luid(), created.luid());
    drop(opened);
    drop(created);
    // cleanup_leftovers relies on open returning None once the adapter is gone.
    let took = wait_for(|| Adapter::open(&dll, INTERFACE_NAME).unwrap().is_none())
        .expect("open still finds the adapter after drop");
    eprintln!("open returned None {took:?} after drop");
}

#[test]
#[ignore]
fn session_starts_receives_and_sends() {
    let dll = dll();
    let adapter = create(&dll);
    let session = adapter.start_session(SESSION_CAPACITY).unwrap();
    assert!(!session.read_event().is_invalid());
    // The adapter has no addresses yet; the ring may be empty or hold stray packets.
    while let Some(packet) = session.try_receive().unwrap() {
        assert!(!packet.is_empty());
    }
    // A bare IPv4 header to the tunnel address: accepted by the ring, dropped by the stack.
    let mut packet = [0u8; 20];
    packet[0] = 0x45;
    packet[3] = 20;
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&[198, 18, 0, 2]);
    packet[16..20].copy_from_slice(&[198, 18, 0, 1]);
    session.send(&packet).unwrap();
    assert!(session.send(&[]).is_err());
    drop(session);
    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}

#[test]
#[ignore]
fn addresses_routes_metric_and_dns_on_our_adapter_only() {
    let dll = dll();
    let adapter = create(&dll);
    let luid = adapter.luid();
    ip::configure_interface(luid, Family::V4).unwrap();
    ip::configure_interface(luid, Family::V6).unwrap();
    ip::add_address(luid, ADDRESS_V4.into(), 32).unwrap();
    ip::add_address(luid, ADDRESS_V6.into(), 128).unwrap();
    // No routes here: IPv4 /1 routes would take this machine off the internet.
    assert_eq!(dns_servers("IPv4"), "");
    assert_no_ipv6_servers();
    dns::set_dns(WINDOWS_ADAPTER_GUID, &[ADDRESS_V4.into()]).unwrap();

    for family in ["IPv4", "IPv6"] {
        // MSFT_NetIPInterface reports a metric of 0 as null, hence the [int].
        let interface = ps(&format!(
            "Get-NetIPInterface -InterfaceAlias {INTERFACE_NAME} -AddressFamily {family} | ForEach-Object {{ \"$([int]$_.InterfaceMetric) $($_.AutomaticMetric) $($_.NlMtu)\" }}"
        ));
        assert_eq!(interface, "0 Disabled 1500", "{family} interface");
        // The stack's own view, read only: `Idx Met MTU State Name`.
        let shown = ps(&format!(
            "netsh interface {} show interfaces",
            family.to_lowercase()
        ));
        let row = shown
            .lines()
            .find(|line| line.ends_with(INTERFACE_NAME))
            .unwrap_or_else(|| panic!("{INTERFACE_NAME} missing from netsh:\n{shown}"));
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(columns[1..3], ["0", "1500"], "{family}: {row}");
    }
    assert_eq!(manual_addresses(), "198.18.0.1/32\nfd6c:6779:6465::1/128");
    assert_eq!(dns_servers("IPv4"), "198.18.0.1");
    assert_no_ipv6_servers();

    // With the phone's DNS off: the fallback servers, one call per family.
    dns::set_dns(WINDOWS_ADAPTER_GUID, &FALLBACK_DNS).unwrap();
    assert_eq!(dns_servers("IPv4"), "1.1.1.1,8.8.8.8");
    assert_eq!(
        dns_servers("IPv6"),
        "2606:4700:4700::1111,2001:4860:4860::8888"
    );

    dns::clear_dns(WINDOWS_ADAPTER_GUID).unwrap();
    assert_eq!(dns_servers("IPv4"), "");
    assert_no_ipv6_servers();
    for service in ["Tcpip", "Tcpip6"] {
        let stored = ps(&format!(
            "(Get-ItemProperty '{}' -ErrorAction SilentlyContinue).NameServer",
            interface_key(service)
        ));
        assert_eq!(stored, "", "{service} NameServer after clear_dns");
    }

    ip::flush(luid).unwrap();
    assert_eq!(manual_addresses(), "");

    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");

    // Uninstall: our Tcpip keys go; a second call finds nothing and is still Ok.
    dns::remove_interface_keys(WINDOWS_ADAPTER_GUID).unwrap();
    dns::remove_interface_keys(WINDOWS_ADAPTER_GUID).unwrap();
    for service in ["Tcpip", "Tcpip6"] {
        let key = interface_key(service);
        assert_eq!(ps(&format!("Test-Path '{key}'")), "False", "{key}");
    }
}

#[test]
#[ignore]
fn routes_go_on_our_luid() {
    let dll = dll();
    let adapter = create(&dll);
    let luid = adapter.luid();
    ip::configure_interface(luid, Family::V6).unwrap();
    // IPv6 only: this machine reaches the internet over IPv4.
    for route in IPV6_ROUTES {
        ip::add_route(luid, route).unwrap();
    }
    let routes = || {
        let listed = ps(&format!(
            "$routes = Get-NetRoute -InterfaceAlias {INTERFACE_NAME} -ErrorAction SilentlyContinue; $routes | Where-Object Protocol -eq NetMgmt | ForEach-Object {{ \"$($_.DestinationPrefix) $($_.NextHop) $($_.RouteMetric)\" }}"
        ));
        let mut lines: Vec<String> = listed.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    assert_eq!(routes(), ["8000::/1 :: 0", "::/1 :: 0"]);
    ip::flush(luid).unwrap();
    assert!(routes().is_empty(), "routes left after flush");
    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}
