//! The main window's first page, after the Mac's `OverviewPage`: the state and the connect
//! switch, the phone, and the connection (its health and the USB device in use).

use crate::app::{Message, button_label};
use crate::panel;
use crate::section::Section;
use crate::tray::ServiceDown;
use crate::widgets::kit::{self, SEMIBOLD, Tone};
use crate::widgets::phone_card;
use cg_i18n::tr;
use cg_ipc::{Snapshot, describe};
use iced::Element;
use iced::widget::{button, column, rule, text};
use jiff::Timestamp;

/// What the page reads from the app.
pub struct View<'a> {
    pub snapshot: Option<&'a Snapshot>,
    pub service: Option<&'a ServiceDown>,
    pub command_error: Option<&'a str>,
    /// The last connect / disconnect press is still being carried out.
    pub command_pending: bool,
    pub now: Timestamp,
}

/// The USB device row's subtitle: the chosen phone, or that any phone will do.
pub fn device_subtitle(snapshot: Option<&Snapshot>) -> String {
    match snapshot.and_then(|s| s.selected.as_deref().map(|serial| (s, serial))) {
        Some((snapshot, serial)) => describe::device_label(snapshot, serial),
        None => tr("overview.auto_device", &[]),
    }
}

pub fn view<'a>(page: View<'a>) -> Element<'a, Message> {
    let mut state = column![].spacing(8);
    match (page.snapshot, page.service) {
        (Some(snapshot), _) => {
            let words = describe::gui_state(snapshot);
            state = state
                .push(text(words.title).size(17).font(SEMIBOLD))
                .push(kit::secondary(words.detail).size(13));
        }
        (None, Some(down)) => state = state.push(text(down.text.clone()).size(13)),
        (None, None) => state = state.push(text(tr("gui.waiting_service", &[])).size(13)),
    }
    if let Some(problem) = page.command_error {
        state = state.push(text(problem.to_owned()).size(13).color(Tone::Red.color()));
    }
    state = state.push(
        button(text(button_label(page.snapshot)).size(13))
            .on_press_maybe((!page.command_pending).then_some(Message::Toggle))
            .style(panel::main_button_style(page.snapshot)),
    );

    let mut cards = column![kit::card(None, state)].spacing(14);
    if let Some(card) = page
        .snapshot
        .and_then(|s| s.phone.as_ref())
        .filter(|c| describe::card_visible(c))
    {
        cards = cards.push(kit::card(
            Some(tr("phone.card_title", &[])),
            phone_card::full(card),
        ));
    }

    let mut connection = column![].spacing(10);
    if let Some(snapshot) = page.snapshot
        && let Some(line) = panel::health_line(snapshot, page.now, &jiff::tz::TimeZone::system())
    {
        connection = connection
            .push(panel::health_row(
                panel::health_tone(snapshot),
                line,
                Message::Navigate(Section::History),
            ))
            .push(rule::horizontal(1));
    }
    connection = connection.push(kit::card_row(
        tr("overview.usb_device", &[]),
        Some(device_subtitle(page.snapshot)),
        button(text(tr("overview.change", &[])).size(13))
            .on_press(Message::Navigate(Section::Settings))
            .style(button::secondary),
    ));
    cards = cards.push(kit::card(Some(tr("overview.connection", &[])), connection));

    kit::page(tr("nav.overview", &[]), cards)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn usb_device_names_the_chosen_phone_or_auto_discovery() {
        let _guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let chosen = fixtures::disconnected_snapshot();
        assert_eq!(
            device_subtitle(Some(&chosen)),
            describe::device_label(&chosen, fixtures::SERIAL)
        );
        let mut none = chosen.clone();
        none.selected = None;
        assert_eq!(device_subtitle(Some(&none)), "自动发现 / 等待已选设备");
        assert_eq!(device_subtitle(None), "自动发现 / 等待已选设备");
    }

    #[test]
    fn every_kind_of_state_builds() {
        let connected = fixtures::connected_snapshot();
        let disconnected = fixtures::disconnected_snapshot();
        let down = ServiceDown {
            text: "down".into(),
            not_in_group: false,
        };
        for (snapshot, service, error) in [
            (Some(&connected), None, None),
            (Some(&disconnected), None, Some("x")),
            (None, Some(&down), None),
            (None, None, None),
        ] {
            let _ = view(View {
                snapshot,
                service,
                command_error: error,
                command_pending: error.is_some(),
                now: Timestamp::now(),
            });
        }
    }
}
