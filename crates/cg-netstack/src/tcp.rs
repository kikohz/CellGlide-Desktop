//! TCP: each connection smoltcp accepts gets a task that does the SOCKS5 CONNECT and
//! shuttles bytes. The poll loop owns the smoltcp socket; the two sides meet through
//! bounded channels, so a slow side holds the other back instead of buffering without end.
//!
//! Half-close travels both ways: the client's FIN (once its data is read) becomes a
//! write shutdown towards the SOCKS server, and the server's EOF becomes our FIN, while
//! the other direction keeps flowing.

use crate::socks;
use smoltcp::socket::tcp::{self, State};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// Receive and send buffer of each smoltcp socket. 256 KiB lets one flow keep the USB
/// link busy (the window is scaled when the client offers window scaling); 256 flows
/// at most would hold 128 MiB.
pub(crate) const SOCKET_BUFFER: usize = 256 * 1024;
/// Largest piece moved through a channel at once.
pub(crate) const CHUNK: usize = 16 * 1024;
/// Pieces in flight between the loop and a flow task, each way.
const QUEUE: usize = 8;

/// From the client, for the SOCKS server.
pub(crate) enum Up {
    Data(Vec<u8>),
    /// The client sent FIN and everything before it has been delivered.
    Fin,
}

/// From the SOCKS server, for the client.
pub(crate) enum Down {
    Data(Vec<u8>),
    /// The server closed its side; we send FIN after the data before it.
    Fin,
}

/// A fresh socket for an incoming connection.
pub(crate) fn new_socket(timeout: Duration) -> tcp::Socket<'static> {
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
        tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
    );
    // Data from the relay is already batched; Nagle would only add latency.
    socket.set_nagle_enabled(false);
    // Gives up on a client that stops acknowledging outstanding data.
    socket.set_timeout(Some(smoltcp::time::Duration::from_micros(
        timeout.as_micros().try_into().unwrap_or(u64::MAX),
    )));
    socket
}

/// The poll loop's side of one connection.
pub(crate) struct Session {
    pub client: SocketAddr,
    pub target: SocketAddr,
    /// Dropped (without `Up::Fin`) to tell the flow task the client is gone.
    up: Option<mpsc::Sender<Up>>,
    down: mpsc::Receiver<Down>,
    /// Set by the flow task: reset the client (SOCKS refused, relay failed, idle).
    abort: CancellationToken,
    /// Data from `down` that did not fit in the socket yet: bytes and how many are sent.
    pending: Option<(Vec<u8>, usize)>,
    peer_fin: bool,
    fin_up: bool,
    /// Our side is closed (FIN queued) or aborted; `down` is no longer read.
    closed: bool,
}

pub(crate) struct Flow {
    pub socks: SocketAddr,
    pub target: SocketAddr,
    pub idle: Duration,
    pub kick: Arc<Notify>,
    pub cancel: CancellationToken,
}

impl Session {
    /// Starts the flow task for a connection from `client` to `flow.target`.
    pub fn spawn(client: SocketAddr, flow: Flow) -> Session {
        let (up_tx, up_rx) = mpsc::channel(QUEUE);
        let (down_tx, down_rx) = mpsc::channel(QUEUE);
        let abort = CancellationToken::new();
        let target = flow.target;
        tokio::spawn(flow.run(up_rx, down_tx, abort.clone()));
        Session {
            client,
            target,
            up: Some(up_tx),
            down: down_rx,
            abort,
            pending: None,
            peer_fin: false,
            fin_up: false,
            closed: false,
        }
    }

    /// Moves what it can between the socket and the flow task; true when anything moved.
    pub fn pump(&mut self, socket: &mut tcp::Socket) -> bool {
        let mut progress = false;
        if self.abort.is_cancelled() && !self.closed {
            socket.abort();
            self.closed = true;
            self.up = None;
            self.pending = None;
            return true;
        }
        if matches!(
            socket.state(),
            State::CloseWait | State::LastAck | State::Closing | State::TimeWait
        ) {
            self.peer_fin = true;
        }

        // Client to server.
        if let Some(up) = &self.up {
            while socket.can_recv() {
                let Ok(permit) = up.try_reserve() else { break };
                match socket.recv(|buf| {
                    let n = buf.len().min(CHUNK);
                    (n, buf[..n].to_vec())
                }) {
                    Ok(data) if !data.is_empty() => {
                        permit.send(Up::Data(data));
                        progress = true;
                    }
                    _ => break,
                }
            }
            if self.peer_fin
                && !self.fin_up
                && !socket.can_recv()
                && let Ok(permit) = up.try_reserve()
            {
                permit.send(Up::Fin);
                self.fin_up = true;
                progress = true;
            }
        }

        // Server to client.
        loop {
            if let Some((data, sent)) = &mut self.pending {
                let n = match socket.send_slice(&data[*sent..]) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                *sent += n;
                progress |= n > 0;
                if *sent < data.len() {
                    break;
                }
                self.pending = None;
                continue;
            }
            if self.closed {
                break;
            }
            match self.down.try_recv() {
                Ok(Down::Data(data)) => self.pending = Some((data, 0)),
                Ok(Down::Fin) => {
                    socket.close();
                    self.closed = true;
                    progress = true;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // The flow task ended without saying how: reset rather than pretend.
                    socket.abort();
                    self.closed = true;
                    self.up = None;
                    progress = true;
                }
            }
        }
        progress
    }

    /// The socket is done: closed with nothing left to send (an RST from `abort` has
    /// gone out once the endpoint is forgotten), or back to LISTEN after an RST in
    /// SYN-RECEIVED.
    pub fn finished(socket: &tcp::Socket) -> bool {
        match socket.state() {
            State::Listen => true,
            State::Closed => socket.remote_endpoint().is_none(),
            _ => false,
        }
    }
}

/// Kicks the poll loop when dropped.
struct Wake(Arc<Notify>);

impl Drop for Wake {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl Flow {
    async fn run(
        self,
        mut up: mpsc::Receiver<Up>,
        down: mpsc::Sender<Down>,
        abort: CancellationToken,
    ) {
        // However this task ends, the loop looks at the session again.
        let _wake = Wake(self.kick.clone());
        let reset = || abort.cancel();
        let connected = tokio::select! {
            _ = self.cancel.cancelled() => return,
            connected = socks::connect(self.socks, self.target) => connected,
        };
        let stream = match connected {
            Ok(stream) => stream,
            Err(error) => {
                tracing::debug!(target = %self.target, %error, "socks connect failed");
                return reset();
            }
        };
        let (mut reader, mut writer) = stream.into_split();
        let (mut up_open, mut down_open) = (true, true);
        let mut buf = vec![0u8; CHUNK];
        let idle = tokio::time::sleep(self.idle);
        tokio::pin!(idle);
        while up_open || down_open {
            tokio::select! {
                _ = self.cancel.cancelled() => return,
                _ = &mut idle => {
                    tracing::debug!(target = %self.target, "tcp idle timeout");
                    return reset();
                }
                item = up.recv(), if up_open => {
                    // Room in the channel again.
                    self.kick.notify_one();
                    match item {
                        Some(Up::Data(data)) => {
                            if writer.write_all(&data).await.is_err() {
                                return reset();
                            }
                            idle.as_mut().reset(tokio::time::Instant::now() + self.idle);
                        }
                        Some(Up::Fin) => {
                            let _ = writer.shutdown().await;
                            up_open = false;
                        }
                        // The client reset, or the engine is stopping.
                        None => return,
                    }
                }
                read = reader.read(&mut buf), if down_open => {
                    let item = match read {
                        Ok(0) => {
                            down_open = false;
                            Down::Fin
                        }
                        Ok(n) => {
                            idle.as_mut().reset(tokio::time::Instant::now() + self.idle);
                            Down::Data(buf[..n].to_vec())
                        }
                        Err(_) => return reset(),
                    };
                    if down.send(item).await.is_err() {
                        return;
                    }
                    self.kick.notify_one();
                }
            }
        }
    }
}
