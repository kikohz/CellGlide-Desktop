//! The SOCKS5 client side, as the forwarder (cg-core) expects it: no authentication,
//! CONNECT and UDP ASSOCIATE with IP addresses. Encoding lives in `cg_protocol::socks`.

use cg_protocol::socks::{
    GREETING_NO_AUTH, UDP_ASSOCIATE_REQUEST, address_rest_len, connect_request, parse_address,
};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// hev's default `connect-timeout` (our hev config leaves it unset): greeting, request and
/// reply must be done within it.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// SOCKS reply 0x05: connection refused (also what we report for any failure code).
fn refused(code: u8) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!("socks reply {code}"),
    )
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// Greeting, request and reply; the bound address when the reply carries an IP address.
async fn handshake(
    socks: SocketAddr,
    request: &[u8],
) -> io::Result<(TcpStream, Option<SocketAddr>)> {
    let mut stream = TcpStream::connect(socks).await?;
    stream.set_nodelay(true)?;
    stream.write_all(&GREETING_NO_AUTH).await?;
    let mut method = [0u8; 2];
    stream.read_exact(&mut method).await?;
    if method != [5, 0] {
        return Err(malformed("socks method refused"));
    }
    stream.write_all(request).await?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 5 {
        return Err(malformed("socks reply version"));
    }
    let first = stream.read_u8().await?;
    let rest = address_rest_len(header[3], first).map_err(|_| malformed("socks reply address"))?;
    let mut address = vec![first; 1 + rest];
    stream.read_exact(&mut address[1..]).await?;
    if header[1] != 0 {
        return Err(refused(header[1]));
    }
    Ok((stream, parse_address(header[3], &address).ok()))
}

/// A stream to `target` through the SOCKS server, ready to carry data.
pub(crate) async fn connect(socks: SocketAddr, target: SocketAddr) -> io::Result<TcpStream> {
    let request = connect_request(target);
    let (stream, _) = tokio::time::timeout(CONNECT_TIMEOUT, handshake(socks, &request))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    Ok(stream)
}

/// The control connection of a new UDP association and its relay address (the SOCKS
/// server's own address when the reply leaves it unspecified).
pub(crate) async fn associate(socks: SocketAddr) -> io::Result<(TcpStream, SocketAddr)> {
    let (stream, relay) =
        tokio::time::timeout(CONNECT_TIMEOUT, handshake(socks, &UDP_ASSOCIATE_REQUEST))
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    let mut relay = relay.ok_or_else(|| malformed("socks relay address"))?;
    if relay.ip().is_unspecified() {
        relay.set_ip(socks.ip());
    }
    Ok((stream, relay))
}
