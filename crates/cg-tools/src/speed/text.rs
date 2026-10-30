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

/// The one line that is rewritten while a leg runs: the leg, its stage, progress and the
/// current rate.
pub fn progress_line(label: &str, progress: &Progress) -> Option<String> {
    let (uploading, rate) = match progress.stage {
        Stage::Latency => return Some(format!("{label}  {}", tr("speed.stage.latency", &[]))),
        Stage::Download => (false, progress.result.download),
        Stage::Upload => (true, progress.result.upload),
        _ => return None,
    };
    let seconds = format!("{:.0}", progress.elapsed.clamp(0.0, 10.0));
    let mut line = format!(
        "{label}  {}",
        tr(
            "speed.stage.transfer",
            &[("direction", &direction(uploading)), ("seconds", &seconds)],
        )
    );
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
    let transfer = |uploading: bool| {
        let seconds = format!("{:.0}", progress.elapsed.clamp(0.0, 10.0));
        tr(
            "speed.stage.transfer",
            &[("direction", &direction(uploading)), ("seconds", &seconds)],
        )
    };
    match progress.stage {
        Stage::Latency => Some(tr("speed.stage.latency", &[])),
        Stage::Download => Some(transfer(false)),
        Stage::Upload => Some(transfer(true)),
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
        assert_eq!(
            stage_text(&p).as_deref(),
            Some("Testing Download… 4 / 10 s")
        );
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
        p.progress = 0.2;
        p.result.download = Some(18.26);
        assert_eq!(
            progress_line("Wi‑Fi", &p).unwrap(),
            "Wi‑Fi  Testing Download… 4 / 10 s  18.3 Mbps  20%"
        );
        p.stage = Stage::Done;
        assert_eq!(progress_line("iPhone", &p), None);
    }
}
