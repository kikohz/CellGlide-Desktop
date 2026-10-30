//! Running under the service control manager: the `CellGlide` service's entry point, its
//! control handler (stop, shutdown and power events) and the states it reports.

use std::ffi::OsString;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use windows_service::service::{
    PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "CellGlide";

/// Hands this thread to the service control manager until the service stops. Fails when
/// the process wasn't started by it (run with `--console` instead).
pub fn start() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let _log = super::logs::init(&super::data_dir().join("logs"), false);
    let stop = CancellationToken::new();
    let (power, events) = mpsc::channel(8);
    let handler = {
        let stop = stop.clone();
        move |control| on_control(control, &stop, &power)
    };
    let status = match service_control_handler::register(SERVICE_NAME, handler) {
        Ok(status) => status,
        Err(error) => {
            tracing::error!("can't register the service control handler: {error}");
            return;
        }
    };
    let report = |state: ServiceState, exit_code: u32, wait: Duration| {
        let accepted = if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::POWER_EVENT
        } else {
            ServiceControlAccept::empty()
        };
        let set = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code: if exit_code == 0 {
                ServiceExitCode::Win32(0)
            } else {
                ServiceExitCode::ServiceSpecific(exit_code)
            },
            checkpoint: 0,
            wait_hint: wait,
            process_id: None,
        });
        if let Err(error) = set {
            tracing::warn!("can't report {state:?} to the service control manager: {error}");
        }
    };

    report(ServiceState::StartPending, 0, Duration::from_secs(10));
    let code = match tokio::runtime::Runtime::new() {
        Ok(runtime) => {
            report(ServiceState::Running, 0, Duration::ZERO);
            let result = runtime.block_on(super::run(stop, events));
            report(ServiceState::StopPending, 0, Duration::from_secs(10));
            match result {
                Ok(()) => 0,
                Err(error) => {
                    tracing::error!("cellglide-service stopped: {error:#}");
                    1
                }
            }
        }
        Err(error) => {
            tracing::error!("can't start the async runtime: {error}");
            1
        }
    };
    report(ServiceState::Stopped, code, Duration::ZERO);
}

/// What the service does with one control: stop and shutdown end it; suspend and the
/// automatic resume that follows every wake are passed on as `true` and `false`.
fn on_control(
    control: ServiceControl,
    stop: &CancellationToken,
    power: &mpsc::Sender<bool>,
) -> ServiceControlHandlerResult {
    match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop.cancel();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(event) => {
            let sleeping = match event {
                PowerEventParam::Suspend => Some(true),
                PowerEventParam::ResumeAutomatic => Some(false),
                _ => None,
            };
            // The handler runs on the control manager's thread and must not block: a full
            // queue drops the event, and the wall clock check still sees the gap.
            if let Some(sleeping) = sleeping
                && power.try_send(sleeping).is_err()
            {
                tracing::warn!("power event dropped (sleeping: {sleeping})");
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_and_shutdown_cancel() {
        for control in [ServiceControl::Stop, ServiceControl::Shutdown] {
            let stop = CancellationToken::new();
            let (power, _events) = mpsc::channel(8);
            assert!(matches!(
                on_control(control, &stop, &power),
                ServiceControlHandlerResult::NoError
            ));
            assert!(stop.is_cancelled());
        }
    }

    #[test]
    fn suspend_and_resume_become_sleep_flags() {
        let stop = CancellationToken::new();
        let (power, mut events) = mpsc::channel(8);
        for event in [
            PowerEventParam::Suspend,
            PowerEventParam::PowerStatusChange,
            PowerEventParam::ResumeAutomatic,
            PowerEventParam::ResumeSuspend,
        ] {
            assert!(matches!(
                on_control(ServiceControl::PowerEvent(event), &stop, &power),
                ServiceControlHandlerResult::NoError
            ));
        }
        assert_eq!(events.try_recv(), Ok(true));
        assert_eq!(events.try_recv(), Ok(false));
        assert!(
            events.try_recv().is_err(),
            "only suspend and automatic resume"
        );
        assert!(!stop.is_cancelled());
    }

    #[test]
    fn a_full_queue_never_blocks_the_handler() {
        let stop = CancellationToken::new();
        let (power, _events) = mpsc::channel(1);
        for _ in 0..3 {
            on_control(
                ServiceControl::PowerEvent(PowerEventParam::Suspend),
                &stop,
                &power,
            );
        }
        assert!(matches!(
            on_control(ServiceControl::Interrogate, &stop, &power),
            ServiceControlHandlerResult::NoError
        ));
        assert!(matches!(
            on_control(ServiceControl::Pause, &stop, &power),
            ServiceControlHandlerResult::NotImplemented
        ));
    }
}
