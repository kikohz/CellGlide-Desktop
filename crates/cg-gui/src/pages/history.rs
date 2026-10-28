//! The connection history page, after the Mac's `HistoryPage`: a 24-hour strip, the day's
//! events (newest first, an issue's readings expand), and paging through the seven days the
//! service keeps. Looking at today it refreshes every 5 s; after midnight it returns to the
//! new today by asking for it, never for a day outside the service's window.

use super::Ctx;
use crate::effect::{Effect, Reply};
use crate::widgets::kit::{self, SEMIBOLD, Tone};
use crate::widgets::strip::{self, DayStrip};
use cg_i18n::tr;
use cg_ipc::Request;
use cg_ipc::describe::{self, EventTone};
use cg_ipc::health::{HealthEvent, HistoryDay, day_bounds, day_spans, hour_marks};
use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Font, Length};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::collections::BTreeSet;

/// Wide enough for "23:59" in the row's small text.
const TIME_WIDTH: f32 = 44.0;

#[derive(Debug, Default)]
pub struct State {
    /// `None` follows today.
    day: Option<String>,
    loaded: Option<HistoryDay>,
    /// Indexes into the loaded day's events (oldest first).
    expanded: BTreeSet<usize>,
    loading: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// The page was opened, or the retry button was pressed.
    Shown,
    /// Every 5 s while the page is open.
    Refresh,
    Prev,
    Next,
    Loaded(Result<HistoryDay, String>),
    Toggle(usize),
}

impl State {
    /// A request is out and unanswered.
    pub fn is_loading(&self) -> bool {
        self.loading
    }
}

/// The day `step` days from `current` among the days on record; `None` past either end.
pub fn neighbor(days: &[String], current: &str, step: i8) -> Option<String> {
    let at = days.iter().position(|d| d == current)?;
    let to = at.checked_add_signed(isize::from(step))?;
    days.get(to).cloned()
}

fn request(state: &mut State) -> Effect {
    state.loading = true;
    Effect::Request(
        Request::History {
            day: state.day.clone(),
        },
        Reply::History,
    )
}

/// The newest day on record, which the service's today.
fn newest(state: &State) -> Option<&str> {
    state.loaded.as_ref()?.days.last().map(String::as_str)
}

fn viewing_today(state: &State) -> bool {
    match (&state.day, newest(state)) {
        (None, _) => true,
        (Some(day), newest) => newest == Some(day.as_str()),
    }
}

/// The service keeps today and the six days before it.
fn outside_window(day: &str, now: Timestamp, tz: &TimeZone) -> bool {
    let today = now.to_zoned(tz.clone()).date();
    let Ok(day) = day.parse::<Date>() else {
        return true;
    };
    day > today || today.since(day).is_ok_and(|span| span.get_days() > 6)
}

/// Before any request: a viewed day that has left the window gives way to today.
fn align(state: &mut State, now: Timestamp, tz: &TimeZone) {
    if state
        .day
        .as_deref()
        .is_some_and(|day| outside_window(day, now, tz))
    {
        follow_today(state);
    }
}

/// The clock has moved past the newest day on record.
fn crossed_midnight(state: &State, now: Timestamp, tz: &TimeZone) -> bool {
    let today = now.to_zoned(tz.clone()).date();
    newest(state)
        .and_then(|day| day.parse::<Date>().ok())
        .is_some_and(|newest| today > newest)
}

fn follow_today(state: &mut State) {
    state.day = None;
    state.expanded.clear();
}

pub fn update(state: &mut State, message: Msg, ctx: &Ctx) -> Effect {
    match message {
        Msg::Shown => {
            align(state, ctx.now, &TimeZone::system());
            request(state)
        }
        Msg::Refresh if state.loading => Effect::None,
        Msg::Refresh if crossed_midnight(state, ctx.now, &TimeZone::system()) => {
            // Keep a day that is still in the window; its request refreshes `days`.
            align(state, ctx.now, &TimeZone::system());
            request(state)
        }
        Msg::Refresh if viewing_today(state) => request(state),
        Msg::Refresh => Effect::None,
        Msg::Prev => page(state, -1),
        Msg::Next => page(state, 1),
        Msg::Loaded(Err(reason)) => {
            state.loading = false;
            state.error = Some(reason);
            Effect::None
        }
        Msg::Loaded(Ok(loaded)) => {
            let today = loaded.days.last().map(String::as_str);
            // A late answer for a day that is no longer the one wanted.
            let stale = match &state.day {
                Some(day) => *day != loaded.day,
                None => today.is_some_and(|today| today != loaded.day),
            };
            if stale {
                return Effect::None;
            }
            state.loading = false;
            state.error = None;
            // The day rolled out of the window (midnight): ask for the new today instead.
            if state
                .day
                .as_ref()
                .is_some_and(|day| !loaded.days.contains(day))
            {
                follow_today(state);
                state.loaded = None;
                return request(state);
            }
            state.loaded = Some(loaded);
            Effect::None
        }
        Msg::Toggle(index) => {
            let has_snapshot = state
                .loaded
                .as_ref()
                .and_then(|day| day.events.get(index))
                .is_some_and(|event| event.snapshot.is_some());
            if has_snapshot && !state.expanded.remove(&index) {
                state.expanded.insert(index);
            }
            Effect::None
        }
    }
}

fn page(state: &mut State, step: i8) -> Effect {
    let Some(loaded) = &state.loaded else {
        return Effect::None;
    };
    let current = state.day.as_deref().or(newest(state));
    let Some(target) = current.and_then(|current| neighbor(&loaded.days, current, step)) else {
        return Effect::None;
    };
    // Landing on the newest day follows today, so midnight moves it along.
    state.day = (newest(state) != Some(target.as_str())).then_some(target);
    state.expanded.clear();
    request(state)
}

fn can_page(state: &State, step: i8) -> bool {
    let Some(loaded) = &state.loaded else {
        return false;
    };
    let current = state.day.as_deref().or(newest(state));
    current.is_some_and(|current| neighbor(&loaded.days, current, step).is_some())
}

/// The strip of a day: its spans between local midnights, up to now.
pub fn strip_for(day: &HistoryDay, now: Timestamp, tz: &TimeZone) -> DayStrip {
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

fn tone(tone: EventTone) -> Tone {
    match tone {
        EventTone::Green => Tone::Green,
        EventTone::Orange => Tone::Orange,
        EventTone::Red => Tone::Red,
        EventTone::Gray => Tone::Gray,
    }
}

/// One row's local time, title (which already carries the duration of an end) and dot.
pub fn row_texts(event: &HealthEvent, tz: &TimeZone) -> (String, String, Tone) {
    (
        describe::clock(event.t, tz),
        describe::event_title(event),
        tone(describe::event_tone(event)),
    )
}

pub fn legend_labels() -> [String; 4] {
    [
        tr("history.legend.ok", &[]),
        tr("history.legend.unstable", &[]),
        tr("history.legend.down", &[]),
        tr("history.legend.not_sharing", &[]),
    ]
}

fn legend<'a>() -> Element<'a, Msg> {
    let tones = [Tone::Green, Tone::Orange, Tone::Red, Tone::Gray];
    let mut items = row![].spacing(12).align_y(Alignment::Center);
    for (label, t) in legend_labels().into_iter().zip(tones) {
        items = items.push(
            row![kit::dot(t, 8.0), kit::secondary(label).size(11)]
                .spacing(4)
                .align_y(Alignment::Center),
        );
    }
    items.into()
}

fn arrow<'a>(label: &'static str, on_press: Option<Msg>) -> Element<'a, Msg> {
    button(text(label).size(14))
        .padding([2, 8])
        .style(button::secondary)
        .on_press_maybe(on_press)
        .into()
}

fn ticks<'a>(day: &HistoryDay, tz: &TimeZone) -> Element<'a, Msg> {
    let gaps = day
        .day
        .parse::<Date>()
        .map(|date| tick_gaps(&date, tz))
        .unwrap_or_else(|_| vec![250; 4]);
    let label = |hour: u8| kit::secondary(format!("{hour:02}:00")).size(11);
    let mut ticks = row![label(0)];
    for (gap, hour) in gaps.into_iter().zip([6u8, 12, 18, 24]) {
        ticks = ticks
            .push(space().width(Length::FillPortion(gap)))
            .push(label(hour));
    }
    ticks.into()
}

fn detail_grid<'a>(event: &HealthEvent) -> Element<'a, Msg> {
    let Some(snapshot) = &event.snapshot else {
        return column![].into();
    };
    let items = describe::snapshot_items(snapshot);
    let cell = |(name, value): &(String, String)| -> Element<'a, Msg> {
        row![
            kit::secondary(name.clone()).size(12),
            space().width(Length::Fill),
            text(value.clone()).size(12).font(Font::MONOSPACE),
        ]
        .spacing(8)
        .width(Length::FillPortion(1))
        .into()
    };
    let mut grid = column![].spacing(6);
    for pair in items.chunks(2) {
        let mut line = row![cell(&pair[0])].spacing(24);
        line = match pair.get(1) {
            Some(second) => line.push(cell(second)),
            None => line.push(space().width(Length::FillPortion(1))),
        };
        grid = grid.push(line);
    }
    grid.into()
}

fn event_row<'a>(index: usize, event: &HealthEvent, open: bool, tz: &TimeZone) -> Element<'a, Msg> {
    let (time, title, dot_tone) = row_texts(event, tz);
    let expandable = event.snapshot.is_some();
    let chevron = match (expandable, open) {
        (false, _) => "",
        (true, true) => "\u{2212}",
        (true, false) => "+",
    };
    let mut body = column![
        row![
            kit::secondary(time)
                .size(13)
                .font(Font::MONOSPACE)
                .width(TIME_WIDTH),
            kit::dot(dot_tone, 8.0),
            text(title).size(13).width(Length::Fill),
            kit::secondary(chevron).width(10),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
    ]
    .spacing(8);
    if open {
        body = body.push(
            container(detail_grid(event))
                .padding(iced::Padding::ZERO.left(TIME_WIDTH + 20.0).bottom(2.0)),
        );
    }
    button(body)
        .width(Length::Fill)
        .padding([10, 14])
        .style(button::text)
        .on_press_maybe(expandable.then_some(Msg::Toggle(index)))
        .into()
}

pub fn view<'a>(state: &'a State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let tz = TimeZone::system();
    let title = state
        .loaded
        .as_ref()
        .map(describe::day_label)
        .unwrap_or_default();
    let header = row![
        arrow("\u{2039}", can_page(state, -1).then_some(Msg::Prev)),
        arrow("\u{203A}", can_page(state, 1).then_some(Msg::Next)),
        text(title).size(15).font(SEMIBOLD),
        space().width(Length::Fill),
        legend(),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut body = column![header].spacing(14);
    if let Some(error) = &state.error {
        body = body.push(
            row![
                text(error.clone()).size(13).color(Tone::Red.color()),
                space().width(Length::Fill),
                button(text(tr("history.retry", &[])).size(12))
                    .on_press_maybe((!state.loading).then_some(Msg::Shown)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    if let Some(day) = &state.loaded {
        body = body.push(
            column![
                container(strip::view(strip_for(day, ctx.now, &tz), 28.0))
                    .clip(true)
                    .style(|_| container::Style {
                        border: iced::Border {
                            radius: 6.0.into(),
                            ..iced::Border::default()
                        },
                        ..container::Style::default()
                    }),
                ticks(day, &tz),
            ]
            .spacing(5),
        );
        let mut list = column![].width(Length::Fill);
        for (position, (index, event)) in day.events.iter().enumerate().rev().enumerate() {
            if position > 0 {
                list = list.push(iced::widget::rule::horizontal(1));
            }
            list = list.push(event_row(
                index,
                event,
                state.expanded.contains(&index),
                &tz,
            ));
        }
        if day.events.is_empty() {
            list = list.push(
                container(kit::secondary(tr("history.empty", &[])).size(13))
                    .center_x(Length::Fill)
                    .padding(28),
            );
        }
        body = body.push(kit::card(None, list));
    }
    kit::page(tr("nav.history", &[]), body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Reply;
    use cg_i18n::Lang;
    use cg_ipc::Request;
    use cg_ipc::health::{DayStart, EventKind, EventPhase, HealthEvent, HealthSnapshot};
    use jiff::tz::TimeZone;
    use jiff::{SignedDuration, Timestamp};

    fn days(from: i64, to: i64) -> Vec<String> {
        (from..=to).map(|d| format!("2026-10-{d}")).collect()
    }

    fn event(secs: i64, kind: EventKind) -> HealthEvent {
        HealthEvent::new(Timestamp::from_second(secs).unwrap(), kind)
    }

    fn with_snapshot(mut e: HealthEvent) -> HealthEvent {
        e.phase = Some(EventPhase::Start);
        e.snapshot = Some(HealthSnapshot::default());
        e
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

    fn state_on(current: Option<&str>, loaded: HistoryDay) -> State {
        State {
            day: current.map(str::to_owned),
            loaded: Some(loaded),
            ..State::default()
        }
    }

    fn history(day: Option<&str>) -> Effect {
        Effect::Request(
            Request::History {
                day: day.map(str::to_owned),
            },
            Reply::History,
        )
    }

    /// A context whose clock reads noon on `date` in this machine's zone.
    fn ctx_on(date: &str) -> Ctx<'static> {
        let mut ctx = Ctx::for_test(None);
        ctx.now = date
            .parse::<jiff::civil::Date>()
            .unwrap()
            .at(12, 0, 0, 0)
            .to_zoned(TimeZone::system())
            .unwrap()
            .timestamp();
        ctx
    }

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        guard
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
    fn prev_requests_that_day_and_collapses_rows() {
        let ctx = ctx_on("2026-10-19");
        let mut state = state_on(None, day("2026-10-19", days(13, 19), vec![]));
        state.expanded.insert(2);
        assert_eq!(
            update(&mut state, Msg::Prev, &ctx),
            history(Some("2026-10-18"))
        );
        assert!(state.expanded.is_empty());
        assert!(state.loading);
        // Forward to the last day follows today again.
        let mut state = state_on(Some("2026-10-18"), day("2026-10-18", days(13, 19), vec![]));
        assert_eq!(update(&mut state, Msg::Next, &ctx), history(None));
        assert_eq!(state.day, None);
        // Nothing before the oldest day.
        let mut state = state_on(Some("2026-10-13"), day("2026-10-13", days(13, 19), vec![]));
        assert_eq!(update(&mut state, Msg::Prev, &ctx), Effect::None);
    }

    #[test]
    fn refresh_only_for_today() {
        let ctx = ctx_on("2026-10-19");
        let list = days(13, 19);
        let mut old = state_on(Some("2026-10-15"), day("2026-10-15", list.clone(), vec![]));
        assert_eq!(update(&mut old, Msg::Refresh, &ctx), Effect::None);
        let mut today = state_on(None, day("2026-10-19", list.clone(), vec![]));
        assert_eq!(update(&mut today, Msg::Refresh, &ctx), history(None));
        assert!(today.loading);
        // Already loading: no second request.
        assert_eq!(update(&mut today, Msg::Refresh, &ctx), Effect::None);
    }

    #[test]
    fn midnight_moves_today_forward() {
        let ctx = Ctx::for_test(None);
        // Viewing the oldest day; the server's window has moved on.
        let mut state = State {
            day: Some("2026-10-13".into()),
            loading: true,
            ..State::default()
        };
        let effect = update(
            &mut state,
            Msg::Loaded(Ok(day("2026-10-13", days(14, 20), vec![]))),
            &ctx,
        );
        assert_eq!(effect, history(None));
        assert_eq!(state.day, None);
        assert!(state.loading);
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn midnight_keeps_a_day_that_is_still_in_the_window() {
        let mut state = state_on(Some("2026-10-18"), day("2026-10-18", days(13, 19), vec![]));
        assert_eq!(
            update(&mut state, Msg::Refresh, &ctx_on("2026-10-20")),
            history(Some("2026-10-18"))
        );
        assert_eq!(state.day.as_deref(), Some("2026-10-18"));
        // Following today stays following today.
        let mut state = state_on(None, day("2026-10-19", days(13, 19), vec![]));
        assert_eq!(
            update(&mut state, Msg::Refresh, &ctx_on("2026-10-20")),
            history(None)
        );
        // A day that has left the window gives way to today.
        let mut state = state_on(Some("2026-10-13"), day("2026-10-13", days(13, 19), vec![]));
        assert_eq!(
            update(&mut state, Msg::Refresh, &ctx_on("2026-10-20")),
            history(None)
        );
        assert_eq!(state.day, None);
    }

    #[test]
    fn shown_and_retry_never_ask_outside_the_window() {
        let mut state = state_on(Some("2026-10-13"), day("2026-10-13", days(13, 19), vec![]));
        assert_eq!(
            update(&mut state, Msg::Shown, &ctx_on("2026-10-20")),
            history(None)
        );
        let mut state = state_on(Some("2026-10-14"), day("2026-10-14", days(13, 19), vec![]));
        assert_eq!(
            update(&mut state, Msg::Shown, &ctx_on("2026-10-20")),
            history(Some("2026-10-14"))
        );
    }

    #[test]
    fn only_rows_with_a_snapshot_expand() {
        let ctx = Ctx::for_test(None);
        let events = vec![
            event(100, EventKind::UsbAttached),
            with_snapshot(event(200, EventKind::Issue)),
        ];
        let mut state = state_on(None, day("2026-10-19", days(13, 19), events));
        assert_eq!(update(&mut state, Msg::Toggle(0), &ctx), Effect::None);
        assert!(state.expanded.is_empty());
        update(&mut state, Msg::Toggle(9), &ctx);
        assert!(state.expanded.is_empty());
        update(&mut state, Msg::Toggle(1), &ctx);
        assert!(state.expanded.contains(&1));
        update(&mut state, Msg::Toggle(1), &ctx);
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn a_failed_load_ends_loading_and_keeps_a_retry() {
        let _lock = en();
        let ctx = Ctx::for_test(None);
        let mut state = State::default();
        assert_eq!(update(&mut state, Msg::Shown, &ctx), history(None));
        assert!(state.loading);
        assert_eq!(
            update(&mut state, Msg::Loaded(Err("service is down".into())), &ctx),
            Effect::None
        );
        assert!(!state.loading);
        assert_eq!(state.error.as_deref(), Some("service is down"));
        let _ = view(&state, &ctx);
        // The retry button asks again.
        assert_eq!(update(&mut state, Msg::Shown, &ctx), history(None));
    }

    #[test]
    fn the_page_recovers_when_the_service_comes_back() {
        let ctx = ctx_on("2026-10-19");
        let mut state = State::default();
        update(&mut state, Msg::Shown, &ctx);
        update(&mut state, Msg::Loaded(Err("down".into())), &ctx);
        // The next refresh asks again on its own; the good answer clears the error.
        assert_eq!(update(&mut state, Msg::Refresh, &ctx), history(None));
        let good = day(
            "2026-10-19",
            days(13, 19),
            vec![event(5, EventKind::UsbAttached)],
        );
        update(&mut state, Msg::Loaded(Ok(good.clone())), &ctx);
        assert_eq!((state.loading, state.error.as_deref()), (false, None));
        assert_eq!(state.loaded, Some(good));
    }

    #[test]
    fn a_late_answer_for_another_day_is_ignored() {
        let ctx = Ctx::for_test(None);
        let mut state = state_on(Some("2026-10-17"), day("2026-10-18", days(13, 19), vec![]));
        state.loading = true;
        let stale = day("2026-10-18", days(13, 19), vec![]);
        update(&mut state, Msg::Loaded(Ok(stale)), &ctx);
        assert!(state.loading);
        assert_eq!(state.loaded.as_ref().unwrap().day, "2026-10-18");
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
        let strip = strip_for(&record, begin + SignedDuration::from_hours(5), &tz);
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
        end.cause = Some(cg_ipc::health::IssueCause::Channel);
        end.duration = Some(65);
        let (time, title, tone) = row_texts(&end, &tz);
        assert_eq!(time, "03:05");
        assert_eq!(title, "Recovered: Cable connection dropped · 1 min 5 sec");
        assert_eq!(tone, Tone::Green);
        assert_eq!(
            legend_labels(),
            ["OK", "Unstable", "Interrupted", "Not sharing"]
        );
    }

    #[test]
    fn views_build_with_and_without_a_day() {
        let _lock = en();
        let ctx = Ctx::for_test(None);
        let _ = view(&State::default(), &ctx);
        let events = vec![
            with_snapshot(event(100, EventKind::Issue)),
            event(50, EventKind::UsbAttached),
        ];
        let mut state = state_on(None, day("2026-10-19", days(13, 19), events));
        state.expanded.insert(0);
        let _ = view(&state, &ctx);
        let _ = view(
            &state_on(None, day("2026-10-19", days(13, 19), vec![])),
            &ctx,
        );
    }
}
