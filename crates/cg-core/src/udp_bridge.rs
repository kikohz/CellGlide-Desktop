//! One SOCKS5 UDP association, carried to the phone as udp-over-usb-v1 frames.
//! Only the first local sender (and the requested port, when one was given) is served.

use crate::Result;
use crate::forwarder::read_address;
use crate::mux::MuxStream;
use cg_protocol::socks::{
    REPLY_COMMAND_FAILED, UDP_OVER_USB_REQUEST, encode_frame, udp_associate_reply,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};

pub(crate) async fn associate(
    mut client: TcpStream,
    mut peer: MuxStream,
    request_address: &[u8],
) -> Result<()> {
    let n = request_address.len();
    let requested_port = u16::from_be_bytes([request_address[n - 2], request_address[n - 1]]);
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    crate::udp::ignore_icmp_reset(&udp)?;
    let udp = Arc::new(udp);
    let local_port = udp.local_addr()?.port();

    peer.write_all(&UDP_OVER_USB_REQUEST).await?;
    let mut reply = [0u8; 4];
    peer.read_exact(&mut reply).await?;
    read_address(peer.as_mut(), reply[3]).await?;
    if reply[..3] != [5, 0, 0] {
        client.write_all(&REPLY_COMMAND_FAILED).await?;
        return Ok(());
    }
    client.write_all(&udp_associate_reply(local_port)).await?;

    let source: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let (phone_r, phone_w) = tokio::io::split(peer);
    // RFC 1928: the association lives until its control connection closes; stray
    // bytes on it are not a close.
    let control = async move {
        let mut buf = [0u8; 64];
        while matches!(client.read(&mut buf).await, Ok(n) if n > 0) {}
    };
    tokio::select! {
        _ = uplink(udp.clone(), source.clone(), requested_port, phone_w) => {}
        _ = downlink(udp, source, phone_r) => {}
        _ = control => {}
    }
    Ok(())
}

type Source = Arc<Mutex<Option<SocketAddr>>>;

/// Local datagrams to the phone, from the association's one sender.
async fn uplink(
    udp: Arc<UdpSocket>,
    source: Source,
    requested_port: u16,
    mut phone: WriteHalf<MuxStream>,
) -> Result<()> {
    let mut buf = vec![0u8; 65535];
    loop {
        let (len, from) = match udp.recv_from(&mut buf).await {
            Ok(received) => received,
            // A late ICMP unreachable for an earlier send (Windows): not the end.
            Err(error) if crate::udp::is_icmp_reset(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if !from.ip().is_loopback() {
            continue;
        }
        let accepted = {
            let mut known = source.lock().unwrap();
            match *known {
                Some(addr) => addr == from,
                None if requested_port != 0 && from.port() != requested_port => false,
                None => {
                    *known = Some(from);
                    true
                }
            }
        };
        if accepted {
            phone.write_all(&encode_frame(&buf[..len])?).await?;
        }
    }
}

/// Frames from the phone back to the local sender (dropped until one is known).
async fn downlink(
    udp: Arc<UdpSocket>,
    source: Source,
    mut phone: ReadHalf<MuxStream>,
) -> Result<()> {
    loop {
        // Frame length is a big-endian u16, which is what read_u16 reads.
        let len = phone.read_u16().await? as usize;
        let mut payload = vec![0u8; len];
        phone.read_exact(&mut payload).await?;
        let target = *source.lock().unwrap();
        if let Some(target) = target {
            udp.send_to(&payload, target).await?;
        }
    }
}
