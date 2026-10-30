//! The control pipe's server end: `\\.\pipe\cellglide-control` with the service's security
//! descriptor ([`cg_ipc::PIPE_SDDL`]), local clients only, each identified from its token.

use super::auth;
use crate::access::{Access, Peer, Requester};
use crate::server::{BoxFuture, Listener};
use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::HSTRING;

pub struct PipeListener {
    name: String,
    sddl: String,
    /// The instance the next client connects to. `Some` whenever no accept is running, so
    /// the name never goes without an instance of ours.
    next: Option<NamedPipeServer>,
    #[cfg(test)]
    pub(crate) faults: Faults,
}

/// Creates the first instance of the pipe. If the name is already taken (another program
/// squatting on it, or a second service), this fails rather than joining that pipe.
pub fn bind(name: &str) -> io::Result<PipeListener> {
    bind_with(name, cg_ipc::PIPE_SDDL)
}

pub(crate) fn bind_with(name: &str, sddl: &str) -> io::Result<PipeListener> {
    let first = create(name, sddl, true).map_err(|e| {
        let taken = [ERROR_ACCESS_DENIED.0, ERROR_PIPE_BUSY.0]
            .iter()
            .any(|&code| e.raw_os_error() == Some(code as i32));
        let what = if taken {
            format!("{name} already exists: another program holds the control pipe ({e})")
        } else {
            format!("can't create {name}: {e}")
        };
        io::Error::new(e.kind(), what)
    })?;
    Ok(PipeListener {
        name: name.to_string(),
        sddl: sddl.to_string(),
        next: Some(first),
        #[cfg(test)]
        faults: Faults::default(),
    })
}

/// One instance of the pipe with `sddl`, refusing remote clients.
fn create(name: &str, sddl: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `descriptor` receives a LocalAlloc'd security descriptor, freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(auth::os_error)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: `attributes` and the descriptor it points to outlive the call.
    let created = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    };
    // SAFETY: allocated by the conversion above; freed exactly once.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    created
}

impl Listener for PipeListener {
    type Conn = NamedPipeServer;

    fn accept(&mut self) -> BoxFuture<'_, io::Result<(NamedPipeServer, Peer)>> {
        Box::pin(async move {
            // At least one of our instances exists at every moment: in a gap, anyone could
            // create the name with FILE_FLAG_FIRST_PIPE_INSTANCE and lock the service out.
            let pipe = match self.next.take() {
                Some(pipe) => pipe,
                None => self.create()?,
            };
            if let Err(error) = self.connect(&pipe).await {
                self.keep_listening(pipe);
                return Err(error);
            }
            // The next client finds an instance waiting before this one is handed over. If
            // there can't be one, this client is let go and its instance listens instead.
            match self.create() {
                Ok(next) => self.next = Some(next),
                Err(error) => {
                    self.keep_listening(pipe);
                    return Err(error);
                }
            }
            let peer = match auth::identify(&pipe) {
                Ok(id) => Peer {
                    access: auth::access(&id),
                    requester: Requester::Sid(id.sid),
                },
                Err(error) => {
                    tracing::warn!(%error, "can't identify a control pipe client");
                    Peer {
                        access: Access::Denied,
                        requester: Requester::Unknown,
                    }
                }
            };
            Ok((pipe, peer))
        })
    }
}

impl PipeListener {
    /// Makes `pipe` (connected, or failed to connect) the instance the next client connects
    /// to. If it can't be disconnected for reuse, a fresh instance replaces it, created before
    /// the old one closes; failing that, the old one stays so the name remains ours.
    fn keep_listening(&mut self, pipe: NamedPipeServer) {
        if pipe.disconnect().is_ok() {
            self.next = Some(pipe);
            return;
        }
        self.next = Some(self.create().unwrap_or(pipe));
    }

    /// Another instance of the pipe.
    fn create(&mut self) -> io::Result<NamedPipeServer> {
        #[cfg(test)]
        if self.faults.creates > 0 {
            self.faults.creates -= 1;
            return Err(io::Error::other("injected create failure"));
        }
        create(&self.name, &self.sddl, false)
    }

    /// Waits for a client on `pipe`.
    async fn connect(&mut self, pipe: &NamedPipeServer) -> io::Result<()> {
        #[cfg(test)]
        if self.faults.connects > 0 {
            self.faults.connects -= 1;
            return Err(io::Error::other("injected connect failure"));
        }
        pipe.connect().await
    }
}

/// Failures the tests inject: the next `creates` instances and `connects` connections fail.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Faults {
    pub creates: u32,
    pub connects: u32,
}

/// Enables SeRestorePrivilege in this process's token, which setting SYSTEM as the pipe's
/// owner takes outside the service (`--console` as an administrator, the tests); false if
/// the token lacks it.
pub fn enable_restore_privilege() -> bool {
    use windows::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        SE_RESTORE_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; `token` is closed by `Owned`.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .is_err()
    {
        return false;
    }
    let token = auth::Owned(token);
    let mut luid = LUID::default();
    // SAFETY: plain out-parameter.
    if unsafe { LookupPrivilegeValueW(None, SE_RESTORE_NAME, &mut luid) }.is_err() {
        return false;
    }
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: valid token and a TOKEN_PRIVILEGES of one entry.
    let adjusted =
        unsafe { AdjustTokenPrivileges(token.0, false, Some(&privileges), 0, None, None) };
    // SAFETY: reads this thread's last error, set by the call above.
    adjusted.is_ok() && unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::server::test_support::spawn_server;
    use crate::windows::auth::testing;
    use cg_ipc::{Endpoint, Request, Response, client};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncWriteExt, BufReader};
    use windows::Win32::Security::{ImpersonateLoggedOnUser, RevertToSelf};

    pub(crate) fn random_pipe_name() -> String {
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
            r"\\.\pipe\cellglide-test-{}-{nanos}-{n}",
            std::process::id()
        )
    }

    /// The service's descriptor with the interactive users' ACE granted to the test user
    /// instead (this shell isn't an interactive logon), and no owner (setting SYSTEM as owner
    /// takes SeRestorePrivilege; see `bind_uses_the_service_descriptor`).
    pub(crate) fn test_sddl() -> String {
        let sddl = cg_ipc::PIPE_SDDL.replacen("O:SY", "", 1);
        assert!(sddl.contains(";;;IU)"), "{sddl}");
        sddl.replace(";;;IU)", &format!(";;;{})", testing::my_sid()))
    }

    /// Runs `f` on this thread as this user with BUILTIN\Administrators deny-only, so only
    /// the user's own ACE (the service's interactive-user rights) grants access.
    fn as_plain_user<T>(f: impl FnOnce() -> T) -> T {
        let token = testing::without_administrators();
        // SAFETY: a valid token; undone right after `f` on the same thread.
        unsafe { ImpersonateLoggedOnUser(token.0) }.unwrap();
        let result = f();
        // SAFETY: ends the impersonation started above.
        unsafe { RevertToSelf() }.unwrap();
        result
    }

    #[test]
    fn the_service_descriptor_parses() {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(cg_ipc::PIPE_SDDL),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .unwrap();
        assert!(!descriptor.0.is_null());
        // SAFETY: freed exactly once.
        let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        // Interactive users: generic read, write data/EA/attributes, and not
        // FILE_CREATE_PIPE_INSTANCE (0x4), so they can't add instances of the service's pipe.
        assert!(cg_ipc::PIPE_SDDL.ends_with("(A;;0x12019b;;;IU)"));
        assert_eq!(0x12019b & 0x4, 0);
    }

    #[tokio::test]
    async fn pipe_round_trip_identifies_this_process() {
        let name = random_pipe_name();
        let mut listener = bind_with(&name, &test_sddl()).unwrap();

        // The server sees who connected.
        let endpoint = Endpoint::Pipe(name.clone());
        let asking = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { client::request(&endpoint, &Request::Hello).await }
        });
        let (pipe, peer) = listener.accept().await.unwrap();
        let id = auth::identify(&pipe).unwrap();
        assert_eq!(id.sid, testing::my_sid());
        assert!(!id.is_system);
        assert_eq!(peer.requester, Requester::Sid(id.sid.clone()));
        assert_eq!(peer.access, auth::access(&id));
        drop(pipe);
        assert!(
            asking.await.unwrap().is_err(),
            "the server hung up unanswered"
        );

        // Served for real: Hello comes back, connection after connection.
        let dir = tempfile::tempdir().unwrap();
        let _mux = spawn_server(dir.path(), listener).await;
        for _ in 0..3 {
            assert!(matches!(
                client::request(&endpoint, &Request::Hello).await.unwrap(),
                Response::Hello {
                    protocol: cg_ipc::PROTOCOL_VERSION,
                    ..
                }
            ));
        }

        // With only the interactive users' rights, a client still connects, writes and reads…
        let plain =
            as_plain_user(|| tokio::net::windows::named_pipe::ClientOptions::new().open(&name))
                .expect("interactive users' rights open the pipe");
        let mut plain = BufReader::new(plain);
        plain
            .get_mut()
            .write_all(&cg_ipc::to_line(&Request::Status))
            .await
            .unwrap();
        let line = cg_ipc::read_line(&mut plain).await.unwrap().unwrap();
        assert!(matches!(
            cg_ipc::from_line::<Response>(&line).unwrap(),
            Response::Status { .. }
        ));
        // …but can't plant an instance of its own.
        let planted = as_plain_user(|| ServerOptions::new().create(&name));
        assert_eq!(
            planted.err().and_then(|e| e.raw_os_error()),
            Some(ERROR_ACCESS_DENIED.0 as i32)
        );
    }

    /// Clients that open the pipe and drop it at once, in tight loops on two threads, and a
    /// squatter trying all along to create the name as its first instance.
    struct Flood {
        stop: Arc<AtomicBool>,
        droppers: Vec<std::thread::JoinHandle<u32>>,
        squatter: tokio::task::JoinHandle<u32>,
        squatted: Arc<AtomicU32>,
    }

    /// What a flood did: connections dropped, squatting attempts, and how many succeeded.
    struct Flooded {
        dropped: u32,
        tries: u32,
        squatted: u32,
    }

    impl Flood {
        fn start(name: &str) -> Flood {
            let stop = Arc::new(AtomicBool::new(false));
            let droppers = (0..2)
                .map(|_| {
                    let (name, stop) = (name.to_string(), stop.clone());
                    std::thread::spawn(move || {
                        let mut opened = 0u32;
                        while !stop.load(Ordering::Relaxed) {
                            let open = std::fs::OpenOptions::new()
                                .read(true)
                                .write(true)
                                .open(&name);
                            opened += u32::from(open.is_ok());
                        }
                        opened
                    })
                })
                .collect();
            let squatted = Arc::new(AtomicU32::new(0));
            let squatter = tokio::task::spawn_blocking({
                let (name, stop, squatted) = (name.to_string(), stop.clone(), squatted.clone());
                move || {
                    let mut tries = 0u32;
                    while !stop.load(Ordering::Relaxed) {
                        let planted = ServerOptions::new().first_pipe_instance(true).create(&name);
                        squatted.fetch_add(u32::from(planted.is_ok()), Ordering::Relaxed);
                        tries += 1;
                    }
                    tries
                }
            });
            Flood {
                stop,
                droppers,
                squatter,
                squatted,
            }
        }

        async fn finish(mut self) -> Flooded {
            self.stop.store(true, Ordering::Relaxed);
            let tries = (&mut self.squatter).await.unwrap();
            let dropped = std::mem::take(&mut self.droppers)
                .into_iter()
                .map(|t| t.join().unwrap())
                .sum();
            Flooded {
                dropped,
                tries,
                squatted: self.squatted.load(Ordering::Relaxed),
            }
        }
    }

    impl Drop for Flood {
        /// Stops the loops however a test ends, so a failure can't hang it.
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Hello, answered three times in a row.
    async fn served_at_once(endpoint: &Endpoint) {
        for _ in 0..3 {
            let reply = client::request(endpoint, &Request::Hello).await;
            assert!(matches!(reply, Ok(Response::Hello { .. })), "{reply:?}");
        }
    }

    /// Clients opening and dropping the pipe in a tight loop never leave the name without
    /// an instance of ours: a squatter's first instance is never created, and a real client
    /// never finds the name missing or someone else's. The flood can keep every instance
    /// busy past a client's one-second patience (first come, first served; not this bug),
    /// so ERROR_PIPE_BUSY is the one failure allowed; once the flood stops, it is served.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clients_that_drop_at_once_never_free_the_name() {
        let name = random_pipe_name();
        let listener = bind_with(&name, &test_sddl()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _mux = spawn_server(dir.path(), listener).await;
        let flood = Flood::start(&name);

        let endpoint = Endpoint::Pipe(name.clone());
        let deadline = Instant::now() + Duration::from_secs(3);
        let (mut served, mut busy) = (0u32, 0u32);
        while Instant::now() < deadline {
            let reply = tokio::time::timeout(
                Duration::from_secs(5),
                client::request(&endpoint, &Request::Hello),
            )
            .await
            .expect("a real client hung");
            match reply {
                Ok(Response::Hello { .. }) => served += 1,
                Err(cg_ipc::client::ClientError::Io(e))
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) =>
                {
                    busy += 1
                }
                other => panic!("a real client failed after {served} served: {other:?}"),
            }
        }
        let flooded = flood.finish().await;
        assert!(served + busy > 0);
        assert!(flooded.dropped > 100, "dropped {}", flooded.dropped);
        assert!(flooded.tries > 100, "tried {}", flooded.tries);
        assert_eq!(flooded.squatted, 0, "the name was free");
        served_at_once(&endpoint).await;
    }

    /// When connecting fails, or creating the next instance does, the instance in hand
    /// keeps listening (a connected client is let go rather than handed over with no
    /// instance behind it), so the name is never free for a squatter, and the service
    /// serves again once the failures stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failures_never_free_the_name() {
        let name = random_pipe_name();
        let mut listener = bind_with(&name, &test_sddl()).unwrap();
        listener.faults = Faults {
            creates: 20,
            connects: 20,
        };
        let dir = tempfile::tempdir().unwrap();
        let _mux = spawn_server(dir.path(), listener).await;
        let flood = Flood::start(&name);
        // serve backs off 100 ms after each failure: 40 of them take about 4 s.
        tokio::time::sleep(Duration::from_secs(6)).await;
        let flooded = flood.finish().await;
        assert!(flooded.tries > 100, "tried {}", flooded.tries);
        assert_eq!(flooded.squatted, 0, "the name was free");
        served_at_once(&Endpoint::Pipe(name)).await;
    }

    #[tokio::test]
    async fn a_squatted_name_is_refused() {
        let name = random_pipe_name();
        let _squatter = ServerOptions::new().create(&name).unwrap();
        let error = bind_with(&name, &test_sddl()).err().unwrap();
        assert!(error.to_string().contains(&name), "{error}");
        assert!(error.to_string().contains("already exists"), "{error}");
    }

    /// `bind` itself, with SYSTEM as owner: possible here only with SeRestorePrivilege, which
    /// an elevated test process has (skipped otherwise). Clients then see a SYSTEM-owned pipe.
    #[tokio::test]
    async fn bind_uses_the_service_descriptor() {
        if !enable_restore_privilege() {
            eprintln!("skipped: SeRestorePrivilege unavailable (not elevated)");
            return;
        }
        let name = random_pipe_name();
        let mut listener = bind(&name).unwrap();
        let opening = tokio::spawn({
            let name = name.clone();
            async move {
                let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(&name)
                    .unwrap();
                let is_service = cg_ipc::pipe_identity::server_is_cellglide_service(&pipe);
                (pipe, is_service)
            }
        });
        let (_server, _peer) = listener.accept().await.unwrap();
        let (_pipe, is_service) = opening.await.unwrap();
        // The owner is SYSTEM; the verdict then rests on the session this test runs in.
        assert_eq!(is_service.unwrap(), this_session() == 0);
    }

    fn this_session() -> u32 {
        let mut session = u32::MAX;
        // SAFETY: plain out-parameter.
        unsafe {
            windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                std::process::id(),
                &mut session,
            )
        }
        .unwrap();
        session
    }
}
