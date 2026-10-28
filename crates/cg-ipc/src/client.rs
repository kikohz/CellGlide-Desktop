//! Client side of the control socket, used by cellglide and cellglide-gui.

use crate::{MAX_REPLY_LINE, Request, Response, Snapshot, from_line, read_line_limited, to_line};
use std::path::Path;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("can't reach the CellGlide service: {0}")]
    Io(#[from] std::io::Error),
    #[error("unreadable reply: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the service closed the connection")]
    Closed,
}

pub async fn request(path: &Path, request: &Request) -> Result<Response, ClientError> {
    let stream = UnixStream::connect(path).await?;
    let (r, mut w) = stream.into_split();
    w.write_all(&to_line(request)).await?;
    let line = read_line_limited(&mut BufReader::new(r), MAX_REPLY_LINE)
        .await?
        .ok_or(ClientError::Closed)?;
    Ok(from_line(&line)?)
}

/// Snapshots as the service publishes them, starting with the current one.
/// The receiver closes when the service goes away.
pub async fn subscribe(path: &Path) -> Result<mpsc::Receiver<Snapshot>, ClientError> {
    let stream = UnixStream::connect(path).await?;
    let (r, mut w) = stream.into_split();
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
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn request_and_subscribe_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut r = BufReader::new(r);
                    while let Some(line) = crate::read_line(&mut r).await.unwrap() {
                        match crate::from_line::<Request>(&line).unwrap() {
                            Request::Subscribe => {
                                for _ in 0..2 {
                                    let snap = Snapshot::default();
                                    w.write_all(&crate::to_line(&Response::Status {
                                        snapshot: snap,
                                    }))
                                    .await
                                    .unwrap();
                                }
                            }
                            _ => w.write_all(&crate::to_line(&Response::Ok)).await.unwrap(),
                        }
                    }
                });
            }
        });
        assert_eq!(
            request(&path, &Request::Disconnect).await.unwrap(),
            Response::Ok
        );
        let mut rx = subscribe(&path).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
    }

    #[tokio::test]
    async fn missing_socket_is_an_io_error() {
        let err = request(Path::new("/nonexistent/cellglide.sock"), &Request::Status)
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Io(_)));
    }
}
