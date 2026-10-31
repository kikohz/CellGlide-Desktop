//! The speed test page: Start and the sentence beside it, one column per route with its
//! numbers, curve, progress and stage, the comparison, and the recent results.

use cg_ipc::describe::ui::{self, CurvePoint};
use cg_tools::speed::text::{
    error_text, note_text, numbers, stage_text, summary_text, verdict_line,
};
use cg_tools::speed::{Progress, Record, Stage, run};
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// A dialog the page puts up.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeedDialog {
    /// The test would use phone data: ask first (`SpeedStart` again with `confirmed`, or
    /// `SpeedCancel` to put it away).
    DataWarning,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SpeedView {
    pub running: bool,
    pub dialog: Option<SpeedDialog>,
    /// While running, Start is the Cancel button and stays enabled.
    pub start_enabled: bool,
    /// Why Start is off, or what the test will measure.
    pub start_note: Option<String>,
    /// This computer's own Wi‑Fi or Ethernet, for "Also test Wi‑Fi or Ethernet (…)".
    pub uplink: Option<String>,
    /// One per route of the last (or current) run, in order.
    pub legs: Vec<SpeedLegView>,
    /// The comparison, once a run is over and both downloads are known.
    pub verdict: Option<String>,
    /// The recent results, newest first.
    pub history: Vec<SpeedRecordView>,
    /// The last save of the history failed.
    pub history_failed: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StageView {
    #[default]
    Idle,
    Latency,
    Download,
    Upload,
    Done,
    Failed,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpeedLegView {
    pub label: String,
    /// The phone's leg (blue), not Wi‑Fi or Ethernet (green).
    pub phone: bool,
    pub stage: StageView,
    /// What the leg is doing now.
    pub stage_text: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// "Latency 40 ms · Download 20.0 Mbps · Upload 5.0 Mbps", what is known so far.
    pub numbers: String,
    /// Milliseconds.
    pub latency: Option<f64>,
    /// Mbps.
    pub download: Option<f64>,
    pub upload: Option<f64>,
    /// 0–1 across both directions.
    pub progress: f64,
    /// Upload's time is up and it waits for the server's confirmations: `progress` stands
    /// still, so show the bar as busy rather than as a percentage (`stage_text` says so).
    pub waiting: bool,
    pub curve: Vec<CurvePoint>,
    pub notes: Vec<String>,
}

/// One row of the recent results.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpeedRecordView {
    /// Local time, "YYYY-MM-DD HH:MM".
    pub date: String,
    /// "↓ 123.4 ↑ 45.6 Mbps · 32 ms".
    pub phone: Option<String>,
    pub wifi: Option<String>,
    /// "Carrier · 5G", what is known of the two.
    pub carrier: Option<String>,
}

/// One route as the page keeps it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpeedLeg {
    pub label: String,
    pub phone: bool,
    pub progress: Progress,
}

/// Everything the page depends on.
pub(crate) struct SpeedPage<'a> {
    pub running: bool,
    pub dialog: Option<SpeedDialog>,
    /// Connected through a phone that shows its card (`Ctx::phone_connected` on Linux).
    pub phone: bool,
    pub uplink: Option<&'a str>,
    pub include_wifi: bool,
    pub legs: &'a [SpeedLeg],
    pub records: &'a [Record],
    pub history_failed: bool,
}

pub(crate) fn speed(page: &SpeedPage, tz: &TimeZone) -> SpeedView {
    let (start_enabled, start_note) = run::start_line(
        page.running,
        page.phone,
        page.uplink.map(str::to_string),
        page.include_wifi,
        None,
    );
    SpeedView {
        running: page.running,
        dialog: page.dialog,
        start_enabled,
        start_note,
        uplink: page.uplink.map(str::to_string),
        legs: page.legs.iter().map(leg).collect(),
        verdict: verdict(page),
        history: page.records.iter().map(|r| record(r, tz)).collect(),
        history_failed: page.history_failed,
    }
}

fn verdict(page: &SpeedPage) -> Option<String> {
    if page.running {
        return None;
    }
    let download = |phone: bool| {
        page.legs
            .iter()
            .find(|leg| leg.phone == phone && leg.progress.stage == Stage::Done)
            .and_then(|leg| leg.progress.result.download)
    };
    verdict_line(download(true), download(false))
}

fn leg(leg: &SpeedLeg) -> SpeedLegView {
    let p = &leg.progress;
    let r = &p.result;
    let (stage, error) = match &p.stage {
        Stage::Idle => (StageView::Idle, None),
        Stage::Latency => (StageView::Latency, None),
        Stage::Download => (StageView::Download, None),
        Stage::Upload => (StageView::Upload, None),
        Stage::Done => (StageView::Done, None),
        Stage::Failed(e) => (StageView::Failed, Some(error_text(e))),
        Stage::Cancelled => (StageView::Cancelled, None),
    };
    SpeedLegView {
        label: leg.label.clone(),
        phone: leg.phone,
        stage,
        stage_text: stage_text(p),
        error,
        numbers: numbers(r.latency, r.download, r.upload),
        latency: r.latency,
        download: r.download,
        upload: r.upload,
        progress: p.progress.clamp(0.0, 1.0),
        waiting: p.waiting && p.stage == Stage::Upload,
        curve: ui::curve_points(&r.download_series),
        notes: r.notes.iter().map(note_text).collect(),
    }
}

fn record(record: &Record, tz: &TimeZone) -> SpeedRecordView {
    let network: Vec<&str> = [&record.carrier, &record.generation]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    SpeedRecordView {
        date: record
            .date
            .to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
        phone: record.phone.as_ref().map(summary_text),
        wifi: record.wifi.as_ref().map(summary_text),
        carrier: (!network.is_empty()).then(|| network.join(" · ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::tr;
    use cg_tools::speed::{Note, SpeedError, SpeedResult};
    use jiff::Timestamp;

    fn leg_at(phone: bool, stage: Stage, download: Option<f64>) -> SpeedLeg {
        SpeedLeg {
            label: if phone { "iPhone" } else { "Wi‑Fi" }.into(),
            phone,
            progress: Progress {
                stage,
                progress: 0.5,
                result: SpeedResult {
                    latency: Some(40.0),
                    download,
                    upload: download.map(|d| d / 4.0),
                    download_series: vec![1.0, 2.0, 4.0],
                    notes: vec![Note::UploadTooSlow],
                },
                ..Progress::default()
            },
        }
    }

    fn page<'a>(legs: &'a [SpeedLeg], records: &'a [Record]) -> SpeedPage<'a> {
        SpeedPage {
            running: false,
            dialog: None,
            phone: true,
            uplink: Some("Wi-Fi"),
            include_wifi: true,
            legs,
            records,
            history_failed: false,
        }
    }

    #[test]
    fn a_finished_run_shows_its_numbers_and_the_comparison() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let legs = [
            leg_at(true, Stage::Done, Some(20.0)),
            leg_at(false, Stage::Done, Some(60.0)),
        ];
        let view = speed(&page(&legs, &[]), &TimeZone::UTC);
        assert_eq!(
            (view.start_enabled, view.start_note.as_deref()),
            (true, None)
        );
        assert_eq!(view.uplink.as_deref(), Some("Wi-Fi"));
        assert_eq!(view.verdict, verdict_line(Some(20.0), Some(60.0)));
        let phone = &view.legs[0];
        assert!(phone.phone && phone.stage == StageView::Done);
        assert_eq!(phone.numbers, numbers(Some(40.0), Some(20.0), Some(5.0)));
        assert_eq!(phone.curve.len(), 3);
        assert_eq!(phone.notes, [tr("speed.upload_too_slow", &[])]);
        assert_eq!(
            (phone.stage_text.clone(), phone.error.clone()),
            (None, None)
        );
        // While running there is no comparison yet, and Start is the Cancel button.
        let running = SpeedPage {
            running: true,
            ..page(&legs, &[])
        };
        let view = speed(&running, &TimeZone::UTC);
        assert_eq!(view.verdict, None);
        assert!(view.start_enabled && view.start_note.is_none());
        crate::testing::plain_shape(&view).unwrap();
    }

    #[test]
    fn a_failed_leg_says_why_and_nothing_to_test_says_connect() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let legs = [leg_at(true, Stage::Failed(SpeedError::Timeout), None)];
        let nothing = SpeedPage {
            phone: false,
            uplink: None,
            ..page(&legs, &[])
        };
        let view = speed(&nothing, &TimeZone::UTC);
        assert_eq!(view.legs[0].stage, StageView::Failed);
        assert_eq!(
            view.legs[0].error.as_deref(),
            Some(error_text(&SpeedError::Timeout).as_str())
        );
        assert!(!view.start_enabled);
        assert_eq!(view.start_note, Some(error_text(&SpeedError::NotConnected)));
    }

    #[test]
    fn the_grace_period_is_a_waiting_leg_not_a_full_bar() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let mut waiting = leg_at(true, Stage::Upload, Some(20.0));
        waiting.progress.waiting = true;
        waiting.progress.progress = 1.0;
        waiting.progress.elapsed = 10.0;
        let legs = [waiting];
        let view = speed(
            &SpeedPage {
                running: true,
                ..page(&legs, &[])
            },
            &TimeZone::UTC,
        );
        let leg = &view.legs[0];
        assert!(leg.waiting);
        assert_eq!(
            leg.stage_text.as_deref(),
            Some(tr("speed.stage.confirming", &[]).as_str())
        );
        assert!(!leg.stage_text.as_deref().unwrap().contains("10"));
        cg_i18n::set(cg_i18n::Lang::En);
    }

    #[test]
    fn history_rows_are_local_time_and_summaries() {
        let _g = cg_i18n::lock_for_tests();
        let phone = SpeedResult {
            download: Some(20.0),
            upload: Some(5.0),
            latency: Some(40.0),
            ..SpeedResult::default()
        };
        let date = "2026-10-30T08:09:10Z".parse::<Timestamp>().unwrap();
        let record = cg_tools::speed::record(
            Some(&phone),
            None,
            true,
            date,
            Some("AT&T".into()),
            Some("5G".into()),
        )
        .unwrap();
        let tz = TimeZone::get("Asia/Shanghai")
            .unwrap_or(TimeZone::fixed(jiff::tz::Offset::constant(8)));
        let records = [record.clone()];
        let view = speed(&page(&[], &records), &tz);
        assert_eq!(
            view.history,
            [SpeedRecordView {
                date: "2026-10-30 16:09".into(),
                phone: Some("↓ 20.0 ↑ 5.0 Mbps · 40 ms".into()),
                wifi: None,
                carrier: Some("AT&T · 5G".into()),
            }]
        );
        let bare = Record {
            carrier: None,
            generation: None,
            ..record
        };
        let records = [bare];
        assert_eq!(speed(&page(&[], &records), &tz).history[0].carrier, None);
    }
}
