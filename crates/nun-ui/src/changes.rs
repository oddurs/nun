//! What changed since git last had a file, as far as drawing it goes: what
//! each changed line is, and the rail column that shows where in the whole
//! file the changes are.
//!
//! The gutter draws a [`Change`] beside each changed line in view (see
//! [`EditorView::with_changes`](crate::EditorView::with_changes)). The rail
//! gives changes a column of their own, just left of the diagnostics' column,
//! rather than a share of it. A cell has one foreground colour, so one column
//! for both would have to let one of them decide what a row shows — a change
//! hiding an error on the same stretch of file, or an error hiding the
//! change. Side by side, neither wins: each keeps its own colour and glyph,
//! each row of each column can be rested on and clicked for what is there,
//! and the thumb is drawn across both so they still read as one scrollbar.
//! It costs one column, and only on a file git follows.

use std::ops::Range;

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::glyph::Glyph;
use crate::marks::{Rail, ground};
use crate::style::Palette;

/// What happened to a line, as the gutter marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// A line git does not have.
    Added,
    /// A line that is different from git's.
    Modified,
    /// Lines git has were removed just above this one.
    RemovedAbove,
    /// Lines git has were removed after this one, the last.
    RemovedBelow,
}

impl Change {
    /// The role it is drawn in.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Added => Role::Added,
            Self::Modified => Role::Changed,
            Self::RemovedAbove | Self::RemovedBelow => Role::Removed,
        }
    }

    /// The glyph it is drawn with.
    #[must_use]
    pub const fn glyph(self) -> Glyph {
        match self {
            Self::Added => Glyph::ChangeAdded,
            Self::Modified => Glyph::ChangeModified,
            Self::RemovedAbove => Glyph::ChangeRemovedAbove,
            Self::RemovedBelow => Glyph::ChangeRemovedBelow,
        }
    }
}

/// The changes column of the rail, drawn.
#[derive(Debug)]
pub struct ChangeRail<'a> {
    /// The lines each change covers — a removal, the one line it is marked
    /// on — and what it is.
    changes: &'a [(Range<usize>, Change)],
    lines: usize,
    view: Range<usize>,
    palette: &'a Palette,
    hovered: Option<u16>,
}

impl<'a> ChangeRail<'a> {
    /// The column for a file of `lines` lines with `changes` in it, in the
    /// order of their lines.
    #[must_use]
    pub const fn new(
        changes: &'a [(Range<usize>, Change)],
        lines: usize,
        palette: &'a Palette,
    ) -> Self {
        Self { changes, lines, view: 0..0, palette, hovered: None }
    }

    /// The lines in view, which the thumb covers.
    #[must_use]
    pub const fn viewing(mut self, lines: Range<usize>) -> Self {
        self.view = lines;
        self
    }

    /// The row under the pointer.
    #[must_use]
    pub const fn hovered(mut self, row: Option<u16>) -> Self {
        self.hovered = row;
        self
    }

    /// The rows with changes on them, top to bottom, and what each shows:
    /// what every change there is, when they agree, and a modification when
    /// they do not — some lines went and some came, which is what one is.
    /// A row is laid out the same way as the diagnostics' column, by
    /// [`Rail::row_of`].
    #[must_use]
    pub fn rows(
        changes: &[(Range<usize>, Change)],
        lines: usize,
        height: u16,
    ) -> Vec<(u16, Change)> {
        let mut rows: Vec<(u16, Change)> = Vec::new();
        for (range, change) in changes {
            let first = Rail::row_of(range.start, lines, height);
            let last = Rail::row_of(range.end.max(range.start + 1) - 1, lines, height);
            for row in first..=last {
                match rows.last_mut() {
                    Some((at, shown)) if *at == row => {
                        if !same_kind(*shown, *change) {
                            *shown = Change::Modified;
                        }
                    }
                    _ => rows.push((row, *change)),
                }
            }
        }
        rows
    }
}

/// Whether two changes are the same kind of thing: a removal is one, above
/// or below.
const fn same_kind(one: Change, other: Change) -> bool {
    matches!(
        (one, other),
        (Change::Added, Change::Added)
            | (Change::Modified, Change::Modified)
            | (
                Change::RemovedAbove | Change::RemovedBelow,
                Change::RemovedAbove | Change::RemovedBelow
            )
    )
}

impl Widget for ChangeRail<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let x = area.left();
        let ground = ground(&self.view, self.lines, area.height, self.hovered);
        for row in 0..area.height {
            cells[(x, area.top() + row)]
                .set_char(' ')
                .set_style(self.palette.on(ground(row), Role::Text));
        }
        for (row, change) in Self::rows(self.changes, self.lines, area.height) {
            cells[(x, area.top() + row)]
                .set_symbol(self.palette.glyph(change.glyph()))
                .set_style(self.palette.on(ground(row), change.role()));
        }
    }
}

#[cfg(test)]
mod tests {
    use nun_theme::{Probe, derive};

    use super::*;

    #[test]
    fn a_change_covers_every_row_its_lines_reach() {
        let changes = [(2..5, Change::Added), (8..9, Change::RemovedAbove)];
        assert_eq!(
            ChangeRail::rows(&changes, 10, 20),
            [(2, Change::Added), (3, Change::Added), (4, Change::Added), (8, Change::RemovedAbove),]
        );
    }

    #[test]
    fn changes_that_disagree_on_one_row_show_as_a_modification() {
        let changes =
            [(0..1, Change::Added), (5..6, Change::RemovedAbove), (10..11, Change::RemovedBelow)];
        // A thousand lines on ten rows: the first two share row 0.
        let rows = ChangeRail::rows(&changes, 1000, 10);
        assert_eq!(rows, [(0, Change::Modified)]);
        let alike = [(5..6, Change::RemovedAbove), (10..11, Change::RemovedBelow)];
        assert_eq!(ChangeRail::rows(&alike, 1000, 10), [(0, Change::RemovedAbove)]);
    }

    #[test]
    fn each_kind_is_drawn_in_its_own_colour_and_glyph_over_the_thumb() {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let changes =
            [(0..1, Change::Added), (3..4, Change::Modified), (6..7, Change::RemovedAbove)];
        let mut cells = Cells::empty(Rect::new(0, 0, 1, 8));
        ChangeRail::new(&changes, 8, &palette)
            .viewing(0..4)
            .render(Rect::new(0, 0, 1, 8), &mut cells);
        for (row, change) in [(0, Change::Added), (3, Change::Modified), (6, Change::RemovedAbove)]
        {
            assert_eq!(cells[(0, row)].symbol(), palette.glyph(change.glyph()));
            assert_eq!(cells[(0, row)].fg, palette.fg(change.role()).fg.unwrap());
        }
        let roles = [Change::Added.role(), Change::Modified.role(), Change::RemovedAbove.role()];
        assert!(roles[0] != roles[1] && roles[1] != roles[2] && roles[0] != roles[2]);
        assert_eq!(cells[(0, 3)].bg, palette.on(Role::Line, Role::Text).bg.unwrap(), "the thumb");
        assert_eq!(cells[(0, 6)].bg, palette.ground(), "the track below it");
    }
}
