//! Diagnostics, as far as drawing them goes: how serious each is, and the
//! rail beside the text that shows where in the whole file they are.
//!
//! The rail is one column at the right of a pane. Each row stands for a band
//! of the file's lines, and a band holding marks shows the worst of them in
//! its colour, with a glyph that fills with how many there are — so two
//! errors that land on one row read as more than one, rather than as a single
//! mark hiding the second. The part of the file in view is drawn behind the
//! marks as a thumb, which makes it a scrollbar as well.

use std::ops::Range;

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::glyph::Glyph;
use crate::style::Palette;

/// How serious a diagnostic is. Ordered most serious first, so the worst of
/// several is the least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something that will not build or run.
    Error,
    /// Something that probably should not be there.
    Warning,
    /// Something worth knowing.
    Info,
    /// A suggestion.
    Hint,
}

impl Severity {
    /// The role it is drawn in.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Error => Role::Error,
            Self::Warning => Role::Warn,
            Self::Info => Role::Info,
            Self::Hint => Role::Dim,
        }
    }

    /// What a person calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

/// One diagnostic's place in the text, in char indices, and how serious it
/// is. An empty range marks the one character at `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// Char index where it begins.
    pub start: usize,
    /// Char index where it ends.
    pub end: usize,
    /// How serious it is.
    pub severity: Severity,
}

impl Mark {
    /// Whether the character at `at` is under this mark.
    #[must_use]
    pub const fn covers(&self, at: usize) -> bool {
        if self.start == self.end { at == self.start } else { self.start <= at && at < self.end }
    }
}

/// How many diagnostics there are of each kind.
///
/// The status line and the rail both count from the same list, and hints are
/// counted with the informational ones in both, so the two always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    /// Errors.
    pub errors: usize,
    /// Warnings.
    pub warnings: usize,
    /// Informational diagnostics and hints.
    pub notes: usize,
}

impl Tally {
    /// Count `severities`.
    #[must_use]
    pub fn of(severities: impl IntoIterator<Item = Severity>) -> Self {
        let mut tally = Self::default();
        for severity in severities {
            match severity {
                Severity::Error => tally.errors += 1,
                Severity::Warning => tally.warnings += 1,
                Severity::Info | Severity::Hint => tally.notes += 1,
            }
        }
        tally
    }

    /// All of them.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.errors + self.warnings + self.notes
    }
}

/// One row of the rail that has marks on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    /// The row, counted from the top of the rail.
    pub row: u16,
    /// How many marks landed on it.
    pub count: usize,
    /// The worst of them.
    pub worst: Severity,
}

/// The rail, drawn.
#[derive(Debug)]
pub struct Rail<'a> {
    /// The line each mark starts on, and how serious it is.
    marks: &'a [(usize, Severity)],
    lines: usize,
    view: Range<usize>,
    palette: &'a Palette,
    hovered: Option<u16>,
}

impl<'a> Rail<'a> {
    /// A rail for a file of `lines` lines, with `marks` on it.
    #[must_use]
    pub const fn new(marks: &'a [(usize, Severity)], lines: usize, palette: &'a Palette) -> Self {
        Self { marks, lines, view: 0..0, palette, hovered: None }
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

    /// The row `line` is drawn on, in a rail `height` rows tall for a file of
    /// `lines` lines.
    ///
    /// A file shorter than the rail is one line per row rather than stretched,
    /// so a short file's marks sit beside the lines they are on.
    #[must_use]
    pub fn row_of(line: usize, lines: usize, height: u16) -> u16 {
        let height_lines = usize::from(height);
        if height_lines == 0 {
            return 0;
        }
        let span = lines.max(height_lines);
        let row = line.min(span - 1) * height_lines / span;
        u16::try_from(row).unwrap_or(height - 1).min(height - 1)
    }

    /// The lines drawn on `row`: every line whose [`Rail::row_of`] is `row`.
    /// Empty for a row below the end of a short file.
    #[must_use]
    pub fn lines_at(row: u16, lines: usize, height: u16) -> Range<usize> {
        let height_lines = usize::from(height);
        if height_lines == 0 {
            return 0..0;
        }
        let span = lines.max(height_lines);
        // The first line whose row is at least `row`: `line * h / span >= row`.
        let first = |row: usize| (row * span).div_ceil(height_lines);
        let row = usize::from(row);
        first(row).min(lines)..first(row + 1).min(lines)
    }

    /// The rows with marks on them, top to bottom.
    #[must_use]
    pub fn buckets(marks: &[(usize, Severity)], lines: usize, height: u16) -> Vec<Bucket> {
        let mut rows: Vec<(u16, Severity)> = marks
            .iter()
            .map(|&(line, severity)| (Self::row_of(line, lines, height), severity))
            .collect();
        rows.sort_unstable();
        let mut buckets: Vec<Bucket> = Vec::new();
        for (row, severity) in rows {
            match buckets.last_mut() {
                // Sorted by row, then by severity, so the first mark of a row
                // is already its worst.
                Some(bucket) if bucket.row == row => bucket.count += 1,
                _ => buckets.push(Bucket { row, count: 1, worst: severity }),
            }
        }
        buckets
    }

    /// The glyph for `count` marks on one row: a bar that fills as they add
    /// up, so a row of five is visibly not a row of one. None for none.
    #[must_use]
    pub const fn glyph(count: usize) -> Option<Glyph> {
        match count {
            0 => None,
            1 => Some(Glyph::Rail1),
            2 => Some(Glyph::Rail2),
            3 | 4 => Some(Glyph::Rail3),
            _ => Some(Glyph::Rail4),
        }
    }
}

impl Widget for Rail<'_> {
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
        for bucket in Self::buckets(self.marks, self.lines, area.height) {
            let style = self.palette.on(ground(bucket.row), bucket.worst.role());
            let glyph = Self::glyph(bucket.count).map_or(" ", |glyph| self.palette.glyph(glyph));
            cells[(x, area.top() + bucket.row)].set_symbol(glyph).set_style(style);
        }
    }
}

/// What each row of a rail `height` rows tall is drawn on: the thumb over
/// `view`, the lines in view, and the hovered row stronger still. Shared by
/// every column of the rail, so they read as one scrollbar.
pub(crate) fn ground(
    view: &Range<usize>,
    lines: usize,
    height: u16,
    hovered: Option<u16>,
) -> impl Fn(u16) -> Role {
    let thumb = (!view.is_empty()).then(|| {
        Rail::row_of(view.start, lines, height)..=Rail::row_of(view.end - 1, lines, height)
    });
    move |row: u16| {
        if hovered == Some(row) {
            Role::LineStrong
        } else if thumb.as_ref().is_some_and(|thumb| thumb.contains(&row)) {
            Role::Line
        } else {
            Role::Ground
        }
    }
}

#[cfg(test)]
mod tests {
    use nun_theme::{Probe, derive};
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_short_file_is_one_line_per_row() {
        assert_eq!(Rail::row_of(3, 10, 20), 3);
        assert_eq!(Rail::lines_at(3, 10, 20), 3..4);
        assert_eq!(Rail::lines_at(15, 10, 20), 10..10, "nothing below the end");
    }

    #[test]
    fn a_long_file_is_squeezed_evenly() {
        assert_eq!(Rail::row_of(0, 1000, 10), 0);
        assert_eq!(Rail::row_of(999, 1000, 10), 9);
        assert_eq!(Rail::lines_at(0, 1000, 10), 0..100);
        assert_eq!(Rail::lines_at(9, 1000, 10), 900..1000);
    }

    #[test]
    fn marks_that_collide_on_one_row_are_counted_and_show_the_worst() {
        let marks = [
            (40, Severity::Warning),
            (41, Severity::Error),
            (45, Severity::Hint),
            (900, Severity::Info),
        ];
        let buckets = Rail::buckets(&marks, 1000, 10);
        assert_eq!(
            buckets,
            [
                Bucket { row: 0, count: 3, worst: Severity::Error },
                Bucket { row: 9, count: 1, worst: Severity::Info },
            ]
        );
        assert_ne!(Rail::glyph(3), Rail::glyph(1), "three read as more than one");
    }

    #[test]
    fn the_rail_draws_each_bucket_in_its_worst_colour() {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let marks = [(0, Severity::Warning), (0, Severity::Error), (5, Severity::Info)];
        let mut cells = Cells::empty(Rect::new(0, 0, 1, 8));
        Rail::new(&marks, 8, &palette).viewing(0..4).render(Rect::new(0, 0, 1, 8), &mut cells);
        assert_eq!(cells[(0, 0)].symbol(), palette.glyph(Glyph::Rail2));
        assert_eq!(cells[(0, 0)].fg, palette.fg(Role::Error).fg.unwrap());
        assert_eq!(cells[(0, 5)].symbol(), palette.glyph(Glyph::Rail1));
        assert_eq!(cells[(0, 5)].fg, palette.fg(Role::Info).fg.unwrap());
        assert_eq!(cells[(0, 3)].bg, palette.on(Role::Line, Role::Text).bg.unwrap(), "the thumb");
        assert_eq!(cells[(0, 6)].bg, palette.ground(), "the track below it");
    }

    proptest! {
        /// Every line is on exactly the row whose lines include it, so a
        /// click on a row and the marks drawn there agree.
        #[test]
        fn rows_and_their_lines_agree(lines in 1usize..5000, height in 1u16..200) {
            let mut covered = 0;
            for row in 0..height {
                let range = Rail::lines_at(row, lines, height);
                prop_assert_eq!(range.start, covered, "rows cover the file in order");
                for line in range.clone() {
                    prop_assert_eq!(Rail::row_of(line, lines, height), row);
                }
                covered = range.end;
            }
            prop_assert_eq!(covered, lines, "and all of it");
        }

        /// However the marks fall, the rail counts every one of them.
        #[test]
        fn the_rail_loses_no_marks(
            marks in proptest::collection::vec((0usize..3000, 0u8..4), 0..200),
            lines in 1usize..3000,
            height in 1u16..100,
        ) {
            let severities = [Severity::Error, Severity::Warning, Severity::Info, Severity::Hint];
            let marks: Vec<(usize, Severity)> =
                marks.into_iter().map(|(line, s)| (line % lines, severities[usize::from(s)])).collect();
            let counted: usize = Rail::buckets(&marks, lines, height).iter().map(|b| b.count).sum();
            prop_assert_eq!(counted, Tally::of(marks.iter().map(|m| m.1)).total());
        }
    }
}
