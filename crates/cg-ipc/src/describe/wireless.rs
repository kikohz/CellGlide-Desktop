//! Text for the wireless connection (design §4): the status line, each phase's title and
//! detail, the three steps, failures, hints and the paired iPhones. The command line, the
//! Windows views and the window all take their words from here.
//!
//! The passphrase is shown only where the caller passes it in (`wireless_steps`); nothing here
//! reads it from a snapshot, which never holds it.

use super::{StateText, reason_text};
use crate::wireless::{
    ApFailure, PairedPhone, PairingMethod, WirelessFailure, WirelessInfo, WirelessPhaseView,
    WirelessStatus,
};
use crate::{Reason, Snapshot};
use cg_i18n::tr;

/// The words for why the access point could not be started.
pub fn wireless_cause_text(cause: ApFailure) -> String {
    tr(cause.key(), &[])
}

/// The key of each failure's text.
fn failure_key(failure: &WirelessFailure) -> &'static str {
    match failure {
        WirelessFailure::AccessPoint { .. } => "wireless.failure.access_point",
        WirelessFailure::NoPhone => "wireless.failure.no_phone",
        WirelessFailure::SeveralPhones => "wireless.failure.several_phones",
        WirelessFailure::PairingStale => "wireless.failure.pairing_stale",
        WirelessFailure::PairingRefused => "wireless.failure.pairing_refused",
        WirelessFailure::NameClash => "wireless.failure.name_clash",
        WirelessFailure::Lost => "wireless.failure.lost",
        WirelessFailure::TurnedOff => "wireless.failure.turned_off",
        WirelessFailure::StoreUnavailable => "wireless.failure.store_unavailable",
        WirelessFailure::PhoneRefused { .. } => "wireless.failure.phone_refused",
        WirelessFailure::PairingDenied => "wireless.failure.pairing_denied",
        WirelessFailure::PairingFailed => "wireless.failure.pairing_failed",
        WirelessFailure::PairingBusy => "wireless.failure.pairing_busy",
        WirelessFailure::NotSharing => "wireless.failure.not_sharing",
    }
}

/// How the attempt ended, as one or two sentences.
pub fn wireless_failure_text(failure: &WirelessFailure) -> String {
    let key = failure_key(failure);
    match failure {
        WirelessFailure::AccessPoint { cause } => {
            tr(key, &[("cause", &wireless_cause_text(*cause))])
        }
        WirelessFailure::PhoneRefused { reason } => tr(key, &[("reason", &reason_text(reason))]),
        _ => tr(key, &[]),
    }
}

/// What holds the attempt up while it keeps waiting (`WirelessStatus::hint`). The hints the
/// attempt gives have words of their own; any other failure reads as itself.
pub fn wireless_hint_text(hint: &WirelessFailure) -> String {
    match hint {
        WirelessFailure::SeveralPhones => tr("wireless.hint.several_phones", &[]),
        WirelessFailure::PairingBusy => tr("wireless.hint.pairing_busy", &[]),
        WirelessFailure::NotSharing => tr("wireless.hint.not_sharing", &[]),
        other => wireless_failure_text(other),
    }
}

/// A six-digit code as two groups of three ("384 271"), as the iPhone shows it; anything else
/// as it is.
pub fn wireless_code(code: &str) -> String {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// The three steps shown while waiting for the iPhone: join the network, share in CellGlide,
/// and this computer connects by itself.
pub fn wireless_steps(ssid: &str, passphrase: &str) -> [String; 3] {
    [
        tr(
            "wireless.step.join",
            &[("ssid", ssid), ("passphrase", passphrase)],
        ),
        tr("wireless.step.share", &[]),
        tr("wireless.step.wait", &[]),
    ]
}

/// The note under the steps: the iPhone may call the network one without internet.
pub fn wireless_note() -> String {
    tr("wireless.step.note", &[])
}

/// The status line while a wireless attempt runs or has just failed (`cellglide status`, the
/// window's state line); `None` without one.
pub fn wireless_line(snapshot: &Snapshot) -> Option<String> {
    let status = snapshot.wireless.as_ref()?;
    Some(match &status.phase {
        WirelessPhaseView::Starting => tr("wireless.line.starting", &[]),
        WirelessPhaseView::Waiting => tr(
            "wireless.line.waiting",
            &[("ssid", status.ssid.as_deref().unwrap_or_default())],
        ),
        WirelessPhaseView::Pairing {
            code,
            confirmed: false,
            ..
        } => tr("wireless.line.pairing", &[("code", &wireless_code(code))]),
        WirelessPhaseView::Pairing {
            confirmed: true, ..
        } => tr("wireless.line.pairing_confirmed", &[]),
        WirelessPhaseView::Connecting => tr("wireless.line.connecting", &[]),
        WirelessPhaseView::Connected if status.weak => tr("wireless.line.connected_weak", &[]),
        WirelessPhaseView::Connected => tr("wireless.line.connected", &[]),
        WirelessPhaseView::Reconnecting => tr("wireless.line.reconnecting", &[]),
        WirelessPhaseView::Failed { reason } => tr(
            "wireless.line.failed",
            &[("reason", &wireless_failure_text(reason))],
        ),
    })
}

/// What holds a waiting attempt up, if anything: two devices with the iPhone's name, the hint,
/// or the last try's error.
fn waiting_trouble(status: &WirelessStatus) -> Option<String> {
    if status.name_clash {
        return Some(tr("wireless.hint.name_clash", &[]));
    }
    if let Some(hint) = &status.hint {
        return Some(wireless_hint_text(hint));
    }
    status.last_error.as_ref().map(reason_text)
}

/// The phase as a title and one sentence, for the steps card and the panel.
pub fn wireless_phase_text(status: &WirelessStatus) -> StateText {
    let text = |title: &str, detail: String| StateText {
        title: tr(title, &[]),
        detail,
    };
    match &status.phase {
        WirelessPhaseView::Starting => text(
            "wireless.starting.title",
            tr("wireless.starting.detail", &[]),
        ),
        WirelessPhaseView::Waiting => text(
            "wireless.waiting.title",
            waiting_trouble(status).unwrap_or_else(|| {
                tr(
                    "wireless.waiting.detail",
                    &[("ssid", status.ssid.as_deref().unwrap_or_default())],
                )
            }),
        ),
        WirelessPhaseView::Pairing {
            code,
            phone_name,
            confirmed,
        } => StateText {
            title: tr("wireless.pairing.title", &[("code", &wireless_code(code))]),
            detail: match (confirmed, phone_name) {
                (true, _) => tr("wireless.pairing.confirmed", &[]),
                (false, Some(name)) => tr("wireless.pairing.detail_named", &[("name", name)]),
                (false, None) => tr("wireless.pairing.detail", &[]),
            },
        },
        WirelessPhaseView::Connecting => text(
            "wireless.connecting.title",
            tr("wireless.connecting.detail", &[]),
        ),
        WirelessPhaseView::Connected => text(
            "wireless.connected.title",
            if status.weak {
                tr("wireless.closer", &[])
            } else {
                tr("wireless.connected.detail", &[])
            },
        ),
        WirelessPhaseView::Reconnecting => text(
            "wireless.reconnecting.title",
            if status.name_clash {
                tr("wireless.hint.name_clash", &[])
            } else {
                tr("wireless.closer", &[])
            },
        ),
        WirelessPhaseView::Failed { reason } => {
            text("wireless.failed.title", wireless_failure_text(reason))
        }
    }
}

pub fn pairing_method_text(method: PairingMethod) -> String {
    match method {
        PairingMethod::Code => tr("wireless.paired.code", &[]),
        PairingMethod::Cable => tr("wireless.paired.cable", &[]),
    }
}

/// One paired iPhone: "Name · Paired with a code", and "· Needs pairing again" when it went stale.
pub fn paired_phone_line(phone: &PairedPhone) -> String {
    let mut parts = vec![phone.name.clone(), pairing_method_text(phone.method)];
    if phone.stale {
        parts.push(tr("wireless.paired.stale", &[]));
    }
    parts.join(" · ")
}

/// Why wireless connections can't work on this computer, when the platform has them but
/// something is missing; `None` when they can (or the platform has none).
pub fn wireless_unavailable_text(info: &WirelessInfo) -> Option<String> {
    if info.available != Some(false) {
        return None;
    }
    Some(reason_text(
        info.unavailable
            .as_ref()
            .unwrap_or(&Reason::WirelessStoreUnavailable),
    ))
}

/// Every key the wireless text uses, for the completeness tests.
#[cfg(test)]
pub(crate) const KEYS: &[&str] = &[
    "wireless.closer",
    "wireless.step.join",
    "wireless.step.share",
    "wireless.step.wait",
    "wireless.step.note",
    "wireless.line.starting",
    "wireless.line.waiting",
    "wireless.line.pairing",
    "wireless.line.pairing_confirmed",
    "wireless.line.connecting",
    "wireless.line.connected",
    "wireless.line.connected_weak",
    "wireless.line.reconnecting",
    "wireless.line.failed",
    "wireless.starting.title",
    "wireless.starting.detail",
    "wireless.waiting.title",
    "wireless.waiting.detail",
    "wireless.pairing.title",
    "wireless.pairing.detail",
    "wireless.pairing.detail_named",
    "wireless.pairing.confirmed",
    "wireless.connecting.title",
    "wireless.connecting.detail",
    "wireless.connected.title",
    "wireless.connected.detail",
    "wireless.reconnecting.title",
    "wireless.failed.title",
    "wireless.hint.several_phones",
    "wireless.hint.pairing_busy",
    "wireless.hint.not_sharing",
    "wireless.hint.name_clash",
    "wireless.paired.code",
    "wireless.paired.cable",
    "wireless.paired.stale",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConnState;
    use cg_i18n::Lang;

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

    fn with(status: WirelessStatus) -> Snapshot {
        Snapshot {
            wireless: Some(status),
            ..Snapshot::default()
        }
    }

    fn phases() -> Vec<WirelessPhaseView> {
        let mut phases = vec![
            WirelessPhaseView::Starting,
            WirelessPhaseView::Waiting,
            WirelessPhaseView::Pairing {
                code: "384271".into(),
                phone_name: None,
                confirmed: false,
            },
            WirelessPhaseView::Pairing {
                code: "384271".into(),
                phone_name: Some("Test iPhone".into()),
                confirmed: false,
            },
            WirelessPhaseView::Pairing {
                code: "384271".into(),
                phone_name: None,
                confirmed: true,
            },
            WirelessPhaseView::Connecting,
            WirelessPhaseView::Connected,
            WirelessPhaseView::Reconnecting,
        ];
        phases.extend(
            WirelessFailure::all()
                .into_iter()
                .map(|reason| WirelessPhaseView::Failed { reason }),
        );
        phases
    }

    fn untranslated(text: &str) -> bool {
        text.is_empty()
            || text.contains("wireless.")
            || text.contains("reason.")
            || text.contains("%{")
    }

    /// Every key exists in both languages, and every phase, failure, hint and cause reads as
    /// words in both.
    #[test]
    fn every_wireless_text_is_translated() {
        let _g = cg_i18n::lock_for_tests();
        for key in KEYS {
            assert!(cg_i18n::KEYS.contains(key), "{key} missing from KEYS");
        }
        for cause in ApFailure::ALL {
            assert!(cg_i18n::KEYS.contains(&cause.key()), "{cause:?}");
        }
        for failure in WirelessFailure::all() {
            assert!(
                cg_i18n::KEYS.contains(&failure_key(&failure)),
                "{failure:?}"
            );
        }
        for lang in [Lang::ZhCn, Lang::En] {
            cg_i18n::set(lang);
            for phase in phases() {
                let mut weak = status(phase.clone());
                weak.weak = true;
                for s in [status(phase.clone()), weak] {
                    let line = wireless_line(&with(s.clone())).unwrap();
                    assert!(!untranslated(&line), "{lang:?} {phase:?}: {line}");
                    let text = wireless_phase_text(&s);
                    assert!(
                        !untranslated(&text.title) && !untranslated(&text.detail),
                        "{lang:?} {phase:?}: {text:?}"
                    );
                }
            }
            for failure in WirelessFailure::all() {
                for text in [
                    wireless_failure_text(&failure),
                    wireless_hint_text(&failure),
                ] {
                    assert!(!untranslated(&text), "{lang:?} {failure:?}: {text}");
                }
            }
            for step in wireless_steps("CellGlide-AB12", "abcd-efgh-2345") {
                assert!(!untranslated(&step), "{step}");
            }
            assert!(!untranslated(&wireless_note()));
            for method in [PairingMethod::Code, PairingMethod::Cable] {
                assert!(!untranslated(&pairing_method_text(method)));
            }
        }
    }

    #[test]
    fn the_copy_follows_the_design() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::ZhCn);
        let connected = with(status(WirelessPhaseView::Connected));
        assert_eq!(
            wireless_line(&connected).unwrap(),
            "已通过无线连接 · 建议插线更稳"
        );
        let mut reconnecting = status(WirelessPhaseView::Reconnecting);
        assert!(
            wireless_line(&with(reconnecting.clone()))
                .unwrap()
                .contains("把手机放近一点，或插上数据线")
        );
        assert_eq!(
            wireless_phase_text(&reconnecting).detail,
            "把手机放近一点，或插上数据线"
        );
        reconnecting.name_clash = true;
        assert_ne!(
            wireless_phase_text(&reconnecting).detail,
            "把手机放近一点，或插上数据线"
        );
        let pairing = status(WirelessPhaseView::Pairing {
            code: "384271".into(),
            phone_name: None,
            confirmed: false,
        });
        let text = wireless_phase_text(&pairing);
        assert_eq!(text.title, "在 iPhone 上确认验证码 384 271");
        assert!(
            text.detail.contains("请与 iPhone 屏幕上的数字核对"),
            "{text:?}"
        );
        let [join, share, wait] = wireless_steps("CellGlide-AB12", "abcd-efgh-2345");
        assert!(
            join.contains("CellGlide-AB12") && join.contains("abcd-efgh-2345"),
            "{join}"
        );
        assert!(share.contains("开始共享"), "{share}");
        assert_eq!(wait, "这台电脑会自动连上。");
        assert_eq!(
            wireless_note(),
            "iPhone 可能提示这个网络无法连接互联网，这是正常的，上网走的是蜂窝。"
        );
        assert!(
            wireless_hint_text(&WirelessFailure::NotSharing).contains("VPN 模式"),
            "the VPN-mode hint"
        );
        assert_eq!(
            wireless_failure_text(&WirelessFailure::NoPhone),
            "没有等到 iPhone"
        );
        assert_eq!(
            wireless_failure_text(&WirelessFailure::PairingStale),
            "这台 iPhone 已不再允许这台电脑无线连接，请重新配对"
        );
        assert!(
            wireless_failure_text(&WirelessFailure::AccessPoint {
                cause: ApFailure::MobileHotspotOn
            })
            .contains("移动热点")
        );
        cg_i18n::set(Lang::En);
        assert_eq!(
            wireless_line(&connected).unwrap(),
            "Connected wirelessly · a cable is steadier"
        );
        assert_eq!(
            wireless_phase_text(&pairing).title,
            "Confirm the code 384 271 on the iPhone"
        );
    }

    #[test]
    fn no_wireless_no_line() {
        let _g = cg_i18n::lock_for_tests();
        let snap = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: crate::DnsMode::Phone,
                wireless: false,
            },
            ..Snapshot::default()
        };
        assert_eq!(wireless_line(&snap), None);
    }

    #[test]
    fn waiting_says_what_holds_it_up() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        let plain = status(WirelessPhaseView::Waiting);
        assert!(
            wireless_phase_text(&plain)
                .detail
                .contains("CellGlide-AB12"),
            "the network to join"
        );
        let mut busy = plain.clone();
        busy.hint = Some(WirelessFailure::PairingBusy);
        assert_eq!(
            wireless_phase_text(&busy).detail,
            wireless_hint_text(&WirelessFailure::PairingBusy)
        );
        let mut clash = busy.clone();
        clash.name_clash = true;
        assert_eq!(
            wireless_phase_text(&clash).detail,
            tr("wireless.hint.name_clash", &[])
        );
        let mut erred = plain.clone();
        erred.last_error = Some(Reason::NoExit);
        assert_eq!(
            wireless_phase_text(&erred).detail,
            reason_text(&Reason::NoExit)
        );
    }

    #[test]
    fn codes_are_grouped() {
        assert_eq!(wireless_code("384271"), "384 271");
        assert_eq!(wireless_code("042137"), "042 137");
        assert_eq!(wireless_code("12345"), "12345");
        assert_eq!(wireless_code("abcdef"), "abcdef");
    }

    #[test]
    fn paired_phones_and_availability() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        let mut phone = PairedPhone {
            peer_id: "A1".into(),
            name: "Test iPhone".into(),
            method: PairingMethod::Cable,
            stale: false,
            created_at: 1,
        };
        assert_eq!(
            paired_phone_line(&phone),
            "Test iPhone · Paired over the cable"
        );
        phone.stale = true;
        phone.method = PairingMethod::Code;
        assert_eq!(
            paired_phone_line(&phone),
            "Test iPhone · Paired with a code · Needs pairing again"
        );
        let mut info = WirelessInfo {
            available: Some(true),
            unavailable: None,
            ssid: String::new(),
            passphrase: String::new(),
            paired: vec![],
        };
        assert_eq!(wireless_unavailable_text(&info), None);
        info.available = None;
        assert_eq!(wireless_unavailable_text(&info), None);
        info.available = Some(false);
        info.unavailable = Some(Reason::WirelessStoreUnavailable);
        assert_eq!(
            wireless_unavailable_text(&info).unwrap(),
            reason_text(&Reason::WirelessStoreUnavailable)
        );
    }

    #[test]
    fn the_unavailable_reason_names_its_cause() {
        let _g = cg_i18n::lock_for_tests();
        for lang in [Lang::ZhCn, Lang::En] {
            cg_i18n::set(lang);
            for cause in ApFailure::ALL {
                let text = reason_text(&Reason::WirelessUnavailable { cause });
                assert!(text.contains(&wireless_cause_text(cause)), "{text}");
                assert!(!untranslated(&text), "{text}");
            }
        }
    }
}
