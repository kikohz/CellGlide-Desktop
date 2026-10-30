//! The diagnostics page, after the Mac's `DiagnosticsPage` without its log export card: pick a
//! range, add an optional email and note, look at exactly what would be sent, then send it; or
//! save the bundle to the Downloads folder.
//!
//! Stricter than the Mac: Send works only once the bundle on hand has been opened in the
//! preview, and any change of range, email or note throws that bundle away. What is uploaded
//! is the preview's text, byte for byte. The fields are frozen while a bundle is being built,
//! so the reply always belongs to the fields on screen.

use super::Ctx;
use crate::effect::{AfterMsg, Effect, Reply};
use crate::widgets::kit::{self, SEMIBOLD, Tone, ToneColor};
use crate::widgets::pager::{self, Pager};
use cg_diag::upload::UploadError;
use cg_i18n::tr;
use cg_ipc::describe::{self, ui::contact};
use cg_ipc::{DiagRange, Request};
use iced::widget::{
    button, column, container, keyed_column, opaque, row, rule, scrollable, space, stack, text,
    text_editor, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Font, Length, Theme};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long "Copied" shows before turning back into "Copy".
const COPIED_FOR: Duration = Duration::from_secs(2);
/// The width of the form's labels, as on the Mac.
const LABEL_WIDTH: f32 = 90.0;

/// What to do with the bundle once the service has built it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    Preview,
    Save,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Busy {
    #[default]
    Idle,
    Preparing(Then),
    Sending,
    Saving,
}

#[derive(Debug)]
pub struct State {
    range: DiagRange,
    email: String,
    note: text_editor::Content,
    /// The bundle for the fields on screen, once built.
    prepared: Option<Pager>,
    /// The preview is open.
    previewing: bool,
    /// `prepared` has been opened in the preview: the one condition for sending it.
    previewed: bool,
    busy: Busy,
    /// The report code of the bundle just sent.
    code: Option<String>,
    failure: Option<String>,
    /// Why the bundle couldn't be built (the service's words, or the timeout sentence).
    failure_detail: Option<String>,
    saved: Option<PathBuf>,
    copied: bool,
}

impl Default for State {
    fn default() -> State {
        State {
            range: DiagRange::Day,
            email: String::new(),
            note: text_editor::Content::new(),
            prepared: None,
            previewing: false,
            previewed: false,
            busy: Busy::Idle,
            code: None,
            failure: None,
            failure_detail: None,
            saved: None,
            copied: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Range(DiagRange),
    Email(String),
    Note(text_editor::Action),
    /// "View what will be sent…".
    View,
    /// The bundle the service built, as the JSON text that would be sent.
    Prepared(Result<String, String>),
    PagePrev,
    PageNext,
    /// Copies the whole bundle from the preview.
    CopyPreview,
    ClosePreview,
    Send,
    Sent(Result<String, UploadError>),
    Save,
    /// The saved file, or the sentence saying why it couldn't be written.
    Saved(Result<PathBuf, String>),
    OpenFolder,
    CopyCode,
    /// Two seconds after a copy: "Copied" turns back into "Copy".
    CopiedEnd,
    /// "Send Another": a new report with the same email and note.
    Again,
    Done,
}

/// Send works only for the bundle that was opened in the preview, and not while busy.
pub fn can_send(state: &State) -> bool {
    state.prepared.is_some() && state.previewed && state.busy == Busy::Idle
}

fn note_text(state: &State) -> String {
    state.note.text()
}

/// The fields changed: the old bundle no longer matches them.
fn invalidate(state: &mut State) {
    state.prepared = None;
    state.previewed = false;
    state.previewing = false;
    state.failure = None;
    state.failure_detail = None;
}

/// Asks the service for the bundle of the fields on screen.
fn prepare(state: &mut State, then: Then) -> Effect {
    state.busy = Busy::Preparing(then);
    state.failure = None;
    state.failure_detail = None;
    Effect::Request(
        Request::Diagnostics {
            range: state.range,
            contact: contact(&state.email, &note_text(state)),
        },
        Reply::Diagnostics,
    )
}

fn save(state: &mut State, json: String) -> Effect {
    state.busy = Busy::Saving;
    state.saved = None;
    state.failure = None;
    state.failure_detail = None;
    Effect::SaveDiagnostics(json)
}

/// Forgets the last report so a new one never sends or shows an old bundle.
fn reset(state: &mut State) {
    invalidate(state);
    state.code = None;
    state.copied = false;
    state.saved = None;
}

pub fn update(state: &mut State, message: Msg, _ctx: &Ctx) -> Effect {
    let idle = state.busy == Busy::Idle;
    match message {
        // The fields are frozen while busy, so a reply always matches what is on screen.
        Msg::Range(range) if idle && range != state.range => {
            state.range = range;
            invalidate(state);
        }
        Msg::Email(email) if idle => {
            state.email = email;
            invalidate(state);
        }
        Msg::Note(action) if idle || !action.is_edit() => {
            let edit = action.is_edit();
            state.note.perform(action);
            if edit {
                invalidate(state);
            }
        }
        Msg::Range(_) | Msg::Email(_) | Msg::Note(_) => {}
        Msg::View if idle => {
            if state.prepared.is_some() {
                state.previewing = true;
                state.previewed = true;
                state.copied = false;
            } else {
                return prepare(state, Then::Preview);
            }
        }
        Msg::Save if idle => {
            return match &state.prepared {
                Some(pager) => {
                    let json = pager.text.clone();
                    save(state, json)
                }
                None => prepare(state, Then::Save),
            };
        }
        Msg::View | Msg::Save => {}
        Msg::Prepared(result) => {
            let Busy::Preparing(then) = state.busy else {
                return Effect::None;
            };
            state.busy = Busy::Idle;
            match result {
                Ok(json) => {
                    state.prepared = Some(Pager::new(json.clone()));
                    match then {
                        Then::Preview => {
                            state.previewing = true;
                            state.previewed = true;
                            state.copied = false;
                        }
                        Then::Save => return save(state, json),
                    }
                }
                Err(reason) => {
                    state.failure = Some(tr("diag.build_failed", &[]));
                    state.failure_detail = Some(reason);
                }
            }
        }
        Msg::PagePrev => {
            if let Some(pager) = &mut state.prepared {
                pager.prev();
            }
        }
        Msg::PageNext => {
            if let Some(pager) = &mut state.prepared {
                pager.next();
            }
        }
        Msg::CopyPreview => {
            if state.previewing
                && let Some(pager) = &state.prepared
            {
                state.copied = true;
                return Effect::Batch(vec![
                    Effect::Copy(pager.text.clone()),
                    Effect::After(COPIED_FOR, AfterMsg::DiagnosticsCopied),
                ]);
            }
        }
        Msg::ClosePreview => {
            state.previewing = false;
            state.copied = false;
        }
        Msg::Send => {
            if !can_send(state) {
                return Effect::None;
            }
            if let Some(pager) = &state.prepared {
                state.busy = Busy::Sending;
                state.failure = None;
                state.failure_detail = None;
                return Effect::Upload(pager.text.clone());
            }
        }
        Msg::Sent(result) => {
            if state.busy != Busy::Sending {
                return Effect::None;
            }
            state.busy = Busy::Idle;
            state.previewing = false;
            state.copied = false;
            match result {
                Ok(code) => {
                    // A sent bundle is done with: another report needs a new preview.
                    state.code = Some(code);
                    state.prepared = None;
                    state.previewed = false;
                }
                Err(UploadError::TooFrequent) => {
                    state.failure = Some(tr("diag.too_frequent", &[]));
                }
                Err(_) => state.failure = Some(tr("diag.failed_gui", &[])),
            }
        }
        Msg::Saved(result) => {
            if state.busy != Busy::Saving {
                return Effect::None;
            }
            state.busy = Busy::Idle;
            match result {
                Ok(path) => state.saved = Some(path),
                Err(text) => state.failure = Some(text),
            }
        }
        Msg::OpenFolder => {
            if let Some(dir) = state.saved.as_deref().and_then(Path::parent) {
                return Effect::OpenFolder(dir.to_path_buf());
            }
        }
        Msg::CopyCode => {
            if let Some(code) = &state.code {
                state.copied = true;
                return Effect::Batch(vec![
                    Effect::Copy(code.clone()),
                    Effect::After(COPIED_FOR, AfterMsg::DiagnosticsCopied),
                ]);
            }
        }
        Msg::CopiedEnd => state.copied = false,
        Msg::Again if idle => reset(state),
        Msg::Done if idle => {
            reset(state);
            state.email.clear();
            state.note = text_editor::Content::new();
        }
        Msg::Again | Msg::Done => {}
    }
    Effect::None
}

fn label<'a>(key: &str) -> Element<'a, Msg> {
    kit::secondary(tr(key, &[]))
        .size(13)
        .width(LABEL_WIDTH)
        .into()
}

fn red<'a>(content: String) -> Element<'a, Msg> {
    text(content)
        .size(12)
        .style(|_: &Theme| text::Style {
            color: Some(Tone::Red.color()),
        })
        .into()
}

fn small(key: &str) -> text::Text<'static> {
    text(tr(key, &[])).size(12)
}

/// One of the two range buttons; the chosen one is filled.
fn range_button<'a>(state: &State, range: DiagRange, key: &str) -> Element<'a, Msg> {
    let chosen = state.range == range;
    button(small(key))
        .padding([4, 10])
        .style(if chosen {
            button::primary
        } else {
            button::secondary
        })
        .on_press_maybe((state.busy == Busy::Idle).then_some(Msg::Range(range)))
        .into()
}

fn busy_line(busy: Busy) -> Option<String> {
    match busy {
        Busy::Preparing(_) => Some(tr("diag.preparing", &[])),
        Busy::Sending => Some(tr("diag.sending", &[])),
        Busy::Idle | Busy::Saving => None,
    }
}

fn form(state: &State) -> Element<'_, Msg> {
    let idle = state.busy == Busy::Idle;
    let range = row![
        label("diag.range"),
        range_button(state, DiagRange::Day, "diag.range_day"),
        range_button(state, DiagRange::Week, "diag.range_week"),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let email = row![
        label("diag.email"),
        text_input(&tr("common.optional", &[]), &state.email)
            .on_input_maybe(idle.then_some(Msg::Email))
            .size(13),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let mut editor = text_editor(&state.note)
        .placeholder(tr("common.optional", &[]))
        .height(76)
        .size(13);
    // Read-only while busy; moving the cursor is harmless, so only edits are refused there.
    editor = editor.on_action(Msg::Note);
    let note = row![container(label("diag.note")).padding([4, 0]), editor].spacing(12);

    let mut status = column![].spacing(2).width(Length::Fill);
    if let Some(failure) = &state.failure {
        status = status.push(red(failure.clone()));
    }
    if let Some(detail) = &state.failure_detail {
        status = status.push(kit::secondary(detail.clone()));
    }
    if let Some(line) = busy_line(state.busy) {
        status = status.push(kit::secondary(line));
    }
    let buttons = row![
        status,
        button(small("diag.save"))
            .style(button::secondary)
            .on_press_maybe(idle.then_some(Msg::Save)),
        button(small("diag.view"))
            .style(button::secondary)
            .on_press_maybe(idle.then_some(Msg::View)),
        button(small("diag.send")).on_press_maybe(can_send(state).then_some(Msg::Send)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut body = column![
        kit::secondary(tr("diag.about_gui", &[])).size(13),
        rule::horizontal(1),
        range,
        rule::horizontal(1),
        email,
        rule::horizontal(1),
        note,
        rule::horizontal(1),
        buttons,
    ]
    .spacing(10);
    if idle && !can_send(state) {
        body = body.push(
            row![
                space::horizontal(),
                kit::secondary(tr("diag.send_needs_preview", &[]))
            ]
            .width(Length::Fill),
        );
    }
    if let Some(path) = &state.saved {
        body = body.push(
            row![
                kit::secondary(tr("diag.saved", &[("path", &path.display().to_string())]))
                    .width(Length::Fill),
                button(small("diag.open_folder"))
                    .style(button::secondary)
                    .on_press(Msg::OpenFolder),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    kit::card(Some(tr("diag.send_title", &[])), body)
}

fn copy_label(copied: bool) -> String {
    tr(
        if copied {
            "common.copied"
        } else {
            "common.copy"
        },
        &[],
    )
}

fn sent_card<'a>(state: &'a State, code: &'a str) -> Element<'a, Msg> {
    let code_box = container(text(code).font(Font::MONOSPACE).size(15))
        .padding([4, 10])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme.palette().text.scale_alpha(0.06))),
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let body = column![
        kit::dot(Tone::Green, 28.0),
        text(tr("diag.sent_title", &[])).size(15).font(SEMIBOLD),
        row![
            code_box,
            button(text(copy_label(state.copied)).size(12))
                .style(button::secondary)
                .on_press(Msg::CopyCode),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        kit::secondary(tr("diag.code_hint", &[])).size(13),
        row![
            button(small("diag.again"))
                .style(button::secondary)
                .on_press(Msg::Again),
            button(small("common.done")).on_press(Msg::Done),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    kit::card(
        None,
        container(body).center_x(Length::Fill).padding([14, 0]),
    )
}

/// The bundle, page by page, over the page; Send here sends exactly this text.
fn preview<'a>(state: &'a State, pager: &'a Pager) -> Element<'a, Msg> {
    let size = tr(
        "diag.preview_size",
        &[("size", &describe::amount(pager.text.len() as i64))],
    );
    // Keyed by page, so each page opens scrolled to its top.
    let page: Element<'a, Msg> =
        scrollable(container(pager::view(pager, Msg::PagePrev, Msg::PageNext)).padding([0, 12]))
            .height(Length::Fill)
            .into();
    let pages = keyed_column([(pager.index, page)]).height(Length::Fill);
    let mut status = column![].spacing(2).width(Length::Fill);
    if let Some(failure) = &state.failure {
        status = status.push(red(failure.clone()));
    }
    if let Some(line) = busy_line(state.busy) {
        status = status.push(kit::secondary(line));
    }
    let bottom = row![
        button(text(copy_label(state.copied)).size(12))
            .style(button::secondary)
            .on_press(Msg::CopyPreview),
        status,
        button(small("common.close"))
            .style(button::secondary)
            .on_press(Msg::ClosePreview),
        button(small("diag.send")).on_press_maybe(can_send(state).then_some(Msg::Send)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let card = container(
        column![
            row![
                text(tr("diag.preview_title", &[])).size(15).font(SEMIBOLD),
                space::horizontal(),
                kit::secondary(size),
            ]
            .align_y(Alignment::Center),
            pages,
            bottom,
        ]
        .spacing(10),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
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
    container(card)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(Background::Color(Color::BLACK.scale_alpha(0.3))),
            ..container::Style::default()
        })
        .into()
}

pub fn view<'a>(state: &'a State, _ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let content = match &state.code {
        Some(code) => sent_card(state, code),
        None => form(state),
    };
    let page = kit::page(tr("nav.diagnostics", &[]), content);
    match (&state.prepared, state.previewing) {
        (Some(pager), true) => stack![page, opaque(preview(state, pager))].into(),
        _ => page,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{AfterMsg, Reply};
    use cg_i18n::Lang;
    use cg_ipc::{Contact, DiagRange, Request};

    const J: &str = "{\n  \"version\": 1,\n  \"log\": \"第一行\"\n}\n";

    fn ctx() -> Ctx<'static> {
        Ctx::for_test(None)
    }

    fn step(state: &mut State, msg: Msg) -> Effect {
        update(state, msg, &ctx())
    }

    fn request(range: DiagRange, contact: Option<Contact>) -> Effect {
        Effect::Request(Request::Diagnostics { range, contact }, Reply::Diagnostics)
    }

    /// Viewed J: the state in which Send is allowed.
    fn previewed() -> State {
        let mut state = State::default();
        step(&mut state, Msg::View);
        step(&mut state, Msg::Prepared(Ok(J.into())));
        state
    }

    #[test]
    fn send_is_refused_until_the_exact_bundle_was_previewed() {
        let mut state = State::default();
        assert!(!can_send(&state));
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
        assert_eq!(step(&mut state, Msg::View), request(DiagRange::Day, None));
        assert_eq!(state.busy, Busy::Preparing(Then::Preview));
        assert!(!can_send(&state));
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
        assert_eq!(step(&mut state, Msg::Prepared(Ok(J.into()))), Effect::None);
        assert!(state.previewing && state.previewed);
        assert_eq!(state.busy, Busy::Idle);
        assert!(can_send(&state));
        // The bytes sent are the bytes shown.
        assert_eq!(state.prepared.as_ref().unwrap().text, J);
        assert_eq!(step(&mut state, Msg::Send), Effect::Upload(J.into()));
        assert_eq!(state.busy, Busy::Sending);
        // No second upload while the first is out.
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
    }

    #[test]
    fn changing_a_field_requires_a_new_preview() {
        let mut state = previewed();
        assert_eq!(step(&mut state, Msg::Email("a@b.c".into())), Effect::None);
        assert!(state.prepared.is_none() && !state.previewed);
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
        assert_eq!(
            step(&mut state, Msg::View),
            request(
                DiagRange::Day,
                Some(Contact {
                    email: Some("a@b.c".into()),
                    note: None
                })
            )
        );

        let mut state = previewed();
        step(&mut state, Msg::Range(DiagRange::Week));
        assert!(state.prepared.is_none() && !can_send(&state));
        assert_eq!(step(&mut state, Msg::View), request(DiagRange::Week, None));

        let mut state = previewed();
        step(
            &mut state,
            Msg::Note(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        );
        assert!(state.prepared.is_none() && !can_send(&state));
        assert_eq!(
            step(&mut state, Msg::View),
            request(
                DiagRange::Day,
                Some(Contact {
                    email: None,
                    note: Some("x".into())
                })
            )
        );

        // Moving the cursor or picking the range already chosen changes nothing sent.
        let mut state = previewed();
        step(
            &mut state,
            Msg::Note(text_editor::Action::Move(text_editor::Motion::Home)),
        );
        step(&mut state, Msg::Range(DiagRange::Day));
        assert!(can_send(&state));
    }

    #[test]
    fn fields_are_frozen_while_a_bundle_is_being_prepared() {
        let mut state = State::default();
        step(&mut state, Msg::View);
        step(&mut state, Msg::Email("late@b.c".into()));
        step(&mut state, Msg::Range(DiagRange::Week));
        assert_eq!(state.email, "");
        assert_eq!(state.range, DiagRange::Day);
        // So the reply is the bundle of the fields on screen.
        step(&mut state, Msg::Prepared(Ok(J.into())));
        assert!(can_send(&state));
    }

    #[test]
    fn replies_nobody_waits_for_are_dropped() {
        let mut state = State::default();
        assert_eq!(step(&mut state, Msg::Prepared(Ok(J.into()))), Effect::None);
        assert!(state.prepared.is_none() && !can_send(&state));
        step(&mut state, Msg::Sent(Ok("CG-ABCDE".into())));
        assert_eq!(state.code, None);
        step(&mut state, Msg::Saved(Ok("/x/y.json".into())));
        assert_eq!(state.saved, None);
    }

    #[test]
    fn service_error_ends_preparing() {
        let _guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::ZhCn);
        let mut state = State::default();
        step(&mut state, Msg::View);
        step(&mut state, Msg::Prepared(Err("服务已停止".into())));
        assert_eq!(state.busy, Busy::Idle);
        assert_eq!(state.failure, Some(tr("diag.build_failed", &[])));
        assert_eq!(state.failure_detail.as_deref(), Some("服务已停止"));
        assert!(!state.previewing && !can_send(&state));
        // A service that never answered: the ipc layer's timeout sentence is the reason.
        let timeout = tr("gui.service_timeout", &[]);
        step(&mut state, Msg::Save);
        assert_eq!(state.busy, Busy::Preparing(Then::Save));
        step(&mut state, Msg::Prepared(Err(timeout.clone())));
        assert_eq!(state.busy, Busy::Idle);
        assert_eq!(state.failure_detail, Some(timeout));
        // A new try clears the old failure.
        step(&mut state, Msg::View);
        assert_eq!(state.failure, None);
    }

    #[test]
    fn upload_failures_have_words() {
        let _guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::ZhCn);
        let sent = |result| {
            let mut state = previewed();
            step(&mut state, Msg::Send);
            step(&mut state, Msg::Sent(result));
            assert_eq!(state.busy, Busy::Idle);
            state
        };
        let state = sent(Err(UploadError::TooFrequent));
        assert_eq!(state.failure, Some(tr("diag.too_frequent", &[])));
        assert_eq!(state.code, None);
        for error in [
            UploadError::Offline,
            UploadError::Rejected(500),
            UploadError::BadReply,
        ] {
            let state = sent(Err(error));
            assert_eq!(state.failure, Some(tr("diag.failed_gui", &[])));
            // The same bundle can be sent again without another preview.
            assert!(can_send(&state));
        }
        let state = sent(Ok("CG-ABCDE".into()));
        assert_eq!(state.code.as_deref(), Some("CG-ABCDE"));
        assert_eq!(state.failure, None);
        assert!(!state.previewing);
        // The sent bundle is gone: sending again needs a new bundle and preview.
        assert!(state.prepared.is_none() && !state.previewed && !can_send(&state));
        let mut state = state;
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
        assert_eq!(step(&mut state, Msg::View), request(DiagRange::Day, None));
    }

    #[test]
    fn save_needs_no_preview_and_never_uploads() {
        let mut state = State::default();
        assert_eq!(step(&mut state, Msg::Save), request(DiagRange::Day, None));
        assert_eq!(state.busy, Busy::Preparing(Then::Save));
        assert_eq!(
            step(&mut state, Msg::Prepared(Ok(J.into()))),
            Effect::SaveDiagnostics(J.into())
        );
        assert_eq!(state.busy, Busy::Saving);
        assert!(!state.previewing && !state.previewed && !can_send(&state));
        step(
            &mut state,
            Msg::Saved(Ok("/home/x/Downloads/a.json".into())),
        );
        assert_eq!(state.busy, Busy::Idle);
        assert_eq!(
            step(&mut state, Msg::OpenFolder),
            Effect::OpenFolder("/home/x/Downloads".into())
        );
        // Saving the prepared bundle again needs no new request.
        assert_eq!(
            step(&mut state, Msg::Save),
            Effect::SaveDiagnostics(J.into())
        );
        assert_eq!(state.saved, None);
        // Still no send: a saved bundle was never shown.
        step(&mut state, Msg::Saved(Err("无法保存".into())));
        assert_eq!(state.failure.as_deref(), Some("无法保存"));
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
    }

    #[test]
    fn preview_pages_and_closes_without_losing_consent() {
        let text: String = (0..450).map(|n| format!("{n}\n")).collect();
        let mut state = State::default();
        step(&mut state, Msg::View);
        step(&mut state, Msg::Prepared(Ok(text.clone())));
        step(&mut state, Msg::PageNext);
        assert_eq!(state.prepared.as_ref().unwrap().index, 1);
        step(&mut state, Msg::PagePrev);
        assert_eq!(state.prepared.as_ref().unwrap().index, 0);
        step(&mut state, Msg::ClosePreview);
        assert!(!state.previewing && can_send(&state));
        // Opening it again shows the same bundle without asking the service.
        assert_eq!(step(&mut state, Msg::View), Effect::None);
        assert!(state.previewing);
        assert_eq!(step(&mut state, Msg::Send), Effect::Upload(text));
    }

    #[test]
    fn copying_the_code_turns_back_after_two_seconds() {
        let mut state = previewed();
        step(&mut state, Msg::Send);
        step(&mut state, Msg::Sent(Ok("CG-ABCDE".into())));
        assert_eq!(
            step(&mut state, Msg::CopyCode),
            Effect::Batch(vec![
                Effect::Copy("CG-ABCDE".into()),
                Effect::After(Duration::from_secs(2), AfterMsg::DiagnosticsCopied),
            ])
        );
        assert!(state.copied);
        step(&mut state, Msg::CopiedEnd);
        assert!(!state.copied);
    }

    #[test]
    fn again_keeps_the_contact_and_done_clears_it() {
        let mut state = State::default();
        step(&mut state, Msg::Email("a@b.c".into()));
        step(
            &mut state,
            Msg::Note(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        );
        step(&mut state, Msg::View);
        step(&mut state, Msg::Prepared(Ok(J.into())));
        step(&mut state, Msg::Send);
        step(&mut state, Msg::Sent(Ok("CG-ABCDE".into())));
        step(&mut state, Msg::Again);
        assert_eq!(state.code, None);
        assert!(state.prepared.is_none() && !state.previewed && state.failure.is_none());
        assert_eq!(state.email, "a@b.c");
        assert_eq!(note_text(&state), "x");
        assert_eq!(step(&mut state, Msg::Send), Effect::None);
        step(&mut state, Msg::Done);
        assert_eq!(state.email, "");
        assert_eq!(note_text(&state), "");
    }

    /// A tiny deterministic generator, so the walk below is the same every run.
    fn next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn no_upload_without_a_fresh_preview() {
        let bundles = ["A\n", "B\n", "C\n"];
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let (mut uploads, mut reports, mut after_report) = (0, 0, 0);
        for _ in 0..400 {
            let mut state = State::default();
            // What the user has seen in the preview since the last change of the fields.
            let mut shown: Option<String> = None;
            // A bundle has been built since the last report went out.
            let mut fresh = false;
            for _ in 0..40 {
                let msg = match next(&mut seed) % 17 {
                    0 => Msg::Range(DiagRange::Week),
                    1 => Msg::Range(DiagRange::Day),
                    2 => Msg::Email(format!("{}@b.c", next(&mut seed) % 3)),
                    3 => Msg::Note(text_editor::Action::Edit(text_editor::Edit::Insert('n'))),
                    4 | 5 => Msg::View,
                    6 | 7 => Msg::Prepared(Ok(bundles[(next(&mut seed) % 3) as usize].to_string())),
                    8 => Msg::Prepared(Err("x".into())),
                    9 | 10 => Msg::Send,
                    11 => Msg::Sent(Err(UploadError::Offline)),
                    12 => Msg::Sent(Ok("CG-1".into())),
                    13 => Msg::Save,
                    14 => Msg::Saved(Ok("/d/f.json".into())),
                    15 => Msg::ClosePreview,
                    _ => {
                        if next(&mut seed).is_multiple_of(2) {
                            Msg::Again
                        } else {
                            Msg::Done
                        }
                    }
                };
                let invalidates = matches!(
                    msg,
                    Msg::Range(_) | Msg::Email(_) | Msg::Note(_) | Msg::Again | Msg::Done
                ) && state.busy == Busy::Idle;
                let before = (state.range, state.email.clone(), note_text(&state));
                let (had_code, had_bundle) = (state.code.is_some(), state.prepared.is_some());
                let effect = step(&mut state, msg);
                let after = (state.range, state.email.clone(), note_text(&state));
                if invalidates && before != after {
                    shown = None;
                }
                if matches!(state.busy, Busy::Idle | Busy::Sending) && state.prepared.is_none() {
                    shown = None;
                }
                // Once a report has gone out, only a newly built bundle may be sent.
                if !had_code && state.code.is_some() {
                    shown = None;
                    fresh = false;
                    reports += 1;
                }
                if !had_bundle && state.prepared.is_some() {
                    fresh = true;
                }
                if state.previewing {
                    shown = state.prepared.as_ref().map(|p| p.text.clone());
                }
                if let Effect::Upload(bytes) = effect {
                    uploads += 1;
                    assert_eq!(shown.as_deref(), Some(bytes.as_str()), "{state:?}");
                    assert!(fresh, "sent again without a new bundle: {state:?}");
                    if reports > 0 {
                        after_report += 1;
                    }
                }
            }
        }
        assert!(reports > 5 && after_report > 5, "{reports} {after_report}");
        // The walk did reach Send with consent, so the check above was exercised.
        assert!(uploads > 20, "{uploads}");
    }

    #[test]
    fn fields_are_frozen_while_sending() {
        let mut state = previewed();
        assert_eq!(step(&mut state, Msg::Send), Effect::Upload(J.into()));
        step(&mut state, Msg::Email("late@b.c".into()));
        step(
            &mut state,
            Msg::Note(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        );
        step(&mut state, Msg::Range(DiagRange::Week));
        assert_eq!(state.email, "");
        assert_eq!(note_text(&state), "");
        assert_eq!(state.range, DiagRange::Day);
        step(&mut state, Msg::Sent(Err(UploadError::Offline)));
        // The retry sends the bytes that were previewed, unchanged.
        assert_eq!(step(&mut state, Msg::Send), Effect::Upload(J.into()));
    }

    /// One line of source names the uploader: its path, an import of it, or a call to a
    /// function called `upload` (spaces before the parenthesis allowed).
    fn names_the_uploader(line: &str) -> bool {
        let code = line.trim_start();
        if code.starts_with("//") {
            return false;
        }
        // Naming the error type alone (the page matches on it) sends nothing.
        let code = code.replace(concat!("cg_diag", "::upload::UploadError"), "");
        let code = code.as_str();
        if code.contains(concat!("cg_diag", "::upload"))
            || code.contains(concat!("upload", "::upload"))
        {
            return true;
        }
        let word = concat!("up", "load");
        code.match_indices(word).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            let after = code[at + word.len()..].trim_start();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && after.starts_with('(')
        })
    }

    /// Every line of every `.rs` file under `crates/<name>/src` that names the uploader, as
    /// (file name, line). Panics if the directory can't be read or holds no Rust files.
    fn uploader_uses(name: &str) -> Vec<(String, String)> {
        let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
            .join(name)
            .join("src");
        let mut dirs = vec![root.clone()];
        let (mut files, mut uses) = (0, Vec::new());
        while let Some(dir) = dirs.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("can't read {}: {e}", dir.display()));
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                files += 1;
                let source = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));
                let file = path.file_name().unwrap().to_string_lossy().to_string();
                for line in source.lines().filter(|l| names_the_uploader(l)) {
                    uses.push((file.clone(), line.trim().to_string()));
                }
            }
        }
        assert!(files > 0, "no Rust sources under {}", root.display());
        uses
    }

    #[test]
    fn the_scan_sees_every_spelling() {
        // `X` stands for the word, so these lines don't count as uses of this file.
        let real = |line: &str| line.replace('X', concat!("up", "load"));
        for line in [
            "use cg_diag::X::{Target, X};",
            "use cg_diag::X::{Target, UploadError, X};",
            "use cg_diag::X as send;",
            "let f = X::X;",
            "    X (body, &t).await",
            "match X(json.as_bytes(), &t) {",
        ] {
            assert!(names_the_uploader(&real(line)), "{line}");
        }
        for line in [
            "// X(json)",
            "r.X)",
            "X: Some(1.0),",
            "Xing: true,",
            "fn can_X_now()",
            "use cg_diag::X::UploadError;",
        ] {
            assert!(!names_the_uploader(&real(line)), "{line}");
        }
    }

    /// The bundle leaves this computer from exactly one place in the app, which sends to
    /// production; the CLI's `--send` is the only other; nothing else in the workspace can.
    #[test]
    fn one_upload_call_site() {
        let production = concat!("Target::", "production()");
        let gui = uploader_uses("cg-gui");
        assert_eq!(gui.len(), 1, "{gui:?}");
        assert_eq!(gui[0].0, "app.rs");
        assert!(gui[0].1.contains(production), "{gui:?}");
        // `Target::production()` appears once in app.rs: on that line.
        let app = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/app.rs"))
            .expect("app.rs");
        let targets = app
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && l.contains(production))
            .count();
        assert_eq!(targets, 1);

        // The CLI's own send: the import and the one call, to production.
        let cli = uploader_uses("cg-cli");
        assert_eq!(cli.len(), 2, "{cli:?}");
        assert!(cli.iter().all(|(file, _)| file == "main.rs"), "{cli:?}");
        assert!(
            cli.iter().any(|(_, line)| line.starts_with("use ")),
            "{cli:?}"
        );
        assert!(
            cli.iter()
                .any(|(_, line)| !line.starts_with("use ") && line.contains(production)),
            "{cli:?}"
        );

        for name in [
            "cg-service",
            "cg-ipc",
            "cg-tools",
            "cg-core",
            "cg-platform",
            "cg-protocol",
            "cg-i18n",
        ] {
            assert_eq!(uploader_uses(name), Vec::new(), "{name}");
        }
    }
}
