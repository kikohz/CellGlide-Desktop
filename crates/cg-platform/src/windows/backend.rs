//! wintun + IP Helper + per-interface DNS + cg-netstack behind `TunnelBackend`.
//!
//! `start` brings up cellglide0 and its addresses only; the controller then binds the
//! resolver on 198.18.0.1 and calls `finish`, which sets the DNS, adds the routes, turns on
//! strict DNS and starts the engine. `stop` (or dropping the tunnel without it) undoes
//! everything in WireGuard's order, strict DNS first. A crash needs nothing: the adapter,
//! its addresses, routes and DNS go with the process's wintun handle, and the strict DNS
//! rules with its dynamic WFP session.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use cg_core::tunnel::{
    ADDRESS_V4, ADDRESS_V6, DnsPlan, ENGINE, INTERFACE_NAME, IPV4_ROUTES, IPV6_ROUTES,
    ResolverOrder, RunningTunnel, TunnelBackend, TunnelError, TunnelPlan, WINDOWS_ADAPTER_GUID,
    WINDOWS_TUNNEL_TYPE,
};
use tokio_util::sync::CancellationToken;

use super::device::{self, JoinHandles};
use super::dns;
use super::ip::{self, Family};
use super::strict_dns::{self, StrictDns};
use super::wintun::{Adapter, SESSION_CAPACITY, Session, Wintun};

/// Engine worker threads; the engine is one poll loop plus a task per flow.
const ENGINE_WORKERS: usize = 2;
/// How long the engine's runtime waits for stray tasks once `run` has returned.
const ENGINE_SHUTDOWN: Duration = Duration::from_secs(1);

/// The Windows tunnel. Holds the loaded `wintun.dll`.
pub struct WindowsBackend {
    dll: Arc<Wintun>,
}

impl WindowsBackend {
    /// Loads `wintun.dll` (next to the executable, or System32).
    pub fn new() -> io::Result<WindowsBackend> {
        Ok(WindowsBackend {
            dll: Wintun::load()?,
        })
    }
}

impl TunnelBackend for WindowsBackend {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError> {
        // `plan.bypass` adds nothing on Windows (spec 5.4): local subnets' on-link routes
        // are already longer than /1 and stay on the physical adapter.
        let adapter = Adapter::create(
            &self.dll,
            INTERFACE_NAME,
            WINDOWS_TUNNEL_TYPE,
            WINDOWS_ADAPTER_GUID,
        )
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => TunnelError(format!(
                "{INTERFACE_NAME} already exists; is another CellGlide running?"
            )),
            _ => TunnelError(format!("can't create {INTERFACE_NAME}: {e}")),
        })?;
        // From here on an early return drops `adapter`, which removes it with everything
        // configured on it.
        let luid = adapter.luid();
        ip::configure_interface(luid, Family::V4)
            .and_then(|()| ip::add_address(luid, ADDRESS_V4.into(), 32))
            .map_err(|e| TunnelError(format!("can't set up IPv4 on {INTERFACE_NAME}: {e}")))?;
        let ipv6 = match ip::configure_interface(luid, Family::V6)
            .and_then(|()| ip::add_address(luid, ADDRESS_V6.into(), 128))
        {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!("IPv6 not set up on {INTERFACE_NAME}: {error}");
                false
            }
        };
        Ok(Box::new(Running {
            luid,
            ipv6,
            socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), plan.socks_port),
            inner: Mutex::new(Inner {
                strict_dns: None,
                engine: None,
                session: None,
                dns_managed: true,
                adapter: Some(adapter),
            }),
        }))
    }

    fn resolver_order(&self) -> ResolverOrder {
        ResolverOrder::AfterInterface
    }
}

/// The DNS servers for our adapter: the resolver's address, or the public servers without
/// the IPv6 ones when IPv6 isn't up on cellglide0 (they would be unreachable).
fn dns_servers(plan: &DnsPlan, ipv6: bool) -> Vec<IpAddr> {
    match plan {
        DnsPlan::Resolver(resolver) => vec![resolver.ip()],
        DnsPlan::Servers(servers) => servers
            .iter()
            .copied()
            .filter(|server| ipv6 || server.is_ipv4())
            .collect(),
    }
}

struct Running {
    luid: u64,
    ipv6: bool,
    socks: SocketAddr,
    /// `finish` and `stop` take `&self` and `Box<Self>`; the controller holds the tunnel
    /// on one thread at a time, so the lock is never contended.
    inner: Mutex<Inner>,
}

struct Inner {
    /// Strict DNS (a dynamic WFP session blocking port 53 off cellglide0), set once
    /// `finish` has set the DNS and routes; `None` when it could not be set up. Teardown
    /// drops it first.
    strict_dns: Option<StrictDns>,
    engine: Option<Engine>,
    /// Ends after the DNS, routes and addresses are gone, before the adapter closes.
    session: Option<Arc<Session>>,
    /// False when `SetInterfaceDnsSettings` refused our servers.
    dns_managed: bool,
    /// `None` once torn down.
    adapter: Option<Adapter>,
}

/// cg-netstack on its own runtime thread, fed by the two bridge threads.
struct Engine {
    stop: CancellationToken,
    thread: JoinHandle<()>,
    bridge: JoinHandles,
}

impl Engine {
    fn start(session: &Arc<Session>, socks: SocketAddr) -> io::Result<Engine> {
        let config = cg_netstack::Config {
            mtu: u16::try_from(ENGINE.mtu)
                .map_err(|_| io::Error::other("MTU does not fit in u16"))?,
            socks,
            tcp_timeout: ENGINE.tcp_timeout,
            udp_idle: ENGINE.udp_idle,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(ENGINE_WORKERS)
            .thread_name("cg-netstack-worker")
            .enable_all()
            .build()?;
        let stop = CancellationToken::new();
        let (channel, bridge) = device::bridge(Arc::clone(session), stop.clone())?;
        let engine_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("cg-netstack".into())
            .spawn(move || {
                match runtime.block_on(cg_netstack::run(channel, config, engine_stop.clone())) {
                    Ok(()) if engine_stop.is_cancelled() => {}
                    Ok(()) => tracing::warn!("the forwarding engine stopped on its own"),
                    Err(error) => tracing::warn!("the forwarding engine failed: {error}"),
                }
                // Whatever ended the engine, the bridge threads go too.
                engine_stop.cancel();
                runtime.shutdown_timeout(ENGINE_SHUTDOWN);
            });
        match thread {
            Ok(thread) => Ok(Engine {
                stop,
                thread,
                bridge,
            }),
            Err(error) => {
                stop.cancel();
                bridge.join();
                Err(error)
            }
        }
    }

    fn has_exited(&self) -> bool {
        self.thread.is_finished()
    }

    fn stop(self) {
        self.stop.cancel();
        let _ = self.thread.join();
        self.bridge.join();
    }
}

impl Running {
    /// `finish`'s steps; whatever it set up is in `inner` when it fails.
    fn bring_up(&self, inner: &mut Inner, dns: &DnsPlan) -> Result<(), TunnelError> {
        let servers = dns_servers(dns, self.ipv6);
        if let Err(error) = dns::set_dns(WINDOWS_ADAPTER_GUID, &servers) {
            tracing::warn!(
                "can't set {INTERFACE_NAME}'s DNS servers ({error}); running without managed DNS"
            );
            inner.dns_managed = false;
            let _ = dns::clear_dns(WINDOWS_ADAPTER_GUID);
        }
        let routes = IPV4_ROUTES
            .iter()
            .chain(if self.ipv6 { &IPV6_ROUTES[..] } else { &[] });
        for route in routes {
            ip::add_route(self.luid, route)
                .map_err(|e| TunnelError(format!("can't add route {route}: {e}")))?;
        }
        self.start_strict_dns(inner);
        let adapter = inner
            .adapter
            .as_ref()
            .ok_or_else(|| TunnelError(format!("{INTERFACE_NAME} is already stopped")))?;
        let session =
            Arc::new(adapter.start_session(SESSION_CAPACITY).map_err(|e| {
                TunnelError(format!("can't start a session on {INTERFACE_NAME}: {e}"))
            })?);
        inner.session = Some(Arc::clone(&session));
        inner.engine = Some(
            Engine::start(&session, self.socks)
                .map_err(|e| TunnelError(format!("can't start the forwarding engine: {e}")))?,
        );
        Ok(())
    }

    /// Blocks port 53 on the physical adapters for as long as `inner.strict_dns` lives
    /// (other VPNs keep theirs; see strict_dns.rs). A failure leaves DNS working through
    /// cellglide0, but Windows may also ask the physical adapters' servers. Skipped when our DNS servers weren't applied: Windows then uses
    /// the physical adapters' servers, and while an off-link one goes over the /1 routes
    /// through cellglide0 (permitted), an on-link one, typically the router, would be
    /// blocked and resolution would stop.
    fn start_strict_dns(&self, inner: &mut Inner) {
        if !inner.dns_managed {
            tracing::warn!("strict DNS off: {INTERFACE_NAME}'s DNS servers aren't managed");
            return;
        }
        match strict_dns::start(self.luid) {
            Ok(strict) => {
                tracing::info!(
                    "strict DNS on: port 53 blocked on physical adapters ({} virtual exempt)",
                    strict.exempt_luids().len()
                );
                inner.strict_dns = Some(strict);
            }
            Err(error) => tracing::warn!(
                "strict DNS off ({error}); DNS queries may also go to other adapters' servers"
            ),
        }
    }

    /// Undoes `finish` after one of its steps failed, leaving what `start` did.
    fn undo_finish(&self, inner: &mut Inner) {
        drop(inner.strict_dns.take());
        if let Some(engine) = inner.engine.take() {
            engine.stop();
        }
        if let Err(error) = ip::flush_routes(self.luid) {
            tracing::warn!("removing {INTERFACE_NAME}'s routes: {error}");
        }
        if let Err(error) = dns::clear_dns(WINDOWS_ADAPTER_GUID) {
            tracing::warn!("clearing {INTERFACE_NAME}'s DNS servers: {error}");
        }
        inner.dns_managed = true;
        drop(inner.session.take());
    }

    /// Engine, DNS, routes and addresses, session, adapter. Idempotent; returns the first
    /// error and carries on past it.
    fn teardown(&mut self) -> Result<(), TunnelError> {
        let luid = self.luid;
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        let Some(adapter) = inner.adapter.take() else {
            return Ok(());
        };
        drop(inner.strict_dns.take());
        if let Some(engine) = inner.engine.take() {
            engine.stop();
        }
        let mut first = None;
        // Cleared even if `finish` never ran or the system refused our servers: nothing
        // else sets DNS on our GUID. A refusal is only an error if setting them worked.
        if let Err(error) = dns::clear_dns(WINDOWS_ADAPTER_GUID) {
            if inner.dns_managed {
                first.get_or_insert(TunnelError(format!(
                    "can't clear {INTERFACE_NAME}'s DNS servers: {error}"
                )));
            } else {
                tracing::debug!("clearing {INTERFACE_NAME}'s DNS servers: {error}");
            }
        }
        if let Err(error) = ip::flush(luid) {
            first.get_or_insert(TunnelError(format!(
                "can't remove {INTERFACE_NAME}'s routes and addresses: {error}"
            )));
        }
        drop(inner.session.take());
        drop(adapter);
        first.map_or(Ok(()), Err)
    }
}

impl RunningTunnel for Running {
    fn stop(mut self: Box<Self>) -> Result<(), TunnelError> {
        self.teardown()
    }

    fn finish(&self, dns: &DnsPlan) -> Result<(), TunnelError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.engine.is_some() || inner.session.is_some() {
            return Err(TunnelError("the tunnel is already finished".into()));
        }
        let result = self.bring_up(&mut inner, dns);
        if result.is_err() {
            self.undo_finish(&mut inner);
        }
        result
    }

    fn has_exited(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.engine.as_ref().is_some_and(Engine::has_exited)
    }

    fn dns_managed(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dns_managed
    }

    fn strict_dns(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .strict_dns
            .is_some()
    }
}

impl Drop for Running {
    /// A tunnel dropped without `stop` (the controller's `finish` thread panicked, say)
    /// still leaves nothing behind.
    fn drop(&mut self) {
        let live = self
            .inner
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .adapter
            .is_some();
        if live {
            tracing::warn!("{INTERFACE_NAME} dropped without stop; cleaning up");
            if let Err(error) = self.teardown() {
                tracing::warn!("{error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::tunnel::{FALLBACK_DNS, WINDOWS_RESOLVER};

    #[test]
    fn dns_addresses_drop_ipv6_when_ipv6_failed() {
        let resolver = DnsPlan::Resolver(WINDOWS_RESOLVER);
        assert_eq!(dns_servers(&resolver, true), [IpAddr::from(ADDRESS_V4)]);
        assert_eq!(dns_servers(&resolver, false), [IpAddr::from(ADDRESS_V4)]);
        let public = DnsPlan::Servers(FALLBACK_DNS.to_vec());
        assert_eq!(dns_servers(&public, true), FALLBACK_DNS);
        assert_eq!(
            dns_servers(&public, false),
            [IpAddr::from([1, 1, 1, 1]), IpAddr::from([8, 8, 8, 8])]
        );
    }
}
