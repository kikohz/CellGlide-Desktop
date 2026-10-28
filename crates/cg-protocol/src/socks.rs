//! SOCKS5 pieces the forwarder needs, and the phone's udp-over-usb-v1 framing:
//! after the private command 0xF0 each frame is a big-endian u16 length followed by
//! one SOCKS5 UDP packet (RSV, FRAG, address, payload).

use crate::ProtocolError;

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

#[cfg(test)]
mod tests {
    use super::*;

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
