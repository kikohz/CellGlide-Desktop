//! Client side of the control socket (a named pipe on Windows), used by cellglide and the GUI.

use crate::describe::{
    self,
    ui::{DownReason, ServiceDown},
};
use crate::{
    Endpoint, MAX_REPLY_LINE, Reason, Request, Response, Snapshot, from_line, read_line_limited,
    to_line,
};
use cg_i18n::tr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("can't reach the CellGlide service: {0}")]
    Io(#[from] std::io::Error),
    #[error("unreadable reply: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the service closed the connection")]
    Closed,
    /// The control pipe isn't owned by LocalSystem in session 0, so it isn't the CellGlide service.
    #[error("the control pipe isn't served by the CellGlide service")]
    NotService,
}

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

async fn connect(endpoint: &Endpoint) -> Result<(Reader, Writer), ClientError> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::Unix(path) => {
            let (r, w) = tokio::net::UnixStream::connect(path).await?.into_split();
            Ok((Box::new(r), Box::new(w)))
        }
        #[cfg(windows)]
        Endpoint::Pipe(name) => {
            let pipe = connect_pipe(name, verifies(name)).await?;
            let (r, w) = tokio::io::split(pipe);
            Ok((Box::new(r), Box::new(w)))
        }
    }
}

/// Every pipe's server is checked except the tests' own ([`crate::TEST_PIPE_PREFIX`]). This
/// fails closed: Windows opens the service's pipe under many spellings (`/` for `\`,
/// `\\localhost\`, `\\?\`, `\\.\GLOBALROOT\Device\NamedPipe\`, any case), and a list of
/// them could miss one and skip the check. The service only ever serves [`crate::PIPE_NAME`],
/// so no other pipe a user names has a reason to pass unchecked.
#[cfg(windows)]
fn verifies(name: &str) -> bool {
    let prefix = crate::TEST_PIPE_PREFIX;
    let is_test = name
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix));
    !is_test
}

/// Opens a pipe, retrying for up to a second while every instance is busy. With `verify`, a
/// pipe that isn't LocalSystem's in session 0 is refused before a single byte is written to it.
#[cfg(windows)]
pub(crate) async fn connect_pipe(
    name: &str,
    verify: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ClientError> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;
    const RETRY: Duration = Duration::from_millis(50);
    const PATIENCE: Duration = Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let pipe = loop {
        match ClientOptions::new().open(name) {
            Ok(pipe) => break pipe,
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32)
                    && tokio::time::Instant::now() + RETRY <= deadline =>
            {
                tokio::time::sleep(RETRY).await
            }
            Err(e) => return Err(e.into()),
        }
    };
    if verify && !crate::pipe_identity::server_is_cellglide_service(&pipe)? {
        return Err(ClientError::NotService);
    }
    Ok(pipe)
}

pub async fn request(endpoint: &Endpoint, request: &Request) -> Result<Response, ClientError> {
    let (r, mut w) = connect(endpoint).await?;
    w.write_all(&to_line(request)).await?;
    let line = read_line_limited(&mut BufReader::new(r), MAX_REPLY_LINE)
        .await?
        .ok_or(ClientError::Closed)?;
    Ok(from_line(&line)?)
}

/// Snapshots as the service publishes them, starting with the current one.
/// The receiver closes when the service goes away.
pub async fn subscribe(endpoint: &Endpoint) -> Result<mpsc::Receiver<Snapshot>, ClientError> {
    let (r, mut w) = connect(endpoint).await?;
    w.write_all(&to_line(&Request::Subscribe)).await?;
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _keep_open = w;
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = read_line_limited(&mut r, MAX_REPLY_LINE).await {
            let Ok(Response::Status { snapshot }) = from_line::<Response>(&line) else {
                break;
            };
            if tx.send(snapshot).await.is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

/// How long a reply may take before the window stops waiting.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How long a subscription may stay silent before [`watch`] asks the service whether it still
/// holds. The service only sends a snapshot when something changed, so silence alone is normal.
pub const SILENCE: Duration = Duration::from_secs(30);
/// The diagnostics bundle reads the journal, which can take a while.
pub const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(90);

pub fn timeout_for(request: &Request) -> Duration {
    match request {
        Request::Diagnostics { .. } => DIAGNOSTICS_TIMEOUT,
        _ => TIMEOUT,
    }
}

/// One request for a window. Every failure is text for the page that asked: the service's own
/// refusal, an unreachable service, and one that never answers ([`timeout_for`]).
pub async fn call(endpoint: &Endpoint, request: Request) -> Result<Response, String> {
    match tokio::time::timeout(timeout_for(&request), self::request(endpoint, &request)).await {
        Err(_elapsed) => Err(tr("gui.service_timeout", &[])),
        Ok(Ok(Response::Error { reason })) => Err(describe::reason_text(&reason)),
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(describe::unreachable(&error)),
    }
}

/// Whether the service speaks our protocol; otherwise why not.
pub async fn handshake(endpoint: &Endpoint) -> Result<(), ServiceDown> {
    match request(endpoint, &Request::Hello).await {
        Ok(Response::Hello { protocol, .. }) if protocol == crate::PROTOCOL_VERSION => Ok(()),
        Ok(Response::Hello { protocol, .. }) => {
            Err(ServiceDown::new(DownReason::VersionMismatch {
                service: protocol.to_string(),
            }))
        }
        Ok(Response::Error {
            reason: Reason::NotInGroup,
        }) => Err(ServiceDown::new(DownReason::NotInGroup)),
        // A v1 reply this client can parse but doesn't expect; unparsable ones are covered by `unreachable`.
        Ok(_) => Err(ServiceDown::new(DownReason::VersionMismatch {
            service: "1".into(),
        })),
        Err(error) => Err(unreachable_down(&error)),
    }
}

/// [`describe::unreachable`] as a reason that can be said again: an unparsable reply is a
/// service from another version, anything else what the doctor's service check says.
fn unreachable_down(error: &ClientError) -> ServiceDown {
    ServiceDown::new(match error {
        ClientError::Json(_) => DownReason::VersionMismatch {
            service: "1".into(),
        },
        _ => DownReason::Unreachable {
            check: describe::service_check(error),
        },
    })
}

/// What a window hears from [`watch`].
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Snapshot(Box<Snapshot>),
    /// No snapshots for now, and why; the last snapshot no longer holds.
    Down(ServiceDown),
}

/// The service's snapshots for as long as the receiver is kept: handshake, subscribe, and each
/// snapshot as it comes. When the subscription can't start or ends, a `Down` says why, and
/// after `retry` it starts over; a service that takes the connection but doesn't start the
/// subscription within [`TIMEOUT`] is down too ([`DownReason::Timeout`]). A subscription
/// silent for [`SILENCE`] is checked with `Status` on a second connection: no answer within
/// [`TIMEOUT`] is `Down(Timeout)` and a retry; a snapshot other than the last one heard means
/// the subscription stopped hearing about changes, and it is replaced without a `Down`; the
/// same snapshot means all is well, and nothing is sent. Dropping the receiver ends it.
pub fn watch(endpoint: Endpoint, retry: Duration) -> mpsc::Receiver<Update> {
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let gone = tx.clone();
        tokio::select! {
            () = gone.closed() => {}
            () = watch_loop(&endpoint, retry, &tx) => {}
        }
    });
    rx
}

/// Handshake, subscribe and the first snapshot: the subscription has started. A service that
/// took the connection but says nothing within [`TIMEOUT`] is `DownReason::Timeout`.
async fn start_watching(
    endpoint: &Endpoint,
) -> Result<(Snapshot, mpsc::Receiver<Snapshot>), ServiceDown> {
    let start = async {
        handshake(endpoint).await?;
        let mut updates = subscribe(endpoint)
            .await
            .map_err(|e| unreachable_down(&e))?;
        match updates.recv().await {
            Some(first) => Ok((first, updates)),
            None => Err(ServiceDown::new(DownReason::Gone)),
        }
    };
    tokio::time::timeout(TIMEOUT, start)
        .await
        .unwrap_or_else(|_elapsed| Err(ServiceDown::new(DownReason::Timeout)))
}

async fn watch_loop(endpoint: &Endpoint, retry: Duration, tx: &mpsc::Sender<Update>) {
    loop {
        let down = match start_watching(endpoint).await {
            Ok((first, updates)) => match follow(endpoint, first, updates, tx).await {
                Ended::Unheard => return,
                Ended::Stale => continue,
                Ended::Down(down) => down,
            },
            Err(problem) => problem,
        };
        if tx.send(Update::Down(down)).await.is_err() {
            return;
        }
        tokio::time::sleep(retry).await;
    }
}

/// How following one subscription ended.
enum Ended {
    /// Nobody listens to the watch any more.
    Unheard,
    /// The service has moved on without telling this subscription: subscribe again at once.
    Stale,
    /// No snapshots for now, and why.
    Down(ServiceDown),
}

/// What a silent subscription turned out to be.
enum Silence {
    /// The service still has the snapshot it last sent: nothing changed.
    Quiet,
    /// The service has another snapshot than the one it last sent.
    Stale,
    /// The service didn't answer `Status` within [`TIMEOUT`], or not with a snapshot.
    Hung,
}

/// Asks for the snapshot on a connection of its own and compares it with `last`, the last one
/// the subscription sent.
async fn probe(endpoint: &Endpoint, last: &Snapshot) -> Silence {
    match tokio::time::timeout(TIMEOUT, request(endpoint, &Request::Status)).await {
        Ok(Ok(Response::Status { snapshot })) if snapshot == *last => Silence::Quiet,
        Ok(Ok(Response::Status { .. })) => Silence::Stale,
        _ => Silence::Hung,
    }
}

/// Sends `first` and every later snapshot of the subscription; after [`SILENCE`] without one,
/// [`probe`]s. A snapshot that arrives while the probe waits wins over the probe.
async fn follow(
    endpoint: &Endpoint,
    first: Snapshot,
    mut updates: mpsc::Receiver<Snapshot>,
    tx: &mpsc::Sender<Update>,
) -> Ended {
    let mut last = first;
    loop {
        if tx
            .send(Update::Snapshot(Box::new(last.clone())))
            .await
            .is_err()
        {
            return Ended::Unheard;
        }
        let next = loop {
            tokio::select! {
                next = updates.recv() => break next,
                () = tokio::time::sleep(SILENCE) => {}
            }
            tokio::select! {
                next = updates.recv() => break next,
                silence = probe(endpoint, &last) => match silence {
                    Silence::Quiet => {}
                    Silence::Stale => return Ended::Stale,
                    Silence::Hung => return Ended::Down(ServiceDown::new(DownReason::Timeout)),
                },
            }
        };
        match next {
            Some(snapshot) => last = snapshot,
            None => return Ended::Down(ServiceDown::new(DownReason::Gone)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

    /// How a test server answers.
    #[derive(Clone, Copy)]
    enum Behaviour {
        /// Every request with `Ok`, a subscription with two default snapshots, kept open.
        Plain,
        /// Like the service: `Hello` with our protocol, and a subscription of two default
        /// snapshots that then ends, as when the service stops.
        Service,
        /// Accepts and never answers; keeps the connection open.
        Silent,
        /// `Hello` with our protocol, then a subscription that never sends a snapshot (the
        /// connection kept open).
        Mute,
        /// `Hello` with our protocol; a subscription of one default snapshot, then nothing more
        /// (kept open); `Status` never answered: a service stuck after the subscription started.
        Hang,
        /// `Hello` with our protocol; a subscription of one default snapshot (kept open), and
        /// `Status` answers the same snapshot: healthy, with nothing new to say.
        Quiet,
        /// `Hello` with our protocol; the first subscription sends one default snapshot, every
        /// later one and every `Status` a snapshot that wants a connection (all kept open): a
        /// subscription that stopped hearing about changes.
        Stale,
    }

    /// What a test server was asked.
    #[derive(Default)]
    struct Counts {
        subscribes: AtomicUsize,
        statuses: AtomicUsize,
    }

    /// The snapshot [`Behaviour::Stale`] moved on to.
    fn wanting() -> Snapshot {
        Snapshot {
            wants_connection: true,
            ..Snapshot::default()
        }
    }

    async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(
        stream: S,
        behaviour: Behaviour,
        counts: Arc<Counts>,
    ) {
        if let Behaviour::Silent = behaviour {
            let _held = stream;
            std::future::pending::<()>().await;
            return;
        }
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);
        let status = |snapshot| crate::to_line(&Response::Status { snapshot });
        while let Ok(Some(line)) = crate::read_line(&mut r).await {
            let request = crate::from_line::<Request>(&line).unwrap();
            let earlier_subscribes = match request {
                Request::Subscribe => counts.subscribes.fetch_add(1, Ordering::SeqCst),
                Request::Status => {
                    counts.statuses.fetch_add(1, Ordering::SeqCst);
                    0
                }
                _ => 0,
            };
            let reply = match (behaviour, &request) {
                (Behaviour::Mute, Request::Subscribe) | (Behaviour::Hang, Request::Status) => {
                    std::future::pending::<()>().await;
                    return;
                }
                (Behaviour::Hang | Behaviour::Quiet, Request::Subscribe) => {
                    status(Snapshot::default())
                }
                (Behaviour::Quiet, Request::Status) => status(Snapshot::default()),
                (Behaviour::Stale, Request::Subscribe) if earlier_subscribes == 0 => {
                    status(Snapshot::default())
                }
                (Behaviour::Stale, Request::Subscribe | Request::Status) => status(wanting()),
                (_, Request::Subscribe) => {
                    [status(Snapshot::default()), status(Snapshot::default())].concat()
                }
                (
                    Behaviour::Service
                    | Behaviour::Mute
                    | Behaviour::Hang
                    | Behaviour::Quiet
                    | Behaviour::Stale,
                    Request::Hello,
                ) => crate::to_line(&Response::Hello {
                    protocol: crate::PROTOCOL_VERSION,
                    version: "test".into(),
                }),
                _ => crate::to_line(&Response::Ok),
            };
            if w.write_all(&reply).await.is_err() {
                break;
            }
            if matches!(
                (behaviour, request),
                (Behaviour::Service, Request::Subscribe)
            ) {
                break;
            }
        }
    }

    fn test_server() -> (Endpoint, Keep) {
        test_server_as(Behaviour::Plain)
    }

    #[cfg(unix)]
    type Keep = tempfile::TempDir;
    #[cfg(windows)]
    type Keep = ();

    fn test_server_as(behaviour: Behaviour) -> (Endpoint, Keep) {
        let (endpoint, keep, _counts) = test_server_counting(behaviour);
        (endpoint, keep)
    }

    /// A test server answering as `behaviour`, and what it has been asked.
    #[cfg(unix)]
    fn test_server_counting(behaviour: Behaviour) -> (Endpoint, Keep, Arc<Counts>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let counts = Arc::<Counts>::default();
        let kept = counts.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, behaviour, kept.clone()));
            }
        });
        (Endpoint::Unix(path), dir, counts)
    }

    #[cfg(windows)]
    fn random_pipe_name() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        // The clock alone can repeat across tests running in parallel (coarse ticks).
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        format!(
            "{}{}-{nanos}-{n}",
            crate::TEST_PIPE_PREFIX,
            std::process::id()
        )
    }

    #[cfg(windows)]
    fn test_server_counting(behaviour: Behaviour) -> (Endpoint, Keep, Arc<Counts>) {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = random_pipe_name();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let pipe_name = name.clone();
        let counts = Arc::<Counts>::default();
        let kept = counts.clone();
        tokio::spawn(async move {
            loop {
                if server.connect().await.is_err() {
                    break;
                }
                let next = ServerOptions::new().create(&pipe_name).unwrap();
                let stream = std::mem::replace(&mut server, next);
                tokio::spawn(serve(stream, behaviour, kept.clone()));
            }
        });
        (Endpoint::Pipe(name), (), counts)
    }

    #[tokio::test]
    async fn request_and_subscribe_round_trip() {
        let (endpoint, _keep) = test_server();
        assert_eq!(
            request(&endpoint, &Request::Disconnect).await.unwrap(),
            Response::Ok
        );
        let mut rx = subscribe(&endpoint).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
        // Several clients in a row each get their own connection.
        for _ in 0..3 {
            assert_eq!(
                request(&endpoint, &Request::Status).await.unwrap(),
                Response::Ok
            );
        }
    }

    #[tokio::test]
    async fn missing_endpoint_is_an_io_error() {
        #[cfg(unix)]
        let endpoint = Endpoint::parse("/nonexistent/cellglide.sock");
        #[cfg(windows)]
        let endpoint = Endpoint::parse(&random_pipe_name());
        let err = request(&endpoint, &Request::Status).await.unwrap_err();
        assert!(matches!(err, ClientError::Io(_)), "{err:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn refuses_server_that_is_not_system() {
        use tokio::io::AsyncReadExt;
        use tokio::net::windows::named_pipe::ServerOptions;
        // This test process owns the pipe, and it doesn't run as LocalSystem.
        let name = random_pipe_name();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let heard = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                match server.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    // The client closing its end reads as a broken pipe.
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => break,
                    Err(e) => panic!("{e}"),
                }
            }
            bytes
        });
        let err = connect_pipe(&name, true).await.unwrap_err();
        assert!(matches!(err, ClientError::NotService), "{err:?}");
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), heard)
            .await
            .expect("the client never hung up")
            .unwrap();
        assert!(bytes.is_empty(), "the client wrote {bytes:?}");
    }

    /// A pipe that Windows serves from a LocalSystem svchost in session 0 (owner SYSTEM) passes
    /// the check. Ignored: it depends on the machine's own services.
    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn accepts_a_local_system_server() {
        connect_pipe(r"\\.\pipe\srvsvc", true).await.unwrap();
    }

    /// What an elevated process gets by creating a pipe with the service's own SDDL. Making
    /// SYSTEM the owner needs SeRestorePrivilege enabled (an elevated token has it, disabled);
    /// with it, only session 0 tells the pipe apart, so outside session 0 the client refuses.
    /// Measured in Task 7 from a session-0 shell with SeRestorePrivilege enabled: the pipe was
    /// created and accepted, as expected for an administrator, whom the check does not defend
    /// against. Run elevated; prints what happened.
    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn elevated_process_with_the_service_sddl() {
        use tokio::net::windows::named_pipe::ServerOptions;
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
        use windows::core::HSTRING;
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sd` receives a LocalAlloc'd descriptor, freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(crate::PIPE_SDDL),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .unwrap();
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let name = random_pipe_name();
        // SAFETY: `attributes` and the descriptor it points to outlive the call.
        let created = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .create_with_security_attributes_raw(
                    &name,
                    (&raw mut attributes).cast::<std::ffi::c_void>(),
                )
        };
        // SAFETY: allocated by the conversion above; freed exactly once.
        let _ = unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        match created {
            Err(e) => println!("creating the pipe with PIPE_SDDL failed: {e:?}"),
            Ok(server) => {
                // Only possible with SeRestorePrivilege enabled; then session 0 decides.
                println!("created the pipe with PIPE_SDDL (SeRestorePrivilege must be enabled)");
                let verdict = connect_pipe(&name, true).await.map(|_| ());
                println!("client verdict: {verdict:?}");
                let plain = ServerOptions::new().create(&name).unwrap();
                let probe = connect_pipe(&name, false).await.unwrap();
                let mut session = u32::MAX;
                // SAFETY: the handle belongs to `probe`.
                unsafe {
                    windows::Win32::System::Pipes::GetNamedPipeServerSessionId(
                        windows::Win32::Foundation::HANDLE(
                            std::os::windows::io::AsRawHandle::as_raw_handle(&probe),
                        ),
                        &mut session,
                    )
                }
                .unwrap();
                println!("this process serves from session {session}");
                if session != 0 {
                    assert!(
                        matches!(verdict, Err(ClientError::NotService)),
                        "{verdict:?}"
                    );
                }
                drop((server, plain, probe));
            }
        }
    }

    /// Only the tests' own pipes skip the identity check.
    #[cfg(windows)]
    #[test]
    fn only_test_pipes_skip_verification() {
        assert!(verifies(crate::PIPE_NAME));
        assert!(!verifies(&random_pipe_name()));
        assert!(!verifies(r"\\.\PIPE\CellGlide-Test-1"));
        // Near misses of the prefix are checked.
        assert!(verifies(r"\\.\pipe\cellglide-test"));
        assert!(verifies(r"\\?\pipe\cellglide-test-1"));
        assert!(verifies(r"\\.\pipe\other"));
    }

    #[cfg(windows)]
    #[test]
    fn verifies_every_spelling_of_the_service_pipe() {
        for name in [
            r"\\.\pipe\cellglide-control",
            r"\\.\PIPE\CellGlide-Control",
            r"\\localhost\pipe\cellglide-control",
            "//./pipe/cellglide-control",
            r"\\?\pipe\cellglide-control",
            r"\\?\PIPE\CELLGLIDE-CONTROL",
            r"\\.\GLOBALROOT\Device\NamedPipe\cellglide-control",
            r"\\?\globalroot\device\namedpipe\cellglide-control",
        ] {
            assert!(verifies(name), "{name}");
        }
    }

    /// The `\\?\` and GLOBALROOT spellings really open the service's pipe name, so they must
    /// be verified: here they reach a pipe served under a test name.
    #[cfg(windows)]
    #[tokio::test]
    async fn other_spellings_reach_the_same_pipe() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let name = random_pipe_name();
        let leaf = name.strip_prefix(r"\\.\pipe\").unwrap();
        for spelling in [
            format!(r"\\?\pipe\{leaf}"),
            format!(r"\\.\GLOBALROOT\Device\NamedPipe\{leaf}"),
        ] {
            let server = ServerOptions::new().create(&name).unwrap();
            let _client = ClientOptions::new()
                .open(&spelling)
                .unwrap_or_else(|e| panic!("{spelling}: {e}"));
            server.connect().await.unwrap();
            assert!(verifies(&spelling), "{spelling}");
        }
    }

    /// The text for `key` in either language: other tests may switch the language meanwhile.
    fn is_text_of(text: &str, key: &str) -> bool {
        [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En]
            .iter()
            .any(|&lang| text == cg_i18n::tr_in(lang, key, &[]))
    }

    #[tokio::test(start_paused = true)]
    async fn a_service_that_never_answers_times_out() {
        let (endpoint, _keep) = test_server_as(Behaviour::Silent);
        let started = tokio::time::Instant::now();
        let result = call(&endpoint, Request::History { day: None }).await;
        let text = result.unwrap_err();
        assert!(is_text_of(&text, "gui.service_timeout"), "{text}");
        assert_eq!(started.elapsed(), TIMEOUT);
    }

    #[test]
    fn diagnostics_get_longer_to_answer() {
        let diagnostics = Request::Diagnostics {
            range: crate::DiagRange::Day,
            contact: None,
        };
        assert_eq!(timeout_for(&diagnostics), Duration::from_secs(90));
        assert_eq!(timeout_for(&Request::Doctor), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn watch_hears_snapshots_then_the_service_going_and_comes_back() {
        let (endpoint, _keep) = test_server_as(Behaviour::Service);
        let mut updates = watch(endpoint, Duration::from_millis(10));
        let snapshot = Update::Snapshot(Box::default());
        for _ in 0..2 {
            assert_eq!(updates.recv().await.unwrap(), snapshot);
            assert_eq!(updates.recv().await.unwrap(), snapshot);
            match updates.recv().await.unwrap() {
                Update::Down(down) => {
                    assert!(is_text_of(&down.text, "gui.service_gone"), "{down:?}");
                    assert!(!down.not_in_group);
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn watch_says_why_a_service_is_unusable_and_keeps_trying() {
        // A service of another protocol (it answers `Hello` with `Ok`).
        let (endpoint, _keep) = test_server_as(Behaviour::Plain);
        let mut updates = watch(endpoint, Duration::from_millis(10));
        for _ in 0..2 {
            let Some(Update::Down(down)) = updates.recv().await else {
                panic!("expected Down");
            };
            assert!(!down.not_in_group && !down.text.is_empty(), "{down:?}");
        }
        // Nothing listening at all.
        #[cfg(unix)]
        let missing = Endpoint::parse("/nonexistent/cellglide.sock");
        #[cfg(windows)]
        let missing = Endpoint::parse(&random_pipe_name());
        let mut updates = watch(missing, Duration::from_millis(10));
        assert!(matches!(updates.recv().await, Some(Update::Down(_))));
        assert!(matches!(updates.recv().await, Some(Update::Down(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn watch_gives_up_on_a_service_that_never_answers_hello() {
        let (endpoint, _keep) = test_server_as(Behaviour::Silent);
        let started = tokio::time::Instant::now();
        let mut updates = watch(endpoint, Duration::from_millis(10));
        let first = tokio::time::timeout(TIMEOUT * 2, updates.recv()).await;
        let Ok(Some(Update::Down(down))) = first else {
            panic!("{first:?}");
        };
        assert!(is_text_of(&down.text, "gui.service_timeout"), "{down:?}");
        assert_eq!(started.elapsed(), TIMEOUT);
        // And it keeps trying.
        let again = tokio::time::timeout(TIMEOUT * 2, updates.recv()).await;
        assert!(matches!(again, Ok(Some(Update::Down(_)))), "{again:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn watch_gives_up_on_a_subscription_that_never_starts() {
        let (endpoint, _keep) = test_server_as(Behaviour::Mute);
        let mut updates = watch(endpoint, Duration::from_millis(10));
        let first = tokio::time::timeout(TIMEOUT * 2, updates.recv()).await;
        let Ok(Some(Update::Down(down))) = first else {
            panic!("{first:?}");
        };
        assert!(is_text_of(&down.text, "gui.service_timeout"), "{down:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_service_after_the_first_snapshot_is_down() {
        let (endpoint, _keep, _counts) = test_server_counting(Behaviour::Hang);
        let mut rx = watch(endpoint, Duration::from_secs(2));
        assert!(matches!(rx.recv().await, Some(Update::Snapshot(_))));
        let next = tokio::time::timeout(SILENCE + TIMEOUT + Duration::from_secs(1), rx.recv())
            .await
            .unwrap();
        assert!(matches!(next, Some(Update::Down(d)) if matches!(d.reason, DownReason::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_healthy_service_is_probed_and_keeps_its_subscription() {
        let (endpoint, _keep, counts) = test_server_counting(Behaviour::Quiet);
        let mut rx = watch(endpoint, Duration::from_secs(2));
        assert!(matches!(rx.recv().await, Some(Update::Snapshot(_))));
        assert!(
            tokio::time::timeout(SILENCE * 3 + Duration::from_secs(1), rx.recv())
                .await
                .is_err(),
            "nothing new, nothing sent"
        );
        assert!(
            counts.statuses.load(Ordering::SeqCst) >= 2,
            "the silence was probed"
        );
        assert_eq!(counts.subscribes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_subscription_is_replaced_silently() {
        let (endpoint, _keep, counts) = test_server_counting(Behaviour::Stale);
        let mut rx = watch(endpoint, Duration::from_secs(2));
        let Some(Update::Snapshot(first)) = rx.recv().await else {
            panic!()
        };
        let Some(Update::Snapshot(second)) = rx.recv().await else {
            panic!("expected a snapshot, not Down")
        };
        assert!(!first.wants_connection && second.wants_connection);
        assert_eq!(counts.subscribes.load(Ordering::SeqCst), 2);
    }
}
