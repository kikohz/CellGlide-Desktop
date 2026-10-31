//! The main window: a sidebar of sections and the selected page. The app keeps running in the
//! tray when the window is closed.

use crate::autostart;
use crate::effect::{AfterMsg, Effect};
use crate::ipc;
use crate::pages::{
    Ctx, PageMsg, Pages, about, diagnostics, history, leak, overview, settings, speed,
};
use crate::panel::{self, PanelAction, PanelState};
use crate::widgets::kit::{self, Tone};
use crate::writer::Writer;
use cg_i18n::tr;
use cg_ipc::describe::ui::{
    self, Badge, GROUPS, KnownDevices, Section, ServiceDown, plugged_in, version_line,
};
use cg_ipc::{Request, Snapshot, describe};
use cg_tools::prefs::{self, PrefChange, Prefs};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{
    Alignment, Background, Border, Element, Length, Size, Subscription, Task, Theme, keyboard,
    window,
};
use jiff::Timestamp;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum Message {
    Snapshot(Box<Snapshot>),
    ServiceDown(ServiceDown),
    /// Open (or raise) the main window, on a section if one is named. Closes the panel.
    OpenWindow(Option<Section>),
    /// A left click on the tray: open the panel, or close it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TogglePanel,
    /// The panel's window now exists.
    PanelOpened(window::Id),
    /// Keys nothing else took, while the panel is open; only Esc is used.
    Key(keyboard::Event),
    /// Show a section, opening the main window if it is closed.
    Navigate(Section),
    Page(PageMsg),
    /// Once a second while a window or the panel is open.
    Tick(Instant),
    /// Focus changes, close requests and closes; nothing else (not every redraw).
    Window(window::Id, window::Event),
    /// A page's `Effect::After` came due (e.g. "Copied" turning back into "Copy").
    After(AfterMsg),
    Toggle,
    CommandDone(Result<(), String>),
    Quit,
    /// Sent by the Linux tray whenever a tray host is, or stops being, available.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TrayReady(bool),
}

#[derive(Default)]
pub struct App {
    snapshot: Option<Snapshot>,
    /// Why there are no snapshots, while there aren't.
    service: Option<ServiceDown>,
    command_error: Option<String>,
    /// The connect / disconnect press being carried out.
    command: Option<Command>,
    window: Option<window::Id>,
    panel: PanelState,
    tray_up: bool,
    /// The open window was not asked for by the user, only opened for lack of a tray.
    auto_opened: bool,
    section: Section,
    prefs: Prefs,
    prefs_path: PathBuf,
    pages: Pages,
    /// Wall-clock time as of the last tick, for pages that show ages and durations.
    now: Timestamp,
    /// Stops the speed test that is running.
    speed_cancel: Option<CancellationToken>,
    /// Where "Save to File" looks for the download directory; `None` saves nothing.
    places: Option<cg_diag::save::Places>,
    /// Where the user's `autostart/` directory is (`$XDG_CONFIG_HOME`); tests point it at a
    /// temporary directory.
    config_home: PathBuf,
    /// The iPhones in the last snapshot and the ones missing lately; `None` before the first
    /// snapshot and after the service went away, so what was already plugged in then doesn't
    /// count as plugging in.
    known_devices: Option<KnownDevices>,
    /// Writes the preferences and the speed test history, one job at a time.
    writer: Writer,
}

/// A connect or disconnect press until the service has acted on it.
#[derive(Clone, Copy, Debug)]
struct Command {
    /// Whether it asked to connect.
    wants: bool,
    sent: Instant,
    /// The service replied.
    done: bool,
}

/// How long the main button stays disabled at least after a press whose snapshot didn't match.
const COMMAND_MIN: Duration = Duration::from_secs(1);

/// The desktop file's id, also the window's app_id / WM_CLASS.
#[cfg(target_os = "linux")]
pub const APP_ID: &str = "cellglide";

const SIDEBAR_WIDTH: f32 = 200.0;

/// How often the speed test page looks for this computer's Wi‑Fi or Ethernet again.
const UPLINK_EVERY: Duration = Duration::from_secs(5);

/// How often the history page looks at today again.
const HISTORY_EVERY: Duration = Duration::from_secs(5);

fn uplink_message() -> Message {
    Message::Page(PageMsg::Speed(speed::Msg::Uplink(
        cg_tools::uplink::physical_uplink(),
    )))
}

/// Started by autostart rather than by the user.
pub fn background_launch() -> bool {
    std::env::args().any(|a| a == "--background")
}

fn open_window() -> (window::Id, Task<Message>) {
    let (id, task) = window::open(window::Settings {
        size: Size::new(880.0, 640.0),
        min_size: Some(Size::new(760.0, 520.0)),
        // Matches cellglide.desktop, so the shell finds the packaged icon (Wayland app_id, X11 WM_CLASS).
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.into(),
            ..Default::default()
        },
        ..window::Settings::default()
    });
    (id, task.discard())
}

/// The window events the app acts on. Redraws and the rest would wake `update` every frame.
fn window_message(
    event: iced::Event,
    _status: iced::event::Status,
    id: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(
            event @ (window::Event::Focused
            | window::Event::Unfocused
            | window::Event::CloseRequested
            | window::Event::Closed),
        ) => Some(Message::Window(id, event)),
        _ => None,
    }
}

/// `xdg-open` with the target as its only argument, never through a shell.
fn xdg_open(target: OsString) -> Task<Message> {
    Task::future(async move {
        let _ = tokio::process::Command::new("xdg-open")
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    })
    .discard()
}

impl App {
    /// `--background` is what autostart passes: a window then shows only because there is no
    /// tray yet, and goes away again when one appears. A manual launch is never auto-closed.
    pub fn boot() -> (App, Task<Message>) {
        Self::boot_with(
            background_launch(),
            prefs::path(),
            cg_tools::speed::default_path(),
            cg_diag::save::Places::from_env(),
            cg_tools::paths::config_home(),
        )
    }

    /// Preferences, the speed test history and the diagnostics' download directory come from
    /// the paths given, so tests never read or write the user's own files.
    fn boot_with(
        background: bool,
        prefs_path: PathBuf,
        history_path: PathBuf,
        places: Option<cg_diag::save::Places>,
        config_home: PathBuf,
    ) -> (App, Task<Message>) {
        let (id, task) = open_window();
        let mut app = App {
            window: Some(id),
            auto_opened: background,
            prefs: prefs::load(&prefs_path),
            prefs_path,
            now: Timestamp::now(),
            places,
            ..App::default()
        };
        app.pages.settings = settings::State::new(
            autostart::current(&config_home).map_err(|error| error.to_string()),
        );
        app.config_home = config_home;
        app.pages.speed = speed::State::new(history_path);
        (app, task)
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn service(&self) -> Option<&ServiceDown> {
        self.service.as_ref()
    }

    pub fn command_error(&self) -> Option<&str> {
        self.command_error.as_deref()
    }

    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// The last connect / disconnect press is still being carried out: the main button waits.
    pub fn command_pending(&self) -> bool {
        self.command_pending_at(Instant::now())
    }

    /// Pending until the service replied and a snapshot shows the wish (that clears it), or,
    /// once it replied, until a second has passed since the press.
    fn command_pending_at(&self, now: Instant) -> bool {
        self.command
            .is_some_and(|c| !c.done || now.saturating_duration_since(c.sent) < COMMAND_MIN)
    }

    /// Clears the press once the service replied and the snapshot shows what was asked.
    fn settle_command(&mut self) {
        if let (Some(command), Some(snapshot)) = (self.command, self.snapshot.as_ref())
            && command.done
            && snapshot.wants_connection == command.wants
        {
            self.command = None;
        }
    }

    /// The same title for the main window and the panel.
    pub fn title(&self, _window: window::Id) -> String {
        ui::window_title(self.snapshot.as_ref())
    }

    /// Raises the main window, or opens it.
    fn show_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window {
            return window::gain_focus(id);
        }
        let (id, task) = open_window();
        self.window = Some(id);
        Task::batch([task, self.load_history()])
    }

    /// The history page loads once when it comes into view; not while a request is out.
    fn load_history(&mut self) -> Task<Message> {
        if self.section != Section::History || self.pages.history.is_loading() {
            return Task::none();
        }
        let ctx = Ctx {
            snapshot: self.snapshot.as_ref(),
            now: self.now,
            prefs: &self.prefs,
        };
        let effect = self
            .pages
            .update(PageMsg::History(history::Msg::Shown), &ctx);
        self.run_effect(effect)
    }

    /// Carries out what the panel's state decided.
    fn panel_action(&mut self, action: PanelAction) -> Task<Message> {
        match action {
            PanelAction::None => Task::none(),
            PanelAction::Open => {
                let (id, task) = panel::open_panel();
                self.panel.requested(id);
                task
            }
            PanelAction::Close(id) => window::close(id),
        }
    }

    /// Closes the panel if it is open, or as soon as it opens.
    fn close_panel(&mut self) -> Task<Message> {
        let action = self.panel.close_now();
        self.panel_action(action)
    }

    fn navigate(&mut self, section: Section) -> Task<Message> {
        self.section = section;
        // Using the window means it is wanted.
        self.auto_opened = false;
        let entered = self.entered();
        if self.window.is_none() {
            return Task::batch([entered, self.show_window()]);
        }
        entered
    }

    /// A page was shown: the speed test reads the uplink at once rather than after 5 s, and
    /// the history asks for its day.
    fn entered(&mut self) -> Task<Message> {
        if self.section == Section::History {
            return self.load_history();
        }
        if self.section == Section::SpeedTest {
            let uplink = speed::Msg::Uplink(cg_tools::uplink::physical_uplink());
            let ctx = Ctx {
                snapshot: self.snapshot.as_ref(),
                now: self.now,
                prefs: &self.prefs,
            };
            let _ = self.pages.update(PageMsg::Speed(uplink), &ctx);
        }
        Task::none()
    }

    /// Pages that follow the connection see every new snapshot (or its loss).
    fn snapshot_changed(&mut self) {
        let ctx = Ctx {
            snapshot: self.snapshot.as_ref(),
            now: self.now,
            prefs: &self.prefs,
        };
        self.pages.speed.on_snapshot(&ctx);
    }

    /// An iPhone was plugged in: an open window is only raised (the user may be reading
    /// another page); a closed one opens on the overview.
    fn plugged_in_window(&mut self) -> Task<Message> {
        if self.window.is_some() {
            self.auto_opened = false;
            let close = self.close_panel();
            return Task::batch([close, self.show_window()]);
        }
        self.update(Message::OpenWindow(Some(Section::Overview)))
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Snapshot(snapshot) => {
                let plugged =
                    plugged_in(&mut self.known_devices, &snapshot.devices, Instant::now());
                self.snapshot = Some(*snapshot);
                self.service = None;
                self.settle_command();
                self.snapshot_changed();
                // Plugging in an iPhone opens the window, but only while we are running.
                if plugged && self.prefs.open_on_plug {
                    return self.plugged_in_window();
                }
            }
            Message::ServiceDown(down) => {
                self.known_devices = None;
                self.snapshot = None;
                self.service = Some(down);
                self.snapshot_changed();
            }
            Message::OpenWindow(section) => {
                self.auto_opened = false;
                if let Some(section) = section {
                    self.section = section;
                }
                let entered = self.entered();
                let close = self.close_panel();
                return Task::batch([close, entered, self.show_window()]);
            }
            Message::TogglePanel => {
                let action = self.panel.toggle(Instant::now());
                return self.panel_action(action);
            }
            Message::PanelOpened(id) => {
                let action = self.panel.opened(id, Instant::now());
                return self.panel_action(action);
            }
            Message::Key(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                let action = self.panel.on_escape();
                return self.panel_action(action);
            }
            Message::Key(_) => {}
            Message::Navigate(section) => return self.navigate(section),
            Message::Page(message) => {
                if matches!(message, PageMsg::Speed(speed::Msg::Finished(_))) {
                    self.speed_cancel = None;
                }
                let ctx = Ctx {
                    snapshot: self.snapshot.as_ref(),
                    now: self.now,
                    prefs: &self.prefs,
                };
                let effect = self.pages.update(message, &ctx);
                return self.run_effect(effect);
            }
            Message::Tick(instant) => {
                self.now = Timestamp::now();
                let action = self.panel.on_tick(instant);
                return self.panel_action(action);
            }
            Message::Window(id, window::Event::Closed) => {
                self.panel.closed(id);
                if self.window == Some(id) {
                    self.window = None;
                    self.auto_opened = false;
                    // Without a tray there is no way to bring the window back.
                    if !self.tray_up {
                        return iced::exit();
                    }
                }
            }
            // The panel acts on focus; both windows close themselves on request.
            Message::Window(id, event) => {
                let action = self.panel.on_window(id, &event, Instant::now());
                return self.panel_action(action);
            }
            Message::After(AfterMsg::DiagnosticsCopied) => {
                return self.update(Message::Page(PageMsg::Diagnostics(
                    diagnostics::Msg::CopiedEnd,
                )));
            }
            Message::After(AfterMsg::AboutCopied) => {
                return self.update(Message::Page(PageMsg::About(about::Msg::CopiedEnd)));
            }
            Message::After(AfterMsg::SettingsCopied) => {
                return self.update(Message::Page(PageMsg::Settings(settings::Msg::CopiedEnd)));
            }
            Message::Toggle => {
                // A second press before the first is carried out would undo it.
                let now = Instant::now();
                if self.command_pending_at(now) {
                    return Task::none();
                }
                // Using the window means it is wanted.
                self.auto_opened = false;
                self.command_error = None;
                let request = self
                    .snapshot
                    .as_ref()
                    .map_or(Request::Connect { serial: None }, describe::toggle_request);
                self.command = Some(Command {
                    wants: matches!(request, Request::Connect { .. }),
                    sent: now,
                    done: false,
                });
                return Task::perform(ipc::call(request), |result| {
                    Message::CommandDone(result.map(|_| ()))
                });
            }
            Message::CommandDone(result) => {
                match (&result, self.command.as_mut()) {
                    // A refused press changes nothing: the button is free again.
                    (Err(_), _) => self.command = None,
                    (Ok(()), Some(command)) => command.done = true,
                    (Ok(()), None) => {}
                }
                self.command_error = result.err();
                self.settle_command();
            }
            Message::Quit => {
                let close = self.close_panel();
                return Task::batch([close, iced::exit()]);
            }
            Message::TrayReady(up) => {
                self.tray_up = up;
                if up {
                    // The tray host came up after us: the window we showed for lack of it can go.
                    if self.auto_opened
                        && let Some(id) = self.window.take()
                    {
                        self.auto_opened = false;
                        return window::close(id);
                    }
                } else if self.window.is_none() {
                    // Never leave the app running with neither tray nor window.
                    let (id, task) = open_window();
                    self.window = Some(id);
                    self.auto_opened = true;
                    return Task::batch([task, self.load_history()]);
                }
            }
        }
        Task::none()
    }

    /// Applies a preference change in memory; the write it asks for is the caller's to run.
    fn apply_pref(&mut self, change: PrefChange) -> Effect {
        self.prefs.apply(change);
        Effect::SavePrefs(self.prefs.clone())
    }

    /// The one place a page's wish becomes I/O.
    fn run_effect(&mut self, effect: Effect) -> Task<Message> {
        match effect {
            Effect::None => Task::none(),
            Effect::Batch(effects) => {
                let tasks: Vec<_> = effects.into_iter().map(|e| self.run_effect(e)).collect();
                Task::batch(tasks)
            }
            Effect::Request(request, reply) => {
                Task::perform(ipc::call(request), move |result| ipc::route(reply, result))
            }
            Effect::Copy(text) => iced::clipboard::write(text),
            Effect::OpenUrl(url) if url.starts_with("https://") => xdg_open(url.into()),
            Effect::OpenUrl(_) => Task::none(),
            Effect::OpenFolder(path) => xdg_open(path.into_os_string()),
            Effect::Quit => self.update(Message::Quit),
            Effect::Prefs(change) => {
                let save = self.apply_pref(change);
                self.run_effect(save)
            }
            // Handed to the writer thread, which does the I/O one job at a time, in order.
            Effect::SavePrefs(prefs) => {
                self.writer.save_prefs(self.prefs_path.clone(), prefs);
                Task::none()
            }
            Effect::SpeedHistory(op) => {
                let path = self.pages.speed.history_path().to_path_buf();
                let reply = self.writer.history(path, op);
                Task::perform(reply, |result| {
                    let result = result.unwrap_or_else(|error| Err(error.to_string()));
                    Message::Page(PageMsg::Speed(speed::Msg::HistorySaved(result)))
                })
            }
            // The user's autostart file, then what it says now. The file work runs on its own
            // thread, started now, so it never blocks the UI or the async executor.
            Effect::SetAutostart(on) => {
                let home = self.config_home.clone();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                std::thread::spawn(move || {
                    let result = autostart::set(&home, on)
                        .and_then(|()| autostart::current(&home))
                        .map_err(|error| error.to_string());
                    let _ = sender.send(result);
                });
                Task::perform(receiver, |result| {
                    let result = result.unwrap_or_else(|_| Err(tr("gui.service_timeout", &[])));
                    Message::Page(PageMsg::Settings(settings::Msg::AutostartDone(result)))
                })
            }
            Effect::StartSpeed(job) => {
                // The page never starts a second run, but never leave one running unowned.
                if let Some(previous) = self.speed_cancel.take() {
                    previous.cancel();
                }
                let cancel = CancellationToken::new();
                self.speed_cancel = Some(cancel.clone());
                Task::run(speed::job_stream(job, cancel), |message| {
                    Message::Page(PageMsg::Speed(message))
                })
            }
            Effect::StartLeak { sharing } => Task::perform(
                cg_tools::dns_leak::view::run_production(sharing),
                |result| Message::Page(PageMsg::Leak(leak::Msg::Done(result))),
            ),
            Effect::CancelSpeed => {
                if let Some(cancel) = &self.speed_cancel {
                    cancel.cancel();
                }
                Task::none()
            }
            // The only upload in the app: the text the user previewed, to production.
            Effect::Upload(json) => Task::perform(
                async move {
                    cg_diag::upload::upload(json.as_bytes(), &cg_diag::upload::Target::production())
                        .await
                },
                |result| Message::Page(PageMsg::Diagnostics(diagnostics::Msg::Sent(result))),
            ),
            Effect::SaveDiagnostics(json) => {
                let places = self.places.clone();
                Task::perform(
                    async move {
                        let Some(places) = places else {
                            return Err(tr(
                                "diag.save_failed",
                                &[("reason", &tr("diag.no_home", &[]))],
                            ));
                        };
                        cg_diag::save::save_bundle(
                            &places.download_dir(),
                            Timestamp::now(),
                            &jiff::tz::TimeZone::system(),
                            &json,
                        )
                    },
                    |result| Message::Page(PageMsg::Diagnostics(diagnostics::Msg::Saved(result))),
                )
            }
            Effect::After(delay, message) => {
                Task::perform(async move { tokio::time::sleep(delay).await }, move |()| {
                    Message::After(message)
                })
            }
        }
    }

    /// The panel, or else the main window.
    pub fn view(&self, window: window::Id) -> Element<'_, Message> {
        if self.panel.shows(window) {
            return panel::view(self);
        }
        let content = match self.section {
            Section::Overview => overview::view(overview::View {
                snapshot: self.snapshot.as_ref(),
                service: self.service.as_ref(),
                command_error: self.command_error.as_deref(),
                command_pending: self.command_pending(),
                now: self.now,
            }),
            section => {
                let ctx = Ctx {
                    snapshot: self.snapshot.as_ref(),
                    now: self.now,
                    prefs: &self.prefs,
                };
                self.pages.view(section, &ctx).map(Message::Page)
            }
        };
        row![self.sidebar(), rule::vertical(1), content].into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(2);
        for (heading, sections) in GROUPS {
            if let Some(key) = heading {
                list = list
                    .push(space().height(10))
                    .push(container(kit::secondary(tr(key, &[])).size(11)).padding([2, 10]));
            }
            for &section in sections {
                list = list.push(self.sidebar_row(section));
            }
        }
        container(
            column![
                scrollable(list).height(Length::Fill),
                container(kit::secondary(version_line()).size(11)).padding([0, 10]),
            ]
            .spacing(8),
        )
        .padding(10)
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .into()
    }

    fn sidebar_row(&self, section: Section) -> Element<'_, Message> {
        let dot: Element<'_, Message> =
            match ui::badge(section, self.snapshot.as_ref(), self.service.as_ref()) {
                Badge::None => space().into(),
                Badge::Warning => kit::dot(Tone::Orange, 7.0),
                Badge::Critical => kit::dot(Tone::Red, 7.0),
            };
        let selected = self.section == section;
        button(
            row![
                text(tr(section.title_key(), &[])).size(13),
                space::horizontal(),
                dot
            ]
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([5, 10])
        .on_press(Message::Navigate(section))
        .style(move |theme: &Theme, status| {
            let palette = theme.extended_palette();
            let background = if selected {
                Some(Background::Color(palette.primary.weak.color))
            } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                Some(Background::Color(palette.background.weak.color))
            } else {
                None
            };
            button::Style {
                background,
                text_color: if selected {
                    palette.primary.weak.text
                } else {
                    palette.background.base.text
                },
                border: Border {
                    radius: 6.0.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        })
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            Subscription::run(crate::tray::events),
            // Empty unless this process owns the session-bus name (Linux).
            Subscription::run(crate::single::activations),
            iced::event::listen_with(window_message),
        ];
        if self.window.is_some() || self.panel.is_open() {
            subscriptions.push(iced::time::every(Duration::from_secs(1)).map(Message::Tick));
        }
        if self.panel.is_open() {
            subscriptions.push(keyboard::listen().map(Message::Key));
        }
        if self.window.is_some() && self.section == Section::SpeedTest {
            subscriptions.push(iced::time::every(UPLINK_EVERY).map(|_| uplink_message()));
        }
        if self.window.is_some() && self.section == Section::History {
            subscriptions.push(
                iced::time::every(HISTORY_EVERY)
                    .map(|_| Message::Page(PageMsg::History(history::Msg::Refresh))),
            );
        }
        Subscription::batch(subscriptions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Reply;
    use crate::pages::{fixtures, history};
    use std::path::Path;

    fn down(text: &str) -> ServiceDown {
        ServiceDown {
            text: text.into(),
            not_in_group: false,
            ..ServiceDown::default()
        }
    }

    fn closed(id: window::Id) -> Message {
        Message::Window(id, window::Event::Closed)
    }

    /// Boots without touching the user's preferences.
    const TEST_HISTORY: &str = "/nonexistent/cellglide/speedtests.json";
    const TEST_CONFIG: &str = "/nonexistent/config";

    fn boot(background: bool) -> (App, Task<Message>) {
        App::boot_with(
            background,
            PathBuf::from("/nonexistent/cellglide/gui.json"),
            PathBuf::from(TEST_HISTORY),
            None,
            PathBuf::from(TEST_CONFIG),
        )
    }

    #[test]
    fn test_apps_never_touch_the_real_speed_history() {
        let (app, _) = boot(false);
        assert_eq!(app.pages.speed.history_path(), Path::new(TEST_HISTORY));
        assert_ne!(
            app.pages.speed.history_path(),
            cg_tools::speed::default_path()
        );
        let app = App::default();
        assert_ne!(
            app.pages.speed.history_path(),
            cg_tools::speed::default_path()
        );
    }

    #[test]
    fn command_error_survives_snapshots() {
        let mut app = App::default();
        let _ = app.update(Message::Snapshot(Box::default()));
        let _ = app.update(Message::CommandDone(Err("x".into())));
        let _ = app.update(Message::Snapshot(Box::default()));
        let _ = app.update(Message::ServiceDown(down("down")));
        assert_eq!(app.command_error.as_deref(), Some("x"));
        let _ = app.update(Message::Toggle);
        assert_eq!(app.command_error, None);
        let _ = app.update(Message::CommandDone(Err("y".into())));
        let _ = app.update(Message::CommandDone(Ok(())));
        assert_eq!(app.command_error, None);
    }

    #[test]
    fn closing_window_without_tray_quits() {
        let mut app = App::default();
        let (id, _) = open_window();
        app.window = Some(id);
        let _ = app.update(Message::TrayReady(false));
        assert_eq!(app.update(closed(id)).units(), 1);

        let mut app = App {
            window: Some(id),
            ..App::default()
        };
        let _ = app.update(Message::TrayReady(true));
        assert_eq!(app.update(closed(id)).units(), 0);
        assert_eq!(app.window, None);
    }

    #[test]
    fn tray_arriving_closes_the_window_we_opened_for_lack_of_it() {
        let mut app = App::default();
        let (id, _) = open_window();
        app.window = Some(id);
        app.auto_opened = true;
        assert_eq!(app.update(Message::TrayReady(true)).units(), 1);
        assert_eq!(app.window, None);
        // The close event that follows must not quit the app.
        assert_eq!(app.update(closed(id)).units(), 0);
    }

    #[test]
    fn tray_arriving_keeps_a_window_the_user_opened() {
        let mut app = App::default();
        let (id, _) = open_window();
        app.window = Some(id);
        app.auto_opened = true;
        let _ = app.update(Message::OpenWindow(None));
        assert_eq!(app.update(Message::TrayReady(true)).units(), 0);
        assert_eq!(app.window, Some(id));

        let mut app = App::default();
        let (id, _) = open_window();
        app.window = Some(id);
        assert_eq!(app.update(Message::TrayReady(true)).units(), 0);
        assert_eq!(app.window, Some(id));
    }

    #[test]
    fn tray_going_offline_without_a_window_opens_one() {
        let mut app = App {
            tray_up: true,
            ..App::default()
        };
        assert_eq!(app.update(Message::TrayReady(false)).units(), 1);
        assert!(app.window.is_some());
        assert!(app.auto_opened);
        assert!(!app.tray_up);
    }

    #[test]
    fn closing_window_while_tray_is_down_exits() {
        let mut app = App::default();
        let (id, _) = open_window();
        app.window = Some(id);
        app.auto_opened = true;
        let _ = app.update(Message::TrayReady(false));
        assert_eq!(app.update(closed(id)).units(), 1);
    }

    #[test]
    fn only_background_launch_window_is_closed_when_tray_arrives() {
        let (mut app, _) = boot(true);
        assert_eq!(app.update(Message::TrayReady(true)).units(), 1);
        assert_eq!(app.window, None);

        let (mut app, _) = boot(false);
        assert_eq!(app.update(Message::TrayReady(true)).units(), 0);
        assert!(app.window.is_some());
    }

    #[test]
    fn using_the_window_keeps_it() {
        let (mut app, _) = boot(true);
        let _ = app.update(Message::Toggle);
        assert_eq!(app.update(Message::TrayReady(true)).units(), 0);
        assert!(app.window.is_some());
    }

    #[test]
    fn navigate_opens_the_main_window_on_that_section() {
        let mut app = App {
            tray_up: true,
            ..App::default()
        };
        // The window, and the history page's first request.
        assert_eq!(app.update(Message::Navigate(Section::History)).units(), 2);
        assert!(app.window.is_some());
        assert_eq!(app.section, Section::History);
        assert!(app.pages.history.is_loading());
        // With the window open, moving between sections opens nothing more.
        assert_eq!(app.update(Message::Navigate(Section::About)).units(), 0);
        assert_eq!(app.section, Section::About);
        // The tray can ask for a section too; the open window is raised.
        assert_eq!(
            app.update(Message::OpenWindow(Some(Section::Settings)))
                .units(),
            1
        );
        assert_eq!(app.section, Section::Settings);
        let _ = app.update(Message::OpenWindow(None));
        assert_eq!(app.section, Section::Settings);
    }

    #[test]
    fn opening_the_window_on_history_loads_it_once() {
        let mut app = App {
            tray_up: true,
            section: Section::History,
            ..App::default()
        };
        // Window plus one history request, not two.
        assert_eq!(app.show_window().units(), 2);
        assert!(app.pages.history.is_loading());
        assert_eq!(app.load_history().units(), 0);
    }

    #[test]
    fn navigate_and_sidebar_keep_a_background_window() {
        let (mut app, _) = boot(true);
        let _ = app.update(Message::Navigate(Section::DnsLeak));
        assert_eq!(app.section, Section::DnsLeak);
        assert_eq!(app.update(Message::TrayReady(true)).units(), 0);
        assert!(app.window.is_some());
    }

    #[test]
    fn pages_recover_after_the_service_goes_down_and_comes_back() {
        let (mut app, _) = boot(false);
        let _ = app.update(Message::Snapshot(Box::new(fixtures::connected_snapshot())));
        // A request in flight when the service restarts ends with text, not silence.
        let failed = ipc::route(Reply::History, Err("x".into()));
        let _ = app.update(Message::ServiceDown(ServiceDown {
            text: "not in group".into(),
            not_in_group: true,
            ..ServiceDown::default()
        }));
        assert!(app.snapshot.is_none());
        assert_eq!(
            ui::badge(
                Section::Settings,
                app.snapshot.as_ref(),
                app.service.as_ref()
            ),
            Badge::Warning
        );
        assert_eq!(app.update(failed).units(), 0);
        // Resubscribed: the first snapshot clears the problem and pages run as before.
        let _ = app.update(Message::Snapshot(Box::new(
            fixtures::disconnected_snapshot(),
        )));
        assert!(app.service.is_none());
        assert!(app.snapshot.is_some());
        assert_eq!(
            ui::badge(
                Section::Settings,
                app.snapshot.as_ref(),
                app.service.as_ref()
            ),
            Badge::None
        );
        let loaded = Message::Page(PageMsg::History(history::Msg::Loaded(Err("y".into()))));
        assert_eq!(app.update(loaded).units(), 0);
        let _ = app.update(Message::Navigate(Section::History));
        let _ = app.view(app.window.unwrap());
    }

    #[test]
    fn prefs_change_returns_save_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellglide/gui.json");
        let (mut app, _) = App::boot_with(
            false,
            path.clone(),
            PathBuf::from(TEST_HISTORY),
            None,
            PathBuf::from(TEST_CONFIG),
        );
        assert!(app.prefs.include_wifi);
        let save = app.apply_pref(PrefChange::IncludeWifi(false));
        assert!(!app.prefs.include_wifi);
        let Effect::SavePrefs(prefs) = save.clone() else {
            panic!("expected SavePrefs, got {save:?}");
        };
        assert_eq!(prefs, app.prefs);
        // Deciding writes nothing; the writer thread does, and the newest snapshot wins.
        assert!(!path.exists());
        let _ = app.run_effect(save);
        let _ = app.run_effect(Effect::Prefs(PrefChange::SpeedConfirmed));
        assert!(app.prefs.speed_confirmed);
        app.writer.flush();
        assert_eq!(prefs::load(&path), app.prefs);
        let (app, _) = App::boot_with(
            true,
            path,
            PathBuf::from(TEST_HISTORY),
            None,
            PathBuf::from(TEST_CONFIG),
        );
        assert!(!app.prefs.include_wifi && app.prefs.speed_confirmed);
    }

    #[test]
    fn plugging_in_opens_the_window_only_when_asked() {
        let with_phone = |serials: &[&str]| {
            let mut snapshot = fixtures::disconnected_snapshot();
            snapshot.devices = serials.iter().map(|s| s.to_string()).collect();
            Message::Snapshot(Box::new(snapshot))
        };
        // Window closed, tray up: an opened window shows as `window` becoming Some.
        let (mut app, _) = boot(false);
        app.window = None;
        app.tray_up = true;
        app.prefs.open_on_plug = true;
        // The phone was already there when we started.
        let _ = app.update(with_phone(&["aaaa"]));
        assert!(app.window.is_none());
        // Still there after the service restarted: the first snapshot again doesn't count.
        let _ = app.update(Message::ServiceDown(down("restart")));
        assert!(app.known_devices.is_none());
        let _ = app.update(with_phone(&["aaaa"]));
        assert!(app.window.is_none());
        // Unplugged, then plugged back in a minute later.
        let _ = app.update(with_phone(&[]));
        assert!(app.window.is_none());
        // As if it went missing a minute ago (`plugged_in` keeps when, privately).
        let a_minute_ago = Instant::now()
            .checked_sub(ui::REPLUG_AFTER)
            .expect("the clock has run for a minute");
        let mut known = None;
        plugged_in(&mut known, &["aaaa".into()], a_minute_ago);
        plugged_in(&mut known, &[], a_minute_ago);
        app.known_devices = known;
        let _ = app.update(with_phone(&["aaaa"]));
        assert!(app.window.is_some());
        assert_eq!(app.section, Section::Overview);

        // Switched off: never opens.
        let (mut app, _) = boot(false);
        app.window = None;
        app.tray_up = true;
        let _ = app.update(with_phone(&[]));
        let _ = app.update(with_phone(&["aaaa"]));
        assert!(app.window.is_none());
    }

    #[test]
    fn plugging_in_with_the_window_open_raises_it_without_changing_the_page() {
        let (mut app, _) = boot(false);
        app.prefs.open_on_plug = true;
        app.section = Section::SpeedTest;
        let with_phone = |serials: &[&str]| {
            let mut snapshot = fixtures::disconnected_snapshot();
            snapshot.devices = serials.iter().map(|s| s.to_string()).collect();
            Message::Snapshot(Box::new(snapshot))
        };
        let _ = app.update(with_phone(&[]));
        let raised = app.update(with_phone(&["aaaa"]));
        assert!(raised.units() >= 1);
        assert!(app.window.is_some());
        assert_eq!(app.section, Section::SpeedTest);
    }

    #[test]
    fn start_at_login_is_written_by_the_apps_own_effect() {
        let dir = tempfile::tempdir().unwrap();
        let boot_in = |dir: &Path| {
            App::boot_with(
                false,
                PathBuf::from("/nonexistent/cellglide/gui.json"),
                PathBuf::from(TEST_HISTORY),
                None,
                dir.to_path_buf(),
            )
            .0
        };
        let mut app = boot_in(dir.path());
        assert_eq!(app.config_home, dir.path());
        assert!(app.pages.settings.autostart());
        assert_eq!(app.run_effect(Effect::SetAutostart(false)).units(), 1);
        // The effect's thread writes the file; wait for it (the rename is atomic).
        let file = autostart::user_file(dir.path());
        for _ in 0..200 {
            if file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(file.exists(), "the effect never wrote {}", file.display());
        // The next launch reads it back as off.
        let app = boot_in(dir.path());
        assert!(!app.pages.settings.autostart());
        let _ = app.view(app.window.unwrap());
    }

    #[test]
    fn effects_become_tasks() {
        let mut app = App::default();
        assert_eq!(app.run_effect(Effect::None).units(), 0);
        assert_eq!(
            app.run_effect(Effect::OpenUrl("http://x.test".into()))
                .units(),
            0
        );
        assert_eq!(
            app.run_effect(Effect::OpenUrl("file:///etc".into()))
                .units(),
            0
        );
        assert_eq!(
            app.run_effect(Effect::OpenUrl("https://x.test".into()))
                .units(),
            1
        );
        assert_eq!(app.run_effect(Effect::Copy("x".into())).units(), 1);
        // Quit is the tray's Quit: with no panel open, just the exit.
        assert_eq!(app.run_effect(Effect::Quit).units(), 1);
        assert_eq!(app.update(Message::After(AfterMsg::AboutCopied)).units(), 0);
        assert_eq!(
            app.run_effect(Effect::After(
                Duration::from_secs(2),
                AfterMsg::SettingsCopied
            ))
            .units(),
            1
        );
        assert_eq!(
            app.run_effect(Effect::Request(
                Request::History { day: None },
                Reply::History
            ))
            .units(),
            1
        );
    }

    #[test]
    fn only_focus_and_close_events_reach_update() {
        let id = window::Id::unique();
        let status = iced::event::Status::Ignored;
        let event = |e| iced::Event::Window(e);
        for wanted in [
            window::Event::Focused,
            window::Event::Unfocused,
            window::Event::CloseRequested,
            window::Event::Closed,
        ] {
            assert!(window_message(event(wanted), status, id).is_some());
        }
        assert!(
            window_message(
                event(window::Event::RedrawRequested(Instant::now())),
                status,
                id
            )
            .is_none()
        );
        assert!(
            window_message(
                event(window::Event::Resized(Size::new(1.0, 1.0))),
                status,
                id
            )
            .is_none()
        );
    }

    /// Clicks the tray and lets the panel's window appear, as iced reports it.
    fn open_panel(app: &mut App) -> window::Id {
        assert_eq!(app.update(Message::TogglePanel).units(), 1);
        let id = app.panel.pending_id().expect("panel requested");
        assert_eq!(app.update(Message::PanelOpened(id)).units(), 0);
        assert_eq!(app.panel.id, Some(id));
        id
    }

    #[test]
    fn opening_the_main_window_closes_the_panel() {
        let (mut app, _) = boot(false);
        let panel = open_panel(&mut app);
        // Raise the main window and close the panel.
        assert_eq!(app.update(Message::OpenWindow(None)).units(), 2);
        assert!(!app.panel.is_open());
        // With no panel, only the window is raised.
        assert_eq!(app.update(Message::OpenWindow(None)).units(), 1);
        // The panel's own close event then changes nothing for the main window.
        assert_eq!(app.update(closed(panel)).units(), 0);
        assert!(app.window.is_some());

        let _ = open_panel(&mut app);
        // Quit closes the panel and exits.
        assert_eq!(app.update(Message::Quit).units(), 2);
        assert!(!app.panel.is_open());

        // The About page's Quit (Effect::Quit) does exactly the same.
        let _ = open_panel(&mut app);
        assert_eq!(app.run_effect(Effect::Quit).units(), 2);
        assert!(!app.panel.is_open());
    }

    #[test]
    fn a_fast_double_click_makes_one_panel() {
        let (mut app, _) = boot(false);
        assert_eq!(app.update(Message::TogglePanel).units(), 1);
        // The window is still being created: nothing to open, nothing iced could close.
        assert_eq!(app.update(Message::TogglePanel).units(), 0);
        let id = app.panel.pending_id().unwrap();
        // It already draws as the panel.
        let _ = app.view(id);
        assert_eq!(app.update(Message::PanelOpened(id)).units(), 0);
        assert_eq!(app.panel.id, Some(id));
        // Opening the main window while it is being created closes it once it exists.
        let mut app = App::default();
        let _ = app.update(Message::TogglePanel);
        let id = app.panel.pending_id().unwrap();
        let _ = app.update(Message::OpenWindow(None));
        assert_eq!(app.update(Message::PanelOpened(id)).units(), 1);
        assert!(!app.panel.is_open());
    }

    #[test]
    fn panel_messages_reach_the_panel() {
        let (mut app, _) = boot(false);
        let id = open_panel(&mut app);
        // The panel draws in its own window; the main window still draws the page.
        let _ = app.view(id);
        let _ = app.view(app.window.unwrap());
        // A second click while open closes it.
        assert_eq!(app.update(Message::TogglePanel).units(), 1);
        assert!(!app.panel.is_open());
        let _ = app.update(closed(id));

        let id = open_panel(&mut app);
        let escape = Message::Key(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::default(),
            text: None,
            repeat: false,
        });
        assert_eq!(app.update(escape.clone()).units(), 1);
        // Esc with nothing open does nothing.
        assert_eq!(app.update(escape).units(), 0);
        let _ = app.update(closed(id));

        // Focus then focus loss closes it; ticks within the minute don't.
        let id = open_panel(&mut app);
        assert_eq!(app.update(Message::Tick(Instant::now())).units(), 0);
        let _ = app.update(Message::Window(id, window::Event::Focused));
        assert_eq!(
            app.update(Message::Window(id, window::Event::Unfocused))
                .units(),
            1
        );
        assert!(!app.panel.is_open());
        // The main window's focus is not the panel's.
        let main = app.window.unwrap();
        assert_eq!(
            app.update(Message::Window(main, window::Event::Unfocused))
                .units(),
            0
        );
    }

    #[test]
    fn a_second_press_waits_for_the_first() {
        let mut app = App::default();
        let _ = app.update(Message::Snapshot(Box::new(
            fixtures::disconnected_snapshot(),
        )));
        assert_eq!(app.update(Message::Toggle).units(), 1);
        assert_eq!(app.update(Message::Toggle).units(), 0);
        assert!(app.command_pending());
        // The reply alone is not enough: the snapshot must show the wish.
        let _ = app.update(Message::CommandDone(Ok(())));
        assert!(app.command_pending());
        assert_eq!(app.update(Message::Toggle).units(), 0);
        let _ = app.update(Message::Snapshot(Box::new(fixtures::connected_snapshot())));
        assert!(!app.command_pending());
        assert_eq!(app.update(Message::Toggle).units(), 1);

        // A refused press frees the button at once.
        let _ = app.update(Message::CommandDone(Err("no".into())));
        assert!(!app.command_pending());

        // A reply whose snapshot never matches frees it a second after the press.
        let mut app = App::default();
        let _ = app.update(Message::Toggle);
        let _ = app.update(Message::CommandDone(Ok(())));
        let sent = app.command.unwrap().sent;
        assert!(app.command_pending_at(sent + Duration::from_millis(900)));
        assert!(!app.command_pending_at(sent + COMMAND_MIN));
        // Without a reply it keeps waiting (the call itself times out with an error).
        let mut app = App::default();
        let _ = app.update(Message::Toggle);
        let sent = app.command.unwrap().sent;
        assert!(app.command_pending_at(sent + Duration::from_secs(5)));
    }

    #[test]
    fn speed_test_runs_as_a_task_and_cancel_stops_it() {
        let mut app = App::default();
        let job = speed::SpeedJob {
            plan: cg_tools::speed::plan(false, Some("wlp2s0".into()), true).unwrap(),
            routes: Vec::new(),
            carrier: None,
            generation: None,
        };
        // Nothing to stop yet.
        assert_eq!(
            app.run_effect(Effect::StartLeak { sharing: None }).units(),
            1
        );
        assert_eq!(app.run_effect(Effect::CancelSpeed).units(), 0);
        assert_eq!(app.run_effect(Effect::StartSpeed(job.clone())).units(), 1);
        let cancel = app.speed_cancel.clone().expect("a token for the run");
        assert!(!cancel.is_cancelled());
        let _ = app.run_effect(Effect::CancelSpeed);
        assert!(cancel.is_cancelled());
        // A new run cancels any earlier one it would otherwise orphan.
        let _ = app.run_effect(Effect::StartSpeed(job.clone()));
        let first = app.speed_cancel.clone().unwrap();
        let _ = app.run_effect(Effect::StartSpeed(job));
        assert!(first.is_cancelled());
        assert!(!app.speed_cancel.as_ref().unwrap().is_cancelled());
        let finished = speed::Msg::Finished(cg_tools::speed::session::Run {
            legs: Vec::new(),
            cancelled: true,
        });
        let _ = app.update(Message::Page(PageMsg::Speed(finished)));
        assert!(app.speed_cancel.is_none());
    }

    #[test]
    fn losing_the_phone_or_the_service_closes_the_speed_warning() {
        let (mut app, _) = boot(false);
        let _ = app.update(Message::Navigate(Section::SpeedTest));
        for gone in [
            Message::Snapshot(Box::new(fixtures::disconnected_snapshot())),
            Message::ServiceDown(down("down")),
        ] {
            let _ = app.update(Message::Snapshot(Box::new(fixtures::connected_snapshot())));
            let _ = app.update(Message::Page(PageMsg::Speed(speed::Msg::Start)));
            assert!(app.pages.speed.warning_shown());
            let _ = app.view(app.window.unwrap());
            let _ = app.update(gone);
            assert!(!app.pages.speed.warning_shown());
        }
    }

    #[test]
    fn ticks_move_the_clock() {
        let mut app = App::default();
        assert_eq!(app.update(Message::Tick(Instant::now())).units(), 0);
        assert!(app.now > Timestamp::UNIX_EPOCH);
    }
}
