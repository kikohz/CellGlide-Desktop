//! Brings the tunnel up and waits; the privileged tests SIGKILL it, SIGTERM stops it cleanly.

#[cfg(target_os = "linux")]
fn main() {
    use cg_core::tunnel::{DnsPlan, RESOLVER, TunnelBackend, TunnelPlan};
    tracing_subscriber::fmt().init();
    let socks = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let plan = TunnelPlan {
        socks_port: socks.local_addr().unwrap().port(),
        dns: DnsPlan::Resolver(RESOLVER),
        bypass: true,
    };
    let tunnel = cg_platform::LinuxBackend.start(&plan).expect("tunnel up");
    println!("up");
    static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    extern "C" fn on_term(_: libc::c_int) {
        TERM.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic flag.
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };
    while !TERM.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if let Err(error) = tunnel.stop() {
        eprintln!("stop: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
