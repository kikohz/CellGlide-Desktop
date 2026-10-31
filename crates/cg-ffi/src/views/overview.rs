//! The overview page, the panel, the tray, the sidebar's dots and the settings page, all from
//! one snapshot.

use super::chrome::{ChromeView, chrome};
use cg_i18n::tr;
use cg_ipc::describe::{self, BatteryBadge, Heat, ui};
use cg_ipc::phone::{PhoneCard, TRAIL_LENGTH};
use cg_ipc::{ConnState, Snapshot};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// The overview page, the panel and the tray, from the service's snapshot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OverviewView {
    pub connected: bool,
    pub wants_connection: bool,
    pub title: String,
    pub detail: String,
    pub status_tone: ui::Tone,
    pub button_label: String,
    pub device_subtitle: String,
    /// Only while connected, as on the Mac.
    pub health: Option<HealthRowView>,
    /// Windows' strict DNS, while connected.
    pub strict_dns: Option<String>,
    /// Only when the card has something to show.
    pub phone: Option<PhoneCardView>,
    /// The tray, its menu, the sidebar and its dots.
    pub chrome: ChromeView,
    pub settings: SettingsView,
}

/// The health row: its dot and its line.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HealthRowView {
    pub tone: ui::Tone,
    pub line: String,
}

/// The settings page's values.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SettingsView {
    /// The service's value. Whether a change is still on its way is the app's to know, so
    /// `ui::AutoConnectSwitch` (`Hidden` without a snapshot, `Busy` while saving) is not sent.
    pub auto_connect: bool,
    pub strict_dns: bool,
    /// The phone chooser's entries.
    pub phones: Vec<ui::PhoneOption>,
    /// Which of `phones` is selected now.
    pub selected_phone: usize,
    pub dns: ui::DnsLine,
    /// The manual proxy address, while the tunnel runs.
    pub proxy: Option<String>,
}

/// The phone card; present only when `describe::card_visible`. Fields from `describe::card_text`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PhoneCardView {
    pub title: String,
    pub carrier: Option<String>,
    pub radio: Option<String>,
    pub model: Option<String>,
    pub battery: Option<BatteryView>,
    pub speeds: ui::Speeds,
    pub download: Option<String>,
    pub upload: Option<String>,
    pub quality: Option<String>,
    pub battery_line: Option<String>,
    pub heat: Option<HeatView>,
    /// Draw the card faded.
    pub dim: bool,
    /// The last minute's speeds as runs of consecutive readings (a gap ends a run).
    pub trail: Vec<Vec<ui::TrailPoint>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BatteryView {
    pub percent: u32,
    pub charging: bool,
    pub low_power: bool,
    /// "80%", or with the charging mark.
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HeatView {
    pub text: String,
    /// Shown red.
    pub severe: bool,
}

pub fn overview(snapshot: &Snapshot, now: Timestamp, tz: &TimeZone) -> OverviewView {
    let some = Some(snapshot);
    let state = describe::gui_state(snapshot);
    OverviewView {
        connected: ui::is_connected(some),
        wants_connection: snapshot.wants_connection,
        title: state.title,
        detail: state.detail,
        status_tone: ui::status_tone(some),
        button_label: ui::button_label(some),
        device_subtitle: ui::device_subtitle(some),
        health: ui::connected_health_line(snapshot, now, tz).map(|line| HealthRowView {
            tone: ui::health_tone(snapshot),
            line,
        }),
        strict_dns: describe::strict_dns_line(snapshot),
        phone: snapshot
            .phone
            .as_ref()
            .filter(|card| describe::card_visible(card))
            .map(phone_card),
        chrome: chrome(some, None),
        settings: settings(snapshot),
    }
}

fn settings(snapshot: &Snapshot) -> SettingsView {
    let (phones, selected) = ui::phone_options(snapshot);
    // Always in the list: a remembered phone that isn't plugged in has its own entry ("not
    // connected"), as in the Linux window, so the choice is never shown as "automatic".
    let selected_phone = phones.iter().position(|option| *option == selected);
    debug_assert!(
        selected_phone.is_some(),
        "phone_options lists its selection"
    );
    let selected_phone = selected_phone.unwrap_or(0);
    let dns = match &snapshot.state {
        ConnState::Connected { dns, .. } => Some(*dns),
        _ => None,
    };
    SettingsView {
        auto_connect: snapshot.settings.auto_connect,
        strict_dns: snapshot.settings.strict_dns,
        phones,
        selected_phone,
        dns: ui::dns_line(dns),
        proxy: snapshot.proxy_address(),
    }
}

fn phone_card(card: &PhoneCard) -> PhoneCardView {
    let text = describe::card_text(card);
    PhoneCardView {
        speeds: ui::speeds(card, &text),
        trail: ui::trail_runs(&card.trail, TRAIL_LENGTH),
        title: text.title,
        carrier: text.carrier,
        radio: text.radio,
        model: text.model,
        battery: text.battery.map(battery),
        download: text.download,
        upload: text.upload,
        quality: text.quality,
        battery_line: text.battery_line,
        heat: text
            .heat
            .map(|Heat { text, severe }| HeatView { text, severe }),
        dim: text.dim,
    }
}

/// The battery badge and its label, as the Linux card draws it.
fn battery(badge: BatteryBadge) -> BatteryView {
    let key = if badge.charging {
        "phone.battery_level_charging"
    } else {
        "phone.battery_level"
    };
    BatteryView {
        label: tr(key, &[("percent", &badge.percent.to_string())]),
        percent: badge.percent,
        charging: badge.charging,
        low_power: badge.low_power,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{SERIAL, connected_snapshot, disconnected_snapshot};
    use cg_ipc::health::IssueCause;
    use cg_ipc::phone::SpeedPoint;
    use cg_ipc::{DnsMode, PhoneChoice, StrictDns};
    use ui::{Badge, Tone};

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    fn now() -> Timestamp {
        "2026-10-30T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn overview_of_a_connected_snapshot() {
        let _g = en();
        let s = connected_snapshot();
        let v = overview(&s, now(), &TimeZone::UTC);
        assert!(v.connected && v.chrome.tray.active);
        assert_eq!(v.title, describe::gui_state(&s).title);
        assert_eq!(v.status_tone, Tone::Green);
        assert_eq!(v.button_label, cg_i18n::tr("gui.disconnect_long", &[]));
        assert!(v.phone.is_some());
    }

    #[test]
    fn overview_of_a_disconnected_snapshot() {
        let _g = en();
        let v = overview(&disconnected_snapshot(), now(), &TimeZone::UTC);
        assert!(!v.connected && v.health.is_none() && v.strict_dns.is_none());
        assert_eq!(v.button_label, cg_i18n::tr("gui.connect", &[]));
    }

    #[test]
    fn strict_dns_line_only_where_it_applies() {
        let _g = en();
        let with = |strict| {
            overview(
                &Snapshot {
                    strict_dns: strict,
                    ..connected_snapshot()
                },
                now(),
                &TimeZone::UTC,
            )
            .strict_dns
        };
        assert_eq!(
            with(StrictDns::On),
            Some(cg_i18n::tr("state.strict_dns_on", &[]))
        );
        assert_eq!(with(StrictDns::NotApplicable), None);
        assert_eq!(
            with(StrictDns::Disabled),
            Some(cg_i18n::tr("state.strict_dns_disabled", &[]))
        );
    }

    #[test]
    fn selected_phone_points_into_the_options() {
        let _g = en();
        let mut s = connected_snapshot();
        s.settings.chosen_serial = Some(SERIAL.into());
        let v = overview(&s, now(), &TimeZone::UTC);
        assert_eq!(
            v.settings.phones[v.settings.selected_phone].choice,
            PhoneChoice::Serial {
                serial: SERIAL.into()
            }
        );
        // Nothing chosen: "automatic", the first entry.
        let v = overview(&disconnected_snapshot(), now(), &TimeZone::UTC);
        assert_eq!(v.settings.selected_phone, 0);
        assert_eq!(v.settings.phones[0].choice, PhoneChoice::Auto);
    }

    #[test]
    fn health_row_and_badges_follow_an_open_issue() {
        let _g = en();
        let mut s = connected_snapshot();
        s.health.cause = Some(IssueCause::PhoneNetwork);
        s.health.since = Some(now() - jiff::SignedDuration::from_mins(3));
        let v = overview(&s, now(), &TimeZone::UTC);
        let health = v.health.expect("a health row while connected");
        assert_eq!(health.tone, ui::health_tone(&s));
        assert_eq!(
            Some(health.line),
            ui::connected_health_line(&s, now(), &TimeZone::UTC)
        );
        assert!(v.chrome.tray.alert);
        assert_ne!(v.chrome.badges.overview, Badge::None);
        assert_eq!(v.chrome.badges.history, v.chrome.badges.overview);
        assert_eq!(v.chrome.badges.settings, Badge::None);
    }

    #[test]
    fn settings_carry_the_service_values_and_the_proxy() {
        let _g = en();
        let mut s = connected_snapshot();
        s.settings.auto_connect = false;
        s.settings.strict_dns = false;
        let v = overview(&s, now(), &TimeZone::UTC);
        assert!(!v.settings.auto_connect && !v.settings.strict_dns);
        assert_eq!(v.settings.dns, ui::dns_line(Some(DnsMode::Phone)));
        assert_eq!(
            v.settings.proxy.as_deref(),
            Some("socks5h://127.0.0.1:1080")
        );
        let v = overview(&disconnected_snapshot(), now(), &TimeZone::UTC);
        assert_eq!(v.settings.dns, ui::dns_line(None));
        assert_eq!(v.settings.proxy, None);
    }

    #[test]
    fn the_phone_card_is_the_card_text() {
        let _g = en();
        let mut s = connected_snapshot();
        let card = s.phone.as_mut().unwrap();
        card.trail = vec![
            Some(SpeedPoint {
                download: 1.0,
                upload: 1.0,
            }),
            None,
            Some(SpeedPoint {
                download: 2.0,
                upload: 0.5,
            }),
        ];
        let text = describe::card_text(card);
        let v = overview(&s, now(), &TimeZone::UTC).phone.unwrap();
        assert_eq!(v.title, text.title);
        assert_eq!(v.radio.as_deref(), Some("5G"));
        assert_eq!(v.download, text.download);
        assert_eq!(v.speeds, ui::Speeds::Shared);
        assert_eq!(v.trail.len(), 2, "a gap splits the trail");
        // A card with nothing to show is no card.
        s.phone = Some(PhoneCard::default());
        assert_eq!(overview(&s, now(), &TimeZone::UTC).phone, None);
    }

    #[test]
    fn the_battery_label_names_the_percentage() {
        let _g = en();
        let plain = battery(BatteryBadge {
            percent: 80,
            charging: false,
            low_power: false,
        });
        let charging = battery(BatteryBadge {
            percent: 80,
            charging: true,
            low_power: true,
        });
        assert!(plain.label.contains("80"));
        assert_ne!(plain.label, charging.label);
        assert!(charging.charging && charging.low_power);
    }

    #[test]
    fn a_remembered_phone_that_is_not_plugged_in_stays_selected() {
        let _g = en();
        let mut s = disconnected_snapshot();
        s.settings.chosen_serial = Some("OTHER".into());
        let v = overview(&s, now(), &TimeZone::UTC);
        let selected = &v.settings.phones[v.settings.selected_phone];
        assert_eq!(
            selected.choice,
            PhoneChoice::Serial {
                serial: "OTHER".into()
            }
        );
        assert_eq!(
            selected.label,
            cg_i18n::tr("settings.remembered_offline", &[])
        );
        assert_ne!(v.settings.selected_phone, 0);
    }

    #[test]
    fn the_view_has_no_tuples_or_fixed_arrays() {
        let _g = en();
        // Every optional part filled in, so the walk reaches all of it.
        let mut s = connected_snapshot();
        s.health.cause = Some(IssueCause::PhoneNetwork);
        s.health.since = Some(now());
        s.settings.chosen_serial = Some("OTHER".into());
        s.strict_dns = StrictDns::On;
        let card = s.phone.as_mut().unwrap();
        card.trail = vec![Some(SpeedPoint {
            download: 1.0,
            upload: 2.0,
        })];
        let frame = card.frame.as_mut().unwrap();
        frame.battery = Some(0.8);
        frame.thermal = Some(3);
        let v = overview(&s, now(), &TimeZone::UTC);
        assert!(
            v.phone
                .as_ref()
                .is_some_and(|p| p.battery.is_some() && p.heat.is_some())
        );
        assert!(v.health.is_some() && v.strict_dns.is_some());
        assert_eq!(crate::testing::tuple_free(&v), Ok(()));
        // Options are null, never left out.
        let json = serde_json::to_value(&v).unwrap();
        assert!(json["phone"]["model"].is_null());
        assert_eq!(serde_json::from_value::<OverviewView>(json).unwrap(), v);
    }
}
