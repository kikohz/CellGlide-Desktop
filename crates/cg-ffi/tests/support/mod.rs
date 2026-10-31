//! A fake service on a real socket (a named pipe on Windows), a test `Config`, and ways to wait
//! for events. Each test binary uses a part of it.
#![allow(dead_code)]

use cellglide::api::{Event, ReplyResult};
use cellglide::runtime::Config;
use cg_ipc::health::{DayStart, HistoryDay};
use cg_ipc::{Endpoint, Request, Response, Snapshot};
use cg_tools::speed::{Route, Server};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

/// What the fake service answers to the requests a test may care about.
#[derive(Clone, Debug)]
pub struct Replies {
    pub doctor: Response,
    pub history: Response,
    pub diagnostics: Response,
    pub set_settings: Response,
    pub connect: Response,
    pub disconnect: Response,
}

impl Default for Replies {
    fn default() -> Self {
        Replies {
            doctor: Response::Doctor { checks: Vec::new() },
            history: Response::History { day: empty_day() },
            diagnostics: Response::Diagnostics { json: "{}".into() },
            set_settings: Response::Ok,
            connect: Response::Ok,
            disconnect: Response::Ok,
        }
    }
}

/// A day with nothing in it, the only day there is.
pub fn empty_day() -> HistoryDay {
    let day = "2026-10-30".to_string();
    HistoryDay {
        day: day.clone(),
        start: DayStart::default(),
        events: Vec::new(),
        days: vec![day],
        begin: jiff::Timestamp::UNIX_EPOCH,
        end: jiff::Timestamp::UNIX_EPOCH,
    }
}

#[derive(Clone)]
struct Shared {
    replies: Replies,
    snapshots: watch::Receiver<Snapshot>,
    requests: Arc<Mutex<Vec<Request>>>,
}

pub struct FakeService {
    /// Every value is pushed to every subscriber, the current one first.
    pub snapshots: watch::Sender<Snapshot>,
    shared: Shared,
    endpoint: Endpoint,
    /// The listener and every connection; `None` while stopped.
    serving: Option<Serving>,
    #[cfg(unix)]
    _dir: tempfile::TempDir,
}

impl FakeService {
    pub async fn start(replies: Replies) -> FakeService {
        let (snapshots, receiver) = watch::channel(cellglide::testing::disconnected_snapshot());
        #[cfg(unix)]
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let endpoint = Endpoint::Unix(dir.path().join("control.sock"));
        #[cfg(windows)]
        let endpoint = Endpoint::Pipe(random_pipe_name());
        let mut service = FakeService {
            snapshots,
            shared: Shared {
                replies,
                snapshots: receiver,
                requests: Arc::default(),
            },
            endpoint,
            serving: None,
            #[cfg(unix)]
            _dir: dir,
        };
        service.restart().await;
        service
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// Every request heard so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// Stops listening and drops every connection.
    pub async fn stop(&mut self) {
        if let Some(serving) = self.serving.take() {
            serving.cancel.cancel();
            let _ = serving.task.await;
        }
        #[cfg(unix)]
        {
            let Endpoint::Unix(path) = &self.endpoint;
            let _ = std::fs::remove_file(path);
        }
    }

    /// Listens again at the same address.
    pub async fn restart(&mut self) {
        self.stop().await;
        self.serving = Some(listen(&self.endpoint, self.shared.clone()));
    }
}

impl Drop for FakeService {
    fn drop(&mut self) {
        if let Some(serving) = self.serving.take() {
            serving.cancel.cancel();
            serving.task.abort();
        }
    }
}

#[cfg(windows)]
fn random_pipe_name() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}ffi-{}-{nanos}-{n}",
        cg_ipc::TEST_PIPE_PREFIX,
        std::process::id()
    )
}

struct Serving {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// Accepts connections until cancelled, then closes the listener and every connection.
#[cfg(unix)]
fn listen(endpoint: &Endpoint, shared: Shared) -> Serving {
    let Endpoint::Unix(path) = endpoint;
    let listener = tokio::net::UnixListener::bind(path).unwrap();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        connections.spawn(serve(stream, shared.clone()));
                    }
                    Err(_) => break,
                },
            }
        }
        drop(listener);
        connections.shutdown().await;
    });
    Serving { cancel, task }
}

#[cfg(windows)]
fn listen(endpoint: &Endpoint, shared: Shared) -> Serving {
    use tokio::net::windows::named_pipe::ServerOptions;
    let Endpoint::Pipe(name) = endpoint;
    let name = name.clone();
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .unwrap();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                connected = server.connect() => {
                    if connected.is_err() {
                        break;
                    }
                    let next = ServerOptions::new().create(&name).unwrap();
                    connections.spawn(serve(std::mem::replace(&mut server, next), shared.clone()));
                }
            }
        }
        drop(server);
        connections.shutdown().await;
    });
    Serving { cancel, task }
}

/// One connection: a reply per request line; a subscription streams snapshots until closed.
async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, shared: Shared) {
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    while let Ok(Some(line)) = cg_ipc::read_line(&mut r).await {
        let Ok(request) = cg_ipc::from_line::<Request>(&line) else {
            return;
        };
        shared.requests.lock().unwrap().push(request.clone());
        let replies = &shared.replies;
        let reply = match request {
            Request::Hello => Response::Hello {
                protocol: cg_ipc::PROTOCOL_VERSION,
                version: "test".into(),
            },
            Request::Status => Response::Status {
                snapshot: shared.snapshots.borrow().clone(),
            },
            Request::Subscribe => {
                let mut snapshots = shared.snapshots.clone();
                loop {
                    let snapshot = snapshots.borrow_and_update().clone();
                    let line = cg_ipc::to_line(&Response::Status { snapshot });
                    if w.write_all(&line).await.is_err() || snapshots.changed().await.is_err() {
                        return;
                    }
                }
            }
            Request::Doctor => replies.doctor.clone(),
            Request::History { .. } => replies.history.clone(),
            Request::Diagnostics { .. } => replies.diagnostics.clone(),
            Request::SetSettings { .. } => replies.set_settings.clone(),
            Request::Connect { .. } => replies.connect.clone(),
            Request::Disconnect => replies.disconnect.clone(),
        };
        if w.write_all(&cg_ipc::to_line(&reply)).await.is_err() {
            return;
        }
    }
}

/// A `Config` for `service` with every file under `dir`, quick retries and ticks, and nothing
/// that leaves this computer.
pub fn config(service: &FakeService, dir: &Path) -> Config {
    config_at(service.endpoint(), dir)
}

/// The same for any endpoint.
pub fn config_at(endpoint: Endpoint, dir: &Path) -> Config {
    let nowhere = "127.0.0.1".to_string();
    Config {
        endpoint,
        prefs_path: dir.join("prefs.json"),
        history_path: dir.join("speedtests.json"),
        download_dir: Some(dir.join("Downloads")),
        speed_server: Server {
            host: nowhere.clone(),
            port: 9,
            tls: false,
        },
        leak_target: cg_tools::https::Target {
            host: nowhere.clone(),
            port: 9,
            tls: false,
        },
        diag_target: cg_diag::upload::Target {
            host: nowhere,
            port: 9,
            tls: false,
            path: "/v1/reports".into(),
        },
        uplink: || None,
        resolve: |_| Box::pin(async {}),
        retry: Duration::from_millis(100),
        tick: Duration::from_millis(100),
        route_override: Some(Route::Unbound),
    }
}

/// An address nobody serves.
pub fn missing_endpoint(dir: &Path) -> Endpoint {
    #[cfg(unix)]
    return Endpoint::Unix(dir.join("nobody.sock"));
    #[cfg(windows)]
    {
        let _ = dir;
        Endpoint::Pipe(random_pipe_name())
    }
}

/// A unique directory under the system's temporary directory that is never created: reading
/// from it finds nothing, and nothing is left behind.
pub fn unused_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("cellglide-ffi-unused-{}-{n}", std::process::id()))
}

pub type Collected = Arc<Mutex<Vec<Event>>>;

/// A sink that keeps every event.
pub fn collect() -> (Box<dyn Fn(Event) + Send>, Collected) {
    let events = Collected::default();
    let kept = events.clone();
    (
        Box::new(move |event| kept.lock().unwrap().push(event)),
        events,
    )
}

/// The first event matching `pred`, checking every 10 ms; panics after 5 s with what came.
pub async fn wait_for(events: &Collected, pred: impl Fn(&Event) -> bool) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = events.lock().unwrap().iter().find(|e| pred(e)) {
            return event.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "no matching event within 5 s; got {:#?}",
                events.lock().unwrap()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The reply to command `id`.
pub async fn reply_of(events: &Collected, id: u64) -> ReplyResult {
    match wait_for(
        events,
        |e| matches!(e, Event::Reply { id: got, .. } if *got == id),
    )
    .await
    {
        Event::Reply { result, .. } => result,
        _ => unreachable!(),
    }
}
