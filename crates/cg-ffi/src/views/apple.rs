//! Apple Devices (the Microsoft Store app whose usbmuxd the service talks to) installed but
//! not answering: the overview, the panel and the doctor's Apple check offer to open it, and
//! the app starts its background part by itself.

use cg_i18n::tr;
use cg_ipc::{APPLE_DEVICES_APP, Check, CheckId};
use serde::{Deserialize, Serialize};

/// The "Open Apple Devices" button and the sentence beside it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppleDevicesView {
    pub button_label: String,
    /// It can take a few minutes after sign-in; CellGlide also tries in the background.
    pub hint: String,
}

pub fn apple_devices() -> AppleDevicesView {
    AppleDevicesView {
        button_label: tr("gui.apple_devices_open", &[]),
        hint: tr("gui.apple_devices_hint", &[]),
    }
}

/// The doctor's Apple check says the Apple Devices app is installed (usbmuxd is its, not a
/// Windows service's) but nothing answers on port 27015.
pub fn stopped(check: &Check) -> bool {
    let arg = |name: &str| check.args.get(name).map(String::as_str);
    check.id == CheckId::AppleService
        && check.ok
        && arg("app") == Some(APPLE_DEVICES_APP)
        && arg("running") == Some("false")
}

/// [`stopped`] for any of `checks`.
pub fn any_stopped(checks: &[Check]) -> bool {
    checks.iter().any(stopped)
}

/// What the doctor's checks say about Apple Devices, while usbmuxd isn't reachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AppleWord {
    /// The Apple Devices app is installed but not answering: offer it, and start it.
    Stopped,
    /// Apple's device service isn't installed at all (the Apple check failed).
    Missing,
    /// Anything else (answering after all, or iTunes' Windows service).
    #[default]
    Other,
}

pub fn word(checks: &[Check]) -> AppleWord {
    if any_stopped(checks) {
        AppleWord::Stopped
    } else if checks
        .iter()
        .any(|c| c.id == CheckId::AppleService && !c.ok)
    {
        AppleWord::Missing
    } else {
        AppleWord::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple(app: Option<&str>, running: &str) -> Check {
        let check = Check::new(CheckId::AppleService, true)
            .with("kind", "store")
            .with("running", running);
        match app {
            Some(app) => check.with("app", app),
            None => check,
        }
    }

    #[test]
    fn only_the_apple_devices_app_that_is_not_answering_is_stopped() {
        assert!(stopped(&apple(Some(APPLE_DEVICES_APP), "false")));
        assert!(!stopped(&apple(Some(APPLE_DEVICES_APP), "true")));
        // iTunes' Windows service (also under WindowsApps when it came from the Store).
        assert!(!stopped(&apple(None, "false")));
        // Not installed.
        let missing = Check::new(CheckId::AppleService, false)
            .with("running", "false")
            .with("app", APPLE_DEVICES_APP);
        assert!(!stopped(&missing));
        // Another check with the same arguments.
        let mut usbmuxd = apple(Some(APPLE_DEVICES_APP), "false");
        usbmuxd.id = CheckId::Usbmuxd;
        assert!(!stopped(&usbmuxd));
        assert!(any_stopped(&[
            usbmuxd,
            apple(Some(APPLE_DEVICES_APP), "false")
        ]));
        assert!(!any_stopped(&[]));
    }

    #[test]
    fn the_word_is_stopped_missing_or_other() {
        assert_eq!(
            word(&[apple(Some(APPLE_DEVICES_APP), "false")]),
            AppleWord::Stopped
        );
        assert_eq!(
            word(&[Check::new(CheckId::AppleService, false).with("running", "false")]),
            AppleWord::Missing
        );
        assert_eq!(word(&[apple(None, "false")]), AppleWord::Other);
        assert_eq!(word(&[]), AppleWord::Other);
    }

    #[test]
    fn the_texts_are_the_locale_files() {
        let _g = cg_i18n::lock_for_tests();
        for lang in [cg_i18n::Lang::En, cg_i18n::Lang::ZhCn] {
            cg_i18n::set(lang);
            let view = apple_devices();
            assert_eq!(view.button_label, tr("gui.apple_devices_open", &[]));
            assert_eq!(view.hint, tr("gui.apple_devices_hint", &[]));
            assert!(!view.button_label.starts_with("gui."), "a real text");
        }
        cg_i18n::set(cg_i18n::Lang::En);
        assert_eq!(apple_devices().button_label, "Open Apple Devices");
        assert!(apple_devices().hint.contains("few minutes"));
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        assert!(apple_devices().button_label.contains("Apple 设备"));
        assert!(apple_devices().hint.contains("几分钟"));
        cg_i18n::set(cg_i18n::Lang::En);
    }
}
