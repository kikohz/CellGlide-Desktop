//! The About page, after the Mac's `AboutPage`: the icon and version, the Telegram channel and
//! billing note, the open source licenses, and Quit. The licenses list is generated
//! (`scripts/update-licenses.sh`) and shown a page at a time.

use super::Ctx;
use crate::effect::{AfterMsg, Effect};
use crate::widgets::kit::{self, SEMIBOLD};
use crate::widgets::pager::{self, Pager};
use cg_i18n::tr;
use iced::widget::{button, column, container, image, row, rule, text};
use iced::{Alignment, Element, Length};
use std::time::Duration;

/// Every third-party license: the crates cargo-about finds, then hev-socks5-tunnel and the C
/// code it bundles. The first line is a header the freshness test reads.
pub const LICENSES: &str = include_str!("../../assets/third-party-licenses.txt");

/// `packaging/icons/128.png` as raw RGBA (`scripts/gen-about-icon.py`): iced is built without
/// image codecs, so it can't decode the PNG itself.
const ICON_RGBA: &[u8] = include_bytes!("../../assets/about-icon.rgba");
const ICON_SIDE: u32 = 128;

const TELEGRAM_URL: &str = "https://t.me/cellglide";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The list as shown: without the header line.
fn licenses_body() -> &'static str {
    LICENSES.split_once('\n').map_or(LICENSES, |(_, rest)| rest)
}

#[derive(Debug, Default)]
pub struct State {
    /// The licenses list, while it is open.
    licenses: Option<Pager>,
    copied: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Telegram,
    CopyVersion,
    /// "Copied" turns back into "Copy".
    CopiedEnd,
    ShowLicenses,
    HideLicenses,
    PrevPage,
    NextPage,
    Quit,
}

pub fn update(state: &mut State, message: Msg, _ctx: &Ctx) -> Effect {
    match message {
        Msg::Telegram => Effect::OpenUrl(TELEGRAM_URL.into()),
        Msg::CopyVersion => {
            state.copied = true;
            Effect::Batch(vec![
                Effect::Copy(VERSION.into()),
                Effect::After(Duration::from_secs(2), AfterMsg::AboutCopied),
            ])
        }
        Msg::CopiedEnd => {
            state.copied = false;
            Effect::None
        }
        Msg::ShowLicenses => {
            state.licenses = Some(Pager::new(licenses_body().to_string()));
            Effect::None
        }
        Msg::HideLicenses => {
            state.licenses = None;
            Effect::None
        }
        Msg::PrevPage => {
            if let Some(pager) = &mut state.licenses {
                pager.prev();
            }
            Effect::None
        }
        Msg::NextPage => {
            if let Some(pager) = &mut state.licenses {
                pager.next();
            }
            Effect::None
        }
        Msg::Quit => Effect::Quit,
    }
}

fn header<'a>(state: &State) -> Element<'a, Msg> {
    let icon = image(image::Handle::from_rgba(ICON_SIDE, ICON_SIDE, ICON_RGBA))
        .width(88)
        .height(88);
    let copy = button(kit::secondary(tr(
        if state.copied {
            "common.copied"
        } else {
            "common.copy"
        },
        &[],
    )))
    .padding([2, 8])
    .on_press(Msg::CopyVersion);
    let version = row![
        kit::secondary(tr("about.version", &[("version", VERSION)])).size(13),
        copy
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    column![icon, text("CellGlide").size(20).font(SEMIBOLD), version,]
        .spacing(6)
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into()
}

fn channel<'a>() -> Element<'a, Msg> {
    let link = button(text("t.me/cellglide").size(13)).on_press(Msg::Telegram);
    kit::card(
        None,
        column![
            kit::card_row(tr("about.telegram", &[]), None, link),
            rule::horizontal(1),
            kit::secondary(tr("about.billing", &[])).width(Length::Fill),
        ]
        .spacing(10),
    )
}

fn licenses<'a>(state: &'a State) -> Element<'a, Msg> {
    match &state.licenses {
        None => kit::card(
            None,
            kit::card_row(
                tr("about.licenses", &[]),
                Some(tr("about.licenses_note", &[])),
                button(text(tr("about.show", &[])).size(13)).on_press(Msg::ShowLicenses),
            ),
        ),
        Some(open) => kit::card(
            Some(tr("about.licenses", &[])),
            column![
                pager::view(open, Msg::PrevPage, Msg::NextPage),
                button(text(tr("common.close", &[])).size(12)).on_press(Msg::HideLicenses),
            ]
            .spacing(10),
        ),
    }
}

pub fn view<'a>(state: &'a State, _ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let quit = container(button(text(tr("about.quit", &[])).size(13)).on_press(Msg::Quit))
        .center_x(Length::Fill);
    kit::page(
        tr("nav.about", &[]),
        column![header(state), channel(), licenses(state), quit].spacing(14),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures::{connected_snapshot, disconnected_snapshot};

    fn ctx() -> Ctx<'static> {
        Ctx::for_test(None)
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn licenses_name_the_c_engine_and_the_big_crates() {
        for name in [
            "hev-socks5-tunnel",
            "lwIP",
            "iced",
            "rustls",
            "tokio",
            "ksni",
        ] {
            assert!(LICENSES.contains(name), "{name}");
        }
    }

    #[test]
    fn licenses_are_not_older_than_cargo_lock() {
        let lock = include_str!("../../../../Cargo.lock");
        let header = LICENSES.lines().next().unwrap();
        let expected = format!(
            "# Generated by scripts/update-licenses.sh; Cargo.lock fnv1a64 {:016x}",
            fnv1a64(lock.as_bytes())
        );
        assert_eq!(
            header, expected,
            "Cargo.lock changed: run scripts/update-licenses.sh \
             (needs: cargo install --locked cargo-about --features cli)"
        );
    }

    #[test]
    fn the_shown_list_leaves_out_the_header() {
        assert!(!licenses_body().starts_with('#'));
        assert!(licenses_body().starts_with("CellGlide third-party licenses"));
    }

    #[test]
    fn the_icon_is_128_square_rgba() {
        assert_eq!(ICON_RGBA.len(), (ICON_SIDE * ICON_SIDE * 4) as usize);
        // The PNG it came from says the same size (IHDR: width, height at bytes 16..24).
        let png = include_bytes!("../../../../packaging/icons/128.png");
        assert_eq!(png[16..20], ICON_SIDE.to_be_bytes());
        assert_eq!(png[20..24], ICON_SIDE.to_be_bytes());
        // The blob is the one made from this very PNG (scripts/gen-about-icon.py records both).
        let sums = include_str!("../../assets/about-icon.rgba.fnv1a64");
        let expected = format!(
            "png {:016x}\nrgba {:016x}\n",
            fnv1a64(png),
            fnv1a64(ICON_RGBA)
        );
        assert_eq!(sums, expected, "run scripts/gen-about-icon.py");
        // A real picture, not a blank buffer.
        assert!(ICON_RGBA.chunks(4).any(|pixel| pixel[3] == 255));
    }

    #[test]
    fn telegram_opens_the_channel() {
        assert_eq!(
            update(&mut State::default(), Msg::Telegram, &ctx()),
            Effect::OpenUrl("https://t.me/cellglide".into())
        );
    }

    #[test]
    fn quit_asks_the_app_to_quit() {
        assert_eq!(
            update(&mut State::default(), Msg::Quit, &ctx()),
            Effect::Quit
        );
    }

    #[test]
    fn copying_the_version_shows_copied_until_the_timer() {
        let mut state = State::default();
        let effect = update(&mut state, Msg::CopyVersion, &ctx());
        assert_eq!(
            effect,
            Effect::Batch(vec![
                Effect::Copy(env!("CARGO_PKG_VERSION").into()),
                Effect::After(Duration::from_secs(2), AfterMsg::AboutCopied),
            ])
        );
        assert!(state.copied);
        assert_eq!(update(&mut state, Msg::CopiedEnd, &ctx()), Effect::None);
        assert!(!state.copied);
    }

    #[test]
    fn the_list_opens_pages_and_closes() {
        let mut state = State::default();
        // Paging with no list open does nothing.
        assert_eq!(update(&mut state, Msg::NextPage, &ctx()), Effect::None);
        assert!(state.licenses.is_none());

        update(&mut state, Msg::ShowLicenses, &ctx());
        let open = state.licenses.as_ref().unwrap();
        assert!(open.page_count() > 1, "the list is long enough to page");
        assert_eq!(open.index, 0);
        update(&mut state, Msg::NextPage, &ctx());
        assert_eq!(state.licenses.as_ref().unwrap().index, 1);
        update(&mut state, Msg::PrevPage, &ctx());
        update(&mut state, Msg::PrevPage, &ctx());
        assert_eq!(state.licenses.as_ref().unwrap().index, 0);
        update(&mut state, Msg::HideLicenses, &ctx());
        assert!(state.licenses.is_none());
    }

    #[test]
    fn the_page_draws_in_both_languages_with_or_without_the_list() {
        let _lock = cg_i18n::lock_for_tests();
        let before = cg_i18n::current();
        for lang in [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En] {
            cg_i18n::set(lang);
            for snapshot in [
                None,
                Some(connected_snapshot()),
                Some(disconnected_snapshot()),
            ] {
                let ctx = Ctx::for_test(snapshot);
                let mut state = State::default();
                let _ = view(&state, &ctx);
                update(&mut state, Msg::ShowLicenses, &ctx);
                let _ = view(&state, &ctx);
            }
        }
        assert_eq!(
            cg_i18n::tr("about.version", &[("version", "1")]),
            "Version 1"
        );
        cg_i18n::set(before);
    }
}
