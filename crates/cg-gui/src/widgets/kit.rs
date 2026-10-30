//! The page frame, cards, rows and dots, after the Mac's `PageKit.swift`.

use iced::widget::{column, container, row, scrollable, space, text};
use iced::{Alignment, Background, Border, Color, Element, Font, Length, Theme, font};

pub use cg_ipc::describe::ui::Tone;

/// A tone as iced draws it: the Mac's system colours, so dots and badges read the same on
/// both platforms.
pub trait ToneColor {
    fn color(self) -> Color;
}

impl ToneColor for Tone {
    fn color(self) -> Color {
        match self {
            Tone::Green => Color::from_rgb8(0x34, 0xC7, 0x59),
            Tone::Orange => Color::from_rgb8(0xFF, 0x95, 0x00),
            Tone::Red => Color::from_rgb8(0xFF, 0x3B, 0x30),
            Tone::Blue => Color::from_rgb8(0x00, 0x7A, 0xFF),
            Tone::Purple => Color::from_rgb8(0xAF, 0x52, 0xDE),
            Tone::Gray => Color::from_rgb8(0x8E, 0x8E, 0x93),
        }
    }
}

pub const SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..Font::DEFAULT
};

/// The colour of secondary text in this theme.
pub fn secondary_color(theme: &Theme) -> Color {
    theme.palette().text.scale_alpha(0.6)
}

/// Small text in the secondary colour.
pub fn secondary<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(content).size(12).style(|theme: &Theme| text::Style {
        color: Some(secondary_color(theme)),
    })
}

/// A page: its title, then the content centred at most 620 wide, scrolling, padded 24 / 20.
pub fn page<'a, M: 'a>(title: String, content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    let body = column![text(title).size(20).font(SEMIBOLD), content.into()]
        .spacing(18)
        .width(Length::Fill);
    scrollable(
        container(container(body).max_width(620))
            .center_x(Length::Fill)
            .padding([20, 24]),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A card: optional title, 10 px corners, 14 px padding, hairline edge.
pub fn card<'a, M: 'a>(
    title: Option<String>,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    let mut body = column![].spacing(10).width(Length::Fill);
    if let Some(title) = title {
        body = body.push(text(title).size(13).font(SEMIBOLD));
    }
    container(body.push(content.into()))
        .padding(14)
        .width(Length::Fill)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(Background::Color(
                    palette.background.weak.color.scale_alpha(0.35),
                )),
                border: Border {
                    color: palette.background.base.text.scale_alpha(0.1),
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..container::Style::default()
            }
        })
        .into()
}

/// A label (and optional explanation) on the left, a control on the right.
pub fn card_row<'a, M: 'a>(
    title: String,
    subtitle: Option<String>,
    trailing: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    let mut label = column![text(title).size(13)].spacing(2);
    if let Some(subtitle) = subtitle {
        label = label.push(secondary(subtitle).size(11));
    }
    row![label.width(Length::Fill), trailing.into()]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

/// A filled circle.
pub fn dot<'a, M: 'a>(tone: Tone, size: f32) -> Element<'a, M> {
    container(space())
        .width(size)
        .height(size)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(tone.color())),
            border: Border {
                radius: (size / 2.0).into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}
