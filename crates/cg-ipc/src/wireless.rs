//! The wireless session as the snapshot shows it. Internal for now: the snapshot carries it only
//! while a wireless attempt is going on or has just failed (`Snapshot::wireless`, left out of the
//! JSON otherwise), so version 6 clients and snapshots are unaffected. The protocol bump that
//! makes it public comes with the wireless requests.
//!
//! The access point's passphrase is never part of any of these.

use serde::{Deserialize, Serialize};

/// Why the access point could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ApFailure {
    #[error("this computer has no wireless adapter")]
    NoAdapter,
    #[error("wireless is turned off")]
    RadioOff,
    /// Windows' Mobile Hotspot is on; the two can't run together and it wins.
    #[error("Mobile Hotspot is on")]
    MobileHotspotOn,
    #[error("the wireless adapter is in use by something else")]
    InUse,
    #[error("the wireless adapter can't host an access point")]
    Unsupported,
}

/// How a wireless attempt ended when it did not end because the user (or USB) ended it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WirelessFailure {
    /// The access point could not be started.
    AccessPoint { cause: ApFailure },
    /// No paired iPhone turned up while the access point waited.
    NoPhone,
    /// More than one unpaired iPhone (code pairing).
    SeveralPhones,
    /// The iPhone, at an address where it had already answered with this pairing, no longer
    /// knows it: the pairing is marked stale (kept), pair again.
    PairingStale,
    /// The window ran out on refusals of the pairing that prove nothing (they did not come from
    /// an address verified for the phone): the pairing is left as it is.
    PairingRefused,
    /// The paired iPhone's name was advertised from more than one address until the window ran
    /// out (something on the access point may pose as it): nothing was dialled.
    NameClash,
    /// The session ended and could not be brought back in time, or the system stopped the
    /// access point.
    Lost,
    /// The wireless switch was turned off.
    TurnedOff,
    /// The pairing store could not give the access point's name and passphrase.
    StoreUnavailable,
}

/// The phase of the wireless attempt (tagged `phase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WirelessPhaseView {
    /// Starting the access point.
    Starting,
    /// The access point is up; waiting for the iPhone to join and be found.
    Waiting,
    /// Connecting to the iPhone that was found.
    Connecting,
    Connected,
    /// The session ended; trying to get it back while the access point stays up.
    Reconnecting,
    Failed {
        reason: WirelessFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirelessStatus {
    pub phase: WirelessPhaseView,
    /// The access point's network name while it is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    /// The link is slow: the median of the last five SOCKS handshakes is above the wireless
    /// limit. A hint only; it never ends the session.
    #[serde(default)]
    pub weak: bool,
    /// The paired iPhone's name has been advertised from more than one address for several
    /// rounds, so nothing is dialled.
    #[serde(default)]
    pub name_clash: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let status = WirelessStatus {
            phase: WirelessPhaseView::Failed {
                reason: WirelessFailure::AccessPoint {
                    cause: ApFailure::MobileHotspotOn,
                },
            },
            ssid: None,
            weak: false,
            name_clash: false,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            json,
            r#"{"phase":{"phase":"failed","reason":{"code":"access_point","cause":"mobile_hotspot_on"}},"weak":false,"name_clash":false}"#
        );
        assert_eq!(
            serde_json::from_str::<WirelessStatus>(&json).unwrap(),
            status
        );
        let waiting: WirelessStatus =
            serde_json::from_str(r#"{"phase":{"phase":"waiting"},"ssid":"CellGlide-AB12"}"#)
                .unwrap();
        assert_eq!(waiting.phase, WirelessPhaseView::Waiting);
        assert_eq!(waiting.ssid.as_deref(), Some("CellGlide-AB12"));
        assert!(!waiting.weak && !waiting.name_clash);
    }

    #[test]
    fn every_failure_round_trips() {
        let causes = [
            ApFailure::NoAdapter,
            ApFailure::RadioOff,
            ApFailure::MobileHotspotOn,
            ApFailure::InUse,
            ApFailure::Unsupported,
        ];
        let mut all: Vec<WirelessFailure> = causes
            .iter()
            .map(|&cause| WirelessFailure::AccessPoint { cause })
            .collect();
        all.extend([
            WirelessFailure::NoPhone,
            WirelessFailure::SeveralPhones,
            WirelessFailure::PairingStale,
            WirelessFailure::PairingRefused,
            WirelessFailure::NameClash,
            WirelessFailure::Lost,
            WirelessFailure::TurnedOff,
            WirelessFailure::StoreUnavailable,
        ]);
        for failure in all {
            let json = serde_json::to_string(&failure).unwrap();
            assert_eq!(
                serde_json::from_str::<WirelessFailure>(&json).unwrap(),
                failure
            );
        }
    }
}
