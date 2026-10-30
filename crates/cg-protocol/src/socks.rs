//! SOCKS5 pieces the forwarder needs, the client side cg-netstack speaks to it, and the
//! phone's udp-over-usb-v1 framing: after the private command 0xF0 each frame is a
//! big-endian u16 length followed by one SOCKS5 UDP packet (RSV, FRAG, address, payload).

use crate::ProtocolError;
use std::net::{IpAddr, SocketAddr};

pub const UDP_OVER_USB_REQUEST: [u8; 10] = [5, 0xf0, 0, 1, 0, 0, 0, 0, 0, 0];
pub const REPLY_COMMAND_FAILED: [u8; 10] = [5, 7, 0, 1, 0, 0, 0, 0, 0, 0];
pub const MAX_FRAME: usize = 65535;

pub fn udp_associate_reply(port: u16) -> [u8; 10] {
    let [hi, lo] = port.to_be_bytes();
    [5, 0, 0, 1, 127, 0, 0, 1, hi, lo]
}

pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let len = u16::try_from(payload.len()).map_err(|_| ProtocolError::TooLarge)?;
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Bytes still to read (port included) once ATYP and the byte after it are known.
pub fn address_rest_len(atyp: u8, first: u8) -> Result<usize, ProtocolError> {
    match atyp {
        1 => Ok(3 + 2),
        3 => Ok(first as usize + 2),
        4 => Ok(15 + 2),
        _ => Err(ProtocolError::Malformed("socks address type")),
    }
}

pub fn connect_domain_request(host: &str, port: u16) -> Result<Vec<u8>, ProtocolError> {
    let len = u8::try_from(host.len()).map_err(|_| ProtocolError::TooLarge)?;
    let mut out = vec![5, 1, 0, 3, len];
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}

// Client side, as hev (and cg-netstack in its place) talks to the forwarder: no
// authentication, IP addresses only (the tunnel never sees names).

/// Version 5, one method offered: no authentication.
pub const GREETING_NO_AUTH: [u8; 3] = [5, 1, 0];
/// UDP ASSOCIATE with an unspecified client address: the relay serves the first sender.
pub const UDP_ASSOCIATE_REQUEST: [u8; 10] = [5, 3, 0, 1, 0, 0, 0, 0, 0, 0];

/// ATYP, address and port of `addr`.
fn push_address(out: &mut Vec<u8>, addr: SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(1);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

pub fn connect_request(target: SocketAddr) -> Vec<u8> {
    let mut out = vec![5, 1, 0];
    push_address(&mut out, target);
    out
}

/// The address after ATYP (port included), as in replies and UDP headers; names are
/// refused because nothing on our side could use one.
pub fn parse_address(atyp: u8, bytes: &[u8]) -> Result<SocketAddr, ProtocolError> {
    let ip = match (atyp, bytes.len()) {
        (1, 6) => IpAddr::from(<[u8; 4]>::try_from(&bytes[..4]).unwrap()),
        (4, 18) => IpAddr::from(<[u8; 16]>::try_from(&bytes[..16]).unwrap()),
        (1 | 4, _) => return Err(ProtocolError::Malformed("socks address length")),
        (3, _) => return Err(ProtocolError::Malformed("socks domain address")),
        _ => return Err(ProtocolError::Malformed("socks address type")),
    };
    let n = bytes.len();
    Ok(SocketAddr::new(
        ip,
        u16::from_be_bytes([bytes[n - 2], bytes[n - 1]]),
    ))
}

/// One SOCKS5 UDP request: RSV, FRAG 0, the target, then the payload.
pub fn udp_datagram(target: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + payload.len());
    out.extend_from_slice(&[0, 0, 0]);
    push_address(&mut out, target);
    out.extend_from_slice(payload);
    out
}

/// The peer and payload of one SOCKS5 UDP datagram; fragments are refused.
pub fn parse_udp_datagram(datagram: &[u8]) -> Result<(SocketAddr, &[u8]), ProtocolError> {
    let short = ProtocolError::Malformed("socks udp header");
    if datagram.len() < 4 {
        return Err(short);
    }
    if datagram[2] != 0 {
        return Err(ProtocolError::Malformed("socks udp fragment"));
    }
    let len = match datagram[3] {
        1 => 6,
        4 => 18,
        3 => return Err(ProtocolError::Malformed("socks domain address")),
        _ => return Err(ProtocolError::Malformed("socks address type")),
    };
    let address = datagram.get(4..4 + len).ok_or(short)?;
    Ok((parse_address(datagram[3], address)?, &datagram[4 + len..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn client_requests_carry_ip_addresses() {
        assert_eq!(GREETING_NO_AUTH, [5, 1, 0]);
        assert_eq!(UDP_ASSOCIATE_REQUEST, [5, 3, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            connect_request("93.184.216.34:443".parse().unwrap()),
            vec![5, 1, 0, 1, 93, 184, 216, 34, 1, 187]
        );
        let v6 = connect_request("[2606:4700::1111]:443".parse().unwrap());
        assert_eq!(&v6[..4], &[5, 1, 0, 4]);
        assert_eq!(&v6[4..6], &[0x26, 0x06]);
        assert_eq!(&v6[18..], &[0x11, 0x11, 1, 187]);
        assert_eq!(v6.len(), 4 + 16 + 2);
    }

    #[test]
    fn addresses_parse_back() {
        let v4: SocketAddr = "1.2.3.4:53".parse().unwrap();
        assert_eq!(parse_address(1, &[1, 2, 3, 4, 0, 53]).unwrap(), v4);
        let v6: SocketAddr = "[fd00::2]:80".parse().unwrap();
        let mut bytes = "fd00::2".parse::<Ipv6Addr>().unwrap().octets().to_vec();
        bytes.extend_from_slice(&[0, 80]);
        assert_eq!(parse_address(4, &bytes).unwrap(), v6);
        assert!(parse_address(1, &[1, 2, 3]).is_err());
        assert!(parse_address(3, &[1, b'a', 0, 80]).is_err());
        assert!(parse_address(9, &[0; 6]).is_err());
    }

    #[test]
    fn udp_datagrams_round_trip() {
        let target: SocketAddr = "1.1.1.1:53".parse().unwrap();
        let datagram = udp_datagram(target, b"q");
        assert_eq!(datagram, vec![0, 0, 0, 1, 1, 1, 1, 1, 0, 53, b'q']);
        assert_eq!(parse_udp_datagram(&datagram).unwrap(), (target, &b"q"[..]));
        // Empty payloads are datagrams too.
        let empty = udp_datagram(target, b"");
        assert_eq!(parse_udp_datagram(&empty).unwrap(), (target, &b""[..]));
        let v6: SocketAddr = "[2606:4700::1111]:443".parse().unwrap();
        let datagram = udp_datagram(v6, b"xy");
        assert_eq!(datagram.len(), 4 + 16 + 2 + 2);
        assert_eq!(parse_udp_datagram(&datagram).unwrap(), (v6, &b"xy"[..]));
        // Fragments, short headers and domain names are refused.
        assert!(parse_udp_datagram(&[0, 0, 1, 1, 1, 1, 1, 1, 0, 53]).is_err());
        assert!(parse_udp_datagram(&[0, 0, 0, 1, 1, 1]).is_err());
        assert!(parse_udp_datagram(&[0, 0, 0, 3, 1, b'a', 0, 53]).is_err());
    }

    #[test]
    fn frames_carry_big_endian_length() {
        assert_eq!(encode_frame(&[]).unwrap(), vec![0, 0]);
        let frame = encode_frame(&[7; 300]).unwrap();
        assert_eq!(&frame[..2], &[1, 44]);
        assert_eq!(frame.len(), 302);
        assert!(encode_frame(&vec![0; MAX_FRAME]).is_ok());
        assert!(encode_frame(&vec![0; MAX_FRAME + 1]).is_err());
    }

    #[test]
    fn address_lengths_after_first_byte() {
        // IPv4: first byte is part of the 4-byte address, then 3 + 2 more.
        assert_eq!(address_rest_len(1, 10).unwrap(), 5);
        // Domain: first byte is the length.
        assert_eq!(address_rest_len(3, 11).unwrap(), 13);
        assert_eq!(address_rest_len(4, 0).unwrap(), 17);
        assert!(address_rest_len(9, 0).is_err());
    }

    #[test]
    fn replies_and_requests() {
        assert_eq!(
            udp_associate_reply(0x1234),
            [5, 0, 0, 1, 127, 0, 0, 1, 0x12, 0x34]
        );
        assert_eq!(UDP_OVER_USB_REQUEST[1], 0xf0);
        assert_eq!(
            connect_domain_request("a.io", 443).unwrap(),
            vec![5, 1, 0, 3, 4, b'a', b'.', b'i', b'o', 1, 187]
        );
        assert!(connect_domain_request(&"x".repeat(256), 1).is_err());
    }
}
