//! The forwarding engine that stands in for hev-socks5-tunnel where hev cannot be built
//! (Windows): raw IP packets from a TUN device in, every TCP connection and UDP flow out
//! as a SOCKS5 request to the local relay (cg-core's forwarder), with hev's settings
//! (`udp: 'udp'`, MTU, TCP and UDP timeouts) passed in by the caller.
//!
//! TCP is terminated by smoltcp (AnyIP: it answers for any destination); UDP is relayed
//! datagram by datagram. See `stack` for the loop, `tcp` and `udp` for the flows.

mod device;
mod socks;
mod stack;
mod tcp;
mod udp;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use device::PacketChannel;

use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Engine settings; callers take them from `cg_core::tunnel::ENGINE` and the forwarder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The TUN device's MTU (at least 1280).
    pub mtu: u16,
    /// The SOCKS5 relay every flow goes to.
    pub socks: SocketAddr,
    /// A TCP connection with no data either way for this long is reset.
    pub tcp_timeout: Duration,
    /// A UDP flow with no datagram either way for this long is released.
    pub udp_idle: Duration,
}

/// Packets queued between the device and the loop, each way.
const DEVICE_QUEUE: usize = 1024;
/// Largest packet read from the device.
const MAX_PACKET: usize = 65535;
/// How long `run` waits, once stopped, for the final RSTs to reach the device.
const FLUSH_ON_STOP: Duration = Duration::from_millis(200);

/// Runs the engine on `device` (each read yields one whole IP packet, each write takes
/// one) until `stop` is cancelled or the device closes; both end in `Ok(())` after every
/// session is closed. A device error is returned.
pub async fn run<D>(device: D, config: Config, stop: CancellationToken) -> io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if config.mtu < 1280 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mtu below 1280",
        ));
    }
    let (mut reader, mut writer) = tokio::io::split(device);
    let (in_tx, mut input) = mpsc::channel::<io::Result<Vec<u8>>>(DEVICE_QUEUE);
    let reader_task = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_PACKET];
        loop {
            let item = match reader.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => Ok(buf[..n].to_vec()),
                Err(error) => Err(error),
            };
            let failed = item.is_err();
            if in_tx.send(item).await.is_err() || failed {
                return;
            }
        }
    });
    let (out, mut out_rx) = mpsc::channel::<Vec<u8>>(DEVICE_QUEUE);
    let writer_task = tokio::spawn(async move {
        while let Some(packet) = out_rx.recv().await {
            writer.write_all(&packet).await?;
        }
        writer.flush().await
    });

    let sessions = stop.child_token();
    let mut stack = stack::Stack::new(config, out.clone(), sessions.clone());
    let kick = stack.kick.clone();
    let result: io::Result<()> = async {
        loop {
            let delay = stack.delay();
            tokio::select! {
                biased;
                _ = stop.cancelled() => return Ok(()),
                packet = input.recv() => {
                    let Some(packet) = packet else { return Ok(()) };
                    stack.input(packet?);
                    // Take what else is already waiting before polling.
                    for _ in 0..DEVICE_QUEUE {
                        match input.try_recv() {
                            Ok(packet) => stack.input(packet?),
                            Err(_) => break,
                        }
                    }
                }
                _ = kick.notified() => {}
                _ = sleep(delay) => {}
            }
            stack.process();
            for packet in stack.take_output() {
                out.send(packet).await.map_err(|_| writer_gone())?;
            }
        }
    }
    .await;

    // Close everything: RST to every client, then drop the SOCKS side.
    stack.abort_all();
    for packet in stack.take_output() {
        let _ = out.try_send(packet);
    }
    sessions.cancel();
    drop(stack);
    drop(out);
    reader_task.abort();
    let writer_abort = writer_task.abort_handle();
    let writer_result = match tokio::time::timeout(FLUSH_ON_STOP, writer_task).await {
        Ok(Ok(written)) => written,
        Ok(Err(_)) => Ok(()),
        Err(_) => {
            writer_abort.abort();
            Ok(())
        }
    };
    result?;
    if writer_result.is_err() {
        tracing::debug!("device write failed while stopping");
    }
    Ok(())
}

fn writer_gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "device write failed")
}

async fn sleep(delay: Option<Duration>) {
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => std::future::pending().await,
    }
}
