//! Undo and redo as a linear stack of reversible revisions.
//!
//! A revision stores the edits needed to go each way plus the selections on
//! either side, so undo restores the caret as well as the text. Both edit lists
//! are kept sorted descending by start: applying highest-first means an edit
//! never disturbs the indices of one still to come.

use crate::edit::Edit;
use crate::selection::Selections;

/// One undoable step.
#[derive(Debug, Clone)]
pub(crate) struct Revision {
    /// Apply these, highest start first, to undo.
    pub inverse: Vec<Edit>,
    /// Apply these, highest start first, to redo.
    pub forward: Vec<Edit>,
    /// Selections as they were before.
    pub before: Selections,
    /// Selections as they were after.
    pub after: Selections,
    /// Whether a further edit may still be folded into this revision.
    pub open: bool,
}

/// The undo stack.
///
/// `cursor` counts how many revisions are currently applied, so undo and redo
/// are just moving it and replaying the matching edit list.
#[derive(Debug, Clone, Default)]
pub struct History {
    revisions: Vec<Revision>,
    cursor: usize,
}

impl History {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub const fn can_redo(&self) -> bool {
        self.cursor < self.revisions.len()
    }

    /// How many revisions are currently applied.
    ///
    /// Used as a save marker: a buffer is unmodified when this matches what it
    /// was when the file was last written, which correctly reports a buffer
    /// undone back to its saved state as clean.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.cursor
    }

    /// Close the open revision so the next edit starts a new undo step.
    ///
    /// Called on cursor movement, save, and focus change — the moments a person
    /// would expect their next undo to stop at.
    pub fn commit(&mut self) {
        if let Some(last) = self.revisions.last_mut() {
            last.open = false;
        }
    }

    /// Record a revision, discarding any redo branch.
    pub(crate) fn push(&mut self, revision: Revision) {
        self.revisions.truncate(self.cursor);
        self.revisions.push(revision);
        self.cursor = self.revisions.len();
    }

    /// The open revision at the tip, if there is one.
    pub(crate) fn open_tip(&mut self) -> Option<&mut Revision> {
        if self.cursor == self.revisions.len() {
            self.revisions.last_mut().filter(|r| r.open)
        } else {
            None
        }
    }

    /// Step back, yielding the revision to reverse.
    pub(crate) fn step_back(&mut self) -> Option<Revision> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        self.revisions[self.cursor].open = false;
        Some(self.revisions[self.cursor].clone())
    }

    /// Step forward, yielding the revision to replay.
    pub(crate) fn step_forward(&mut self) -> Option<Revision> {
        if !self.can_redo() {
            return None;
        }
        let revision = self.revisions[self.cursor].clone();
        self.cursor += 1;
        Some(revision)
    }
}
