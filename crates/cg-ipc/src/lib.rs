//! Messages between cellglide-service and its clients: one JSON object per line.
//!
//! Field names come in two styles on purpose. The socket's own messages (`Request`,
//! `Response`, `Snapshot`, `PhoneCard` and the rest of [`phone`]) are snake_case, as the
//! protocol has always been. The connection timeline in [`health`] (events, snapshots,
//! causes, event kinds) is camelCase because it is also the on-disk record, and matches the
//! Mac's `HealthModel.swift` so both platforms' logs and diagnostics bundles read the same
//! (ruling P13); so does [`Contact`], which goes into the bundle as is. Keep each type in
//! its family's style rather than converting one to the other.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

pub mod carrier;
pub mod describe;
pub mod health;
pub mod phone;

use phone::{PhoneCard, PhoneLabel};
use std::collections::BTreeMap;

pub mod client;
#[cfg(windows)]
pub mod pipe_identity;

pub const SOCKET_PATH: &str = "/run/cellglide/control.sock";
/// The service's control pipe on Windows; clients check it is owned by LocalSystem and served
/// from session 0 (see [`pipe_identity`]).
#[cfg(windows)]
pub const PIPE_NAME: &str = r"\\.\pipe\cellglide-control";
/// The control pipe's security descriptor: owned by LocalSystem (the owner is what clients
/// check), full access for SYSTEM and administrators, read/write for interactive users:
/// `0x12019b` is FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES,
/// i.e. GENERIC_WRITE without FILE_CREATE_PIPE_INSTANCE, so they can't add instances of it.
#[cfg(windows)]
pub const PIPE_SDDL: &str = "O:SYD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;IU)";

/// Where the service listens: a Unix socket, or a named pipe on Windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    #[cfg(windows)]
    Pipe(String),
}

impl Endpoint {
    /// The system service's endpoint: [`SOCKET_PATH`], or `PIPE_NAME` on Windows.
    pub fn system() -> Endpoint {
        #[cfg(unix)]
        return Endpoint::Unix(SOCKET_PATH.into());
        #[cfg(windows)]
        return Endpoint::Pipe(PIPE_NAME.into());
    }

    /// A socket path, or on Windows a pipe name (`\\.\pipe\name`, or just `name`).
    pub fn parse(text: &str) -> Endpoint {
        #[cfg(unix)]
        return Endpoint::Unix(text.into());
        #[cfg(windows)]
        return if text.starts_with(r"\\") {
            Endpoint::Pipe(text.into())
        } else {
            Endpoint::Pipe(format!(r"\\.\pipe\{text}"))
        };
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Endpoint::Unix(path) => write!(f, "{}", path.display()),
            #[cfg(windows)]
            Endpoint::Pipe(name) => f.write_str(name),
        }
    }
}
pub const GROUP: &str = "cellglide";
pub const MAX_LINE: usize = 64 * 1024;
/// Replies (history, diagnostics) can be far longer than requests.
pub const MAX_REPLY_LINE: usize = 4 * 1024 * 1024;
/// Bumped whenever a message changes shape; clients check it with `Request::Hello`.
pub const PROTOCOL_VERSION: u32 = 6;

/// `CellGlide-Linux/<version>` (`CellGlide-Windows/<version>` on Windows), sent as the
/// User-Agent of every request CellGlide makes, the service's probes included.
pub fn user_agent() -> String {
    #[cfg(windows)]
    let platform = "Windows";
    #[cfg(not(windows))]
    let platform = "Linux";
    format!("CellGlide-{platform}/{}", env!("CARGO_PKG_VERSION"))
}

/// What a user typed into the diagnostics dialog: their own words, sent on purpose.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Contact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How much of the timeline and journal a diagnostics bundle covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagRange {
    /// The last 24 hours.
    Day,
    /// The last seven days.
    Week,
}

/// Why something failed, as a code the client renders in the user's language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    NoUsbService,
    PhoneNotReady,
    PhoneTooOld,
    SessionChanged,
    NoExit,
    PhoneGone,
    ResolverPortBusy {
        port: u16,
    },
    ChosenPhoneMissing,
    ForwardingStopped,
    Timeout,
    Tunnel {
        detail: String,
    },
    Internal {
        detail: String,
    },
    NotInGroup,
    /// Windows: only an administrator or a user in an active session may do this.
    NotAllowed,
    BadRequest,
    ShuttingDown,
    /// `history` asked for a day that isn't `YYYY-MM-DD` or not among the last seven.
    BadDay,
}

#[cfg(test)]
impl Reason {
    /// One value of every variant (data-carrying ones with a sample); adding a variant fails to
    /// compile until the match below, and so the list, names it.
    pub(crate) fn all() -> Vec<Reason> {
        let all = vec![
            Reason::NoUsbService,
            Reason::PhoneNotReady,
            Reason::PhoneTooOld,
            Reason::SessionChanged,
            Reason::NoExit,
            Reason::PhoneGone,
            Reason::ResolverPortBusy { port: 1053 },
            Reason::ChosenPhoneMissing,
            Reason::ForwardingStopped,
            Reason::Timeout,
            Reason::Tunnel { detail: "x".into() },
            Reason::Internal { detail: "x".into() },
            Reason::NotInGroup,
            Reason::NotAllowed,
            Reason::BadRequest,
            Reason::ShuttingDown,
            Reason::BadDay,
        ];
        for reason in &all {
            match reason {
                Reason::NoUsbService
                | Reason::PhoneNotReady
                | Reason::PhoneTooOld
                | Reason::SessionChanged
                | Reason::NoExit
                | Reason::PhoneGone
                | Reason::ResolverPortBusy { .. }
                | Reason::ChosenPhoneMissing
                | Reason::ForwardingStopped
                | Reason::Timeout
                | Reason::Tunnel { .. }
                | Reason::Internal { .. }
                | Reason::NotInGroup
                | Reason::NotAllowed
                | Reason::BadRequest
                | Reason::ShuttingDown
                | Reason::BadDay => {}
            }
        }
        all
    }
}

impl Reason {
    /// The `cg-i18n` key for this reason.
    pub fn key(&self) -> &'static str {
        match self {
            Reason::NoUsbService => "reason.no_usb_service",
            Reason::PhoneNotReady => "reason.phone_not_ready",
            Reason::PhoneTooOld => "reason.phone_too_old",
            Reason::SessionChanged => "reason.session_changed",
            Reason::NoExit => "reason.no_exit",
            Reason::PhoneGone => "reason.phone_gone",
            Reason::ResolverPortBusy { .. } => "reason.resolver_port_busy",
            Reason::ChosenPhoneMissing => "reason.chosen_phone_missing",
            Reason::ForwardingStopped => "reason.forwarding_stopped",
            Reason::Timeout => "reason.timeout",
            Reason::Tunnel { .. } => "reason.tunnel",
            Reason::Internal { .. } => "reason.internal",
            Reason::NotInGroup => "reason.not_in_group",
            Reason::NotAllowed => "reason.not_allowed",
            Reason::BadRequest => "reason.bad_request",
            Reason::ShuttingDown => "reason.shutting_down",
            Reason::BadDay => "reason.bad_day",
        }
    }

    pub fn args(&self) -> Vec<(&'static str, String)> {
        match self {
            Reason::ResolverPortBusy { port } => vec![("port", port.to_string())],
            Reason::Tunnel { detail } | Reason::Internal { detail } => {
                vec![("detail", detail.clone())]
            }
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsMode {
    /// The phone's encrypted DNS through our local resolver.
    Phone,
    /// Public resolvers through the tunnel (the phone's CellGlide DNS is off).
    Public,
    /// The system wouldn't let CellGlide set DNS; lookups use the original network.
    Unmanaged,
}

/// Windows' strict DNS while connected: port 53 blocked on the other adapters so Windows
/// can't also ask their servers. Not applicable on other platforms, or while not connected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StrictDns {
    #[default]
    NotApplicable,
    On,
    /// It couldn't be turned on; DNS queries may also go to the other adapters' servers.
    Off {
        reason: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    /// usbmuxd is not reachable.
    NoUsbService,
    #[default]
    NoPhone,
    /// More than one iPhone is attached and none was chosen.
    ChoosePhone,
    /// A phone is attached; nobody asked to connect.
    Idle,
    Connecting,
    Connected {
        phone_version: String,
        dns: DnsMode,
    },
    /// Asked to connect; the last attempt failed and will be retried.
    Retrying {
        reason: Reason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub auto_connect: bool,
    pub chosen_serial: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_connect: true,
            chosen_serial: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PhoneChoice {
    Auto,
    Serial { serial: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub state: ConnState,
    /// Serials of attached iPhones.
    pub devices: Vec<String>,
    pub selected: Option<String>,
    pub wants_connection: bool,
    #[serde(default)]
    pub phone: Option<PhoneCard>,
    /// Names and models of known phones, by serial.
    #[serde(default)]
    pub labels: BTreeMap<String, PhoneLabel>,
    #[serde(default)]
    pub settings: Settings,
    /// The open issue, if any, and the last one that ended.
    #[serde(default)]
    pub health: health::HealthStatus,
    /// The local SOCKS port the tunnel uses, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks_port: Option<u16>,
    /// Strict DNS, while connected (`NotApplicable` otherwise and on Linux).
    #[serde(default)]
    pub strict_dns: StrictDns,
}

impl Snapshot {
    /// The address for apps that take a manual proxy: only while the tunnel runs.
    pub fn proxy_address(&self) -> Option<String> {
        self.socks_port
            .map(|port| format!("socks5h://127.0.0.1:{port}"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hello,
    Status,
    Connect {
        serial: Option<String>,
    },
    Disconnect,
    SetSettings {
        auto_connect: Option<bool>,
        phone: Option<PhoneChoice>,
    },
    Doctor,
    Subscribe,
    /// One day of the connection timeline; `day` is `YYYY-MM-DD`, `None` for today.
    History {
        #[serde(default)]
        day: Option<String>,
    },
    /// The service builds the redacted diagnostics bundle (JSON) for the requester to review and send.
    Diagnostics {
        range: DiagRange,
        #[serde(default)]
        contact: Option<Contact>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    Service,
    Group,
    Usbmuxd,
    Phone,
    Trust,
    Resolved,
    Polkit,
    PolkitRule,
    Tun,
    /// Windows: Apple's device service is installed and running.
    AppleService,
    /// Windows: wintun.dll is present and properly signed.
    Wintun,
    /// Windows: nothing else holds the resolver's port 53.
    Dns,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: CheckId,
    pub ok: bool,
    /// Arguments for the check's text, e.g. `count`, `serials`, `error`, `running`.
    #[serde(default)]
    pub args: std::collections::BTreeMap<String, String>,
}

impl Check {
    pub fn new(id: CheckId, ok: bool) -> Check {
        Check {
            id,
            ok,
            args: Default::default(),
        }
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Check {
        self.args.insert(name.into(), value.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// A status reply carries the whole phone card; replies are short-lived, so boxing buys nothing.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        version: String,
    },
    Status {
        snapshot: Snapshot,
    },
    Ok,
    Error {
        reason: Reason,
    },
    Doctor {
        checks: Vec<Check>,
    },
    History {
        day: health::HistoryDay,
    },
    /// The bundle as pretty JSON text, exactly what `cellglide diagnostics` shows and uploads.
    Diagnostics {
        json: String,
    },
}

pub fn to_line<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("IPC types always serialize");
    bytes.push(b'\n');
    bytes
}

pub fn from_line<T: DeserializeOwned>(line: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(line)
}

/// One line without its newline; `None` at a clean end of stream.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    read_line_limited(reader, MAX_LINE).await
}

/// Like [`read_line`] with a chosen limit: clients read replies with [`MAX_REPLY_LINE`].
pub async fn read_line_limited<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = reader
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long or truncated",
        ));
    }
    line.pop();
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_leaves_out_what_is_missing() {
        let both = Contact {
            email: Some("a@b.co".into()),
            note: Some("hi".into()),
        };
        assert_eq!(
            serde_json::to_string(&both).unwrap(),
            r#"{"email":"a@b.co","note":"hi"}"#
        );
        assert_eq!(serde_json::to_string(&Contact::default()).unwrap(), "{}");
        let parsed: Contact = serde_json::from_str(r#"{"note":"x"}"#).unwrap();
        assert_eq!(parsed.email, None);
        assert_eq!(parsed.note.as_deref(), Some("x"));
    }

    #[test]
    fn proxy_address_only_with_a_port() {
        let mut s = Snapshot::default();
        assert_eq!(s.proxy_address(), None);
        assert!(
            !String::from_utf8(to_line(&s))
                .unwrap()
                .contains("socks_port")
        );
        s.socks_port = Some(41234);
        assert_eq!(
            s.proxy_address().as_deref(),
            Some("socks5h://127.0.0.1:41234")
        );
    }

    #[test]
    fn wire_shapes_are_stable() {
        assert_eq!(to_line(&Request::Hello), b"{\"cmd\":\"hello\"}\n");
        assert_eq!(
            to_line(&Response::Error {
                reason: Reason::ResolverPortBusy { port: 1053 }
            }),
            b"{\"type\":\"error\",\"reason\":{\"code\":\"resolver_port_busy\",\"port\":1053}}\n"
        );
        assert_eq!(
            to_line(&Request::SetSettings {
                auto_connect: Some(false),
                phone: Some(PhoneChoice::Serial {
                    serial: "ab".into()
                })
            }),
            b"{\"cmd\":\"set_settings\",\"auto_connect\":false,\"phone\":{\"mode\":\"serial\",\"serial\":\"ab\"}}\n"
        );
        let connected = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Unmanaged,
            },
            ..Snapshot::default()
        };
        assert!(
            String::from_utf8(to_line(&connected))
                .unwrap()
                .contains("\"dns\":\"unmanaged\"")
        );
        assert_eq!(PROTOCOL_VERSION, 6);
        assert_eq!(
            to_line(&Request::History {
                day: Some("2026-10-16".into())
            }),
            b"{\"cmd\":\"history\",\"day\":\"2026-10-16\"}\n"
        );
        assert_eq!(
            from_line::<Request>(b"{\"cmd\":\"history\"}").unwrap(),
            Request::History { day: None }
        );
        assert_eq!(
            to_line(&Response::Error {
                reason: Reason::BadDay
            }),
            b"{\"type\":\"error\",\"reason\":{\"code\":\"bad_day\"}}\n"
        );
        assert_eq!(
            to_line(&Request::Diagnostics {
                range: DiagRange::Week,
                contact: Some(Contact {
                    email: Some("a@b.co".into()),
                    note: None
                })
            }),
            b"{\"cmd\":\"diagnostics\",\"range\":\"week\",\"contact\":{\"email\":\"a@b.co\"}}\n"
        );
        assert_eq!(
            from_line::<Request>(b"{\"cmd\":\"diagnostics\",\"range\":\"day\"}").unwrap(),
            Request::Diagnostics {
                range: DiagRange::Day,
                contact: None
            }
        );
        assert_eq!(
            to_line(&Response::Diagnostics { json: "{}".into() }),
            b"{\"type\":\"diagnostics\",\"json\":\"{}\"}\n"
        );
        let check = Check::new(CheckId::Trust, false).with("serials", "ab");
        assert_eq!(
            to_line(&check),
            b"{\"id\":\"trust\",\"ok\":false,\"args\":{\"serials\":\"ab\"}}\n"
        );
        let snap = Snapshot {
            state: ConnState::Retrying {
                reason: Reason::NoExit,
            },
            ..Snapshot::default()
        };
        let back: Snapshot = from_line(&to_line(&snap)).unwrap();
        assert_eq!(back, snap);
        assert_eq!(Snapshot::default().state, ConnState::NoPhone);
    }

    /// Strict DNS joined protocol 6 as an optional field: a snapshot without it (from a
    /// service before it, or Linux's default) reads as not applicable.
    #[test]
    fn strict_dns_on_the_wire() {
        let off = Snapshot {
            strict_dns: StrictDns::Off {
                reason: "BFE unavailable".into(),
            },
            ..Snapshot::default()
        };
        let line = String::from_utf8(to_line(&off)).unwrap();
        assert!(
            line.contains(r#""strict_dns":{"state":"off","reason":"BFE unavailable"}"#),
            "{line}"
        );
        assert_eq!(from_line::<Snapshot>(line.as_bytes()).unwrap(), off);
        let on = Snapshot {
            strict_dns: StrictDns::On,
            ..Snapshot::default()
        };
        assert!(
            String::from_utf8(to_line(&on))
                .unwrap()
                .contains(r#""strict_dns":{"state":"on"}"#)
        );
        let old = br#"{"state":{"state":"no_phone"},"devices":[],"selected":null,"wants_connection":false}"#;
        assert_eq!(
            from_line::<Snapshot>(old).unwrap().strict_dns,
            StrictDns::NotApplicable
        );
    }

    #[test]
    fn protocol_is_six() {
        assert_eq!(PROTOCOL_VERSION, 6);
    }

    #[test]
    fn endpoint_parses_and_prints() {
        #[cfg(windows)]
        {
            assert_eq!(Endpoint::parse(PIPE_NAME), Endpoint::Pipe(PIPE_NAME.into()));
            assert_eq!(Endpoint::system(), Endpoint::Pipe(PIPE_NAME.into()));
            assert_eq!(
                Endpoint::system().to_string(),
                r"\\.\pipe\cellglide-control"
            );
            // A bare name is a pipe in the local namespace.
            assert_eq!(Endpoint::parse("cellglide-control"), Endpoint::system());
        }
        #[cfg(unix)]
        {
            assert_eq!(
                Endpoint::parse(SOCKET_PATH),
                Endpoint::Unix(SOCKET_PATH.into())
            );
            assert_eq!(Endpoint::system().to_string(), SOCKET_PATH);
        }
        assert_eq!(
            Endpoint::system().to_string(),
            Endpoint::parse(&Endpoint::system().to_string()).to_string()
        );
    }

    #[test]
    fn windows_wire_names() {
        assert_eq!(
            to_line(&Response::Error {
                reason: Reason::NotAllowed
            }),
            b"{\"type\":\"error\",\"reason\":{\"code\":\"not_allowed\"}}\n"
        );
        assert_eq!(Reason::NotAllowed.key(), "reason.not_allowed");
        for (id, name) in [
            (CheckId::AppleService, "apple_service"),
            (CheckId::Wintun, "wintun"),
            (CheckId::Dns, "dns"),
        ] {
            assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{name}\""));
        }
    }

    #[tokio::test]
    async fn reply_limit_is_larger_than_request_limit() {
        let big = format!("{}\n", "x".repeat(MAX_LINE + 10));
        let mut r = tokio::io::BufReader::new(big.as_bytes());
        assert_eq!(
            read_line_limited(&mut r, MAX_REPLY_LINE)
                .await
                .unwrap()
                .unwrap()
                .len(),
            MAX_LINE + 10
        );
    }

    #[tokio::test]
    async fn read_line_limits_length() {
        let mut ok = tokio::io::BufReader::new(&b"{\"cmd\":\"status\"}\nrest"[..]);
        assert_eq!(
            read_line(&mut ok).await.unwrap().unwrap(),
            b"{\"cmd\":\"status\"}"
        );
        let mut eof = tokio::io::BufReader::new(&b""[..]);
        assert!(read_line(&mut eof).await.unwrap().is_none());
        let long = vec![b'x'; MAX_LINE + 10];
        let mut too_long = tokio::io::BufReader::new(&long[..]);
        assert_eq!(
            read_line(&mut too_long).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
