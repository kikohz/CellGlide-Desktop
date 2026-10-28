//! The 24-hour strip of a day: grey ground, green where the phone was shared, orange or red
//! where a leg was broken. After the Mac's `DayStrip`.

use super::kit::Tone;
use cg_ipc::health::{Span, SpanKind, strip_fraction};
use iced::widget::canvas::{self, Canvas};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};
use jiff::Timestamp;

/// One painted rectangle of the strip, in pixels from the left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub x: f32,
    pub width: f32,
    pub tone: Tone,
    pub alpha: f32,
}

/// The cells to paint, in order. `day_spans` already puts sharing before issues, so the issues
/// land on top. Every cell is at least 1 px wide and stays inside `[0, width]`.
pub fn cells(spans: &[Span], begin: Timestamp, next: Timestamp, width: f32) -> Vec<Cell> {
    spans
        .iter()
        .map(|span| {
            let from = strip_fraction(span.from, begin, next) as f32 * width;
            let to = strip_fraction(span.to, begin, next) as f32 * width;
            let cell_width = (to - from).max(1.0).min(width.max(1.0));
            let (tone, alpha) = match span.kind {
                SpanKind::Sharing => (Tone::Green, 0.6),
                SpanKind::Issue(cause) if cause.is_severe() => (Tone::Red, 1.0),
                SpanKind::Issue(_) => (Tone::Orange, 1.0),
            };
            Cell {
                x: from.min((width - cell_width).max(0.0)),
                width: cell_width,
                tone,
                alpha,
            }
        })
        .collect()
}

/// A day's spans between its first instant and the next day's.
pub struct DayStrip {
    pub spans: Vec<Span>,
    pub begin: Timestamp,
    pub next: Timestamp,
}

impl<M> canvas::Program<M> for DayStrip {
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
        for cell in cells(&self.spans, self.begin, self.next, bounds.width) {
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
    Canvas::new(strip)
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::health::IssueCause;

    fn at(hours: i64) -> Timestamp {
        Timestamp::from_second(hours * 3600).unwrap()
    }

    fn span(from: i64, to: i64, kind: SpanKind) -> Span {
        Span {
            from: at(from),
            to: at(to),
            kind,
        }
    }

    #[test]
    fn cells_follow_the_day_and_colour_by_severity() {
        let spans = [
            span(0, 12, SpanKind::Sharing),
            span(6, 9, SpanKind::Issue(IssueCause::Channel)),
            span(9, 12, SpanKind::Issue(IssueCause::PhoneUnstable)),
        ];
        let c = cells(&spans, at(0), at(24), 240.0);
        assert_eq!((c[0].x, c[0].width, c[0].tone), (0.0, 120.0, Tone::Green));
        assert!((c[0].alpha - 0.6).abs() < 1e-6);
        assert_eq!((c[1].x, c[1].width, c[1].tone), (60.0, 30.0, Tone::Red));
        assert_eq!((c[2].x, c[2].tone), (90.0, Tone::Orange));
    }

    #[test]
    fn a_cell_is_at_least_one_pixel_and_inside_the_strip() {
        let spans = [
            span(5, 5, SpanKind::Issue(IssueCause::Channel)),
            span(24, 24, SpanKind::Issue(IssueCause::Channel)),
        ];
        let c = cells(&spans, at(0), at(24), 240.0);
        assert_eq!(c[0].width, 1.0);
        assert_eq!((c[1].x, c[1].width), (239.0, 1.0));
    }

    #[test]
    fn a_dst_day_still_spans_the_whole_strip() {
        // A 23-hour day: the last instant is the right edge.
        let spans = [span(0, 23, SpanKind::Sharing)];
        let c = cells(&spans, at(0), at(23), 100.0);
        assert_eq!((c[0].x, c[0].width), (0.0, 100.0));
    }
}
