//! Client side of the control socket (a named pipe on Windows), used by cellglide and the GUI.

use crate::{
    Endpoint, MAX_REPLY_LINE, Request, Response, Snapshot, from_line, read_line_limited, to_line,
};
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
    /// The control pipe's server isn't LocalSystem, so it isn't the CellGlide service.
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

/// Only the service's own pipe name is checked; tests serve pipes of their own.
#[cfg(windows)]
fn verifies(name: &str) -> bool {
    name.eq_ignore_ascii_case(crate::PIPE_NAME)
}

/// Opens a pipe, retrying for up to a second while every instance is busy. With `verify`, a
/// server that isn't LocalSystem is refused before a single byte is written to it.
#[cfg(windows)]
pub(crate) async fn connect_pipe(
    name: &str,
    verify: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ClientError> {
    use std::time::Duration;
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
    if verify && !crate::pipe_identity::server_is_local_system(&pipe)? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

    /// Answers every request with `Ok` and a subscription with two default snapshots.
    async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S) {
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = crate::read_line(&mut r).await {
            let reply = match crate::from_line::<Request>(&line).unwrap() {
                Request::Subscribe => {
                    let status = crate::to_line(&Response::Status {
                        snapshot: Snapshot::default(),
                    });
                    [status.clone(), status].concat()
                }
                _ => crate::to_line(&Response::Ok),
            };
            if w.write_all(&reply).await.is_err() {
                break;
            }
        }
    }

    #[cfg(unix)]
    fn test_server() -> (Endpoint, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream));
            }
        });
        (Endpoint::Unix(path), dir)
    }

    #[cfg(windows)]
    fn random_pipe_name() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(r"\\.\pipe\cellglide-test-{}-{nanos}", std::process::id())
    }

    #[cfg(windows)]
    fn test_server() -> (Endpoint, ()) {
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
                tokio::spawn(serve(std::mem::replace(&mut server, next)));
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

    /// A pipe that Windows serves from a LocalSystem svchost passes the check. Run elevated: a
    /// non-elevated caller can't open a LocalSystem process (measured in Task 7).
    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn accepts_a_local_system_server() {
        connect_pipe(r"\\.\pipe\srvsvc", true).await.unwrap();
    }

    /// The system pipe is only trusted from the service's own name.
    #[cfg(windows)]
    #[test]
    fn only_the_system_pipe_is_verified() {
        assert!(verifies(crate::PIPE_NAME));
        assert!(!verifies(r"\\.\pipe\cellglide-test"));
    }
}
