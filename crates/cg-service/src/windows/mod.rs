//! The Windows service: its entry points (under the service control manager, or in the
//! foreground with `--console`), the control pipe and who may use it, its log, power
//! events and environment checks.

pub mod auth;
pub mod doctor;
pub mod logs;
pub mod pipe;
pub mod power;
pub mod scm;
pub mod system;

use crate::diagnostics::{LogSource, Sources};
use crate::server;
use anyhow::Context;
use cg_core::controller::{Controller, ControllerConfig, SettingsSource};
use cg_core::mux::MuxEndpoint;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long the resolver waits for the phone before answering SERVFAIL: before Windows's
/// DNS client gives up on our adapter and asks the others (W0 measurement, Task 5).
const RESOLVER_DEADLINE: Duration = Duration::from_millis(2800);

/// `%ProgramData%\CellGlide`: settings, the timeline and the log.
pub fn data_dir() -> PathBuf {
    system::program_data()
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("CellGlide")
}

/// Where `cellglide-service.exe` (and `wintun.dll` beside it) is installed.
fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default()
}

struct SystemDoctor {
    endpoint: MuxEndpoint,
    install_dir: PathBuf,
}

impl server::DoctorApi for SystemDoctor {
    fn run(&self) -> server::BoxFuture<'_, Vec<cg_ipc::Check>> {
        Box::pin(doctor::checks(&self.endpoint, &self.install_dir))
    }
}

/// The service until `stop`: the controller behind the control pipe, with sleep signals
/// from `power` passed on. `standby` is the sender for Modern Standby records, held by
/// their subscription (renewed with backoff when it fails) until `stop`. Shared by the
/// service and `--console`.
pub async fn run(
    stop: CancellationToken,
    standby: mpsc::Sender<power::Signal>,
    power: mpsc::Receiver<power::Signal>,
) -> anyhow::Result<()> {
    if let Err(error) = cg_platform::cleanup_leftovers() {
        tracing::warn!("clearing leftovers from a previous run: {error}");
    }
    let data = data_dir();
    // wintun.dll loads on the first connect: without it the pipe and doctor still run, and
    // connecting says what is missing.
    let backend = cg_platform::WindowsBackend::new();
    let mut config = ControllerConfig::new(MuxEndpoint::system(), Arc::new(backend));
    config.resolver_addr = cg_core::tunnel::WINDOWS_RESOLVER;
    config.resolver_deadline = RESOLVER_DEADLINE;
    config.bypass = false;
    config.sysfs_root = None;
    config.settings = SettingsSource::File(data.join("settings.json"));
    config.health_dir = Some(data.join("health"));
    config.computer_name = system::computer_name();
    let sources = Sources {
        log: LogSource::Files(data.join("logs")),
        root: PathBuf::new(),
        health: Some(cg_core::health::log::HealthLog::new(
            data.join("health"),
            config.time_zone.clone(),
        )),
        hostname: config.computer_name.clone(),
        netbios: system::netbios_name(),
    };
    let listener = pipe::bind(cg_ipc::PIPE_NAME)
        .with_context(|| format!("can't serve {}", cg_ipc::PIPE_NAME))?;
    let controller = Controller::spawn(config);
    let serving = server::serve(
        listener,
        controller.clone(),
        Arc::new(SystemDoctor {
            endpoint: MuxEndpoint::system(),
            install_dir: install_dir(),
        }),
        Arc::new(sources),
    );
    tokio::spawn(power::watch(controller.clone(), power));
    let unsubscribe = stop.child_token();
    let watching = tokio::spawn(power::keep_subscribed(
        move || power::Standby::subscribe(standby.clone()),
        unsubscribe.clone(),
        cg_core::retry::backoff,
    ));
    tracing::info!("cellglide-service ready on {}", cg_ipc::PIPE_NAME);
    tokio::select! {
        _ = serving => {}
        _ = stop.cancelled() => {}
    }
    tracing::info!("cellglide-service stopping");
    unsubscribe.cancel();
    if let Err(error) = watching.await {
        tracing::warn!("the Modern Standby watch ended abnormally: {error}");
    }
    controller.shutdown().await;
    Ok(())
}

/// `--console`: the service in the foreground, logging to the terminal as well, until
/// Ctrl+C. No power events arrive here; Modern Standby and the wall clock check still do.
pub fn console() -> anyhow::Result<()> {
    let _log = logs::init(&data_dir().join("logs"), true);
    // The pipe is owned by SYSTEM, as the service's is; an administrator may set that only
    // with SeRestorePrivilege enabled.
    if !pipe::enable_restore_privilege() {
        tracing::warn!("SeRestorePrivilege unavailable: run --console as an administrator");
    }
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let stop = CancellationToken::new();
        tokio::spawn({
            let stop = stop.clone();
            async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    stop.cancel();
                }
            }
        });
        // Only Modern Standby records and the clock: no power events arrive here.
        let (standby, power) = mpsc::channel(32);
        let result = run(stop, standby, power).await;
        if let Err(error) = &result {
            tracing::error!("cellglide-service stopped: {error:#}");
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::test_sources;
    use cg_core::settings::Stored;
    use cg_core::testing::{
        FakeMux, FakePhone, FakePhoneOptions, FakeProbe, FakeTunnelProbe, SharedProbe, device,
    };
    use cg_ipc::{CheckId, ConnState, Endpoint, Reason, Request, Response, client};

    /// Without a loadable wintun.dll (debug builds take its path from CELLGLIDE_WINTUN_DLL)
    /// the service is still served as `run` serves it: doctor's Wintun check fails, and
    /// connecting fails with a reason that says where wintun.dll belongs.
    #[tokio::test]
    async fn serves_without_wintun() {
        // SAFETY: Windows serializes access to the environment block; no other test in this
        // binary reads or writes this variable.
        unsafe {
            std::env::set_var(
                "CELLGLIDE_WINTUN_DLL",
                r"C:\nonexistent\cellglide\wintun.dll",
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        let mux = FakeMux::start().await;
        let _phone = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let mut config =
            ControllerConfig::new(mux.endpoint(), Arc::new(cg_platform::WindowsBackend::new()));
        config.resolver_addr = cg_core::tunnel::WINDOWS_RESOLVER;
        config.bypass = false;
        config.sysfs_root = None;
        config.settings = SettingsSource::Memory(Stored {
            auto_connect: false,
            ..Stored::default()
        });
        config.health_dir = Some(dir.path().join("health"));
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.tunnel_probe = FakeTunnelProbe::new();
        let controller = Controller::spawn(config);
        let name = pipe::tests::random_pipe_name();
        let listener = pipe::bind_with(&name, &pipe::tests::test_sddl()).unwrap();
        tokio::spawn(server::serve(
            listener,
            controller.clone(),
            Arc::new(SystemDoctor {
                endpoint: mux.endpoint(),
                install_dir: install.path().to_path_buf(),
            }),
            Arc::new(test_sources(dir.path())),
        ));
        let endpoint = Endpoint::Pipe(name);

        let Response::Doctor { checks } =
            client::request(&endpoint, &Request::Doctor).await.unwrap()
        else {
            panic!("expected doctor's checks");
        };
        let wintun = checks.iter().find(|c| c.id == CheckId::Wintun).unwrap();
        assert!(!wintun.ok, "{checks:?}");

        mux.attach(device("aaaa", 1));
        let mut updates = controller.subscribe();
        let connect = client::request(&endpoint, &Request::Connect { serial: None });
        assert_eq!(connect.await.unwrap(), Response::Ok);
        let state = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let state = updates.borrow_and_update().state.clone();
                if let ConnState::Retrying { .. } = state {
                    return state;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("connecting never failed");
        let ConnState::Retrying {
            reason: Reason::Tunnel { detail },
        } = state
        else {
            panic!("expected a tunnel error, got {state:?}");
        };
        assert!(detail.contains("wintun.dll"), "{detail}");
        assert!(
            detail.contains("same directory as cellglide-service.exe"),
            "{detail}"
        );
        controller.shutdown().await;
    }
}
