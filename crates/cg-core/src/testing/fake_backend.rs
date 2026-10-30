use crate::tunnel::{
    DnsPlan, ResolverOrder, RunningTunnel, TunnelBackend, TunnelError, TunnelNotice, TunnelPlan,
};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the controller handed `RunningTunnel::finish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishCall {
    pub dns: DnsPlan,
    /// Whether our resolver accepted a TCP connection at that moment.
    pub resolver_listening: bool,
}

/// Records what the controller asks of the platform.
#[derive(Default)]
pub struct FakeBackend {
    order: ResolverOrder,
    started: AtomicUsize,
    stopped: AtomicUsize,
    plans: Mutex<Vec<TunnelPlan>>,
    /// Per `start`: whether the plan's resolver was already listening.
    listening_at_start: Mutex<Vec<bool>>,
    finishes: Mutex<Vec<FinishCall>>,
    finish_fails: AtomicBool,
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

/// Whether a resolver listens on the plan's address. The resolver binds UDP and TCP on
/// the same port together, and a loopback TCP connect is answered or refused at once,
/// so an accepted connection is a yes/no signal with no reply to wait for.
fn resolver_listening(dns: &DnsPlan) -> bool {
    match dns {
        DnsPlan::Resolver(addr) => listening(*addr),
        DnsPlan::Servers(_) => false,
    }
}

fn listening(addr: SocketAddr) -> bool {
    addr.port() != 0 && TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// A backend that asks for the resolver at `order`.
    pub fn with_order(order: ResolverOrder) -> Arc<Self> {
        Arc::new(Self {
            order,
            ..Self::default()
        })
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
    /// At the last `start`: whether the plan's resolver was already listening.
    pub fn listening_at_start(&self) -> Option<bool> {
        self.listening_at_start.lock().unwrap().last().copied()
    }
    pub fn finished(&self) -> usize {
        self.finishes.lock().unwrap().len()
    }
    pub fn last_finish(&self) -> Option<FinishCall> {
        self.finishes.lock().unwrap().last().cloned()
    }
    /// Running tunnels fail `finish` from now on.
    pub fn set_finish_fails(&self, fails: bool) {
        self.finish_fails.store(fails, Ordering::SeqCst);
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
        backend
            .listening_at_start
            .lock()
            .unwrap()
            .push(resolver_listening(&plan.dns));
        Ok(Box::new(FakeRunning {
            backend: backend.clone(),
            exited: backend.exited.clone(),
            dns_managed: !backend.dns_unmanaged.load(Ordering::SeqCst),
            notices: backend.notices.clone(),
        }))
    }

    fn resolver_order(&self) -> ResolverOrder {
        self.0.order
    }
}

impl RunningTunnel for FakeRunning {
    fn stop(self: Box<Self>) -> Result<(), TunnelError> {
        self.backend.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn finish(&self, dns: &DnsPlan) -> Result<(), TunnelError> {
        self.backend.finishes.lock().unwrap().push(FinishCall {
            dns: dns.clone(),
            resolver_listening: resolver_listening(dns),
        });
        if self.backend.finish_fails.load(Ordering::SeqCst) {
            return Err(TunnelError("finish refused".into()));
        }
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
