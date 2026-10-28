//! The phone card: what the phone app reports each second, plus what lockdown reveals over USB.
//! Every reading is optional; one that can't be read is `None` and is left out of the wire.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Seconds of speed history on the card.
pub const TRAIL_LENGTH: usize = 60;

/// Everything known about the connected phone, as the service publishes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct PhoneCard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<PhoneFrame>,
    /// The last frame is old: the phone app stopped reporting.
    pub stale: bool,
    /// One-minute speed trail; a second without a speed reading is a gap (`None`), never 0.
    pub trail: Vec<Option<SpeedPoint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb: Option<UsbPhone>,
}

/// One metrics frame from the phone app.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct PhoneFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharing: Option<bool>,
    /// Bytes per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<i64>,
    /// Milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<f64>,
    /// 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss: Option<f64>,
    /// 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_power: Option<bool>,
    /// 0–3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thermal: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_latency: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connects_ok: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connects_failed: Option<BTreeMap<String, i64>>,
}

impl PhoneFrame {
    pub fn grade(&self) -> Option<Grade> {
        self.quality_score.map(Grade::from_score)
    }
}

/// Bytes per second.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpeedPoint {
    pub download: f64,
    pub upload: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct UsbPhone {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    pub sims: Vec<SimCarrier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseband: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sim_tray: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryReading>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct SimCarrier {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnc: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct BatteryReading {
    /// Percent, 0–100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    pub charging: bool,
    pub external_connected: bool,
    pub fully_charged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_watts: Option<u32>,
    pub wireless: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_capacity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_charge_capacity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_watts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes_to_full: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// A phone's name and model as the user knows it, by serial.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct PhoneLabel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Excellent,
    Good,
    Fair,
    Poor,
}

impl Grade {
    pub fn from_score(score: i64) -> Grade {
        match score {
            85.. => Grade::Excellent,
            65.. => Grade::Good,
            40.. => Grade::Fair,
            _ => Grade::Poor,
        }
    }
}

const MODEL_NAMES: &[(&str, &str)] = &[
    ("iPhone12,1", "iPhone 11"),
    ("iPhone12,3", "iPhone 11 Pro"),
    ("iPhone12,5", "iPhone 11 Pro Max"),
    ("iPhone12,8", "iPhone SE"),
    ("iPhone13,1", "iPhone 12 mini"),
    ("iPhone13,2", "iPhone 12"),
    ("iPhone13,3", "iPhone 12 Pro"),
    ("iPhone13,4", "iPhone 12 Pro Max"),
    ("iPhone14,4", "iPhone 13 mini"),
    ("iPhone14,5", "iPhone 13"),
    ("iPhone14,2", "iPhone 13 Pro"),
    ("iPhone14,3", "iPhone 13 Pro Max"),
    ("iPhone14,6", "iPhone SE"),
    ("iPhone14,7", "iPhone 14"),
    ("iPhone14,8", "iPhone 14 Plus"),
    ("iPhone15,2", "iPhone 14 Pro"),
    ("iPhone15,3", "iPhone 14 Pro Max"),
    ("iPhone15,4", "iPhone 15"),
    ("iPhone15,5", "iPhone 15 Plus"),
    ("iPhone16,1", "iPhone 15 Pro"),
    ("iPhone16,2", "iPhone 15 Pro Max"),
    ("iPhone17,3", "iPhone 16"),
    ("iPhone17,4", "iPhone 16 Plus"),
    ("iPhone17,1", "iPhone 16 Pro"),
    ("iPhone17,2", "iPhone 16 Pro Max"),
    ("iPhone17,5", "iPhone 16e"),
    ("iPhone18,3", "iPhone 17"),
    ("iPhone18,1", "iPhone 17 Pro"),
    ("iPhone18,2", "iPhone 17 Pro Max"),
    ("iPhone18,4", "iPhone Air"),
];

/// The marketing name for a lockdown model identifier; models newer than the table show the identifier.
pub fn model_name(identifier: &str) -> &str {
    MODEL_NAMES
        .iter()
        .find(|(id, _)| *id == identifier)
        .map_or(identifier, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_thresholds_match_the_ios_app() {
        assert_eq!(Grade::from_score(85), Grade::Excellent);
        assert_eq!(Grade::from_score(84), Grade::Good);
        assert_eq!(Grade::from_score(65), Grade::Good);
        assert_eq!(Grade::from_score(64), Grade::Fair);
        assert_eq!(Grade::from_score(40), Grade::Fair);
        assert_eq!(Grade::from_score(39), Grade::Poor);
    }

    #[test]
    fn model_names() {
        assert_eq!(model_name("iPhone18,4"), "iPhone Air");
        assert_eq!(model_name("iPhone17,5"), "iPhone 16e");
        assert_eq!(model_name("iPhone99,1"), "iPhone99,1");
    }

    #[test]
    fn absent_readings_are_left_out_of_the_wire() {
        let card = PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("AT&T".into()),
                ..Default::default()
            }),
            stale: false,
            trail: vec![],
            usb: None,
        };
        let text = serde_json::to_string(&card).unwrap();
        assert!(text.contains("\"carrier\":\"AT&T\""));
        assert!(!text.contains("battery") && !text.contains("usb"));
    }

    #[test]
    fn a_gap_in_the_trail_survives_the_wire() {
        let card = PhoneCard {
            trail: vec![
                Some(SpeedPoint {
                    download: 1.0,
                    upload: 2.0,
                }),
                None,
            ],
            ..Default::default()
        };
        let back: PhoneCard = serde_json::from_str(&serde_json::to_string(&card).unwrap()).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn grade_comes_from_the_quality_score() {
        let frame = PhoneFrame {
            quality_score: Some(70),
            ..Default::default()
        };
        assert_eq!(frame.grade(), Some(Grade::Good));
        assert_eq!(PhoneFrame::default().grade(), None);
    }
}
