//! hev-socks5-tunnel on its own thread. hev keeps global state, so there is at most
//! one per process.

use cg_core::tunnel::TunnelError;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long hev gets to fail on its config before the tunnel counts as up.
const STARTUP_GRACE: Duration = Duration::from_millis(300);
const STOP_WAIT: Duration = Duration::from_secs(5);

pub struct Hev {
    thread: Option<JoinHandle<i32>>,
    exited: Arc<AtomicBool>,
}

impl Hev {
    pub fn spawn(config: String, tun_fd: RawFd) -> Result<Hev, TunnelError> {
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        let thread = std::thread::Builder::new()
            .name("hev".into())
            .spawn(move || {
                let code = unsafe {
                    hev_sys::hev_socks5_tunnel_main_from_str(
                        config.as_ptr(),
                        config.len() as u32,
                        tun_fd,
                    )
                };
                flag.store(true, Ordering::SeqCst);
                code
            })
            .map_err(|e| TunnelError(format!("can't start the forwarding thread: {e}")))?;
        std::thread::sleep(STARTUP_GRACE);
        let hev = Hev {
            thread: Some(thread),
            exited,
        };
        if hev.has_exited() {
            let code = hev.join();
            return Err(TunnelError(format!(
                "hev failed to start (returned {code})"
            )));
        }
        Ok(hev)
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn join(mut self) -> i32 {
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or(-1)
    }

    /// Asks hev to quit once (hev honours an early quit) and waits for it to exit. If
    /// it does not, the fd may still be in use, so abort and let systemd restart us;
    /// the kernel removes the link with the process.
    pub fn stop(self) {
        unsafe { hev_sys::hev_socks5_tunnel_quit() };
        let deadline = Instant::now() + STOP_WAIT;
        while !self.has_exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.has_exited() {
            self.join();
        } else {
            tracing::error!("hev did not quit within {STOP_WAIT:?}; aborting the process");
            std::process::abort();
        }
    }
}
