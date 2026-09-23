//! The references panel: every place a symbol is used, under the files they
//! are in.
//!
//! It is the search panel's list without the search panel's fields. The rows
//! are [`SearchRow`]s and they are drawn by the same code, so a reference
//! reads exactly as a search hit does — a file with a count, then its lines,
//! each with its number and the symbol picked out — and a person who has used
//! one has already used the other.
//!
//! Geometry is exposed as associated functions, as the search panel's is, so
//! the binary lays out exactly the hit regions this draws.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

use crate::search::{BACK, SearchRow, SearchView, band, fill, put};
use crate::style::Palette;

/// The panel's name.
const TITLE: &str = "REFERENCES";

/// Rows above the list: the header and the summary.
const HEAD_ROWS: u16 = 2;

/// The references panel, drawn.
///
/// As with [`SearchView`], `rows` may be only the window on screen, passed
/// with a scroll of zero, `selected` and `hovered` rebased into it, and
/// [`ReferencesView::widest_line`] for the whole list.
#[derive(Debug)]
pub struct ReferencesView<'a> {
    rows: &'a [SearchRow<'a>],
    palette: &'a Palette,
    summary: &'a str,
    scroll: usize,
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_back: bool,
    widest_line: Option<u32>,
}

impl<'a> ReferencesView<'a> {
    /// A panel listing `rows`, saying `summary` about them.
    #[must_use]
    pub const fn new(rows: &'a [SearchRow<'a>], summary: &'a str, palette: &'a Palette) -> Self {
        Self {
            rows,
            palette,
            summary,
            scroll: 0,
            selected: None,
            hovered: None,
            hovered_back: false,
            widest_line: None,
        }
    }

    /// Start drawing the list from row `row`.
    #[must_use]
    pub const fn scrolled_to(mut self, row: usize) -> Self {
        self.scroll = row;
        self
    }

    /// Mark row `row` as the one last opened.
    #[must_use]
    pub const fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }

    /// Mark row `row` as under the pointer.
    #[must_use]
    pub const fn hovered(mut self, row: Option<usize>) -> Self {
        self.hovered = row;
        self
    }

    /// Mark the header's button as under the pointer.
    #[must_use]
    pub const fn hovered_back(mut self, hovered: bool) -> Self {
        self.hovered_back = hovered;
        self
    }

    /// The largest line number anywhere in the list, when `rows` is a window.
    #[must_use]
    pub const fn widest_line(mut self, line: u32) -> Self {
        self.widest_line = Some(line);
        self
    }

    /// What the header's button does, for the status line on hover.
    pub const BACK_DESCRIPTION: &'static str = SearchView::BACK_DESCRIPTION;

    /// The header row of `area`.
    #[must_use]
    pub fn header_area(area: Rect) -> Rect {
        band(area, 0)
    }

    /// The cell in the header that gives the sidebar back to the file tree,
    /// where the search panel has the same button.
    #[must_use]
    pub fn back_area(area: Rect) -> Option<Rect> {
        SearchView::back_area(area)
    }

    /// The line under the header that says what was found.
    #[must_use]
    pub fn summary_area(area: Rect) -> Rect {
        band(area, 1)
    }

    /// Where the list goes.
    #[must_use]
    pub fn rows_area(area: Rect) -> Rect {
        let head = HEAD_ROWS.min(area.height);
        Rect { y: area.y + head, height: area.height - head, ..area }
    }

    /// How many rows of the list fit.
    #[must_use]
    pub fn visible_rows(area: Rect) -> usize {
        usize::from(Self::rows_area(area).height)
    }
}

impl Widget for ReferencesView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        fill(cells, area, self.palette.on(Role::Raised, Role::Text));

        let header = Self::header_area(area);
        let style = self.palette.on(Role::Raised, Role::Dim).add_modifier(Modifier::BOLD);
        put(
            cells,
            header.x.saturating_add(1),
            header.y,
            header.width.saturating_sub(4),
            TITLE,
            style,
        );
        if let Some(cell) = Self::back_area(area) {
            let style = if self.hovered_back {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Dim)
            };
            put(cells, cell.x, cell.y, 1, BACK, style);
        }

        let summary = Self::summary_area(area);
        if summary.height > 0 {
            let x = summary.x.saturating_add(1);
            let style = self.palette.on(Role::Raised, Role::Faint);
            put(cells, x, summary.y, summary.right().saturating_sub(x), self.summary, style);
        }

        let mut list = SearchView::new("", self.rows, self.palette)
            .scrolled_to(self.scroll)
            .selected(self.selected)
            .hovered(self.hovered);
        if let Some(line) = self.widest_line {
            list = list.widest_line(line);
        }
        list.draw_rows(cells, Self::rows_area(area));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Harness;
    use crate::search::HitState;
    use nun_theme::{Probe, derive};

    #[test]
    fn references_read_as_search_hits_under_a_header_of_their_own() {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let matched = [std::ops::Range { start: 4, end: 9 }];
        let rows = [
            SearchRow::File { path: "src/lib.rs", hits: 1, collapsed: false },
            SearchRow::Hit {
                line: 12,
                text: "let value = 1;",
                matched: &matched,
                state: HitState::Plain,
            },
        ];
        let mut screen = Harness::new(30, 6);
        screen.draw(ReferencesView::new(&rows, "1 reference to value", &palette));
        let text = screen.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("REFERENCES"), "{text}");
        assert!(lines[0].contains(BACK), "{text}");
        assert!(lines[1].contains("1 reference to value"), "{text}");
        assert!(lines[2].contains("src/lib.rs"), "{text}");
        assert!(lines[3].contains("12") && lines[3].contains("let value = 1;"), "{text}");
    }

    #[test]
    fn the_list_starts_under_the_summary() {
        let area = Rect::new(0, 0, 20, 10);
        assert_eq!(ReferencesView::rows_area(area), Rect::new(0, 2, 20, 8));
        assert_eq!(ReferencesView::visible_rows(Rect::new(0, 0, 20, 1)), 0);
    }
}
