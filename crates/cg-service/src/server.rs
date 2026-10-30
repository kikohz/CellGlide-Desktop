//! The control socket (a named pipe on Windows): newline-delimited JSON requests from
//! cellglide and cellglide-gui. The transport says who connected ([`Listener`]); the server
//! decides per request what that peer may do.

use crate::access::{Access, Peer, Requester, needs_full};
#[cfg(unix)]
use crate::auth::Policy;
#[cfg(unix)]
use crate::diagnostics::{self, Sources};
#[cfg(windows)]
use crate::windows::diagnostics::{self, Sources};
use cg_core::controller::{Command, Controller};
pub use cg_core::resolver::BoxFuture;
use cg_ipc::{Check, Reason, Request, Response, from_line, read_line, to_line};
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

pub trait DoctorApi: Send + Sync + 'static {
    fn run(&self) -> BoxFuture<'_, Vec<Check>>;
}

/// Where clients come from: a Unix socket or a named pipe, each telling who connected.
pub trait Listener: Send + 'static {
    type Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    fn accept(&mut self) -> BoxFuture<'_, io::Result<(Self::Conn, Peer)>>;
}

/// What a turned-away client is told: on Linux that it isn't in the cellglide group; Windows
/// has no such group.
fn denied() -> Reason {
    #[cfg(unix)]
    return Reason::NotInGroup;
    #[cfg(not(unix))]
    return Reason::NotAllowed;
}

#[cfg(unix)]
pub fn bind(path: &Path, group: &str) -> io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    if let Ok(Some(g)) = nix::unistd::Group::from_name(group) {
        nix::unistd::chown(path, None, Some(g.gid)).map_err(io::Error::from)?;
    }
    Ok(listener)
}

/// The Unix control socket: the policy's members get full access, everyone else is denied.
#[cfg(unix)]
pub struct SocketListener {
    listener: UnixListener,
    policy: Policy,
}

#[cfg(unix)]
impl SocketListener {
    pub fn new(listener: UnixListener, policy: Policy) -> SocketListener {
        SocketListener { listener, policy }
    }
}

#[cfg(unix)]
impl Listener for SocketListener {
    type Conn = UnixStream;

    fn accept(&mut self) -> BoxFuture<'_, io::Result<(UnixStream, Peer)>> {
        Box::pin(async move {
            let (stream, _) = self.listener.accept().await?;
            // SO_PEERCRED: the kernel's word on who connected, used for authorization and to
            // know whose names a diagnostics bundle must hide.
            let cred = stream.peer_cred().ok();
            let allowed = cred
                .as_ref()
                .is_some_and(|cred| self.policy.allows(cred.uid(), cred.gid()));
            let peer = Peer {
                access: if allowed {
                    Access::Full
                } else {
                    Access::Denied
                },
                requester: cred.map_or(Requester::Unknown, |cred| Requester::Uid(cred.uid())),
            };
            Ok((stream, peer))
        })
    }
}

pub async fn serve<L: Listener>(
    mut listener: L,
    controller: Controller,
    doctor: Arc<dyn DoctorApi>,
    sources: Arc<Sources>,
) {
    let mut failing = false;
    loop {
        let (conn, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                // Out of descriptors: back off instead of spinning; warn once per streak.
                if !failing {
                    tracing::warn!(%error, "control socket accept failed");
                    failing = true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        failing = false;
        if peer.access == Access::Denied {
            tokio::spawn(turn_away(conn));
            continue;
        }
        tokio::spawn(client(
            conn,
            peer,
            controller.clone(),
            doctor.clone(),
            sources.clone(),
        ));
    }
}

/// Reads the request first so the client never writes into a closed connection, answers
/// with one error line and hangs up.
async fn turn_away<C: AsyncRead + AsyncWrite + Unpin>(conn: C) {
    let (r, mut w) = tokio::io::split(conn);
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        read_line(&mut BufReader::new(r)),
    )
    .await;
    let reply = Response::Error { reason: denied() };
    let _ = w.write_all(&to_line(&reply)).await;
}

/// The uid diagnostics use to hide the requester's names (Windows: Task 13).
fn requester_uid(requester: &Requester) -> Option<u32> {
    match requester {
        Requester::Uid(uid) => Some(*uid),
        Requester::Sid(_) | Requester::Unknown => None,
    }
}

async fn client<C: AsyncRead + AsyncWrite + Unpin>(
    conn: C,
    peer: Peer,
    controller: Controller,
    doctor: Arc<dyn DoctorApi>,
    sources: Arc<Sources>,
) {
    let (r, mut w) = tokio::io::split(conn);
    let mut r = BufReader::new(r);
    loop {
        let line = match read_line(&mut r).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(_) => {
                let _ = w
                    .write_all(&to_line(&Response::Error {
                        reason: Reason::BadRequest,
                    }))
                    .await;
                return;
            }
        };
        let reply = match from_line::<Request>(&line) {
            Err(_) => Response::Error {
                reason: Reason::BadRequest,
            },
            // Anything else stays open to a read-only peer, on the same connection.
            Ok(request) if peer.access != Access::Full && needs_full(&request) => Response::Error {
                reason: Reason::NotAllowed,
            },
            Ok(Request::Hello) => Response::Hello {
                protocol: cg_ipc::PROTOCOL_VERSION,
                version: env!("CARGO_PKG_VERSION").into(),
            },
            Ok(Request::Status) => Response::Status {
                snapshot: controller.snapshot(),
            },
            Ok(Request::Doctor) => Response::Doctor {
                checks: doctor.run().await,
            },
            Ok(Request::History { day }) => history(&controller, day).await,
            Ok(Request::Diagnostics { range, contact }) => {
                let checks = doctor.run().await;
                match diagnostics::build(
                    &sources,
                    range,
                    contact,
                    &controller.snapshot(),
                    &checks,
                    requester_uid(&peer.requester),
                    jiff::Timestamp::now(),
                )
                .await
                {
                    Ok(json) => Response::Diagnostics { json },
                    Err(reason) => Response::Error { reason },
                }
            }
            Ok(Request::Connect { serial }) => {
                outcome(controller.send(Command::Connect { serial }).await)
            }
            Ok(Request::Disconnect) => outcome(controller.send(Command::Disconnect).await),
            Ok(Request::SetSettings {
                auto_connect,
                phone,
            }) => outcome(
                controller
                    .send(Command::SetSettings {
                        auto_connect,
                        phone,
                    })
                    .await,
            ),
            Ok(Request::Subscribe) => {
                let mut updates = controller.subscribe();
                let mut probe = [0u8; 1];
                loop {
                    let snapshot = updates.borrow_and_update().clone();
                    if w.write_all(&to_line(&Response::Status { snapshot }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // A subscriber that hangs up is let go now, not at the next change;
                    // anything else it sends is ignored.
                    loop {
                        tokio::select! {
                            changed = updates.changed() => match changed {
                                Ok(()) => break,
                                Err(_) => return,
                            },
                            read = r.read(&mut probe) => match read {
                                Ok(0) | Err(_) => return,
                                Ok(_) => {}
                            },
                        }
                    }
                }
            }
        };
        if w.write_all(&to_line(&reply)).await.is_err() {
            return;
        }
    }
}

/// A day of the timeline: `YYYY-MM-DD` among the seven kept, or today. The files are read
/// on a blocking thread, never by the controller's actor.
async fn history(controller: &Controller, day: Option<String>) -> Response {
    let bad_day = Response::Error {
        reason: Reason::BadDay,
    };
    let day = match day {
        None => None,
        Some(text) => match parse_day(&text) {
            Some(day) => Some(day),
            None => return bad_day,
        },
    };
    let controller = controller.clone();
    match tokio::task::spawn_blocking(move || controller.history(day)).await {
        Ok(page) if page.days.contains(&page.day) => Response::History { day: page },
        Ok(_) => bad_day,
        Err(error) => Response::Error {
            reason: Reason::Internal {
                detail: error.to_string(),
            },
        },
    }
}

/// Only the extended form `YYYY-MM-DD`.
fn parse_day(text: &str) -> Option<jiff::civil::Date> {
    let shaped = text.len() == 10
        && text.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    shaped.then(|| text.parse().ok()).flatten()
}

fn outcome(result: cg_core::Result<()>) -> Response {
    match result {
        Ok(()) => Response::Ok,
        Err(error) => Response::Error {
            reason: error.reason(),
        },
    }
}

/// A controller on fakes behind [`serve`], for the server's tests and the transports'.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use cg_core::controller::{ControllerConfig, HealthTiming};
    use cg_core::testing::{
        FakeBackend, FakeMux, FakeProbe, FakeTunnelProbe, SharedFake, SharedProbe,
    };
    use std::path::Path;
    use std::time::Duration;

    pub struct NoDoctor;
    impl DoctorApi for NoDoctor {
        fn run(&self) -> BoxFuture<'_, Vec<Check>> {
            Box::pin(async { vec![Check::new(cg_ipc::CheckId::Tun, true)] })
        }
    }

    /// A journal that holds more than the bundle can carry, so the reply is near its 1 MB limit.
    #[cfg(unix)]
    pub fn test_sources(root: &Path) -> Sources {
        let word_line = "zzzzz ".repeat(100);
        Sources {
            journal: vec![
                "sh".into(),
                "-c".into(),
                format!("yes '{word_line}' | head -n 2000"),
                "sh".into(),
            ],
            root: root.to_path_buf(),
            health: None,
            hostname: Some("tester-host".into()),
        }
    }

    #[cfg(windows)]
    pub fn test_sources(_root: &Path) -> Sources {
        Sources
    }

    /// Serves `listener` with a controller whose state lives under `dir`; keep the mux alive.
    pub async fn spawn_server<L: Listener>(dir: &Path, listener: L) -> FakeMux {
        let mux = FakeMux::start().await;
        let mut config =
            ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(FakeBackend::new())));
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.tunnel_probe = FakeTunnelProbe::new();
        config.health_dir = Some(dir.join("health"));
        config.health = HealthTiming {
            quick: Duration::from_secs(30),
            quick_timeout: Duration::from_millis(500),
            full: Duration::from_secs(30),
            retry: Duration::from_secs(5),
            early_spacing: Duration::from_millis(300),
        };
        let controller = Controller::spawn(config);
        let sources = Arc::new(test_sources(dir));
        tokio::spawn(serve(listener, controller, Arc::new(NoDoctor), sources));
        mux
    }
}

/// Per-request access through an in-memory transport, on every platform.
#[cfg(test)]
mod access_tests {
    use super::test_support::spawn_server;
    use super::*;
    use cg_ipc::DiagRange;
    use tokio::io::DuplexStream;
    use tokio::sync::mpsc;

    type Dialed = (DuplexStream, Peer);

    struct MemListener(mpsc::UnboundedReceiver<Dialed>);

    impl Listener for MemListener {
        type Conn = DuplexStream;
        fn accept(&mut self) -> BoxFuture<'_, io::Result<Dialed>> {
            Box::pin(async {
                match self.0.recv().await {
                    Some(dialed) => Ok(dialed),
                    // The test is over: wait to be dropped rather than spin.
                    None => std::future::pending().await,
                }
            })
        }
    }

    fn dial(to: &mpsc::UnboundedSender<Dialed>, access: Access) -> BufReader<DuplexStream> {
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let peer = Peer {
            access,
            requester: Requester::Unknown,
        };
        to.send((theirs, peer)).unwrap();
        BufReader::new(ours)
    }

    async fn ask(conn: &mut BufReader<DuplexStream>, request: &Request) -> Option<Response> {
        conn.get_mut().write_all(&to_line(request)).await.unwrap();
        let line = read_line(conn).await.unwrap()?;
        Some(from_line(&line).unwrap())
    }

    async fn start() -> (
        tempfile::TempDir,
        cg_core::testing::FakeMux,
        mpsc::UnboundedSender<Dialed>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let mux = spawn_server(dir.path(), MemListener(rx)).await;
        (dir, mux, tx)
    }

    #[tokio::test]
    async fn read_only_peer_cannot_connect() {
        let (_dir, _mux, tx) = start().await;
        let mut conn = dial(&tx, Access::ReadOnly);
        let not_allowed = Some(Response::Error {
            reason: Reason::NotAllowed,
        });
        for request in [
            Request::Connect { serial: None },
            Request::Disconnect,
            Request::SetSettings {
                auto_connect: Some(false),
                phone: None,
            },
            Request::Diagnostics {
                range: DiagRange::Day,
                contact: None,
            },
        ] {
            assert_eq!(ask(&mut conn, &request).await, not_allowed, "{request:?}");
        }
        // The same connection still answers what a read-only peer may ask.
        assert!(matches!(
            ask(&mut conn, &Request::Status).await,
            Some(Response::Status { .. })
        ));
        assert!(matches!(
            ask(&mut conn, &Request::Hello).await,
            Some(Response::Hello { .. })
        ));
        assert!(matches!(
            ask(&mut conn, &Request::Doctor).await,
            Some(Response::Doctor { .. })
        ));
        assert!(matches!(
            ask(&mut conn, &Request::History { day: None }).await,
            Some(Response::History { .. })
        ));
        // The refused setting was never applied.
        let Some(Response::Status { snapshot }) = ask(&mut conn, &Request::Status).await else {
            panic!("expected a status");
        };
        assert!(snapshot.settings.auto_connect);
        // A full peer gets the same command through.
        let mut full = dial(&tx, Access::Full);
        assert_eq!(
            ask(&mut full, &Request::Disconnect).await,
            Some(Response::Ok)
        );
    }

    #[tokio::test]
    async fn denied_peer_gets_one_error_line() {
        let (_dir, _mux, tx) = start().await;
        let mut conn = dial(&tx, Access::Denied);
        assert_eq!(
            ask(&mut conn, &Request::Status).await,
            Some(Response::Error { reason: denied() })
        );
        // Then the server hangs up.
        assert_eq!(read_line(&mut conn).await.unwrap(), None);
        #[cfg(unix)]
        assert_eq!(denied(), Reason::NotInGroup);
        #[cfg(windows)]
        assert_eq!(denied(), Reason::NotAllowed);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::test_support::spawn_server;
    use super::*;
    use cg_core::testing::FakeMux;
    use cg_ipc::client;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    /// The tests hold the socket path; the client takes an endpoint.
    fn ep(path: &std::path::Path) -> cg_ipc::Endpoint {
        cg_ipc::Endpoint::Unix(path.into())
    }

    async fn start(policy: Policy) -> (tempfile::TempDir, std::path::PathBuf, FakeMux) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = bind(&path, "no-such-group-cellglide-test").unwrap();
        let mux = spawn_server(dir.path(), SocketListener::new(listener, policy)).await;
        (dir, path, mux)
    }
    fn me() -> Policy {
        let uid = nix::unistd::getuid().as_raw();
        Policy::for_test(None, move |_| vec![]).with_extra_uid(uid)
    }

    #[tokio::test]
    async fn answers_status_doctor_and_commands() {
        let (_dir, path, _mux) = start(me()).await;
        assert!(matches!(
            client::request(&ep(&path), &Request::Hello).await.unwrap(),
            Response::Hello {
                protocol: cg_ipc::PROTOCOL_VERSION,
                ..
            }
        ));
        assert!(matches!(
            client::request(&ep(&path), &Request::Status).await.unwrap(),
            Response::Status { .. }
        ));
        assert!(
            matches!(client::request(&ep(&path), &Request::Doctor).await.unwrap(), Response::Doctor { checks } if checks.len() == 1)
        );
        assert_eq!(
            client::request(&ep(&path), &Request::Disconnect)
                .await
                .unwrap(),
            Response::Ok
        );
        let mut updates = client::subscribe(&ep(&path)).await.unwrap();
        assert!(updates.recv().await.is_some());
    }

    #[tokio::test]
    async fn settings_round_trip() {
        let (_dir, path, _mux) = start(me()).await;
        let set = Request::SetSettings {
            auto_connect: Some(false),
            phone: None,
        };
        assert_eq!(
            client::request(&ep(&path), &set).await.unwrap(),
            Response::Ok
        );
        let Response::Status { snapshot } =
            client::request(&ep(&path), &Request::Status).await.unwrap()
        else {
            panic!("expected a status");
        };
        assert!(!snapshot.settings.auto_connect);
    }

    #[tokio::test]
    async fn history_round_trip() {
        let (_dir, path, _mux) = start(me()).await;
        let reply = client::request(&ep(&path), &Request::History { day: None })
            .await
            .unwrap();
        let Response::History { day } = reply else {
            panic!("expected a history page: {reply:?}");
        };
        assert_eq!(day.days.len(), 7);
        assert_eq!(day.days.last(), Some(&day.day));
        assert!(day.begin <= day.end);
        // The service wrote its start to the timeline.
        assert!(
            day.events
                .iter()
                .any(|e| e.kind == cg_ipc::health::EventKind::ServiceStarted)
        );
        let first = day.days[0].clone();
        let reply = client::request(
            &ep(&path),
            &Request::History {
                day: Some(first.clone()),
            },
        )
        .await
        .unwrap();
        assert!(matches!(reply, Response::History { day } if day.day == first));
        let too_old: jiff::civil::Date = first.parse().unwrap();
        let too_old = too_old.yesterday().unwrap().to_string();
        let tomorrow: jiff::civil::Date = day.day.parse().unwrap();
        let tomorrow = tomorrow.tomorrow().unwrap().to_string();
        for bad in [
            "nonsense",
            "20261016",
            "2026-1-16",
            too_old.as_str(),
            tomorrow.as_str(),
        ] {
            assert_eq!(
                client::request(
                    &ep(&path),
                    &Request::History {
                        day: Some(bad.into())
                    }
                )
                .await
                .unwrap(),
                Response::Error {
                    reason: Reason::BadDay
                },
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn diagnostics_round_trip() {
        use cg_ipc::DiagRange;
        let (_dir, path, _mux) = start(me()).await;
        let reply = client::request(
            &ep(&path),
            &Request::Diagnostics {
                range: DiagRange::Day,
                contact: None,
            },
        )
        .await
        .unwrap();
        let Response::Diagnostics { json } = reply else {
            panic!("expected diagnostics: {reply:?}");
        };
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["platform"], "linux");
        assert_eq!(v["format"], 1);
        // The journal held ~1.2 MB; the bundle is cut to fit 1 MB and still comes back whole.
        assert!(json.len() > 800_000 && json.len() <= cg_diag::bundle::MAX_BYTES);
        assert!(v.get("contact").is_none());
        assert!(
            v["environment"]["Checks"]
                .as_str()
                .unwrap()
                .contains("tun ok")
        );
    }

    #[tokio::test]
    async fn garbage_line_gets_error_and_server_survives() {
        let (_dir, path, _mux) = start(me()).await;
        let mut raw = tokio::net::UnixStream::connect(&path).await.unwrap();
        raw.write_all(b"not json\n").await.unwrap();
        let mut reader = tokio::io::BufReader::new(raw);
        let line = cg_ipc::read_line(&mut reader).await.unwrap().unwrap();
        assert_eq!(
            cg_ipc::from_line::<Response>(&line).unwrap(),
            Response::Error {
                reason: Reason::BadRequest
            }
        );

        let mut flood = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _ = flood.write_all(&vec![b'x'; cg_ipc::MAX_LINE * 2]).await;
        drop(flood);

        assert!(matches!(
            client::request(&ep(&path), &Request::Status).await.unwrap(),
            Response::Status { .. }
        ));
    }

    #[tokio::test]
    async fn closed_subscriber_task_ends() {
        let (_dir, path, _mux) = start(me()).await;
        let alive = || {
            tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks()
        };
        let settle = || async {
            // The controller's own tasks come and go (usbmuxd listen): wait for quiet.
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        settle().await;
        let baseline = alive();
        for _ in 0..20 {
            let mut raw = tokio::net::UnixStream::connect(&path).await.unwrap();
            raw.write_all(&to_line(&Request::Subscribe)).await.unwrap();
            let mut reader = tokio::io::BufReader::new(raw);
            let line = cg_ipc::read_line(&mut reader).await.unwrap().unwrap();
            assert!(matches!(
                from_line::<Response>(&line).unwrap(),
                Response::Status { .. }
            ));
            drop(reader);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while alive() > baseline {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{} subscriber tasks still alive",
                alive() - baseline
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(matches!(
            client::request(&ep(&path), &Request::Status).await.unwrap(),
            Response::Status { .. }
        ));
    }

    #[tokio::test]
    async fn strangers_are_turned_away() {
        if nix::unistd::getuid().is_root() {
            return; // root is always allowed
        }
        let (_dir, path, _mux) = start(Policy::for_test(Some(u32::MAX - 1), |_| vec![])).await;
        let reply = client::request(&ep(&path), &Request::Status).await.unwrap();
        assert_eq!(
            reply,
            Response::Error {
                reason: Reason::NotInGroup
            }
        );
    }

    #[test]
    fn socket_is_group_only() {
        use std::os::unix::fs::PermissionsExt;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        std::fs::write(&path, "stale").unwrap();
        let _listener = bind(&path, "no-such-group-cellglide-test").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o660
        );
    }
}
