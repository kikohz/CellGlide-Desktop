//! Client side of the control socket (a named pipe on Windows), used by cellglide and the GUI.

use crate::describe::{self, ui::ServiceDown};
use crate::{
    Check, CheckId, Endpoint, MAX_REPLY_LINE, Reason, Request, Response, Snapshot, from_line,
    read_line_limited, to_line,
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
        Ok(Response::Hello { protocol, .. }) => Err(ServiceDown::other(
            describe::version_mismatch(&protocol.to_string()),
        )),
        Ok(Response::Error {
            reason: Reason::NotInGroup,
        }) => Err(ServiceDown {
            text: describe::checks(&[Check::new(CheckId::Group, false)])
                .trim_end()
                .into(),
            not_in_group: true,
        }),
        // A v1 reply this client can parse but doesn't expect; unparsable ones are covered by `unreachable`.
        Ok(_) => Err(ServiceDown::other(describe::version_mismatch("1"))),
        Err(error) => Err(ServiceDown::other(describe::unreachable(&error))),
    }
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
/// after `retry` it starts over. Dropping the receiver ends it.
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

async fn watch_loop(endpoint: &Endpoint, retry: Duration, tx: &mpsc::Sender<Update>) {
    loop {
        let subscribed = match handshake(endpoint).await {
            Ok(()) => subscribe(endpoint)
                .await
                .map_err(|e| ServiceDown::other(describe::unreachable(&e))),
            Err(problem) => Err(problem),
        };
        let down = match subscribed {
            Ok(mut updates) => {
                while let Some(snapshot) = updates.recv().await {
                    if tx.send(Update::Snapshot(Box::new(snapshot))).await.is_err() {
                        return;
                    }
                }
                ServiceDown::other(tr("gui.service_gone", &[]))
            }
            Err(problem) => problem,
        };
        if tx.send(Update::Down(down)).await.is_err() {
            return;
        }
        tokio::time::sleep(retry).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }

    async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, behaviour: Behaviour) {
        if let Behaviour::Silent = behaviour {
            let _held = stream;
            std::future::pending::<()>().await;
            return;
        }
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = crate::read_line(&mut r).await {
            let request = crate::from_line::<Request>(&line).unwrap();
            let reply = match request {
                Request::Subscribe => {
                    let status = crate::to_line(&Response::Status {
                        snapshot: Snapshot::default(),
                    });
                    [status.clone(), status].concat()
                }
                Request::Hello if matches!(behaviour, Behaviour::Service) => {
                    crate::to_line(&Response::Hello {
                        protocol: crate::PROTOCOL_VERSION,
                        version: "test".into(),
                    })
                }
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

    #[cfg(unix)]
    fn test_server_as(behaviour: Behaviour) -> (Endpoint, Keep) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, behaviour));
            }
        });
        (Endpoint::Unix(path), dir)
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
    fn test_server_as(behaviour: Behaviour) -> (Endpoint, Keep) {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = random_pipe_name();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let pipe_name = name.clone();
        tokio::spawn(async move {
            loop {
                if server.connect().await.is_err() {
                    break;
                }
                let next = ServerOptions::new().create(&pipe_name).unwrap();
                tokio::spawn(serve(std::mem::replace(&mut server, next), behaviour));
            }
        });
        (Endpoint::Pipe(name), ())
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
}
