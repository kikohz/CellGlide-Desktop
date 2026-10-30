//! Needs an elevated shell and wintun.dll. Take network snapshots before and after:
//!   scripts\fetch-wintun.ps1
//!   $env:CELLGLIDE_WINTUN_DLL = "$PWD\target\debug\wintun.dll"
//!   cargo test -p cg-platform --test windows_privileged -- --ignored --test-threads=1
#![cfg(windows)]

use cg_core::tunnel::{INTERFACE_NAME, WINDOWS_ADAPTER_GUID, WINDOWS_TUNNEL_TYPE};
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
