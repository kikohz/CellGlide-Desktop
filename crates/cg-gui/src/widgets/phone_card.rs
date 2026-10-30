//! The phone at a glance, after the Mac's `PhoneCard`: carrier, line, model and battery; speed
//! and the last minute's trail; quality, battery and heat. Every sentence comes from
//! `cg_ipc::describe::card_text`; this only lays it out. No frame of its own: the overview puts
//! it in a titled card, the panel shows it bare, as on the Mac.

use super::kit::{SEMIBOLD, Tone, ToneColor, secondary_color};
use super::trail;
use cg_i18n::tr;
use cg_ipc::describe::ui::{Speeds, speeds};
use cg_ipc::describe::{self, BatteryBadge};
use cg_ipc::phone::PhoneCard;
use iced::widget::{column, container, row, rule, space, text};
use iced::{Alignment, Background, Border, Color, Element, Font, Length, Theme};

/// The main window's layout: bigger speed, a wider trail, a rule between the blocks.
pub fn full<'a, M: 'a>(card: &'a PhoneCard) -> Element<'a, M> {
    build(card, true)
}

/// The panel's layout.
pub fn compact<'a, M: 'a>(card: &'a PhoneCard) -> Element<'a, M> {
    build(card, false)
}

/// The low-power-mode yellow.
const YELLOW: Color = Color::from_rgb(1.0, 0.8, 0.0);

fn build<'a, M: 'a>(phone: &'a PhoneCard, expanded: bool) -> Element<'a, M> {
    let info = describe::card_text(phone);
    // A card dims as a whole: every colour here is drawn at half strength.
    let alpha = if info.dim { 0.5 } else { 1.0 };
    let plain = move |content: String, size: f32| {
        text(content)
            .size(size)
            .style(move |theme: &Theme| text::Style {
                color: Some(theme.palette().text.scale_alpha(alpha)),
            })
    };
    let muted = move |content: String, size: f32| {
        text(content)
            .size(size)
            .style(move |theme: &Theme| text::Style {
                color: Some(secondary_color(theme).scale_alpha(alpha)),
            })
    };
    let tinted = move |content: String, size: f32, color: Color| {
        text(content).size(size).color(color.scale_alpha(alpha))
    };

    // First line: carrier, radio, then model and battery on the right.
    let mut head = row![plain(info.title.clone(), 15.0).font(SEMIBOLD)]
        .spacing(8)
        .align_y(Alignment::Center);
    if let Some(radio) = &info.radio {
        head = head.push(radio_tag(radio.clone(), alpha));
    }
    head = head.push(space::horizontal());
    if let Some(model) = &info.model {
        head = head.push(muted(model.clone(), 13.0));
    }
    if let Some(battery) = &info.battery {
        let label = battery_label(battery);
        head = head.push(if battery.low_power {
            tinted(label, 13.0, YELLOW)
        } else {
            muted(label, 13.0)
        });
    }

    let mut body = column![head].spacing(8).width(Length::Fill);
    let divider = |body: iced::widget::Column<'a, M>| {
        if expanded {
            body.push(rule::horizontal(1))
        } else {
            body
        }
    };

    match speeds(phone, &info) {
        Speeds::Hidden => {}
        Speeds::NotShared => body = body.push(muted(tr("phone.not_sharing", &[]), 13.0)),
        Speeds::Shared => {
            let size = if expanded { 20.0 } else { 13.0 };
            let (width, height) = if expanded {
                (200.0, 40.0)
            } else {
                (120.0, 24.0)
            };
            let speed = |arrow: &str, value: &Option<String>, tone: Tone| {
                // A reading the phone didn't send leaves its arrow out.
                value.as_ref().map(|shown| {
                    let line = text(format!("{arrow} {shown}")).size(size);
                    Element::from(
                        if expanded { line.font(SEMIBOLD) } else { line }
                            .color(tone.color().scale_alpha(alpha)),
                    )
                })
            };
            let mut line = row![].spacing(14).align_y(Alignment::Center);
            line = line.extend(speed("↓", &info.download, Tone::Blue));
            line = line.extend(speed("↑", &info.upload, Tone::Purple));
            body = body.push(line.push(space::horizontal()).push(trail::view(
                &phone.trail,
                width,
                height,
                alpha,
            )));
        }
    }
    if let Some(quality) = &info.quality {
        body = divider(body).push(muted(quality.clone(), 13.0));
    }
    if let Some(line) = &info.battery_line {
        body = divider(body).push(muted(line.clone(), 13.0));
    }
    if let Some(heat) = &info.heat {
        let tone = if heat.severe { Tone::Red } else { Tone::Orange };
        body = body.push(tinted(heat.text.clone(), 13.0, tone.color()));
    }
    body.into()
}

/// "87%", "87% (charging)".
fn battery_label(battery: &BatteryBadge) -> String {
    let key = if battery.charging {
        "phone.battery_level_charging"
    } else {
        "phone.battery_level"
    };
    tr(key, &[("percent", &battery.percent.to_string())])
}

/// The radio ("5G") in a capsule tinted with the accent colour.
fn radio_tag<'a, M: 'a>(radio: String, alpha: f32) -> Element<'a, M> {
    container(
        text(radio)
            .size(11)
            .font(Font {
                weight: iced::font::Weight::Medium,
                ..Font::DEFAULT
            })
            .style(move |theme: &Theme| text::Style {
                color: Some(theme.palette().primary.scale_alpha(alpha)),
            }),
    )
    .padding([1, 6])
    .style(move |theme: &Theme| container::Style {
        background: Some(Background::Color(
            theme.palette().primary.scale_alpha(0.15 * alpha),
        )),
        border: Border {
            radius: 20.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::phone::{PhoneFrame, SpeedPoint};

    fn frame(sharing: Option<bool>) -> PhoneCard {
        PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("China Mobile".into()),
                radio: Some("5G".into()),
                sharing,
                download: Some(2_500_000.0),
                upload: Some(400_000.0),
                ..PhoneFrame::default()
            }),
            trail: vec![
                Some(SpeedPoint {
                    download: 1.0,
                    upload: 1.0,
                }),
                None,
            ],
            ..PhoneCard::default()
        }
    }

    #[test]
    fn both_layouts_build_for_every_kind_of_card() {
        let _lock = cg_i18n::lock_for_tests();
        let mut stale = frame(Some(true));
        stale.stale = true;
        for card in [
            frame(Some(true)),
            frame(Some(false)),
            stale,
            PhoneCard::default(),
        ] {
            let _full: Element<'_, ()> = full(&card);
            let _compact: Element<'_, ()> = compact(&card);
        }
    }
}
