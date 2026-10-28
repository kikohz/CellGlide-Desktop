use crate::bypass::Bypass;
use crate::hev::Hev;
use crate::resolved::ApplyError;
use crate::{ip, resolved, tun};
use cg_core::tunnel::{
    INTERFACE_NAME, RunningTunnel, TunnelBackend, TunnelError, TunnelNotice, TunnelPlan, hev_config,
};
use std::os::fd::{AsRawFd, OwnedFd};

/// /dev/net/tun + ip + systemd-resolved + hev.
pub struct LinuxBackend;

struct Running {
    index: i32,
    hev: Option<Hev>,
    bypass: Option<Bypass>,
    /// False when resolved refused to let this service set DNS.
    dns_managed: bool,
    /// Dropped last: closing it removes the interface and its routes.
    fd: Option<OwnedFd>,
}

impl TunnelBackend for LinuxBackend {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError> {
        let fd = tun::open(INTERFACE_NAME).map_err(|e| match e.raw_os_error() {
            Some(libc::EBUSY) => TunnelError(format!(
                "{INTERFACE_NAME} is busy; is another CellGlide running?"
            )),
            _ => TunnelError(format!("can't create {INTERFACE_NAME}: {e}")),
        })?;
        // From here on an early return drops `fd`, which undoes the interface and routes.
        for command in ip::link_commands(INTERFACE_NAME) {
            ip::run(&command)?;
        }
        if let Err(error) = ip::ipv6_commands(INTERFACE_NAME)
            .iter()
            .try_for_each(|c| ip::run(c))
        {
            tracing::warn!("IPv6 not set up on {INTERFACE_NAME}: {error}");
        }
        for command in ip::route_commands(INTERFACE_NAME) {
            ip::run(&command)?;
        }
        let index = tun::index(INTERFACE_NAME)
            .map_err(|e| TunnelError(format!("{INTERFACE_NAME} not found: {e}")))?;
        let bypass = plan.bypass.then(|| Bypass::start(INTERFACE_NAME));
        let hev = match Hev::spawn(hev_config(plan.socks_port), fd.as_raw_fd()) {
            Ok(hev) => hev,
            Err(error) => {
                if let Some(bypass) = bypass {
                    bypass.stop();
                }
                return Err(error);
            }
        };
        let dns_managed = match resolved::apply(index, &plan.dns) {
            Ok(()) => true,
            Err(ApplyError::NotAuthorized(detail)) => {
                tracing::warn!(
                    "systemd-resolved won't let this service set DNS ({detail}); running without managed DNS"
                );
                let _ = resolved::revert(index);
                false
            }
            Err(ApplyError::Unavailable(detail)) => {
                tracing::warn!(
                    "systemd-resolved isn't running or can't be reached ({detail}); running without managed DNS, lookups keep using the system's DNS"
                );
                let _ = resolved::revert(index);
                false
            }
            Err(ApplyError::Failed(error)) => {
                let _ = resolved::revert(index);
                hev.stop();
                if let Some(bypass) = bypass {
                    bypass.stop();
                }
                return Err(error);
            }
        };
        Ok(Box::new(Running {
            index,
            hev: Some(hev),
            bypass,
            dns_managed,
            fd: Some(fd),
        }))
    }
}

impl RunningTunnel for Running {
    fn stop(mut self: Box<Self>) -> Result<(), TunnelError> {
        let reverted = if self.dns_managed {
            resolved::revert(self.index)
        } else {
            Ok(())
        };
        if let Some(hev) = self.hev.take() {
            hev.stop();
        }
        // The network goes back to the original interface first, then its bypass goes.
        drop(self.fd.take());
        if let Some(bypass) = self.bypass.take() {
            bypass.stop();
        }
        reverted
    }

    fn has_exited(&self) -> bool {
        self.hev.as_ref().is_none_or(Hev::has_exited)
    }

    fn dns_managed(&self) -> bool {
        self.dns_managed
    }

    fn take_notices(&self) -> Vec<TunnelNotice> {
        self.bypass
            .as_ref()
            .map(Bypass::take_notices)
            .unwrap_or_default()
    }
}
