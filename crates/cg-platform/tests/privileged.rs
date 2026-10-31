//! Needs root and systemd-resolved. Run: sudo -E cargo test -p cg-platform --test privileged -- --ignored --test-threads=1
#![cfg(target_os = "linux")]

use cg_core::tunnel::{DnsPlan, INTERFACE_NAME, RESOLVER, TunnelBackend, TunnelPlan};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn snapshot() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/network-snapshot.sh");
    let out = Command::new(script).arg(INTERFACE_NAME).output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn sh(command: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(command).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn link_exists() -> bool {
    Path::new("/sys/class/net").join(INTERFACE_NAME).exists()
}

fn plan() -> TunnelPlan {
    TunnelPlan {
        socks_port: 1080,
        dns: DnsPlan::Resolver(RESOLVER),
        bypass: false,
        strict_dns: true,
    }
}

fn in_netns() -> bool {
    std::env::var_os("CELLGLIDE_NETNS").is_some()
}

fn ours() -> Vec<cg_platform::bypass::OurRoute> {
    let out = Command::new("ip")
        .args(["-j", "-4", "route", "show", "table", "main"])
        .output()
        .unwrap();
    cg_platform::bypass::parse_ours(&String::from_utf8_lossy(&out.stdout))
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore]
fn tunnel_up_and_down_touches_only_our_link() {
    let before = snapshot();
    let tunnel = cg_platform::LinuxBackend.start(&plan()).unwrap();
    assert!(sh("ip route get 1.1.1.1").contains(&format!("dev {INTERFACE_NAME}")));
    assert!(sh(&format!("resolvectl dns {INTERFACE_NAME}")).contains(&RESOLVER.to_string()));
    assert!(sh(&format!("resolvectl domain {INTERFACE_NAME}")).contains("~."));
    assert_eq!(snapshot(), before, "other links changed while up");
    assert!(!tunnel.has_exited());
    tunnel.stop().unwrap();
    assert!(!link_exists());
    assert_eq!(snapshot(), before, "other links changed after stop");
}

#[test]
#[ignore]
fn second_tunnel_is_refused() {
    let first = cg_platform::LinuxBackend.start(&plan()).unwrap();
    let second = cg_platform::LinuxBackend.start(&plan());
    assert!(second.is_err());
    first.stop().unwrap();
    assert!(!link_exists());
}

#[test]
#[ignore]
fn killed_holder_leaves_nothing() {
    let before = snapshot();
    let example: PathBuf = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/hold_tunnel");
    let mut child = Command::new(example)
        .stdout(Stdio::piped())
        .spawn()
        .expect("build examples first: cargo build -p cg-platform --examples");
    // The example prints "up" only after the interface, routes and DNS are all set.
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok(line) if line.trim() == "up" => break,
            Ok(_) => {}
            Err(_) => panic!("tunnel never came up"),
        }
    }
    assert!(sh(&format!("resolvectl dns {INTERFACE_NAME}")).contains(&RESOLVER.to_string()));
    assert!(sh("ip route get 1.1.1.1").contains(&format!("dev {INTERFACE_NAME}")));
    assert!(!ours().is_empty(), "bypass routes should be up");
    child.kill().unwrap(); // SIGKILL
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!link_exists());
    assert!(!sh("resolvectl dns").contains(&RESOLVER.to_string()));
    // The routes outlive the process; the service's next start cleans them up.
    assert!(!ours().is_empty(), "bypass routes should be left behind");
    cg_platform::bypass::cleanup_leftovers();
    assert!(ours().is_empty());
    assert_eq!(snapshot(), before);
}

#[test]
#[ignore]
fn lan_routes_vanish_with_the_tunnel() {
    let before = snapshot();
    let mut with_bypass = plan();
    with_bypass.bypass = true;
    let tunnel = cg_platform::LinuxBackend.start(&with_bypass).unwrap();
    assert!(!ours().is_empty());
    tunnel.stop().unwrap();
    assert!(ours().is_empty());
    assert!(!link_exists());
    assert_eq!(snapshot(), before);
}

#[test]
#[ignore]
fn bypass_adds_routes_via_the_default_gateway() {
    if !in_netns() {
        return;
    }
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    let routes = ours();
    assert_eq!(routes.len(), 6, "{routes:?}");
    assert!(routes.iter().all(|r| r.dev == "up1"));
    assert_eq!(
        routes
            .iter()
            .filter(|r| r.via == Some("10.91.0.1".parse().unwrap()))
            .count(),
        4
    );
    bypass.stop();
    assert!(ours().is_empty());
}

#[test]
#[ignore]
fn bypass_follows_gateway_change() {
    if !in_netns() {
        return;
    }
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    sh("ip route replace default via 10.92.0.1 dev up2 metric 100");
    wait_until("routes moved to up2", || {
        let r = ours();
        r.len() == 6 && r.iter().all(|r| r.dev == "up2")
    });
    bypass.stop();
    assert!(ours().is_empty());
    sh("ip route replace default via 10.91.0.1 dev up1 metric 100");
}

#[test]
#[ignore]
fn bypass_follows_when_an_uplink_goes_down() {
    if !in_netns() {
        return;
    }
    sh("ip route add default via 10.92.0.1 dev up2 metric 200");
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    assert!(ours().iter().all(|r| r.dev == "up1"));
    sh("ip link set up1 down");
    wait_until("routes moved to up2", || {
        let r = ours();
        r.len() == 6 && r.iter().all(|r| r.dev == "up2")
    });
    bypass.stop();
    assert!(ours().is_empty());
    sh("ip link set up1 up");
    sh("ip route replace default via 10.91.0.1 dev up1 metric 100");
    sh("ip route del default dev up2");
}

/// The connection timeline hears once when the gateway goes missing, not per route event.
#[test]
#[ignore]
fn bypass_reports_a_missing_gateway() {
    if !in_netns() {
        return;
    }
    use cg_core::tunnel::TunnelNotice;
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    assert_eq!(bypass.take_notices(), []);
    sh("ip route del default");
    std::thread::sleep(Duration::from_millis(800));
    sh("ip link set up2 down");
    sh("ip link set up2 up");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(bypass.take_notices(), [TunnelNotice::BypassUnavailable]);
    assert_eq!(bypass.take_notices(), []);
    sh("ip route add default via 10.91.0.1 dev up1 metric 100");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(bypass.take_notices(), []);
    bypass.stop();
}

#[test]
#[ignore]
fn bypass_keeps_old_routes_without_gateway() {
    if !in_netns() {
        return;
    }
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    sh("ip route del default");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(ours().len(), 6);
    bypass.stop();
    assert!(ours().is_empty());
    sh("ip route add default via 10.91.0.1 dev up1 metric 100");
}

#[test]
#[ignore]
fn bypass_cleanup_removes_only_realm_198() {
    if !in_netns() {
        return;
    }
    sh("ip route add 10.0.0.0/8 dev up2 metric 500");
    sh("ip route add 172.16.0.0/12 via 10.91.0.1 dev up1 proto static realm 198 metric 32766");
    cg_platform::bypass::cleanup_leftovers();
    assert!(ours().is_empty());
    assert!(
        sh("ip route show 10.0.0.0/8").contains("dev up2"),
        "the user's own route stays"
    );
    sh("ip route del 10.0.0.0/8 dev up2 metric 500");
}

#[test]
#[ignore]
fn bypass_restores_routes_after_address_flush() {
    if !in_netns() {
        return;
    }
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    assert_eq!(ours().len(), 6);
    sh("ip addr flush dev up1");
    assert!(ours().is_empty(), "the kernel drops the routes on flush");
    sh("ip addr add 10.91.0.2/24 dev up1");
    sh("ip route add default via 10.91.0.1 dev up1 metric 100");
    wait_until("routes back on up1", || {
        let r = ours();
        r.len() == 6 && r.iter().all(|r| r.dev == "up1")
    });
    bypass.stop();
    assert!(ours().is_empty());
}

#[test]
#[ignore]
fn bypass_yields_to_the_users_own_route() {
    if !in_netns() {
        return;
    }
    sh("ip route add 10.0.0.0/8 dev up2 metric 500");
    let bypass = cg_platform::bypass::Bypass::start("cellglide0");
    assert_eq!(ours().len(), 6, "ours are added next to the user's");
    let chosen = sh("ip route get 10.1.2.3");
    assert!(
        chosen.contains("dev up2"),
        "user's route must win: {chosen}"
    );
    bypass.stop();
    assert!(sh("ip route show 10.0.0.0/8").contains("dev up2"));
    sh("ip route del 10.0.0.0/8 dev up2 metric 500");
}
