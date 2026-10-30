//! The last minute of download (blue) and upload (purple) on a canvas, after the Mac's
//! `SpeedTrail`. A second without a reading is a break in the line, never a drop to zero.

use super::kit::{Tone, ToneColor};
use cg_ipc::describe::ui::{TrailPoint, trail_runs};
use cg_ipc::phone::{SpeedPoint, TRAIL_LENGTH};
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

/// The speed trail as a canvas program.
pub struct Trail {
    pub points: Vec<Option<SpeedPoint>>,
    /// 1.0 normally, 0.5 while the card is dimmed.
    pub alpha: f32,
}

/// The download and upload colours at the given strength.
pub fn colors(alpha: f32) -> (Color, Color) {
    (
        Tone::Blue.color().scale_alpha(alpha),
        Tone::Purple.color().scale_alpha(alpha),
    )
}

/// Room kept above and below the line so the top and bottom points aren't clipped.
const INSET: f32 = 1.0;

/// The y of a point `relative` (0–1) high, in a canvas `height` tall.
pub fn y_at(height: f32, relative: f32) -> f32 {
    INSET + (height - 2.0 * INSET).max(0.0) * (1.0 - relative)
}

impl<M> canvas::Program<M> for Trail {
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
        let (width, height) = (bounds.width, bounds.height);
        let at = |x: f32, relative: f32| Point::new(width * x, y_at(height, relative));
        let (blue, purple) = colors(self.alpha);
        for run in trail_runs(&self.points, TRAIL_LENGTH) {
            for (color, value) in [
                (blue, (|t: &TrailPoint| t.down) as fn(&TrailPoint) -> f32),
                (purple, |t: &TrailPoint| t.up),
            ] {
                if let [only] = run.as_slice() {
                    // One reading between two gaps is a dot, 2 px across.
                    frame.fill(&Path::circle(at(only.x, value(only)), 1.0), color);
                } else {
                    let path = Path::new(|b| {
                        for (i, t) in run.iter().enumerate() {
                            if i == 0 {
                                b.move_to(at(t.x, value(t)));
                            } else {
                                b.line_to(at(t.x, value(t)));
                            }
                        }
                    });
                    frame.stroke(&path, Stroke::default().with_color(color).with_width(1.5));
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

/// A trail canvas of the given size.
pub fn view<'a, M: 'a>(
    points: &[Option<SpeedPoint>],
    width: f32,
    height: f32,
    alpha: f32,
) -> Element<'a, M> {
    Canvas::new(Trail {
        points: points.to_vec(),
        alpha,
    })
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dimmed_trail_is_fainter_than_a_fresh_one() {
        let (fresh_down, fresh_up) = colors(1.0);
        let (dim_down, dim_up) = colors(0.5);
        assert_eq!((fresh_down.a, fresh_up.a), (1.0, 1.0));
        assert_eq!((dim_down.a, dim_up.a), (0.5, 0.5));
        assert_eq!((dim_down.r, dim_up.b), (fresh_down.r, fresh_up.b));
    }

    #[test]
    fn the_line_stays_inside_the_canvas() {
        assert_eq!(y_at(40.0, 1.0), 1.0);
        assert_eq!(y_at(40.0, 0.0), 39.0);
        assert_eq!(y_at(1.0, 0.5), 1.0);
    }
}
