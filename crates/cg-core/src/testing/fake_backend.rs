use crate::tunnel::{RunningTunnel, TunnelBackend, TunnelError, TunnelNotice, TunnelPlan};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Records what the controller asks of the platform.
#[derive(Default)]
pub struct FakeBackend {
    started: AtomicUsize,
    stopped: AtomicUsize,
    plans: Mutex<Vec<TunnelPlan>>,
    exited: Arc<AtomicBool>,
    dns_unmanaged: AtomicBool,
    notices: Arc<Mutex<Vec<TunnelNotice>>>,
}

struct FakeRunning {
    backend: Arc<FakeBackend>,
    exited: Arc<AtomicBool>,
    dns_managed: bool,
    notices: Arc<Mutex<Vec<TunnelNotice>>>,
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
    pub fn stopped(&self) -> usize {
        self.stopped.load(Ordering::SeqCst)
    }
    pub fn last_plan(&self) -> Option<TunnelPlan> {
        self.plans.lock().unwrap().last().cloned()
    }
    /// Tunnels started from now on report that the system refused to set DNS.
    pub fn set_dns_unmanaged(&self, unmanaged: bool) {
        self.dns_unmanaged.store(unmanaged, Ordering::SeqCst);
    }
    /// The running tunnel reports this at its next `take_notices`.
    pub fn push_notice(&self, notice: TunnelNotice) {
        self.notices.lock().unwrap().push(notice);
    }
    /// The running tunnel reports that hev returned.
    pub fn exit_running(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }
}

/// `Arc<FakeBackend>` is what tests hold; the controller gets a clone of the same Arc.
pub struct SharedFake(pub Arc<FakeBackend>);

impl TunnelBackend for SharedFake {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError> {
        let backend = &self.0;
        backend.exited.store(false, Ordering::SeqCst);
        backend.started.fetch_add(1, Ordering::SeqCst);
        backend.plans.lock().unwrap().push(plan.clone());
        Ok(Box::new(FakeRunning {
            backend: backend.clone(),
            exited: backend.exited.clone(),
            dns_managed: !backend.dns_unmanaged.load(Ordering::SeqCst),
            notices: backend.notices.clone(),
        }))
    }
}

impl RunningTunnel for FakeRunning {
    fn stop(self: Box<Self>) -> Result<(), TunnelError> {
        self.backend.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    fn dns_managed(&self) -> bool {
        self.dns_managed
    }
    fn take_notices(&self) -> Vec<TunnelNotice> {
        std::mem::take(&mut *self.notices.lock().unwrap())
    }
}
