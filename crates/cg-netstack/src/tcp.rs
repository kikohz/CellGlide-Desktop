//! TCP: each connection smoltcp accepts gets a task that does the SOCKS5 CONNECT (once
//! the handshake is complete) and shuttles bytes. The poll loop owns the smoltcp socket;
//! the two sides meet through bounded channels, so a slow side holds the other back
//! instead of buffering without end.
//!
//! The two directions run independently: one that cannot make progress (a server that
//! does not read, a client that does not read) never holds up the other. Half-close
//! travels both ways: the client's FIN (once its data is read) becomes a write shutdown
//! towards the SOCKS server, and the server's EOF becomes our FIN, while the other
//! direction keeps flowing.

use crate::socks;
use smoltcp::socket::tcp::{self, State};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Receive and send buffer of each smoltcp socket. 256 KiB lets one flow keep the USB
/// link busy (the window is scaled when the client offers window scaling).
pub(crate) const SOCKET_BUFFER: usize = 256 * 1024;
/// Most TCP connections at once (handshaking ones included); further SYNs get RST.
/// Twice the forwarder's 256 sessions, so connections it turns away can still be
/// answered with RST; bounds the socket buffers at 512 × 512 KiB = 256 MiB.
pub(crate) const MAX_SESSIONS: usize = 512;
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

/// The flow task's ends of the channels, until the handshake completes.
struct Start {
    flow: Flow,
    up: mpsc::Receiver<Up>,
    down: mpsc::Sender<Down>,
}

/// The poll loop's side of one connection.
pub(crate) struct Session {
    pub client: SocketAddr,
    pub target: SocketAddr,
    /// Present until the flow task is spawned (when the client completes the handshake).
    start: Option<Start>,
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
    /// A connection from `client` to `flow.target`, still in its handshake.
    pub fn new(client: SocketAddr, flow: Flow) -> Session {
        let (up_tx, up_rx) = mpsc::channel(QUEUE);
        let (down_tx, down_rx) = mpsc::channel(QUEUE);
        let target = flow.target;
        Session {
            client,
            target,
            start: Some(Start {
                flow,
                up: up_rx,
                down: down_tx,
            }),
            up: Some(up_tx),
            down: down_rx,
            abort: CancellationToken::new(),
            pending: None,
            peer_fin: false,
            fin_up: false,
            closed: false,
        }
    }

    /// Moves what it can between the socket and the flow task; true when anything moved.
    pub fn pump(&mut self, socket: &mut tcp::Socket) -> bool {
        // The SOCKS CONNECT waits for a complete handshake: a SYN alone (or a SYN flood)
        // costs the phone nothing.
        if self.start.is_some() {
            if matches!(
                socket.state(),
                State::Listen | State::SynSent | State::SynReceived | State::Closed
            ) {
                return false;
            }
            let start = self.start.take().unwrap();
            tokio::spawn(start.flow.run(start.up, start.down, self.abort.clone()));
        }

        let mut progress = false;
        if self.abort.is_cancelled() && (self.up.is_some() || !self.closed) {
            if !self.closed {
                socket.abort();
            }
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

        // Client to server. A closed socket keeps its received data until read.
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

    /// The connection is over: the socket is closed with nothing left to send (an RST
    /// from `abort` has gone out once the endpoint is forgotten) and the client's data
    /// and FIN have been handed to the flow task (unless it is gone); or the socket fell
    /// back to LISTEN after an RST in SYN-RECEIVED.
    pub fn finished(&self, socket: &tcp::Socket) -> bool {
        match socket.state() {
            State::Listen => true,
            State::Closed if socket.remote_endpoint().is_none() => {
                let flow_gone = self.up.as_ref().is_none_or(|up| up.is_closed());
                let undelivered = self.peer_fin && !self.fin_up;
                self.start.is_some() || flow_gone || !undelivered
            }
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

/// Why a flow stopped early.
enum End {
    /// Reset the client and the SOCKS connection.
    Reset,
    /// The session is gone (client reset, engine stopping): just close.
    Gone,
}

/// The last time data moved either way.
struct Activity(Mutex<Instant>);

impl Activity {
    fn touch(&self) {
        *self.0.lock().unwrap() = Instant::now();
    }

    fn last(&self) -> Instant {
        *self.0.lock().unwrap()
    }

    /// Returns once nothing has moved for `idle`.
    async fn expired(&self, idle: Duration) {
        loop {
            let deadline = self.last() + idle;
            if Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep_until(deadline).await;
        }
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
        let connected = tokio::select! {
            _ = self.cancel.cancelled() => return,
            connected = socks::connect(self.socks, self.target) => connected,
        };
        let stream = match connected {
            Ok(stream) => stream,
            Err(error) => {
                tracing::debug!(target = %self.target, %error, "socks connect failed");
                return abort.cancel();
            }
        };
        let (mut reader, mut writer) = stream.into_split();
        let activity = Activity(Mutex::new(Instant::now()));
        // Every await of both directions sits under this select, so stopping and the
        // idle timeout apply even while one of them waits on a full buffer.
        let end = tokio::select! {
            _ = self.cancel.cancelled() => End::Gone,
            _ = activity.expired(self.idle) => {
                tracing::debug!(target = %self.target, "tcp idle timeout");
                End::Reset
            }
            done = async {
                tokio::try_join!(
                    uplink(&mut up, &mut writer, &activity, &self.kick),
                    downlink(&mut reader, &down, &activity, &self.kick),
                )
            } => match done {
                Ok(_) => return,
                Err(end) => end,
            },
        };
        if let End::Reset = end {
            abort.cancel();
            // RST towards the relay too, so the phone drops its side at once.
            if let Ok(stream) = reader.reunite(writer) {
                let _ = stream.set_zero_linger();
            }
        }
    }
}

/// Client to SOCKS server, until the client's FIN.
async fn uplink(
    up: &mut mpsc::Receiver<Up>,
    writer: &mut OwnedWriteHalf,
    activity: &Activity,
    kick: &Notify,
) -> Result<(), End> {
    loop {
        let item = up.recv().await;
        // Room in the channel again.
        kick.notify_one();
        match item {
            Some(Up::Data(data)) => {
                writer.write_all(&data).await.map_err(|_| End::Reset)?;
                activity.touch();
            }
            Some(Up::Fin) => {
                let _ = writer.shutdown().await;
                return Ok(());
            }
            None => return Err(End::Gone),
        }
    }
}

/// SOCKS server to client, until the server's EOF; notices the session going away
/// even while the server is silent.
async fn downlink(
    reader: &mut OwnedReadHalf,
    down: &mpsc::Sender<Down>,
    activity: &Activity,
    kick: &Notify,
) -> Result<(), End> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = tokio::select! {
            read = reader.read(&mut buf) => read,
            _ = down.closed() => return Err(End::Gone),
        };
        let item = match read {
            Ok(0) => Down::Fin,
            Ok(n) => {
                activity.touch();
                Down::Data(buf[..n].to_vec())
            }
            Err(_) => return Err(End::Reset),
        };
        let fin = matches!(item, Down::Fin);
        down.send(item).await.map_err(|_| End::Gone)?;
        kick.notify_one();
        if fin {
            return Ok(());
        }
    }
}
