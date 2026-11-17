//! The wireless connection (design §4): the overview's and the panel's card, the settings
//! page's switch and paired iPhones, and the steps card's network name and password.
//!
//! The snapshot never holds the access point's password, and neither does anything made from
//! it here: the password reaches the app only in [`WirelessInfoView`], the reply to
//! `Command::WirelessInfo`, which the app asks for when it shows the steps card.

use cg_i18n::tr;
use cg_ipc::describe;
use cg_ipc::wireless::{ApFailure, PairedPhone, WirelessInfo, WirelessPhaseView, WirelessStatus};
use cg_ipc::{ConnState, Reason, Snapshot};
use serde::{Deserialize, Serialize};

/// The attempt's phase, without what it carries.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WirelessPhase {
    Starting,
    Waiting,
    Pairing,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// The overview's and the panel's wireless part; present while the button is offered or an
/// attempt runs (or has just failed).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WirelessView {
    /// "Connect Wirelessly" (or "Try Again" after a failure) is offered: `button_label`.
    pub offered: bool,
    /// The secondary button's label while `offered`.
    pub button_label: Option<String>,
    /// `None` while there is no attempt (only the button).
    pub phase: Option<WirelessPhase>,
    /// The phase as a title and a sentence (`describe::wireless_phase_text`): the code dialog's
    /// words while pairing, the failure's after one; empty without a phase.
    pub title: String,
    pub detail: String,
    /// Show the steps card (opening the network, waiting for the iPhone); its network name,
    /// password and steps come with `Command::WirelessInfo`.
    pub show_steps: bool,
    /// The access point's name, once it is known.
    pub ssid: Option<String>,
    /// The code to compare, in two groups ("384 271"), while pairing.
    pub code: Option<String>,
    /// The code was confirmed here; waiting for the iPhone (the dialog keeps showing the code).
    pub confirmed: bool,
    /// A hint beside the phase: while reconnecting, that the iPhone may have joined another Wi‑Fi.
    pub hint: Option<String>,
    /// An attempt runs (it owns the phase): Cancel is offered, and "Pair Again" and "New Network
    /// Name and Password" are not.
    pub can_cancel: bool,
}

/// The settings page's wireless part.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WirelessSettingsView {
    /// The service's switch.
    pub enabled: bool,
    /// Turning the switch on is being checked (it can take half a minute): show it busy.
    pub checking: bool,
    /// An attempt owns the phase: "Pair Again" and "New Network Name and Password" are off.
    pub busy: bool,
    /// Why wireless connections can't work on this computer; `None` when they can (or it isn't
    /// known yet).
    pub available_text: Option<String>,
    /// The paired iPhones, while the switch is on.
    pub paired: Vec<PairedPhoneView>,
}

/// One paired iPhone in the settings list.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PairedPhoneView {
    /// What `WirelessForget` and `WirelessRepair` name.
    pub peer_id: String,
    pub name: String,
    /// "Paired with a code" or "Paired over the cable".
    pub method: String,
    /// The iPhone no longer accepts this pairing: show "Needs pairing again" with the "Pair Again" button.
    pub stale: bool,
}

/// The reply to `Command::WirelessInfo`: the network name and password for the steps card, and
/// the paired iPhones. The only place the password goes to the app.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct WirelessInfoView {
    /// Why wireless connections can't work on this computer; `None` when they can.
    pub unavailable: Option<String>,
    pub ssid: String,
    pub passphrase: String,
    /// The three steps, with the network name and the password in the first.
    pub steps: Vec<String>,
    /// "The iPhone may say this network has no internet…".
    pub note: String,
    pub paired: Vec<PairedPhoneView>,
}

impl std::fmt::Debug for WirelessInfoView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WirelessInfoView")
            .field("unavailable", &self.unavailable)
            .field("ssid", &self.ssid)
            .field("passphrase", &"<hidden>")
            .field("steps", &"<hidden>")
            .field("paired", &self.paired)
            .finish()
    }
}

fn phase_of(phase: &WirelessPhaseView) -> WirelessPhase {
    match phase {
        WirelessPhaseView::Starting => WirelessPhase::Starting,
        WirelessPhaseView::Waiting => WirelessPhase::Waiting,
        WirelessPhaseView::Pairing { .. } => WirelessPhase::Pairing,
        WirelessPhaseView::Connecting => WirelessPhase::Connecting,
        WirelessPhaseView::Connected => WirelessPhase::Connected,
        WirelessPhaseView::Reconnecting => WirelessPhase::Reconnecting,
        WirelessPhaseView::Failed { .. } => WirelessPhase::Failed,
    }
}

/// The attempt's phase, if there is one.
pub fn wireless_phase(snapshot: &Snapshot) -> Option<WirelessPhase> {
    snapshot.wireless.as_ref().map(|s| phase_of(&s.phase))
}

/// Whether an attempt holds the phase (everything but a failure that ended one).
fn owns(status: Option<&WirelessStatus>) -> bool {
    status.is_some_and(|s| !matches!(s.phase, WirelessPhaseView::Failed { .. }))
}

/// The overview's and the panel's wireless part; `None` when there is nothing to show.
pub fn wireless(snapshot: &Snapshot) -> Option<WirelessView> {
    let status = snapshot.wireless.as_ref();
    let offered = snapshot.wireless_offered;
    if status.is_none() && !offered {
        return None;
    }
    let failed = status.is_some_and(|s| matches!(s.phase, WirelessPhaseView::Failed { .. }));
    let button_label = offered.then(|| {
        tr(
            if failed {
                "wireless.action.retry"
            } else {
                "wireless.action.connect"
            },
            &[],
        )
    });
    let text = status.map(describe::wireless_phase_text);
    let phase = status.map(|s| phase_of(&s.phase));
    let (code, confirmed) = match status.map(|s| &s.phase) {
        Some(WirelessPhaseView::Pairing {
            code, confirmed, ..
        }) => (Some(describe::wireless_code(code)), *confirmed),
        _ => (None, false),
    };
    let ssid = status.and_then(|s| s.ssid.clone());
    let hint = (phase == Some(WirelessPhase::Reconnecting)).then(|| {
        tr(
            "wireless.hint.other_wifi",
            &[("ssid", ssid.as_deref().unwrap_or_default())],
        )
    });
    Some(WirelessView {
        offered,
        button_label,
        phase,
        title: text.as_ref().map(|t| t.title.clone()).unwrap_or_default(),
        detail: text.map(|t| t.detail).unwrap_or_default(),
        show_steps: matches!(
            phase,
            Some(WirelessPhase::Starting | WirelessPhase::Waiting)
        ),
        ssid,
        code,
        confirmed,
        hint,
        can_cancel: owns(status),
    })
}

/// The overview's title and sentence while a wireless attempt runs: the phase's own words, and
/// once connected the wireless line ("Connected wirelessly · a cable is steadier") over the
/// usual sentence; `None` when the usual state text stands.
pub fn state_text(snapshot: &Snapshot) -> Option<describe::StateText> {
    let status = snapshot.wireless.as_ref()?;
    match (&status.phase, &snapshot.state) {
        (WirelessPhaseView::Failed { .. }, _) => None,
        (WirelessPhaseView::Connected, ConnState::Connected { .. }) => Some(describe::StateText {
            title: describe::wireless_line(snapshot)?,
            detail: describe::gui_state(snapshot).detail,
        }),
        _ => Some(describe::wireless_phase_text(status)),
    }
}

/// The settings page's wireless part. `info` is the service's last `WirelessInfo` (its password
/// is never read here), `None` while it hasn't been asked or couldn't be.
pub fn wireless_settings(snapshot: &Snapshot, info: Option<&WirelessInfo>) -> WirelessSettingsView {
    let enabled = snapshot.settings.wireless_enabled;
    WirelessSettingsView {
        enabled,
        checking: snapshot.wireless_checking,
        busy: owns(snapshot.wireless.as_ref()),
        available_text: info.and_then(describe::wireless_unavailable_text),
        paired: match info {
            Some(info) if enabled => paired(&info.paired),
            _ => Vec::new(),
        },
    }
}

fn paired(phones: &[PairedPhone]) -> Vec<PairedPhoneView> {
    phones
        .iter()
        .map(|phone| PairedPhoneView {
            peer_id: phone.peer_id.clone(),
            name: phone.name.clone(),
            method: describe::pairing_method_text(phone.method),
            stale: phone.stale,
        })
        .collect()
}

/// The reply to `Command::WirelessInfo`.
pub fn wireless_info(info: &WirelessInfo) -> WirelessInfoView {
    let unavailable = match info.available {
        // No wireless on this platform at all.
        None => Some(describe::reason_text(&Reason::WirelessUnavailable {
            cause: ApFailure::Unsupported,
        })),
        Some(_) => describe::wireless_unavailable_text(info),
    };
    let steps = if unavailable.is_none() {
        describe::wireless_steps(&info.ssid, &info.passphrase).to_vec()
    } else {
        Vec::new()
    };
    WirelessInfoView {
        unavailable,
        ssid: info.ssid.clone(),
        passphrase: info.passphrase.clone(),
        steps,
        note: describe::wireless_note(),
        paired: paired(&info.paired),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{connected_snapshot, disconnected_snapshot};
    use cg_ipc::DnsMode;
    use cg_ipc::wireless::{PairingMethod, WirelessFailure};

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    fn status(phase: WirelessPhaseView) -> WirelessStatus {
        WirelessStatus {
            phase,
            ssid: Some("CellGlide-AB12".into()),
            weak: false,
            name_clash: false,
            last_error: None,
            hint: None,
        }
    }

    fn attempt(phase: WirelessPhaseView) -> Snapshot {
        let mut s = disconnected_snapshot();
        s.settings.wireless_enabled = true;
        s.state = ConnState::Connecting;
        s.wireless = Some(status(phase));
        s
    }

    fn pairing(confirmed: bool) -> WirelessPhaseView {
        WirelessPhaseView::Pairing {
            code: "384271".into(),
            phone_name: None,
            confirmed,
        }
    }

    fn failed() -> WirelessPhaseView {
        WirelessPhaseView::Failed {
            reason: WirelessFailure::NoPhone,
        }
    }

    pub(crate) fn info() -> WirelessInfo {
        WirelessInfo {
            available: Some(true),
            unavailable: None,
            ssid: "CellGlide-AB12".into(),
            passphrase: "test-pass-0000".into(),
            paired: vec![
                PairedPhone {
                    peer_id: "A1".into(),
                    name: "Anna's iPhone".into(),
                    method: PairingMethod::Code,
                    stale: false,
                    created_at: 1_763_000_000,
                },
                PairedPhone {
                    peer_id: "B2".into(),
                    name: "Ben's iPhone".into(),
                    method: PairingMethod::Cable,
                    stale: true,
                    created_at: 1_763_000_100,
                },
            ],
        }
    }

    #[test]
    fn nothing_without_an_attempt_or_the_button() {
        let _g = en();
        assert_eq!(wireless(&disconnected_snapshot()), None);
        assert_eq!(state_text(&disconnected_snapshot()), None);
    }

    #[test]
    fn the_button_alone_while_offered() {
        let _g = en();
        let mut s = disconnected_snapshot();
        s.wireless_offered = true;
        let v = wireless(&s).expect("offered");
        assert!(v.offered && !v.can_cancel && !v.show_steps);
        assert_eq!(v.button_label.as_deref(), Some("Connect Wirelessly"));
        assert_eq!(v.phase, None);
        assert_eq!((v.title.as_str(), v.detail.as_str()), ("", ""));
    }

    #[test]
    fn each_phase_has_its_title_and_buttons() {
        let _g = en();
        // (phase, kind, steps card, cancel)
        let cases = [
            (
                WirelessPhaseView::Starting,
                WirelessPhase::Starting,
                true,
                true,
            ),
            (
                WirelessPhaseView::Waiting,
                WirelessPhase::Waiting,
                true,
                true,
            ),
            (pairing(false), WirelessPhase::Pairing, false, true),
            (pairing(true), WirelessPhase::Pairing, false, true),
            (
                WirelessPhaseView::Connecting,
                WirelessPhase::Connecting,
                false,
                true,
            ),
            (
                WirelessPhaseView::Connected,
                WirelessPhase::Connected,
                false,
                true,
            ),
            (
                WirelessPhaseView::Reconnecting,
                WirelessPhase::Reconnecting,
                false,
                true,
            ),
            (failed(), WirelessPhase::Failed, false, false),
        ];
        for (phase, kind, steps, cancel) in cases {
            let s = attempt(phase.clone());
            let v = wireless(&s).expect("an attempt");
            let text = describe::wireless_phase_text(s.wireless.as_ref().unwrap());
            assert_eq!(v.phase, Some(kind), "{phase:?}");
            assert_eq!((v.show_steps, v.can_cancel), (steps, cancel), "{phase:?}");
            assert_eq!((&v.title, &v.detail), (&text.title, &text.detail));
            assert!(!v.title.is_empty(), "{phase:?}");
            assert_eq!(v.ssid.as_deref(), Some("CellGlide-AB12"));
            // Not offered while it runs (the service says so); no button then.
            assert!(!v.offered && v.button_label.is_none());
        }
    }

    #[test]
    fn pairing_shows_the_code_and_then_waits_for_the_iphone() {
        let _g = en();
        let v = wireless(&attempt(pairing(false))).unwrap();
        assert_eq!(v.code.as_deref(), Some("384 271"));
        assert!(!v.confirmed);
        assert_eq!(v.title, "Confirm the code 384 271 on the iPhone");
        assert_eq!(
            v.detail,
            "Compare it with the number on the iPhone's screen."
        );
        // Confirmed here: the code stays, waiting for the iPhone.
        let v = wireless(&attempt(pairing(true))).unwrap();
        assert_eq!(v.code.as_deref(), Some("384 271"));
        assert!(v.confirmed);
        assert_eq!(v.detail, cg_i18n::tr("wireless.pairing.confirmed", &[]));
        // No code outside pairing.
        assert_eq!(
            wireless(&attempt(WirelessPhaseView::Waiting)).unwrap().code,
            None
        );
    }

    #[test]
    fn a_failure_says_why_and_offers_a_retry() {
        let _g = en();
        let mut s = attempt(failed());
        s.state = ConnState::Idle;
        s.wireless_offered = true;
        let v = wireless(&s).unwrap();
        assert_eq!(v.phase, Some(WirelessPhase::Failed));
        assert_eq!(v.detail, "No iPhone showed up");
        assert_eq!(v.button_label.as_deref(), Some("Try Again"));
        assert!(!v.can_cancel);
        // The overview keeps its usual state text.
        assert_eq!(state_text(&s), None);
        // Not offered (a phone on the cable): the reason, no button.
        s.wireless_offered = false;
        let v = wireless(&s).unwrap();
        assert!(v.button_label.is_none() && !v.offered);
    }

    #[test]
    fn reconnecting_hints_that_the_iphone_may_have_joined_another_wifi() {
        let _g = en();
        let v = wireless(&attempt(WirelessPhaseView::Reconnecting)).unwrap();
        let hint = v.hint.expect("a hint while reconnecting");
        assert!(hint.contains("CellGlide-AB12"), "{hint}");
        for phase in [
            WirelessPhaseView::Waiting,
            WirelessPhaseView::Connected,
            failed(),
        ] {
            assert_eq!(wireless(&attempt(phase)).unwrap().hint, None);
        }
    }

    #[test]
    fn the_overview_says_the_phase_and_the_wireless_line_once_connected() {
        let _g = en();
        let s = attempt(WirelessPhaseView::Waiting);
        let text = state_text(&s).unwrap();
        assert_eq!(text.title, "Waiting for the iPhone");
        let mut s = connected_snapshot();
        s.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Phone,
            wireless: true,
        };
        s.wireless = Some(status(WirelessPhaseView::Connected));
        let text = state_text(&s).unwrap();
        assert_eq!(text.title, "Connected wirelessly · a cable is steadier");
        assert_eq!(text.detail, describe::gui_state(&s).detail);
        s.wireless.as_mut().unwrap().weak = true;
        assert_eq!(
            state_text(&s).unwrap().title,
            cg_i18n::tr("wireless.line.connected_weak", &[])
        );
    }

    #[test]
    fn settings_list_the_paired_iphones_while_the_switch_is_on() {
        let _g = en();
        let mut s = disconnected_snapshot();
        s.settings.wireless_enabled = true;
        s.wireless_checking = true;
        let info = info();
        let v = wireless_settings(&s, Some(&info));
        assert!(v.enabled && v.checking && !v.busy);
        assert_eq!(v.available_text, None);
        assert_eq!(
            v.paired,
            vec![
                PairedPhoneView {
                    peer_id: "A1".into(),
                    name: "Anna's iPhone".into(),
                    method: "Paired with a code".into(),
                    stale: false,
                },
                PairedPhoneView {
                    peer_id: "B2".into(),
                    name: "Ben's iPhone".into(),
                    method: "Paired over the cable".into(),
                    stale: true,
                },
            ]
        );
        // An attempt owns the phase: busy.
        let v = wireless_settings(&attempt(WirelessPhaseView::Waiting), Some(&info));
        assert!(v.busy);
        let v = wireless_settings(&attempt(failed()), Some(&info));
        assert!(!v.busy);
        // Off: no list.
        s.settings.wireless_enabled = false;
        assert!(wireless_settings(&s, Some(&info)).paired.is_empty());
        // Not asked yet.
        s.settings.wireless_enabled = true;
        assert_eq!(wireless_settings(&s, None).paired, vec![]);
        // The store can't be opened: why.
        let broken = WirelessInfo {
            available: Some(false),
            unavailable: Some(Reason::WirelessStoreUnavailable),
            ..info
        };
        assert_eq!(
            wireless_settings(&s, Some(&broken)).available_text,
            Some(describe::reason_text(&Reason::WirelessStoreUnavailable))
        );
    }

    #[test]
    fn the_info_reply_has_the_steps_with_the_password() {
        let _g = en();
        let v = wireless_info(&info());
        assert_eq!(v.unavailable, None);
        assert_eq!(v.passphrase, "test-pass-0000");
        assert_eq!(v.steps.len(), 3);
        assert!(v.steps[0].contains("CellGlide-AB12") && v.steps[0].contains("test-pass-0000"));
        assert_eq!(v.note, describe::wireless_note());
        assert_eq!(v.paired.len(), 2);
        // Never printed.
        assert!(!format!("{v:?}").contains("test-pass-0000"));
        // No wireless here at all: why, and no steps.
        let none = WirelessInfo {
            available: None,
            ssid: String::new(),
            passphrase: String::new(),
            paired: Vec::new(),
            unavailable: None,
        };
        let v = wireless_info(&none);
        assert!(v.unavailable.is_some() && v.steps.is_empty());
    }
}
