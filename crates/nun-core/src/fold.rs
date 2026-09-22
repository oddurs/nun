//! Folded regions: text out of sight but still in the buffer.
//!
//! A fold is held as the char offsets of the text it hides rather than as line
//! numbers, for the same reason selections are: an edit anywhere above it
//! moves it, and offsets map through an edit the way everything else in this
//! crate does. Line numbers are worked out from the rope when something asks
//! which lines are hidden.

use crate::edit::{Assoc, Edit};

/// One folded region: from the newline that ends its header line to the end
/// of the last line it hides, before that line's own newline.
///
/// So the hidden text starts with a line break and ends just short of one,
/// and the header and everything after the region are untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fold {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Fold {
    /// Whether a caret at `at` would be out of sight. The end of the header
    /// line — `start` itself — is in view, since that is where the fold is
    /// drawn.
    pub(crate) const fn hides(&self, at: usize) -> bool {
        self.start < at && at <= self.end
    }

    /// Where this fold is after `edit`, or `None` when the edit reached into
    /// what it hides: text that changed out of sight is shown, not kept
    /// folded over something nobody has looked at.
    fn mapped(self, edit: &Edit) -> Option<Self> {
        let clear = edit.end <= self.start || edit.start > self.end;
        // A line break put in at the very end of the header splits it, and
        // the fold would follow the break onto a line of its own — a blank
        // line wearing the fold, the real header left bare above it.
        if edit.end == self.start && edit.text.contains('\n') {
            return None;
        }
        clear.then(|| Self {
            start: edit.map_pos(self.start, Assoc::After),
            end: edit.map_pos(self.end, Assoc::After),
        })
    }
}

/// Every fold in a buffer, ordered by where each starts.
///
/// Folds may nest — a method folded inside a folded `impl` stays folded when
/// the `impl` is opened — so this is a set of regions, not a partition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Folds {
    folds: Vec<Fold>,
}

impl Folds {
    /// Fold a region. Folding one that starts where another does replaces
    /// it, since the two would share a header and an arrow.
    pub(crate) fn add(&mut self, fold: Fold) {
        match self.folds.binary_search_by_key(&fold.start, |f| f.start) {
            Ok(at) => self.folds[at] = fold,
            Err(at) => self.folds.insert(at, fold),
        }
    }

    /// Unfold the region whose header ends at `start`. Whether there was one.
    pub(crate) fn remove_at(&mut self, start: usize) -> bool {
        let before = self.folds.len();
        self.folds.retain(|fold| fold.start != start);
        self.folds.len() != before
    }

    /// Unfold everything. Whether anything was folded.
    pub(crate) fn clear(&mut self) -> bool {
        let any = !self.folds.is_empty();
        self.folds.clear();
        any
    }

    /// The folds, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Fold> {
        self.folds.iter()
    }

    /// Follow one edit, dropping any fold it reached into.
    pub(crate) fn map_through(&mut self, edit: &Edit) {
        if self.folds.is_empty() || edit.is_noop() {
            return;
        }
        self.folds = self.folds.iter().filter_map(|fold| fold.mapped(edit)).collect();
    }

    /// Open every fold hiding one of `heads`, which must be sorted. Whether
    /// any was opened.
    pub(crate) fn reveal(&mut self, heads: &[usize]) -> bool {
        let before = self.folds.len();
        self.folds.retain(|fold| {
            // The first head past the start is the only one that can be inside.
            let at = heads.partition_point(|&head| head <= fold.start);
            heads.get(at).is_none_or(|&head| !fold.hides(head))
        });
        self.folds.len() != before
    }
}

/// Which lines are out of sight, as runs of whole lines, merged and in order.
///
/// A snapshot: work it out once, then ask it as many questions as a frame
/// needs. Everything that turns rows into lines or back — drawing, clicking,
/// scrolling, moving the caret up and down — goes through one of these, which
/// is what keeps a click landing on the line drawn under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hidden {
    /// Inclusive runs of hidden lines, sorted, disjoint and not touching.
    runs: Vec<(usize, usize)>,
    /// The buffer's last line.
    last: usize,
}

impl Hidden {
    /// The hidden runs `runs` (inclusive, in any order, possibly overlapping)
    /// in a buffer whose last line is `last`.
    pub(crate) fn new(mut runs: Vec<(usize, usize)>, last: usize) -> Self {
        runs.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
        for (first, end) in runs {
            match merged.last_mut() {
                Some(previous) if first <= previous.1 + 1 => previous.1 = previous.1.max(end),
                _ => merged.push((first, end)),
            }
        }
        Self { runs: merged, last }
    }

    /// Whether nothing is folded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The hidden run containing `line`, if it is hidden.
    fn run_of(&self, line: usize) -> Option<(usize, usize)> {
        let at = self.runs.partition_point(|run| run.1 < line);
        self.runs.get(at).copied().filter(|run| run.0 <= line)
    }

    /// Whether `line` is out of sight.
    #[must_use]
    pub fn is_hidden(&self, line: usize) -> bool {
        self.run_of(line).is_some()
    }

    /// `line` if it is in view, otherwise the line whose fold hides it —
    /// where a view scrolled into the middle of a fold should stand.
    #[must_use]
    pub fn in_view(&self, line: usize) -> usize {
        // A run never starts on line 0: there is always a header above it.
        self.run_of(line).map_or(line, |run| run.0.saturating_sub(1))
    }

    /// The next line in view after `line`, or `None` at the end.
    #[must_use]
    pub fn next(&self, line: usize) -> Option<usize> {
        let mut next = line + 1;
        if let Some(run) = self.run_of(next) {
            next = run.1 + 1;
        }
        (next <= self.last).then_some(next)
    }

    /// The line in view before `line`, or `None` at the start.
    #[must_use]
    pub fn previous(&self, line: usize) -> Option<usize> {
        let previous = line.checked_sub(1)?;
        Some(self.in_view(previous))
    }

    /// The line `rows` lines in view below `line` (above, when negative),
    /// stopping at the first or the last line in view.
    #[must_use]
    pub fn step(&self, line: usize, rows: isize) -> usize {
        let mut at = self.in_view(line.min(self.last));
        for _ in 0..rows.unsigned_abs() {
            let moved = if rows < 0 { self.previous(at) } else { self.next(at) };
            match moved {
                Some(moved) => at = moved,
                None => break,
            }
        }
        at
    }

    /// How many lines in view there are from `from` up to, not including, `to`.
    #[must_use]
    pub fn rows_between(&self, from: usize, to: usize) -> usize {
        if to <= from {
            return 0;
        }
        let hidden: usize = self
            .runs
            .iter()
            .map(|&(first, end)| {
                let (first, end) = (first.max(from), end.min(to - 1));
                if end >= first { end - first + 1 } else { 0 }
            })
            .sum();
        to - from - hidden
    }

    /// The last line in view.
    #[must_use]
    pub fn last_in_view(&self) -> usize {
        self.in_view(self.last)
    }

    /// The lines in view from `line` on, in order.
    pub fn from(&self, line: usize) -> impl Iterator<Item = usize> + '_ {
        let first = (line <= self.last).then(|| self.in_view(line));
        std::iter::successors(first, |&line| self.next(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines 2..=4 and 8..=8 hidden in a buffer of 12 lines.
    fn hidden() -> Hidden {
        Hidden::new(vec![(8, 8), (2, 4), (3, 3)], 11)
    }

    #[test]
    fn runs_are_merged_and_asked_about_by_line() {
        let h = hidden();
        assert_eq!(h.runs, [(2, 4), (8, 8)], "the nested run is absorbed");
        assert!(!h.is_hidden(1) && h.is_hidden(2) && h.is_hidden(4) && !h.is_hidden(5));
        assert_eq!(h.in_view(3), 1, "the header of the fold");
    }

    #[test]
    fn stepping_skips_what_is_hidden_and_stops_at_the_ends() {
        let h = hidden();
        assert_eq!(h.step(1, 1), 5);
        assert_eq!(h.step(5, -1), 1);
        assert_eq!(h.step(7, 1), 9);
        assert_eq!(h.step(0, -3), 0);
        assert_eq!(h.step(10, 5), 11);
        assert_eq!(h.from(0).collect::<Vec<_>>(), [0, 1, 5, 6, 7, 9, 10, 11]);
    }

    #[test]
    fn rows_between_counts_only_lines_in_view() {
        let h = hidden();
        assert_eq!(h.rows_between(0, 12), 8);
        assert_eq!(h.rows_between(1, 5), 1, "line 1 alone; 2 to 4 are hidden");
        assert_eq!(h.rows_between(5, 5), 0);
    }

    #[test]
    fn a_fold_hidden_at_the_end_leaves_its_header_as_the_last_line() {
        let h = Hidden::new(vec![(10, 11)], 11);
        assert_eq!(h.last_in_view(), 9);
        assert_eq!(h.step(0, 100), 9);
    }

    #[test]
    fn an_edit_above_a_fold_moves_it_and_one_inside_opens_it() {
        let mut folds = Folds::default();
        folds.add(Fold { start: 10, end: 20 });
        folds.map_through(&Edit::insert(0, "abc"));
        assert_eq!(folds.iter().next(), Some(&Fold { start: 13, end: 23 }));
        folds.map_through(&Edit::insert(13, "x"));
        assert_eq!(folds.iter().next(), Some(&Fold { start: 14, end: 24 }), "typing on the header");
        folds.map_through(&Edit::delete(15, 16));
        assert_eq!(folds.iter().next(), None, "an edit out of sight shows the text");
    }

    #[test]
    fn a_caret_inside_opens_the_fold_and_one_on_the_header_does_not() {
        let mut folds = Folds::default();
        folds.add(Fold { start: 10, end: 20 });
        assert!(!folds.reveal(&[3, 10, 21]));
        assert!(folds.reveal(&[3, 15]));
        assert_eq!(folds.iter().count(), 0);
    }
}
