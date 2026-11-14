//! The wireless session as the snapshot shows it. Internal for now: the snapshot carries it only
//! while a wireless attempt is going on or has just failed (`Snapshot::wireless`, left out of the
//! JSON otherwise), so version 6 clients and snapshots are unaffected. The protocol bump that
//! makes it public comes with the wireless requests.
//!
//! The access point's passphrase is never part of any of these.

use crate::Reason;
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The iPhone was reached but can't be used, and trying again won't change that (e.g. its
    /// app is too old or lacks a capability).
    PhoneRefused { reason: Reason },
    /// Code pairing: the iPhone's user said no.
    PairingDenied,
    /// Code pairing did not complete: the code was not confirmed here (or not in time), the
    /// connection broke, or the iPhone's lines did not check out. Nothing was saved.
    PairingFailed,
    /// The iPhone turned code pairing away (another pairing in progress, or its lockout after
    /// three failures) until the window ran out.
    PairingBusy,
}

/// The phase of the wireless attempt (tagged `phase`). `Debug` never prints the pairing code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Code pairing: show `code` and ask whether the iPhone shows the same
    /// (`ConfirmCode { matches }`).
    Pairing {
        code: String,
        /// The iPhone's name, when this computer knows it (an earlier pairing with it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phone_name: Option<String>,
    },
    Failed {
        reason: WirelessFailure,
    },
}

impl std::fmt::Debug for WirelessPhaseView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WirelessPhaseView::Starting => f.write_str("Starting"),
            WirelessPhaseView::Waiting => f.write_str("Waiting"),
            WirelessPhaseView::Connecting => f.write_str("Connecting"),
            WirelessPhaseView::Connected => f.write_str("Connected"),
            WirelessPhaseView::Reconnecting => f.write_str("Reconnecting"),
            WirelessPhaseView::Pairing { phone_name, .. } => f
                .debug_struct("Pairing")
                .field("code", &"<hidden>")
                .field("phone_name", phone_name)
                .finish(),
            WirelessPhaseView::Failed { reason } => {
                f.debug_struct("Failed").field("reason", reason).finish()
            }
        }
    }
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
    /// Why the last try to build the session failed, while the attempt keeps trying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<Reason>,
    /// What holds the attempt up while it keeps waiting (several unpaired iPhones, the iPhone
    /// turning pairing away for now); the attempt ends with this failure if it lasts until the
    /// window runs out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<WirelessFailure>,
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
            last_error: None,
            hint: None,
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
    fn code_pairing_shapes_and_failures() {
        let pairing = WirelessPhaseView::Pairing {
            code: "042137".into(),
            phone_name: Some("Test iPhone".into()),
        };
        let json = serde_json::to_string(&pairing).unwrap();
        assert_eq!(
            json,
            r#"{"phase":"pairing","code":"042137","phone_name":"Test iPhone"}"#
        );
        assert_eq!(
            serde_json::from_str::<WirelessPhaseView>(&json).unwrap(),
            pairing
        );
        let unnamed: WirelessPhaseView =
            serde_json::from_str(r#"{"phase":"pairing","code":"042137"}"#).unwrap();
        assert_eq!(
            unnamed,
            WirelessPhaseView::Pairing {
                code: "042137".into(),
                phone_name: None
            }
        );
        // The code is for the screen, never for logs.
        assert!(!format!("{pairing:?}").contains("042137"));
        for failure in [
            WirelessFailure::PairingDenied,
            WirelessFailure::PairingFailed,
            WirelessFailure::PairingBusy,
        ] {
            let json = serde_json::to_string(&failure).unwrap();
            assert_eq!(
                serde_json::from_str::<WirelessFailure>(&json).unwrap(),
                failure
            );
        }
        assert_eq!(
            serde_json::to_string(&WirelessFailure::PairingBusy).unwrap(),
            r#"{"code":"pairing_busy"}"#
        );
        let held: WirelessStatus = serde_json::from_str(
            r#"{"phase":{"phase":"waiting"},"hint":{"code":"several_phones"}}"#,
        )
        .unwrap();
        assert_eq!(held.hint, Some(WirelessFailure::SeveralPhones));
        let plain = WirelessStatus {
            phase: WirelessPhaseView::Waiting,
            ssid: None,
            weak: false,
            name_clash: false,
            last_error: None,
            hint: None,
        };
        assert!(!serde_json::to_string(&plain).unwrap().contains("hint"));
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
            WirelessFailure::PhoneRefused {
                reason: Reason::PhoneTooOld,
            },
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
