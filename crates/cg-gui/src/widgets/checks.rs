//! The doctor's checks as a list: a green tick or a red cross with the sentence, and under a
//! failed one the fix. Shared by the diagnostics page and the setup help.

use super::kit::{Tone, ToneColor, secondary};
use crate::effect::{DoctorFor, Effect, Reply};
use cg_i18n::tr;
use cg_ipc::describe::ui::check_rows;
use cg_ipc::{Check, Request};
use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Font, Length};

/// A `doctor` run a page asks for and shows in place. The Diagnostics-style pages (the DNS
/// leak test, Settings) keep one and forward `Msg::Checks` to `finish`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// The last answer: the checks, or why there are none.
    pub checks: Option<Result<Vec<Check>, String>>,
    /// Asked for, not answered yet.
    pub checking: bool,
    /// Numbers the asks: only the reply to the latest ask is taken, so a late answer to an
    /// earlier one (the page started over meanwhile) is dropped.
    pub token: u64,
}

impl Run {
    /// Asks the service to run `doctor` for `page`; a second ask while one is out does nothing.
    pub fn start(&mut self, page: DoctorFor) -> Effect {
        if self.checking {
            return Effect::None;
        }
        self.checking = true;
        self.checks = None;
        self.token += 1;
        Effect::Request(Request::Doctor, Reply::Doctor(page, self.token))
    }

    /// The service answered (or failed to): no longer waiting. A reply to an ask that is not
    /// the latest one is dropped; returns whether it was taken.
    pub fn finish(&mut self, token: u64, result: Result<Vec<Check>, String>) -> bool {
        if !self.checking || token != self.token {
            return false;
        }
        self.checking = false;
        self.checks = Some(result);
        true
    }

    /// Forgets the answer, e.g. when the page starts over; an answer still on its way is
    /// dropped when it arrives.
    pub fn clear(&mut self) {
        self.token += 1;
        self.checking = false;
        self.checks = None;
    }
}

/// The run as the page shows it: nothing before it starts, a progress line while waiting, the
/// reason in red when it failed, else the list.
pub fn run_view<'a, M: Clone + 'a>(
    run: &Run,
    copied: Option<&str>,
    on_copy: impl Fn(String) -> M,
) -> Option<Element<'a, M>> {
    if run.checking {
        return Some(secondary(tr("doctor.running", &[])).into());
    }
    Some(match run.checks.as_ref()? {
        Ok(checks) => view(checks, copied, on_copy),
        Err(reason) => text(reason.clone())
            .size(12)
            .color(Tone::Red.color())
            .into(),
    })
}

/// The list. `on_copy` makes the message for copying a fix: the fix is shown as read-only
/// monospace text (iced cannot select text) with a Copy button beside it.
pub fn view<'a, M: Clone + 'a>(
    checks: &[Check],
    copied: Option<&str>,
    on_copy: impl Fn(String) -> M,
) -> Element<'a, M> {
    let mut list = column![].spacing(8);
    for item in check_rows(checks, copied) {
        let (mark, tone) = if item.ok {
            ("✓", Tone::Green)
        } else {
            ("✗", Tone::Red)
        };
        let mut entry = column![
            row![
                text(mark).size(13).color(tone.color()),
                text(item.line).size(13).width(Length::Fill)
            ]
            .spacing(8)
        ]
        .spacing(4);
        if let Some(fix) = item.fix {
            let shown = tr("check.fix", &[("fix", &fix)]);
            let label = tr(
                if item.copied {
                    "common.copied"
                } else {
                    "common.copy"
                },
                &[],
            );
            entry = entry.push(
                row![
                    text(shown)
                        .font(Font::MONOSPACE)
                        .size(12)
                        .width(Length::Fill),
                    button(secondary(label))
                        .on_press(on_copy(fix))
                        .padding([2, 8]),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        list = list.push(entry);
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_asks_once_and_ends_with_the_answer() {
        let mut run = Run::default();
        assert_eq!(
            run.start(DoctorFor::Leak),
            Effect::Request(Request::Doctor, Reply::Doctor(DoctorFor::Leak, 1))
        );
        assert!(run.checking);
        assert_eq!(run.start(DoctorFor::Leak), Effect::None);
        assert!(run.finish(1, Err("x".into())));
        assert!(!run.checking);
        assert_eq!(run.checks, Some(Err("x".into())));
        // Asking again forgets the old answer.
        let _ = run.start(DoctorFor::Settings);
        assert_eq!(run.checks, None);
        run.clear();
        assert!(!run.checking && run.checks.is_none());
    }

    #[test]
    fn a_stale_answer_is_dropped() {
        let mut run = Run::default();
        let _ = run.start(DoctorFor::Settings);
        // The page starts over (token 2 now), then asks again (token 3).
        run.clear();
        assert!(!run.checking);
        // The first ask's late answer finds nothing waiting.
        assert!(!run.finish(1, Err("late".into())));
        assert_eq!(run.checks, None);
        let _ = run.start(DoctorFor::Settings);
        assert!(!run.finish(1, Err("late".into())));
        assert!(run.checking && run.checks.is_none());
        assert!(!run.finish(2, Err("late".into())));
        assert!(run.finish(3, Ok(vec![])));
        assert_eq!(run.checks, Some(Ok(vec![])));
        // Answered once only.
        assert!(!run.finish(3, Err("again".into())));
        assert_eq!(run.checks, Some(Ok(vec![])));
    }
}
