//! Needs an elevated shell and wintun.dll. Take network snapshots before and after:
//!   scripts\fetch-wintun.ps1
//!   $env:CELLGLIDE_WINTUN_DLL = "$PWD\target\debug\wintun.dll"
//!   cargo test -p cg-platform --test windows_privileged -- --ignored --test-threads=1
#![cfg(windows)]

use cg_core::tunnel::{
    ADDRESS_V4, ADDRESS_V6, DnsPlan, FALLBACK_DNS, INTERFACE_NAME, IPV6_ROUTES, ResolverOrder,
    TunnelBackend, TunnelPlan, WINDOWS_ADAPTER_GUID, WINDOWS_RESOLVER, WINDOWS_TUNNEL_TYPE,
};
use cg_netstack::testkit::FakeSocks;
use cg_platform::windows::ip::{self, Family};
use cg_platform::windows::wintun::{Adapter, SESSION_CAPACITY, Wintun};
use cg_platform::windows::{WindowsBackend, cleanup_leftovers_with};
use cg_platform::windows::{device, dns, strict_dns};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
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

// ---- WindowsBackend (Task 11). The IPv4 /1 routes take this machine off the internet
// while they exist: each test keeps that window short, and `Running`'s Drop removes
// everything on every path, a failed assertion included.

const CRASH_CHILD: &str = "CG_CRASH_CHILD";
const CHILD_READY: &str = "cg-crash-child-ready";

fn tunnel_plan(socks_port: u16) -> TunnelPlan {
    TunnelPlan {
        socks_port,
        dns: DnsPlan::Resolver(WINDOWS_RESOLVER),
        bypass: false,
    }
}

/// Our NetMgmt routes on cellglide0 as sorted `prefix metric` lines.
fn our_routes() -> Vec<String> {
    let listed = ps(&format!(
        "$routes = Get-NetRoute -InterfaceAlias {INTERFACE_NAME} -ErrorAction SilentlyContinue; $routes | Where-Object Protocol -eq NetMgmt | ForEach-Object {{ \"$($_.DestinationPrefix) $($_.RouteMetric)\" }}"
    ));
    let mut lines: Vec<String> = listed.lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

/// The tunnel's /1 routes on any interface, as `prefix alias` lines.
fn half_routes_anywhere() -> String {
    ps(
        "$r = Get-NetRoute -DestinationPrefix '0.0.0.0/1','128.0.0.0/1','::/1','8000::/1' -ErrorAction SilentlyContinue; $r | ForEach-Object { \"$($_.DestinationPrefix) $($_.InterfaceAlias)\" }",
    )
}

fn assert_absent(dll: &Arc<Wintun>) {
    assert!(
        Adapter::open(dll, INTERFACE_NAME).unwrap().is_none(),
        "{INTERFACE_NAME} already exists; remove the leftover first"
    );
}

#[test]
#[ignore]
fn start_finish_stop_leaves_nothing() {
    let dll = dll();
    assert_absent(&dll);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socks = runtime.block_on(FakeSocks::start());
    let backend = WindowsBackend::new().unwrap();
    assert_eq!(backend.resolver_order(), ResolverOrder::AfterInterface);

    let tunnel = backend.start(&tunnel_plan(socks.addr().port())).unwrap();
    // start: the interface and its addresses only.
    assert!(adapter_listed(), "{INTERFACE_NAME} not listed after start");
    let addresses = manual_addresses();
    let ipv6 = addresses.contains("fd6c:6779:6465::1/128");
    assert!(addresses.contains("198.18.0.1/32"), "{addresses}");
    assert!(our_routes().is_empty(), "routes before finish");
    let (physical_v4, _) = physical_dns();
    assert_eq!(dns_servers("IPv4"), "");

    // finish: DNS, routes, engine. From here until stop this machine is off the internet.
    let started = Instant::now();
    tunnel.finish(&DnsPlan::Resolver(WINDOWS_RESOLVER)).unwrap();
    assert!(tunnel.dns_managed());
    assert!(tunnel.strict_dns());
    let mut expected = vec!["0.0.0.0/1 0", "128.0.0.0/1 0"];
    if ipv6 {
        expected.extend(["8000::/1 0", "::/1 0"]);
    }
    expected.sort();
    assert_eq!(our_routes(), expected);
    let metric = ps(&format!(
        "Get-NetIPInterface -InterfaceAlias {INTERFACE_NAME} -AddressFamily IPv4 | ForEach-Object {{ [int]$_.InterfaceMetric }}"
    ));
    assert_eq!(metric, "0");
    assert_eq!(dns_servers("IPv4"), "198.18.0.1");
    // The physical adapter's DNS server is on-link, so off the tunnel: blocked.
    assert!(
        !answered(physical_v4),
        "{physical_v4} answered with the tunnel up"
    );

    // A plain TCP connection (no IP_UNICAST_IF) goes through the tunnel to the relay.
    let target: SocketAddr = "1.1.1.1:443".parse().unwrap();
    let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"ping").unwrap();
    let mut echo = [0u8; 4];
    stream.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping", "FakeSocks echoes");
    assert!(socks.connects().contains(&target), "{:?}", socks.connects());
    drop(stream);
    assert!(!tunnel.has_exited());

    tunnel.stop().unwrap();
    eprintln!("routes were up for {:?}", started.elapsed());
    let left = half_routes_anywhere();
    assert!(left.is_empty(), "routes left after stop: {left}");
    wait_for(|| !adapter_listed()).expect("adapter still listed after stop");
    assert!(Adapter::open(&dll, INTERFACE_NAME).unwrap().is_none());
}

/// The controller's `finish` thread panicking drops the tunnel without `stop`.
#[test]
#[ignore]
fn dropping_without_stop_cleans_up() {
    let dll = dll();
    assert_absent(&dll);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socks = runtime.block_on(FakeSocks::start());
    let backend = WindowsBackend::new().unwrap();
    let tunnel = backend.start(&tunnel_plan(socks.addr().port())).unwrap();
    tunnel.finish(&DnsPlan::Resolver(WINDOWS_RESOLVER)).unwrap();
    drop(tunnel);
    let left = half_routes_anywhere();
    assert!(left.is_empty(), "routes left after drop: {left}");
    assert!(Adapter::open(&dll, INTERFACE_NAME).unwrap().is_none());
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}

/// Kills the child if the test fails before it does.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Hidden entry for `crash_leaves_nothing`: does nothing unless CG_CRASH_CHILD=1.
#[test]
#[ignore]
fn crash_child_entry() {
    if std::env::var_os(CRASH_CHILD).is_none_or(|v| v != "1") {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let socks = runtime.block_on(FakeSocks::start());
    let backend = WindowsBackend::new().unwrap();
    let tunnel = backend.start(&tunnel_plan(socks.addr().port())).unwrap();
    tunnel.finish(&DnsPlan::Resolver(WINDOWS_RESOLVER)).unwrap();
    // libtest prints "test crash_child_entry ... " before this on the same line.
    println!("{CHILD_READY}");
    std::io::stdout().flush().unwrap();
    // The parent kills us long before this; if it doesn't, Drop cleans up.
    std::thread::sleep(Duration::from_secs(30));
    drop(tunnel);
}

#[test]
#[ignore]
fn crash_leaves_nothing() {
    assert_absent(&dll());
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "crash_child_entry",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CRASH_CHILD, "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = KillOnDrop(child);
    let stdout = child.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if line.contains(CHILD_READY) {
                let _ = ready_tx.send(());
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("child never became ready");
    let routes = half_routes_anywhere();
    assert!(
        routes.contains("0.0.0.0/1 cellglide0") && routes.contains("128.0.0.0/1 cellglide0"),
        "routes while the child runs: {routes}"
    );

    // Child::kill is TerminateProcess.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let took = wait_for(|| !adapter_listed() && half_routes_anywhere().is_empty())
        .expect("adapter or routes still there after the child was killed");
    eprintln!("adapter and routes gone {took:?} after TerminateProcess");
    assert!(Adapter::open(&dll(), INTERFACE_NAME).unwrap().is_none());
}

/// Holds 0.0.0.0:53 (UDP) with SO_EXCLUSIVEADDRUSE, as a service like SharedAccess might.
fn hold_port_53_exclusively() -> std::net::UdpSocket {
    use std::os::windows::io::FromRawSocket;
    use windows::Win32::Networking::WinSock::{
        AF_INET, IPPROTO_UDP, SO_EXCLUSIVEADDRUSE, SOCK_DGRAM, SOCKADDR, SOCKADDR_IN, SOL_SOCKET,
        WSAGetLastError, bind, setsockopt, socket,
    };
    // Initializes Winsock for this process.
    drop(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
    // SAFETY: plain socket creation.
    let s = unsafe { socket(AF_INET.0.into(), SOCK_DGRAM, IPPROTO_UDP.0) }.unwrap();
    // SAFETY: we own the new socket; the UdpSocket closes it.
    let owned = unsafe { std::net::UdpSocket::from_raw_socket(s.0 as u64) };
    let on = 1u32.to_ne_bytes();
    // SAFETY: the option takes a 4-byte BOOL.
    let set = unsafe { setsockopt(s, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, Some(&on)) };
    // SAFETY: reads the thread's last Winsock error.
    let error = unsafe { WSAGetLastError() }.0;
    assert_eq!(set, 0, "setsockopt: WSA {error}");
    let address = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: 53u16.to_be(),
        ..Default::default()
    };
    let length = std::mem::size_of::<SOCKADDR_IN>() as i32;
    // SAFETY: address is a SOCKADDR_IN of `length` bytes.
    let bound = unsafe { bind(s, (&raw const address).cast::<SOCKADDR>(), length) };
    // SAFETY: as above.
    let error = unsafe { WSAGetLastError() }.0;
    assert_eq!(bound, 0, "bind 0.0.0.0:53: WSA {error}");
    owned
}

#[test]
#[ignore]
fn resolver_port_taken_rolls_back() {
    use cg_core::controller::{
        Command as ControllerCommand, Controller, ControllerConfig, SettingsSource,
    };
    use cg_core::settings::Stored;
    use cg_core::testing::{
        FakeMux, FakePhone, FakePhoneOptions, FakeProbe, FakeTunnelProbe, SharedProbe, device,
    };
    use cg_ipc::{ConnState, Reason, Snapshot};

    let dll = dll();
    assert_absent(&dll);
    let _held = hold_port_53_exclusively();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (state, gone) = runtime.block_on(async {
        let mux = FakeMux::start().await;
        let _phone = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let mut config =
            ControllerConfig::new(mux.endpoint(), Arc::new(WindowsBackend::new().unwrap()));
        config.resolver_addr = WINDOWS_RESOLVER;
        config.bypass = false;
        config.settings = SettingsSource::Memory(Stored {
            auto_connect: false,
            ..Stored::default()
        });
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.tunnel_probe = FakeTunnelProbe::new();
        let controller = Controller::spawn(config);

        async fn wait(
            controller: &Controller,
            what: &str,
            pred: impl Fn(&Snapshot) -> bool,
        ) -> Snapshot {
            let mut rx = controller.subscribe();
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let snap = rx.borrow_and_update().clone();
                    if pred(&snap) {
                        return snap;
                    }
                    rx.changed().await.unwrap();
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        }
        mux.attach(device("aaaa", 1));
        wait(&controller, "idle", |s| s.state == ConnState::Idle).await;
        controller
            .send(ControllerCommand::Connect { serial: None })
            .await
            .unwrap();
        let snap = wait(&controller, "retrying", |s| {
            matches!(s.state, ConnState::Retrying { .. })
        })
        .await;
        // Rolled back before the error was reported (and so before any retry).
        let gone = Adapter::open(&dll, INTERFACE_NAME).unwrap().is_none();
        controller
            .send(ControllerCommand::Disconnect)
            .await
            .unwrap();
        controller.shutdown().await;
        (snap.state, gone)
    });
    assert_eq!(
        state,
        ConnState::Retrying {
            reason: Reason::ResolverPortBusy { port: 53 }
        }
    );
    assert!(
        gone,
        "{INTERFACE_NAME} still there when Retrying was reported"
    );
    wait_for(|| !adapter_listed()).expect("adapter still listed after shutdown");
}

#[test]
#[ignore]
fn cleanup_leftovers_with_nothing_left_is_ok() {
    let dll = dll();
    assert_absent(&dll);
    cleanup_leftovers_with(&dll).unwrap();
    assert!(Adapter::open(&dll, INTERFACE_NAME).unwrap().is_none());
}

/// No routes: the session and the two bridge threads only.
#[test]
#[ignore]
fn bridge_threads_exit_within_a_second_of_stop() {
    let dll = dll();
    let adapter = create(&dll);
    let session = Arc::new(adapter.start_session(SESSION_CAPACITY).unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let (engine_end, threads) = device::bridge(Arc::clone(&session), stop.clone()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!threads.is_finished());
    let start = Instant::now();
    stop.cancel();
    threads.join();
    let took = start.elapsed();
    eprintln!("bridge threads exited {took:?} after stop");
    assert!(took < Duration::from_secs(1), "{took:?}");
    drop(engine_end);
    drop(session);
    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}

/// A live cellglide0 (here: ours, standing in for another process's) is neither closed nor
/// stripped of its DNS servers.
#[test]
#[ignore]
fn cleanup_leftovers_leaves_a_live_adapter_and_its_dns() {
    let dll = dll();
    let adapter = create(&dll);
    let luid = adapter.luid();
    ip::configure_interface(luid, Family::V4).unwrap();
    ip::add_address(luid, ADDRESS_V4.into(), 32).unwrap();
    dns::set_dns(WINDOWS_ADAPTER_GUID, &[ADDRESS_V4.into()]).unwrap();
    cleanup_leftovers_with(&dll).unwrap();
    assert!(adapter_listed(), "{INTERFACE_NAME} gone after cleanup");
    assert_eq!(dns_servers("IPv4"), "198.18.0.1");
    dns::clear_dns(WINDOWS_ADAPTER_GUID).unwrap();
    ip::flush(luid).unwrap();
    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}

// ---- Strict DNS (Task 11b). No routes: the machine stays on its physical adapter, whose
// DNS server is what the rules must block.

const STRICT_CHILD: &str = "CG_STRICT_CHILD";
const STRICT_READY: &str = "cg-strict-child-ready";
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// The DNS servers (IPv4, and IPv6 if any) of the interface holding the best IPv4 default
/// route: the physical adapter whose servers Windows would also ask.
fn physical_dns() -> (IpAddr, Option<IpAddr>) {
    let listed = ps(&format!(
        "$i = (Get-NetRoute -DestinationPrefix 0.0.0.0/0 | Where-Object InterfaceAlias -ne {INTERFACE_NAME} | Sort-Object RouteMetric | Select-Object -First 1).ifIndex; foreach ($f in 'IPv4','IPv6') {{ $s = (Get-DnsClientServerAddress -InterfaceIndex $i -AddressFamily $f).ServerAddresses | Select-Object -First 1; if ($s) {{ $s }} else {{ '-' }} }}"
    ));
    let mut lines = listed.lines();
    let v4 = lines
        .next()
        .and_then(|s| s.parse().ok())
        .expect("no IPv4 DNS server on the default-route interface");
    let v6 = lines.next().and_then(|s| s.parse().ok());
    (v4, v6)
}

/// A standard query for `example.com` A with the given id.
fn dns_query(id: u16) -> Vec<u8> {
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in ["example", "com"] {
        query.push(u8::try_from(label.len()).unwrap());
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    query
}

/// Whether `server` answers one UDP query within ANSWER_WITHIN. A send refused locally
/// (the WFP block) counts as no answer.
fn answered(server: IpAddr) -> bool {
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind).unwrap();
    socket.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
    let id = 0x5a5a;
    if let Err(error) = socket.send_to(&dns_query(id), SocketAddr::new(server, 53)) {
        eprintln!("send to {server}:53 refused: {error}");
        return false;
    }
    let deadline = Instant::now() + ANSWER_WITHIN;
    let mut buf = [0u8; 1500];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if n >= 2 && from.ip() == server && buf[..2] == id.to_be_bytes() => {
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// cellglide0 with its IPv4 address, no routes.
fn adapter_with_address(dll: &Arc<Wintun>) -> Adapter {
    let adapter = create(dll);
    let luid = adapter.luid();
    ip::configure_interface(luid, Family::V4).unwrap();
    ip::add_address(luid, ADDRESS_V4.into(), 32).unwrap();
    adapter
}

#[test]
#[ignore]
fn strict_dns_blocks_other_port_53_only_while_alive() {
    let dll = dll();
    let (dns_v4, dns_v6) = physical_dns();
    assert!(
        answered(dns_v4),
        "{dns_v4} doesn't answer even without strict DNS"
    );
    let v6 = dns_v6.filter(|server| answered(*server));
    eprintln!(
        "physical DNS: {dns_v4}, {dns_v6:?}; IPv6 answers: {}",
        v6.is_some()
    );

    let adapter = adapter_with_address(&dll);
    let luid = adapter.luid();
    // Our resolver's address: a query to it stays on the machine and must still pass.
    let resolver = UdpSocket::bind(WINDOWS_RESOLVER).unwrap();
    resolver.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();

    let strict = strict_dns::start(luid).unwrap();
    assert!(!answered(dns_v4), "{dns_v4} answered with strict DNS on");
    if let Some(v6) = v6 {
        assert!(!answered(v6), "{v6} answered with strict DNS on");
    }
    // Other traffic over the physical adapter is unaffected.
    let target: SocketAddr = "1.1.1.1:443".parse().unwrap();
    TcpStream::connect_timeout(&target, Duration::from_secs(5))
        .expect("TCP 1.1.1.1:443 blocked with strict DNS on");
    // Port 53 on our own address is allowed.
    let client = UdpSocket::bind("0.0.0.0:0").unwrap();
    client.send_to(&dns_query(1), WINDOWS_RESOLVER).unwrap();
    let mut buf = [0u8; 512];
    let (n, _) = resolver
        .recv_from(&mut buf)
        .expect("query to 198.18.0.1:53 blocked with strict DNS on");
    assert_eq!(buf[..n], dns_query(1));

    drop(strict);
    assert!(answered(dns_v4), "{dns_v4} still blocked after drop");
    if let Some(v6) = v6 {
        assert!(answered(v6), "{v6} still blocked after drop");
    }
    drop(resolver);
    ip::flush(luid).unwrap();
    drop(adapter);
    wait_for(|| !adapter_listed()).expect("adapter still listed after drop");
}

/// Hidden entry for `strict_dns_rules_vanish_when_the_process_dies`: does nothing unless
/// CG_STRICT_CHILD=1.
#[test]
#[ignore]
fn strict_child_entry() {
    if std::env::var_os(STRICT_CHILD).is_none_or(|v| v != "1") {
        return;
    }
    let dll = dll();
    let adapter = adapter_with_address(&dll);
    let strict = strict_dns::start(adapter.luid()).unwrap();
    println!("{STRICT_READY}");
    std::io::stdout().flush().unwrap();
    // The parent kills us long before this; if it doesn't, Drop cleans up.
    std::thread::sleep(Duration::from_secs(30));
    drop(strict);
    drop(adapter);
}

#[test]
#[ignore]
fn strict_dns_rules_vanish_when_the_process_dies() {
    assert_absent(&dll());
    let (dns_v4, _) = physical_dns();
    assert!(
        answered(dns_v4),
        "{dns_v4} doesn't answer even without strict DNS"
    );
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "strict_child_entry",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(STRICT_CHILD, "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = KillOnDrop(child);
    let stdout = child.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if line.contains(STRICT_READY) {
                let _ = ready_tx.send(());
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("child never became ready");
    assert!(!answered(dns_v4), "{dns_v4} answered while the child runs");

    // Child::kill is TerminateProcess.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let killed = Instant::now();
    assert!(
        answered(dns_v4),
        "{dns_v4} still blocked 2 s after TerminateProcess"
    );
    eprintln!("answered {:?} after TerminateProcess", killed.elapsed());
    wait_for(|| !adapter_listed()).expect("adapter still listed after the child was killed");
}
