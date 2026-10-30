//! The 24-hour strip of a day: grey ground, green where the phone was shared, orange or red
//! where a leg was broken. After the Mac's `DayStrip`; the cells come from
//! `cg_ipc::describe::ui::strip_cells`.

use super::kit::{Tone, ToneColor};
use cg_ipc::describe::ui::{DayStrip, strip_cells};
use iced::widget::canvas::{self, Canvas};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

/// A day's strip as a canvas program.
struct Painter(DayStrip);

impl<M> canvas::Program<M> for Painter {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Tone::Gray.color().scale_alpha(0.2),
        );
        let strip = &self.0;
        for cell in strip_cells(&strip.spans, strip.begin, strip.next, bounds.width) {
            frame.fill_rectangle(
                Point::new(cell.x, 0.0),
                Size::new(cell.width, bounds.height),
                Color {
                    a: cell.alpha,
                    ..cell.tone.color()
                },
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The strip across the available width.
pub fn view<'a, M: 'a>(strip: DayStrip, height: f32) -> Element<'a, M> {
    Canvas::new(Painter(strip))
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}
