use cg_ipc::Reason;
use cg_protocol::ProtocolError;

/// Display text is for logs only; users see `reason()` rendered through cg-i18n.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Protocol(#[from] ProtocolError),
    #[error("timed out waiting for the phone")]
    Timeout,
    #[error("usbmuxd not reachable")]
    NoUsbService,
    #[error("phone service not ready")]
    PhoneNotReady,
    #[error("phone app too old")]
    PhoneTooOld,
    #[error("phone session changed")]
    SessionChanged,
    #[error("no way out through the phone")]
    NoExit,
    #[error("selected phone disconnected")]
    PhoneGone,
    #[error("local DNS port {0} in use")]
    ResolverPortBusy(u16),
    #[error("tunnel: {0}")]
    Tunnel(String),
    #[error("service shutting down")]
    ShuttingDown,
    /// Lockdown or the diagnostics relay said no; text for logs only.
    #[error("lockdown: {0}")]
    Lockdown(String),
}

impl CoreError {
    pub fn reason(&self) -> Reason {
        match self {
            CoreError::Io(e) => Reason::Internal {
                detail: e.to_string(),
            },
            CoreError::Protocol(ProtocolError::Incompatible) => Reason::PhoneTooOld,
            CoreError::Protocol(e) => Reason::Internal {
                detail: e.to_string(),
            },
            CoreError::Timeout => Reason::Timeout,
            CoreError::NoUsbService => Reason::NoUsbService,
            CoreError::PhoneNotReady => Reason::PhoneNotReady,
            CoreError::PhoneTooOld => Reason::PhoneTooOld,
            CoreError::SessionChanged => Reason::SessionChanged,
            CoreError::NoExit => Reason::NoExit,
            CoreError::PhoneGone => Reason::PhoneGone,
            CoreError::ResolverPortBusy(port) => Reason::ResolverPortBusy { port: *port },
            CoreError::Tunnel(detail) => Reason::Tunnel {
                detail: detail.clone(),
            },
            CoreError::ShuttingDown => Reason::ShuttingDown,
            CoreError::Lockdown(detail) => Reason::Internal {
                detail: format!("lockdown: {detail}"),
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
