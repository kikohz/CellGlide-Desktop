//! The words for a speed test: failures, the comparison, progress and numbers. Shared by the
//! terminal and the window.

use super::{Note, Progress, SpeedError, Stage, Summary, Verdict, verdict};
use cg_i18n::tr;

/// The text key of a failure; the details (`minutes`, `code`, `detail`) are filled in by
/// [`error_text`].
fn error_key(error: &SpeedError) -> &'static str {
    match error {
        SpeedError::NoInterface(_) => "speed.error.no_interface",
        SpeedError::NotConnected => "speed.error.not_connected",
        SpeedError::RateLimited { .. } => "speed.error.rate_limited",
        SpeedError::Refused(_) => "speed.error.refused",
        SpeedError::Timeout => "speed.error.timeout",
        SpeedError::ClosedEarly => "speed.error.closed_early",
        SpeedError::BadLength => "speed.error.bad_length",
        SpeedError::Chunked => "speed.error.chunked",
        SpeedError::BadReply => "speed.error.bad_reply",
        SpeedError::Io(_) => "speed.error.io",
    }
}

/// The user's words for a failure.
pub fn error_text(error: &SpeedError) -> String {
    let key = error_key(error);
    match error {
        SpeedError::RateLimited { minutes } => tr(key, &[("minutes", &minutes.to_string())]),
        SpeedError::Refused(code) => tr(key, &[("code", &code.to_string())]),
        SpeedError::Io(detail) => tr(key, &[("detail", detail)]),
        _ => tr(key, &[]),
    }
}

pub fn direction(uploading: bool) -> String {
    tr(
        if uploading {
            "speed.upload"
        } else {
            "speed.download"
        },
        &[],
    )
}

/// "Latency 40 ms · Download 20.0 Mbps · Upload 5.0 Mbps"; missing numbers are left out.
pub fn numbers(latency: Option<f64>, download: Option<f64>, upload: Option<f64>) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = latency {
        parts.push(format!("{} {ms:.0} ms", tr("speed.latency", &[])));
    }
    if let Some(mbps) = download {
        parts.push(format!("{} {mbps:.1} Mbps", tr("speed.download", &[])));
    }
    if let Some(mbps) = upload {
        parts.push(format!("{} {mbps:.1} Mbps", tr("speed.upload", &[])));
    }
    parts.join(" · ")
}

/// The comparison sentence, when both downloads are known.
pub fn verdict_line(phone: Option<f64>, wifi: Option<f64>) -> Option<String> {
    Some(match verdict(phone, wifi)? {
        Verdict::WifiFaster { ratio } => {
            tr("speed.wifi_faster", &[("ratio", &format!("{ratio:.1}"))])
        }
        Verdict::PhoneFaster { ratio } => {
            tr("speed.phone_faster", &[("ratio", &format!("{ratio:.1}"))])
        }
        Verdict::Similar => tr("speed.similar", &[]),
    })
}

/// "Testing Download… 3 / 5 s": `elapsed` against the direction's own `duration`.
fn transfer_text(progress: &Progress, uploading: bool) -> String {
    // `max` turns a NaN (or a negative) into 0: `clamp` would panic on a NaN bound, and the
    // text would read "NaN".
    let total = progress.duration.max(0.0);
    let seconds = format!("{:.0}", progress.elapsed.max(0.0).min(total));
    tr(
        "speed.stage.transfer",
        &[
            ("direction", &direction(uploading)),
            ("seconds", &seconds),
            ("total", &format!("{total:.0}")),
        ],
    )
}

/// The one line that is rewritten while a leg runs: the leg, its stage, progress and the
/// current rate.
pub fn progress_line(label: &str, progress: &Progress) -> Option<String> {
    let (uploading, rate) = match progress.stage {
        Stage::Latency => return Some(format!("{label}  {}", tr("speed.stage.latency", &[]))),
        Stage::Upload if progress.waiting => {
            return Some(format!("{label}  {}", tr("speed.stage.confirming", &[])));
        }
        Stage::Download => (false, progress.result.download),
        Stage::Upload => (true, progress.result.upload),
        _ => return None,
    };
    let mut line = format!("{label}  {}", transfer_text(progress, uploading));
    if let Some(mbps) = rate {
        line.push_str(&format!("  {mbps:.1} Mbps"));
    }
    line.push_str(&format!(
        "  {:.0}%",
        progress.progress.clamp(0.0, 1.0) * 100.0
    ));
    Some(line)
}

/// What a leg is doing now, for the window; nothing once it finished or before its turn.
pub fn stage_text(progress: &Progress) -> Option<String> {
    match progress.stage {
        Stage::Latency => Some(tr("speed.stage.latency", &[])),
        Stage::Download => Some(transfer_text(progress, false)),
        // The grace period: time is up, the server has yet to confirm what was sent.
        Stage::Upload if progress.waiting => Some(tr("speed.stage.confirming", &[])),
        Stage::Upload => Some(transfer_text(progress, true)),
        Stage::Cancelled => Some(tr("speed.cancelled", &[])),
        Stage::Idle | Stage::Done | Stage::Failed(_) => None,
    }
}

pub fn note_text(note: &Note) -> String {
    match note {
        Note::CapReached { uploading, seconds } => tr(
            "speed.cap_note",
            &[
                ("direction", &direction(*uploading)),
                ("seconds", &format!("{seconds:.1}")),
            ],
        ),
        Note::UploadTooSlow => tr("speed.upload_too_slow", &[]),
    }
}

/// "↓ 123.4 ↑ 45.6 Mbps · 32 ms"; a missing figure is left out.
pub fn summary_text(summary: &Summary) -> String {
    let mut speeds = Vec::new();
    if let Some(down) = summary.download {
        speeds.push(format!("↓ {down:.1}"));
    }
    if let Some(up) = summary.upload {
        speeds.push(format!("↑ {up:.1}"));
    }
    let mut parts = Vec::new();
    if !speeds.is_empty() {
        parts.push(format!("{} Mbps", speeds.join(" ")));
    }
    if let Some(ms) = summary.latency {
        parts.push(format!("{ms:.0} ms"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    #[test]
    fn history_cells_leave_out_what_is_missing() {
        let full = Summary {
            download: Some(123.44),
            upload: Some(45.6),
            latency: Some(31.6),
        };
        assert_eq!(summary_text(&full), "↓ 123.4 ↑ 45.6 Mbps · 32 ms");
        let no_upload = Summary {
            upload: None,
            ..full.clone()
        };
        assert_eq!(summary_text(&no_upload), "↓ 123.4 Mbps · 32 ms");
        let latency_only = Summary {
            download: None,
            upload: None,
            latency: Some(40.0),
        };
        assert_eq!(summary_text(&latency_only), "40 ms");
    }

    #[test]
    fn stage_and_note_sentences() {
        let _lang = language(cg_i18n::Lang::ZhCn);
        let mut p = Progress {
            stage: Stage::Latency,
            ..Progress::default()
        };
        assert_eq!(stage_text(&p).as_deref(), Some("正在检测 HTTPS 延迟…"));
        cg_i18n::set(cg_i18n::Lang::En);
        p.stage = Stage::Download;
        p.elapsed = 4.4;
        p.duration = 8.0;
        assert_eq!(stage_text(&p).as_deref(), Some("Testing Download… 4 / 8 s"));
        p.stage = Stage::Upload;
        p.elapsed = 10.0;
        p.progress = 1.0;
        p.waiting = true;
        let waiting = tr("speed.stage.confirming", &[]);
        assert_eq!(stage_text(&p).as_deref(), Some(waiting.as_str()));
        let line = progress_line("iPhone", &p).unwrap();
        assert!(line.ends_with(&waiting) && !line.contains('%'), "{line}");
        p.waiting = false;
        for stage in [Stage::Idle, Stage::Done, Stage::Failed(SpeedError::Timeout)] {
            p.stage = stage;
            assert_eq!(stage_text(&p), None);
        }
        let note = note_text(&Note::CapReached {
            uploading: true,
            seconds: 3.4,
        });
        assert!(!note.contains("%{") && note.contains("3.4"), "{note}");
    }

    fn language(lang: cg_i18n::Lang) -> MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(lang);
        guard
    }

    #[test]
    fn every_error_has_text_without_placeholders() {
        let _lang = language(cg_i18n::Lang::En);
        for error in SpeedError::all() {
            assert!(
                cg_i18n::KEYS.contains(&error_key(&error)),
                "{error:?} key missing from KEYS"
            );
            for lang in [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En] {
                cg_i18n::set(lang);
                let text = error_text(&error);
                assert!(
                    !text.is_empty() && !text.contains("%{") && !text.contains("speed."),
                    "{error:?}: {text}"
                );
            }
        }
        cg_i18n::set(cg_i18n::Lang::En);
        assert!(error_text(&SpeedError::RateLimited { minutes: 2 }).contains('2'));
        assert!(error_text(&SpeedError::Refused(403)).contains("403"));
        assert!(error_text(&SpeedError::Io("reset".into())).contains("reset"));
    }

    #[test]
    fn verdict_sentences() {
        let _lang = language(cg_i18n::Lang::En);
        assert_eq!(
            verdict_line(Some(20.0), Some(60.0)).unwrap(),
            "Wi‑Fi download is 3.0x faster than the iPhone."
        );
        assert_eq!(
            verdict_line(Some(60.0), Some(20.0)).unwrap(),
            "iPhone download is 3.0x faster than Wi‑Fi."
        );
        assert_eq!(
            verdict_line(Some(60.0), Some(55.0)).unwrap(),
            "The two connections run at similar speeds."
        );
        assert_eq!(verdict_line(None, Some(55.0)), None);
    }

    #[test]
    fn progress_line_shows_stage_seconds_rate_and_percent() {
        let _lang = language(cg_i18n::Lang::En);
        let mut p = Progress {
            stage: Stage::Latency,
            ..Progress::default()
        };
        assert_eq!(
            progress_line("iPhone", &p).unwrap(),
            "iPhone  Checking HTTPS latency…"
        );
        p.stage = Stage::Download;
        p.elapsed = 4.4;
        p.duration = 6.0;
        p.progress = 0.2;
        p.result.download = Some(18.26);
        assert_eq!(
            progress_line("Wi‑Fi", &p).unwrap(),
            "Wi‑Fi  Testing Download… 4 / 6 s  18.3 Mbps  20%"
        );
        p.stage = Stage::Done;
        assert_eq!(progress_line("iPhone", &p), None);
    }

    #[test]
    fn stage_text_counts_to_the_tests_own_duration() {
        let _lang = language(cg_i18n::Lang::En);
        let p = Progress {
            stage: Stage::Download,
            elapsed: 3.2,
            duration: 5.0,
            ..Progress::default()
        };
        assert!(
            stage_text(&p).unwrap().ends_with("3 / 5 s"),
            "{:?}",
            stage_text(&p)
        );
        let full = Progress {
            stage: Stage::Upload,
            elapsed: 12.0,
            duration: 7.0,
            ..Progress::default()
        };
        assert!(stage_text(&full).unwrap().ends_with("7 / 7 s"));
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        assert!(
            stage_text(&p).unwrap().ends_with("3 / 5 秒"),
            "{:?}",
            stage_text(&p)
        );
    }

    #[test]
    fn progress_line_counts_to_the_tests_own_duration() {
        let _lang = language(cg_i18n::Lang::En);
        let p = Progress {
            stage: Stage::Download,
            elapsed: 3.2,
            duration: 5.0,
            ..Progress::default()
        };
        assert!(
            progress_line("iPhone", &p).unwrap().contains("3 / 5 s"),
            "{:?}",
            progress_line("iPhone", &p)
        );
    }

    #[test]
    fn a_missing_or_broken_duration_reads_zero_without_panicking() {
        let _lang = language(cg_i18n::Lang::En);
        for (elapsed, duration) in [
            (3.2, 0.0),
            (3.2, f64::NAN),
            (3.2, -5.0),
            (f64::NAN, 5.0),
            (-1.0, 5.0),
        ] {
            let p = Progress {
                stage: Stage::Download,
                elapsed,
                duration,
                ..Progress::default()
            };
            let text = stage_text(&p).unwrap();
            let expected = if duration > 0.0 { "0 / 5 s" } else { "0 / 0 s" };
            assert!(text.ends_with(expected), "{elapsed} / {duration}: {text}");
            let line = progress_line("iPhone", &p).unwrap();
            assert!(line.contains(expected) && !line.contains("NaN"), "{line}");
        }
    }
}
