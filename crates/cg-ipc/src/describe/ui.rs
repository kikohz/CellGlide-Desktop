//! What the window shows, decided without a toolkit: the sidebar and its dots, the tray's look
//! and menu, the main button, the pages' lines and colours, and the shapes the canvases draw.
//! The Linux window (cg-gui) and the Windows DLL (cg-ffi) both draw from these, so the two read
//! the same. Every sentence is translated here; the colours are the Mac's system colours by
//! name, and each toolkit maps them to its own.

use crate::describe::{self, CardText, EventTone};
use crate::health::{HealthEvent, HistoryDay, Span, SpanKind, day_bounds, day_spans, hour_marks};
use crate::phone::{PhoneCard, SpeedPoint};
use crate::{Check, CheckId, ConnState, Contact, DnsMode, PhoneChoice, Snapshot};
use cg_i18n::tr;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

/// The Mac's system colours, so dots and badges read the same on every platform.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Green,
    Orange,
    Red,
    Blue,
    Purple,
    Gray,
}

// The main window's sidebar: sections in the Mac's order, their groups, and the dots beside them.

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    #[default]
    Overview,
    SpeedTest,
    DnsLeak,
    History,
    Diagnostics,
    Settings,
    About,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Overview,
        Section::SpeedTest,
        Section::DnsLeak,
        Section::History,
        Section::Diagnostics,
        Section::Settings,
        Section::About,
    ];

    pub fn title_key(self) -> &'static str {
        match self {
            Section::Overview => "nav.overview",
            Section::SpeedTest => "nav.speed_test",
            Section::DnsLeak => "nav.dns_leak",
            Section::History => "nav.history",
            Section::Diagnostics => "nav.diagnostics",
            Section::Settings => "nav.settings",
            Section::About => "nav.about",
        }
    }
}

/// The sidebar's groups, each with an optional heading key, as on the Mac (no map or split).
pub const GROUPS: [(Option<&str>, &[Section]); 4] = [
    (None, &[Section::Overview]),
    (Some("nav.tools"), &[Section::SpeedTest, Section::DnsLeak]),
    (
        Some("nav.activity"),
        &[Section::History, Section::Diagnostics],
    ),
    (Some("nav.app"), &[Section::Settings, Section::About]),
];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Badge {
    None,
    Warning,
    Critical,
}

/// The dot beside a section, after the Mac's `MainSectionBadge`. Overview: an open issue (red
/// when severe), else a connection waiting on the user; starting up is transient and not marked.
/// History: an open issue, same colour. Settings: the user can't talk to the service for lack
/// of the group, or DNS couldn't be set (the Mac's missing background permission). Nothing else.
pub fn badge(
    section: Section,
    snapshot: Option<&Snapshot>,
    service: Option<&ServiceDown>,
) -> Badge {
    let issue = snapshot.and_then(|s| s.health.cause).map(|cause| {
        if cause.is_severe() {
            Badge::Critical
        } else {
            Badge::Warning
        }
    });
    match section {
        Section::Overview => issue.unwrap_or_else(|| match snapshot.map(|s| &s.state) {
            Some(ConnState::Retrying { .. } | ConnState::ChoosePhone) => Badge::Warning,
            _ => Badge::None,
        }),
        Section::History => issue.unwrap_or(Badge::None),
        Section::Settings => {
            let outside = service.is_some_and(|s| s.not_in_group);
            let unmanaged = snapshot.is_some_and(|s| {
                matches!(
                    s.state,
                    ConnState::Connected {
                        dns: DnsMode::Unmanaged,
                        ..
                    }
                )
            });
            if outside || unmanaged {
                Badge::Warning
            } else {
                Badge::None
            }
        }
        Section::SpeedTest | Section::DnsLeak | Section::Diagnostics | Section::About => {
            Badge::None
        }
    }
}

// The tray: its look for a snapshot, and its menu.

/// Why the app has no snapshots: the text to show, and whether it is because the user isn't in
/// the `cellglide` group (Settings then shows a dot).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceDown {
    pub text: String,
    pub not_in_group: bool,
    /// What `text` says, kept so it can be said again in another language and so a retry that
    /// fails the same way (with other OS detail) reads as the same reason.
    #[serde(skip)]
    pub reason: DownReason,
}

/// Why there are no snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DownReason {
    /// The subscription ended: the service stopped.
    #[default]
    Gone,
    /// The user isn't in the `cellglide` group.
    NotInGroup,
    /// The service speaks another protocol version.
    VersionMismatch { service: String },
    /// It couldn't be reached; the doctor's service check says why.
    Unreachable { check: Check },
    /// It took the connection but didn't answer within `client::TIMEOUT`.
    Timeout,
    /// What follows the service failed on our side (`detail` is the developer's words); it
    /// starts over by itself.
    Internal { detail: String },
}

impl DownReason {
    /// The sentence, in the current language.
    pub fn text(&self) -> String {
        match self {
            DownReason::Gone => tr("gui.service_gone", &[]),
            DownReason::NotInGroup => describe::checks(&[Check::new(CheckId::Group, false)])
                .trim_end()
                .into(),
            DownReason::VersionMismatch { service } => describe::version_mismatch(service),
            DownReason::Unreachable { check } => describe::checks(std::slice::from_ref(check))
                .trim_end()
                .into(),
            DownReason::Timeout => tr("gui.service_timeout", &[]),
            DownReason::Internal { detail } => tr("reason.internal", &[("detail", detail)]),
        }
    }

    /// The same reason, whatever the OS said in detail (`error`).
    pub fn same_kind(&self, other: &DownReason) -> bool {
        match (self, other) {
            (DownReason::Unreachable { check: a }, DownReason::Unreachable { check: b }) => {
                a.id == b.id && a.ok == b.ok && a.args.get("cause") == b.args.get("cause")
            }
            (a, b) => a == b,
        }
    }
}

impl ServiceDown {
    pub fn new(reason: DownReason) -> ServiceDown {
        ServiceDown {
            text: reason.text(),
            not_in_group: reason == DownReason::NotInGroup,
            reason,
        }
    }

    /// The same reason, said in the current language.
    pub fn retranslated(&self) -> ServiceDown {
        let reason = match &self.reason {
            DownReason::Unreachable { check } => DownReason::Unreachable {
                check: describe::retranslated_check(check),
            },
            reason => reason.clone(),
        };
        ServiceDown::new(reason)
    }

    /// Says the same as `other`, OS detail aside: telling the user again adds nothing.
    pub fn same_reason(&self, other: &ServiceDown) -> bool {
        self.not_in_group == other.not_in_group && self.reason.same_kind(&other.reason)
    }
}

/// What the tray shows for a snapshot: the icon's two bits, the wish for the menu, and the tooltip text.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TrayLook {
    /// Routing through the phone.
    pub active: bool,
    /// An issue is open (the orange dot).
    pub alert: bool,
    /// A connection is wanted, so the menu offers to disconnect.
    pub wants: bool,
    pub title: String,
}

pub fn tray_look(snapshot: Option<&Snapshot>) -> TrayLook {
    TrayLook {
        active: is_connected(snapshot),
        alert: snapshot.is_some_and(|s| s.health.cause.is_some()),
        wants: snapshot.is_some_and(|s| s.wants_connection),
        title: snapshot.map_or_else(
            || tr("gui.waiting_service", &[]),
            |s| describe::gui_state(s).title,
        ),
    }
}

/// What the tray shows while there are no snapshots: nothing connected, and why.
pub fn down_look(down: &ServiceDown) -> TrayLook {
    TrayLook {
        title: down.text.clone(),
        ..tray_look(None)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MenuEntry {
    Version,
    Separator,
    Toggle { disconnect: bool },
    Open,
    Quit,
}

/// The right-click menu in order (spec 4.2) with each entry's enabled state.
pub fn menu_layout(look: &TrayLook) -> Vec<(MenuEntry, bool)> {
    vec![
        (MenuEntry::Version, false),
        (MenuEntry::Separator, true),
        (
            MenuEntry::Toggle {
                disconnect: look.wants,
            },
            true,
        ),
        (MenuEntry::Open, true),
        (MenuEntry::Separator, true),
        (MenuEntry::Quit, true),
    ]
}

// The app: its version, the main button, and noticing an iPhone being plugged in.

/// What the sidebar footer and the tray tooltip say; the workspace version, so it cannot drift.
pub fn version_line() -> String {
    format!("CellGlide {}", env!("CARGO_PKG_VERSION"))
}

/// The overview's and the panel's main button, after what the user asked for rather than the
/// state reached: "disconnect and go back" while a connection is wanted, else "connect".
pub fn button_label(snapshot: Option<&Snapshot>) -> String {
    let wants = snapshot.is_some_and(|s| s.wants_connection);
    tr(
        if wants {
            "gui.disconnect_long"
        } else {
            "gui.connect"
        },
        &[],
    )
}

pub fn is_connected(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|s| matches!(s.state, ConnState::Connected { .. }))
}

/// The main window's title, which says so while connected: the Linux window and the Windows
/// app (`ChromeView.window_title`) both use it.
pub fn window_title(snapshot: Option<&Snapshot>) -> String {
    let key = if is_connected(snapshot) {
        "gui.title_connected"
    } else {
        "gui.title"
    };
    tr(key, &[])
}

/// How long an iPhone must have been missing before its return counts as plugging it in.
/// usbmuxd can lose a phone for a moment and find it again: after resume it came back 26 s
/// later once resume-check restarted usbmuxd (validation l), and a relock can empty the list
/// briefly. `Instant` doesn't advance while suspended, so sleeping doesn't count as missing.
/// The cost: unplugging and replugging within a minute doesn't open the window.
pub const REPLUG_AFTER: Duration = Duration::from_secs(60);

/// The iPhones seen in the last snapshot, and when each that has gone since went missing.
#[derive(Clone, Debug, Default)]
pub struct KnownDevices {
    present: BTreeSet<String>,
    missing: BTreeMap<String, Instant>,
}

impl KnownDevices {
    /// What the first snapshot shows: already there, so not plugged in.
    fn new(devices: &[String]) -> KnownDevices {
        KnownDevices {
            present: devices.iter().cloned().collect(),
            missing: Default::default(),
        }
    }

    /// Takes the next snapshot's iPhones; true when one was plugged in: one that wasn't in the
    /// last snapshot and was never seen or has been missing for at least [`REPLUG_AFTER`].
    pub fn update(&mut self, devices: &[String], now: Instant) -> bool {
        self.plugged(devices, now).is_some()
    }

    /// [`KnownDevices::update`], naming the first iPhone (in `devices` order) that was plugged
    /// in.
    pub fn plugged(&mut self, devices: &[String], now: Instant) -> Option<String> {
        for gone in self.present.iter().filter(|s| !devices.contains(s)) {
            self.missing.entry(gone.clone()).or_insert(now);
        }
        self.missing
            .retain(|_, since| now.saturating_duration_since(*since) < REPLUG_AFTER);
        let mut plugged = None;
        for serial in devices {
            // Every returning phone leaves `missing`, even after the first new one is found.
            if !self.present.contains(serial)
                && self.missing.remove(serial).is_none()
                && plugged.is_none()
            {
                plugged = Some(serial.clone());
            }
        }
        self.present = devices.iter().cloned().collect();
        plugged
    }
}

/// Whether the snapshot's `devices` has an iPhone that was plugged in since the last call,
/// updating `known`; [`plugged_serial`] says which one. With no list yet (`known` is `None`) the
/// list starts from `devices` and nothing was plugged in. Whether a service restart starts the
/// list over is the caller's choice: the Linux window sets `known` back to `None`, the Windows
/// DLL keeps it, so the iPhones it saw before aren't plugged in again.
pub fn plugged_in(known: &mut Option<KnownDevices>, devices: &[String], now: Instant) -> bool {
    plugged_serial(known, devices, now).is_some()
}

/// [`plugged_in`], naming the iPhone that was plugged in (the first in `devices` order).
pub fn plugged_serial(
    known: &mut Option<KnownDevices>,
    devices: &[String],
    now: Instant,
) -> Option<String> {
    match known {
        Some(known) => known.plugged(devices, now),
        None => {
            *known = Some(KnownDevices::new(devices));
            None
        }
    }
}

// The panel's and the overview's state dot and health line.

/// Green only while the computer really uses the phone; orange while it waits on the user.
pub fn status_tone(snapshot: Option<&Snapshot>) -> Tone {
    match snapshot.map(|s| &s.state) {
        Some(ConnState::Connected { .. }) => Tone::Green,
        Some(ConnState::Retrying { .. } | ConnState::ChoosePhone) => Tone::Orange,
        _ => Tone::Gray,
    }
}

/// The health line the panel and the overview show: only while connected, as the Mac's
/// `PanelView` and `OverviewPage` show their health row only while the tunnel runs. (The CLI's
/// `status` also shows an open issue or a recent drop when not connected.)
pub fn connected_health_line(snapshot: &Snapshot, now: Timestamp, tz: &TimeZone) -> Option<String> {
    if matches!(snapshot.state, ConnState::Connected { .. }) {
        describe::status_health_line(snapshot, now, tz)
    } else {
        None
    }
}

/// The colour of the health dot: the issue's, or green when there is none.
pub fn health_tone(snapshot: &Snapshot) -> Tone {
    match snapshot.health.cause {
        Some(cause) if cause.is_severe() => Tone::Red,
        Some(_) => Tone::Orange,
        None => Tone::Green,
    }
}

// The overview page.

/// The USB device row's subtitle: the chosen phone, or that any phone will do.
pub fn device_subtitle(snapshot: Option<&Snapshot>) -> String {
    match snapshot.and_then(|s| s.selected.as_deref().map(|serial| (s, serial))) {
        Some((snapshot, serial)) => describe::device_label(snapshot, serial),
        None => tr("overview.auto_device", &[]),
    }
}

// The settings page.

/// One entry of the phone chooser.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
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

/// The DNS line for the connection's mode, and its colour.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DnsLine {
    pub text: String,
    pub tone: Tone,
}

pub fn dns_line(mode: Option<DnsMode>) -> DnsLine {
    let (key, tone) = match mode {
        Some(DnsMode::Phone) => ("settings.dns_phone", Tone::Green),
        Some(DnsMode::Public) => ("settings.dns_public", Tone::Gray),
        Some(DnsMode::Unmanaged) => ("settings.dns_unmanaged", Tone::Orange),
        None => ("settings.dns_idle", Tone::Gray),
    };
    DnsLine {
        text: tr(key, &[]),
        tone,
    }
}

/// How the auto-connect switch is drawn.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

// The history page.

/// The day `step` days from `current` among the days on record; `None` past either end.
pub fn neighbor(days: &[String], current: &str, step: i8) -> Option<String> {
    let at = days.iter().position(|d| d == current)?;
    let to = at.checked_add_signed(isize::from(step))?;
    days.get(to).cloned()
}

/// A day's spans between its first instant and the next day's.
#[derive(Clone, Debug, PartialEq)]
pub struct DayStrip {
    pub spans: Vec<Span>,
    pub begin: Timestamp,
    pub next: Timestamp,
}

/// The strip of a day: its spans between local midnights, up to now.
pub fn day_strip(day: &HistoryDay, now: Timestamp, tz: &TimeZone) -> DayStrip {
    let (begin, next) = day
        .day
        .parse::<Date>()
        .ok()
        .and_then(|date| day_bounds(date, tz))
        .unwrap_or_else(|| {
            let next = day
                .begin
                .checked_add(jiff::SignedDuration::from_hours(24))
                .unwrap_or(day.end);
            (day.begin, next)
        });
    DayStrip {
        spans: day_spans(&day.events, day.start, day.begin, day.end.min(now)),
        begin,
        next,
    }
}

/// The space between the five hour labels, in thousandths of the strip, so the labels sit under
/// the hours even on a 23- or 25-hour day.
pub fn tick_gaps(day: &Date, tz: &TimeZone) -> Vec<u16> {
    let marks = hour_marks(*day, tz);
    marks
        .windows(2)
        .map(|pair| ((pair[1].1 - pair[0].1) * 1000.0).round().max(1.0) as u16)
        .collect()
}

/// An hour label under the strip: "06:00".
pub fn tick_label(hour: u8) -> String {
    format!("{hour:02}:00")
}

fn event_tone(tone: EventTone) -> Tone {
    match tone {
        EventTone::Green => Tone::Green,
        EventTone::Orange => Tone::Orange,
        EventTone::Red => Tone::Red,
        EventTone::Gray => Tone::Gray,
    }
}

/// One row of the day's events.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    /// Local time, "03:05".
    pub time: String,
    /// The event; an end already carries its duration.
    pub title: String,
    pub tone: Tone,
}

pub fn row_texts(event: &HealthEvent, tz: &TimeZone) -> EventRow {
    EventRow {
        time: describe::clock(event.t, tz),
        title: describe::event_title(event),
        tone: event_tone(describe::event_tone(event)),
    }
}

/// One entry of the legend beside the strip.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LegendItem {
    pub label: String,
    pub tone: Tone,
}

pub fn legend() -> Vec<LegendItem> {
    [
        ("history.legend.ok", Tone::Green),
        ("history.legend.unstable", Tone::Orange),
        ("history.legend.down", Tone::Red),
        ("history.legend.not_sharing", Tone::Gray),
    ]
    .into_iter()
    .map(|(key, tone)| LegendItem {
        label: tr(key, &[]),
        tone,
    })
    .collect()
}

/// One painted rectangle of the 24-hour strip, in pixels from the left.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct StripCell {
    pub x: f32,
    pub width: f32,
    pub tone: Tone,
    pub alpha: f32,
}

/// The cells to paint, in order. `day_spans` already puts sharing before issues, so the issues
/// land on top. Every cell is at least 1 px wide and stays inside `[0, width]`.
pub fn strip_cells(
    spans: &[Span],
    begin: Timestamp,
    next: Timestamp,
    width: f32,
) -> Vec<StripCell> {
    spans
        .iter()
        .map(|span| {
            let from = crate::health::strip_fraction(span.from, begin, next) as f32 * width;
            let to = crate::health::strip_fraction(span.to, begin, next) as f32 * width;
            let cell_width = (to - from).max(1.0).min(width.max(1.0));
            let (tone, alpha) = match span.kind {
                SpanKind::Sharing => (Tone::Green, 0.6),
                SpanKind::Issue(cause) if cause.is_severe() => (Tone::Red, 1.0),
                SpanKind::Issue(_) => (Tone::Orange, 1.0),
            };
            StripCell {
                x: from.min((width - cell_width).max(0.0)),
                width: cell_width,
                tone,
                alpha,
            }
        })
        .collect()
}

// The doctor's checks.

/// How a check came out. `Unknown` is a check that couldn't be made (the service couldn't read
/// what it needed): neither a pass nor a plain failure, so it is drawn as a state of its own.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass,
    Fail,
    Unknown,
}

pub fn check_state(check: &Check) -> CheckState {
    if check.ok {
        CheckState::Pass
    } else if describe::check_unchecked(check) {
        CheckState::Unknown
    } else {
        CheckState::Fail
    }
}

/// One check, ready to draw.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CheckRow {
    pub state: CheckState,
    pub line: String,
    /// The bare fix (for copying), when the check didn't pass (failed or couldn't be made).
    pub fix: Option<String>,
    /// This fix is the one just copied: its button says so.
    pub copied: bool,
}

pub fn check_rows(checks: &[Check], copied: Option<&str>) -> Vec<CheckRow> {
    checks
        .iter()
        .map(|check| {
            let (line, fix) = describe::check_lines(check);
            CheckRow {
                copied: fix.as_deref().is_some() && fix.as_deref() == copied,
                state: check_state(check),
                line,
                fix,
            }
        })
        .collect()
}

// The phone card, its speed trail and the speed-test curve.

/// What the speed line of the card shows.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Speeds {
    /// No frame from the phone app: no speed line at all.
    Hidden,
    NotShared,
    Shared,
}

pub fn speeds(card: &PhoneCard, text: &CardText) -> Speeds {
    match (&card.frame, text.sharing) {
        (None, _) => Speeds::Hidden,
        (Some(_), Some(true)) => Speeds::Shared,
        (Some(_), _) => Speeds::NotShared,
    }
}

/// One drawn point: `x` is how far along the minute (0–1), `down` and `up` are heights
/// relative to the peak of the whole trail (0–1).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TrailPoint {
    pub x: f32,
    pub down: f32,
    pub up: f32,
}

/// The trail as runs of consecutive readings, right-aligned in a window of `len` seconds so a
/// short history grows in from the right. A missing second ends the run, never drops to zero.
pub fn trail_runs(points: &[Option<SpeedPoint>], len: usize) -> Vec<Vec<TrailPoint>> {
    // Only the newest `len` seconds fit.
    let points = &points[points.len().saturating_sub(len)..];
    let offset = len - points.len();
    let span = len.saturating_sub(1).max(1) as f32;
    let peak = points
        .iter()
        .flatten()
        .flat_map(|p| [p.download, p.upload])
        .fold(1.0_f64, f64::max);
    let relative = |value: f64| (value / peak).clamp(0.0, 1.0) as f32;
    let mut out: Vec<Vec<TrailPoint>> = Vec::new();
    let mut open = false;
    for (index, point) in points.iter().enumerate() {
        let Some(point) = point else {
            open = false;
            continue;
        };
        if !open {
            out.push(Vec::new());
            open = true;
        }
        if let Some(run) = out.last_mut() {
            run.push(TrailPoint {
                x: (index + offset) as f32 / span,
                down: relative(point.download),
                up: relative(point.upload),
            });
        }
    }
    out
}

/// One point of the speed-test curve: `x` across the series and `y` the height, both 0–1.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

/// Where the curve goes, from 0 to 1 across the series, scaled to this series' peak (at least
/// 1 so an all-zero series stays flat). Fewer than two points is no curve.
pub fn curve_points(series: &[f64]) -> Vec<CurvePoint> {
    if series.len() < 2 {
        return Vec::new();
    }
    let peak = series.iter().copied().fold(1.0_f64, f64::max);
    let last = (series.len() - 1) as f32;
    series
        .iter()
        .enumerate()
        .map(|(i, v)| CurvePoint {
            x: i as f32 / last,
            y: (v / peak).clamp(0.0, 1.0) as f32,
        })
        .collect()
}

// The diagnostics page.

/// The email and note as typed, trimmed; `None` when both are empty.
pub fn contact(email: &str, note: &str) -> Option<Contact> {
    let some = |s: &str| {
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    let (email, note) = (some(email), some(note));
    (email.is_some() || note.is_some()).then_some(Contact { email, note })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Reason;
    use crate::health::{DayStart, EventKind, EventPhase, HealthStatus, IssueCause, LastIssue};
    use crate::phone::{PhoneFrame, PhoneLabel, UsbPhone};
    use jiff::SignedDuration;

    const SERIAL: &str = "00008140-000000000000002C";

    #[test]
    fn a_down_reason_is_said_again_in_the_new_language_and_os_detail_is_not_a_new_reason() {
        let _lock = cg_i18n::lock_for_tests();
        let unreachable = |error: &str| {
            ServiceDown::new(DownReason::Unreachable {
                check: Check::new(CheckId::Service, false).with("error", error),
            })
        };
        cg_i18n::set(cg_i18n::Lang::En);
        let busy = unreachable("All pipe instances are busy.");
        let missing = unreachable("The system cannot find the file specified.");
        assert_ne!(busy.text, missing.text);
        assert!(busy.same_reason(&missing));
        let refused = ServiceDown::new(DownReason::Unreachable {
            check: Check::new(CheckId::Service, false)
                .with("error", "x")
                .with("cause", "not_allowed"),
        });
        assert!(!busy.same_reason(&refused));
        let gone = ServiceDown::new(DownReason::Gone);
        assert!(!gone.same_reason(&busy));
        assert!(!gone.same_reason(&ServiceDown::new(DownReason::NotInGroup)));
        assert_eq!(gone.text, cg_i18n::tr("gui.service_gone", &[]));
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let again = gone.retranslated();
        assert_eq!(again.text, cg_i18n::tr("gui.service_gone", &[]));
        assert_ne!(again.text, gone.text);
        assert!(again.same_reason(&gone));
        assert!(ServiceDown::new(DownReason::NotInGroup).not_in_group);
        cg_i18n::set(cg_i18n::Lang::En);
    }

    #[test]
    fn our_own_fragment_is_said_again_in_the_new_language() {
        let _lock = cg_i18n::lock_for_tests();
        for (cause, key) in [
            ("not_allowed", "reason.not_allowed"),
            ("impostor", "check.service.impostor"),
        ] {
            cg_i18n::set(cg_i18n::Lang::En);
            let english = cg_i18n::tr(key, &[]);
            let down = ServiceDown::new(DownReason::Unreachable {
                check: Check::new(CheckId::Service, false)
                    .with("error", english.clone())
                    .with("cause", cause),
            });
            assert!(down.text.contains(&english), "{}", down.text);
            cg_i18n::set(cg_i18n::Lang::ZhCn);
            let chinese = cg_i18n::tr(key, &[]);
            let again = down.retranslated();
            assert!(again.text.contains(&chinese), "{}", again.text);
            assert!(!again.text.contains(&english), "{}", again.text);
            assert!(again.same_reason(&down));
        }
        // The OS's own words are kept as they were.
        let os = ServiceDown::new(DownReason::Unreachable {
            check: Check::new(CheckId::Service, false).with("error", "Access is denied."),
        });
        assert!(os.retranslated().text.contains("Access is denied."));
        cg_i18n::set(cg_i18n::Lang::En);
    }

    /// Connected through one iPhone that reports a frame: "phone connected" for the pages.
    fn connected_snapshot() -> Snapshot {
        let mut snapshot = disconnected_snapshot();
        snapshot.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Phone,
        };
        snapshot.wants_connection = true;
        snapshot.phone = Some(PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("中国移动".into()),
                radio: Some("5G".into()),
                sharing: Some(true),
                download: Some(2_500_000.0),
                upload: Some(400_000.0),
                quality_score: Some(90),
                latency: Some(38.0),
                ..PhoneFrame::default()
            }),
            ..PhoneCard::default()
        });
        snapshot.socks_port = Some(1080);
        snapshot
    }

    /// The same iPhone plugged in, nobody asked to connect.
    fn disconnected_snapshot() -> Snapshot {
        Snapshot {
            state: ConnState::Idle,
            devices: vec![SERIAL.into()],
            selected: Some(SERIAL.into()),
            labels: [(
                SERIAL.to_string(),
                PhoneLabel {
                    name: Some("Test iPhone".into()),
                    model: Some("iPhone17,5".into()),
                },
            )]
            .into(),
            ..Snapshot::default()
        }
    }

    fn zh() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        guard
    }

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    #[test]
    fn view_types_round_trip_as_json() {
        let row = CheckRow {
            state: CheckState::Unknown,
            line: "x".into(),
            fix: Some("y".into()),
            copied: false,
        };
        let text = serde_json::to_string(&row).unwrap();
        assert_eq!(
            text,
            r#"{"state":"unknown","line":"x","fix":"y","copied":false}"#
        );
        assert_eq!(serde_json::from_str::<CheckRow>(&text).unwrap(), row);
        assert_eq!(serde_json::to_string(&Tone::Orange).unwrap(), r#""orange""#);
        assert_eq!(
            serde_json::to_string(&Badge::Critical).unwrap(),
            r#""critical""#
        );
    }

    #[test]
    fn legend_has_the_four_tones_in_order() {
        let tones: Vec<Tone> = legend().into_iter().map(|i| i.tone).collect();
        assert_eq!(tones, [Tone::Green, Tone::Orange, Tone::Red, Tone::Gray]);
    }

    // From cg-gui's section.rs.

    #[test]
    fn sidebar_order_and_groups() {
        let flat: Vec<Section> = GROUPS.iter().flat_map(|(_, s)| s.iter().copied()).collect();
        assert_eq!(flat, Section::ALL.to_vec());
    }

    fn with(state: ConnState, cause: Option<IssueCause>) -> Snapshot {
        Snapshot {
            state,
            health: HealthStatus {
                cause,
                ..HealthStatus::default()
            },
            ..Snapshot::default()
        }
    }

    fn connected(dns: DnsMode) -> ConnState {
        ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns,
        }
    }

    #[test]
    fn badges_follow_the_mac_rules() {
        let severe = with(connected(DnsMode::Phone), Some(IssueCause::PhoneNetwork));
        let mild = with(connected(DnsMode::Phone), Some(IssueCause::PhoneUnstable));
        let retrying = with(
            ConnState::Retrying {
                reason: Reason::NoExit,
            },
            None,
        );
        let choose = with(ConnState::ChoosePhone, None);
        let connecting = with(ConnState::Connecting, None);
        let unmanaged = with(connected(DnsMode::Unmanaged), None);
        let fine = with(connected(DnsMode::Phone), None);
        let outside = ServiceDown::new(DownReason::NotInGroup);
        let stopped = ServiceDown::new(DownReason::Gone);
        assert!(outside.not_in_group && !stopped.not_in_group);

        assert_eq!(
            badge(Section::Overview, Some(&severe), None),
            Badge::Critical
        );
        assert_eq!(
            badge(Section::History, Some(&severe), None),
            Badge::Critical
        );
        assert_eq!(badge(Section::Overview, Some(&mild), None), Badge::Warning);
        assert_eq!(badge(Section::History, Some(&mild), None), Badge::Warning);
        assert_eq!(
            badge(Section::Overview, Some(&retrying), None),
            Badge::Warning
        );
        assert_eq!(badge(Section::History, Some(&retrying), None), Badge::None);
        assert_eq!(
            badge(Section::Overview, Some(&choose), None),
            Badge::Warning
        );
        assert_eq!(
            badge(Section::Overview, Some(&connecting), None),
            Badge::None
        );
        assert_eq!(
            badge(Section::History, Some(&connecting), None),
            Badge::None
        );
        assert_eq!(badge(Section::Overview, Some(&fine), None), Badge::None);
        assert_eq!(
            badge(Section::Settings, Some(&unmanaged), None),
            Badge::Warning
        );
        assert_eq!(badge(Section::Settings, Some(&fine), None), Badge::None);
        assert_eq!(
            badge(Section::Settings, None, Some(&outside)),
            Badge::Warning
        );
        assert_eq!(badge(Section::Settings, None, Some(&stopped)), Badge::None);
        assert_eq!(badge(Section::Overview, None, Some(&stopped)), Badge::None);

        let cases = [
            (Some(&severe), None),
            (Some(&mild), None),
            (Some(&retrying), None),
            (Some(&choose), None),
            (Some(&connecting), None),
            (Some(&unmanaged), None),
            (None, Some(&outside)),
        ];
        for (snapshot, service) in cases {
            for section in [
                Section::About,
                Section::SpeedTest,
                Section::DnsLeak,
                Section::Diagnostics,
            ] {
                assert_eq!(
                    badge(section, snapshot, service),
                    Badge::None,
                    "{section:?}"
                );
            }
        }
    }

    #[test]
    fn every_section_has_a_title_key() {
        for section in Section::ALL {
            assert!(section.title_key().starts_with("nav."));
        }
    }

    // From cg-gui's tray.rs.

    #[test]
    fn tray_look_follows_connection_and_health() {
        let _guard = zh();
        let none = tray_look(None);
        assert!(!none.active && !none.alert && !none.wants);
        assert_eq!(none.title, tr("gui.waiting_service", &[]));

        let connected = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.4.0".into(),
                dns: DnsMode::Phone,
            },
            wants_connection: true,
            health: HealthStatus {
                cause: Some(IssueCause::PhoneUnstable),
                ..Default::default()
            },
            ..Snapshot::default()
        };
        let look = tray_look(Some(&connected));
        assert!(look.active && look.alert && look.wants);
        assert_eq!(look.title, describe::gui_state(&connected).title);

        for wants in [true, false] {
            let retrying = Snapshot {
                state: ConnState::Retrying {
                    reason: Reason::NoExit,
                },
                wants_connection: wants,
                ..Snapshot::default()
            };
            let look = tray_look(Some(&retrying));
            assert!(!look.active && !look.alert);
            assert_eq!(look.wants, wants);
        }
    }

    #[test]
    fn menu_order_and_enabled_state() {
        let mut look = tray_look(None);
        let layout = menu_layout(&look);
        let entries: Vec<_> = layout.iter().map(|(e, _)| *e).collect();
        assert_eq!(
            entries,
            [
                MenuEntry::Version,
                MenuEntry::Separator,
                MenuEntry::Toggle { disconnect: false },
                MenuEntry::Open,
                MenuEntry::Separator,
                MenuEntry::Quit,
            ]
        );
        assert_eq!(layout.iter().filter(|(_, on)| !on).count(), 1);
        assert!(!layout[0].1);
        look.wants = true;
        assert_eq!(
            menu_layout(&look)[2].0,
            MenuEntry::Toggle { disconnect: true }
        );
    }

    // From cg-gui's app.rs.

    #[test]
    fn version_line_names_the_workspace_version() {
        assert_eq!(
            version_line(),
            format!("CellGlide {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn button_follows_wish_not_state() {
        let _guard = zh();
        let mut snapshot = Snapshot {
            state: ConnState::Retrying {
                reason: Reason::NoExit,
            },
            wants_connection: true,
            ..Snapshot::default()
        };
        assert_eq!(button_label(Some(&snapshot)), "断开，回到原来的网络");
        snapshot.wants_connection = false;
        assert_eq!(button_label(Some(&snapshot)), "连接");
        assert_eq!(button_label(None), "连接");
    }

    #[test]
    fn title_shows_connection() {
        let connected = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone,
            },
            ..Snapshot::default()
        };
        assert!(is_connected(Some(&connected)));
        assert!(!is_connected(Some(&Snapshot::default())));
        assert!(!is_connected(None));
        let _lang = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        assert_eq!(window_title(Some(&connected)), "CellGlide · Connected");
        assert_eq!(window_title(Some(&Snapshot::default())), "CellGlide");
        assert_eq!(window_title(None), "CellGlide");
    }

    #[test]
    fn plug_detection_ignores_first_snapshot_and_service_restarts() {
        let a = vec!["aaaa".to_string()];
        let t0 = Instant::now();
        let mut known = None;
        assert!(!plugged_in(&mut known, &a, t0));
        assert!(!plugged_in(&mut known, &a, t0));
        let mut empty = Some(KnownDevices::default());
        assert!(plugged_in(&mut empty, &a, t0));
        // Missing for a moment, then back: not plugged in.
        assert!(!plugged_in(&mut known, &[], t0));
        assert!(!plugged_in(&mut known, &a, t0 + REPLUG_AFTER / 2));
        // Missing for longer: plugged in, even if no snapshot came in between.
        let back = t0 + REPLUG_AFTER;
        assert!(!plugged_in(&mut known, &[], back));
        assert!(plugged_in(&mut known, &a, back + REPLUG_AFTER));
        // A service restart forgets the list: its first snapshot again doesn't count.
        let mut restarted = None;
        assert!(!plugged_in(&mut restarted, &a, back + REPLUG_AFTER));
    }

    #[test]
    fn the_plugged_in_phone_is_named() {
        let t0 = Instant::now();
        let mut known = None;
        assert_eq!(plugged_serial(&mut known, &["a".into()], t0), None);
        let both = ["a".to_string(), "b".to_string()];
        assert_eq!(plugged_serial(&mut known, &both, t0), Some("b".into()));
        assert_eq!(plugged_serial(&mut known, &both, t0), None);
        // "a" back within a moment does not count; "c" is new and named.
        assert_eq!(plugged_serial(&mut known, &["b".into()], t0), None);
        let back = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(plugged_serial(&mut known, &back, t0), Some("c".into()));
    }

    #[test]
    fn a_phone_usbmuxd_lost_for_a_moment_is_not_plugged_in() {
        // After resume (or a relock) usbmuxd can drop the phone and find it again seconds later.
        let t0 = Instant::now();
        let mut known = None;
        let seconds = |s: u64| t0 + Duration::from_secs(s);
        assert!(!plugged_in(&mut known, &["aaaa".into()], t0));
        assert!(!plugged_in(&mut known, &[], seconds(1)));
        assert!(!plugged_in(&mut known, &["aaaa".into()], seconds(27)));
        // A second iPhone that was never here still counts.
        assert!(plugged_in(
            &mut known,
            &["aaaa".into(), "bbbb".into()],
            seconds(28)
        ));
    }

    // From cg-gui's panel.rs.

    #[test]
    fn health_line_shows_only_while_connected_like_the_mac() {
        let _guard = en();
        let now = Timestamp::now();
        let utc = TimeZone::UTC;
        let mut snapshot = disconnected_snapshot();
        // A drop that ended an hour ago: `cellglide status` shows it, the GUI doesn't.
        snapshot.health = HealthStatus {
            last_issue: Some(LastIssue {
                cause: IssueCause::Channel,
                ended: now - SignedDuration::from_hours(1),
                duration: Some(45),
            }),
            ..Default::default()
        };
        assert!(describe::status_health_line(&snapshot, now, &utc).is_some());
        assert_eq!(connected_health_line(&snapshot, now, &utc), None);
        snapshot.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Phone,
        };
        assert_eq!(
            connected_health_line(&snapshot, now, &utc),
            describe::status_health_line(&snapshot, now, &utc)
        );
    }

    #[test]
    fn dot_colours() {
        let connected = connected_snapshot();
        assert_eq!(status_tone(Some(&connected)), Tone::Green);
        let mut waiting = disconnected_snapshot();
        assert_eq!(status_tone(Some(&waiting)), Tone::Gray);
        waiting.state = ConnState::Retrying {
            reason: Reason::NoExit,
        };
        assert_eq!(status_tone(Some(&waiting)), Tone::Orange);
        waiting.state = ConnState::ChoosePhone;
        assert_eq!(status_tone(Some(&waiting)), Tone::Orange);
        assert_eq!(status_tone(None), Tone::Gray);

        let mut health = connected_snapshot();
        assert_eq!(health_tone(&health), Tone::Green);
        health.health.cause = Some(IssueCause::Channel);
        assert_eq!(health_tone(&health), Tone::Red);
        health.health.cause = Some(IssueCause::PhoneUnstable);
        assert_eq!(health_tone(&health), Tone::Orange);
    }

    // From cg-gui's overview page.

    #[test]
    fn usb_device_names_the_chosen_phone_or_auto_discovery() {
        let _guard = zh();
        let chosen = disconnected_snapshot();
        assert_eq!(
            device_subtitle(Some(&chosen)),
            describe::device_label(&chosen, SERIAL)
        );
        let mut none = chosen.clone();
        none.selected = None;
        assert_eq!(device_subtitle(Some(&none)), "自动发现 / 等待已选设备");
        assert_eq!(device_subtitle(None), "自动发现 / 等待已选设备");
    }

    // From cg-gui's settings page.

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
    fn dns_line_follows_the_mode() {
        let _lang = en();
        assert_eq!(dns_line(Some(DnsMode::Unmanaged)).tone, Tone::Orange);
        assert_eq!(dns_line(Some(DnsMode::Phone)).tone, Tone::Green);
        assert_ne!(
            dns_line(Some(DnsMode::Phone)).text,
            dns_line(Some(DnsMode::Public)).text
        );
        assert_ne!(dns_line(None).text, dns_line(Some(DnsMode::Public)).text);
    }

    // From cg-gui's history page.

    fn days(from: i64, to: i64) -> Vec<String> {
        (from..=to).map(|d| format!("2026-10-{d}")).collect()
    }

    fn event(secs: i64, kind: EventKind) -> HealthEvent {
        HealthEvent::new(Timestamp::from_second(secs).unwrap(), kind)
    }

    fn day(day: &str, list: Vec<String>, events: Vec<HealthEvent>) -> HistoryDay {
        HistoryDay {
            day: day.into(),
            start: DayStart::default(),
            events,
            days: list,
            begin: Timestamp::from_second(0).unwrap(),
            end: Timestamp::from_second(86_400).unwrap(),
        }
    }

    #[test]
    fn paging_stays_within_the_seven_days() {
        let days = days(13, 19);
        assert_eq!(neighbor(&days, "2026-10-19", 1), None);
        assert_eq!(
            neighbor(&days, "2026-10-19", -1).as_deref(),
            Some("2026-10-18")
        );
        assert_eq!(neighbor(&days, "2026-10-13", -1), None);
        assert_eq!(neighbor(&days, "2026-10-01", 1), None);
    }

    #[test]
    fn the_strip_covers_the_day_and_ticks_follow_the_hours() {
        let tz = TimeZone::UTC;
        let begin = Timestamp::from_second(0).unwrap();
        let mut record = day(
            "1970-01-01",
            vec!["1970-01-01".into()],
            vec![event(3600, EventKind::TunnelStarted)],
        );
        record.begin = begin;
        record.end = begin + SignedDuration::from_hours(2);
        let strip = day_strip(&record, begin + SignedDuration::from_hours(5), &tz);
        assert_eq!(strip.begin, begin);
        assert_eq!(strip.next, begin + SignedDuration::from_hours(24));
        // Sharing from 01:00 to the end of the record (02:00).
        assert_eq!((strip.spans.len(), strip.spans[0].to), (1, record.end));
        assert_eq!(tick_gaps(&"1970-01-01".parse().unwrap(), &tz), [250; 4]);
    }

    #[test]
    fn rows_show_clock_and_event_title_only() {
        let _lock = en();
        let tz = TimeZone::UTC;
        let mut end = event(3 * 3600 + 5 * 60, EventKind::Issue);
        end.phase = Some(EventPhase::End);
        end.cause = Some(IssueCause::Channel);
        end.duration = Some(65);
        let row = row_texts(&end, &tz);
        assert_eq!(row.time, "03:05");
        assert_eq!(
            row.title,
            "Recovered: Cable connection dropped · 1 min 5 sec"
        );
        assert_eq!(row.tone, Tone::Green);
        let labels: Vec<String> = legend().into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["OK", "Unstable", "Interrupted", "Not sharing"]);
    }

    // From cg-gui's strip widget.

    fn at(hours: i64) -> Timestamp {
        Timestamp::from_second(hours * 3600).unwrap()
    }

    fn span(from: i64, to: i64, kind: SpanKind) -> Span {
        Span {
            from: at(from),
            to: at(to),
            kind,
        }
    }

    #[test]
    fn cells_follow_the_day_and_colour_by_severity() {
        let spans = [
            span(0, 12, SpanKind::Sharing),
            span(6, 9, SpanKind::Issue(IssueCause::Channel)),
            span(9, 12, SpanKind::Issue(IssueCause::PhoneUnstable)),
        ];
        let c = strip_cells(&spans, at(0), at(24), 240.0);
        assert_eq!((c[0].x, c[0].width, c[0].tone), (0.0, 120.0, Tone::Green));
        assert!((c[0].alpha - 0.6).abs() < 1e-6);
        assert_eq!((c[1].x, c[1].width, c[1].tone), (60.0, 30.0, Tone::Red));
        assert_eq!((c[2].x, c[2].tone), (90.0, Tone::Orange));
    }

    #[test]
    fn a_cell_is_at_least_one_pixel_and_inside_the_strip() {
        let spans = [
            span(5, 5, SpanKind::Issue(IssueCause::Channel)),
            span(24, 24, SpanKind::Issue(IssueCause::Channel)),
        ];
        let c = strip_cells(&spans, at(0), at(24), 240.0);
        assert_eq!(c[0].width, 1.0);
        assert_eq!((c[1].x, c[1].width), (239.0, 1.0));
    }

    #[test]
    fn a_dst_day_still_spans_the_whole_strip() {
        // A 23-hour day: the last instant is the right edge.
        let spans = [span(0, 23, SpanKind::Sharing)];
        let c = strip_cells(&spans, at(0), at(23), 100.0);
        assert_eq!((c[0].x, c[0].width), (0.0, 100.0));
    }

    // From cg-gui's checks widget.

    #[test]
    fn a_failed_check_carries_its_fix_and_a_passed_one_does_not() {
        let _lock = en();
        let list = check_rows(
            &[
                Check::new(CheckId::Tun, true),
                Check::new(CheckId::Tun, false),
            ],
            None,
        );
        assert!(list[0].state == CheckState::Pass && list[0].fix.is_none());
        assert_eq!(list[1].state, CheckState::Fail);
        assert!(list[1].fix.as_deref().is_some_and(|f| !f.is_empty()));
        assert_ne!(list[0].line, list[1].line);
    }

    #[test]
    fn a_check_that_could_not_be_made_is_its_own_state() {
        let _lock = en();
        let unreadable = Check::new(CheckId::Dns, false)
            .with("cause", "unreadable")
            .with("error", "denied");
        let rows = check_rows(
            &[
                unreadable,
                Check::new(CheckId::Dns, false).with("owner", "x"),
                Check::new(CheckId::Dns, true),
            ],
            None,
        );
        let states: Vec<CheckState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [CheckState::Unknown, CheckState::Fail, CheckState::Pass]
        );
        assert!(rows[0].fix.is_some());
        assert_eq!(
            serde_json::to_string(&CheckState::Unknown).unwrap(),
            r#""unknown""#
        );
    }

    #[test]
    fn only_the_copied_fix_is_marked() {
        let _lock = en();
        let checks = [
            Check::new(CheckId::Tun, false),
            Check::new(CheckId::Service, false),
            Check::new(CheckId::Phone, true),
        ];
        let fix = check_rows(&checks, None)[0].fix.clone().unwrap();
        let marked: Vec<bool> = check_rows(&checks, Some(&fix))
            .iter()
            .map(|r| r.copied)
            .collect();
        assert_eq!(marked, [true, false, false]);
        assert!(
            check_rows(&checks, Some("something else"))
                .iter()
                .all(|r| !r.copied)
        );
    }

    // From cg-gui's phone card widget.

    fn frame(sharing: Option<bool>) -> PhoneCard {
        PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("China Mobile".into()),
                radio: Some("5G".into()),
                sharing,
                download: Some(2_500_000.0),
                upload: Some(400_000.0),
                ..PhoneFrame::default()
            }),
            trail: vec![
                Some(SpeedPoint {
                    download: 1.0,
                    upload: 1.0,
                }),
                None,
            ],
            ..PhoneCard::default()
        }
    }

    #[test]
    fn the_speed_line_follows_sharing_and_the_frame() {
        let pick = |c: &PhoneCard| speeds(c, &describe::card_text(c));
        assert_eq!(pick(&frame(Some(true))), Speeds::Shared);
        assert_eq!(pick(&frame(Some(false))), Speeds::NotShared);
        assert_eq!(pick(&frame(None)), Speeds::NotShared);
        let usb_only = PhoneCard {
            usb: Some(UsbPhone::default()),
            ..PhoneCard::default()
        };
        assert_eq!(pick(&usb_only), Speeds::Hidden);
        assert_eq!(pick(&PhoneCard::default()), Speeds::Hidden);
    }

    // From cg-gui's trail widget.

    #[test]
    fn gaps_split_the_line_and_never_drop_to_zero() {
        let p = |d: f64| {
            Some(SpeedPoint {
                download: d,
                upload: d / 2.0,
            })
        };
        let r = trail_runs(&[p(100.0), None, p(200.0), p(400.0)], 60);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].len(), 1);
        assert!((r[1][1].down - 1.0).abs() < 1e-6); // peak 400
        assert!((r[0][0].x - 56.0 / 59.0).abs() < 1e-6); // right-aligned: 4 slots at the end
        assert!(r.iter().flatten().all(|t| t.down > 0.0)); // a gap is not 0
    }

    #[test]
    fn all_gaps_draw_nothing() {
        assert!(trail_runs(&[None, None], 60).is_empty());
    }

    // From cg-gui's curve widget.

    #[test]
    fn scales_to_the_peak_and_spreads_over_the_width() {
        let p = curve_points(&[0.0, 50.0, 100.0]);
        let point = |x, y| CurvePoint { x, y };
        assert_eq!(p, vec![point(0.0, 0.0), point(0.5, 0.5), point(1.0, 1.0)]);
    }

    #[test]
    fn fewer_than_two_points_draws_nothing() {
        assert!(curve_points(&[]).is_empty());
        assert!(curve_points(&[5.0]).is_empty());
    }

    #[test]
    fn an_all_zero_series_stays_flat() {
        assert!(curve_points(&[0.0, 0.0, 0.0]).iter().all(|p| p.y == 0.0));
    }

    // From cg-gui's diagnostics page.

    #[test]
    fn contact_is_trimmed() {
        assert_eq!(contact("  ", " \n"), None);
        assert_eq!(
            contact(" a@b.c ", "\n"),
            Some(Contact {
                email: Some("a@b.c".into()),
                note: None
            })
        );
        assert_eq!(
            contact("", "  断网了 \n"),
            Some(Contact {
                email: None,
                note: Some("断网了".into())
            })
        );
    }
}
