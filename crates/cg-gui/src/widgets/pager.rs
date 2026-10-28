//! Long read-only text in pages, so a 1 MB diagnostics bundle never lays out at once.

use super::kit::secondary;
use cg_i18n::tr;
use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Font};
use std::ops::Range;

/// Pages cut at line ends: at most `max_lines` lines and `max_bytes` bytes each. A line longer
/// than `max_bytes` gets a page of its own, whole. The pages joined are exactly `text`.
pub fn pages(text: &str, max_lines: usize, max_bytes: usize) -> Vec<Range<usize>> {
    let max_lines = max_lines.max(1);
    let mut out = Vec::new();
    let (mut start, mut end, mut lines) = (0, 0, 0);
    for line in text.split_inclusive('\n') {
        if lines > 0 && (lines == max_lines || end - start + line.len() > max_bytes) {
            out.push(start..end);
            start = end;
            lines = 0;
        }
        end += line.len();
        lines += 1;
    }
    if lines > 0 {
        out.push(start..end);
    }
    out
}

/// A read-only text split into pages, with the page on show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pager {
    pub text: String,
    pub pages: Vec<Range<usize>>,
    pub index: usize,
}

impl Pager {
    /// 200 lines or 32 KiB a page.
    pub fn new(text: String) -> Pager {
        let pages = pages(&text, 200, 32 * 1024);
        Pager {
            text,
            pages,
            index: 0,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    pub fn has_prev(&self) -> bool {
        self.index > 0
    }

    pub fn has_next(&self) -> bool {
        self.index + 1 < self.pages.len()
    }

    pub fn next(&mut self) {
        if self.has_next() {
            self.index += 1;
        }
    }

    pub fn prev(&mut self) {
        self.index = self.index.saturating_sub(1);
    }

    /// The text of the page on show; empty for an empty text.
    pub fn current(&self) -> &str {
        self.pages
            .get(self.index)
            .map_or("", |range| &self.text[range.clone()])
    }
}

/// The page, in a monospace font, with the page number and Previous / Next buttons.
pub fn view<'a, M: Clone + 'a>(pager: &'a Pager, on_prev: M, on_next: M) -> Element<'a, M> {
    let number = tr(
        "common.page",
        &[
            ("page", &(pager.index + 1).to_string()),
            ("pages", &pager.page_count().to_string()),
        ],
    );
    let controls = row![
        button(text(tr("common.prev", &[])).size(12))
            .on_press_maybe(pager.has_prev().then_some(on_prev)),
        secondary(number),
        button(text(tr("common.next", &[])).size(12))
            .on_press_maybe(pager.has_next().then_some(on_next)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    column![
        text(pager.current()).font(Font::MONOSPACE).size(12),
        controls
    ]
    .spacing(8)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_the_pages_and_stops_at_the_ends() {
        let text: String = (0..450).map(|n| format!("{n}\n")).collect();
        let mut pager = Pager::new(text.clone());
        assert_eq!(pager.page_count(), 3);
        assert!(!pager.has_prev() && pager.has_next());
        pager.prev();
        assert_eq!(pager.index, 0);
        assert!(pager.current().starts_with("0\n"));
        pager.next();
        pager.next();
        pager.next();
        assert_eq!(pager.index, 2);
        assert!(pager.current().ends_with("449\n"));
        assert!(Pager::new(String::new()).current().is_empty());
        assert_eq!(Pager::new(String::new()).page_count(), 1);
    }

    fn big_text() -> String {
        let mut text = String::new();
        let mut n = 0;
        while text.len() < 1_000_000 {
            match n % 4 {
                0 => text.push_str(&format!("line {n} plain ascii\n")),
                1 => text.push_str(&format!("第 {n} 行：中文内容，多字节字符\n")),
                2 => text.push_str(&format!("{}\n", "x".repeat(2000))),
                _ => text.push('\n'),
            }
            n += 1;
        }
        text.push_str("last line without a newline");
        text
    }

    #[test]
    fn pages_cover_the_text_exactly_on_line_boundaries() {
        let text = big_text();
        let ranges = pages(&text, 200, 32 * 1024);
        assert!(ranges.len() > 20);
        let joined: String = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(joined, text);
        for (i, r) in ranges.iter().enumerate() {
            let page = &text[r.clone()];
            if i + 1 < ranges.len() {
                assert!(page.ends_with('\n'), "page {i} ends mid-line");
            }
            let lines = page.split_inclusive('\n').count();
            assert!(lines <= 200, "page {i} has {lines} lines");
            assert!(lines == 1 || page.len() <= 32 * 1024, "page {i} too big");
        }
    }

    #[test]
    fn a_line_longer_than_a_page_stays_whole() {
        let long = "y".repeat(40 * 1024);
        let text = format!("a\nb\n{long}\nc\n");
        let ranges = pages(&text, 200, 32 * 1024);
        assert_eq!(ranges.len(), 3);
        assert_eq!(&text[ranges[0].clone()], "a\nb\n");
        assert_eq!(&text[ranges[1].clone()], format!("{long}\n"));
        assert_eq!(&text[ranges[2].clone()], "c\n");
    }

    #[test]
    fn the_limits_are_inclusive() {
        let lines = |n: usize| "a\n".repeat(n);
        assert_eq!(pages(&lines(200), 200, 1 << 20).len(), 1);
        assert_eq!(pages(&lines(201), 200, 1 << 20).len(), 2);
        // 32768 bytes on the first line, then a second line: 32768 fits, one more doesn't.
        let exact = format!("{}\n", "x".repeat(32 * 1024 - 1));
        assert_eq!(pages(&exact, 200, 32 * 1024).len(), 1);
        let two_exact = format!("{exact}b\n");
        assert_eq!(pages(&two_exact, 200, 32 * 1024).len(), 2);
        // Two lines totalling exactly 32768 share a page; 32769 splits them.
        let fits = format!(
            "{}\n{}\n",
            "x".repeat(16 * 1024 - 1),
            "y".repeat(16 * 1024 - 1)
        );
        assert_eq!(fits.len(), 32 * 1024);
        assert_eq!(pages(&fits, 200, 32 * 1024).len(), 1);
        let over = format!("{}\n{}\n", "x".repeat(16 * 1024 - 1), "y".repeat(16 * 1024));
        assert_eq!(over.len(), 32 * 1024 + 1);
        assert_eq!(pages(&over, 200, 32 * 1024).len(), 2);
    }
}
