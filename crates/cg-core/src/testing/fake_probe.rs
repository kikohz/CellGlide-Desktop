use crate::exit_check::{ExitProbe, TunnelProbe};
use crate::resolver::BoxFuture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct FakeProbe {
    ok: AtomicBool,
    calls: AtomicUsize,
    delay: Mutex<Duration>,
}

impl FakeProbe {
    pub fn new() -> Arc<FakeProbe> {
        Arc::new(FakeProbe {
            ok: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            delay: Mutex::new(Duration::ZERO),
        })
    }

    /// How long each check takes before answering, like a slow HTTPS probe.
    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    pub fn set_ok(&self, ok: bool) {
        self.ok.store(ok, Ordering::SeqCst);
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

pub struct SharedProbe(pub Arc<FakeProbe>);

impl ExitProbe for SharedProbe {
    fn check(&self, _socks_port: u16) -> BoxFuture<'_, bool> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let ok = self.0.ok.load(Ordering::SeqCst);
        let delay = *self.0.delay.lock().unwrap();
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            ok
        })
    }
}

/// Stands in for the captive check through the tunnel.
pub struct FakeTunnelProbe {
    ok: AtomicBool,
    calls: AtomicUsize,
}

impl FakeTunnelProbe {
    pub fn new() -> Arc<FakeTunnelProbe> {
        Arc::new(FakeTunnelProbe {
            ok: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        })
    }

    pub fn set_ok(&self, ok: bool) {
        self.ok.store(ok, Ordering::SeqCst);
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TunnelProbe for FakeTunnelProbe {
    fn check(&self) -> BoxFuture<'_, bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ok = self.ok.load(Ordering::SeqCst);
        Box::pin(async move { ok })
    }
}
