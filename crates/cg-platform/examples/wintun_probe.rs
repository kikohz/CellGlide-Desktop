//! Creates cellglide0 and a session, prints `ready`, then sleeps until killed. Used to check
//! that the adapter disappears when the process dies (`taskkill /F /IM wintun_probe.exe`).

#[cfg(windows)]
fn main() {
    use cg_core::tunnel::{INTERFACE_NAME, WINDOWS_ADAPTER_GUID, WINDOWS_TUNNEL_TYPE};
    use cg_platform::windows::wintun::{Adapter, SESSION_CAPACITY, Wintun};
    use std::io::Write;

    let dll = Wintun::load().unwrap_or_else(|e| fail("load", e));
    match Adapter::open(&dll, INTERFACE_NAME) {
        Ok(Some(existing)) => println!(
            "{INTERFACE_NAME} already exists (luid {:#x})",
            existing.luid()
        ),
        Ok(None) => println!("{INTERFACE_NAME} does not exist yet"),
        Err(e) => println!("open: {e} (code {:?})", e.raw_os_error()),
    }
    let adapter = Adapter::create(
        &dll,
        INTERFACE_NAME,
        WINDOWS_TUNNEL_TYPE,
        WINDOWS_ADAPTER_GUID,
    )
    .unwrap_or_else(|e| fail("create", e));
    let session = adapter
        .start_session(SESSION_CAPACITY)
        .unwrap_or_else(|e| fail("start_session", e));
    println!("ready luid={:#x}", adapter.luid());
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
        let _ = &session;
    }
}

#[cfg(windows)]
fn fail(step: &str, e: std::io::Error) -> ! {
    eprintln!("{step}: {e} (code {:?})", e.raw_os_error());
    std::process::exit(1)
}

#[cfg(not(windows))]
fn main() {}
