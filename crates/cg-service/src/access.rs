//! What a client of the control socket (or pipe) may ask for, decided per connection by the
//! transport and applied per request by the server.

use cg_ipc::Request;

/// How far a connected client is trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Turned away: one error line, then the connection closes.
    Denied,
    /// May look (status, history, doctor) but not change anything or read diagnostics.
    /// Only the Windows pipe grants it; the Unix socket is all or nothing.
    #[cfg_attr(not(windows), allow(dead_code))]
    ReadOnly,
    Full,
}

/// Who is on the other end, as far as diagnostics need to know whose names to hide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requester {
    Uid(u32),
    /// Windows only.
    #[cfg_attr(not(windows), allow(dead_code))]
    Sid(String),
    Unknown,
}

/// A connected client, as its transport identified it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub access: Access,
    pub requester: Requester,
}

/// Requests that change the connection or settings, or reveal the diagnostics bundle.
/// Every variant is listed so a new request has to be classified here.
pub fn needs_full(request: &Request) -> bool {
    match request {
        Request::Connect { .. }
        | Request::Disconnect
        | Request::SetSettings { .. }
        | Request::Diagnostics { .. } => true,
        Request::Hello
        | Request::Status
        | Request::Doctor
        | Request::Subscribe
        | Request::History { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::DiagRange;

    #[test]
    fn classifies_requests() {
        assert!(needs_full(&Request::Connect { serial: None }));
        assert!(needs_full(&Request::Disconnect));
        assert!(needs_full(&Request::SetSettings {
            auto_connect: None,
            phone: None
        }));
        assert!(needs_full(&Request::Diagnostics {
            range: DiagRange::Day,
            contact: None
        }));
        assert!(!needs_full(&Request::Hello));
        assert!(!needs_full(&Request::Status));
        assert!(!needs_full(&Request::Doctor));
        assert!(!needs_full(&Request::Subscribe));
        assert!(!needs_full(&Request::History { day: None }));
    }
}
