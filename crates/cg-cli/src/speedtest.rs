//! `cellglide speedtest`: what the terminal prints. Which legs run, the words and the history
//! entry are `cg_tools::speed`; this file is the terminal side.

use cg_i18n::tr;
use cg_tools::speed::session::Leg;
use cg_tools::speed::text::{direction, error_text, numbers, verdict_line};
use cg_tools::speed::{Note, Plan, Record, Stage};

/// What to print before the first leg starts: that only Wi‑Fi or Ethernet is measured when no
/// phone is connected, or why the comparison cannot run.
pub fn intro_lines(plan: &Plan) -> Vec<String> {
    let mut lines = Vec::new();
    if !plan.phone && plan.wifi.is_some() {
        lines.push(tr("speed.wifi_only", &[]));
    }
    if let Some(reason) = &plan.wifi_unavailable {
        lines.push(error_text(reason));
    }
    lines
}

/// What a leg printed once it ended: its name, then the numbers and notes or the failure.
pub fn leg_lines(leg: &Leg) -> Vec<String> {
    let mut lines = vec![leg.label.clone()];
    match &leg.stage {
        Stage::Done => {
            let r = &leg.result;
            lines.push(format!("  {}", numbers(r.latency, r.download, r.upload)));
            for Note::CapReached { uploading, seconds } in &r.notes {
                lines.push(format!(
                    "  {}",
                    tr(
                        "speed.cap_note",
                        &[
                            ("direction", &direction(*uploading)),
                            ("seconds", &format!("{seconds:.1}")),
                        ]
                    )
                ));
            }
        }
        Stage::Failed(error) => lines.push(format!("  {}", error_text(error))),
        _ => {}
    }
    lines
}

/// Whether the answer to the data warning is a yes: only a line of `y` or `Y` is; end of input,
/// "yes", "n" and an empty line are not.
pub fn confirmed(input: &mut impl std::io::BufRead) -> bool {
    let mut answer = String::new();
    input.read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y")
}

/// The saved results, newest first, `zone` being the time zone dates are shown in.
pub fn history_lines(records: &[Record], zone: &jiff::tz::TimeZone) -> Vec<String> {
    if records.is_empty() {
        return vec![tr("speed.history_empty", &[])];
    }
    let mut lines = Vec::new();
    for record in records {
        let mut head = vec![
            record
                .date
                .to_zoned(zone.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string(),
        ];
        head.extend(record.carrier.clone());
        head.extend(record.generation.clone());
        lines.push(head.join(" · "));
        // A leg whose numbers are all missing has nothing to show.
        for (name, leg) in [
            ("speed.phone", &record.phone),
            ("speed.wifi_short", &record.wifi),
        ] {
            if let Some(s) = leg {
                let text = numbers(s.latency, s.download, s.upload);
                if !text.is_empty() {
                    lines.push(format!("  {}  {text}", tr(name, &[])));
                }
            }
        }
        if let (Some(p), Some(w)) = (&record.phone, &record.wifi)
            && let Some(line) = verdict_line(p.download, w.download)
        {
            lines.push(format!("  {line}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_tools::speed::session::{Run, history_record};
    use cg_tools::speed::{Route, SpeedResult, plan};
    use jiff::Timestamp;
    use std::sync::MutexGuard;

    fn language(lang: cg_i18n::Lang) -> MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(lang);
        guard
    }

    fn finished(download: f64) -> SpeedResult {
        SpeedResult {
            latency: Some(40.0),
            download: Some(download),
            upload: Some(download / 4.0),
            ..SpeedResult::default()
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_second(1_791_000_000).unwrap()
    }

    /// A measurer that reports Done with canned numbers and logs the order of starts and ends.
    #[test]
    fn wifi_only_plan_says_so() {
        let _lang = language(cg_i18n::Lang::En);
        let wifi_only = Plan {
            phone: false,
            wifi: Some("wlan0".into()),
            wifi_unavailable: None,
        };
        assert_eq!(intro_lines(&wifi_only), vec![tr("speed.wifi_only", &[])]);
        assert!(intro_lines(&wifi_only)[0].contains("Wi‑Fi or Ethernet only"));
        let both = plan(true, Some("wlan0".into()), true).unwrap();
        assert!(intro_lines(&both).is_empty());
        let no_uplink = plan(true, None, true).unwrap();
        assert_eq!(
            intro_lines(&no_uplink),
            vec![error_text(no_uplink.wifi_unavailable.as_ref().unwrap())]
        );
    }

    #[test]
    fn leg_lines_show_one_decimal_and_leave_gaps_out() {
        let _lang = language(cg_i18n::Lang::En);
        let leg = Leg {
            route: Route::Phone,
            label: "iPhone".into(),
            stage: Stage::Done,
            result: SpeedResult {
                latency: Some(40.4),
                download: Some(20.0),
                upload: None,
                notes: vec![Note::CapReached {
                    uploading: true,
                    seconds: 3.25,
                }],
                ..SpeedResult::default()
            },
        };
        let lines = leg_lines(&leg);
        assert_eq!(lines[0], "iPhone");
        assert_eq!(lines[1], "  Latency 40 ms · Download 20.0 Mbps");
        assert!(
            lines[2].contains("Upload") && lines[2].contains("3.2"),
            "{lines:?}"
        );
        assert!(!lines[1].contains("Upload"), "{lines:?}");
    }

    #[test]
    fn history_rows_and_empty() {
        let _lang = language(cg_i18n::Lang::En);
        let utc = jiff::tz::TimeZone::UTC;
        assert_eq!(history_lines(&[], &utc), ["No speed tests yet"]);
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let run = Run {
            legs: vec![
                Leg {
                    route: Route::Phone,
                    label: String::new(),
                    stage: Stage::Done,
                    result: finished(20.0),
                },
                Leg {
                    route: Route::Interface("wlp2s0".into()),
                    label: String::new(),
                    stage: Stage::Done,
                    result: finished(60.0),
                },
            ],
            cancelled: false,
        };
        let rec = history_record(
            &plan,
            &run,
            now(),
            Some("Carrier".into()),
            Some("5G".into()),
        )
        .unwrap();
        let lines = history_lines(&[rec], &utc);
        assert_eq!(lines[0], "2026-10-03 04:00 · Carrier · 5G");
        assert_eq!(
            lines[1],
            "  iPhone  Latency 40 ms · Download 20.0 Mbps · Upload 5.0 Mbps"
        );
        assert!(
            lines[2].starts_with("  Wi‑Fi or Ethernet  Latency 40 ms"),
            "{lines:?}"
        );
        assert_eq!(lines[3], "  Wi‑Fi download is 3.0x faster than the iPhone.");
    }

    #[test]
    fn only_y_confirms_the_data_warning() {
        use std::io::Cursor;
        assert!(!confirmed(&mut Cursor::new("")), "end of input");
        assert!(confirmed(&mut Cursor::new("y\n")));
        assert!(confirmed(&mut Cursor::new("Y\n")));
        assert!(!confirmed(&mut Cursor::new("yes\n")));
        assert!(!confirmed(&mut Cursor::new("n\n")));
        assert!(!confirmed(&mut Cursor::new("\n")), "empty line");
    }

    #[test]
    fn history_skips_a_leg_with_no_numbers() {
        let _lang = language(cg_i18n::Lang::En);
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let run = Run {
            legs: vec![Leg {
                route: Route::Phone,
                label: String::new(),
                stage: Stage::Done,
                result: finished(20.0),
            }],
            cancelled: false,
        };
        let mut record = history_record(&plan, &run, now(), None, None).unwrap();
        record.wifi = Some(cg_tools::speed::Summary {
            latency: None,
            download: None,
            upload: None,
        });
        let lines = history_lines(&[record], &jiff::tz::TimeZone::UTC);
        assert_eq!(lines.len(), 2, "{lines:?}"); // date line, phone row
        assert!(lines[1].starts_with("  iPhone"), "{lines:?}");
    }
}
