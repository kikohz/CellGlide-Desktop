//! The speed-test curve, scaled to its own peak (the Mac's `SpeedCurve`); the points come from
//! `cg_ipc::describe::ui::curve_points`.

use super::kit::{Tone, ToneColor};
use cg_ipc::describe::ui::curve_points;
use iced::widget::canvas::{self, Canvas, Path, Stroke};
use iced::{Element, Length, Point, Rectangle, Renderer, Theme, mouse};

pub struct Curve {
    pub series: Vec<f64>,
    pub tone: Tone,
}

impl<M> canvas::Program<M> for Curve {
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
        let line = curve_points(&self.series);
        if !line.is_empty() {
            let path = Path::new(|b| {
                for (i, point) in line.iter().enumerate() {
                    let p = Point::new(bounds.width * point.x, bounds.height * (1.0 - point.y));
                    if i == 0 {
                        b.move_to(p);
                    } else {
                        b.line_to(p);
                    }
                }
            });
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(self.tone.color())
                    .with_width(1.5),
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The curve across the available width.
pub fn view<'a, M: 'a>(curve: Curve, height: f32) -> Element<'a, M> {
    Canvas::new(curve)
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}
