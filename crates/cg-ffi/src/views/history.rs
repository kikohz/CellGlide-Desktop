//! The history page (one day: its strip, hours, events and legend) and the doctor's checks.

use cg_ipc::Check;
use cg_ipc::describe::{self, ui};
use cg_ipc::health::{HealthEvent, HistoryDay, hour_marks};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// How wide the strip is in `strip` cells' units: thousandths, as `ui::tick_gaps` uses.
const STRIP_WIDTH: f32 = 1000.0;

/// One day of the connection history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HistoryView {
    /// `YYYY-MM-DD`, to ask for again.
    pub day: String,
    /// "Today, Oct 30".
    pub label: String,
    /// The days to page to; `None` past either end of the seven on record.
    pub prev_day: Option<String>,
    pub next_day: Option<String>,
    /// The 24-hour strip, `x` and `width` in thousandths of its width.
    pub strip: Vec<ui::StripCell>,
    /// The hour labels under the strip.
    pub ticks: Vec<TickView>,
    /// The day's events, newest first, as on the Mac.
    pub rows: Vec<HistoryRowView>,
    pub legend: Vec<ui::LegendItem>,
}

/// One hour label: `at` is how far along the strip (0–1), so a 23- or 25-hour day still puts
/// the label under its hour.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TickView {
    pub label: String,
    pub at: f64,
}

/// One event: its clock time, title and dot, and the readings when an issue started or got
/// worse (empty otherwise; a row with details can expand).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HistoryRowView {
    pub time: String,
    pub title: String,
    pub tone: ui::Tone,
    pub details: Vec<DetailItem>,
}

/// One reading of the detail grid (`describe::snapshot_items` gives pairs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DetailItem {
    pub label: String,
    pub value: String,
}

pub fn history(day: &HistoryDay, now: Timestamp, tz: &TimeZone) -> HistoryView {
    let strip = ui::day_strip(day, now, tz);
    HistoryView {
        day: day.day.clone(),
        label: describe::day_label(day),
        prev_day: ui::neighbor(&day.days, &day.day, -1),
        next_day: ui::neighbor(&day.days, &day.day, 1),
        strip: ui::strip_cells(&strip.spans, strip.begin, strip.next, STRIP_WIDTH),
        ticks: ticks(day, tz),
        rows: day.events.iter().rev().map(|e| row(e, tz)).collect(),
        legend: ui::legend(),
    }
}

/// The checks as rows to draw; one that couldn't be made is `CheckState::Unknown`, not a pass.
pub fn checks(checks: &[Check]) -> Vec<ui::CheckRow> {
    ui::check_rows(checks, None)
}

fn ticks(day: &HistoryDay, tz: &TimeZone) -> Vec<TickView> {
    let marks = match day.day.parse::<Date>() {
        Ok(date) => hour_marks(date, tz),
        // Not a date: evenly spaced, as `hour_marks` does for a day it can't place.
        Err(_) => [0u8, 6, 12, 18, 24]
            .into_iter()
            .map(|hour| (hour, f64::from(hour) / 24.0))
            .collect(),
    };
    marks
        .into_iter()
        .map(|(hour, at)| TickView {
            label: ui::tick_label(hour),
            at,
        })
        .collect()
}

fn row(event: &HealthEvent, tz: &TimeZone) -> HistoryRowView {
    let ui::EventRow { time, title, tone } = ui::row_texts(event, tz);
    let details = event
        .snapshot
        .as_ref()
        .map(describe::snapshot_items)
        .unwrap_or_default()
        .into_iter()
        .map(|(label, value)| DetailItem { label, value })
        .collect();
    HistoryRowView {
        time,
        title,
        tone,
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::middle_day;
    use cg_ipc::CheckId;
    use ui::{CheckState, Tone};

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    fn end_of(day: &HistoryDay) -> Timestamp {
        day.end
    }

    #[test]
    fn history_view_pages_and_paints() {
        let _g = en();
        let day = middle_day();
        let v = history(&day, end_of(&day), &TimeZone::UTC);
        assert_eq!(
            (v.prev_day.as_deref(), v.next_day.as_deref()),
            (Some(day.days[2].as_str()), Some(day.days[4].as_str()))
        );
        assert!(!v.strip.is_empty());
        assert!(
            v.strip
                .iter()
                .all(|c| c.x >= 0.0 && c.x + c.width <= 1000.0 + 1e-3)
        );
        assert_eq!(
            v.ticks.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["00:00", "06:00", "12:00", "18:00", "24:00"]
        );
        assert!(v.ticks.windows(2).all(|w| w[0].at < w[1].at));
        assert_eq!(v.rows.len(), day.events.len());
        assert_eq!(v.legend.len(), 4);
        assert_eq!(crate::testing::plain_shape(&v), Ok(()));
    }

    #[test]
    fn rows_are_newest_first_and_only_an_opening_issue_has_details() {
        let _g = en();
        let day = middle_day();
        let v = history(&day, end_of(&day), &TimeZone::UTC);
        let newest = day.events.last().unwrap();
        assert_eq!(v.rows[0].time, describe::clock(newest.t, &TimeZone::UTC));
        assert_eq!(v.rows[0].title, describe::event_title(newest));
        for (row, event) in v.rows.iter().zip(day.events.iter().rev()) {
            let expected: Vec<DetailItem> = event
                .snapshot
                .as_ref()
                .map(describe::snapshot_items)
                .unwrap_or_default()
                .into_iter()
                .map(|(label, value)| DetailItem { label, value })
                .collect();
            assert_eq!(row.details, expected);
        }
        assert!(v.rows.iter().any(|r| !r.details.is_empty()));
        assert!(
            v.rows
                .iter()
                .any(|r| r.tone == Tone::Red || r.tone == Tone::Orange)
        );
        assert_eq!(v.label, describe::day_label(&day));
    }

    #[test]
    fn the_first_and_last_days_page_one_way() {
        let _g = en();
        let mut day = middle_day();
        day.day = day.days[0].clone();
        let v = history(&day, end_of(&day), &TimeZone::UTC);
        assert_eq!(v.prev_day, None);
        assert_eq!(v.next_day.as_deref(), Some(day.days[1].as_str()));
        day.day = day.days[6].clone();
        let v = history(&day, end_of(&day), &TimeZone::UTC);
        assert_eq!(v.prev_day.as_deref(), Some(day.days[5].as_str()));
        assert_eq!(v.next_day, None);
    }

    #[test]
    fn ticks_follow_a_dst_day() {
        // 2026-03-08 has 23 hours in New York: 6 o'clock is a quarter of 23 hours less one.
        let tz = TimeZone::get("America/New_York").unwrap();
        let mut day = middle_day();
        day.day = "2026-03-08".into();
        let ticks = ticks(&day, &tz);
        assert_eq!(ticks.len(), 5);
        assert!((ticks[1].at - 5.0 / 23.0).abs() < 1e-9, "{ticks:?}");
        assert_eq!(ticks[4].at, 1.0);
    }

    #[test]
    fn a_check_that_could_not_be_made_is_not_a_pass() {
        let _g = en();
        let rows = checks(&[
            Check::new(CheckId::Service, true),
            Check::new(CheckId::Dns, false)
                .with("cause", "unreadable")
                .with("error", "denied"),
            Check::new(CheckId::Phone, false),
        ]);
        let states: Vec<CheckState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [CheckState::Pass, CheckState::Unknown, CheckState::Fail]
        );
        assert!(rows[0].fix.is_none() && rows[1].fix.is_some() && rows[2].fix.is_some());
        assert!(rows.iter().all(|r| !r.copied));
    }
}
