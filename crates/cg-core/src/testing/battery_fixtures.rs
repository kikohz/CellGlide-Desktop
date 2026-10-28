//! AppleSmartBattery registry entries for tests, built the way the diagnostics relay returns them.

use plist::{Dictionary, Value};

pub fn int(value: i64) -> Value {
    Value::Integer(value.into())
}

pub fn dict<const N: usize>(entries: [(&str, Value); N]) -> Dictionary {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Trimmed from a real iPhone Air charging from a Mac's USB-C port (same values as the Mac's tests).
pub fn charging() -> Dictionary {
    dict([
        ("IsCharging", Value::Boolean(true)),
        ("ExternalConnected", Value::Boolean(true)),
        ("FullyCharged", Value::Boolean(false)),
        ("CurrentCapacity", int(75)),
        ("Voltage", int(4207)),
        ("InstantAmperage", int(609)),
        ("Amperage", int(542)),
        ("CycleCount", int(396)),
        (
            "AdapterDetails",
            Value::Dictionary(dict([
                ("Description", "pd charger".into()),
                ("Watts", int(15)),
                ("IsWireless", Value::Boolean(false)),
                ("AdapterVoltage", int(5000)),
                ("Current", int(3000)),
            ])),
        ),
        (
            "BatteryData",
            Value::Dictionary(dict([
                ("DesignCapacity", int(3116)),
                ("NominalChargeCapacity", int(2957)),
                ("CurrentCapacity", int(75)),
                ("FullChargeCapacity", int(2994)),
                ("RemainingCapacity", int(2237)),
            ])),
        ),
    ])
}
