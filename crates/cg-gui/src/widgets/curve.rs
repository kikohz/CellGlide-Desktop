//! The speed-test curve, scaled to its own peak (the Mac's `SpeedCurve`).

use super::kit::Tone;
use iced::widget::canvas::{self, Canvas, Path, Stroke};
use iced::{Element, Length, Point, Rectangle, Renderer, Theme, mouse};

/// Where the curve goes, as (x, height) pairs from 0 to 1 across the series, scaled to this
/// series' peak (at least 1 so an all-zero series stays flat). Fewer than two points is no curve.
pub fn points(series: &[f64]) -> Vec<(f32, f32)> {
    if series.len() < 2 {
        return Vec::new();
    }
    let peak = series.iter().copied().fold(1.0_f64, f64::max);
    let last = (series.len() - 1) as f32;
    series
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f32 / last, (v / peak).clamp(0.0, 1.0) as f32))
        .collect()
}

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
        let line = points(&self.series);
        if !line.is_empty() {
            let path = Path::new(|b| {
                for (i, (x, h)) in line.iter().enumerate() {
                    let p = Point::new(bounds.width * x, bounds.height * (1.0 - h));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_to_the_peak_and_spreads_over_the_width() {
        let p = points(&[0.0, 50.0, 100.0]);
        assert_eq!(p, vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
    }

    #[test]
    fn fewer_than_two_points_draws_nothing() {
        assert!(points(&[]).is_empty());
        assert!(points(&[5.0]).is_empty());
    }

    #[test]
    fn an_all_zero_series_stays_flat() {
        assert!(points(&[0.0, 0.0, 0.0]).iter().all(|(_, h)| *h == 0.0));
    }
}
