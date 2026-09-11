//! The single unit of mutation.

/// Which side of a replaced range a mapped position sticks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    /// Collapse to the start of the replacement. Used for a selection's tail.
    Before,
    /// Collapse to the end of the replacement. Used for a caret that typed it.
    After,
}

/// Replace the char range `start..end` with `text`.
///
/// An insert is an edit with an empty range; a delete is one with empty text.
/// Having one shape rather than three means undo, selection fixup and the
/// reparse hook each need exactly one code path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Char index where the replaced range begins.
    pub start: usize,
    /// Char index where the replaced range ends; never less than `start`.
    pub end: usize,
    /// Text to put in its place.
    pub text: String,
}

impl Edit {
    /// Insert `text` at char index `at`.
    #[must_use]
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self { start: at, end: at, text: text.into() }
    }

    /// Delete the char range `start..end`.
    #[must_use]
    pub fn delete(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "edit range is inverted");
        Self { start, end, text: String::new() }
    }

    /// Replace the char range `start..end` with `text`.
    #[must_use]
    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        debug_assert!(start <= end, "edit range is inverted");
        Self { start, end, text: text.into() }
    }

    /// Number of chars removed.
    #[must_use]
    pub const fn removed(&self) -> usize {
        self.end - self.start
    }

    /// Number of chars inserted.
    #[must_use]
    pub fn inserted(&self) -> usize {
        self.text.chars().count()
    }

    /// Char index just past the inserted text once this edit has been applied.
    #[must_use]
    pub fn end_after(&self) -> usize {
        self.start + self.inserted()
    }

    /// True when this edit changes nothing.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.start == self.end && self.text.is_empty()
    }

    /// Map a char index from before this edit to after it.
    ///
    /// Positions strictly before the start are unmoved; positions strictly
    /// after the end shift by the net length change. Anything *within* the
    /// replaced range — including either endpoint — has no unambiguous
    /// post-edit position, so it goes to whichever side `assoc` names.
    ///
    /// The endpoints are deliberately included rather than left fixed. A caret
    /// sits exactly at the start of the insert it just produced, and it has to
    /// end up after the typed text rather than in front of it.
    #[must_use]
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        if pos < self.start {
            pos
        } else if pos > self.end {
            // Computed rather than added so a delete longer than `pos` cannot
            // underflow: `pos > self.end >= self.start` holds here.
            pos - self.removed() + self.inserted()
        } else {
            match assoc {
                Assoc::Before => self.start,
                Assoc::After => self.end_after(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_ends_up_after_the_text_it_typed() {
        let edit = Edit::insert(3, "abc");
        assert_eq!(edit.map_pos(3, Assoc::After), 6);
        assert_eq!(edit.map_pos(3, Assoc::Before), 3);
    }

    #[test]
    fn positions_before_an_edit_do_not_move() {
        let edit = Edit::replace(5, 9, "xy");
        assert_eq!(edit.map_pos(0, Assoc::After), 0);
        assert_eq!(edit.map_pos(4, Assoc::After), 4);
    }

    #[test]
    fn positions_after_an_edit_shift_by_the_net_change() {
        let edit = Edit::replace(5, 9, "xy"); // removes 4, inserts 2
        assert_eq!(edit.map_pos(20, Assoc::After), 18);
    }

    #[test]
    fn a_position_inside_a_deleted_span_collapses() {
        let edit = Edit::delete(5, 9);
        assert_eq!(edit.map_pos(7, Assoc::After), 5);
        assert_eq!(edit.map_pos(7, Assoc::Before), 5);
        assert_eq!(edit.map_pos(9, Assoc::After), 5);
    }

    #[test]
    fn a_delete_longer_than_the_position_does_not_underflow() {
        let edit = Edit::delete(0, 10);
        assert_eq!(edit.map_pos(12, Assoc::After), 2);
    }
}
