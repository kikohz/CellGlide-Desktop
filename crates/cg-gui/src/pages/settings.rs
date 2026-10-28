//! The settings page, after the Mac's `SettingsPage`: which iPhone to use, connecting
//! automatically, starting at login, opening the window when an iPhone is plugged in, the
//! manual proxy address, and DNS with the doctor's checks. Changes to the connection go to the
//! service as one `SetSettings` each; starting at login is the user's autostart file.

use super::Ctx;
use crate::effect::{AfterMsg, DoctorFor, Effect, Reply, SettingField};
use crate::prefs::PrefChange;
use crate::widgets::checks::{self, Run};
use crate::widgets::kit::{self, Tone};
use cg_i18n::tr;
use cg_ipc::{Check, DnsMode, PhoneChoice, Request, Snapshot, describe};
use iced::widget::{button, column, container, pick_list, row, rule, text, toggler};
use iced::{Alignment, Background, Border, Element, Font, Length, Theme};
use std::fmt;
use std::time::Duration;

/// One entry of the phone chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneOption {
    pub choice: PhoneChoice,
    pub label: String,
}

impl fmt::Display for PhoneOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// The chooser's entries and the one that is selected now. First "automatic"; a remembered
/// phone that isn't plugged in comes next; then every plugged-in phone by name and model.
pub fn phone_options(snapshot: &Snapshot) -> (Vec<PhoneOption>, PhoneOption) {
    let auto = PhoneOption {
        choice: PhoneChoice::Auto,
        label: tr("settings.auto_discover", &[]),
    };
    let mut options = vec![auto.clone()];
    let chosen = snapshot.settings.chosen_serial.as_deref();
    if let Some(serial) = chosen
        && !snapshot.devices.iter().any(|d| d == serial)
    {
        options.push(PhoneOption {
            choice: PhoneChoice::Serial {
                serial: serial.to_string(),
            },
            label: tr("settings.remembered_offline", &[]),
        });
    }
    for serial in &snapshot.devices {
        options.push(PhoneOption {
            choice: PhoneChoice::Serial {
                serial: serial.clone(),
            },
            label: describe::device_label(snapshot, serial),
        });
    }
    let selected = chosen
        .and_then(|serial| {
            options
                .iter()
                .find(|o| matches!(&o.choice, PhoneChoice::Serial { serial: s } if s == serial))
        })
        .cloned()
        .unwrap_or(auto);
    (options, selected)
}

/// Changes of one control sent to the service and not answered yet, and the last refusal.
#[derive(Debug, Default)]
struct Saving {
    pending: u32,
    error: Option<String>,
}

#[derive(Debug)]
pub struct State {
    /// Starting at login as the autostart files say; changes only when the write is done.
    autostart: bool,
    /// A write of the autostart file is under way.
    autostart_busy: bool,
    autostart_error: Option<String>,
    copied: bool,
    /// The fix of the checks that was just copied.
    fix_copied: Option<String>,
    /// The doctor's checks and whether they are being run.
    doctor: Run,
    auto_connect: Saving,
    phone: Saving,
}

impl Default for State {
    fn default() -> Self {
        State::new(Ok(true))
    }
}

impl State {
    /// `autostart` is what reading the autostart file said at launch.
    pub fn new(autostart: Result<bool, String>) -> State {
        let mut state = State {
            autostart: true,
            autostart_busy: false,
            autostart_error: None,
            copied: false,
            fix_copied: None,
            doctor: Run::default(),
            auto_connect: Saving::default(),
            phone: Saving::default(),
        };
        state.autostart_done(autostart);
        state
    }

    /// Starting at login as last read from the autostart file.
    #[cfg(test)]
    pub fn autostart(&self) -> bool {
        self.autostart
    }

    fn autostart_done(&mut self, result: Result<bool, String>) {
        self.autostart_busy = false;
        match result {
            Ok(on) => {
                self.autostart = on;
                self.autostart_error = None;
            }
            Err(error) => {
                self.autostart_error = Some(tr("settings.autostart_failed", &[("error", &error)]));
            }
        }
    }

    fn saving(&mut self, field: SettingField) -> &mut Saving {
        match field {
            SettingField::AutoConnect => &mut self.auto_connect,
            SettingField::Phone => &mut self.phone,
        }
    }

    /// The control's last change is still on its way.
    pub fn is_saving(&self, field: SettingField) -> bool {
        match field {
            SettingField::AutoConnect => self.auto_connect.pending > 0,
            SettingField::Phone => self.phone.pending > 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    AutoConnect(bool),
    Phone(PhoneOption),
    LaunchAtLogin(bool),
    /// The autostart file was written (or not) and read back: what it says now.
    AutostartDone(Result<bool, String>),
    OpenOnPlug(bool),
    CopyProxy,
    /// "Copied" turns back into "Copy".
    CopiedEnd,
    RunChecks,
    /// The token of the ask it answers (`checks::Run`).
    Checks(u64, Result<Vec<Check>, String>),
    /// Copy a fix from the checks.
    CopyFix(String),
    /// The service took (or refused) a settings change.
    Saved(SettingField, Result<(), String>),
}

fn save(
    state: &mut State,
    field: SettingField,
    auto_connect: Option<bool>,
    phone: Option<PhoneChoice>,
) -> Effect {
    let saving = state.saving(field);
    saving.pending += 1;
    saving.error = None;
    Effect::Request(
        Request::SetSettings {
            auto_connect,
            phone,
        },
        Reply::Settings(field),
    )
}

pub fn update(state: &mut State, message: Msg, ctx: &Ctx) -> Effect {
    match message {
        Msg::AutoConnect(on) => save(state, SettingField::AutoConnect, Some(on), None),
        Msg::Phone(option) => save(state, SettingField::Phone, None, Some(option.choice)),
        // One write at a time: a second click waits for the file to be read back.
        Msg::LaunchAtLogin(_) if state.autostart_busy => Effect::None,
        Msg::LaunchAtLogin(on) => {
            state.autostart_busy = true;
            state.autostart_error = None;
            Effect::SetAutostart(on)
        }
        Msg::AutostartDone(result) => {
            state.autostart_done(result);
            Effect::None
        }
        Msg::OpenOnPlug(on) => Effect::Prefs(PrefChange::OpenOnPlug(on)),
        Msg::CopyProxy => {
            let Some(address) = ctx.snapshot.and_then(Snapshot::proxy_address) else {
                return Effect::None;
            };
            state.copied = true;
            Effect::Batch(vec![
                Effect::Copy(address),
                Effect::After(Duration::from_secs(2), AfterMsg::SettingsCopied),
            ])
        }
        Msg::CopiedEnd => {
            state.copied = false;
            state.fix_copied = None;
            Effect::None
        }
        Msg::RunChecks => state.doctor.start(DoctorFor::Settings),
        Msg::Checks(token, result) => {
            state.doctor.finish(token, result);
            Effect::None
        }
        Msg::CopyFix(fix) => {
            state.fix_copied = Some(fix.clone());
            Effect::Batch(vec![
                Effect::Copy(fix),
                Effect::After(Duration::from_secs(2), AfterMsg::SettingsCopied),
            ])
        }
        Msg::Saved(field, result) => {
            let saving = state.saving(field);
            saving.pending = saving.pending.saturating_sub(1);
            if let Err(reason) = result {
                saving.error = Some(reason);
            }
            Effect::None
        }
    }
}

/// The DNS line for the connection's mode, and its colour.
pub fn dns_line(mode: Option<DnsMode>) -> (String, Tone) {
    match mode {
        Some(DnsMode::Phone) => (tr("settings.dns_phone", &[]), Tone::Green),
        Some(DnsMode::Public) => (tr("settings.dns_public", &[]), Tone::Gray),
        Some(DnsMode::Unmanaged) => (tr("settings.dns_unmanaged", &[]), Tone::Orange),
        None => (tr("settings.dns_idle", &[]), Tone::Gray),
    }
}

fn switch<'a>(on: bool, message: impl Fn(bool) -> Msg + 'a) -> Element<'a, Msg> {
    toggler(on).on_toggle(message).into()
}

fn small<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(content).size(13)
}

fn error_line<'a>(error: &Option<String>) -> Option<Element<'a, Msg>> {
    error.as_ref().map(|reason| {
        text(reason.clone())
            .size(12)
            .color(Tone::Red.color())
            .into()
    })
}

/// How the auto-connect switch is drawn.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoConnectSwitch {
    /// No snapshot, so no value to show.
    Hidden,
    /// Showing the service's value while the last change is on its way.
    Busy(bool),
    On(bool),
}

pub fn auto_connect_switch(snapshot: Option<&Snapshot>, saving: bool) -> AutoConnectSwitch {
    match snapshot {
        None => AutoConnectSwitch::Hidden,
        Some(s) if saving => AutoConnectSwitch::Busy(s.settings.auto_connect),
        Some(s) => AutoConnectSwitch::On(s.settings.auto_connect),
    }
}

fn phone_card<'a>(state: &State, snapshot: Option<&Snapshot>) -> Element<'a, Msg> {
    let chooser: Element<'a, Msg> = match snapshot {
        Some(snapshot) => {
            let (options, selected) = phone_options(snapshot);
            if state.is_saving(SettingField::Phone) {
                // Not changeable while the last change is on its way.
                text(selected.label).size(13).into()
            } else {
                pick_list(options, Some(selected), Msg::Phone)
                    .text_size(13)
                    .into()
            }
        }
        None => text("").into(),
    };
    let mut body = column![
        kit::card_row(tr("settings.usb_device", &[]), None, chooser),
        kit::secondary(tr("settings.switch_note", &[])).size(11),
    ]
    .spacing(8);
    if let Some(error) = error_line(&state.phone.error) {
        body = body.push(error);
    }
    kit::card(None, body)
}

fn auto_connect_card<'a>(state: &State, snapshot: Option<&Snapshot>) -> Element<'a, Msg> {
    let control: Element<'a, Msg> =
        match auto_connect_switch(snapshot, state.is_saving(SettingField::AutoConnect)) {
            AutoConnectSwitch::Hidden => text("").into(),
            AutoConnectSwitch::Busy(on) => toggler(on).into(),
            AutoConnectSwitch::On(on) => switch(on, Msg::AutoConnect),
        };
    let mut body = column![kit::card_row(
        tr("settings.auto_connect", &[]),
        Some(tr("settings.auto_connect_note", &[])),
        control,
    )]
    .spacing(8);
    if let Some(error) = error_line(&state.auto_connect.error) {
        body = body.push(error);
    }
    kit::card(None, body)
}

fn startup_card<'a>(state: &State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let mut body = column![kit::card_row(
        tr("settings.launch_at_login", &[]),
        Some(tr("settings.launch_note", &[])),
        switch(state.autostart, Msg::LaunchAtLogin),
    ),]
    .spacing(10);
    if let Some(error) = &state.autostart_error {
        body = body.push(text(error.clone()).size(12).color(Tone::Red.color()));
    }
    body = body.push(rule::horizontal(1)).push(kit::card_row(
        tr("settings.open_on_plug", &[]),
        Some(tr("settings.open_on_plug_note", &[])),
        switch(ctx.prefs.open_on_plug, Msg::OpenOnPlug),
    ));
    kit::card(None, body)
}

fn advanced<'a>(state: &State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let address = ctx.snapshot.and_then(Snapshot::proxy_address);
    let shown = match &address {
        Some(address) => text(address.clone()).font(Font::MONOSPACE).size(12),
        None => kit::secondary(tr("settings.proxy_none", &[])),
    };
    let label = tr(
        if state.copied {
            "common.copied"
        } else {
            "common.copy"
        },
        &[],
    );
    let mut copy = button(kit::secondary(label)).padding([2, 8]);
    if address.is_some() {
        copy = copy.on_press(Msg::CopyProxy);
    }
    let field = container(shown)
        .padding([5, 8])
        .width(Length::Fill)
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme.palette().text.scale_alpha(0.06))),
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let proxy = column![
        kit::card_row::<Msg>(
            tr("settings.proxy", &[]),
            Some(tr("settings.proxy_note", &[])),
            column![],
        ),
        row![field, copy].spacing(8).align_y(Alignment::Center),
    ]
    .spacing(8);

    let (line, tone) = dns_line(ctx.dns());
    let checking = state.doctor.checking;
    let mut run = button(small(tr(
        if checking {
            "settings.checking"
        } else {
            "settings.run_checks"
        },
        &[],
    )))
    .padding([4, 12]);
    if !checking {
        run = run.on_press(Msg::RunChecks);
    }
    let mut dns = column![kit::card_row(
        tr("settings.dns", &[]),
        None,
        row![text(line).size(12).color(tone.color()), run]
            .spacing(12)
            .align_y(Alignment::Center),
    )]
    .spacing(8);
    if !checking
        && let Some(list) =
            checks::run_view(&state.doctor, state.fix_copied.as_deref(), Msg::CopyFix)
    {
        dns = dns.push(list);
    }
    column![
        text(tr("settings.advanced", &[]))
            .size(13)
            .font(kit::SEMIBOLD),
        kit::card(None, proxy),
        kit::card(None, dns),
    ]
    .spacing(8)
    .into()
}

pub fn view<'a>(state: &'a State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let body = column![
        phone_card(state, ctx.snapshot),
        auto_connect_card(state, ctx.snapshot),
        startup_card(state, ctx),
        advanced(state, ctx),
    ]
    .spacing(14);
    kit::page(tr("nav.settings", &[]), body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures::{SERIAL, connected_snapshot, disconnected_snapshot};
    use cg_i18n::Lang;
    use cg_ipc::phone::PhoneLabel;

    fn zh() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::ZhCn);
        guard
    }

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        guard
    }

    #[test]
    fn options_list_auto_remembered_offline_and_labelled_phones() {
        let _lang = zh();
        let mut snapshot = disconnected_snapshot();
        snapshot.devices = vec!["aaaa".into()];
        snapshot.settings.chosen_serial = Some("bbbb".into());
        snapshot.labels = [(
            "aaaa".to_string(),
            PhoneLabel {
                name: Some("Alice 的 iPhone".into()),
                model: Some("iPhone17,5".into()),
            },
        )]
        .into();
        let (options, selected) = phone_options(&snapshot);
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "自动发现 / 等待已选设备",
                "已记住的 iPhone（离线）",
                "Alice 的 iPhone · iPhone 16e"
            ]
        );
        assert_eq!(selected, options[1]);
        assert_eq!(
            options[1].choice,
            PhoneChoice::Serial {
                serial: "bbbb".into()
            }
        );
        assert_eq!(options[0].choice, PhoneChoice::Auto);
    }

    #[test]
    fn a_plugged_in_choice_is_selected_without_an_offline_entry() {
        let _lang = en();
        let mut snapshot = disconnected_snapshot();
        snapshot.settings.chosen_serial = Some(SERIAL.into());
        let (options, selected) = phone_options(&snapshot);
        assert_eq!(options.len(), 2);
        assert_eq!(selected, options[1]);
        snapshot.settings.chosen_serial = None;
        let (options, selected) = phone_options(&snapshot);
        assert_eq!(selected, options[0]);
    }

    #[test]
    fn toggles_send_one_setting_each() {
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let mut state = State::default();
        assert_eq!(
            update(&mut state, Msg::AutoConnect(false), &ctx),
            Effect::Request(
                Request::SetSettings {
                    auto_connect: Some(false),
                    phone: None
                },
                Reply::Settings(SettingField::AutoConnect)
            )
        );
        let auto = PhoneOption {
            choice: PhoneChoice::Auto,
            label: String::new(),
        };
        assert_eq!(
            update(&mut state, Msg::Phone(auto), &ctx),
            Effect::Request(
                Request::SetSettings {
                    auto_connect: None,
                    phone: Some(PhoneChoice::Auto)
                },
                Reply::Settings(SettingField::Phone)
            )
        );
        assert!(state.is_saving(SettingField::AutoConnect));
        assert!(state.is_saving(SettingField::Phone));
        assert_eq!(
            update(&mut state, Msg::OpenOnPlug(true), &ctx),
            Effect::Prefs(PrefChange::OpenOnPlug(true))
        );
        assert_eq!(
            update(&mut state, Msg::LaunchAtLogin(false), &ctx),
            Effect::SetAutostart(false)
        );
    }

    #[test]
    fn a_refused_change_ends_saving_beside_its_own_control() {
        use SettingField::{AutoConnect, Phone};
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let mut state = State::default();
        let _ = update(&mut state, Msg::AutoConnect(false), &ctx);
        let _ = update(&mut state, Msg::AutoConnect(true), &ctx);
        assert!(state.is_saving(AutoConnect) && !state.is_saving(Phone));
        let _ = update(&mut state, Msg::Saved(AutoConnect, Ok(())), &ctx);
        assert!(state.is_saving(AutoConnect));
        let _ = update(
            &mut state,
            Msg::Saved(AutoConnect, Err("服务已停止".into())),
            &ctx,
        );
        assert!(!state.is_saving(AutoConnect));
        assert_eq!(state.auto_connect.error.as_deref(), Some("服务已停止"));
        // The phone chooser has nothing to complain about.
        assert_eq!(state.phone.error, None);
        // The next change clears the old complaint.
        let _ = update(&mut state, Msg::AutoConnect(false), &ctx);
        assert_eq!(state.auto_connect.error, None);
        // A phone refusal lands on the phone chooser only, and frees it.
        let auto = PhoneOption {
            choice: PhoneChoice::Auto,
            label: String::new(),
        };
        let _ = update(&mut state, Msg::Phone(auto), &ctx);
        assert!(state.is_saving(Phone));
        let _ = update(&mut state, Msg::Saved(Phone, Err("no".into())), &ctx);
        assert!(!state.is_saving(Phone));
        assert_eq!(state.phone.error.as_deref(), Some("no"));
        assert_eq!(state.auto_connect.error, None);
        // A stray reply never underflows.
        let _ = update(&mut state, Msg::Saved(AutoConnect, Ok(())), &ctx);
        let _ = update(&mut state, Msg::Saved(AutoConnect, Ok(())), &ctx);
        assert!(!state.is_saving(AutoConnect));
        let _ = view(&state, &ctx);
    }

    #[test]
    fn auto_connect_has_no_value_without_a_snapshot() {
        assert_eq!(auto_connect_switch(None, false), AutoConnectSwitch::Hidden);
        assert_eq!(auto_connect_switch(None, true), AutoConnectSwitch::Hidden);
        let mut snapshot = disconnected_snapshot();
        snapshot.settings.auto_connect = false;
        assert_eq!(
            auto_connect_switch(Some(&snapshot), false),
            AutoConnectSwitch::On(false)
        );
        assert_eq!(
            auto_connect_switch(Some(&snapshot), true),
            AutoConnectSwitch::Busy(false)
        );
    }

    #[test]
    fn start_at_login_ignores_clicks_while_a_write_is_under_way() {
        let ctx = Ctx::for_test(None);
        let mut state = State::default();
        assert_eq!(
            update(&mut state, Msg::LaunchAtLogin(false), &ctx),
            Effect::SetAutostart(false)
        );
        assert_eq!(
            update(&mut state, Msg::LaunchAtLogin(true), &ctx),
            Effect::None
        );
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::AutostartDone(Err("x".into())), &ctx);
        assert_eq!(
            update(&mut state, Msg::LaunchAtLogin(true), &ctx),
            Effect::SetAutostart(true)
        );
    }

    #[test]
    fn a_copied_fix_shows_as_copied_until_the_timer() {
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let mut state = State::default();
        assert_eq!(
            update(&mut state, Msg::CopyFix("sudo x".into()), &ctx),
            Effect::Batch(vec![
                Effect::Copy("sudo x".into()),
                Effect::After(Duration::from_secs(2), AfterMsg::SettingsCopied),
            ])
        );
        assert_eq!(state.fix_copied.as_deref(), Some("sudo x"));
        let _ = update(&mut state, Msg::CopiedEnd, &ctx);
        assert_eq!(state.fix_copied, None);
    }

    #[test]
    fn the_autostart_switch_follows_the_file_not_the_click() {
        let _lang = en();
        let ctx = Ctx::for_test(None);
        let mut state = State::default();
        assert!(state.autostart);
        let _ = update(&mut state, Msg::LaunchAtLogin(false), &ctx);
        // Nothing changes until the write is done.
        assert!(state.autostart);
        let _ = update(&mut state, Msg::AutostartDone(Ok(false)), &ctx);
        assert!(!state.autostart && state.autostart_error.is_none());
        let _ = update(&mut state, Msg::AutostartDone(Err("denied".into())), &ctx);
        assert!(!state.autostart);
        assert_eq!(
            state.autostart_error.as_deref(),
            Some("Couldn't change start at login: denied")
        );
        let _ = update(&mut state, Msg::LaunchAtLogin(true), &ctx);
        assert_eq!(state.autostart_error, None);
        let launch = State::new(Err("x".into()));
        assert!(launch.autostart_error.is_some());
    }

    #[test]
    fn copy_needs_an_address() {
        let none = Ctx::for_test(Some(disconnected_snapshot()));
        let mut state = State::default();
        assert_eq!(update(&mut state, Msg::CopyProxy, &none), Effect::None);
        assert!(!state.copied);

        let mut snapshot = connected_snapshot();
        snapshot.socks_port = Some(41234);
        let ctx = Ctx::for_test(Some(snapshot));
        assert_eq!(
            update(&mut state, Msg::CopyProxy, &ctx),
            Effect::Batch(vec![
                Effect::Copy("socks5h://127.0.0.1:41234".into()),
                Effect::After(Duration::from_secs(2), AfterMsg::SettingsCopied),
            ])
        );
        assert!(state.copied);
        let _ = update(&mut state, Msg::CopiedEnd, &ctx);
        assert!(!state.copied);
    }

    #[test]
    fn checks_run_once_and_a_stale_answer_is_dropped() {
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let mut state = State::default();
        assert_eq!(
            update(&mut state, Msg::RunChecks, &ctx),
            Effect::Request(Request::Doctor, Reply::Doctor(DoctorFor::Settings, 1))
        );
        assert_eq!(update(&mut state, Msg::RunChecks, &ctx), Effect::None);
        let _ = update(&mut state, Msg::Checks(0, Err("old".into())), &ctx);
        assert!(state.doctor.checking);
        let _ = update(&mut state, Msg::Checks(1, Err("down".into())), &ctx);
        assert!(!state.doctor.checking);
        assert_eq!(state.doctor.checks, Some(Err("down".into())));
    }

    #[test]
    fn dns_line_follows_the_mode() {
        let _lang = en();
        assert_eq!(dns_line(Some(DnsMode::Unmanaged)).1, Tone::Orange);
        assert_eq!(dns_line(Some(DnsMode::Phone)).1, Tone::Green);
        assert_ne!(
            dns_line(Some(DnsMode::Phone)).0,
            dns_line(Some(DnsMode::Public)).0
        );
        assert_ne!(dns_line(None).0, dns_line(Some(DnsMode::Public)).0);
    }

    #[test]
    fn the_view_builds_in_every_state() {
        let _lang = en();
        let mut state = State::default();
        let _ = view(&state, &Ctx::for_test(None));
        let mut unmanaged = connected_snapshot();
        unmanaged.state = cg_ipc::ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Unmanaged,
        };
        unmanaged.settings.chosen_serial = Some("gone".into());
        let ctx = Ctx::for_test(Some(unmanaged));
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::RunChecks, &ctx);
        let _ = view(&state, &ctx);
        let _ = update(
            &mut state,
            Msg::Checks(1, Ok(vec![Check::new(cg_ipc::CheckId::Tun, false)])),
            &ctx,
        );
        let _ = view(&state, &ctx);
        let _ = update(
            &mut state,
            Msg::Saved(SettingField::Phone, Err("x".into())),
            &ctx,
        );
        let _ = update(&mut state, Msg::AutostartDone(Err("x".into())), &ctx);
        let _ = update(&mut state, Msg::CopyProxy, &ctx);
        let _ = view(&state, &ctx);
        let _ = view(&state, &Ctx::for_test(Some(disconnected_snapshot())));
    }
}
