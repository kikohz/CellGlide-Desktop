//! One value of every command and event, for the contract fixtures in `tests/fixtures/` that
//! the C# side reads too. Views come from the real view functions at a fixed time in UTC, so
//! call these with the language set to English. Adding a variant (of `Command`, `Event`,
//! `cg_ipc::Request` or `ReplyValue`) fails to compile until the matches below, and so the
//! samples, name it.

use super::{Command, Event, ReplyResult, ReplyValue, SpeedCaps};
use crate::testing::{SERIAL, connected_snapshot, disconnected_snapshot, middle_day};
use crate::views::{
    self, ChromeView, LeakOutcome, MenuKind, SpeedDialog, SpeedLeg, SpeedPage, SpeedView,
};
use cg_i18n::tr;
use cg_ipc::describe::ui::{DownReason, ServiceDown};
use cg_ipc::health::{EventKind, HealthEvent, HistoryDay, IssueCause};
use cg_ipc::phone::{BatteryReading, SpeedPoint, UsbPhone};
use cg_ipc::{Check, CheckId, Contact, DiagRange, DnsMode, PhoneChoice, Request, StrictDns};
use cg_tools::ToolError;
use cg_tools::dns_leak::{Report, judge};
use cg_tools::prefs::Prefs;
use cg_tools::speed::{Note, Progress, Record, SpeedError, SpeedResult, Stage};
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// What the tray menu's first line says in every sample, whatever this build's version, so
/// that a version bump leaves the fixtures alone.
const VERSION_LINE: &str = "CellGlide 1.0.0";

/// Every command, at least once; the name is the fixture's file name.
pub fn command_samples() -> Vec<(&'static str, Command)> {
    let request = |request| Command::Request { request };
    let samples = vec![
        ("subscribe", Command::Subscribe),
        ("request_hello", request(Request::Hello)),
        ("request_status", request(Request::Status)),
        (
            "request_connect",
            request(Request::Connect {
                serial: Some(SERIAL.into()),
            }),
        ),
        (
            "request_connect_any",
            request(Request::Connect { serial: None }),
        ),
        ("request_disconnect", request(Request::Disconnect)),
        (
            "request_set_settings",
            request(Request::SetSettings {
                auto_connect: Some(false),
                phone: Some(PhoneChoice::Serial {
                    serial: SERIAL.into(),
                }),
                strict_dns: Some(true),
            }),
        ),
        (
            "request_set_settings_strict_dns",
            request(Request::SetSettings {
                auto_connect: None,
                phone: None,
                strict_dns: Some(false),
            }),
        ),
        ("request_doctor", request(Request::Doctor)),
        ("request_subscribe", request(Request::Subscribe)),
        (
            "request_history",
            request(Request::History {
                day: Some("2026-10-27".into()),
            }),
        ),
        (
            "request_history_today",
            request(Request::History { day: None }),
        ),
        (
            "request_diagnostics",
            request(Request::Diagnostics {
                range: DiagRange::Week,
                contact: Some(Contact {
                    email: Some("someone@example.com".into()),
                    note: Some("The connection drops every few minutes.".into()),
                }),
            }),
        ),
        (
            "request_diagnostics_email_only",
            request(Request::Diagnostics {
                range: DiagRange::Day,
                contact: Some(Contact {
                    email: Some("someone@example.com".into()),
                    note: None,
                }),
            }),
        ),
        (
            "request_diagnostics_no_contact",
            request(Request::Diagnostics {
                range: DiagRange::Day,
                contact: None,
            }),
        ),
        ("toggle", Command::Toggle),
        (
            "speed_start",
            Command::SpeedStart {
                include_wifi: true,
                confirmed: false,
                caps: None,
            },
        ),
        (
            "speed_start_small",
            Command::SpeedStart {
                include_wifi: false,
                confirmed: true,
                caps: Some(SpeedCaps {
                    download_bytes: 20_000_000,
                    upload_bytes: 10_000_000,
                    seconds: 10,
                }),
            },
        ),
        ("speed_cancel", Command::SpeedCancel),
        ("speed_history_clear", Command::SpeedHistoryClear),
        ("leak_start", Command::LeakStart),
        ("leak_cancel", Command::LeakCancel),
        (
            "diagnostics_prepare",
            Command::DiagnosticsPrepare {
                range: DiagRange::Day,
                email: "someone@example.com".into(),
                note: "Slow since this morning.".into(),
            },
        ),
        (
            "diagnostics_send",
            Command::DiagnosticsSend {
                json: DIAGNOSTICS_JSON.into(),
            },
        ),
        (
            "diagnostics_save",
            Command::DiagnosticsSave {
                json: DIAGNOSTICS_JSON.into(),
                path: None,
            },
        ),
        (
            "diagnostics_save_to",
            Command::DiagnosticsSave {
                json: DIAGNOSTICS_JSON.into(),
                path: Some(r"D:\Reports\cellglide-diagnostics-20261030-120000.json".into()),
            },
        ),
        (
            "set_lang",
            Command::SetLang {
                lang: "zh-CN".into(),
            },
        ),
        (
            "set_lang_system",
            Command::SetLang {
                lang: String::new(),
            },
        ),
        ("get_prefs", Command::GetPrefs),
        ("set_prefs", Command::SetPrefs { prefs: prefs() }),
    ];
    for (_, command) in &samples {
        match command {
            Command::Request {
                request:
                    Request::Hello
                    | Request::Status
                    | Request::Connect { .. }
                    | Request::Disconnect
                    | Request::SetSettings { .. }
                    | Request::Doctor
                    | Request::Subscribe
                    | Request::History { .. }
                    | Request::Diagnostics { .. },
            }
            | Command::Subscribe
            | Command::Toggle
            | Command::SpeedStart { .. }
            | Command::SpeedCancel
            | Command::SpeedHistoryClear
            | Command::LeakStart
            | Command::LeakCancel
            | Command::DiagnosticsPrepare { .. }
            | Command::DiagnosticsSend { .. }
            | Command::DiagnosticsSave { .. }
            | Command::SetLang { .. }
            | Command::GetPrefs
            | Command::SetPrefs { .. } => {}
        }
    }
    samples
}

/// Every event, at least once, with every `ReplyValue` and an `Err`; the name is the
/// fixture's file name.
pub fn event_samples() -> Vec<(&'static str, Event)> {
    let reply = |id, value| Event::Reply {
        id,
        result: ReplyResult::Ok { value },
    };
    let state = |snapshot| Event::State {
        view: Box::new(overview(&snapshot)),
    };
    let speed_progress = |view| Event::SpeedProgress { view };
    let leak_progress = |view| Event::LeakProgress { view };
    let samples =
        vec![
        ("reply_done", reply(1, ReplyValue::Done)),
        (
            "reply_checks",
            reply(
                2,
                ReplyValue::Checks {
                    rows: views::checks(&checks()),
                },
            ),
        ),
        (
            "reply_history",
            reply(
                3,
                ReplyValue::History {
                    view: history(&every_event_kind()),
                },
            ),
        ),
        (
            "reply_history_today",
            reply(
                4,
                ReplyValue::History {
                    view: history(&today()),
                },
            ),
        ),
        (
            "reply_diagnostics",
            reply(
                5,
                ReplyValue::Diagnostics {
                    json: DIAGNOSTICS_JSON.into(),
                    file_name: "cellglide-diagnostics-20261030-120000.json".into(),
                },
            ),
        ),
        (
            "reply_sent",
            reply(
                6,
                ReplyValue::Sent {
                    code: "CG-7K2Q9".into(),
                },
            ),
        ),
        (
            "reply_saved",
            reply(
                7,
                ReplyValue::Saved {
                    path: r"C:\Users\someone\Downloads\cellglide-diagnostics-20261030-120000-2.json"
                        .into(),
                },
            ),
        ),
        ("reply_prefs", reply(8, ReplyValue::Prefs { prefs: prefs() })),
        (
            "reply_err_already_running",
            Event::Reply {
                id: 9,
                result: ReplyResult::err(tr("speed.already_running", &[])),
            },
        ),
        ("state_connected", state(everything_connected())),
        ("state_disconnected", state(disconnected_snapshot())),
        (
            "state_strict_dns_disabled",
            state(with_strict_dns(StrictDns::Disabled)),
        ),
        (
            "state_strict_dns_off",
            state(with_strict_dns(StrictDns::Off {
                reason: "SetInterfaceDnsSettings failed".into(),
            })),
        ),
        ("service_gone_stopped", gone(DownReason::Gone)),
        ("service_gone_not_in_group", gone(DownReason::NotInGroup)),
        (
            "phone_plugged",
            Event::PhonePlugged {
                serial: SERIAL.into(),
            },
        ),
        (
            "speed_progress_latency",
            speed_progress(speed(
                true,
                None,
                &[
                    leg(true, Stage::Latency, Results::None),
                    leg(false, Stage::Idle, Results::None),
                ],
                &[],
            )),
        ),
        (
            "speed_progress_confirming",
            speed_progress(speed(
                true,
                None,
                &[
                    confirming(leg(true, Stage::Upload, Results::Partial)),
                    leg(false, Stage::Idle, Results::None),
                ],
                &[],
            )),
        ),
        (
            "speed_progress_download",
            speed_progress(speed(
                true,
                None,
                &[
                    leg(true, Stage::Done, Results::Phone),
                    leg(false, Stage::Download, Results::Partial),
                ],
                &[],
            )),
        ),
        (
            "speed_progress_done",
            speed_progress(speed(
                false,
                None,
                &[
                    leg(true, Stage::Done, Results::Phone),
                    leg(false, Stage::Done, Results::Wifi),
                ],
                &records(),
            )),
        ),
        (
            "speed_progress_failed",
            speed_progress(speed(
                false,
                None,
                &[
                    leg(true, Stage::Failed(SpeedError::Timeout), Results::None),
                    leg(false, Stage::Cancelled, Results::Partial),
                ],
                &records(),
            )),
        ),
        (
            "speed_progress_data_warning",
            speed_progress(speed(false, Some(SpeedDialog::DataWarning), &[], &[])),
        ),
        (
            "speed_progress_not_connected",
            speed_progress(views::speed(
                &SpeedPage {
                    running: false,
                    dialog: None,
                    phone: false,
                    uplink: None,
                    include_wifi: true,
                    legs: &[],
                    records: &[],
                    history_failed: true,
                },
                &TimeZone::UTC,
            )),
        ),
        ("leak_progress_running", leak_progress(views::leak(true, None, None))),
        (
            "leak_progress_result",
            leak_progress(views::leak(
                false,
                Some(&leak_result()),
                Some(DnsMode::Unmanaged),
            )),
        ),
        (
            "leak_progress_failed",
            leak_progress(views::leak(
                false,
                Some(&Err(ToolError::Status(503))),
                Some(DnsMode::Phone),
            )),
        ),
        ("prefs", Event::Prefs { prefs: prefs() }),
        (
            "fault",
            Event::Fault {
                message: "speed test: index out of bounds: the len is 2 but the index is 2".into(),
            },
        ),
    ];
    for (_, event) in &samples {
        match event {
            Event::Reply {
                result:
                    ReplyResult::Ok {
                        value:
                            ReplyValue::Done
                            | ReplyValue::Checks { .. }
                            | ReplyValue::History { .. }
                            | ReplyValue::Diagnostics { .. }
                            | ReplyValue::Sent { .. }
                            | ReplyValue::Saved { .. }
                            | ReplyValue::Prefs { .. },
                    }
                    | ReplyResult::Err { .. },
                ..
            }
            | Event::State { .. }
            | Event::ServiceGone { .. }
            | Event::PhonePlugged { .. }
            | Event::SpeedProgress { .. }
            | Event::LeakProgress { .. }
            | Event::Prefs { .. }
            | Event::Fault { .. } => {}
        }
    }
    samples
}

/// A redacted bundle as the service hands it over: opaque to the app.
const DIAGNOSTICS_JSON: &str = r#"{"format":1,"platform":"windows","log":[]}"#;

/// The samples' clock: noon of the last day `middle_day` has on record.
fn now() -> Timestamp {
    "2026-10-30T12:00:00Z".parse().expect("a timestamp")
}

fn prefs() -> Prefs {
    Prefs {
        speed_confirmed: true,
        include_wifi: false,
        open_on_plug: true,
    }
}

fn fixed_version(mut chrome: ChromeView) -> ChromeView {
    for item in &mut chrome.menu {
        if item.kind == MenuKind::Version {
            item.label = VERSION_LINE.into();
        }
    }
    chrome
}

fn overview(snapshot: &cg_ipc::Snapshot) -> views::OverviewView {
    let mut view = views::overview(snapshot, now(), &TimeZone::UTC);
    view.chrome = fixed_version(view.chrome);
    view
}

fn gone(reason: DownReason) -> Event {
    match crate::subscription::gone(&ServiceDown::new(reason)) {
        Event::ServiceGone {
            text,
            not_in_group,
            chrome,
        } => Event::ServiceGone {
            text,
            not_in_group,
            chrome: fixed_version(chrome),
        },
        other => other,
    }
}

/// Connected with every optional part of the overview filled in: an open issue, strict DNS,
/// a phone card with a model, battery, heat and trail, a remembered phone that isn't plugged
/// in, and the proxy.
fn everything_connected() -> cg_ipc::Snapshot {
    let mut s = connected_snapshot();
    s.health.cause = Some(IssueCause::PhoneNetwork);
    s.health.since = Some(now() - jiff::SignedDuration::from_mins(3));
    s.settings.chosen_serial = Some("00008110-000000000000001A".into());
    s.strict_dns = StrictDns::On;
    let card = s.phone.as_mut().expect("a phone card");
    card.trail = vec![
        Some(SpeedPoint {
            download: 2_100_000.5,
            upload: 380_000.25,
        }),
        Some(SpeedPoint {
            download: 2_500_000.0,
            upload: 400_000.0,
        }),
        None,
        Some(SpeedPoint {
            download: 1_250_000.0,
            upload: 120_000.5,
        }),
    ];
    card.usb = Some(UsbPhone {
        model: Some("iPhone17,5".into()),
        battery: Some(BatteryReading {
            level: Some(80),
            charging: true,
            external_connected: true,
            watts: Some(18.5),
            adapter_watts: Some(20),
            temperature: Some(46.4),
            health: Some(0.93),
            cycles: Some(212),
            ..BatteryReading::default()
        }),
        ..UsbPhone::default()
    });
    let frame = card.frame.as_mut().expect("a frame");
    frame.battery = Some(0.8);
    frame.charging = Some(true);
    frame.low_power = Some(true);
    frame.thermal = Some(3);
    s
}

fn with_strict_dns(strict_dns: StrictDns) -> cg_ipc::Snapshot {
    cg_ipc::Snapshot {
        strict_dns,
        ..connected_snapshot()
    }
}

/// A pass, a failure with its fix, and a check that couldn't be made. Windows checks whose
/// text has no `_windows` variant, so the fixture reads the same on Linux.
fn checks() -> Vec<Check> {
    vec![
        Check::new(CheckId::AppleService, true).with("kind", "store"),
        Check::new(CheckId::Wintun, false),
        Check::new(CheckId::Dns, false)
            .with("cause", "unreadable")
            .with("error", "access denied"),
    ]
}

fn history(day: &HistoryDay) -> views::HistoryView {
    views::history(day, day.end, &TimeZone::UTC)
}

/// `middle_day` with one more event of every kind it lacks, in the afternoon.
fn every_event_kind() -> HistoryDay {
    let mut day = middle_day();
    let afternoon = day.begin + jiff::SignedDuration::from_hours(13);
    let more = [
        EventKind::UsbDetached,
        EventKind::VersionChanged,
        EventKind::BypassUnavailable,
        EventKind::BypassUpdateFailed,
        EventKind::DnsNotAuthorized,
        EventKind::StrictDnsUnavailable,
        EventKind::Unknown,
    ];
    for (minutes, kind) in (0i64..).map(|n| n * 5).zip(more) {
        let detail = (kind == EventKind::VersionChanged).then(|| "1.0.0 → 1.0.1".to_string());
        day.events.push(HealthEvent {
            detail,
            ..HealthEvent::new(afternoon + jiff::SignedDuration::from_mins(minutes), kind)
        });
    }
    for event in &day.events {
        match event.kind {
            EventKind::Issue
            | EventKind::TunnelStarted
            | EventKind::TunnelStopped
            | EventKind::UsbAttached
            | EventKind::UsbDetached
            | EventKind::VersionChanged
            | EventKind::ServiceStarted
            | EventKind::BypassUnavailable
            | EventKind::BypassUpdateFailed
            | EventKind::DnsNotAuthorized
            | EventKind::StrictDnsUnavailable
            | EventKind::Unknown => {}
        }
    }
    day
}

/// The last of the seven days, with nothing on record: no day after it.
fn today() -> HistoryDay {
    let mut day = middle_day();
    let shift = jiff::SignedDuration::from_hours(24 * 3);
    day.day = day.days[6].clone();
    day.begin += shift;
    day.end += shift;
    day.events.clear();
    day
}

/// How much of a leg's result is known.
#[derive(Clone, Copy)]
enum Results {
    None,
    /// Latency and some of the download.
    Partial,
    Phone,
    Wifi,
}

fn leg(phone: bool, stage: Stage, results: Results) -> SpeedLeg {
    let (latency, download, upload, series, notes, progress) = match results {
        Results::None => (None, None, None, vec![], vec![], 0.0),
        Results::Partial => (
            Some(38.5),
            Some(87.3),
            None,
            vec![12.5, 64.25, 87.3],
            vec![],
            0.45,
        ),
        Results::Phone => (
            Some(41.5),
            Some(195.4),
            Some(38.7),
            vec![20.5, 150.25, 195.4, 188.75],
            vec![Note::CapReached {
                uploading: true,
                seconds: 6.5,
            }],
            1.0,
        ),
        Results::Wifi => (
            Some(12.5),
            Some(310.6),
            None,
            vec![120.5, 290.25, 310.6],
            vec![Note::UploadTooSlow],
            1.0,
        ),
    };
    SpeedLeg {
        label: if phone { "iPhone" } else { "Wi-Fi" }.into(),
        phone,
        progress: Progress {
            stage,
            progress,
            elapsed: 4.5,
            result: SpeedResult {
                latency,
                download,
                upload,
                download_series: series,
                notes,
            },
            payload_bytes: 12_345_678,
            waiting: false,
        },
    }
}

/// Upload's time is up and the leg waits for the server's confirmations.
fn confirming(mut leg: SpeedLeg) -> SpeedLeg {
    leg.progress.waiting = true;
    leg.progress.progress = 1.0;
    leg.progress.elapsed = 10.0;
    leg
}

fn speed(
    running: bool,
    dialog: Option<SpeedDialog>,
    legs: &[SpeedLeg],
    records: &[Record],
) -> SpeedView {
    views::speed(
        &SpeedPage {
            running,
            dialog,
            phone: true,
            uplink: Some("Wi-Fi"),
            include_wifi: true,
            legs,
            records,
            history_failed: false,
        },
        &TimeZone::UTC,
    )
}

/// Two recent results: both routes with the network named, and the phone alone with nothing
/// known of its network.
fn records() -> Vec<Record> {
    let result = |download, upload, latency| SpeedResult {
        download: Some(download),
        upload: Some(upload),
        latency: Some(latency),
        ..SpeedResult::default()
    };
    let phone = result(195.4, 38.7, 41.5);
    let wifi = result(310.6, 95.2, 12.5);
    let both = cg_tools::speed::record(
        Some(&phone),
        Some(&wifi),
        true,
        "2026-10-30T08:09:10Z".parse().expect("a timestamp"),
        Some("AT&T".into()),
        Some("5G".into()),
    );
    let alone = cg_tools::speed::record(
        Some(&phone),
        None,
        true,
        "2026-10-29T21:30:00Z".parse().expect("a timestamp"),
        None,
        None,
    );
    [both, alone].into_iter().flatten().collect()
}

/// Two resolvers of one provider, one passing on the client subnet, and one of an unknown
/// network with no location or query count.
fn leak_result() -> LeakOutcome {
    let report: Report = serde_json::from_str(
        r#"{"you":{"ip":"1.2.3.4","org":"AT&T Enterprises, LLC","country":"US","region":"California"},
            "resolvers":[{"ip":"9.9.9.9","asn":19281,"org":"Quad9","country":"US","queries":3,"subnet":"1.2.3.0/24"},
                         {"ip":"9.9.9.10","asn":19281,"org":"Quad9","country":"US","region":"California","queries":2},
                         {"ip":"203.0.113.7"}]}"#,
    )
    .expect("a leak report");
    let verdict = judge(&report, Some(DnsMode::Unmanaged));
    Ok((report, verdict))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_snake_case() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let names: Vec<&str> = command_samples()
            .into_iter()
            .map(|(n, _)| n)
            .chain(event_samples().into_iter().map(|(n, _)| n))
            .collect();
        for name in &names {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{name}"
            );
        }
        let commands: std::collections::BTreeSet<_> =
            command_samples().into_iter().map(|(n, _)| n).collect();
        let events: std::collections::BTreeSet<_> =
            event_samples().into_iter().map(|(n, _)| n).collect();
        assert_eq!(commands.len(), command_samples().len());
        assert_eq!(events.len(), event_samples().len());
    }

    #[test]
    fn the_samples_show_the_newer_shapes() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let events = event_samples();
        let json = serde_json::to_string(&events.iter().map(|(_, e)| e).collect::<Vec<_>>())
            .expect("JSON");
        for text in [
            tr("speed.stage.confirming", &[]),
            tr("speed.already_running", &[]),
            tr("state.strict_dns_on", &[]),
            tr("state.strict_dns_off", &[]),
            tr("state.strict_dns_disabled", &[]),
            tr(
                "history.event.version_changed",
                &[("detail", "1.0.0 → 1.0.1")],
            ),
        ] {
            assert!(
                json.contains(&serde_json::to_string(&text).expect("JSON")),
                "{text}"
            );
        }
        for state in ["\"pass\"", "\"fail\"", "\"unknown\""] {
            assert!(json.contains(&format!("\"state\":{state}")), "{state}");
        }
        assert!(json.contains("\"waiting\":true"));
        // Every tray menu's first line is the fixed one, whatever this build's version.
        let chromes = events.iter().filter_map(|(_, e)| match e {
            Event::State { view } => Some(&view.chrome),
            Event::ServiceGone { chrome, .. } => Some(chrome),
            _ => None,
        });
        let mut count = 0;
        for chrome in chromes {
            assert_eq!(chrome.menu[0].kind, MenuKind::Version);
            assert_eq!(chrome.menu[0].label, VERSION_LINE);
            count += 1;
        }
        assert!(count >= 2);
    }
}
