//! UDP socket set-up shared by the resolver and the SOCKS UDP relay.
//!
//! On Windows, a datagram sent to a closed local port comes back as ICMP port
//! unreachable, which Winsock reports as `ConnectionReset` on the socket's next
//! receive (WSAECONNRESET), even for an unconnected socket. A client that closes
//! before its answer arrives would then disturb everyone else's receive loop.
//! Linux reports no such error on unconnected sockets.

use std::io;
use tokio::net::UdpSocket;

/// Stops ICMP port unreachable from surfacing as `ConnectionReset` on receive
/// (Windows `SIO_UDP_CONNRESET = FALSE`). Nothing to do elsewhere.
#[cfg(windows)]
pub(crate) fn ignore_icmp_reset(socket: &UdpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET, WSAIoctl};

    let off: u32 = 0; // BOOL FALSE
    let mut returned = 0u32;
    // SAFETY: the socket stays open for the call (we hold a reference); the input is a
    // live 4-byte BOOL, there is no output buffer, and no OVERLAPPED, so the call is
    // synchronous and keeps no pointer.
    let rc = unsafe {
        WSAIoctl(
            SOCKET(socket.as_raw_socket() as usize),
            SIO_UDP_CONNRESET,
            Some(std::ptr::from_ref(&off).cast()),
            std::mem::size_of_val(&off) as u32,
            None,
            0,
            &mut returned,
            None,
            None,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn ignore_icmp_reset(_socket: &UdpSocket) -> io::Result<()> {
    Ok(())
}

/// A receive error that says nothing about the socket itself: a late ICMP
/// unreachable for an earlier send. The loop carries on at once.
pub(crate) fn is_icmp_reset(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ConnectionReset
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Sends from `socket` to a port that was just closed, then reports what the
    /// next receive sees within 300 ms.
    async fn receive_after_unreachable(socket: &UdpSocket) -> Option<io::Result<usize>> {
        let gone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = gone.local_addr().unwrap();
        drop(gone);
        socket.send_to(b"hello", addr).await.unwrap();
        let mut buf = [0u8; 16];
        tokio::time::timeout(Duration::from_millis(300), socket.recv_from(&mut buf))
            .await
            .ok()
            .map(|r| r.map(|(n, _)| n))
    }

    #[tokio::test]
    async fn without_the_ioctl_windows_reports_connection_reset() {
        // The premise: this machine does turn ICMP unreachable into ConnectionReset.
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        match receive_after_unreachable(&socket).await {
            Some(Err(error)) => assert!(is_icmp_reset(&error), "{error}"),
            other => panic!("expected ConnectionReset, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ignore_icmp_reset_keeps_receive_quiet() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        ignore_icmp_reset(&socket).unwrap();
        let seen = receive_after_unreachable(&socket).await;
        assert!(seen.is_none(), "receive must just wait, got {seen:?}");
    }
}
