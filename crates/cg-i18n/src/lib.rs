//! User-facing text in Simplified Chinese and English. Every sentence a user can see
//! lives in `locales/*.yml`; other crates pass keys and arguments.

rust_i18n::i18n!("locales", fallback = "en");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    ZhCn,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhCn => "zh-CN",
            Lang::En => "en",
        }
    }

    /// A POSIX locale tag such as `zh_CN.UTF-8`.
    pub fn from_tag(tag: &str) -> Lang {
        if tag.to_ascii_lowercase().starts_with("zh") {
            Lang::ZhCn
        } else {
            Lang::En
        }
    }

    /// The language the environment asks for; on Windows without any of the variables, the
    /// user's first preferred display language.
    pub fn from_env() -> Lang {
        ["CELLGLIDE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty())
            .or_else(os_language)
            .map_or(Lang::En, |value| Lang::from_tag(&value))
    }
}

#[cfg(not(windows))]
fn os_language() -> Option<String> {
    None
}

/// The first entry of `GetUserPreferredUILanguages`, e.g. `zh-CN` or `en-US`.
#[cfg(windows)]
fn os_language() -> Option<String> {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    use windows::core::PWSTR;
    let mut count = 0u32;
    let mut len = 0u32;
    // SAFETY: a null buffer asks only for the length, written to `len`.
    unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len) }.ok()?;
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: `buffer` holds `len` UTF-16 units, as the call above asked for.
    unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
    }
    .ok()?;
    // A list of NUL-terminated names ending with an empty one.
    let first = buffer.split(|&unit| unit == 0).next()?;
    let name = String::from_utf16(first).ok()?;
    (!name.is_empty()).then_some(name)
}

pub fn set(lang: Lang) {
    rust_i18n::set_locale(lang.code());
}

/// Serialises tests that read or switch the process-wide language. Hold the guard for the whole
/// test. Not `#[cfg(test)]` because other crates' tests call it; a poisoned lock is recovered.
pub fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    static LANGUAGE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LANGUAGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The language `tr` currently answers in.
pub fn current() -> Lang {
    if rust_i18n::locale().starts_with("zh") {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

pub fn init() {
    set(Lang::from_env());
}

fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for (name, value) in args {
        text = text.replace(&format!("%{{{name}}}"), value);
    }
    text
}

pub fn tr(key: &str, args: &[(&str, &str)]) -> String {
    fill(&rust_i18n::t!(key), args)
}

pub fn tr_in(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    fill(&rust_i18n::t!(key, locale = lang.code()), args)
}

/// Like [`tr`], but on Windows `<key>_windows` wins when the locale files have it (plan
/// decision 6). On other systems this is exactly `tr`.
pub fn tr_os(key: &str, args: &[(&str, &str)]) -> String {
    #[cfg(windows)]
    {
        let windows = format!("{key}_windows");
        // Every key exists in English (the fallback), so English decides whether the variant exists.
        if _rust_i18n_backend_translate(Lang::En.code(), &windows).is_some() {
            return tr(&windows, args);
        }
    }
    tr(key, args)
}

pub const KEYS: &[&str] = &[
    "list_separator",
    "state.no_usb_service",
    "state.no_usb_service_windows",
    "state.no_phone",
    "state.choose_phone",
    "state.idle",
    "state.connecting",
    "state.connected",
    "state.connected_public_dns",
    "state.connected_dns_unmanaged",
    "state.retrying",
    "phone.card_title",
    "phone.detecting",
    "phone.not_sharing",
    "phone.quality",
    "phone.quality_measuring",
    "phone.loss",
    "phone.session",
    "phone.grade.excellent",
    "phone.grade.good",
    "phone.grade.fair",
    "phone.grade.poor",
    "phone.charging_watts",
    "phone.supply",
    "phone.supply_wireless",
    "phone.full",
    "phone.plugged_not_charging",
    "phone.battery_temperature",
    "phone.battery_health",
    "phone.cycles",
    "phone.hot",
    "phone.overheating",
    "phone.hot_degrees",
    "phone.overheating_degrees",
    "phone.battery_level",
    "phone.battery_level_charging",
    "reason.no_usb_service",
    "reason.no_usb_service_windows",
    "reason.phone_not_ready",
    "reason.phone_too_old",
    "reason.session_changed",
    "reason.no_exit",
    "reason.phone_gone",
    "reason.resolver_port_busy",
    "reason.chosen_phone_missing",
    "reason.forwarding_stopped",
    "reason.timeout",
    "reason.tunnel",
    "reason.internal",
    "reason.not_in_group",
    "reason.not_allowed",
    "reason.bad_request",
    "reason.shutting_down",
    "reason.version_mismatch",
    "reason.bad_day",
    "health.cause.channel",
    "health.cause.phone_network",
    "health.cause.computer_relay",
    "health.cause.phone_unstable",
    "health.cause.partial_failures",
    "health.recovered",
    "health.escalated",
    "health.ongoing",
    "health.ok",
    "health.ok_last",
    "health.seconds",
    "health.minutes",
    "history.title",
    "history.today",
    "history.yesterday",
    "history.date",
    "history.older",
    "history.weekday.1",
    "history.weekday.2",
    "history.weekday.3",
    "history.weekday.4",
    "history.weekday.5",
    "history.weekday.6",
    "history.weekday.7",
    "history.empty",
    "history.retry",
    "history.legend.ok",
    "history.legend.unstable",
    "history.legend.down",
    "history.legend.not_sharing",
    "history.days",
    "history.event.tunnel_started",
    "history.event.tunnel_stopped",
    "history.event.usb_attached",
    "history.event.usb_detached",
    "history.event.version_changed",
    "history.event.service_started",
    "history.event.bypass_unavailable",
    "history.event.bypass_update_failed",
    "history.event.dns_not_authorized",
    "history.detail.cable_latency",
    "history.detail.phone_latency",
    "history.detail.loss",
    "history.detail.network",
    "history.detail.failures",
    "history.detail.phone_temperature",
    "history.detail.main_cause",
    "history.failure.timeout",
    "history.failure.refused",
    "history.failure.dns",
    "history.failure.other",
    "history.thermal.0",
    "history.thermal.1",
    "history.thermal.2",
    "history.thermal.3",
    "check.fix",
    "check.service.ok",
    "check.service.fail",
    "check.service.fail_windows",
    "check.service.impostor",
    "check.service.fix",
    "check.service.fix_windows",
    "check.service.fix_not_allowed",
    "check.service.fix_impostor",
    "check.group.ok",
    "check.group.fail",
    "check.group.fix",
    "check.usbmuxd.ok",
    "check.usbmuxd.idle",
    "check.usbmuxd.fail",
    "check.usbmuxd.ok_windows",
    "check.usbmuxd.idle_windows",
    "check.usbmuxd.fail_windows",
    "check.usbmuxd.fix",
    "check.usbmuxd.fix_windows",
    "check.phone.ok",
    "check.phone.fail",
    "check.phone.fix",
    "check.trust.ok",
    "check.trust.fail",
    "check.trust.fix",
    "check.trust.fix_windows",
    "check.resolved.ok",
    "check.resolved.fail",
    "check.resolved.fix",
    "check.polkit.ok",
    "check.polkit.fail",
    "check.polkit.fix",
    "check.polkit_rule.ok",
    "check.polkit_rule.fail",
    "check.polkit_rule.fix",
    "check.tun.ok",
    "check.tun.fail",
    "check.tun.fix",
    "check.apple_service.ok",
    "check.apple_service.kind_store",
    "check.apple_service.kind_itunes",
    "check.apple_service.fail",
    "check.apple_service.fix",
    "check.wintun.ok",
    "check.wintun.fail",
    "check.wintun.fix",
    "check.dns.ok",
    "check.dns.fail",
    "check.dns.fix",
    "cli.disconnected",
    "cli.still_connecting",
    "cli.service_stopped",
    "cli.unexpected_reply",
    "cli.settings_auto_connect",
    "cli.settings_phone",
    "cli.on",
    "cli.off",
    "cli.phone_auto",
    "cli.settings_saved",
    "diag.about",
    "diag.confirm",
    "diag.needs_yes",
    "diag.sudo_hint",
    "diag.sending",
    "diag.sent",
    "diag.too_frequent",
    "diag.failed",
    "diag.saved",
    "diag.cancelled",
    "diag.send_title",
    "diag.about_gui",
    "diag.range",
    "diag.range_day",
    "diag.range_week",
    "diag.email",
    "diag.note",
    "diag.view",
    "diag.send",
    "diag.preparing",
    "diag.preview_title",
    "diag.preview_size",
    "diag.send_needs_preview",
    "diag.sent_title",
    "diag.code_hint",
    "diag.again",
    "diag.build_failed",
    "diag.failed_gui",
    "diag.save",
    "diag.open_folder",
    "diag.save_failed",
    "diag.no_home",
    "leak.running",
    "leak.start",
    "leak.again",
    "leak.none_seen",
    "leak.run_checks",
    "leak.attribution",
    "leak.about",
    "leak.you",
    "leak.resolvers",
    "leak.unknown_network",
    "leak.many_ips",
    "leak.queries",
    "leak.subnet",
    "leak.unavailable",
    "leak.failed",
    "leak.encrypted.title",
    "leak.encrypted.detail",
    "leak.carrier.title",
    "leak.carrier.detail",
    "leak.leak.title",
    "leak.leak.detail",
    "leak.not_sharing.title",
    "leak.not_sharing.detail",
    "leak.nothing.title",
    "leak.nothing.detail",
    "leak.unmanaged.title",
    "leak.unmanaged.detail",
    "speed.data_warning",
    "speed.needs_yes",
    "speed.no_phone",
    "speed.phone",
    "speed.wifi",
    "speed.wifi_short",
    "speed.wifi_only",
    "speed.latency",
    "speed.download",
    "speed.upload",
    "speed.stage.latency",
    "speed.stage.transfer",
    "speed.done",
    "speed.cancelled",
    "speed.cap_note",
    "speed.wifi_faster",
    "speed.phone_faster",
    "speed.similar",
    "speed.history_empty",
    "speed.page_about",
    "speed.include_wifi",
    "speed.start",
    "speed.warning",
    "speed.dont_ask",
    "speed.recent",
    "speed.clear_confirm",
    "speed.history_failed",
    "speed.error.no_interface",
    "speed.error.not_connected",
    "speed.error.rate_limited",
    "speed.error.refused",
    "speed.error.timeout",
    "speed.error.closed_early",
    "speed.error.bad_length",
    "speed.error.chunked",
    "speed.error.bad_reply",
    "speed.error.io",
    "gui.title",
    "gui.title_connected",
    "gui.connect",
    "gui.disconnect_long",
    "gui.waiting_service",
    "gui.service_gone",
    "gui.service_timeout",
    "gui.state.waiting_usb.title",
    "gui.state.waiting_usb.detail",
    "gui.state.choose_phone.title",
    "gui.state.choose_phone.detail",
    "gui.state.idle.title",
    "gui.state.idle.detail",
    "gui.state.connecting.title",
    "gui.state.connecting.detail",
    "gui.state.connected.title",
    "gui.state.connected.detail",
    "gui.state.connected_public.detail",
    "gui.state.connected_unmanaged.detail",
    "gui.state.retrying.title",
    "panel.open",
    "panel.quit",
    "overview.connection",
    "overview.usb_device",
    "overview.auto_device",
    "overview.change",
    "tray.open",
    "tray.connect",
    "tray.disconnect",
    "tray.quit",
    "nav.overview",
    "nav.tools",
    "nav.activity",
    "nav.app",
    "nav.speed_test",
    "nav.dns_leak",
    "nav.history",
    "nav.diagnostics",
    "nav.settings",
    "nav.about",
    "common.copy",
    "common.copied",
    "common.cancel",
    "common.done",
    "common.continue",
    "common.clear",
    "common.optional",
    "common.page",
    "common.prev",
    "common.next",
    "common.close",
    "about.version",
    "about.telegram",
    "about.billing",
    "about.licenses",
    "about.licenses_note",
    "about.show",
    "about.quit",
    "settings.usb_device",
    "settings.auto_discover",
    "settings.remembered_offline",
    "settings.switch_note",
    "settings.auto_connect",
    "settings.auto_connect_note",
    "settings.launch_at_login",
    "settings.launch_note",
    "settings.open_on_plug",
    "settings.open_on_plug_note",
    "settings.advanced",
    "settings.proxy",
    "settings.proxy_note",
    "settings.proxy_none",
    "settings.dns",
    "settings.dns_phone",
    "settings.dns_public",
    "settings.dns_unmanaged",
    "settings.dns_idle",
    "settings.run_checks",
    "settings.checking",
    "settings.autostart_failed",
    "doctor.running",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<String> {
        let mut names: Vec<String> = text
            .split("%{")
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name.to_string()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_key_exists_in_both_languages_with_the_same_placeholders() {
        for key in KEYS {
            let zh = tr_in(Lang::ZhCn, key, &[]);
            let en = tr_in(Lang::En, key, &[]);
            assert_ne!(zh, *key, "zh-CN missing {key}");
            assert_ne!(en, *key, "en missing {key}");
            // `t!` falls back to English, so also ask each locale's own table directly.
            // `_rust_i18n_backend_translate` is macro-private (doc(hidden)), not public API.
            for lang in [Lang::ZhCn, Lang::En] {
                assert!(
                    crate::_rust_i18n_backend_translate(lang.code(), key).is_some(),
                    "{} has no own entry for {key}",
                    lang.code()
                );
            }
            assert_eq!(
                placeholders(&zh),
                placeholders(&en),
                "placeholders differ for {key}"
            );
        }
    }

    #[test]
    fn substitutes_arguments() {
        assert_eq!(
            tr_in(Lang::En, "reason.resolver_port_busy", &[("port", "1053")]),
            "The local DNS port 1053 is already in use"
        );
        assert_eq!(
            tr_in(Lang::ZhCn, "check.fix", &[("fix", "sudo modprobe tun")]),
            "修复：sudo modprobe tun"
        );
    }

    #[test]
    fn tr_os_prefers_the_windows_key_on_windows() {
        let _g = lock_for_tests();
        set(Lang::En);
        let text = tr_os("reason.no_usb_service", &[]);
        if cfg!(windows) {
            assert!(text.contains("Apple Devices"), "{text}");
            assert!(!text.contains("usbmuxd"), "{text}");
        } else {
            assert_eq!(text, tr("reason.no_usb_service", &[]));
            assert!(text.contains("usbmuxd"), "{text}");
        }
        // Without a Windows variant both platforms use the shared key.
        assert_eq!(tr_os("check.trust.ok", &[]), tr("check.trust.ok", &[]));
        assert_eq!(
            tr_os("check.apple_service.ok", &[("kind", "x")]),
            tr("check.apple_service.ok", &[("kind", "x")])
        );
    }

    #[test]
    fn windows_keys_exist_in_both_languages() {
        let windows: Vec<&str> = KEYS
            .iter()
            .copied()
            .filter(|k| k.ends_with("_windows"))
            .collect();
        for key in [
            "state.no_usb_service_windows",
            "reason.no_usb_service_windows",
            "check.service.fix_windows",
            "check.usbmuxd.ok_windows",
            "check.usbmuxd.idle_windows",
            "check.usbmuxd.fail_windows",
            "check.usbmuxd.fix_windows",
            "check.trust.fix_windows",
        ] {
            assert!(windows.contains(&key), "{key} missing from KEYS");
        }
        for key in windows {
            let shared = key.strip_suffix("_windows").unwrap();
            assert!(KEYS.contains(&shared), "{key} has no shared key {shared}");
            for lang in [Lang::ZhCn, Lang::En] {
                assert!(
                    crate::_rust_i18n_backend_translate(lang.code(), key).is_some(),
                    "{} has no {key}",
                    lang.code()
                );
            }
        }
        let fix = tr_in(Lang::En, "check.usbmuxd.fix_windows", &[]);
        assert!(
            fix.contains("winget install --id 9NP83LWLPZ9K --source msstore")
                && fix.contains("ms-windows-store://pdp/?ProductId=9NP83LWLPZ9K"),
            "{fix}"
        );
        for lang in [Lang::ZhCn, Lang::En] {
            let text = tr_in(lang, "state.no_usb_service_windows", &[]);
            assert!(!text.contains("Start-Service"), "{text}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_a_display_language() {
        let name = os_language().expect("a preferred UI language");
        assert!(name.contains('-') || name.len() == 2, "{name}");
    }

    #[test]
    fn picks_language_from_tags() {
        assert_eq!(Lang::from_tag("zh_CN.UTF-8"), Lang::ZhCn);
        assert_eq!(Lang::from_tag("ZH_tw"), Lang::ZhCn);
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Lang::En);
        assert_eq!(Lang::from_tag("C"), Lang::En);
        // Windows display-language names.
        assert_eq!(Lang::from_tag("zh-Hans-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_tag("en-US"), Lang::En);
        assert_eq!(Lang::from_tag(""), Lang::En);
    }
}
