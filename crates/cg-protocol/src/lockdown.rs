//! lockdownd's framing on port 62078 and on the services it starts: a 4-byte
//! big-endian length followed by an XML property list.

use crate::ProtocolError;
use plist::{Dictionary, Value};
use std::io::Cursor;

pub const PORT: u16 = 62078;
/// The largest body accepted, as the Mac reads it.
pub const MAX_FRAME: usize = 4_000_000;

pub fn encode(message: &Dictionary) -> Result<Vec<u8>, ProtocolError> {
    let mut frame = vec![0u8; 4];
    plist::to_writer_xml(&mut frame, &Value::Dictionary(message.clone()))?;
    let len = frame.len() - 4;
    if len > MAX_FRAME {
        return Err(ProtocolError::TooLarge);
    }
    // MAX_FRAME fits in a u32.
    frame[..4].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(frame)
}

pub fn body_len(header: [u8; 4]) -> Result<usize, ProtocolError> {
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(ProtocolError::Malformed("lockdown frame length"));
    }
    Ok(len)
}

pub fn decode(body: &[u8]) -> Result<Dictionary, ProtocolError> {
    Value::from_reader(Cursor::new(body))?
        .into_dictionary()
        .ok_or(ProtocolError::Malformed(
            "lockdown plist is not a dictionary",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};

    #[test]
    fn frame_round_trips() {
        let mut message = Dictionary::new();
        message.insert("Label".into(), "CellGlide".into());
        message.insert("Request".into(), "GetValue".into());
        message.insert("Key".into(), "ProductType".into());
        message.insert("Port".into(), Value::Integer(51234u64.into()));
        let frame = encode(&message).unwrap();
        let header: [u8; 4] = frame[..4].try_into().unwrap();
        assert_eq!(body_len(header).unwrap(), frame.len() - 4);
        assert_eq!(u32::from_be_bytes(header) as usize, frame.len() - 4);
        assert!(frame[4..].starts_with(b"<?xml"));
        assert_eq!(decode(&frame[4..]).unwrap(), message);
    }

    #[test]
    fn zero_and_huge_frames_are_rejected() {
        assert!(matches!(
            body_len([0, 0, 0, 0]),
            Err(ProtocolError::Malformed(_))
        ));
        let huge = u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes();
        assert!(matches!(body_len(huge), Err(ProtocolError::Malformed(_))));
        let largest = u32::try_from(MAX_FRAME).unwrap().to_be_bytes();
        assert_eq!(body_len(largest).unwrap(), MAX_FRAME);
        assert!(decode(b"<plist><array/></plist>").is_err());
    }
}
