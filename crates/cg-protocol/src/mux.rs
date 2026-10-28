//! usbmuxd's plist protocol: a 16-byte little-endian header (length, version 1,
//! type 8 = plist, tag) followed by an XML property list.

use crate::ProtocolError;
use plist::{Dictionary, Value};
use std::io::Cursor;

pub const HEADER_LEN: usize = 16;
pub const MAX_MESSAGE: u32 = 1_048_576;
const VERSION: u32 = 1;
const PLIST_TYPE: u32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The iPhone's UDID as usbmuxd reports it.
    pub serial: String,
    pub mux_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxEvent {
    Attached(Device),
    Detached(u32),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxRequest {
    ListDevices,
    Listen,
    Connect { mux_id: u32, port: u16 },
    ReadPairRecord { serial: String },
}

impl MuxRequest {
    fn fields(&self) -> Dictionary {
        let mut d = Dictionary::new();
        let kind = match self {
            Self::ListDevices => "ListDevices",
            Self::Listen => "Listen",
            Self::Connect { mux_id, port } => {
                d.insert("DeviceID".into(), Value::Integer(u64::from(*mux_id).into()));
                // usbmuxd reads the port in network order from a little-endian integer.
                let wire = u16::from_le_bytes(port.to_be_bytes());
                d.insert("PortNumber".into(), Value::Integer(u64::from(wire).into()));
                "Connect"
            }
            Self::ReadPairRecord { serial } => {
                d.insert("PairRecordID".into(), serial.as_str().into());
                "ReadPairRecord"
            }
        };
        d.insert("MessageType".into(), kind.into());
        d.insert("ClientVersionString".into(), "CellGlide-Desktop-1".into());
        d.insert("ProgName".into(), "CellGlideDesktop".into());
        d.insert("kLibUSBMuxVersion".into(), Value::Integer(3u64.into()));
        d
    }

    pub fn encode(&self, tag: u32) -> Result<Vec<u8>, ProtocolError> {
        encode_message(&self.fields(), tag)
    }
}

pub fn encode_message(dict: &Dictionary, tag: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(dict.clone()))?;
    let len = u32::try_from(body.len() + HEADER_LEN).map_err(|_| ProtocolError::TooLarge)?;
    if len > MAX_MESSAGE {
        return Err(ProtocolError::TooLarge);
    }
    let mut out = Vec::with_capacity(len as usize);
    for value in [len, VERSION, PLIST_TYPE, tag] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn body_len(header: &[u8; HEADER_LEN]) -> Result<usize, ProtocolError> {
    let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let len = field(0);
    if len < HEADER_LEN as u32 || len > MAX_MESSAGE || field(1) != VERSION || field(2) != PLIST_TYPE
    {
        return Err(ProtocolError::Malformed("usbmux header"));
    }
    Ok(len as usize - HEADER_LEN)
}

pub fn decode_body(body: &[u8]) -> Result<Dictionary, ProtocolError> {
    Value::from_reader(Cursor::new(body))?
        .into_dictionary()
        .ok_or(ProtocolError::Malformed("usbmux plist is not a dictionary"))
}

fn uint(dict: &Dictionary, key: &str) -> Option<u64> {
    dict.get(key).and_then(Value::as_unsigned_integer)
}

pub fn result_number(dict: &Dictionary) -> Option<u64> {
    uint(dict, "Number")
}

/// A USB-connected device from a `DeviceList` item or an `Attached` event.
fn usb_device(item: &Dictionary) -> Option<Device> {
    let props = item.get("Properties")?.as_dictionary()?;
    if props.get("ConnectionType")?.as_string()? != "USB" {
        return None;
    }
    Some(Device {
        serial: props.get("SerialNumber")?.as_string()?.to_owned(),
        mux_id: u32::try_from(uint(item, "DeviceID")?).ok()?,
    })
}

pub fn device_list(dict: &Dictionary) -> Result<Vec<Device>, ProtocolError> {
    let list = dict
        .get("DeviceList")
        .and_then(Value::as_array)
        .ok_or(ProtocolError::Malformed("DeviceList missing"))?;
    let mut devices: Vec<Device> = list
        .iter()
        .filter_map(Value::as_dictionary)
        .filter_map(usb_device)
        .collect();
    devices.sort_by(|a, b| a.serial.cmp(&b.serial));
    Ok(devices)
}

pub fn parse_event(dict: &Dictionary) -> MuxEvent {
    match dict.get("MessageType").and_then(Value::as_string) {
        Some("Attached") => usb_device(dict).map_or(MuxEvent::Other, MuxEvent::Attached),
        Some("Detached") => uint(dict, "DeviceID")
            .and_then(|id| u32::try_from(id).ok())
            .map_or(MuxEvent::Other, MuxEvent::Detached),
        _ => MuxEvent::Other,
    }
}

pub fn has_pair_record(dict: &Dictionary) -> bool {
    dict.contains_key("PairRecordData")
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};

    fn int(v: u64) -> Value {
        Value::Integer(v.into())
    }

    fn usb_device(serial: &str, id: u64, kind: &str) -> Value {
        let mut props = Dictionary::new();
        props.insert("ConnectionType".into(), kind.into());
        props.insert("SerialNumber".into(), serial.into());
        let mut item = Dictionary::new();
        item.insert("DeviceID".into(), int(id));
        item.insert("Properties".into(), Value::Dictionary(props));
        Value::Dictionary(item)
    }

    #[test]
    fn connect_header_and_port_byte_order() {
        let bytes = MuxRequest::Connect {
            mux_id: 7,
            port: 9876,
        }
        .encode(3)
        .unwrap();
        let header: [u8; 16] = bytes[..16].try_into().unwrap();
        assert_eq!(
            u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(header[8..12].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(header[12..16].try_into().unwrap()), 3);
        let dict = decode_body(&bytes[16..]).unwrap();
        assert_eq!(
            dict.get("MessageType").and_then(Value::as_string),
            Some("Connect")
        );
        assert_eq!(
            dict.get("DeviceID").and_then(Value::as_unsigned_integer),
            Some(7)
        );
        // 9876 = 0x2694, sent as network order inside a little-endian integer: 0x9426.
        assert_eq!(
            dict.get("PortNumber").and_then(Value::as_unsigned_integer),
            Some(0x9426)
        );
        assert_eq!(
            dict.get("kLibUSBMuxVersion")
                .and_then(Value::as_unsigned_integer),
            Some(3)
        );
    }

    #[test]
    fn body_len_rejects_bad_headers() {
        let good = MuxRequest::ListDevices.encode(1).unwrap();
        let header: [u8; 16] = good[..16].try_into().unwrap();
        assert_eq!(body_len(&header).unwrap(), good.len() - 16);

        let mut wrong_type = header;
        wrong_type[8] = 1;
        assert!(body_len(&wrong_type).is_err());

        let mut too_big = header;
        too_big[0..4].copy_from_slice(&(MAX_MESSAGE + 1).to_le_bytes());
        assert!(body_len(&too_big).is_err());

        let mut too_small = header;
        too_small[0..4].copy_from_slice(&15u32.to_le_bytes());
        assert!(body_len(&too_small).is_err());
    }

    #[test]
    fn device_list_keeps_usb_only_sorted_by_serial() {
        let mut dict = Dictionary::new();
        dict.insert(
            "DeviceList".into(),
            Value::Array(vec![
                usb_device("bbbb", 2, "USB"),
                usb_device("cccc", 3, "Network"),
                usb_device("aaaa", 1, "USB"),
            ]),
        );
        let devices = device_list(&dict).unwrap();
        assert_eq!(
            devices,
            vec![
                Device {
                    serial: "aaaa".into(),
                    mux_id: 1
                },
                Device {
                    serial: "bbbb".into(),
                    mux_id: 2
                },
            ]
        );
        assert!(device_list(&Dictionary::new()).is_err());
    }

    #[test]
    fn events_and_results() {
        let Value::Dictionary(mut attached) = usb_device("aaaa", 5, "USB") else {
            unreachable!()
        };
        attached.insert("MessageType".into(), "Attached".into());
        assert_eq!(
            parse_event(&attached),
            MuxEvent::Attached(Device {
                serial: "aaaa".into(),
                mux_id: 5
            })
        );

        let Value::Dictionary(mut wifi) = usb_device("aaaa", 6, "Network") else {
            unreachable!()
        };
        wifi.insert("MessageType".into(), "Attached".into());
        assert_eq!(parse_event(&wifi), MuxEvent::Other);

        let mut detached = Dictionary::new();
        detached.insert("MessageType".into(), "Detached".into());
        detached.insert("DeviceID".into(), int(5));
        assert_eq!(parse_event(&detached), MuxEvent::Detached(5));

        let mut result = Dictionary::new();
        result.insert("MessageType".into(), "Result".into());
        result.insert("Number".into(), int(0));
        assert_eq!(result_number(&result), Some(0));
        assert!(!has_pair_record(&result));
        result.insert("PairRecordData".into(), Value::Data(vec![1, 2]));
        assert!(has_pair_record(&result));
    }

    #[test]
    fn encode_message_round_trips() {
        let mut dict = Dictionary::new();
        dict.insert("MessageType".into(), "Result".into());
        dict.insert("Number".into(), int(3));
        let bytes = encode_message(&dict, 9).unwrap();
        let header: [u8; 16] = bytes[..16].try_into().unwrap();
        assert_eq!(body_len(&header).unwrap(), bytes.len() - 16);
        assert_eq!(decode_body(&bytes[16..]).unwrap(), dict);
    }
}
