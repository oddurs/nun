//! The completion popup, and the documentation beside it.
//!
//! The binary decides what is offered and what picking it does; this draws
//! the list and says where it goes, so the layout pass and the drawing agree
//! on every row. The popup opens under the word being completed and flips
//! above it when there is more room there than below.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::palette::{Entry, write, write_matched};
use crate::style::Palette;

/// One suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// A few letters saying what it is: `fn`, `var`, `mod`.
    pub kind: &'static str,
    /// The colour of those letters.
    pub kind_role: Role,
    /// What it says.
    pub label: String,
    /// Char offsets of `label` that matched what was typed.
    pub matched: Vec<u32>,
    /// Its type or signature, dimmed at the right.
    pub detail: String,
    /// Whether the server marked it deprecated; it is struck through.
    pub deprecated: bool,
}

/// Rows of suggestions, at most.
pub const MOST_SUGGESTIONS: u16 = 10;

/// Columns the kind takes, with the space after it.
const KIND: u16 = 5;

/// The popup, drawn.
#[derive(Debug)]
pub struct CompletionView<'a> {
    palette: &'a Palette,
    suggestions: &'a [Suggestion],
    selected: usize,
    scroll: usize,
    hovered: Option<usize>,
}

impl<'a> CompletionView<'a> {
    /// A popup offering `suggestions`.
    #[must_use]
    pub const fn new(palette: &'a Palette, suggestions: &'a [Suggestion]) -> Self {
        Self { palette, suggestions, selected: 0, scroll: 0, hovered: None }
    }

    /// Which row the keyboard is on.
    #[must_use]
    pub const fn selected(mut self, row: usize) -> Self {
        self.selected = row;
        self
    }

    /// The first row shown.
    #[must_use]
    pub const fn scrolled_to(mut self, row: usize) -> Self {
        self.scroll = row;
        self
    }

    /// The row under the pointer.
    #[must_use]
    pub const fn hovered(mut self, row: Option<usize>) -> Self {
        self.hovered = row;
        self
    }

    /// How wide the popup wants to be for `suggestions`.
    #[must_use]
    pub fn width(suggestions: &[Suggestion]) -> u16 {
        let widest = suggestions
            .iter()
            .take(200)
            .map(|suggestion| {
                let detail = suggestion.detail.width();
                suggestion.label.width() + if detail > 0 { detail + 2 } else { 0 }
            })
            .max()
            .unwrap_or(0);
        u16::try_from(widest).unwrap_or(u16::MAX).saturating_add(KIND + 2).clamp(24, 64)
    }

    /// Where a popup of `rows` rows, `width` wide, goes for a word whose
    /// first cell is `(x, y)`, within `bounds`.
    ///
    /// Below the word when it fits; otherwise on whichever side has more
    /// room, which is above it near the bottom of the screen. Never off the
    /// right edge, and never over the word's own row.
    #[must_use]
    pub fn area(bounds: Rect, x: u16, y: u16, rows: usize, width: u16) -> Rect {
        let wanted = u16::try_from(rows).unwrap_or(MOST_SUGGESTIONS).min(MOST_SUGGESTIONS);
        let below = bounds.bottom().saturating_sub(y.saturating_add(1));
        let above = y.saturating_sub(bounds.y);
        let (top, height) = if below >= wanted || below >= above {
            (y.saturating_add(1), wanted.min(below))
        } else {
            let height = wanted.min(above);
            (y - height, height)
        };
        let width = width.min(bounds.width);
        // One column left of the word, so the labels line up under it past
        // the kind.
        let left = x.saturating_sub(KIND + 1).max(bounds.x);
        let left = left.min(bounds.right().saturating_sub(width));
        Rect::new(left, top, width, height)
    }

    /// The suggestion drawn at screen row `y` of a popup at `area`.
    #[must_use]
    pub fn row_at(area: Rect, scroll: usize, y: u16, rows: usize) -> Option<usize> {
        if y < area.y || y >= area.bottom() {
            return None;
        }
        let index = scroll + usize::from(y - area.y);
        (index < rows).then_some(index)
    }

    /// The scroll that keeps `selected` in view.
    #[must_use]
    pub const fn scroll_to(area: Rect, selected: usize, scroll: usize) -> usize {
        let visible = area.height as usize;
        if selected < scroll {
            return selected;
        }
        if visible > 0 && selected >= scroll + visible {
            return selected + 1 - visible;
        }
        scroll
    }
}

impl Widget for CompletionView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        let area = area.intersection(*cells.area());
        if area.width < KIND + 4 || area.height == 0 {
            return;
        }
        let ground = self.palette.on(Role::Overlay, Role::Text);
        let rows = (self.scroll..self.suggestions.len()).take(usize::from(area.height));
        for (offset, index) in rows.enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let y = area.y + offset;
            let suggestion = &self.suggestions[index];
            let selected = index == self.selected;

            let row = if selected {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else if self.hovered == Some(index) {
                self.palette.on(Role::Raised, Role::Text)
            } else {
                ground
            };
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(row);
            }

            let kind =
                if selected { row } else { self.palette.on(Role::Overlay, suggestion.kind_role) };
            write(cells, area.x + 1, y, KIND - 1, suggestion.kind, kind);

            let detail_width = u16::try_from(suggestion.detail.width()).unwrap_or(u16::MAX);
            let label_room = area.width.saturating_sub(KIND + 2);
            let label_width = u16::try_from(suggestion.label.width()).unwrap_or(u16::MAX);
            // The detail gets what the label leaves, and is dropped before the
            // label is cut.
            let detail_room = label_room.saturating_sub(label_width.saturating_add(2));
            let label_style =
                if suggestion.deprecated { row.add_modifier(Modifier::CROSSED_OUT) } else { row };
            let entry = Entry {
                label: suggestion.label.clone(),
                matched: suggestion.matched.clone(),
                hint: String::new(),
            };
            write_matched(
                cells,
                area.x + 1 + KIND,
                y,
                label_room,
                &entry,
                label_style,
                self.palette.on(Role::Overlay, Role::Accent),
                selected,
            );
            if detail_room >= 4 {
                let width = detail_width.min(detail_room);
                let detail = if selected { row } else { self.palette.on(Role::Overlay, Role::Dim) };
                write(cells, area.right() - 1 - width, y, width, &suggestion.detail, detail);
            }
        }
    }
}

/// Lines of documentation, at most.
pub const MOST_DOC_LINES: u16 = 14;

/// Narrowest the documentation is shown at; narrower, it is left out.
const NARROWEST_DOCS: u16 = 24;

/// Widest it is drawn.
const WIDEST_DOCS: u16 = 60;

/// The documentation of the suggestion being looked at, beside the popup.
#[derive(Debug)]
pub struct DocsView<'a> {
    palette: &'a Palette,
    text: &'a str,
}

impl<'a> DocsView<'a> {
    /// Documentation reading `text`, as plain text.
    #[must_use]
    pub const fn new(palette: &'a Palette, text: &'a str) -> Self {
        Self { palette, text }
    }

    /// Where the documentation for a popup at `popup` goes within `bounds`:
    /// to its right, or its left when that is where the room is, and not at
    /// all when neither side has any. `above` says the popup opened above
    /// the word, so the two line up at the bottom rather than the top.
    #[must_use]
    pub fn area(bounds: Rect, popup: Rect, above: bool, text: &str) -> Option<Rect> {
        let right = bounds.right().saturating_sub(popup.right());
        let left = popup.x.saturating_sub(bounds.x);
        let (x, width) = if right >= NARROWEST_DOCS {
            (popup.right(), right.min(WIDEST_DOCS))
        } else if left >= NARROWEST_DOCS {
            let width = left.min(WIDEST_DOCS);
            (popup.x - width, width)
        } else {
            return None;
        };
        let lines = u16::try_from(wrap(text, width.saturating_sub(2)).len()).unwrap_or(u16::MAX);
        if lines == 0 {
            return None;
        }
        let room = if above {
            popup.bottom().saturating_sub(bounds.y)
        } else {
            bounds.bottom().saturating_sub(popup.y)
        };
        let height = lines.min(MOST_DOC_LINES).min(room);
        let y = if above { popup.bottom() - height } else { popup.y };
        Some(Rect::new(x, y, width, height))
    }
}

impl Widget for DocsView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        let area = area.intersection(*cells.area());
        if area.width < 4 || area.height == 0 {
            return;
        }
        let style = self.palette.on(Role::Raised, Role::Text);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(style);
            }
        }
        let room = area.width.saturating_sub(2);
        for (offset, line) in
            wrap(self.text, room).iter().take(usize::from(area.height)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            write(cells, area.x + 1, area.y + offset, room, line, style);
        }
    }
}

/// `text` broken into lines no wider than `width`: at spaces where it can
/// be, anywhere where a word is wider than the line. Markdown code fences
/// are left out; what they fence is kept.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for source in text.lines() {
        if source.trim_start().starts_with("```") {
            continue;
        }
        // A tab or a stray carriage return written into a cell would move
        // the terminal's cursor and tear the row.
        let source: String =
            source.replace('\t', "    ").chars().filter(|ch| !ch.is_control()).collect();
        let mut line = String::new();
        let mut used = 0usize;
        for word in source.as_str().split_word_bounds() {
            let wide = word.width();
            if used + wide > width && used > 0 {
                lines.push(std::mem::take(&mut line).trim_end().to_string());
                used = 0;
                if word.trim().is_empty() {
                    continue;
                }
            }
            if wide > width {
                // A word wider than the line breaks where it must.
                for cluster in word.graphemes(true) {
                    let cluster_width = cluster.width();
                    if used + cluster_width > width {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push_str(cluster);
                    used += cluster_width;
                }
                continue;
            }
            line.push_str(word);
            used += wide;
        }
        lines.push(line.trim_end().to_string());
    }
    // Blank lines at the end say nothing.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Harness;
    use nun_theme::{Probe, derive};

    fn palette() -> Palette {
        Palette::new(derive(&Probe::builtin_dark()))
    }

    fn suggestion(label: &str, detail: &str) -> Suggestion {
        Suggestion {
            kind: "fn",
            kind_role: Role::Function,
            label: label.to_string(),
            matched: Vec::new(),
            detail: detail.to_string(),
            deprecated: false,
        }
    }

    #[test]
    fn it_opens_below_the_word_when_there_is_room() {
        let bounds = Rect::new(0, 0, 80, 24);
        let area = CompletionView::area(bounds, 20, 3, 5, 30);
        assert_eq!(area, Rect::new(14, 4, 30, 5));
    }

    #[test]
    fn it_flips_above_the_word_near_the_bottom() {
        let bounds = Rect::new(0, 0, 80, 24);
        let area = CompletionView::area(bounds, 20, 21, 8, 30);
        assert_eq!((area.y, area.height), (13, 8), "ends on the row above the word");
        assert_eq!(area.bottom(), 21);
    }

    #[test]
    fn it_takes_the_bigger_side_when_neither_fits() {
        let bounds = Rect::new(0, 0, 80, 8);
        let low = CompletionView::area(bounds, 5, 2, 10, 30);
        assert_eq!((low.y, low.height), (3, 5), "five below beats two above");
        let high = CompletionView::area(bounds, 5, 5, 10, 30);
        assert_eq!((high.y, high.height), (0, 5), "five above beats two below");
    }

    #[test]
    fn it_never_runs_off_the_right_edge() {
        let bounds = Rect::new(0, 0, 40, 24);
        let area = CompletionView::area(bounds, 38, 0, 3, 30);
        assert_eq!(area.right(), 40);
    }

    #[test]
    fn rows_are_found_where_they_are_drawn() {
        let area = Rect::new(4, 10, 30, 5);
        assert_eq!(CompletionView::row_at(area, 3, 10, 20), Some(3));
        assert_eq!(CompletionView::row_at(area, 3, 14, 20), Some(7));
        assert_eq!(CompletionView::row_at(area, 3, 15, 20), None);
        assert_eq!(CompletionView::row_at(area, 18, 12, 20), None, "past the last");
    }

    #[test]
    fn a_row_shows_its_kind_label_and_detail() {
        let suggestions = [suggestion("println", "macro"), suggestion("print", "")];
        let mut harness = Harness::new(30, 2);
        let palette = palette();
        harness.draw(CompletionView::new(&palette, &suggestions));
        let text = harness.to_text();
        let first = text.lines().next().unwrap();
        assert!(first.starts_with(" fn   println"), "{first:?}");
        assert!(first.trim_end().ends_with("macro"), "{first:?}");
    }

    #[test]
    fn the_selected_row_is_washed_in_the_accent() {
        let suggestions = [suggestion("a", ""), suggestion("b", "")];
        let palette = palette();
        let mut harness = Harness::new(30, 2);
        harness.draw(CompletionView::new(&palette, &suggestions).selected(1));
        let accent = palette.on(Role::Accent, Role::OnAccent).bg;
        assert_eq!(harness.cells()[(0, 1)].bg, accent.unwrap());
        assert_ne!(harness.cells()[(0, 0)].bg, accent.unwrap());
    }

    #[test]
    fn docs_go_beside_the_popup_where_there_is_room() {
        let bounds = Rect::new(0, 0, 100, 24);
        let popup = Rect::new(10, 5, 30, 6);
        let right = DocsView::area(bounds, popup, false, "Prints to stdout.").unwrap();
        assert_eq!((right.x, right.y, right.height), (40, 5, 1));

        let crowded = Rect::new(70, 5, 30, 6);
        let left = DocsView::area(bounds, crowded, false, "Prints.").unwrap();
        assert_eq!(left.right(), 70);

        let narrow = Rect::new(0, 0, 40, 24);
        assert_eq!(DocsView::area(narrow, Rect::new(5, 5, 30, 6), false, "x"), None);
    }

    #[test]
    fn docs_above_the_word_line_up_at_the_bottom() {
        let bounds = Rect::new(0, 0, 100, 24);
        let popup = Rect::new(10, 12, 30, 8);
        let docs = DocsView::area(bounds, popup, true, "one\ntwo").unwrap();
        assert_eq!(docs.bottom(), popup.bottom());
    }

    #[test]
    fn documentation_wraps_at_spaces_and_drops_fences() {
        assert_eq!(wrap("one two three", 8), ["one two", "three"]);
        assert_eq!(wrap("```rust\nfn x()\n```", 20), ["fn x()"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("日本語の文", 4), ["日本", "語の", "文"]);
        assert_eq!(wrap("text\n\n", 10), ["text"]);
        assert_eq!(wrap("\tcode\rhere", 20), ["    codehere"], "no control chars reach a cell");
    }
}
