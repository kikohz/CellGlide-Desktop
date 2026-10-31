//! Text for states, readings and checks, in the user's language (see cg-i18n).

use crate::carrier;
use crate::health::{
    EventKind, EventPhase, HealthEvent, HealthSnapshot, HealthStatus, HistoryDay, IssueCause,
    SpanKind, day_bounds, day_spans, hour_marks,
};
use crate::phone::{BatteryReading, Grade, PhoneCard, PhoneFrame, model_name};
use crate::{Check, CheckId, ConnState, DnsMode, Reason, Request, Snapshot, StrictDns};
use cg_i18n::{tr, tr_os};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use std::fmt::Write as _;

pub mod ui;

fn tr_args(key: &str, args: &[(&str, String)]) -> String {
    let borrowed: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    tr_os(key, &borrowed)
}

pub fn reason_text(reason: &Reason) -> String {
    tr_args(reason.key(), &reason.args())
}

pub fn state_line(snapshot: &Snapshot) -> String {
    match &snapshot.state {
        ConnState::NoUsbService => tr_os("state.no_usb_service", &[]),
        ConnState::NoPhone => tr("state.no_phone", &[]),
        ConnState::ChoosePhone => tr(
            "state.choose_phone",
            &[(
                "devices",
                &snapshot.devices.join(&tr("list_separator", &[])),
            )],
        ),
        ConnState::Idle => tr("state.idle", &[]),
        ConnState::Connecting => tr("state.connecting", &[]),
        ConnState::Connected { phone_version, dns } => {
            let key = match dns {
                DnsMode::Phone => "state.connected",
                DnsMode::Public => "state.connected_public_dns",
                DnsMode::Unmanaged => "state.connected_dns_unmanaged",
            };
            tr(key, &[("version", phone_version)])
        }
        ConnState::Retrying { reason } => tr("state.retrying", &[("reason", &reason_text(reason))]),
    }
}

/// Windows' strict DNS for `status`, while connected; `None` where it doesn't apply (Linux,
/// or not connected).
pub fn strict_dns_line(snapshot: &Snapshot) -> Option<String> {
    if !matches!(snapshot.state, ConnState::Connected { .. }) {
        return None;
    }
    match snapshot.strict_dns {
        StrictDns::NotApplicable | StrictDns::Unknown => None,
        StrictDns::On => Some(tr("state.strict_dns_on", &[])),
        StrictDns::Off { .. } => Some(tr("state.strict_dns_off", &[])),
    }
}

/// A state as a title and one sentence for the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateText {
    pub title: String,
    pub detail: String,
}

pub fn gui_state(snapshot: &Snapshot) -> StateText {
    let pair = |name: &str| StateText {
        title: tr(&format!("gui.state.{name}.title"), &[]),
        detail: tr(&format!("gui.state.{name}.detail"), &[]),
    };
    match &snapshot.state {
        ConnState::NoUsbService => StateText {
            title: tr("gui.state.waiting_usb.title", &[]),
            detail: tr_os("state.no_usb_service", &[]),
        },
        ConnState::NoPhone => pair("waiting_usb"),
        ConnState::ChoosePhone => pair("choose_phone"),
        ConnState::Idle => pair("idle"),
        ConnState::Connecting => pair("connecting"),
        ConnState::Connected { dns, .. } => {
            let detail = match dns {
                DnsMode::Phone => "gui.state.connected.detail",
                DnsMode::Public => "gui.state.connected_public.detail",
                DnsMode::Unmanaged => "gui.state.connected_unmanaged.detail",
            };
            StateText {
                title: tr("gui.state.connected.title", &[]),
                detail: tr(detail, &[]),
            }
        }
        ConnState::Retrying { reason } => StateText {
            title: tr("gui.state.retrying.title", &[]),
            detail: reason_text(reason),
        },
    }
}

/// "Name · Model" of a known phone; whichever is missing is left out, and a phone with neither
/// is its serial's first 8 characters.
pub fn device_label(snapshot: &Snapshot, serial: &str) -> String {
    let label = snapshot.labels.get(serial);
    let name = label
        .and_then(|l| l.name.as_deref())
        .filter(|n| !n.is_empty());
    let model = label
        .and_then(|l| l.model.as_deref())
        .filter(|m| !m.is_empty())
        .map(model_name);
    let parts: Vec<&str> = name.into_iter().chain(model).collect();
    if parts.is_empty() {
        serial.chars().take(8).collect()
    } else {
        parts.join(" · ")
    }
}

/// Same units as the Mac app and the phone: KB/s below 1 MB/s, else MB/s.
pub fn speed(bytes_per_second: f64) -> String {
    let value = bytes_per_second.max(0.0);
    if value >= 1_000_000.0 {
        format!("{:.2} MB/s", value / 1_000_000.0)
    } else {
        format!("{:.1} KB/s", value / 1000.0)
    }
}

pub fn amount(bytes: i64) -> String {
    let value = bytes.max(0) as f64;
    if value >= 1_000_000_000.0 {
        format!("{:.2} GB", value / 1_000_000_000.0)
    } else if value >= 1_000_000.0 {
        format!("{:.1} MB", value / 1_000_000.0)
    } else {
        format!("{:.0} KB", value / 1000.0)
    }
}

fn grade_text(grade: Grade) -> String {
    let key = match grade {
        Grade::Excellent => "phone.grade.excellent",
        Grade::Good => "phone.grade.good",
        Grade::Fair => "phone.grade.fair",
        Grade::Poor => "phone.grade.poor",
    };
    tr(key, &[])
}

fn battery_percent(card: &PhoneCard) -> Option<u32> {
    let usb = card.usb.as_ref().and_then(|u| u.battery.as_ref());
    usb.and_then(|b| b.level.map(u32::from)).or_else(|| {
        let level = card.frame.as_ref()?.battery?;
        Some((level * 100.0).round() as u32)
    })
}

fn is_charging(card: &PhoneCard) -> bool {
    match card.usb.as_ref().and_then(|u| u.battery.as_ref()) {
        Some(b) => b.charging || b.fully_charged,
        None => card.frame.as_ref().and_then(|f| f.charging) == Some(true),
    }
}

fn carrier_name(card: &PhoneCard) -> Option<String> {
    let reported = card.frame.as_ref().and_then(|f| f.carrier.as_deref());
    let sims = card.usb.as_ref().map_or(&[][..], |u| u.sims.as_slice());
    carrier::data_line(sims, reported)
        .map(|c| c.display(cg_i18n::current()).to_string())
        .or_else(|| reported.map(str::to_string))
}

fn speed_line(frame: &PhoneFrame) -> Option<String> {
    if frame.sharing == Some(false) {
        return Some(tr("phone.not_sharing", &[]));
    }
    let mut parts = Vec::new();
    if let Some(down) = frame.download {
        parts.push(format!("↓ {}", speed(down)));
    }
    if let Some(up) = frame.upload {
        parts.push(format!("↑ {}", speed(up)));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn quality_line(frame: &PhoneFrame) -> String {
    let mut parts = Vec::new();
    match (frame.grade(), frame.latency) {
        (Some(grade), Some(ms)) => parts.push(tr(
            "phone.quality",
            &[
                ("grade", &grade_text(grade)),
                ("ms", &format!("{}", ms.round() as u64)),
            ],
        )),
        _ => parts.push(tr("phone.quality_measuring", &[])),
    }
    if let Some(loss) = frame.loss.filter(|l| *l > 0.0) {
        // Any loss at all shows as at least 1%.
        let percent = ((loss * 100.0).round() as u32).max(1).to_string();
        parts.push(tr("phone.loss", &[("percent", &percent)]));
    }
    if frame.sharing == Some(true)
        && let Some(bytes) = frame.session_bytes
    {
        parts.push(tr("phone.session", &[("amount", &amount(bytes))]));
    }
    parts.join(" · ")
}

fn watts(value: f64) -> String {
    format!("{value:.1}")
}

fn battery_line(battery: &BatteryReading) -> Option<String> {
    let mut parts = Vec::new();
    match battery.watts.filter(|_| battery.charging) {
        Some(w) => {
            parts.push(tr("phone.charging_watts", &[("watts", &watts(w))]));
            if let Some(supply) = battery.adapter_watts {
                let key = if battery.wireless {
                    "phone.supply_wireless"
                } else {
                    "phone.supply"
                };
                parts.push(tr(key, &[("watts", &supply.to_string())]));
            }
        }
        None if battery.fully_charged => parts.push(tr("phone.full", &[])),
        None if battery.external_connected => parts.push(tr("phone.plugged_not_charging", &[])),
        None => {}
    }
    if let Some(t) = battery.temperature {
        parts.push(tr(
            "phone.battery_temperature",
            &[("degrees", &format!("{}", t.round() as i64))],
        ));
    }
    if let Some(h) = battery.health {
        parts.push(tr(
            "phone.battery_health",
            &[("percent", &format!("{}", (h * 100.0).round() as i64))],
        ));
    }
    if let Some(n) = battery.cycles {
        parts.push(tr("phone.cycles", &[("count", &n.to_string())]));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A heat warning, shown red when severe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heat {
    pub text: String,
    pub severe: bool,
}

/// The battery's temperature decides when it is known, else the phone's thermal state.
fn heat(card: &PhoneCard) -> Option<Heat> {
    let temperature = card
        .usb
        .as_ref()
        .and_then(|u| u.battery.as_ref())
        .and_then(|b| b.temperature);
    let degrees = |t: f64| format!("{}", t.round() as i64);
    let warn = |text: String, severe: bool| Some(Heat { text, severe });
    if let Some(t) = temperature {
        return if t >= 45.0 {
            warn(
                tr("phone.overheating_degrees", &[("degrees", &degrees(t))]),
                true,
            )
        } else if t >= 40.0 {
            warn(tr("phone.hot_degrees", &[("degrees", &degrees(t))]), false)
        } else {
            None
        };
    }
    match card.frame.as_ref()?.thermal? {
        3.. => warn(tr("phone.overheating", &[]), true),
        2 => warn(tr("phone.hot", &[]), false),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryBadge {
    pub percent: u32,
    pub charging: bool,
    pub low_power: bool,
}

/// The phone card in parts, for a layout of its own; a reading that isn't known is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardText {
    /// The carrier, "Detecting" while a frame has none yet, "iPhone" with nothing at all.
    /// For stored history use `carrier`, never this.
    pub title: String,
    /// The carrier's real name, if known.
    pub carrier: Option<String>,
    pub radio: Option<String>,
    pub model: Option<String>,
    pub battery: Option<BatteryBadge>,
    pub sharing: Option<bool>,
    /// "2.50 MB/s".
    pub download: Option<String>,
    pub upload: Option<String>,
    pub quality: Option<String>,
    pub battery_line: Option<String>,
    pub heat: Option<Heat>,
    /// The frame is stale and nothing from USB backs it up: draw the card faded.
    pub dim: bool,
}

pub fn card_text(card: &PhoneCard) -> CardText {
    let frame = card.frame.as_ref();
    let usb_battery = card.usb.as_ref().and_then(|u| u.battery.as_ref());
    let carrier = carrier_name(card);
    let title = match (&carrier, frame) {
        (Some(name), _) => name.clone(),
        (None, Some(_)) => tr("phone.detecting", &[]),
        (None, None) => tr("phone.card_title", &[]),
    };
    CardText {
        title,
        carrier,
        radio: frame.and_then(|f| f.radio.clone()),
        model: card
            .usb
            .as_ref()
            .and_then(|u| u.model.as_deref())
            .map(|m| model_name(m).to_string()),
        battery: battery_percent(card).map(|percent| BatteryBadge {
            percent,
            charging: is_charging(card),
            low_power: frame.and_then(|f| f.low_power) == Some(true),
        }),
        sharing: frame.and_then(|f| f.sharing),
        download: frame.and_then(|f| f.download).map(speed),
        upload: frame.and_then(|f| f.upload).map(speed),
        quality: frame.map(quality_line),
        battery_line: usb_battery.and_then(battery_line),
        heat: heat(card),
        dim: card.stale && frame.is_some() && usb_battery.is_none(),
    }
}

/// The card has anything to show: a frame or a USB battery reading.
pub fn card_visible(card: &PhoneCard) -> bool {
    card.frame.is_some() || card.usb.as_ref().is_some_and(|u| u.battery.is_some())
}

fn battery_text(badge: &BatteryBadge) -> String {
    let key = if badge.charging {
        "phone.battery_level_charging"
    } else {
        "phone.battery_level"
    };
    tr(key, &[("percent", &badge.percent.to_string())])
}

/// The phone card as lines of text, in a fixed order; a reading that isn't known leaves its part out.
pub fn phone_lines(card: &PhoneCard) -> Vec<String> {
    let text = card_text(card);
    let mut identity = Vec::new();
    if text.carrier.is_some() || card.frame.is_some() {
        identity.push(text.title.clone());
    }
    identity.extend(text.radio.clone());
    identity.extend(text.model.clone());
    identity.extend(text.battery.as_ref().map(battery_text));
    if identity.is_empty() {
        identity.push("iPhone".into());
    }
    let mut lines = vec![identity.join(" · ")];
    if let Some(frame) = &card.frame {
        lines.extend(speed_line(frame));
        lines.extend(text.quality);
    }
    lines.extend(text.battery_line);
    lines.extend(text.heat.map(|h| h.text));
    lines
}

/// One line for the tray and window: carrier · radio · speeds · battery.
pub fn phone_summary(card: &PhoneCard) -> Option<String> {
    let text = card_text(card);
    let mut parts = Vec::new();
    parts.extend(text.carrier);
    parts.extend(text.radio);
    parts.extend(card.frame.as_ref().and_then(speed_line));
    parts.extend(text.battery.as_ref().map(battery_text));
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Cells in the day strip, 30 minutes each.
const STRIP_CELLS: i64 = 48;
/// Indent of a snapshot line under its event (the width of `HH:MM  `).
const DETAIL_INDENT: &str = "       ";

fn cause_key(cause: IssueCause) -> &'static str {
    match cause {
        IssueCause::Channel => "health.cause.channel",
        IssueCause::PhoneNetwork => "health.cause.phone_network",
        IssueCause::ComputerRelay => "health.cause.computer_relay",
        IssueCause::PhoneUnstable => "health.cause.phone_unstable",
        IssueCause::PartialFailures => "health.cause.partial_failures",
    }
}

fn cause_text(cause: IssueCause) -> String {
    tr(cause_key(cause), &[])
}

/// "45 sec" or "1 min 5 sec", as the Mac shows a duration.
pub fn duration_text(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        tr("health.seconds", &[("s", &seconds.to_string())])
    } else {
        tr(
            "health.minutes",
            &[
                ("m", &(seconds / 60).to_string()),
                ("s", &(seconds % 60).to_string()),
            ],
        )
    }
}

pub fn clock(t: Timestamp, tz: &TimeZone) -> String {
    t.to_zoned(tz.clone()).strftime("%H:%M").to_string()
}

/// The status line under the phone card, in this machine's zone.
pub fn health_line(status: &HealthStatus, now: Timestamp) -> String {
    health_line_in(status, now, &TimeZone::system())
}

/// "Phone network unstable · for 1 min 5 sec", "Connection OK · last drop 09:30 (45 sec)"
/// for a drop that ended within 24 hours, else "Connection OK".
pub fn health_line_in(status: &HealthStatus, now: Timestamp, tz: &TimeZone) -> String {
    if let (Some(cause), Some(since)) = (status.cause, status.since) {
        let held = now.duration_since(since).as_secs();
        return tr(
            "health.ongoing",
            &[
                ("cause", &cause_text(cause)),
                ("duration", &duration_text(held)),
            ],
        );
    }
    let recent = status.last_issue.filter(|last| {
        let age = now.duration_since(last.ended);
        age >= SignedDuration::ZERO && age < SignedDuration::from_hours(24)
    });
    match recent.and_then(|last| last.duration.map(|d| (last.ended, d))) {
        Some((ended, duration)) => tr(
            "health.ok_last",
            &[
                ("time", &clock(ended, tz)),
                ("duration", &duration_text(duration)),
            ],
        ),
        None => tr("health.ok", &[]),
    }
}

/// The health line for `status`: always while connected; otherwise only when there is an open
/// problem or one ended within the last 24 hours, so a plain "Connection OK" is not shown for
/// a phone that is not connected.
pub fn status_health_line(snapshot: &Snapshot, now: Timestamp, tz: &TimeZone) -> Option<String> {
    let status = &snapshot.health;
    let ongoing = status.cause.is_some() && status.since.is_some();
    let recent = status.last_issue.is_some_and(|last| {
        let age = now.duration_since(last.ended);
        last.duration.is_some()
            && age >= SignedDuration::ZERO
            && age < SignedDuration::from_hours(24)
    });
    (matches!(snapshot.state, ConnState::Connected { .. }) || ongoing || recent)
        .then(|| health_line_in(status, now, tz))
}

/// The strip's cell for the worst state painted into it: a severe issue, another issue,
/// sharing, not sharing; cells that haven't happened yet stay blank. The 48 cells share
/// the day's real length, so a daylight-saving day has shorter or longer cells.
fn strip_line(day: &HistoryDay, now: Timestamp, tz: &TimeZone) -> String {
    let (begin, next) = day_bounds_of(day, tz);
    let end = day.end.min(now);
    let spans = day_spans(&day.events, day.start, day.begin, end);
    let total = next.duration_since(begin).as_nanos();
    let at = |i: i64| {
        let nanos = total * i128::from(i) / i128::from(STRIP_CELLS);
        i64::try_from(nanos)
            .ok()
            .and_then(|n| begin.checked_add(SignedDuration::from_nanos(n)).ok())
    };
    (0..STRIP_CELLS)
        .map(|i| {
            let (Some(from), Some(to)) = (at(i), at(i + 1)) else {
                return ' ';
            };
            if from >= end {
                return ' ';
            }
            let worst = spans
                .iter()
                .filter(|s| s.from < to && (s.to > from || s.from >= from))
                .map(|s| match s.kind {
                    SpanKind::Issue(c) if c.is_severe() => 3,
                    SpanKind::Issue(_) => 2,
                    SpanKind::Sharing => 1,
                })
                .max()
                .unwrap_or(0);
            ['·', '▂', '▅', '▇'][worst]
        })
        .collect()
}

/// The day's local midnights; a day string that doesn't parse falls back to the record's
/// own `begin` and 24 hours.
fn day_bounds_of(day: &HistoryDay, tz: &TimeZone) -> (Timestamp, Timestamp) {
    day.day
        .parse::<jiff::civil::Date>()
        .ok()
        .and_then(|d| day_bounds(d, tz))
        .unwrap_or_else(|| {
            let next = day
                .begin
                .checked_add(SignedDuration::from_hours(24))
                .unwrap_or(day.end);
            (day.begin, next)
        })
}

/// The hours 0, 6, 12, 18 and 24 under the strip's cell for that moment.
fn tick_line(day: &HistoryDay, tz: &TimeZone) -> String {
    let marks = match day.day.parse::<jiff::civil::Date>() {
        Ok(d) => hour_marks(d, tz),
        Err(_) => [0u8, 6, 12, 18, 24]
            .into_iter()
            .map(|h| (h, f64::from(h) / 24.0))
            .collect(),
    };
    let mut line = String::new();
    for (hour, fraction) in marks {
        let column = (fraction * STRIP_CELLS as f64).round() as usize;
        while line.chars().count() < column {
            line.push(' ');
        }
        let _ = write!(line, "{hour}");
    }
    line
}

/// The text key of an event kind that has a fixed title; `Issue` and `VersionChanged` build theirs.
fn event_key(kind: EventKind) -> Option<&'static str> {
    Some(match kind {
        EventKind::Issue | EventKind::VersionChanged => return None,
        EventKind::TunnelStarted => "history.event.tunnel_started",
        EventKind::TunnelStopped => "history.event.tunnel_stopped",
        EventKind::UsbAttached => "history.event.usb_attached",
        EventKind::UsbDetached => "history.event.usb_detached",
        EventKind::ServiceStarted => "history.event.service_started",
        EventKind::BypassUnavailable => "history.event.bypass_unavailable",
        EventKind::BypassUpdateFailed => "history.event.bypass_update_failed",
        EventKind::DnsNotAuthorized => "history.event.dns_not_authorized",
        EventKind::StrictDnsUnavailable => "history.event.strict_dns_unavailable",
        EventKind::Unknown => "history.event.unknown",
    })
}

pub fn event_title(e: &HealthEvent) -> String {
    match e.kind {
        EventKind::Issue => {
            let cause = e.cause.map(cause_text).unwrap_or_default();
            match e.phase {
                Some(EventPhase::End) => {
                    let recovered = tr("health.recovered", &[("cause", &cause)]);
                    match e.duration {
                        Some(d) => format!("{recovered} · {}", duration_text(d)),
                        None => recovered,
                    }
                }
                Some(EventPhase::Escalate) => tr("health.escalated", &[("cause", &cause)]),
                _ => cause,
            }
        }
        EventKind::VersionChanged => tr(
            "history.event.version_changed",
            &[("detail", e.detail.as_deref().unwrap_or_default())],
        ),
        EventKind::TunnelStarted
        | EventKind::TunnelStopped
        | EventKind::UsbAttached
        | EventKind::UsbDetached
        | EventKind::ServiceStarted
        | EventKind::BypassUnavailable
        | EventKind::BypassUpdateFailed
        | EventKind::DnsNotAuthorized
        | EventKind::StrictDnsUnavailable
        | EventKind::Unknown => event_key(e.kind)
            .map(|key| tr(key, &[]))
            .unwrap_or_default(),
    }
}

fn failure_kind(kind: &str) -> String {
    let key = match kind {
        "timeout" => "history.failure.timeout",
        "refused" => "history.failure.refused",
        "dns" => "history.failure.dns",
        _ => "history.failure.other",
    };
    tr(key, &[])
}

/// How `snapshot_items` reports connection failures.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Failures {
    /// The Mac's detail grid: the total, then the most frequent kind as its own item.
    Total,
    /// The CLI line: every kind with its count.
    Each,
}

fn snapshot_parts(s: &HealthSnapshot, failures: Failures) -> Vec<(String, String)> {
    let item = |key: &str, value: String| (tr(key, &[]), value);
    let mut parts = Vec::new();
    if let Some(ms) = s.channel_rtt {
        parts.push(item("history.detail.cable_latency", format!("{ms} ms")));
    }
    if let Some(ms) = s.phone_latency {
        parts.push(item("history.detail.phone_latency", format!("{ms} ms")));
    }
    if let Some(loss) = s.phone_loss {
        let percent = (loss * 100.0).round() as i64;
        parts.push(item("history.detail.loss", format!("{percent}%")));
    }
    if let Some(radio) = &s.radio {
        parts.push(item("history.detail.network", radio.clone()));
    }
    if let Some(failed) = s.connects_failed.as_ref().filter(|f| !f.is_empty()) {
        let mut kinds: Vec<(&String, &i64)> = failed.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        match failures {
            Failures::Each => {
                let text = kinds
                    .iter()
                    .map(|(kind, n)| format!("{} {n}", failure_kind(kind)))
                    .collect::<Vec<_>>()
                    .join(&tr("list_separator", &[]));
                parts.push(item("history.detail.failures", text));
            }
            Failures::Total => {
                let total: i64 = failed.values().sum();
                parts.push(item("history.detail.failures", total.to_string()));
                parts.push(item("history.detail.main_cause", failure_kind(kinds[0].0)));
            }
        }
    }
    if let Some(state) = s.thermal {
        let state = state.clamp(0, 3);
        parts.push(item(
            "history.detail.phone_temperature",
            tr(&format!("history.thermal.{state}"), &[]),
        ));
    }
    parts
}

/// What the legs looked like when an issue started or got worse, as (name, value) in the Mac's
/// detail-grid order; unknown readings are left out.
pub fn snapshot_items(s: &HealthSnapshot) -> Vec<(String, String)> {
    snapshot_parts(s, Failures::Total)
}

/// The same readings as one line, for the CLI.
fn snapshot_line(s: &HealthSnapshot) -> Option<String> {
    let parts: Vec<String> = snapshot_parts(s, Failures::Each)
        .into_iter()
        .map(|(name, value)| format!("{name} {value}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The dot beside an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventTone {
    Green,
    Orange,
    Red,
    Gray,
}

pub fn event_tone(e: &HealthEvent) -> EventTone {
    match e.kind {
        EventKind::Issue => match (e.phase, e.cause) {
            (Some(EventPhase::End), _) => EventTone::Green,
            (_, Some(cause)) if cause.is_severe() => EventTone::Red,
            _ => EventTone::Orange,
        },
        EventKind::TunnelStarted => EventTone::Green,
        _ => EventTone::Gray,
    }
}

/// "Today, Oct 19", "Yesterday, Oct 18"; an older day also names its weekday.
pub fn day_label(day: &HistoryDay) -> String {
    let parsed = day.day.parse::<jiff::civil::Date>().ok();
    let date = parsed
        .map(|d| d.strftime(&tr("history.date", &[])).to_string())
        .unwrap_or_else(|| day.day.clone());
    let position = day.days.iter().rposition(|d| *d == day.day);
    let last = day.days.len().checked_sub(1);
    match (position, last) {
        (Some(p), Some(l)) if p == l => tr("history.today", &[("date", &date)]),
        (Some(p), Some(l)) if p + 1 == l => tr("history.yesterday", &[("date", &date)]),
        _ => match parsed {
            Some(d) => {
                let weekday = tr(
                    &format!("history.weekday.{}", d.weekday().to_monday_one_offset()),
                    &[],
                );
                tr("history.older", &[("date", &date), ("weekday", &weekday)])
            }
            None => date,
        },
    }
}

fn day_title(day: &HistoryDay) -> String {
    format!("{} · {}", tr("history.title", &[]), day_label(day))
}

/// One day of the timeline as text, in this machine's zone.
pub fn history_lines(day: &HistoryDay, now: Timestamp) -> Vec<String> {
    history_lines_in(day, now, &TimeZone::system())
}

/// The title, the 24-hour strip and its hours, one line per event (oldest first; an issue's
/// start and escalation followed by what the legs looked like), and the days on record.
pub fn history_lines_in(day: &HistoryDay, now: Timestamp, tz: &TimeZone) -> Vec<String> {
    let mut lines = vec![day_title(day), strip_line(day, now, tz), tick_line(day, tz)];
    for e in &day.events {
        lines.push(format!("{}  {}", clock(e.t, tz), event_title(e)));
        let opens = e.kind == EventKind::Issue
            && matches!(e.phase, Some(EventPhase::Start | EventPhase::Escalate));
        if opens && let Some(line) = e.snapshot.as_ref().and_then(snapshot_line) {
            lines.push(format!("{DETAIL_INDENT}{line}"));
        }
    }
    if day.events.is_empty() {
        lines.push(tr("history.empty", &[]));
    }
    lines.push(tr(
        "history.days",
        &[("days", &day.days.join(&tr("list_separator", &[])))],
    ));
    lines
}

fn check_name(id: CheckId) -> &'static str {
    match id {
        CheckId::Service => "service",
        CheckId::Group => "group",
        CheckId::Usbmuxd => "usbmuxd",
        CheckId::Phone => "phone",
        CheckId::Trust => "trust",
        CheckId::Resolved => "resolved",
        CheckId::Polkit => "polkit",
        CheckId::PolkitRule => "polkit_rule",
        CheckId::Tun => "tun",
        CheckId::AppleService => "apple_service",
        CheckId::Wintun => "wintun",
        CheckId::Dns => "dns",
    }
}

/// The check's sentence and, when it failed, the fix to show.
pub fn check_lines(check: &Check) -> (String, Option<String>) {
    let name = check_name(check.id);
    let args: Vec<(&str, String)> = check
        .args
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let state = match (check.ok, check.id) {
        (true, CheckId::Usbmuxd)
            if check.args.get("running").map(String::as_str) == Some("false") =>
        {
            "idle"
        }
        // The service couldn't read its port tables: not known to be free.
        (false, CheckId::Dns)
            if check.args.get("cause").map(String::as_str) == Some("unreadable") =>
        {
            "unknown"
        }
        (true, _) => "ok",
        (false, _) => "fail",
    };
    let args: Vec<(&str, String)> = args
        .into_iter()
        .map(|(k, v)| match (check.id, k) {
            // Which Apple package provides the service, as words.
            (CheckId::AppleService, "kind") => {
                let key = format!("check.apple_service.kind_{v}");
                let text = if cg_i18n::KEYS.contains(&key.as_str()) {
                    tr(&key, &[])
                } else {
                    v
                };
                (k, text)
            }
            _ => (k, v),
        })
        .collect();
    let line = tr_args(&format!("check.{name}.{state}"), &args);
    let fix = (!check.ok).then(|| tr_os(&fix_key(check), &[]));
    (line, fix)
}

/// The fix for a failed check: usually one per check, but an unreachable service is fixed
/// differently when it refused us or isn't ours (`cause`) than when it isn't running.
fn fix_key(check: &Check) -> String {
    let name = check_name(check.id);
    if let Some(cause) = check.args.get("cause") {
        let key = format!("check.{name}.fix_{cause}");
        if cg_i18n::KEYS.contains(&key.as_str()) {
            return key;
        }
    }
    format!("check.{name}.fix")
}

pub fn checks(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let (line, fix) = check_lines(check);
        let _ = writeln!(out, "{} {line}", if check.ok { "✓" } else { "✗" });
        if let Some(fix) = fix {
            let _ = writeln!(out, "  {}", tr("check.fix", &[("fix", &fix)]));
        }
    }
    out
}

pub fn version_mismatch(service: &str) -> String {
    tr(
        "reason.version_mismatch",
        &[
            ("service", service),
            ("client", &crate::PROTOCOL_VERSION.to_string()),
        ],
    )
}

/// The check that explains why the service couldn't be reached.
pub fn service_check(error: &crate::client::ClientError) -> Check {
    use crate::client::ClientError;
    let service = Check::new(CheckId::Service, false);
    match error {
        // Linux: the socket is for the cellglide group.
        #[cfg(unix)]
        ClientError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Check::new(CheckId::Group, false)
        }
        // Windows: the pipe lets in administrators and interactive users only.
        #[cfg(windows)]
        ClientError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => service
            .with("error", tr("reason.not_allowed", &[]))
            .with("cause", "not_allowed"),
        // Only the OS-level cause: the sentence around it comes from the locale file.
        ClientError::Io(e) => service.with("error", e.to_string()),
        ClientError::Json(e) => service.with("error", e.to_string()),
        ClientError::Closed => service.with("error", "closed"),
        ClientError::NotService => service
            .with("error", tr("check.service.impostor", &[]))
            .with("cause", "impostor"),
    }
}

/// Text for a failed request: an unparsable reply means a service from another version.
pub fn unreachable(error: &crate::client::ClientError) -> String {
    match error {
        crate::client::ClientError::Json(_) => version_mismatch("1"),
        _ => checks(&[service_check(error)]).trim_end().into(),
    }
}

/// What the connect switch does from this state.
pub fn toggle_request(snapshot: &Snapshot) -> Request {
    if snapshot.wants_connection {
        Request::Disconnect
    } else {
        Request::Connect { serial: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::Lang;
    use std::sync::MutexGuard;

    #[test]
    fn reason_and_cause_keys_are_listed() {
        let _guard = language();
        for reason in &Reason::all() {
            assert!(cg_i18n::KEYS.contains(&reason.key()), "{reason:?}");
            for lang in [Lang::ZhCn, Lang::En] {
                cg_i18n::set(lang);
                let text = reason_text(reason);
                assert!(!text.contains("reason.") && !text.contains("%{"), "{text}");
            }
        }
        for cause in IssueCause::all() {
            assert!(cg_i18n::KEYS.contains(&cause_key(cause)), "{cause:?}");
            for lang in [Lang::ZhCn, Lang::En] {
                cg_i18n::set(lang);
                let text = cause_text(cause);
                assert!(!text.contains("health.") && !text.contains("%{"), "{text}");
            }
        }
        for kind in EventKind::all() {
            if let Some(key) = event_key(kind) {
                assert!(cg_i18n::KEYS.contains(&key), "{key} missing from KEYS");
            }
            for lang in [Lang::ZhCn, Lang::En] {
                cg_i18n::set(lang);
                let mut event = HealthEvent::new(Timestamp::UNIX_EPOCH, kind);
                event.cause = Some(IssueCause::Channel);
                event.detail = Some("1.0".into());
                let text = event_title(&event);
                assert!(
                    !text.is_empty()
                        && !text.contains("history.")
                        && !text.contains("health.")
                        && !text.contains("%{"),
                    "{kind:?}: {text}"
                );
            }
        }
        for key in [
            "health.recovered",
            "health.escalated",
            "history.event.version_changed",
        ] {
            assert!(cg_i18n::KEYS.contains(&key), "{key} missing from KEYS");
        }
    }

    /// The language is process-wide: tests that switch it take turns (the shared lock).
    fn language() -> MutexGuard<'static, ()> {
        cg_i18n::lock_for_tests()
    }

    #[test]
    fn an_unreadable_port_table_reads_as_unchecked() {
        let _lang = language();
        let check = Check::new(CheckId::Dns, false)
            .with("cause", "unreadable")
            .with("error", "TCP (IPv6): error 87");
        cg_i18n::set(Lang::En);
        let (line, fix) = check_lines(&check);
        assert_eq!(
            line,
            "Couldn't check whether another program is using DNS port 53 (TCP (IPv6): error 87)"
        );
        assert!(fix.unwrap().contains("restart the computer"));
        cg_i18n::set(Lang::ZhCn);
        let (line, fix) = check_lines(&check);
        assert!(line.starts_with("无法检查 DNS 端口 53"), "{line}");
        assert!(fix.unwrap().contains("重启电脑"));
    }

    fn snap(state: ConnState) -> Snapshot {
        Snapshot {
            state,
            devices: vec!["00008110-001A".into()],
            ..Snapshot::default()
        }
    }

    #[test]
    fn strict_dns_shows_only_where_it_applies() {
        let _lang = language();
        let connected = |strict_dns| Snapshot {
            strict_dns,
            ..snap(ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone,
            })
        };
        let off = StrictDns::Off {
            reason: "BFE unavailable".into(),
        };
        // Linux, and anything not connected: no line at all.
        assert_eq!(strict_dns_line(&connected(StrictDns::NotApplicable)), None);
        assert_eq!(
            strict_dns_line(&Snapshot {
                strict_dns: StrictDns::On,
                ..snap(ConnState::Idle)
            }),
            None
        );
        cg_i18n::set(Lang::ZhCn);
        assert_eq!(
            strict_dns_line(&connected(StrictDns::On)).unwrap(),
            "严格 DNS：已开启（DNS 查询只经手机发出）"
        );
        assert!(
            strict_dns_line(&connected(off.clone()))
                .unwrap()
                .starts_with("严格 DNS：未开启")
        );
        cg_i18n::set(Lang::En);
        assert!(
            strict_dns_line(&connected(StrictDns::On))
                .unwrap()
                .starts_with("Strict DNS: on")
        );
        assert!(
            strict_dns_line(&connected(off))
                .unwrap()
                .starts_with("Strict DNS: off")
        );
    }

    /// A state from a newer service: no line rather than a guess.
    #[test]
    fn unknown_strict_dns_shows_nothing() {
        let s = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.4.0".into(),
                dns: DnsMode::Phone,
            },
            strict_dns: StrictDns::Unknown,
            ..Snapshot::default()
        };
        assert_eq!(strict_dns_line(&s), None);
    }

    #[test]
    fn renders_in_both_languages() {
        let _lang = language();
        cg_i18n::set(Lang::ZhCn);
        assert_eq!(state_line(&snap(ConnState::NoPhone)), "没有连接 iPhone");
        assert_eq!(
            state_line(&snap(ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone
            })),
            "已连接，电脑正在使用手机网络（手机 CellGlide 2.2.0）"
        );
        assert!(
            state_line(&snap(ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Unmanaged
            }))
            .ends_with("但系统没有让 CellGlide 设置 DNS，域名查询仍走原来的网络，没有其他网络时网址会打不开。运行 cellglide doctor 查看原因和修复命令")
        );
        assert_eq!(
            check_lines(&Check::new(CheckId::Polkit, false)).1.unwrap(),
            "Ubuntu 22.04：sudo apt install policykit-1；较新的 Ubuntu / Debian：sudo apt install polkitd；Fedora：sudo dnf install polkit"
        );
        assert_eq!(
            state_line(&snap(ConnState::Retrying {
                reason: Reason::ResolverPortBusy { port: 1053 }
            })),
            "连接未成功，稍后自动重试：本机 DNS 端口 1053 已被占用"
        );
        let choose = state_line(&Snapshot {
            state: ConnState::ChoosePhone,
            devices: vec!["a".into(), "b".into()],
            ..Snapshot::default()
        });
        assert!(choose.contains("--serial") && choose.ends_with("a、b"));
        let text = checks(&[
            Check::new(CheckId::Tun, true),
            Check::new(CheckId::Trust, false).with("serials", "abcd"),
        ]);
        #[cfg(unix)]
        {
            assert_eq!(
                text,
                "✓ 内核支持 TUN 虚拟网卡\n✗ iPhone 还没有信任这台电脑：abcd\n  修复：解锁 iPhone，在弹窗中点「信任」；没有弹窗时重新插拔数据线\n"
            );
            assert_eq!(
                check_lines(&Check::new(CheckId::Usbmuxd, true).with("running", "false")).0,
                "USB 服务已安装，插上 iPhone 时会自动启动"
            );
            assert_eq!(
                state_line(&snap(ConnState::NoUsbService)),
                "没有检测到 iPhone（USB 服务 usbmuxd 未运行，插上 iPhone 后会自动启动）"
            );
        }
        #[cfg(windows)]
        {
            assert_eq!(
                text,
                "✓ 内核支持 TUN 虚拟网卡\n✗ iPhone 还没有信任这台电脑：abcd\n  修复：解锁 iPhone，在弹窗中点「信任」；没有弹窗时打开「Apple 设备」应用，再重新插拔数据线\n"
            );
            assert_eq!(
                check_lines(&Check::new(CheckId::Usbmuxd, true).with("running", "false")).0,
                "Apple 设备服务已安装，登录 Windows 后会自动运行（端口 27015 暂时没有响应）"
            );
            assert!(
                state_line(&snap(ConnState::NoUsbService)).contains("「Apple 设备」应用"),
                "{}",
                state_line(&snap(ConnState::NoUsbService))
            );
            assert_eq!(
                check_lines(&Check::new(CheckId::AppleService, true).with("kind", "store")).0,
                "Apple 设备服务已安装（「Apple 设备」应用）"
            );
            let (line, fix) =
                check_lines(&Check::new(CheckId::Dns, false).with("owner", "svchost.exe"));
            assert_eq!(line, "DNS 端口 53 被其他程序占用：svchost.exe");
            assert!(fix.unwrap().contains("Internet 连接共享"));
        }

        cg_i18n::set(Lang::En);
        assert_eq!(state_line(&snap(ConnState::NoPhone)), "No iPhone connected");
        assert!(
            state_line(&snap(ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Unmanaged
            }))
            .ends_with("but the system didn't let CellGlide set DNS, so lookups still use the original network and websites won't open without another network. Run cellglide doctor to see why and how to fix it")
        );
        let (line, fix) = check_lines(&Check::new(CheckId::PolkitRule, false));
        assert_eq!(
            line,
            "CellGlide's polkit rule is missing, so systemd-resolved won't let CellGlide set DNS"
        );
        assert_eq!(
            fix.unwrap(),
            "reinstall CellGlide: sudo apt install --reinstall cellglide (Fedora: sudo dnf reinstall cellglide)"
        );
        assert_eq!(
            check_lines(&Check::new(CheckId::Resolved, false))
                .1
                .unwrap(),
            "Ubuntu / Debian: sudo apt install systemd-resolved, then sudo systemctl enable --now systemd-resolved; Fedora: sudo systemctl enable --now systemd-resolved"
        );
        assert_eq!(
            reason_text(&Reason::Tunnel {
                detail: "EBUSY".into()
            }),
            "Couldn't set up the virtual network interface: EBUSY"
        );
        assert!(
            state_line(&Snapshot {
                state: ConnState::ChoosePhone,
                devices: vec!["a".into(), "b".into()],
                ..Snapshot::default()
            })
            .ends_with("a, b")
        );
        assert_eq!(
            check_lines(&Check::new(CheckId::Phone, true).with("count", "2")).0,
            "2 iPhone(s) detected"
        );
        #[cfg(unix)]
        {
            let denied =
                crate::client::ClientError::Io(std::io::ErrorKind::PermissionDenied.into());
            assert_eq!(
                unreachable(&denied),
                "✗ The current user isn't in the cellglide group and can't control the service\n  Fix: sudo usermod -aG cellglide $USER, then log out and back in"
            );
            let missing = crate::client::ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such socket",
            ));
            let text = unreachable(&missing);
            assert!(text.contains("no such socket"), "{text}");
            assert!(!text.contains("can't reach"), "{text}");
        }
        #[cfg(windows)]
        {
            use crate::client::ClientError;
            let denied = ClientError::Io(std::io::ErrorKind::PermissionDenied.into());
            assert_eq!(
                unreachable(&denied),
                "✗ Can't reach the CellGlide service (Only an administrator or the user currently using this computer can do this)\n  Fix: use the account that is signed in at this computer, or ask an administrator"
            );
            assert_eq!(
                service_check(&denied),
                Check::new(CheckId::Service, false)
                    .with(
                        "error",
                        "Only an administrator or the user currently using this computer can do this"
                    )
                    .with("cause", "not_allowed")
            );
            // Starting the service fixes neither of these: it is running, or someone else
            // holds its pipe.
            let impostor = unreachable(&ClientError::NotService);
            assert_eq!(
                impostor,
                "✗ Can't reach the CellGlide service (another program has taken its control pipe)\n  Fix: another program is using CellGlide's control channel; restart the computer or reinstall CellGlide"
            );
            let stopped = unreachable(&ClientError::Io(std::io::ErrorKind::NotFound.into()));
            assert!(stopped.ends_with("Start-Service CellGlide"), "{stopped}");
            let (line, fix) = check_lines(&Check::new(CheckId::Usbmuxd, false));
            assert!(line.contains("27015"), "{line}");
            assert!(
                fix.unwrap()
                    .contains("winget install --id 9NP83LWLPZ9K --source msstore")
            );
            assert_eq!(
                check_lines(&Check::new(CheckId::AppleService, true).with("kind", "itunes")).0,
                "Apple's device service is installed (iTunes / Apple Mobile Device Support)"
            );
            assert_eq!(
                reason_text(&Reason::NoUsbService),
                "Apple's device service (from the Apple Devices app) can't be found"
            );
        }
        assert_eq!(speed(512.0), "0.5 KB/s");
        assert_eq!(speed(0.0), "0.0 KB/s");
        assert_eq!(speed(999_999.0), "1000.0 KB/s");
        assert_eq!(speed(3_500_000.0), "3.50 MB/s");
        assert_eq!(amount(123_000), "123 KB");
        assert_eq!(amount(5_500_000), "5.5 MB");
        assert_eq!(amount(1_234_567_890), "1.23 GB");
    }

    #[test]
    fn every_check_renders_in_both_languages() {
        let _lang = language();
        let all = [
            CheckId::Service,
            CheckId::Group,
            CheckId::Usbmuxd,
            CheckId::Phone,
            CheckId::Trust,
            CheckId::Resolved,
            CheckId::Polkit,
            CheckId::PolkitRule,
            CheckId::Tun,
            CheckId::AppleService,
            CheckId::Wintun,
            CheckId::Dns,
        ];
        for id in all {
            // Adding a CheckId fails to compile here until the list above names it.
            match id {
                CheckId::Service
                | CheckId::Group
                | CheckId::Usbmuxd
                | CheckId::Phone
                | CheckId::Trust
                | CheckId::Resolved
                | CheckId::Polkit
                | CheckId::PolkitRule
                | CheckId::Tun
                | CheckId::AppleService
                | CheckId::Wintun
                | CheckId::Dns => {}
            }
            let name = check_name(id);
            for state in ["ok", "fail", "fix"] {
                let key = format!("check.{name}.{state}");
                assert!(cg_i18n::KEYS.contains(&key.as_str()), "{key} missing");
            }
            for lang in [Lang::ZhCn, Lang::En] {
                cg_i18n::set(lang);
                for ok in [true, false] {
                    let check = Check::new(id, ok)
                        .with("count", "1")
                        .with("serials", "ab")
                        .with("error", "e")
                        .with("kind", "store")
                        .with("owner", "x.exe");
                    let (line, fix) = check_lines(&check);
                    for text in std::iter::once(&line).chain(fix.as_ref()) {
                        assert!(
                            !text.is_empty() && !text.contains("check.") && !text.contains("%{"),
                            "{id:?} {lang:?}: {text}"
                        );
                    }
                    assert_eq!(fix.is_some(), !ok);
                }
            }
        }
        for kind in ["store", "itunes"] {
            let key = format!("check.apple_service.kind_{kind}");
            assert!(cg_i18n::KEYS.contains(&key.as_str()), "{key} missing");
        }
    }

    fn full_card() -> PhoneCard {
        use crate::phone::*;
        PhoneCard {
            frame: Some(PhoneFrame {
                sharing: Some(true),
                download: Some(2_500_000.0),
                upload: Some(12_300.0),
                quality_score: Some(90),
                latency: Some(41.6),
                loss: Some(0.004),
                session_bytes: Some(1_234_567_890),
                thermal: Some(3),
                carrier: Some("AT&T".into()),
                radio: Some("5G NSA".into()),
                ..Default::default()
            }),
            usb: Some(UsbPhone {
                model: Some("iPhone18,4".into()),
                sims: vec![SimCarrier {
                    bundle: Some("com.apple.ATT_aio_NR_US".into()),
                    mcc: Some("310".into()),
                    mnc: Some("410".into()),
                }],
                battery: Some(BatteryReading {
                    level: Some(75),
                    charging: true,
                    external_connected: true,
                    watts: Some(2.562),
                    adapter_watts: Some(15),
                    health: Some(0.949),
                    cycles: Some(396),
                    temperature: Some(38.15),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn phone_lines_in_both_languages() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        assert_eq!(
            phone_lines(&full_card()),
            [
                "Cricket · 5G NSA · iPhone Air · 75% (charging)",
                "↓ 2.50 MB/s ↑ 12.3 KB/s",
                "Network quality Excellent · 42 ms · 1% loss · This session 1.23 GB",
                "Charging 2.6 W · 15 W supply · Battery 38°C · Battery health 95% · 396 cycles",
            ]
        );
        cg_i18n::set(Lang::ZhCn);
        assert_eq!(
            phone_lines(&full_card()),
            [
                "Cricket · 5G NSA · iPhone Air · 75%（充电中）",
                "↓ 2.50 MB/s ↑ 12.3 KB/s",
                "网络质量 优 · 42 ms · 丢包 1% · 本次 1.23 GB",
                "充电 2.6 W · 15 W 供电 · 电池 38°C · 电池健康 95% · 396 次循环",
            ]
        );
        cg_i18n::set(Lang::En);
    }

    #[test]
    fn heat_follows_the_battery_temperature_then_the_thermal_state() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        let mut card = full_card();
        card.usb
            .as_mut()
            .unwrap()
            .battery
            .as_mut()
            .unwrap()
            .temperature = Some(46.0);
        assert_eq!(
            phone_lines(&card).last().unwrap(),
            "iPhone is overheating at 46°C and may slow down"
        );
        card.usb
            .as_mut()
            .unwrap()
            .battery
            .as_mut()
            .unwrap()
            .temperature = Some(41.0);
        assert_eq!(phone_lines(&card).last().unwrap(), "iPhone is hot at 41°C");
        let frame_only = PhoneCard {
            frame: Some(PhoneFrame {
                thermal: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        let lines = phone_lines(&frame_only);
        assert_eq!(
            lines.last().unwrap(),
            "iPhone is overheating and may slow down"
        );
        assert_eq!(lines.len(), 3, "no battery detail line: {lines:?}");
    }

    #[test]
    fn not_sharing_and_measuring() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        let mut card = PhoneCard {
            frame: Some(PhoneFrame {
                sharing: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            phone_lines(&card),
            ["Detecting", "Not sharing", "Measuring network quality"]
        );
        card.frame.as_mut().unwrap().session_bytes = Some(5_500_000);
        assert_eq!(phone_lines(&card).len(), 3);
        assert!(!phone_lines(&card)[2].contains("session"));
        card.frame.as_mut().unwrap().sharing = Some(true);
        card.frame.as_mut().unwrap().download = Some(0.0);
        card.frame.as_mut().unwrap().upload = Some(0.0);
        assert_eq!(
            phone_lines(&card)[1..],
            [
                "↓ 0.0 KB/s ↑ 0.0 KB/s",
                "Measuring network quality · This session 5.5 MB"
            ]
        );
        assert_eq!(phone_summary(&card).unwrap(), "↓ 0.0 KB/s ↑ 0.0 KB/s");
        assert_eq!(phone_lines(&PhoneCard::default()), ["iPhone"]);
    }

    #[test]
    fn summary_is_carrier_radio_speeds_battery() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        assert_eq!(
            phone_summary(&full_card()).unwrap(),
            "Cricket · 5G NSA · ↓ 2.50 MB/s ↑ 12.3 KB/s · 75% (charging)"
        );
        assert_eq!(phone_summary(&PhoneCard::default()), None);
        let mut usb_only = full_card();
        usb_only.frame = None;
        assert_eq!(
            phone_summary(&usb_only).unwrap(),
            "Cricket · 75% (charging)"
        );
    }

    #[test]
    fn loss_is_rounded_with_a_floor_of_one_percent() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        for (loss, text) in [(0.004, "1% loss"), (0.014, "1% loss"), (0.101, "10% loss")] {
            let frame = PhoneFrame {
                loss: Some(loss),
                ..Default::default()
            };
            assert!(quality_line(&frame).ends_with(text), "{loss}");
        }
    }

    #[test]
    fn adapter_watts_show_only_while_charging_with_watts() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        let plugged = BatteryReading {
            external_connected: true,
            fully_charged: true,
            adapter_watts: Some(15),
            ..Default::default()
        };
        assert_eq!(battery_line(&plugged).unwrap(), "Fully charged");
    }

    mod history {
        use super::super::*;
        use super::language;
        use crate::health::*;
        use cg_i18n::Lang;
        use jiff::Timestamp;
        use jiff::tz::TimeZone;
        use std::collections::BTreeMap;

        /// 2026-10-16 at `h:m` UTC (the day Task 6's day_spans cases use).
        fn hm(h: i64, m: i64) -> Timestamp {
            Timestamp::from_second(1_792_108_800 + h * 3600 + m * 60).unwrap()
        }

        fn issue(
            t: Timestamp,
            cause: IssueCause,
            phase: EventPhase,
            duration: Option<i64>,
            snapshot: Option<HealthSnapshot>,
        ) -> HealthEvent {
            HealthEvent {
                cause: Some(cause),
                phase: Some(phase),
                duration,
                snapshot,
                ..HealthEvent::new(t, EventKind::Issue)
            }
        }

        fn days() -> Vec<String> {
            (10..=16).map(|d| format!("2026-10-{d}")).collect()
        }

        /// Every kind of event, around Task 6's day_spans case (sharing 9–12, an issue
        /// 10:00–10:10 escalating at 10:05), plus a milder issue at 13:00.
        fn full_day() -> HistoryDay {
            let detail = HealthEvent {
                detail: Some("0.9.0 | 2.2.0 → 1.0.0 | 2.2.0".into()),
                ..HealthEvent::new(hm(8, 2), EventKind::VersionChanged)
            };
            let first = HealthSnapshot {
                channel: Some(SegmentState::Ok),
                phone_exit: Some(SegmentState::Failed),
                channel_rtt: Some(4),
                phone_latency: Some(41),
                phone_loss: Some(0.02),
                radio: Some("5G".into()),
                connects_failed: Some(BTreeMap::from([
                    ("refused".into(), 1),
                    ("timeout".into(), 3),
                ])),
                thermal: Some(1),
                computer_version: Some("1.0.0".into()),
                ..Default::default()
            };
            let worse = HealthSnapshot {
                thermal: Some(2),
                ..Default::default()
            };
            HistoryDay {
                day: "2026-10-16".into(),
                start: DayStart::default(),
                events: vec![
                    HealthEvent::new(hm(8, 0), EventKind::ServiceStarted),
                    HealthEvent::new(hm(8, 1), EventKind::UsbAttached),
                    detail,
                    HealthEvent::new(hm(9, 0), EventKind::TunnelStarted),
                    HealthEvent::new(hm(9, 0), EventKind::DnsNotAuthorized),
                    HealthEvent::new(hm(9, 30), EventKind::BypassUnavailable),
                    issue(
                        hm(10, 0),
                        IssueCause::PhoneUnstable,
                        EventPhase::Start,
                        None,
                        Some(first),
                    ),
                    issue(
                        hm(10, 5),
                        IssueCause::Channel,
                        EventPhase::Escalate,
                        None,
                        Some(worse),
                    ),
                    issue(
                        hm(10, 10),
                        IssueCause::Channel,
                        EventPhase::End,
                        Some(600),
                        None,
                    ),
                    HealthEvent::new(hm(11, 0), EventKind::BypassUpdateFailed),
                    HealthEvent::new(hm(12, 0), EventKind::TunnelStopped),
                    HealthEvent::new(hm(12, 1), EventKind::UsbDetached),
                    issue(
                        hm(13, 0),
                        IssueCause::PartialFailures,
                        EventPhase::Start,
                        None,
                        Some(HealthSnapshot::default()),
                    ),
                    issue(
                        hm(13, 5),
                        IssueCause::PartialFailures,
                        EventPhase::End,
                        Some(300),
                        None,
                    ),
                ],
                days: days(),
                begin: hm(0, 0),
                end: hm(18, 0),
            }
        }

        /// 0:00–9:00 not sharing, 9:00–12:00 sharing with the severe issue in the 10:00
        /// cell, the mild issue at 13:00, nothing after 18:00 (still to come).
        fn strip() -> String {
            format!(
                "{}▂▂▇▂▂▂{}▅{}{}",
                "·".repeat(18),
                "·".repeat(2),
                "·".repeat(9),
                " ".repeat(12)
            )
        }

        const TICKS: &str = "0           6           12          18          24";

        #[test]
        fn history_text_in_both_languages() {
            let _lang = language();
            let utc = TimeZone::UTC;
            let now = hm(18, 0);
            cg_i18n::set(Lang::ZhCn);
            assert_eq!(
                history_lines_in(&full_day(), now, &utc),
                [
                    "连接记录 · 今天，10月16日".to_string(),
                    strip(),
                    TICKS.into(),
                    "08:00  后台服务启动".into(),
                    "08:01  USB 已连接".into(),
                    "08:02  版本更新：0.9.0 | 2.2.0 → 1.0.0 | 2.2.0".into(),
                    "09:00  已开始共享".into(),
                    "09:00  系统没有让 CellGlide 设置 DNS".into(),
                    "09:30  没有找到原来的网关，局域网设备暂时经手机访问".into(),
                    "10:00  手机网络不稳".into(),
                    "       数据线延迟 4 ms · 手机延迟 41 ms · 丢包 2% · 网络 5G · 连接失败 超时 3、拒绝 1 · 手机温度 偏热".into(),
                    "10:05  数据线连接中断（升级）".into(),
                    "       手机温度 很热".into(),
                    "10:10  数据线连接中断 已恢复 · 10 分 0 秒".into(),
                    "11:00  网络切换后局域网路由更新失败，下次变化时重试".into(),
                    "12:00  已停止共享".into(),
                    "12:01  USB 已断开".into(),
                    "13:00  部分连接失败".into(),
                    "13:05  部分连接失败 已恢复 · 5 分 0 秒".into(),
                    format!("可查看：{}", days().join("、")),
                ]
            );
            cg_i18n::set(Lang::En);
            assert_eq!(
                history_lines_in(&full_day(), now, &utc),
                [
                    "Connection History · Today, Oct 16".to_string(),
                    strip(),
                    TICKS.into(),
                    "08:00  Background service started".into(),
                    "08:01  USB connected".into(),
                    "08:02  Updated: 0.9.0 | 2.2.0 → 1.0.0 | 2.2.0".into(),
                    "09:00  Sharing started".into(),
                    "09:00  The system didn't let CellGlide set DNS".into(),
                    "09:30  No original gateway found, so local network devices are reached through the phone for now".into(),
                    "10:00  Phone network unstable".into(),
                    "       Cable latency 4 ms · iPhone latency 41 ms · Packet loss 2% · Network 5G · Connection failed timeout 3, refused 1 · iPhone temperature warm".into(),
                    "10:05  Cable connection dropped (escalated)".into(),
                    "       iPhone temperature hot".into(),
                    "10:10  Recovered: Cable connection dropped · 10 min 0 sec".into(),
                    "11:00  Couldn't update the local network routes after a network change; retrying on the next change".into(),
                    "12:00  Sharing stopped".into(),
                    "12:01  USB disconnected".into(),
                    "13:00  Some connections failing".into(),
                    "13:05  Recovered: Some connections failing · 5 min 0 sec".into(),
                    format!("Available: {}", days().join(", ")),
                ]
            );
        }

        #[test]
        fn older_days_carry_a_weekday() {
            let _lang = language();
            let day = |d: &str| HistoryDay {
                day: d.into(),
                start: DayStart::default(),
                events: Vec::new(),
                days: (13..=19).map(|d| format!("2026-10-{d}")).collect(),
                begin: hm(0, 0),
                end: hm(24, 0),
            };
            cg_i18n::set(Lang::ZhCn);
            assert_eq!(day_label(&day("2026-10-14")), "10月14日 星期三");
            assert_eq!(day_label(&day("2026-10-13")), "10月13日 星期二");
            assert_eq!(day_label(&day("2026-10-19")), "今天，10月19日");
            assert_eq!(day_label(&day("2026-10-18")), "昨天，10月18日");
            cg_i18n::set(Lang::En);
            assert_eq!(day_label(&day("2026-10-14")), "Wednesday, Oct 14");
            assert_eq!(day_label(&day("2026-10-19")), "Today, Oct 19");
            assert_eq!(day_label(&day("2026-10-18")), "Yesterday, Oct 18");
            // Every weekday has a name in both languages (Sunday is 7).
            assert_eq!(day_label(&day("2026-10-11")), "Sunday, Oct 11");
        }

        #[test]
        fn spring_forward_strip_puts_noon_where_noon_is() {
            let _lang = language();
            cg_i18n::set(Lang::En);
            let la = TimeZone::get("America/Los_Angeles").unwrap();
            let date = jiff::civil::date(2026, 3, 8);
            let (begin, next) = day_bounds(date, &la).unwrap();
            // 12:00 PDT is 11 real hours after the day's midnight.
            let noon = begin + SignedDuration::from_hours(11);
            assert_eq!(
                noon.to_zoned(la.clone()).strftime("%H:%M").to_string(),
                "12:00"
            );
            let day = HistoryDay {
                day: "2026-03-08".into(),
                start: DayStart::default(),
                events: vec![HealthEvent::new(noon, EventKind::TunnelStarted)],
                days: vec![
                    "2026-03-07".into(),
                    "2026-03-08".into(),
                    "2026-03-09".into(),
                ],
                begin,
                end: next,
            };
            let lines = history_lines_in(&day, next, &la);
            let strip: Vec<char> = lines[1].chars().collect();
            assert_eq!(strip.len(), 48);
            // 11/23 * 48 = 22.96: noon falls at the very end of cell 22.
            assert!(strip[..22].iter().all(|c| *c == '·'), "{}", lines[1]);
            assert!(strip[22..].iter().all(|c| *c == '▂'), "{}", lines[1]);
            assert_eq!(lines[2].find("12"), Some(23), "{}", lines[2]);
            assert_eq!(lines[2].find('6'), Some(10));
            assert_eq!(lines[2].find("18"), Some(35));
            assert_eq!(lines[2].len(), 48 + 2);
        }

        #[test]
        fn fall_back_strip_has_25_hours_of_cells() {
            let _lang = language();
            cg_i18n::set(Lang::En);
            let la = TimeZone::get("America/Los_Angeles").unwrap();
            let (begin, next) = day_bounds(jiff::civil::date(2026, 11, 1), &la).unwrap();
            let day = HistoryDay {
                day: "2026-11-01".into(),
                start: DayStart {
                    sharing: true,
                    issue: None,
                },
                events: Vec::new(),
                days: vec!["2026-11-01".into(), "2026-11-02".into()],
                begin,
                end: next,
            };
            let lines = history_lines_in(&day, next, &la);
            assert_eq!(lines[1], "▂".repeat(48));
            // 13/25 * 48 = 24.96 rounds to column 25.
            assert_eq!(lines[2].find("12"), Some(25), "{}", lines[2]);
        }

        #[test]
        fn other_days_and_an_empty_day() {
            let _lang = language();
            let utc = TimeZone::UTC;
            let day = |d: &str| HistoryDay {
                day: d.into(),
                start: DayStart {
                    sharing: true,
                    issue: Some(IssueCause::ComputerRelay),
                },
                events: Vec::new(),
                days: days(),
                begin: hm(-24, 0),
                end: hm(0, 0),
            };
            cg_i18n::set(Lang::ZhCn);
            let lines = history_lines_in(&day("2026-10-15"), hm(40, 0), &utc);
            assert_eq!(lines[0], "连接记录 · 昨天，10月15日");
            // Sharing and an open severe issue carried over from the day before, all day.
            assert_eq!(lines[1], "▇".repeat(48));
            assert_eq!(lines[3], "这一天没有记录");
            assert_eq!(lines.len(), 5);
            assert_eq!(
                history_lines_in(&day("2026-10-14"), hm(60, 0), &utc)[0],
                "连接记录 · 10月14日 星期三"
            );
            cg_i18n::set(Lang::En);
            assert_eq!(
                history_lines_in(&day("2026-10-15"), hm(40, 0), &utc)[0],
                "Connection History · Yesterday, Oct 15"
            );
            let lines = history_lines_in(&day("2026-10-14"), hm(60, 0), &utc);
            assert_eq!(lines[0], "Connection History · Wednesday, Oct 14");
            assert_eq!(lines[3], "Nothing recorded this day");
        }

        #[test]
        fn status_shows_a_recent_drop_even_when_disconnected() {
            let _lang = language();
            cg_i18n::set(Lang::En);
            let utc = TimeZone::UTC;
            let now = hm(30, 0);
            let ended = |hours: i64| HealthStatus {
                last_issue: Some(LastIssue {
                    cause: IssueCause::Channel,
                    ended: now - jiff::SignedDuration::from_hours(hours),
                    duration: Some(45),
                }),
                ..Default::default()
            };
            let mut snapshot = Snapshot {
                state: ConnState::NoPhone,
                health: ended(2),
                ..Default::default()
            };
            let line = status_health_line(&snapshot, now, &utc).unwrap();
            assert!(line.contains("45"), "{line}");
            snapshot.health = ended(25);
            assert_eq!(status_health_line(&snapshot, now, &utc), None);
            snapshot.health = HealthStatus::default();
            assert_eq!(status_health_line(&snapshot, now, &utc), None);
            snapshot.health = HealthStatus {
                cause: Some(IssueCause::PhoneUnstable),
                since: Some(now),
                last_issue: None,
            };
            assert!(status_health_line(&snapshot, now, &utc).is_some());
            snapshot.state = ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone,
            };
            snapshot.health = HealthStatus::default();
            assert_eq!(
                status_health_line(&snapshot, now, &utc).as_deref(),
                Some("Connection OK")
            );
        }

        #[test]
        fn health_line_says_ongoing_last_drop_or_ok() {
            let _lang = language();
            let utc = TimeZone::UTC;
            let ongoing = HealthStatus {
                cause: Some(IssueCause::PhoneUnstable),
                since: Some(hm(10, 0)),
                last_issue: None,
            };
            let recovered = HealthStatus {
                last_issue: Some(LastIssue {
                    cause: IssueCause::Channel,
                    ended: hm(9, 30),
                    duration: Some(45),
                }),
                ..Default::default()
            };
            cg_i18n::set(Lang::ZhCn);
            assert_eq!(
                health_line_in(
                    &ongoing,
                    hm(10, 1) + jiff::SignedDuration::from_secs(5),
                    &utc
                ),
                "手机网络不稳 · 已持续 1 分 5 秒"
            );
            assert_eq!(
                health_line_in(&recovered, hm(10, 0), &utc),
                "连接正常 · 上次中断 09:30（45 秒）"
            );
            cg_i18n::set(Lang::En);
            assert_eq!(
                health_line_in(
                    &ongoing,
                    hm(10, 0) + jiff::SignedDuration::from_secs(9),
                    &utc
                ),
                "Phone network unstable · for 9 sec"
            );
            assert_eq!(
                health_line_in(&recovered, hm(10, 0), &utc),
                "Connection OK · last drop 09:30 (45 sec)"
            );
            // A day later it's no longer worth mentioning; an unknown duration isn't made up.
            assert_eq!(health_line_in(&recovered, hm(34, 0), &utc), "Connection OK");
            let unknown = HealthStatus {
                last_issue: Some(LastIssue {
                    duration: None,
                    ..recovered.last_issue.unwrap()
                }),
                ..Default::default()
            };
            assert_eq!(health_line_in(&unknown, hm(10, 0), &utc), "Connection OK");
            assert_eq!(
                health_line_in(&HealthStatus::default(), hm(10, 0), &utc),
                "Connection OK"
            );
        }
    }

    #[test]
    fn toggle_follows_wish() {
        let mut s = snap(ConnState::Idle);
        assert_eq!(toggle_request(&s), Request::Connect { serial: None });
        s.wants_connection = true;
        assert_eq!(toggle_request(&s), Request::Disconnect);
    }

    fn usb_with_battery(temperature: Option<f64>) -> crate::phone::UsbPhone {
        crate::phone::UsbPhone {
            battery: Some(BatteryReading {
                level: Some(50),
                temperature,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn card_text_leaves_unknown_readings_out() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        let card = PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("AT&T".into()),
                sharing: Some(true),
                download: Some(2_500_000.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let text = card_text(&card);
        assert_eq!(text.title, "AT&T");
        assert_eq!(text.carrier.as_deref(), Some("AT&T"));
        assert_eq!(text.sharing, Some(true));
        assert_eq!(text.download.as_deref(), Some("2.50 MB/s"));
        assert_eq!(text.upload, None);
        assert_eq!(text.battery, None);
        assert_eq!(text.battery_line, None);
        assert_eq!(text.heat, None);
        assert_eq!(text.radio, None);
        assert!(!text.dim);
        // A frame without a carrier name is still being detected; nothing at all is just iPhone.
        let detecting = PhoneCard {
            frame: Some(PhoneFrame::default()),
            ..Default::default()
        };
        assert_eq!(card_text(&detecting).title, "Detecting");
        assert_eq!(card_text(&detecting).carrier, None);
        assert_eq!(card_text(&PhoneCard::default()).title, "iPhone");
        assert!(!card_visible(&PhoneCard::default()));
        assert!(card_visible(&detecting));
        let usb_only = PhoneCard {
            usb: Some(usb_with_battery(None)),
            ..Default::default()
        };
        assert!(card_visible(&usb_only));
        let badge = card_text(&full_card()).battery.unwrap();
        assert_eq!(
            badge,
            BatteryBadge {
                percent: 75,
                charging: true,
                low_power: false
            }
        );
    }

    #[test]
    fn card_heat_prefers_the_battery_gauge() {
        let _lang = language();
        cg_i18n::set(Lang::En);
        let card = |temperature: Option<f64>, thermal: Option<i64>| PhoneCard {
            frame: Some(PhoneFrame {
                thermal,
                ..Default::default()
            }),
            usb: temperature.map(|t| usb_with_battery(Some(t))),
            ..Default::default()
        };
        let heat = card_text(&card(Some(45.2), Some(1))).heat.unwrap();
        assert!(heat.text.contains("45") && heat.severe, "{heat:?}");
        let warm = card_text(&card(Some(41.0), Some(3))).heat.unwrap();
        assert!(!warm.severe && warm.text.contains("41"), "{warm:?}");
        let hot = card_text(&card(None, Some(2))).heat.unwrap();
        assert!(!hot.severe);
        assert!(card_text(&card(None, Some(3))).heat.unwrap().severe);
        assert_eq!(card_text(&card(None, Some(1))).heat, None);
    }

    #[test]
    fn stale_frame_dims_only_without_usb_battery() {
        let frame = Some(PhoneFrame::default());
        let dim = |stale: bool, frame: Option<PhoneFrame>, usb: Option<crate::phone::UsbPhone>| {
            card_text(&PhoneCard {
                frame,
                stale,
                usb,
                ..Default::default()
            })
            .dim
        };
        assert!(dim(true, frame.clone(), None));
        assert!(!dim(true, frame.clone(), Some(usb_with_battery(None))));
        assert!(!dim(false, frame, None));
        assert!(!dim(true, None, None));
    }

    #[test]
    fn every_state_has_title_and_detail() {
        let _lang = language();
        let states = [
            ConnState::NoUsbService,
            ConnState::NoPhone,
            ConnState::ChoosePhone,
            ConnState::Idle,
            ConnState::Connecting,
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone,
            },
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Public,
            },
            ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Unmanaged,
            },
            ConnState::Retrying {
                reason: Reason::NoExit,
            },
        ];
        for lang in [Lang::ZhCn, Lang::En] {
            cg_i18n::set(lang);
            for state in &states {
                let text = gui_state(&snap(state.clone()));
                for part in [&text.title, &text.detail] {
                    assert!(
                        !part.is_empty() && !part.contains("gui.state") && !part.contains("%{"),
                        "{state:?}: {part}"
                    );
                }
            }
        }
        cg_i18n::set(Lang::En);
        let choose = gui_state(&snap(ConnState::ChoosePhone));
        assert_eq!(choose.title, "Several iPhones connected");
    }

    #[test]
    fn snapshot_items_follow_the_mac_order_and_skip_unknowns() {
        let _lang = language();
        cg_i18n::set(Lang::ZhCn);
        let snapshot = HealthSnapshot {
            channel_rtt: Some(12),
            radio: Some("5G".into()),
            connects_failed: Some([("timeout".into(), 3), ("dns".into(), 1)].into()),
            ..Default::default()
        };
        let items = snapshot_items(&snapshot);
        let names: Vec<_> = items.iter().map(|(n, _)| n.as_str()).collect();
        let values: Vec<_> = items.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(names, ["数据线延迟", "网络", "连接失败", "主要原因"]);
        assert_eq!(values, ["12 ms", "5G", "4", "超时"]);
        assert!(snapshot_items(&HealthSnapshot::default()).is_empty());
    }

    #[test]
    fn event_tones() {
        let at = |kind, cause, phase| {
            let mut e = HealthEvent::new(Timestamp::UNIX_EPOCH, kind);
            e.cause = cause;
            e.phase = phase;
            e
        };
        let issue = |cause, phase| event_tone(&at(EventKind::Issue, Some(cause), Some(phase)));
        assert_eq!(
            issue(IssueCause::PhoneNetwork, EventPhase::End),
            EventTone::Green
        );
        assert_eq!(
            issue(IssueCause::PhoneNetwork, EventPhase::Start),
            EventTone::Red
        );
        assert_eq!(
            issue(IssueCause::PhoneUnstable, EventPhase::Start),
            EventTone::Orange
        );
        assert_eq!(
            issue(IssueCause::PhoneUnstable, EventPhase::Escalate),
            EventTone::Orange
        );
        assert_eq!(
            event_tone(&at(EventKind::TunnelStarted, None, None)),
            EventTone::Green
        );
        assert_eq!(
            event_tone(&at(EventKind::UsbAttached, None, None)),
            EventTone::Gray
        );
        assert_eq!(
            event_tone(&at(EventKind::Unknown, None, None)),
            EventTone::Gray
        );
    }

    #[test]
    fn device_labels() {
        use crate::phone::PhoneLabel;
        let _lang = language();
        let mut s = Snapshot::default();
        s.labels.insert(
            "a".into(),
            PhoneLabel {
                name: Some("Kiko's iPhone".into()),
                model: Some("iPhone18,4".into()),
            },
        );
        s.labels.insert(
            "b".into(),
            PhoneLabel {
                name: None,
                model: Some("iPhone18,4".into()),
            },
        );
        assert_eq!(device_label(&s, "a"), "Kiko's iPhone · iPhone Air");
        assert_eq!(device_label(&s, "b"), "iPhone Air");
        assert_eq!(device_label(&s, "00008110-001A2B"), "00008110");
    }
}
