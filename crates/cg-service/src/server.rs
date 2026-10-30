//! The control socket: newline-delimited JSON requests from cellglide and cellglide-gui.

use crate::auth::Policy;
use crate::diagnostics::{self, Sources};
use cg_core::controller::{Command, Controller};
pub use cg_core::resolver::BoxFuture;
use cg_ipc::{Check, Reason, Request, Response, from_line, read_line, to_line};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub trait DoctorApi: Send + Sync + 'static {
    fn run(&self) -> BoxFuture<'_, Vec<Check>>;
}

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

pub async fn serve(
    listener: UnixListener,
    controller: Controller,
    doctor: Arc<dyn DoctorApi>,
    policy: Policy,
    sources: Arc<Sources>,
) {
    let mut failing = false;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
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
        // SO_PEERCRED: the kernel's word on who connected, used for authorization and to
        // know whose names a diagnostics bundle must hide.
        let peer = stream.peer_cred().ok();
        let allowed = peer
            .as_ref()
            .is_some_and(|cred| policy.allows(cred.uid(), cred.gid()));
        if !allowed {
            // Read the request first so the client never writes into a closed socket.
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    read_line(&mut BufReader::new(r)),
                )
                .await;
                let reply = Response::Error {
                    reason: Reason::NotInGroup,
                };
                let _ = w.write_all(&to_line(&reply)).await;
            });
            continue;
        }
        tokio::spawn(client(
            stream,
            controller.clone(),
            doctor.clone(),
            sources.clone(),
            peer.map(|cred| cred.uid()),
        ));
    }
}

async fn client(
    stream: UnixStream,
    controller: Controller,
    doctor: Arc<dyn DoctorApi>,
    sources: Arc<Sources>,
    peer_uid: Option<u32>,
) {
    let (r, mut w) = stream.into_split();
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
                    peer_uid,
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

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::controller::{ControllerConfig, HealthTiming};
    use cg_core::testing::{
        FakeBackend, FakeMux, FakeProbe, FakeTunnelProbe, SharedFake, SharedProbe,
    };
    use cg_ipc::client;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    /// The tests hold the socket path; the client takes an endpoint.
    fn ep(path: &std::path::Path) -> cg_ipc::Endpoint {
        cg_ipc::Endpoint::Unix(path.into())
    }

    struct NoDoctor;
    impl DoctorApi for NoDoctor {
        fn run(&self) -> BoxFuture<'_, Vec<Check>> {
            Box::pin(async { vec![Check::new(cg_ipc::CheckId::Tun, true)] })
        }
    }

    /// A journal that holds more than the bundle can carry, so the reply is near its 1 MB limit.
    fn test_sources(root: &Path) -> Sources {
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

    async fn start(policy: Policy) -> (tempfile::TempDir, std::path::PathBuf, FakeMux) {
        let mux = FakeMux::start().await;
        let dir = tempfile::tempdir().unwrap();
        let mut config =
            ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(FakeBackend::new())));
        config.exit_probe = Arc::new(SharedProbe(FakeProbe::new()));
        config.tunnel_probe = FakeTunnelProbe::new();
        config.health_dir = Some(dir.path().join("health"));
        config.health = HealthTiming {
            quick: Duration::from_secs(30),
            quick_timeout: Duration::from_millis(500),
            full: Duration::from_secs(30),
            retry: Duration::from_secs(5),
            early_spacing: Duration::from_millis(300),
        };
        let controller = Controller::spawn(config);
        let path = dir.path().join("control.sock");
        let listener = bind(&path, "no-such-group-cellglide-test").unwrap();
        let sources = Arc::new(test_sources(dir.path()));
        tokio::spawn(serve(
            listener,
            controller,
            Arc::new(NoDoctor),
            policy,
            sources,
        ));
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
