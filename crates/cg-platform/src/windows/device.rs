//! The wintun session as cg-netstack's packet device: two dedicated threads, one waiting on
//! the session's read event and handing each received packet to the engine, one taking
//! the engine's packets and queueing them on the session's send ring.

use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;

use cg_netstack::PacketChannel;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio_util::sync::CancellationToken;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, WAIT_FAILED};
use windows::Win32::System::Threading::WaitForSingleObject;

use super::wintun::Session;

/// Packets queued each way between the threads and the engine.
const QUEUE: usize = 1024;
/// How long the receiving thread waits on the read event before looking at `stop` again;
/// well under the one second both threads have to exit in.
const WAIT_MS: u32 = 100;
/// `WINTUN_MAX_IP_PACKET_SIZE`.
const MAX_PACKET: usize = 0xFFFF;

/// The two bridge threads.
pub struct JoinHandles {
    receive: JoinHandle<()>,
    send: JoinHandle<()>,
}

impl JoinHandles {
    /// Waits for both threads (after `stop` is cancelled or the engine's end is gone).
    pub fn join(self) {
        let _ = self.receive.join();
        let _ = self.send.join();
    }

    pub fn is_finished(&self) -> bool {
        self.receive.is_finished() && self.send.is_finished()
    }
}

/// Starts the two threads on `session` and returns the engine's end of the link. Both
/// threads exit within a second of `stop` being cancelled, and on their own once the
/// engine drops its end or the session fails (the adapter going away). A failing
/// session call cancels `stop`, so the other thread and the engine wind down too.
pub fn bridge(
    session: Arc<Session>,
    stop: CancellationToken,
) -> io::Result<(PacketChannel, JoinHandles)> {
    let (engine, ours) = PacketChannel::pair(QUEUE);
    let (from_engine, to_engine) = tokio::io::split(ours);
    // Each thread drives its half of the channel on a runtime of its own (no I/O or timer
    // drivers: only the channel and the token are awaited).
    let runtime = || tokio::runtime::Builder::new_current_thread().build();
    let (receive_rt, send_rt) = (runtime()?, runtime()?);

    let receive = {
        let (session, stop) = (Arc::clone(&session), stop.clone());
        std::thread::Builder::new()
            .name("wintun-receive".into())
            .spawn(move || receive_rt.block_on(receive_loop(&session, to_engine, &stop)))?
    };
    let send = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("wintun-send".into())
            .spawn(move || send_rt.block_on(send_loop(&session, from_engine, &stop)))
    };
    let send = match send {
        Ok(send) => send,
        Err(error) => {
            stop.cancel();
            let _ = receive.join();
            return Err(error);
        }
    };
    Ok((engine, JoinHandles { receive, send }))
}

/// Session -> engine. Blocking on the read event is fine here: this runtime runs nothing
/// else.
async fn receive_loop(
    session: &Session,
    mut to_engine: WriteHalf<PacketChannel>,
    stop: &CancellationToken,
) {
    let event = session.read_event();
    while !stop.is_cancelled() {
        match session.try_receive() {
            Ok(Some(packet)) => {
                tokio::select! {
                    written = to_engine.write_all(&packet) => {
                        if written.is_err() {
                            return; // the engine is gone
                        }
                    }
                    () = stop.cancelled() => return,
                }
            }
            Ok(None) => {
                // SAFETY: the event belongs to the session, which outlives this loop.
                if unsafe { WaitForSingleObject(event, WAIT_MS) } == WAIT_FAILED {
                    tracing::warn!(
                        "waiting for wintun packets failed: {}",
                        io::Error::last_os_error()
                    );
                    stop.cancel();
                    return;
                }
            }
            Err(error) => {
                tracing::warn!("wintun receive ended: {error}");
                stop.cancel();
                return;
            }
        }
    }
}

/// Engine -> session. A full send ring drops the packet, as a NIC would.
async fn send_loop(
    session: &Session,
    mut from_engine: ReadHalf<PacketChannel>,
    stop: &CancellationToken,
) {
    let mut buf = vec![0u8; MAX_PACKET];
    loop {
        let n = tokio::select! {
            read = from_engine.read(&mut buf) => match read {
                Ok(0) | Err(_) => return, // the engine is gone
                Ok(n) => n,
            },
            () = stop.cancelled() => return,
        };
        if let Err(error) = session.send(&buf[..n]) {
            let full = error.raw_os_error() == Some(ERROR_BUFFER_OVERFLOW.0 as i32);
            if !full && error.kind() != io::ErrorKind::InvalidInput {
                tracing::warn!("wintun send ended: {error}");
                stop.cancel();
                return;
            }
        }
    }
}
