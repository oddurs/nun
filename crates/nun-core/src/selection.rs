//! The plural selection model.
//!
//! Selections are plural from the first commit even though the UI starts with
//! one caret. A single caret is the degenerate case of one empty range, so
//! there is never a separate code path for it — which is what stops multi-cursor
//! from being a rewrite of every edit path later.

use crate::edit::{Assoc, Edit};

/// One selection: an `anchor` that stays put and a `head` that moves.
///
/// The two may be in either order; `head < anchor` simply means the selection
/// was made backwards, and that direction is preserved through edits so that
/// extending it later grows from the end the user is actually dragging.
#[derive(Debug, Clone, Copy)]
pub struct Range {
    /// The fixed end.
    pub anchor: usize,
    /// The moving end, and where the caret is drawn.
    pub head: usize,
    /// Display column to aim for during vertical movement.
    ///
    /// Derived UI state rather than identity: two ranges covering the same text
    /// compare equal regardless of it.
    pub sticky: Option<usize>,
}

impl PartialEq for Range {
    /// Compares the covered text only. `sticky` is derived state.
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.head == other.head
    }
}

impl Eq for Range {}

impl Range {
    /// A range from `anchor` to `head`.
    #[must_use]
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, sticky: None }
    }

    /// An empty range — a bare caret — at `at`.
    #[must_use]
    pub const fn caret(at: usize) -> Self {
        Self { anchor: at, head: at, sticky: None }
    }

    /// The lower bound.
    #[must_use]
    pub const fn from(&self) -> usize {
        if self.anchor < self.head { self.anchor } else { self.head }
    }

    /// The upper bound.
    #[must_use]
    pub const fn to(&self) -> usize {
        if self.anchor < self.head { self.head } else { self.anchor }
    }

    /// True when nothing is selected and this is just a caret.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Number of chars covered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.to() - self.from()
    }

    /// True when the two ranges touch or overlap and should become one.
    #[must_use]
    pub const fn merges_with(&self, other: &Self) -> bool {
        self.to() >= other.from() && other.to() >= self.from()
    }

    /// Move the head, keeping the anchor.
    #[must_use]
    pub const fn with_head(self, head: usize) -> Self {
        Self { anchor: self.anchor, head, sticky: None }
    }

    /// Collapse to a caret at the head.
    #[must_use]
    pub const fn collapsed(self) -> Self {
        Self { anchor: self.head, head: self.head, sticky: self.sticky }
    }

    /// Map both ends through an edit.
    ///
    /// Both ends use [`Assoc::After`], so text typed at a caret leaves the caret
    /// after it, and a range whose contents were deleted collapses to the point
    /// where the text used to be.
    #[must_use]
    pub fn mapped(self, edit: &Edit) -> Self {
        Self {
            anchor: edit.map_pos(self.anchor, Assoc::After),
            head: edit.map_pos(self.head, Assoc::After),
            sticky: None,
        }
    }
}

/// Every selection in a buffer, with one marked primary.
///
/// Always non-empty, always sorted by lower bound, and always disjoint —
/// overlapping ranges are merged on construction and after every edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Range>,
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self::single(Range::caret(0))
    }
}

impl Selections {
    /// One selection.
    #[must_use]
    pub fn single(range: Range) -> Self {
        Self { ranges: vec![range], primary: 0 }
    }

    /// Several selections, normalised.
    ///
    /// `primary` indexes `ranges` before normalisation; if that range is merged
    /// away, the range that absorbed it becomes primary.
    ///
    /// # Panics
    ///
    /// Panics if `ranges` is empty. A buffer always has at least one caret.
    #[must_use]
    pub fn new(ranges: Vec<Range>, primary: usize) -> Self {
        assert!(!ranges.is_empty(), "a buffer always has at least one selection");
        let mut this = Self { ranges, primary };
        this.normalize();
        this
    }

    /// The selections, sorted and disjoint.
    #[must_use]
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// How many selections there are; never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Always false — kept so the type reads like a collection.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The selection the user is driving.
    #[must_use]
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    /// Index of the primary selection.
    #[must_use]
    pub const fn primary_index(&self) -> usize {
        self.primary
    }

    /// Replace the primary selection.
    pub fn set_primary(&mut self, range: Range) {
        self.ranges[self.primary] = range;
        self.normalize();
    }

    /// Collapse to the primary selection alone.
    pub fn collapse_to_primary(&mut self) {
        let primary = self.primary();
        self.ranges = vec![primary];
        self.primary = 0;
    }

    /// Replace every range by passing it through `f`.
    pub fn transform(&mut self, mut f: impl FnMut(Range) -> Range) {
        for range in &mut self.ranges {
            *range = f(*range);
        }
        self.normalize();
    }

    /// Map every selection through an edit and re-merge any that collided.
    pub fn map_through(&mut self, edit: &Edit) {
        for range in &mut self.ranges {
            *range = range.mapped(edit);
        }
        self.normalize();
    }

    /// Sort, merge touching ranges, and keep the primary pointing at something.
    ///
    /// Merging is why `map_through` cannot simply map in place: two carets a
    /// character apart become one after the character between them is deleted,
    /// and leaving both would duplicate every subsequent edit.
    fn normalize(&mut self) {
        debug_assert!(!self.ranges.is_empty());
        let primary = self.ranges[self.primary.min(self.ranges.len() - 1)];

        self.ranges.sort_by_key(Range::from);

        let mut merged: Vec<Range> = Vec::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            match merged.last_mut() {
                Some(last) if last.merges_with(&range) => {
                    // Keep the direction of the range that reaches furthest, so
                    // a backwards drag that swallows a caret stays backwards.
                    let from = last.from().min(range.from());
                    let to = last.to().max(range.to());
                    *last = if range.head < range.anchor {
                        Range { anchor: to, head: from, sticky: range.sticky }
                    } else {
                        Range { anchor: from, head: to, sticky: range.sticky }
                    };
                }
                _ => merged.push(range),
            }
        }
        self.ranges = merged;

        self.primary = self
            .ranges
            .iter()
            .position(|r| r.from() <= primary.from() && r.to() >= primary.to())
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_sorted_on_construction() {
        let s = Selections::new(vec![Range::new(10, 12), Range::new(2, 4)], 0);
        assert_eq!(s.ranges(), &[Range::new(2, 4), Range::new(10, 12)]);
    }

    #[test]
    fn overlapping_ranges_merge() {
        let s = Selections::new(vec![Range::new(0, 5), Range::new(3, 9)], 0);
        assert_eq!(s.len(), 1);
        assert_eq!(s.ranges()[0], Range::new(0, 9));
    }

    #[test]
    fn touching_ranges_merge() {
        let s = Selections::new(vec![Range::new(0, 3), Range::new(3, 6)], 0);
        assert_eq!(s.len(), 1, "ranges that share an endpoint are one selection");
    }

    #[test]
    fn merging_keeps_the_primary_alive() {
        let s = Selections::new(vec![Range::new(0, 2), Range::new(1, 8)], 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s.primary(), Range::new(0, 8));
    }

    #[test]
    fn a_backwards_range_stays_backwards_through_a_merge() {
        let s = Selections::new(vec![Range::new(2, 0), Range::new(1, 5)], 0);
        let merged = s.ranges()[0];
        assert_eq!(merged.from(), 0);
        assert_eq!(merged.to(), 5);
    }

    #[test]
    fn sticky_column_is_not_part_of_identity() {
        let a = Range { anchor: 1, head: 4, sticky: Some(7) };
        let b = Range { anchor: 1, head: 4, sticky: None };
        assert_eq!(a, b);
    }

    #[test]
    fn carets_that_collide_after_a_delete_become_one() {
        let mut s = Selections::new(vec![Range::caret(3), Range::caret(4)], 0);
        assert_eq!(s.len(), 2);
        s.map_through(&Edit::delete(3, 4));
        assert_eq!(s.len(), 1, "deleting the char between two carets merges them");
        assert_eq!(s.ranges()[0], Range::caret(3));
    }

    #[test]
    fn an_insert_pushes_later_selections_along() {
        let mut s = Selections::new(vec![Range::caret(0), Range::caret(10)], 0);
        s.map_through(&Edit::insert(0, "abc"));
        assert_eq!(s.ranges()[1], Range::caret(13));
    }
}
