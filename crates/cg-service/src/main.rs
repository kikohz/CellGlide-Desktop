// The modules are written for Linux (and, under windows/, Windows); macOS only compiles them
// for development, and the Windows entry point is still to come.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod access;
#[cfg(unix)]
mod auth;
#[cfg(unix)]
mod diagnostics;
#[cfg(unix)]
mod doctor;
#[cfg(unix)]
mod resume_check;
mod server;
#[cfg(unix)]
mod sleep;
#[cfg(windows)]
mod windows;

/// CAP_NET_ADMIN in the effective set, from /proc/self/status (bit 12 of CapEff).
#[cfg(target_os = "linux")]
fn has_net_admin() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("CapEff:"))
                .map(str::to_owned)
        })
        .and_then(|l| u64::from_str_radix(l["CapEff:".len()..].trim(), 16).ok())
        .is_some_and(|caps| caps & (1 << 12) != 0)
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use cg_core::controller::{Controller, ControllerConfig, SettingsSource};
    use cg_core::mux::MuxEndpoint;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::signal::unix::{SignalKind, signal};

    struct SystemDoctor(MuxEndpoint);
    impl server::DoctorApi for SystemDoctor {
        fn run(&self) -> server::BoxFuture<'_, Vec<cg_ipc::Check>> {
            Box::pin(doctor::checks(&self.0, Path::new("/")))
        }
    }

    // Run as root by the systemd-sleep hook, not by the service unit: no capability check.
    if std::env::args().nth(1).as_deref() == Some("resume-check") {
        resume_check::main().await;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    anyhow::ensure!(
        has_net_admin(),
        "cellglide-service needs CAP_NET_ADMIN; start it through cellglide.service"
    );

    cg_platform::bypass::cleanup_leftovers();
    let mut config =
        ControllerConfig::new(MuxEndpoint::system(), Arc::new(cg_platform::LinuxBackend));
    config.computer_name = nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok());
    config.settings = SettingsSource::File("/var/lib/cellglide/settings.json".into());
    config.sysfs_root = Some("/".into());
    config.health_dir = Some("/var/lib/cellglide/health".into());
    let mut sources = diagnostics::Sources::system();
    sources.health = Some(cg_core::health::log::HealthLog::new(
        "/var/lib/cellglide/health".into(),
        config.time_zone.clone(),
    ));
    let controller = Controller::spawn(config);

    let path = Path::new(cg_ipc::SOCKET_PATH);
    let listener = server::bind(path, cg_ipc::GROUP)?;
    let serving = server::serve(
        server::SocketListener::new(listener, auth::Policy::system(cg_ipc::GROUP)),
        controller.clone(),
        Arc::new(SystemDoctor(MuxEndpoint::system())),
        Arc::new(sources),
    );
    // logind's suspend and resume reset the health judgement; failing to listen is logged
    // there and never stops the service.
    tokio::spawn(sleep::watch(controller.clone()));
    let mut terminate = signal(SignalKind::terminate())?;
    tracing::info!("cellglide-service ready on {}", path.display());
    tokio::select! {
        _ = serving => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    controller.shutdown().await;
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("cellglide-service only runs on Linux")
}
