//! The tray panel, after the Mac's `PanelView`: the state, the connect switch, the phone and the
//! connection's health at a glance, and a way into the main window.
//!
//! It closes when it loses focus. GNOME on Wayland may never give a new window focus (focus
//! stealing prevention), so it also closes on a second tray click, on Esc, and 60 s after
//! opening if it never got focus; it is never left on screen with no way to close it.

use crate::app::{App, Message};
use crate::section::Section;
use crate::widgets::kit::{self, SEMIBOLD, Tone};
use crate::widgets::phone_card;
use cg_i18n::tr;
use cg_ipc::{ConnState, Snapshot, describe};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Alignment, Background, Border, Element, Length, Size, Task, Theme, window};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::time::{Duration, Instant};

/// How long a panel that never got focus stays open.
const UNFOCUSED_LIMIT: Duration = Duration::from_secs(60);

/// A tray click this soon after the panel closed on losing focus was the click that took the
/// focus away (KDE delivers the focus loss first), so it must not reopen the panel.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

const SIZE: Size = Size::new(340.0, 460.0);

#[derive(Debug, Default)]
pub struct PanelState {
    /// The panel's window, once it exists.
    pub id: Option<window::Id>,
    /// Asked to open; the window is not there yet.
    waiting: bool,
    /// The window asked for while waiting. iced ignores a close for a window it hasn't created
    /// yet, so nothing is closed before `opened`.
    pending_id: Option<window::Id>,
    /// Told to close while waiting: close it as soon as it opens.
    close_on_open: bool,
    /// Has had focus since it opened.
    focused: bool,
    /// When it was asked to open, then when it opened.
    opened_at: Option<Instant>,
    /// When it last closed because it lost focus.
    unfocus_closed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelAction {
    None,
    Open,
    Close(window::Id),
}

impl PanelState {
    /// A tray click: opens a closed panel and closes an open one. While one is being created it
    /// does nothing, so a double click never makes two; right after a close on focus loss it
    /// does nothing either, as that click is what took the focus.
    pub fn toggle(&mut self, now: Instant) -> PanelAction {
        if self.waiting {
            return PanelAction::None;
        }
        if let Some(id) = self.id {
            return self.close(id);
        }
        let guarded = self
            .unfocus_closed_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) < REOPEN_GUARD);
        if guarded {
            return PanelAction::None;
        }
        *self = PanelState {
            waiting: true,
            opened_at: Some(now),
            ..PanelState::default()
        };
        PanelAction::Open
    }

    /// The window `Open` asked for has an id; it is not created yet.
    pub fn requested(&mut self, id: window::Id) {
        if self.waiting {
            self.pending_id = Some(id);
        }
    }

    /// The window exists. Closes it straight away if it was told to close meanwhile, or if it
    /// is not the one being waited for (a request already given up on).
    pub fn opened(&mut self, id: window::Id, now: Instant) -> PanelAction {
        if !self.waiting || self.pending_id != Some(id) {
            return PanelAction::Close(id);
        }
        let close = self.close_on_open;
        *self = PanelState {
            id: Some(id),
            opened_at: Some(now),
            ..PanelState::default()
        };
        if close {
            self.close(id)
        } else {
            PanelAction::None
        }
    }

    /// Focus changes of the panel: it closes when it loses focus it once had.
    pub fn on_window(
        &mut self,
        id: window::Id,
        event: &window::Event,
        now: Instant,
    ) -> PanelAction {
        if self.id != Some(id) {
            return PanelAction::None;
        }
        match event {
            window::Event::Focused => {
                self.focused = true;
                PanelAction::None
            }
            window::Event::Unfocused if self.focused => {
                let action = self.close(id);
                self.unfocus_closed_at = Some(now);
                action
            }
            _ => PanelAction::None,
        }
    }

    pub fn on_escape(&mut self) -> PanelAction {
        self.close_now()
    }

    /// Closes the panel: now if it is open, as soon as it opens if it is being created.
    pub fn close_now(&mut self) -> PanelAction {
        match self.id {
            Some(id) => self.close(id),
            None => {
                if self.waiting {
                    self.close_on_open = true;
                }
                PanelAction::None
            }
        }
    }

    /// Closes a panel that has been open a minute without ever getting focus; also gives up on
    /// an open request that never produced a window (should it appear later, `opened` closes it).
    pub fn on_tick(&mut self, now: Instant) -> PanelAction {
        let expired = self
            .opened_at
            .is_some_and(|at| now.saturating_duration_since(at) >= UNFOCUSED_LIMIT);
        if !expired || self.focused {
            return PanelAction::None;
        }
        match self.id {
            Some(id) => self.close(id),
            None => {
                *self = PanelState::default();
                PanelAction::None
            }
        }
    }

    /// The window is gone, however it went.
    pub fn closed(&mut self, id: window::Id) {
        if self.id == Some(id) {
            *self = PanelState::default();
        }
    }

    /// Being created or open.
    pub fn is_open(&self) -> bool {
        self.id.is_some() || self.waiting
    }

    /// The window being created, if any.
    #[cfg(test)]
    pub fn pending_id(&self) -> Option<window::Id> {
        self.pending_id
    }

    /// Whether `id` is the panel's window, created or still being created.
    pub fn shows(&self, id: window::Id) -> bool {
        self.id == Some(id) || self.pending_id == Some(id)
    }

    /// Forgets the panel as soon as it is told to close, so the next click opens a new one.
    fn close(&mut self, id: window::Id) -> PanelAction {
        *self = PanelState::default();
        PanelAction::Close(id)
    }
}

/// A borderless, fixed-size window above the others; where it appears is up to the desktop on
/// Wayland. Same app id as the main window, so the Dock shows one CellGlide.
pub fn open_panel() -> (window::Id, Task<Message>) {
    let (id, task) = window::open(window::Settings {
        size: SIZE,
        resizable: false,
        decorations: false,
        level: window::Level::AlwaysOnTop,
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: crate::app::APP_ID.into(),
            ..Default::default()
        },
        ..window::Settings::default()
    });
    (id, task.map(Message::PanelOpened))
}

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
pub fn health_line(snapshot: &Snapshot, now: Timestamp, tz: &TimeZone) -> Option<String> {
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

/// A dot, a line and a chevron, as one flat button.
pub fn health_row<'a, M: Clone + 'a>(tone: Tone, line: String, on_press: M) -> Element<'a, M> {
    button(
        row![
            kit::dot(tone, 8.0),
            text(line).size(13),
            space::horizontal(),
            kit::secondary("›").size(13),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([2, 0])
    .on_press(on_press)
    .style(button::text)
    .into()
}

/// A menu-like row: plain text that highlights under the pointer.
fn menu_row<'a>(label: String, message: Message) -> Element<'a, Message> {
    button(text(label).size(13))
        .width(Length::Fill)
        .padding([4, 6])
        .on_press(message)
        .style(|theme: &Theme, status| {
            let palette = theme.extended_palette();
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(palette.background.weak.color)),
                text_color: palette.background.base.text,
                border: Border {
                    radius: 5.0.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        })
        .into()
}

pub fn view(app: &App) -> Element<'_, Message> {
    let snapshot = app.snapshot();
    let (title, detail) = match (snapshot, app.service()) {
        (Some(snapshot), _) => {
            let state = describe::gui_state(snapshot);
            (state.title, Some(state.detail))
        }
        (None, Some(down)) => (down.text.clone(), None),
        (None, None) => (tr("gui.waiting_service", &[]), None),
    };
    let mut words = column![text(title).size(13).font(SEMIBOLD)].spacing(2);
    if let Some(detail) = detail {
        words = words.push(kit::secondary(detail).size(11));
    }
    let status = row![
        container(kit::dot(status_tone(snapshot), 10.0)).padding([4, 0]),
        words.width(Length::Fill),
    ]
    .spacing(10)
    .align_y(Alignment::Start);

    let mut body = column![status].spacing(10).width(Length::Fill);
    if let Some(problem) = app.command_error() {
        body = body.push(text(problem.to_owned()).size(11).color(Tone::Red.color()));
    }
    body = body.push(primary_button(snapshot, app.command_pending()));

    if let Some(snapshot) = snapshot {
        if let Some(card) = snapshot
            .phone
            .as_ref()
            .filter(|c| describe::card_visible(c))
        {
            body = body.push(phone_card::compact(card));
        }
        if let Some(line) = health_line(snapshot, app.now(), &TimeZone::system()) {
            body = body.push(health_row(
                health_tone(snapshot),
                line,
                Message::OpenWindow(Some(Section::History)),
            ));
        }
    }

    body = body
        .push(rule::horizontal(1))
        .push(menu_row(tr("panel.open", &[]), Message::OpenWindow(None)))
        .push(menu_row(tr("panel.quit", &[]), Message::Quit));

    scrollable(container(body).padding(12))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Full width: "disconnect and go back" while a connection is wanted, else a prominent "connect".
/// Not pressable while the last press is still being carried out.
fn primary_button<'a>(snapshot: Option<&Snapshot>, pending: bool) -> Element<'a, Message> {
    let label = crate::app::button_label(snapshot);
    button(container(text(label).size(13)).center_x(Length::Fill))
        .width(Length::Fill)
        .padding([6, 10])
        .on_press_maybe((!pending).then_some(Message::Toggle))
        .style(main_button_style(snapshot))
        .into()
}

/// Prominent for "connect"; plain for "disconnect", which is not the thing to invite.
pub fn main_button_style(
    snapshot: Option<&Snapshot>,
) -> fn(&Theme, button::Status) -> button::Style {
    if snapshot.is_some_and(|s| s.wants_connection) {
        button::secondary
    } else {
        button::primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;
    use cg_ipc::Reason;
    use cg_ipc::health::IssueCause;

    /// Clicks the tray and lets the window appear.
    fn open(panel: &mut PanelState, now: Instant) -> window::Id {
        assert_eq!(panel.toggle(now), PanelAction::Open);
        let id = window::Id::unique();
        panel.requested(id);
        assert_eq!(panel.opened(id, now), PanelAction::None);
        id
    }

    fn unfocused(panel: &mut PanelState, id: window::Id, now: Instant) -> PanelAction {
        panel.on_window(id, &window::Event::Unfocused, now)
    }

    #[test]
    fn health_line_shows_only_while_connected_like_the_mac() {
        use cg_ipc::health::{HealthStatus, LastIssue};
        let _guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        let now = jiff::Timestamp::now();
        let utc = jiff::tz::TimeZone::UTC;
        let mut snapshot = fixtures::disconnected_snapshot();
        // A drop that ended an hour ago: `cellglide status` shows it, the GUI doesn't.
        snapshot.health = HealthStatus {
            last_issue: Some(LastIssue {
                cause: IssueCause::Channel,
                ended: now - jiff::SignedDuration::from_hours(1),
                duration: Some(45),
            }),
            ..Default::default()
        };
        assert!(describe::status_health_line(&snapshot, now, &utc).is_some());
        assert_eq!(health_line(&snapshot, now, &utc), None);
        snapshot.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: cg_ipc::DnsMode::Phone,
        };
        assert_eq!(
            health_line(&snapshot, now, &utc),
            describe::status_health_line(&snapshot, now, &utc)
        );
    }

    #[test]
    fn panel_toggles_and_closes_on_focus_loss_only_after_focus() {
        let now = Instant::now();
        let mut panel = PanelState::default();
        let id = open(&mut panel, now);
        // GNOME may never focus it: losing focus it never had closes nothing.
        assert_eq!(unfocused(&mut panel, id, now), PanelAction::None);
        assert_eq!(
            panel.on_window(id, &window::Event::Focused, now),
            PanelAction::None
        );
        // Another window's focus is not the panel's.
        let other = window::Id::unique();
        assert_eq!(unfocused(&mut panel, other, now), PanelAction::None);
        assert_eq!(unfocused(&mut panel, id, now), PanelAction::Close(id));
        assert!(!panel.is_open());
        // A later click opens a fresh one.
        let later = now + Duration::from_secs(1);
        assert_eq!(panel.toggle(later), PanelAction::Open);
    }

    #[test]
    fn panel_that_never_got_focus_still_closes() {
        let t0 = Instant::now();
        let mut panel = PanelState::default();
        let id = open(&mut panel, t0);
        assert_eq!(
            panel.on_tick(t0 + Duration::from_secs(59)),
            PanelAction::None
        );
        assert_eq!(
            panel.on_tick(t0 + Duration::from_secs(61)),
            PanelAction::Close(id)
        );
        assert!(!panel.is_open());

        // A second tray click closes it.
        let mut panel = PanelState::default();
        let id = open(&mut panel, t0);
        assert_eq!(panel.toggle(t0), PanelAction::Close(id));

        // So does Esc; with nothing open, Esc does nothing.
        let mut panel = PanelState::default();
        assert_eq!(panel.on_escape(), PanelAction::None);
        let id = open(&mut panel, t0);
        assert_eq!(panel.on_escape(), PanelAction::Close(id));

        // A panel that got focus is left to the focus rule, however long it stays open.
        let mut panel = PanelState::default();
        let id = open(&mut panel, t0);
        let _ = panel.on_window(id, &window::Event::Focused, t0);
        assert_eq!(
            panel.on_tick(t0 + Duration::from_secs(600)),
            PanelAction::None
        );
    }

    #[test]
    fn second_click_never_opens_a_second_panel() {
        let now = Instant::now();
        let mut panel = PanelState::default();
        assert_eq!(panel.toggle(now), PanelAction::Open);
        // Still being created: no second panel, and no close iced would ignore.
        assert_eq!(panel.toggle(now), PanelAction::None);
        let id = window::Id::unique();
        panel.requested(id);
        assert_eq!(panel.toggle(now), PanelAction::None);
        assert!(panel.is_open() && panel.shows(id) && panel.id.is_none());
        assert_eq!(panel.opened(id, now), PanelAction::None);
        assert_eq!(panel.id, Some(id));
        assert_eq!(panel.toggle(now), PanelAction::Close(id));
    }

    #[test]
    fn closing_while_being_created_closes_on_open() {
        let now = Instant::now();
        let mut panel = PanelState::default();
        let _ = panel.toggle(now);
        let id = window::Id::unique();
        panel.requested(id);
        assert_eq!(panel.close_now(), PanelAction::None);
        assert_eq!(panel.opened(id, now), PanelAction::Close(id));
        assert!(!panel.is_open());
    }

    #[test]
    fn a_request_given_up_on_is_closed_when_it_appears() {
        let now = Instant::now();
        let mut panel = PanelState::default();
        let _ = panel.toggle(now);
        let id = window::Id::unique();
        panel.requested(id);
        assert_eq!(
            panel.on_tick(now + Duration::from_secs(61)),
            PanelAction::None
        );
        assert!(!panel.is_open());
        assert_eq!(panel.opened(id, now), PanelAction::Close(id));
    }

    #[test]
    fn a_stale_close_event_leaves_the_new_panel_alone() {
        let now = Instant::now();
        let mut panel = PanelState::default();
        let old = open(&mut panel, now);
        assert_eq!(panel.toggle(now), PanelAction::Close(old));
        let new = open(&mut panel, now);
        panel.closed(old);
        assert_eq!(panel.id, Some(new));
        panel.closed(new);
        assert!(!panel.is_open());
    }

    #[test]
    fn the_click_that_took_the_focus_does_not_reopen() {
        let t0 = Instant::now();
        let mut panel = PanelState::default();
        let id = open(&mut panel, t0);
        let _ = panel.on_window(id, &window::Event::Focused, t0);
        // KDE: the tray click first takes the focus, then arrives as a click.
        assert_eq!(unfocused(&mut panel, id, t0), PanelAction::Close(id));
        panel.closed(id);
        assert_eq!(
            panel.toggle(t0 + Duration::from_millis(200)),
            PanelAction::None
        );
        // The guard is used up; the next click opens.
        assert_eq!(
            panel.toggle(t0 + Duration::from_millis(250)),
            PanelAction::Open
        );

        // A click well after the close opens it again.
        let mut panel = PanelState::default();
        let id = open(&mut panel, t0);
        let _ = panel.on_window(id, &window::Event::Focused, t0);
        let _ = unfocused(&mut panel, id, t0);
        assert_eq!(
            panel.toggle(t0 + Duration::from_millis(400)),
            PanelAction::Open
        );

        // Esc and a second click are not focus losses: no guard.
        let mut panel = PanelState::default();
        let _ = open(&mut panel, t0);
        let _ = panel.on_escape();
        assert_eq!(panel.toggle(t0), PanelAction::Open);
    }

    #[test]
    fn dot_colours() {
        let connected = fixtures::connected_snapshot();
        assert_eq!(status_tone(Some(&connected)), Tone::Green);
        let mut waiting = fixtures::disconnected_snapshot();
        assert_eq!(status_tone(Some(&waiting)), Tone::Gray);
        waiting.state = ConnState::Retrying {
            reason: Reason::NoExit,
        };
        assert_eq!(status_tone(Some(&waiting)), Tone::Orange);
        waiting.state = ConnState::ChoosePhone;
        assert_eq!(status_tone(Some(&waiting)), Tone::Orange);
        assert_eq!(status_tone(None), Tone::Gray);

        let mut health = fixtures::connected_snapshot();
        assert_eq!(health_tone(&health), Tone::Green);
        health.health.cause = Some(IssueCause::Channel);
        assert_eq!(health_tone(&health), Tone::Red);
        health.health.cause = Some(IssueCause::PhoneUnstable);
        assert_eq!(health_tone(&health), Tone::Orange);
    }
}
