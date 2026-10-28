//! The speed test page, after the Mac's `SpeedTestPage` and `SpeedTestController`: the phone
//! through CellGlide and, to compare, this computer's own Wi‑Fi or Ethernet, side by side with
//! live curves, a one-line comparison and the recent results. The test runs in this process
//! (`cg_tools::speed::session`); leaving the page doesn't stop it.

use super::Ctx;
use crate::effect::{Effect, HistoryOp};
use crate::prefs::PrefChange;
use crate::widgets::curve::{self, Curve};
use crate::widgets::kit::{self, Tone};
use cg_i18n::tr;
use cg_ipc::describe;
use cg_tools::speed::session::{self, Run};
use cg_tools::speed::text::{direction, error_text, verdict_line};
use cg_tools::speed::{
    HistoryStore, Note, Params, Plan, Progress, Record, Route, Server, SpeedError, SpeedResult,
    Stage, Summary, history::LIMIT, needs_confirmation, plan,
};
use futures::{SinkExt, Stream};
use iced::widget::{
    button, center, checkbox, column, container, opaque, progress_bar, row, rule, space, stack,
    text,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Theme};
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// One run as the app starts it: the plan, its routes with the names shown, and the phone's
/// carrier and radio for the history (unknown stays `None`).
#[derive(Clone, Debug, PartialEq)]
pub struct SpeedJob {
    pub plan: Plan,
    pub routes: Vec<(Route, String)>,
    pub carrier: Option<String>,
    pub generation: Option<String>,
}

#[derive(Debug)]
pub struct State {
    /// The history file; only `apply_history` (run off the UI thread) touches it.
    history_path: PathBuf,
    /// The recent results as shown, newest first; replaced by what the file says once saved.
    records: Vec<Record>,
    /// The last save of the history failed.
    history_failed: bool,
    /// History writes asked for whose `HistorySaved` hasn't come back. They run in order, so
    /// only the reply that leaves none outstanding says what the file holds now; the earlier
    /// ones are stale next to the optimistic `records`.
    history_pending: u32,
    /// This computer's own Wi‑Fi or Ethernet, refreshed every 5 s while the page is shown.
    uplink: Option<String>,
    /// The data warning is up.
    confirming: bool,
    /// "Don't ask again" is ticked on it.
    dont_ask: bool,
    /// The user went through the warning once since launch.
    confirmed_this_launch: bool,
    confirming_clear: bool,
    /// Why Start did nothing.
    notice: Option<String>,
    running: Option<SpeedJob>,
    /// One per route of the last run, live while it runs.
    legs: Vec<LegView>,
    last: Option<Run>,
    /// Whether the last snapshot had the phone connected; a change clears `notice`.
    phone: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LegView {
    pub label: String,
    pub phone: bool,
    pub progress: Progress,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Uplink(Option<String>),
    IncludeWifi(bool),
    Start,
    DontAsk(bool),
    Confirm,
    CancelWarning,
    Cancel,
    Progress {
        index: usize,
        progress: Progress,
    },
    Finished(Run),
    /// The history write is done: the records the file holds now, or why it failed.
    HistorySaved(Result<Vec<Record>, String>),
    AskClear,
    Clear,
    CancelClear,
}

/// Where a page built with `Default` keeps its history: nowhere real. The app passes the
/// user's file (`cg_tools::speed::default_path()`) to `State::new` itself.
pub const UNUSED_HISTORY: &str = "/nonexistent/cellglide/speedtests.json";

impl Default for State {
    fn default() -> Self {
        State::new(PathBuf::from(UNUSED_HISTORY))
    }
}

/// Which dialog covers the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    DataWarning,
    ClearHistory,
}

pub fn dialog(state: &State) -> Option<Dialog> {
    if state.confirming {
        Some(Dialog::DataWarning)
    } else if state.confirming_clear {
        Some(Dialog::ClearHistory)
    } else {
        None
    }
}

/// Whether Start can be pressed, and the sentence beside it. With nothing to test (no phone,
/// no Wi‑Fi or Ethernet) it stays disabled and says to connect the phone, as on the Mac.
pub fn start_line(state: &State, ctx: &Ctx) -> (bool, Option<String>) {
    if state.running.is_some() {
        return (true, None);
    }
    let phone = ctx.phone_connected();
    if plan(phone, state.uplink.clone(), ctx.prefs.include_wifi).is_none() {
        return (false, Some(error_text(&SpeedError::NotConnected)));
    }
    if let Some(notice) = &state.notice {
        return (true, Some(notice.clone()));
    }
    (true, (!phone).then(|| tr("speed.wifi_only", &[])))
}

/// The comparison, once the run is over and both legs finished.
pub fn verdict(state: &State) -> Option<String> {
    if state.running.is_some() {
        return None;
    }
    let download = |phone: bool| {
        state
            .legs
            .iter()
            .find(|leg| leg.phone == phone && leg.progress.stage == Stage::Done)
            .and_then(|leg| leg.progress.result.download)
    };
    verdict_line(download(true), download(false))
}

impl State {
    pub fn new(history_path: PathBuf) -> State {
        State {
            records: HistoryStore::open(history_path.clone()).records().to_vec(),
            history_path,
            history_failed: false,
            history_pending: 0,
            uplink: None,
            confirming: false,
            dont_ask: false,
            confirmed_this_launch: false,
            confirming_clear: false,
            notice: None,
            running: None,
            legs: Vec::new(),
            last: None,
            phone: false,
        }
    }

    pub fn history_path(&self) -> &std::path::Path {
        &self.history_path
    }

    /// The warning is about phone data: with the phone gone it no longer applies.
    pub fn on_snapshot(&mut self, ctx: &Ctx) {
        // As on the Mac: a sentence about the phone is stale once it comes or goes.
        let phone = ctx.phone_connected();
        if phone != self.phone {
            self.phone = phone;
            self.notice = None;
        }
        if self.confirming && !phone {
            self.confirming = false;
            self.dont_ask = false;
            self.notice = None;
        }
    }

    /// Plans a run and starts it, or shows the warning first when `ask` and the plan needs it.
    fn start(&mut self, ctx: &Ctx, ask: bool) -> Effect {
        self.notice = None;
        let Some(plan) = plan(
            ctx.phone_connected(),
            self.uplink.clone(),
            ctx.prefs.include_wifi,
        ) else {
            self.notice = Some(error_text(&SpeedError::NotConnected));
            return Effect::None;
        };
        let confirmed = ctx.prefs.speed_confirmed || self.confirmed_this_launch;
        if ask && needs_confirmation(&plan, confirmed) {
            self.confirming = true;
            return Effect::None;
        }
        // P17: the carrier and radio as reported, never the card's placeholder title.
        let card = plan
            .phone
            .then(|| ctx.snapshot.and_then(|s| s.phone.as_ref()))
            .flatten()
            .map(describe::card_text);
        let job = SpeedJob {
            routes: session::labelled_routes(&plan),
            carrier: card.as_ref().and_then(|c| c.carrier.clone()),
            generation: card.and_then(|c| c.radio),
            plan,
        };
        self.legs = job
            .routes
            .iter()
            .map(|(route, label)| LegView {
                label: label.clone(),
                phone: *route == Route::Phone,
                progress: Progress::default(),
            })
            .collect();
        self.last = None;
        self.running = Some(job.clone());
        Effect::StartSpeed(job)
    }

    #[cfg(test)]
    pub fn records_for_test(&self) -> &[Record] {
        &self.records
    }

    /// The data warning is up.
    #[cfg(test)]
    pub fn warning_shown(&self) -> bool {
        self.confirming
    }

    /// Takes the finished run; the history write it asks for is the app's to do.
    fn finish(&mut self, run: Run, ctx: &Ctx) -> Effect {
        let Some(job) = self.running.take() else {
            return Effect::None;
        };
        let record = session::history_record(&job.plan, &run, ctx.now, job.carrier, job.generation);
        if let Some(record) = &record {
            self.records.insert(0, record.clone());
            self.records.truncate(LIMIT);
        }
        // The legs as they ended; one cut short by a cancel says so.
        for (index, view) in self.legs.iter_mut().enumerate() {
            match run.legs.get(index) {
                Some(leg) => {
                    view.progress.stage = leg.stage.clone();
                    view.progress.result = leg.result.clone();
                }
                None if !matches!(view.progress.stage, Stage::Done | Stage::Failed(_)) => {
                    view.progress.stage = Stage::Cancelled;
                }
                None => {}
            }
        }
        self.last = Some(run);
        record.map_or(Effect::None, |record| {
            self.history_pending += 1;
            Effect::SpeedHistory(HistoryOp::Add(record))
        })
    }
}

/// Carries out `op` on the history file, blocking; the app runs it on a blocking thread.
/// Returns the records the file holds afterwards.
pub fn apply_history(path: PathBuf, op: HistoryOp) -> Result<Vec<Record>, String> {
    let mut store = HistoryStore::open(path);
    match op {
        HistoryOp::Add(record) => store.add(record),
        HistoryOp::Clear => store.clear(),
    }
    .map_err(|error| error.to_string())?;
    Ok(store.records().to_vec())
}

pub fn update(state: &mut State, message: Msg, ctx: &Ctx) -> Effect {
    match message {
        Msg::Uplink(uplink) => state.uplink = uplink,
        Msg::IncludeWifi(on) if state.running.is_none() => {
            return Effect::Prefs(PrefChange::IncludeWifi(on));
        }
        Msg::IncludeWifi(_) => {}
        Msg::Start if state.running.is_none() && !state.confirming => {
            return state.start(ctx, true);
        }
        Msg::Start => {}
        Msg::DontAsk(on) => state.dont_ask = on,
        // Only from the warning; it may have closed itself since (the phone left).
        Msg::Confirm if state.confirming => {
            state.confirming = false;
            let dont_ask = std::mem::take(&mut state.dont_ask);
            let phone = plan(
                ctx.phone_connected(),
                state.uplink.clone(),
                ctx.prefs.include_wifi,
            )
            .is_some_and(|p| p.phone);
            if !phone {
                return state.start(ctx, true);
            }
            state.confirmed_this_launch = true;
            let start = state.start(ctx, false);
            if dont_ask {
                return Effect::Batch(vec![Effect::Prefs(PrefChange::SpeedConfirmed), start]);
            }
            return start;
        }
        Msg::Confirm => {}
        Msg::CancelWarning => {
            state.confirming = false;
            state.dont_ask = false;
        }
        Msg::Cancel if state.running.is_some() => return Effect::CancelSpeed,
        Msg::Cancel => {}
        Msg::Progress { index, progress } => {
            if state.running.is_some()
                && let Some(leg) = state.legs.get_mut(index)
            {
                leg.progress = progress;
            }
        }
        Msg::Finished(run) => return state.finish(run, ctx),
        Msg::HistorySaved(result) => {
            state.history_pending = state.history_pending.saturating_sub(1);
            match result {
                Ok(records) if state.history_pending == 0 => {
                    state.history_failed = false;
                    state.records = records;
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("cellglide-gui: can't save the speed test history: {error}");
                    state.history_failed = true;
                }
            }
        }
        Msg::AskClear if state.running.is_none() => state.confirming_clear = true,
        Msg::AskClear => {}
        Msg::Clear if state.confirming_clear => {
            state.confirming_clear = false;
            state.records.clear();
            state.history_pending += 1;
            return Effect::SpeedHistory(HistoryOp::Clear);
        }
        Msg::Clear => {}
        Msg::CancelClear => state.confirming_clear = false,
    }
    Effect::None
}

/// Runs `job` against Cloudflare: `Progress` every half second, then `Finished`. Cancelling
/// `cancel` stops it; the run then reports `cancelled` and records nothing.
pub fn job_stream(job: SpeedJob, cancel: CancellationToken) -> impl Stream<Item = Msg> {
    stream_with(job.routes, cancel, |route, tx, cancel| {
        cg_tools::speed::measure(route, Server::cloudflare(), Params::default(), tx, cancel)
    })
}

/// `job_stream` with the measuring passed in, so tests never touch the network.
fn stream_with<M, F>(
    routes: Vec<(Route, String)>,
    cancel: CancellationToken,
    measure: M,
) -> impl Stream<Item = Msg>
where
    M: FnMut(Route, watch::Sender<Progress>, CancellationToken) -> F + Send + 'static,
    F: Future<Output = SpeedResult> + Send,
{
    iced::stream::channel(32, async move |mut output| {
        let mut updates = output.clone();
        let run = session::run_legs(routes, &cancel, measure, |index, _label, progress| {
            // A full queue drops this one; the next comes half a second later.
            let _ = updates.try_send(Msg::Progress {
                index,
                progress: progress.clone(),
            });
        })
        .await;
        let _ = output.send(Msg::Finished(run)).await;
    })
}

fn tone(phone: bool) -> Tone {
    if phone { Tone::Blue } else { Tone::Green }
}

/// A coloured dot and the route's name: iPhone blue, Wi‑Fi or Ethernet green.
fn route_name<'a>(name: String, phone: bool, size: u32) -> Element<'a, Msg> {
    row![kit::dot(tone(phone), 7.0), text(name).size(size)]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

fn red(content: String) -> text::Text<'static> {
    text(content).size(12).style(|_: &Theme| text::Style {
        color: Some(Tone::Red.color()),
    })
}

fn orange(content: String) -> text::Text<'static> {
    text(content).size(11).style(|_: &Theme| text::Style {
        color: Some(Tone::Orange.color()),
    })
}

/// What a leg is doing now; nothing once it finished or before its turn.
fn stage_text(progress: &Progress) -> Option<String> {
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

fn note_text(note: &Note) -> String {
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

/// One route's numbers, curve, progress and stage, live while it runs.
fn leg_column<'a>(leg: &'a LegView, running: bool) -> Element<'a, Msg> {
    let mut col = column![route_name(leg.label.clone(), leg.phone, 13)].spacing(8);
    if let Stage::Failed(error) = &leg.progress.stage {
        col = col.push(red(error_text(error)));
        return col.width(Length::Fill).into();
    }
    let result = &leg.progress.result;
    let mut figures = column![].spacing(4);
    for (key, value) in [
        (
            "speed.latency",
            result.latency.map(|ms| format!("{ms:.0} ms")),
        ),
        (
            "speed.download",
            result.download.map(|v| format!("{v:.1} Mbps")),
        ),
        (
            "speed.upload",
            result.upload.map(|v| format!("{v:.1} Mbps")),
        ),
    ] {
        if let Some(value) = value {
            figures = figures.push(
                row![
                    kit::secondary(tr(key, &[]))
                        .size(13)
                        .width(Length::Fixed(64.0)),
                    text(value).size(13)
                ]
                .spacing(12),
            );
        }
    }
    col = col.push(figures).push(curve::view(
        Curve {
            series: result.download_series.clone(),
            tone: tone(leg.phone),
        },
        32.0,
    ));
    let measuring = matches!(
        leg.progress.stage,
        Stage::Latency | Stage::Download | Stage::Upload
    );
    if running && measuring {
        col = col
            .push(progress_bar(0.0..=1.0, leg.progress.progress.clamp(0.0, 1.0) as f32).girth(4));
    }
    if let Some(stage) = stage_text(&leg.progress) {
        col = col.push(kit::secondary(stage));
    }
    for note in &result.notes {
        col = col.push(orange(note_text(note)));
    }
    col.width(Length::Fill).into()
}

fn results<'a>(state: &'a State) -> Element<'a, Msg> {
    let running = state.running.is_some();
    let mut columns = row![].spacing(16);
    if !state.legs.iter().any(|leg| leg.phone) {
        columns = columns.push(
            column![
                route_name(tr("speed.phone", &[]), true, 13),
                kit::secondary(tr("speed.no_phone", &[])).size(13)
            ]
            .spacing(8)
            .width(Length::Fill),
        );
    }
    for (index, leg) in state.legs.iter().enumerate() {
        if index > 0 || !leg.phone {
            columns = columns.push(rule::vertical(1));
        }
        columns = columns.push(leg_column(leg, running));
    }
    let mut body = column![columns].spacing(10);
    if let Some(line) = verdict(state) {
        body = body.push(rule::horizontal(1)).push(text(line).size(13));
    }
    kit::card(None, body)
}

fn history<'a>(state: &'a State, zone: &jiff::tz::TimeZone) -> Element<'a, Msg> {
    let records = state.records.as_slice();
    let mut clear = button(text(tr("common.clear", &[])).size(12)).style(button::text);
    if state.running.is_none() {
        clear = clear.on_press(Msg::AskClear);
    }
    let mut head =
        row![text(tr("speed.recent", &[])).size(13).font(kit::SEMIBOLD)].align_y(Alignment::Center);
    head = head.push(space::horizontal());
    if !records.is_empty() {
        head = head.push(clear);
    }
    let mut body = column![head].spacing(8);
    if state.history_failed {
        body = body.push(kit::secondary(tr("speed.history_failed", &[])).size(12));
    }
    if records.is_empty() {
        body = body.push(kit::secondary(tr("speed.history_empty", &[])).size(13));
        return kit::card(None, body);
    }
    let cell = |content: Element<'a, Msg>, portion: u16| {
        container(content).width(Length::FillPortion(portion))
    };
    body = body.push(
        row![
            cell(space().into(), 3),
            cell(route_name(tr("speed.phone", &[]), true, 11), 4),
            cell(route_name(tr("speed.wifi_short", &[]), false, 11), 4),
        ]
        .spacing(16),
    );
    for record in records {
        let mut when = column![
            text(
                record
                    .date
                    .to_zoned(zone.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            )
            .size(12)
        ]
        .spacing(2);
        let network: Vec<&str> = [&record.carrier, &record.generation]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if !network.is_empty() {
            when = when.push(kit::secondary(network.join(" · ")).size(11));
        }
        let leg = |summary: &Option<Summary>| -> Element<'a, Msg> {
            text(summary.as_ref().map(summary_text).unwrap_or_default())
                .size(12)
                .into()
        };
        body = body.push(rule::horizontal(1)).push(
            row![
                cell(when.into(), 3),
                cell(leg(&record.phone), 4),
                cell(leg(&record.wifi), 4),
            ]
            .spacing(16),
        );
    }
    kit::card(None, body)
}

fn controls<'a>(state: &'a State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let phone = ctx.phone_connected();
    let running = state.running.is_some();
    let mut col = column![].spacing(12);
    // Without an iPhone the Wi‑Fi or Ethernet is the whole test: nothing to switch.
    if phone && let Some(iface) = &state.uplink {
        let mut include = checkbox(ctx.prefs.include_wifi)
            .label(tr("speed.include_wifi", &[("iface", iface)]))
            .text_size(13);
        if !running {
            include = include.on_toggle(Msg::IncludeWifi);
        }
        col = col.push(include);
    }
    let (enabled, hint) = start_line(state, ctx);
    let action = if running {
        button(text(tr("common.cancel", &[])).size(13)).on_press(Msg::Cancel)
    } else {
        button(text(tr("speed.start", &[])).size(13))
            .on_press_maybe(enabled.then_some(Msg::Start))
            .style(button::primary)
    };
    let mut line = row![action].spacing(10).align_y(Alignment::Center);
    if let Some(hint) = hint {
        line = line.push(kit::secondary(hint).size(13));
    }
    col.push(line).into()
}

/// A dialog over the page: the page dimmed and blocked, the box in the middle.
fn overlay<'a>(page: Element<'a, Msg>, content: Element<'a, Msg>) -> Element<'a, Msg> {
    let dialog = container(content)
        .padding(20)
        .max_width(360)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(Background::Color(palette.background.base.color)),
                border: Border {
                    color: palette.background.base.text.scale_alpha(0.15),
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..container::Style::default()
            }
        });
    let backdrop = center(dialog).style(|_: &Theme| container::Style {
        background: Some(Background::Color(Color::BLACK.scale_alpha(0.3))),
        ..container::Style::default()
    });
    stack![page, opaque(backdrop)].into()
}

fn dialog_buttons<'a>(
    cancel: Msg,
    confirm: (String, Msg),
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'a, Msg> {
    row![
        space::horizontal(),
        button(text(tr("common.cancel", &[])).size(13))
            .on_press(cancel)
            .style(button::secondary),
        button(text(confirm.0).size(13))
            .on_press(confirm.1)
            .style(style),
    ]
    .spacing(8)
    .into()
}

fn data_warning<'a>(state: &State) -> Element<'a, Msg> {
    column![
        row![
            kit::dot(Tone::Orange, 10.0),
            text(tr("speed.warning", &[])).size(14).font(kit::SEMIBOLD)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        checkbox(state.dont_ask)
            .label(tr("speed.dont_ask", &[]))
            .text_size(13)
            .on_toggle(Msg::DontAsk),
        dialog_buttons(
            Msg::CancelWarning,
            (tr("common.continue", &[]), Msg::Confirm),
            button::primary,
        ),
    ]
    .spacing(14)
    .into()
}

fn clear_dialog<'a>() -> Element<'a, Msg> {
    column![
        text(tr("speed.clear_confirm", &[]))
            .size(14)
            .font(kit::SEMIBOLD),
        dialog_buttons(
            Msg::CancelClear,
            (tr("common.clear", &[]), Msg::Clear),
            button::danger,
        ),
    ]
    .spacing(14)
    .into()
}

pub fn view<'a>(state: &'a State, ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let mut body = column![
        kit::secondary(tr("speed.page_about", &[])).size(13),
        controls(state, ctx)
    ]
    .spacing(18);
    if !state.legs.is_empty() {
        body = body.push(results(state));
    }
    body = body.push(history(state, &jiff::tz::TimeZone::system()));
    let page = kit::page(tr("nav.speed_test", &[]), body);
    match dialog(state) {
        Some(Dialog::DataWarning) => overlay(page, data_warning(state)),
        Some(Dialog::ClearHistory) => overlay(page, clear_dialog()),
        None => page,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures::{connected_snapshot, disconnected_snapshot};
    use crate::prefs::Prefs;
    use cg_tools::speed::session::Leg;
    use cg_tools::speed::text::error_text;
    use cg_tools::speed::{Route, SpeedError, SpeedResult, Stage};
    use std::sync::MutexGuard;

    fn zh() -> MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        guard
    }

    /// A page whose history lives in a fresh temporary directory.
    fn page(uplink: Option<&str>) -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::new(dir.path().join("cellglide/speedtests.json"));
        state.uplink = uplink.map(Into::into);
        (dir, state)
    }

    /// "Don't ask again" was ticked in an earlier launch.
    fn confirmed(snapshot: cg_ipc::Snapshot) -> Ctx<'static> {
        Ctx {
            prefs: Box::leak(Box::new(Prefs {
                speed_confirmed: true,
                ..Prefs::default()
            })),
            ..Ctx::for_test(Some(snapshot))
        }
    }

    fn started(effect: &Effect) -> Option<&SpeedJob> {
        match effect {
            Effect::StartSpeed(job) => Some(job),
            Effect::Batch(effects) => effects.iter().find_map(started),
            _ => None,
        }
    }

    fn done(download: f64) -> SpeedResult {
        SpeedResult {
            latency: Some(40.0),
            download: Some(download),
            upload: Some(download / 4.0),
            ..SpeedResult::default()
        }
    }

    fn leg(route: Route, stage: Stage, result: SpeedResult) -> Leg {
        Leg {
            route,
            label: String::new(),
            stage,
            result,
        }
    }

    #[test]
    fn first_phone_test_asks_then_starts_with_bound_routes_only() {
        let _lang = zh();
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        assert!(ctx.prefs.include_wifi && !ctx.prefs.speed_confirmed);
        assert_eq!(update(&mut state, Msg::Start, &ctx), Effect::None);
        assert!(state.confirming);
        let effect = update(&mut state, Msg::Confirm, &ctx);
        let Effect::StartSpeed(job) = &effect else {
            panic!("expected a start, got {effect:?}");
        };
        assert!(!state.confirming);
        assert_eq!(
            job.routes,
            [
                (Route::Phone, "iPhone".to_string()),
                (
                    Route::Interface("wlp2s0".into()),
                    "Wi‑Fi 或有线（wlp2s0）".to_string()
                ),
            ]
        );
        assert!(job.plan.phone);
        // P17: the carrier and radio as the phone reports them, not the card's title.
        assert_eq!(job.carrier.as_deref(), Some("中国移动"));
        assert_eq!(job.generation.as_deref(), Some("5G"));
        assert!(state.running.is_some());
        // A second start while one runs does nothing.
        assert_eq!(update(&mut state, Msg::Start, &ctx), Effect::None);
        // Once finished, the next start in the same launch doesn't ask again.
        let _ = update(
            &mut state,
            Msg::Finished(Run {
                legs: vec![],
                cancelled: true,
            }),
            &ctx,
        );
        let effect = update(&mut state, Msg::Start, &ctx);
        assert!(matches!(effect, Effect::StartSpeed(_)), "{effect:?}");
        assert!(!state.confirming);
    }

    #[test]
    fn dont_ask_again_is_saved() {
        let (_dir, mut state) = page(None);
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        assert!(state.confirming);
        let _ = update(&mut state, Msg::DontAsk(true), &ctx);
        let effect = update(&mut state, Msg::Confirm, &ctx);
        let Effect::Batch(effects) = &effect else {
            panic!("expected a batch, got {effect:?}");
        };
        assert!(effects.contains(&Effect::Prefs(PrefChange::SpeedConfirmed)));
        assert!(started(&effect).is_some());
        // Without the tick, nothing is saved.
        let (_dir, mut state) = page(None);
        let _ = update(&mut state, Msg::Start, &ctx);
        let effect = update(&mut state, Msg::Confirm, &ctx);
        assert!(matches!(effect, Effect::StartSpeed(_)), "{effect:?}");
    }

    #[test]
    fn saved_confirmation_never_asks() {
        let (_dir, mut state) = page(None);
        let ctx = confirmed(connected_snapshot());
        let effect = update(&mut state, Msg::Start, &ctx);
        assert!(matches!(effect, Effect::StartSpeed(_)), "{effect:?}");
    }

    #[test]
    fn cancelling_the_warning_starts_nothing() {
        let (_dir, mut state) = page(None);
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = update(&mut state, Msg::DontAsk(true), &ctx);
        assert_eq!(update(&mut state, Msg::CancelWarning, &ctx), Effect::None);
        assert!(!state.confirming && state.running.is_none());
        // Asked again next time, and the old tick is gone.
        let _ = update(&mut state, Msg::Start, &ctx);
        assert!(state.confirming && !state.dont_ask);
    }

    #[test]
    fn wifi_only_never_asks() {
        let _lang = zh();
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let effect = update(&mut state, Msg::Start, &ctx);
        let Effect::StartSpeed(job) = &effect else {
            panic!("expected a start, got {effect:?}");
        };
        assert!(!job.plan.phone);
        assert_eq!(
            job.routes,
            [(
                Route::Interface("wlp2s0".into()),
                "Wi‑Fi 或有线（wlp2s0）".to_string()
            )]
        );
        assert_eq!((job.carrier.clone(), job.generation.clone()), (None, None));
        assert!(!state.confirming);
    }

    #[test]
    fn nothing_to_test_says_connect_the_phone() {
        let _lang = zh();
        let (_dir, mut state) = page(None);
        for snapshot in [None, Some(disconnected_snapshot())] {
            let ctx = Ctx::for_test(snapshot);
            assert_eq!(update(&mut state, Msg::Start, &ctx), Effect::None);
            assert_eq!(
                state.notice.as_deref(),
                Some(error_text(&SpeedError::NotConnected).as_str())
            );
            assert!(!state.confirming && state.running.is_none());
        }
    }

    #[test]
    fn phone_leaving_closes_the_data_warning() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let connected = Ctx::for_test(Some(connected_snapshot()));
        let _ = update(&mut state, Msg::Start, &connected);
        assert!(state.confirming);
        state.notice = Some("old".into());
        // A snapshot with the phone still there changes nothing.
        state.on_snapshot(&connected);
        assert!(state.confirming);
        let gone = Ctx::for_test(Some(disconnected_snapshot()));
        state.on_snapshot(&gone);
        assert!(!state.confirming);
        assert_eq!(state.notice, None);
        // A late Confirm (the click raced the snapshot) starts no phone leg.
        let effect = update(&mut state, Msg::Confirm, &gone);
        assert!(
            started(&effect).is_none_or(|job| !job.plan.phone),
            "{effect:?}"
        );
        // The service going away entirely closes it too.
        let _ = update(&mut state, Msg::Start, &connected);
        assert!(state.confirming);
        state.on_snapshot(&Ctx::for_test(None));
        assert!(!state.confirming);
    }

    #[test]
    fn unplug_mid_run_records_nothing() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = confirmed(connected_snapshot());
        assert!(started(&update(&mut state, Msg::Start, &ctx)).is_some());
        let run = Run {
            legs: vec![
                leg(
                    Route::Phone,
                    Stage::Failed(SpeedError::NotConnected),
                    SpeedResult::default(),
                ),
                leg(Route::Interface("wlp2s0".into()), Stage::Done, done(60.0)),
            ],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(run.clone()), &ctx);
        assert!(state.records.is_empty());
        assert!(state.running.is_none());
        assert_eq!(state.last, Some(run));
        // The phone column shows why it stopped; Wi‑Fi has its numbers.
        assert_eq!(
            state.legs[0].progress.stage,
            Stage::Failed(SpeedError::NotConnected)
        );
        assert_eq!(state.legs[1].progress.result.download, Some(60.0));
    }

    #[test]
    fn speed_finish_returns_history_effect() {
        let (dir, mut state) = page(Some("wlp2s0"));
        let ctx = confirmed(connected_snapshot());
        let _ = update(&mut state, Msg::Start, &ctx);
        let both = Run {
            legs: vec![
                leg(Route::Phone, Stage::Done, done(20.0)),
                leg(Route::Interface("wlp2s0".into()), Stage::Done, done(60.0)),
            ],
            cancelled: false,
        };
        let effect = update(&mut state, Msg::Finished(both), &ctx);
        let Effect::SpeedHistory(HistoryOp::Add(record)) = effect else {
            panic!("expected SpeedHistory(Add), got {effect:?}");
        };
        assert_eq!(state.records, vec![record]);
        assert!(!dir.path().join("cellglide/speedtests.json").exists());
        // A failed save shows the sentence; a good one replaces the list.
        let _ = update(&mut state, Msg::HistorySaved(Err("disk full".into())), &ctx);
        assert!(state.history_failed);
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::HistorySaved(Ok(Vec::new())), &ctx);
        assert!(!state.history_failed && state.records.is_empty());
    }

    /// Add then Clear in quick succession: the file ends empty, and the Add's late reply
    /// (the file as it was after the Add) doesn't bring the record back.
    #[test]
    fn add_then_clear_ends_empty_and_a_late_reply_is_ignored() {
        let (dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let wifi = Run {
            legs: vec![leg(
                Route::Interface("wlp2s0".into()),
                Stage::Done,
                done(60.0),
            )],
            cancelled: false,
        };
        let add = update(&mut state, Msg::Finished(wifi), &ctx);
        let _ = update(&mut state, Msg::AskClear, &ctx);
        let clear = update(&mut state, Msg::Clear, &ctx);
        assert!(state.records.is_empty());

        // What the writer thread does, in submission order.
        let path = state.history_path().to_path_buf();
        let (Effect::SpeedHistory(add), Effect::SpeedHistory(clear)) = (add, clear) else {
            panic!("expected two history effects");
        };
        let after_add = apply_history(path.clone(), add).unwrap();
        assert_eq!(after_add.len(), 1);
        let after_clear = apply_history(path.clone(), clear).unwrap();
        assert!(after_clear.is_empty());
        assert!(State::new(path).records.is_empty());
        assert!(dir.path().exists());

        // The replies arrive in that order; the first is stale.
        let _ = update(&mut state, Msg::HistorySaved(Ok(after_add)), &ctx);
        assert!(state.records.is_empty());
        let _ = update(&mut state, Msg::HistorySaved(Ok(after_clear)), &ctx);
        assert!(state.records.is_empty());
        assert_eq!(state.history_pending, 0);
    }

    #[test]
    fn clear_returns_history_effect_and_forgets_at_once() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let wifi = Run {
            legs: vec![leg(
                Route::Interface("wlp2s0".into()),
                Stage::Done,
                done(60.0),
            )],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(wifi), &ctx);
        let _ = update(&mut state, Msg::AskClear, &ctx);
        assert_eq!(
            update(&mut state, Msg::Clear, &ctx),
            Effect::SpeedHistory(HistoryOp::Clear)
        );
        assert!(state.records.is_empty());
    }

    #[test]
    fn finished_run_is_recorded_with_carrier_and_cancel_is_not() {
        let (dir, mut state) = page(Some("wlp2s0"));
        let ctx = confirmed(connected_snapshot());
        let both = Run {
            legs: vec![
                leg(Route::Phone, Stage::Done, done(20.0)),
                leg(Route::Interface("wlp2s0".into()), Stage::Done, done(60.0)),
            ],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = update(&mut state, Msg::Finished(both.clone()), &ctx);
        let records = state.records.as_slice();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].carrier.as_deref(), Some("中国移动"));
        assert_eq!(records[0].generation.as_deref(), Some("5G"));
        assert_eq!(records[0].phone.as_ref().unwrap().download, Some(20.0));
        assert_eq!(records[0].wifi.as_ref().unwrap().download, Some(60.0));
        // Nothing was written by `update`; the app's blocking thread does it.
        let path = dir.path().join("cellglide/speedtests.json");
        assert!(!path.exists());
        let saved = apply_history(path.clone(), HistoryOp::Add(records[0].clone())).unwrap();
        assert_eq!(saved, state.records);
        // Saved: a new page over the same file reads it back.
        assert_eq!(State::new(path).records.len(), 1);

        // Cancel asks the app to stop the run; the cancelled run records nothing.
        let _ = update(&mut state, Msg::Start, &ctx);
        assert_eq!(update(&mut state, Msg::Cancel, &ctx), Effect::CancelSpeed);
        let cancelled = Run {
            legs: vec![leg(Route::Phone, Stage::Done, done(20.0))],
            cancelled: true,
        };
        let _ = update(&mut state, Msg::Finished(cancelled), &ctx);
        assert_eq!(state.records.len(), 1);
        assert!(state.running.is_none());
        // Nothing running: Cancel has nothing to stop.
        assert_eq!(update(&mut state, Msg::Cancel, &ctx), Effect::None);
    }

    #[test]
    fn progress_moves_the_leg_it_names() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        assert_eq!(state.legs.len(), 1);
        assert!(!state.legs[0].phone);
        let progress = Progress {
            stage: Stage::Download,
            progress: 0.4,
            ..Progress::default()
        };
        let _ = update(
            &mut state,
            Msg::Progress {
                index: 0,
                progress: progress.clone(),
            },
            &ctx,
        );
        assert_eq!(state.legs[0].progress, progress);
        // An index past the legs is ignored.
        let before = state.legs.clone();
        let _ = update(&mut state, Msg::Progress { index: 5, progress }, &ctx);
        assert_eq!(state.legs, before);
        // Progress after the run ended is ignored too.
        let _ = update(
            &mut state,
            Msg::Finished(Run {
                legs: vec![],
                cancelled: true,
            }),
            &ctx,
        );
        let ended = state.legs.clone();
        assert_eq!(ended[0].progress.stage, Stage::Cancelled);
        let _ = update(
            &mut state,
            Msg::Progress {
                index: 0,
                progress: Progress::default(),
            },
            &ctx,
        );
        assert_eq!(state.legs, ended);
    }

    #[test]
    fn include_wifi_is_a_preference() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        assert_eq!(
            update(&mut state, Msg::IncludeWifi(false), &ctx),
            Effect::Prefs(PrefChange::IncludeWifi(false))
        );
        let _ = update(&mut state, Msg::Uplink(None), &ctx);
        assert_eq!(state.uplink, None);
    }

    #[test]
    fn start_while_the_warning_is_up_does_nothing_more() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = update(&mut state, Msg::DontAsk(true), &ctx);
        assert_eq!(update(&mut state, Msg::Start, &ctx), Effect::None);
        assert!(state.confirming && state.dont_ask && state.running.is_none());
    }

    #[test]
    fn dont_ask_is_not_saved_when_the_phone_left_before_confirm() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let _ = update(
            &mut state,
            Msg::Start,
            &Ctx::for_test(Some(connected_snapshot())),
        );
        let _ = update(
            &mut state,
            Msg::DontAsk(true),
            &Ctx::for_test(Some(connected_snapshot())),
        );
        // The click raced the snapshot: the warning is still up, the phone is gone.
        let gone = Ctx::for_test(Some(disconnected_snapshot()));
        let effect = update(&mut state, Msg::Confirm, &gone);
        let job = started(&effect).expect("the Wi‑Fi still runs");
        assert!(!job.plan.phone);
        let saved = |e: &Effect| match e {
            Effect::Prefs(PrefChange::SpeedConfirmed) => true,
            Effect::Batch(effects) => effects.contains(&Effect::Prefs(PrefChange::SpeedConfirmed)),
            _ => false,
        };
        assert!(!saved(&effect), "{effect:?}");
        assert!(!state.confirmed_this_launch);
    }

    #[test]
    fn the_notice_goes_when_the_phone_comes_or_goes() {
        let _lang = zh();
        let (_dir, mut state) = page(None);
        let none = Ctx::for_test(Some(disconnected_snapshot()));
        state.on_snapshot(&none);
        let _ = update(&mut state, Msg::Start, &none);
        assert!(state.notice.is_some());
        // Same phone state: the notice stays.
        state.on_snapshot(&none);
        assert!(state.notice.is_some());
        let connected = Ctx::for_test(Some(connected_snapshot()));
        state.on_snapshot(&connected);
        assert_eq!(state.notice, None);
        state.notice = Some("x".into());
        state.on_snapshot(&none);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn without_anything_to_test_start_is_disabled_and_says_why() {
        let _lang = zh();
        let (_dir, state) = page(None);
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        assert_eq!(
            start_line(&state, &ctx),
            (false, Some(error_text(&SpeedError::NotConnected)))
        );
        let (_dir, state) = page(Some("wlp2s0"));
        assert_eq!(
            start_line(&state, &ctx),
            (true, Some(tr("speed.wifi_only", &[])))
        );
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        assert_eq!(start_line(&state, &ctx), (true, None));
    }

    #[test]
    fn clear_without_asking_clears_nothing() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let wifi = Run {
            legs: vec![leg(
                Route::Interface("wlp2s0".into()),
                Stage::Done,
                done(60.0),
            )],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(wifi), &ctx);
        let _ = update(&mut state, Msg::Clear, &ctx);
        assert_eq!(state.records.len(), 1);
    }

    #[test]
    fn default_state_reads_no_real_file() {
        let state = State::default();
        assert_eq!(state.history_path, std::path::Path::new(UNUSED_HISTORY));
        assert!(state.history_path.is_absolute());
        assert!(state.records.is_empty());
    }

    #[test]
    fn clear_asks_first() {
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let wifi = Run {
            legs: vec![leg(
                Route::Interface("wlp2s0".into()),
                Stage::Done,
                done(60.0),
            )],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(wifi), &ctx);
        assert_eq!(state.records.len(), 1);
        let _ = update(&mut state, Msg::AskClear, &ctx);
        assert!(state.confirming_clear);
        let _ = update(&mut state, Msg::CancelClear, &ctx);
        assert!(!state.confirming_clear);
        assert_eq!(state.records.len(), 1);
        let _ = update(&mut state, Msg::AskClear, &ctx);
        let _ = update(&mut state, Msg::Clear, &ctx);
        assert!(!state.confirming_clear);
        assert!(state.records.is_empty());
    }

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
    fn the_page_shows_each_step_of_a_run() {
        let _lang = zh();
        let (_dir, mut state) = page(Some("wlp2s0"));
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        assert_eq!(dialog(&state), None);
        assert_eq!(start_line(&state, &ctx), (true, None));
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::Start, &ctx);
        assert_eq!(dialog(&state), Some(Dialog::DataWarning));
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::Confirm, &ctx);
        assert_eq!(dialog(&state), None);
        // Running: no hint beside Cancel, no verdict yet.
        assert_eq!(start_line(&state, &ctx), (true, None));
        assert_eq!(verdict(&state), None);
        let _ = update(
            &mut state,
            Msg::Progress {
                index: 0,
                progress: Progress {
                    stage: Stage::Latency,
                    ..Progress::default()
                },
            },
            &ctx,
        );
        assert_eq!(
            stage_text(&state.legs[0].progress).as_deref(),
            Some("正在检测 HTTPS 延迟…")
        );
        let _ = view(&state, &ctx);
        // Wi‑Fi failed: its reason, and no comparison.
        let failed = Run {
            legs: vec![
                leg(Route::Phone, Stage::Done, done(20.0)),
                leg(
                    Route::Interface("wlp2s0".into()),
                    Stage::Failed(SpeedError::Timeout),
                    SpeedResult::default(),
                ),
            ],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(failed), &ctx);
        assert_eq!(verdict(&state), None);
        assert_eq!(stage_text(&state.legs[0].progress), None);
        assert_eq!(
            state.legs[1].progress.stage,
            Stage::Failed(SpeedError::Timeout)
        );
        let _ = view(&state, &ctx);
        // Both finished: the comparison.
        let _ = update(&mut state, Msg::Start, &ctx);
        let both = Run {
            legs: vec![
                leg(Route::Phone, Stage::Done, done(20.0)),
                leg(Route::Interface("wlp2s0".into()), Stage::Done, done(60.0)),
            ],
            cancelled: false,
        };
        let _ = update(&mut state, Msg::Finished(both), &ctx);
        assert_eq!(
            verdict(&state).as_deref(),
            Some("Wi‑Fi 下载比 iPhone 快 3.0 倍。")
        );
        assert_eq!(state.records.len(), 2);
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::AskClear, &ctx);
        assert_eq!(dialog(&state), Some(Dialog::ClearHistory));
        let _ = view(&state, &Ctx::for_test(None));
    }

    /// A measurer that never touches the network: a second of "download", then numbers.
    async fn fake_measure(
        route: Route,
        tx: watch::Sender<Progress>,
        cancel: CancellationToken,
    ) -> SpeedResult {
        tx.send_modify(|p| p.stage = Stage::Download);
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            () = cancel.cancelled() => {
                tx.send_modify(|p| p.stage = Stage::Cancelled);
                return SpeedResult::default();
            }
        }
        let result = done(if route == Route::Phone { 20.0 } else { 60.0 });
        tx.send_modify(|p| {
            p.stage = Stage::Done;
            p.result = result.clone();
        });
        result
    }

    fn routes() -> Vec<(Route, String)> {
        vec![
            (Route::Phone, "iPhone".into()),
            (Route::Interface("wlp2s0".into()), "w".into()),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn the_stream_reports_progress_then_how_the_run_ended() {
        use futures::StreamExt;
        let messages: Vec<Msg> = stream_with(routes(), CancellationToken::new(), fake_measure)
            .collect()
            .await;
        let progress: Vec<usize> = messages
            .iter()
            .filter_map(|m| match m {
                Msg::Progress { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert!(
            progress.contains(&0) && progress.contains(&1),
            "{progress:?}"
        );
        let Some(Msg::Finished(run)) = messages.last() else {
            panic!("the stream must end with Finished: {messages:?}");
        };
        assert!(!run.cancelled);
        assert_eq!(run.legs.len(), 2);
        assert_eq!(run.legs[1].result.download, Some(60.0));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_ends_the_stream_with_a_cancelled_run() {
        use futures::StreamExt;
        let cancel = CancellationToken::new();
        let stream = stream_with(routes(), cancel.clone(), fake_measure);
        let mut stream = std::pin::pin!(stream);
        // The first leg is running.
        assert!(matches!(
            stream.next().await,
            Some(Msg::Progress { index: 0, .. })
        ));
        cancel.cancel();
        let rest: Vec<Msg> = stream.collect().await;
        let Some(Msg::Finished(run)) = rest.last() else {
            panic!("the stream must end with Finished: {rest:?}");
        };
        assert!(run.cancelled && run.legs.is_empty());
        assert!(
            !rest
                .iter()
                .any(|m| matches!(m, Msg::Progress { index: 1, .. })),
            "the second leg never started"
        );
    }
}
