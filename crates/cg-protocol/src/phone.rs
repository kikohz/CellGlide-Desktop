//! Messages on the iPhone app's loopback ports, reached through usbmuxd.

use crate::ProtocolError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;

pub mod port {
    pub const SOCKS: u16 = 9876;
    pub const CONTROL: u16 = 8088;
    pub const STATUS: u16 = 8089;
    pub const METRICS: u16 = 8090;
    /// The phone app listens here for the computer's battery reports.
    pub const BATTERY: u16 = 8091;
}

pub mod capability {
    pub const SOCKS5_TCP: &str = "socks5-tcp";
    pub const UDP_OVER_USB: &str = "udp-over-usb-v1";
    pub const DNS_FORWARD: &str = "dns-forward-v1";
    pub const SESSION_WATCH: &str = "session-watch-v1";
}

pub const HELLO_REQUEST: &[u8] =
    b"GET /cellproxy/hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
pub const MAX_HTTP_HEAD: usize = 8192;
pub const MAX_HELLO_BODY: usize = 8192;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub service: String,
    pub protocol_version: i64,
    pub version: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub capabilities: Vec<String>,
    pub dns: Option<HelloDns>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HelloDns {
    pub enabled: Option<bool>,
    pub endpoint: String,
    pub cache_enabled: bool,
}

impl Hello {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let ok = self.service == "CellProxy"
            && self.protocol_version == 1
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.has(capability::SOCKS5_TCP);
        if ok {
            Ok(())
        } else {
            Err(ProtocolError::Incompatible)
        }
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// The phone's "CellGlide DNS" switch; phones that send no dns block have it on.
    pub fn dns_enabled(&self) -> bool {
        self.dns.as_ref().and_then(|d| d.enabled).unwrap_or(true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpHead {
    pub status: u16,
    pub content_length: Option<usize>,
}

/// Parses a response head that ends with CRLF CRLF.
pub fn parse_http_head(head: &[u8]) -> Result<HttpHead, ProtocolError> {
    let text = std::str::from_utf8(head).map_err(|_| ProtocolError::Malformed("http head"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or(ProtocolError::Malformed("http status"))?;
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok());
    Ok(HttpHead {
        status,
        content_length,
    })
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The session-watch request. The phone shows the computer's name and link from these
/// headers; both are bounded and percent-encoded so they cannot break the framing.
pub fn watch_request(
    session_id: &str,
    computer_name: Option<&str>,
    link_speed: Option<u64>,
) -> String {
    let mut text =
        format!("GET /cellproxy/watch?session={session_id} HTTP/1.1\r\nHost: localhost\r\n");
    if let Some(name) = computer_name.filter(|n| !n.is_empty()) {
        let short: String = name.chars().take(64).collect();
        let _ = write!(text, "X-CellGlide-Mac: {}\r\n", percent_encode(&short));
    }
    if let Some(speed) = link_speed {
        let _ = write!(text, "X-CellGlide-Link: usb; speed={speed}\r\n");
    }
    text.push_str("\r\n");
    text
}

pub const MAX_STATUS_LINE: usize = 4096;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharingStatus {
    pub service: String,
    pub version: i64,
    pub instance: String,
    pub revision: u64,
    pub running: bool,
    pub starting: bool,
    pub background: bool,
    pub dns_enabled: bool,
}

impl SharingStatus {
    pub fn is_valid(&self) -> bool {
        self.service == "CellGlide-status"
            && self.version == 1
            && uuid::Uuid::parse_str(&self.instance).is_ok()
    }
}

/// Status frames must come from one phone instance with growing revisions.
#[derive(Debug, Default)]
pub struct StatusSequence {
    last: Option<(String, u64)>,
}

impl StatusSequence {
    pub fn accept(&mut self, status: &SharingStatus) -> Result<(), ProtocolError> {
        let in_order = match &self.last {
            None => true,
            Some((instance, revision)) => {
                *instance == status.instance && status.revision > *revision
            }
        };
        if !status.is_valid() || !in_order {
            return Err(ProtocolError::Malformed("status sequence"));
        }
        self.last = Some((status.instance.clone(), status.revision));
        Ok(())
    }
}

pub const MAX_METRICS_LINE: usize = 4096;

/// One frame per second from the phone; every reading is optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PhoneMetrics {
    pub service: String,
    pub version: i64,
    pub carrier: Option<String>,
    pub radio: Option<String>,
    pub sharing: Option<bool>,
    /// Bytes per second the computer moves through the phone.
    pub download: Option<f64>,
    pub upload: Option<f64>,
    pub session_bytes: Option<i64>,
    pub quality_score: Option<i64>,
    pub latency: Option<f64>,
    pub loss: Option<f64>,
    /// 0–1.
    pub battery: Option<f64>,
    pub charging: Option<bool>,
    pub low_power: Option<bool>,
    pub thermal: Option<i64>,
    #[serde(rename = "exitOK")]
    pub exit_ok: Option<bool>,
    pub exit_latency: Option<f64>,
    #[serde(rename = "connectsOK")]
    pub connects_ok: Option<i64>,
    pub connects_failed: Option<HashMap<String, i64>>,
    pub history: Option<Vec<Vec<i64>>>,
}

impl PhoneMetrics {
    pub fn is_valid(&self) -> bool {
        self.service == "CellGlide-metrics" && self.version == 1
    }
}

/// One report line, including the trailing newline, stays under this.
pub const MAX_BATTERY_FRAME: usize = 1024;

/// The phone's battery as its gauge reports it, sent by the computer to the phone app on
/// port 8091 (one JSON line each time), so the app can show measured charging power it
/// cannot read itself. Same fields as the Mac's `PhoneBatteryReport`; absent readings are
/// left out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BatteryReport {
    pub service: String,
    pub version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    /// A power source is attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_connected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fully_charged: Option<bool>,
    /// Power going into the battery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watts: Option<f64>,
    /// Power the charger delivers into the phone (battery plus what the phone uses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_watts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes_to_full: Option<i64>,
    /// What the power source offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_watts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wireless: Option<bool>,
    /// Full-charge against design capacity, 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<i64>,
    /// mAh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_capacity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_charge_capacity: Option<i64>,
    /// Each SIM's carrier bundle and network code as lockdown lists them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sims: Option<Vec<BatteryReportSim>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatteryReportSim {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnc: Option<String>,
}

impl Default for BatteryReport {
    fn default() -> Self {
        Self {
            service: "CellGlide-battery".into(),
            version: 1,
            level: None,
            charging: None,
            external_connected: None,
            fully_charged: None,
            watts: None,
            input_watts: None,
            minutes_to_full: None,
            adapter_watts: None,
            wireless: None,
            health: None,
            cycles: None,
            design_capacity: None,
            full_charge_capacity: None,
            sims: None,
        }
    }
}

impl BatteryReport {
    pub fn is_valid(&self) -> bool {
        self.service == "CellGlide-battery" && self.version == 1
    }

    /// The JSON line sent to the phone, newline included; `TooLarge` past `MAX_BATTERY_FRAME`.
    pub fn line(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        if line.len() > MAX_BATTERY_FRAME {
            return Err(ProtocolError::TooLarge);
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";

    fn hello_json(caps: &str) -> String {
        format!(
            r#"{{"service":"CellProxy","protocolVersion":1,"version":"2.2.0","sessionID":"{SESSION}","capabilities":[{caps}],"dns":{{"enabled":false,"endpoint":"https://1.1.1.1/dns-query","cacheEnabled":true}}}}"#
        )
    }

    #[test]
    fn hello_parses_and_validates() {
        let hello: Hello =
            serde_json::from_str(&hello_json(r#""socks5-tcp","udp-over-usb-v1""#)).unwrap();
        hello.validate().unwrap();
        assert_eq!(hello.version, "2.2.0");
        assert!(hello.has(capability::UDP_OVER_USB));
        assert!(!hello.has(capability::SESSION_WATCH));
        assert!(!hello.dns_enabled());
    }

    #[test]
    fn hello_rejects_incompatible_phone() {
        let no_tcp: Hello = serde_json::from_str(&hello_json(r#""socks5-udp""#)).unwrap();
        assert!(matches!(
            no_tcp.validate(),
            Err(ProtocolError::Incompatible)
        ));
        let bad_session: Hello = serde_json::from_str(
            r#"{"service":"CellProxy","protocolVersion":1,"version":"1","sessionID":"nope","capabilities":["socks5-tcp"]}"#,
        )
        .unwrap();
        assert!(bad_session.validate().is_err());
        assert!(bad_session.dns_enabled(), "missing dns block means enabled");
        let v2: Hello = serde_json::from_str(
            &hello_json(r#""socks5-tcp""#)
                .replace(r#""protocolVersion":1"#, r#""protocolVersion":2"#),
        )
        .unwrap();
        assert!(v2.validate().is_err());
    }

    #[test]
    fn http_head() {
        let head = parse_http_head(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 42\r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, Some(42));
        assert_eq!(
            parse_http_head(b"HTTP/1.1 404 Not Found\r\n\r\n")
                .unwrap()
                .status,
            404
        );
        assert!(parse_http_head(b"SSH-2.0\r\n\r\n").is_err());
    }

    #[test]
    fn watch_request_encodes_name_and_speed() {
        let text = watch_request(SESSION, Some("Alice’s 电脑 ThinkPad"), Some(480_000_000));
        assert!(text.starts_with(&format!(
            "GET /cellproxy/watch?session={SESSION} HTTP/1.1\r\nHost: localhost\r\n"
        )));
        assert!(
            text.contains("X-CellGlide-Mac: Alice%E2%80%99s%20%E7%94%B5%E8%84%91%20ThinkPad\r\n")
        );
        assert!(text.contains("X-CellGlide-Link: usb; speed=480000000\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        let plain = watch_request(SESSION, None, None);
        assert_eq!(
            plain,
            format!("GET /cellproxy/watch?session={SESSION} HTTP/1.1\r\nHost: localhost\r\n\r\n")
        );
        let long = "x".repeat(100);
        assert!(
            watch_request(SESSION, Some(&long), None)
                .contains(&format!("X-CellGlide-Mac: {}\r\n", "x".repeat(64)))
        );
    }

    fn status(instance: &str, revision: u64) -> SharingStatus {
        serde_json::from_str(&format!(
            r#"{{"service":"CellGlide-status","version":1,"instance":"{instance}","revision":{revision},"running":true,"starting":false,"background":true,"dnsEnabled":true}}"#
        ))
        .unwrap()
    }

    #[test]
    fn status_sequence_requires_same_instance_and_growing_revision() {
        let mut seq = StatusSequence::default();
        seq.accept(&status(SESSION, 3)).unwrap();
        seq.accept(&status(SESSION, 4)).unwrap();
        assert!(seq.accept(&status(SESSION, 4)).is_err());
        let other = "7F9619FF-8B86-D011-B42D-00C04FC964FF";
        assert!(
            StatusSequence::default()
                .accept(&status("not-a-uuid", 1))
                .is_err()
        );
        let mut seq = StatusSequence::default();
        seq.accept(&status(SESSION, 1)).unwrap();
        assert!(seq.accept(&status(other, 9)).is_err());
    }

    #[test]
    fn metrics_accept_partial_frames() {
        let m: PhoneMetrics = serde_json::from_str(
            r#"{"service":"CellGlide-metrics","version":1,"carrier":"中国移动","radio":"5G SA","download":1024.5,"battery":0.8,"exitOK":true,"connectsFailed":{"timeout":2},"history":[[1,2],[3,4]]}"#,
        )
        .unwrap();
        assert!(m.is_valid());
        assert_eq!(m.carrier.as_deref(), Some("中国移动"));
        assert_eq!(m.exit_ok, Some(true));
        assert_eq!(m.upload, None);
        assert_eq!(m.connects_failed.unwrap()["timeout"], 2);
        let unknown: PhoneMetrics =
            serde_json::from_str(r#"{"service":"other","version":1}"#).unwrap();
        assert!(!unknown.is_valid());
    }

    #[test]
    fn battery_report_round_trips_and_validates() {
        let report = BatteryReport {
            level: Some(80),
            adapter_watts: Some(15),
            ..BatteryReport::default()
        };
        let text = serde_json::to_string(&report).unwrap();
        assert_eq!(
            text,
            r#"{"service":"CellGlide-battery","version":1,"level":80,"adapterWatts":15}"#
        );
        let decoded: BatteryReport = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded, report);
        assert!(decoded.is_valid());
        let other: BatteryReport = serde_json::from_str(r#"{"service":"x","version":1}"#).unwrap();
        assert!(!other.is_valid());
    }

    #[test]
    fn battery_report_carries_each_sims_carrier_and_stays_small() {
        let sims = vec![
            BatteryReportSim {
                bundle: Some("com.apple.Verizon_TFW_LTE_US".into()),
                mcc: Some("311".into()),
                mnc: Some("480".into()),
            },
            BatteryReportSim {
                bundle: Some("com.apple.TMobile_MVNO_US".into()),
                mcc: Some("310".into()),
                mnc: Some("240".into()),
            },
        ];
        let report = BatteryReport {
            level: Some(76),
            charging: Some(true),
            external_connected: Some(true),
            fully_charged: Some(false),
            watts: Some(2.5),
            input_watts: Some(5.0),
            minutes_to_full: Some(75),
            adapter_watts: Some(15),
            wireless: Some(false),
            health: Some(0.95),
            cycles: Some(396),
            design_capacity: Some(3116),
            full_charge_capacity: Some(2957),
            sims: Some(sims.clone()),
            ..BatteryReport::default()
        };
        let line = report.line().unwrap();
        assert!(line.len() < MAX_BATTERY_FRAME, "{} bytes", line.len());
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
        let decoded: BatteryReport = serde_json::from_slice(&line).unwrap();
        assert_eq!(decoded.sims, Some(sims));
        assert_eq!(decoded, report);
        let text = String::from_utf8(line).unwrap();
        for key in [
            "externalConnected",
            "fullyCharged",
            "inputWatts",
            "minutesToFull",
            "adapterWatts",
            "designCapacity",
            "fullChargeCapacity",
        ] {
            assert!(text.contains(&format!("\"{key}\":")), "{key} in {text}");
        }
        // An older Mac sends no SIMs; the phone keeps its own lookup.
        let old: BatteryReport =
            serde_json::from_str(r#"{"service":"CellGlide-battery","version":1,"level":50}"#)
                .unwrap();
        assert_eq!(old.sims, None);
    }
}
