//! The document: text, selections, and undo in one owner.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ropey::Rope;

use crate::edit::Edit;
use crate::grapheme;
use crate::history::{History, Revision};
use crate::selection::{Range, Selections};
use crate::text::{BOM, LineEnding, LoadReport};

/// Default columns a tab advances to.
const DEFAULT_TAB_WIDTH: usize = 4;

/// Why a save could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The buffer has no path and none was supplied.
    #[error("buffer has no path; supply one with `save_as`")]
    NoPath,

    /// The file changed on disk since it was read.
    ///
    /// Overwriting would silently discard whatever made the change, so the save
    /// is refused and the caller has to decide.
    #[error("{path} changed on disk since it was read")]
    ChangedOnDisk {
        /// The file that moved underneath us.
        path: PathBuf,
    },

    /// The underlying filesystem operation failed.
    #[error("writing {path}: {source}")]
    Io {
        /// The file being written.
        path: PathBuf,
        /// The failure.
        #[source]
        source: io::Error,
    },
}

/// What the file looked like when it was read, so a change can be spotted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl DiskStamp {
    fn of(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self { len: meta.len(), modified: meta.modified().ok() })
    }
}

/// A text document: the rope, the selections into it, and its undo history.
///
/// Text is held with `\n` line endings regardless of what the file uses, so
/// every index calculation has one shape. The original ending and any
/// byte-order mark are reapplied on save.
/// What has happened to the text since somebody last asked.
///
/// The three cases are genuinely different to whoever is following along: one
/// edit can be replayed, several cannot be described as one, and nothing at
/// all means there is no work to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Changed {
    /// The text has not changed.
    #[default]
    Nothing,
    /// Exactly one edit, which can be followed.
    One(Change),
    /// Several edits, an undo, or a redo: start again.
    Several,
}

/// One change to the text, as offsets a parser can follow.
///
/// Char indices, like everything else here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Where the change starts.
    pub start: usize,
    /// Where it ended before.
    pub old_end: usize,
    /// Where it ends now.
    pub new_end: usize,
}

#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    selections: Selections,
    history: History,
    line_ending: LineEnding,
    had_bom: bool,
    lossy: bool,
    path: Option<PathBuf>,
    saved_at: usize,
    stamp: Option<DiskStamp>,
    tab_width: usize,
    /// What has happened since it was last taken.
    change: Changed,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    /// An empty buffer with no path.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            selections: Selections::default(),
            history: History::new(),
            line_ending: LineEnding::default(),
            had_bom: false,
            lossy: false,
            path: None,
            saved_at: 0,
            stamp: None,
            tab_width: DEFAULT_TAB_WIDTH,
            change: Changed::Nothing,
        }
    }

    /// A buffer holding `text`, with line endings detected from it.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let (buffer, _) = Self::from_bytes(text.as_bytes());
        buffer
    }

    /// A buffer holding `bytes`, decoded as UTF-8.
    ///
    /// Invalid sequences are replaced rather than rejected, and the fact is
    /// reported in [`LoadReport::lossy`] so the caller can refuse to save over
    /// the original.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> (Self, LoadReport) {
        let decoded = String::from_utf8_lossy(bytes);
        let lossy = matches!(decoded, std::borrow::Cow::Owned(_));

        let had_bom = decoded.starts_with(BOM);
        let text = if had_bom { &decoded[BOM.len()..] } else { &decoded[..] };

        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        let line_ending = LineEnding::detect(text);
        let mixed = crlf > 0 && lf > 0;

        // Normalise to `\n` so no grapheme cluster ever spans a line break and
        // every offset calculation has a single shape.
        let normalised = if crlf > 0 { text.replace("\r\n", "\n") } else { text.to_string() };

        let report = LoadReport { had_bom, lossy, mixed_line_endings: mixed, line_ending };
        let buffer =
            Self { rope: Rope::from_str(&normalised), had_bom, lossy, line_ending, ..Self::new() };
        (buffer, report)
    }

    /// Read a file into a buffer.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] if the file cannot be read.
    pub fn load(path: impl AsRef<Path>) -> io::Result<(Self, LoadReport)> {
        let path = path.as_ref();
        let bytes = fs::read(path)?;
        let (mut buffer, report) = Self::from_bytes(&bytes);
        buffer.stamp = DiskStamp::of(path).ok();
        buffer.path = Some(path.to_path_buf());
        Ok((buffer, report))
    }

    /// The text, for reading.
    #[must_use]
    pub const fn text(&self) -> &Rope {
        &self.rope
    }

    /// Total chars.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Total lines. An empty buffer has one.
    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// The path this buffer came from, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Give the buffer a path to save to.
    ///
    /// Used when opening a file that does not exist yet: there is nothing to
    /// read, but `save` still needs somewhere to write. No disk stamp is
    /// recorded, so the first save will not complain that the file changed.
    pub fn set_path(&mut self, path: impl AsRef<Path>) {
        self.path = Some(path.as_ref().to_path_buf());
    }

    /// The line ending that will be written on save.
    #[must_use]
    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the file was decoded lossily and cannot be safely written back.
    #[must_use]
    pub const fn is_lossy(&self) -> bool {
        self.lossy
    }

    /// Whether there are unsaved changes.
    #[must_use]
    pub const fn is_modified(&self) -> bool {
        self.history.position() != self.saved_at
    }

    /// Columns a tab advances to.
    #[must_use]
    pub const fn tab_width(&self) -> usize {
        self.tab_width
    }

    /// Set the columns a tab advances to.
    pub const fn set_tab_width(&mut self, width: usize) {
        self.tab_width = width;
    }

    /// The text itself, for anything that needs to read all of it.
    ///
    /// Cloning a rope is cheap — the two share their structure — which is how
    /// a snapshot reaches the parser without copying the document.
    #[must_use]
    pub const fn rope(&self) -> &Rope {
        &self.rope
    }

    /// What has happened to the text since this was last asked.
    ///
    /// A caller that needs to follow along — the parser does — replays one
    /// edit and starts again for anything else.
    pub fn take_change(&mut self) -> Changed {
        std::mem::take(&mut self.change)
    }

    /// The current selections.
    #[must_use]
    pub const fn selections(&self) -> &Selections {
        &self.selections
    }

    /// Replace the selections, ending the open undo group.
    ///
    /// Moving the caret is a natural place for undo to stop, which is why this
    /// commits rather than leaving the group open.
    pub fn set_selections(&mut self, selections: Selections) {
        self.selections = selections;
        self.history.commit();
    }

    /// End the open undo group so the next edit starts a new one.
    pub fn commit_undo_group(&mut self) {
        self.history.commit();
    }

    // ── text queries ────────────────────────────────────────────────────────

    /// Line containing char index `char_idx`.
    ///
    /// # Panics
    ///
    /// Panics if `char_idx` is past the end of the buffer.
    #[must_use]
    pub fn line_of(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx)
    }

    /// First char index of `line`.
    ///
    /// # Panics
    ///
    /// Panics if `line` is out of range.
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line)
    }

    /// Char index just past the last visible char of `line`, excluding its newline.
    #[must_use]
    pub fn line_end(&self, line: usize) -> usize {
        let start = self.line_start(line);
        let slice = self.rope.line(line);
        let len = slice.len_chars();
        // `Rope::line` includes the trailing newline where there is one.
        if len > 0 && slice.char(len - 1) == '\n' { start + len - 1 } else { start + len }
    }

    /// The text of `line`, including any trailing newline.
    #[must_use]
    pub fn line_text(&self, line: usize) -> String {
        self.rope.line(line).to_string()
    }

    /// Display column of a char index.
    #[must_use]
    pub fn column_of(&self, char_idx: usize) -> usize {
        let line = self.line_of(char_idx);
        let text = self.line_text(line);
        grapheme::width_to(&text, char_idx - self.line_start(line), self.tab_width)
    }

    /// The whole buffer as it would be written to disk.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        if self.had_bom {
            out.push_str(BOM);
        }
        let text = self.rope.to_string();
        match self.line_ending {
            LineEnding::Lf => out.push_str(&text),
            LineEnding::Crlf => out.push_str(&text.replace('\n', "\r\n")),
        }
        out.into_bytes()
    }

    // ── editing ─────────────────────────────────────────────────────────────

    /// Apply one edit to the rope and return the edit that reverses it.
    fn apply_to_rope(&mut self, edit: &Edit) -> Edit {
        let removed = self.rope.slice(edit.start..edit.end).to_string();
        if edit.start != edit.end {
            self.rope.remove(edit.start..edit.end);
        }
        if !edit.text.is_empty() {
            self.rope.insert(edit.start, &edit.text);
        }
        Edit::replace(edit.start, edit.end_after(), removed)
    }

    /// Apply a set of disjoint edits as one undoable revision.
    ///
    /// Edits are applied highest-start-first so that each one's indices are
    /// still valid when it runs. Selections are mapped through every edit.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if two edits overlap.
    pub fn edit(&mut self, mut edits: Vec<Edit>) {
        edits.retain(|e| !e.is_noop());
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|e| std::cmp::Reverse(e.start));
        debug_assert!(
            edits.windows(2).all(|w| w[1].end <= w[0].start),
            "edits in one revision must be disjoint"
        );

        // One edit can be followed through a reparse; anything else means the
        // follower has to start again, and saying so is cheaper than being
        // subtly wrong about where the text moved.
        let single = if edits.len() == 1 {
            let edit = &edits[0];
            Changed::One(Change { start: edit.start, old_end: edit.end, new_end: edit.end_after() })
        } else {
            Changed::Several
        };
        self.change = match (self.change, single) {
            (Changed::Nothing, change) => change,
            _ => Changed::Several,
        };

        let before = self.selections.clone();

        let mut inverse = Vec::with_capacity(edits.len());
        for edit in &edits {
            inverse.push(self.apply_to_rope(edit));
        }
        // Each inverse was taken in the coordinates of the text before the
        // whole revision, but undo applies it to the text after it — where
        // every edit below this one has already grown or shrunk what precedes
        // it. Shift each by the net change of the edits beneath it, so undo can
        // replay them highest-first just as the forward edits were applied.
        let mut shift: isize = 0;
        for (edit, inverse) in edits.iter().zip(inverse.iter_mut()).rev() {
            inverse.start = inverse.start.saturating_add_signed(shift);
            inverse.end = inverse.end.saturating_add_signed(shift);
            shift += edit.inserted().cast_signed() - edit.removed().cast_signed();
        }
        for edit in &edits {
            self.selections.map_through(edit);
        }

        let after = self.selections.clone();
        self.record(edits, inverse, before, after);
    }

    /// Fold into the open revision when this continues a run of typing or
    /// backspacing, otherwise start a new one.
    fn record(
        &mut self,
        forward: Vec<Edit>,
        inverse: Vec<Edit>,
        before: Selections,
        after: Selections,
    ) {
        if self.try_coalesce(&forward, &inverse, &after) {
            return;
        }
        self.history.push(Revision { inverse, forward, before, after, open: true });
    }

    /// Widen the open revision to cover this edit too, if the two are a run.
    ///
    /// Both lists are rewritten relative to the state before the whole
    /// revision, which is the only frame in which undo later replays them.
    fn try_coalesce(&mut self, forward: &[Edit], inverse: &[Edit], after: &Selections) -> bool {
        if forward.len() != 1 || inverse.len() != 1 {
            return false;
        }
        let Some(tip) = self.history.open_tip() else { return false };
        if tip.forward.len() != 1 || tip.inverse.len() != 1 {
            return false;
        }

        let (previous, previous_inverse) = (tip.forward[0].clone(), tip.inverse[0].clone());
        let (next, next_inverse) = (forward[0].clone(), inverse[0].clone());

        // Typing forward: the new insert begins exactly where the last one
        // ended. A newline ends the run, so undo stops at line boundaries.
        let insert_run = previous.removed() == 0
            && next.removed() == 0
            && next.start == previous.end_after()
            && !previous.text.contains('\n')
            && !next.text.contains('\n');

        if insert_run {
            let start = previous.start;
            tip.forward[0] = Edit::insert(start, format!("{}{}", previous.text, next.text));
            tip.inverse[0] = Edit::delete(start, tip.forward[0].end_after());
            tip.after = after.clone();
            return true;
        }

        // Backspacing: the new delete ends exactly where the last one began, so
        // together they remove one contiguous span.
        let delete_run = previous.text.is_empty()
            && next.text.is_empty()
            && next.end == previous.start
            && !previous_inverse.text.contains('\n')
            && !next_inverse.text.contains('\n');

        if delete_run {
            let start = next.start;
            tip.forward[0] = Edit::delete(start, previous.end);
            tip.inverse[0] = Edit::replace(
                start,
                start,
                format!("{}{}", next_inverse.text, previous_inverse.text),
            );
            tip.after = after.clone();
            return true;
        }

        false
    }

    /// Insert `text` at every selection, replacing anything selected.
    pub fn insert(&mut self, text: &str) {
        let edits: Vec<Edit> = self
            .selections
            .ranges()
            .iter()
            .map(|r| Edit::replace(r.from(), r.to(), text))
            .collect();
        self.edit(edits);
    }

    /// Delete the selection, or one grapheme before the caret when empty.
    pub fn delete_backward(&mut self) {
        let edits: Vec<Edit> = self
            .selections
            .ranges()
            .iter()
            .filter_map(|r| {
                if r.is_empty() {
                    let head = r.head;
                    if head == 0 {
                        return None;
                    }
                    Some(Edit::delete(self.prev_grapheme(head), head))
                } else {
                    Some(Edit::delete(r.from(), r.to()))
                }
            })
            .collect();
        self.edit(merge_deletes(edits));
    }

    /// Delete the selection, or one grapheme after the caret when empty.
    pub fn delete_forward(&mut self) {
        let len = self.len_chars();
        let edits: Vec<Edit> = self
            .selections
            .ranges()
            .iter()
            .filter_map(|r| {
                if r.is_empty() {
                    let head = r.head;
                    if head >= len {
                        return None;
                    }
                    Some(Edit::delete(head, self.next_grapheme(head)))
                } else {
                    Some(Edit::delete(r.from(), r.to()))
                }
            })
            .collect();
        self.edit(merge_deletes(edits));
    }

    /// Reverse the most recent revision. Returns false when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(revision) = self.history.step_back() else { return false };
        self.change = Changed::Several;
        for edit in &revision.inverse {
            self.apply_to_rope(edit);
        }
        self.selections = revision.before;
        true
    }

    /// Replay the next revision. Returns false when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(revision) = self.history.step_forward() else { return false };
        self.change = Changed::Several;
        for edit in &revision.forward {
            self.apply_to_rope(edit);
        }
        self.selections = revision.after;
        true
    }

    // ── movement ────────────────────────────────────────────────────────────

    /// Char index of the grapheme boundary before `char_idx`, crossing lines.
    #[must_use]
    pub fn prev_grapheme(&self, char_idx: usize) -> usize {
        if char_idx == 0 {
            return 0;
        }
        let line = self.line_of(char_idx);
        let start = self.line_start(line);
        if char_idx == start {
            // At the head of a line, step back over the newline itself.
            return char_idx - 1;
        }
        let text = self.line_text(line);
        start + grapheme::prev_boundary(&text, char_idx - start)
    }

    /// Char index of the grapheme boundary after `char_idx`, crossing lines.
    #[must_use]
    pub fn next_grapheme(&self, char_idx: usize) -> usize {
        let len = self.len_chars();
        if char_idx >= len {
            return len;
        }
        let line = self.line_of(char_idx);
        let start = self.line_start(line);
        let text = self.line_text(line);
        (start + grapheme::next_boundary(&text, char_idx - start)).min(len)
    }

    /// Move every caret one grapheme left, extending the selection if asked.
    pub fn move_left(&mut self, extend: bool) {
        self.move_horizontal(extend, true);
    }

    /// Move every caret one grapheme right, extending the selection if asked.
    pub fn move_right(&mut self, extend: bool) {
        self.move_horizontal(extend, false);
    }

    fn move_horizontal(&mut self, extend: bool, left: bool) {
        let positions: Vec<usize> = self
            .selections
            .ranges()
            .iter()
            .map(|r| {
                // A non-empty selection collapses to its edge rather than
                // moving, which is what every non-modal editor does.
                if !extend && !r.is_empty() {
                    if left { r.from() } else { r.to() }
                } else if left {
                    self.prev_grapheme(r.head)
                } else {
                    self.next_grapheme(r.head)
                }
            })
            .collect();

        let mut index = 0;
        self.selections.transform(|r| {
            let head = positions[index];
            index += 1;
            if extend { r.with_head(head) } else { Range::caret(head) }
        });
        self.history.commit();
    }

    /// Move every caret one line up, extending the selection if asked.
    pub fn move_up(&mut self, extend: bool) {
        self.move_vertical(extend, true);
    }

    /// Move every caret one line down, extending the selection if asked.
    pub fn move_down(&mut self, extend: bool) {
        self.move_vertical(extend, false);
    }

    fn move_vertical(&mut self, extend: bool, up: bool) {
        let last_line = self.len_lines() - 1;
        let moved: Vec<(usize, usize)> = self
            .selections
            .ranges()
            .iter()
            .map(|r| {
                let line = self.line_of(r.head);
                // The column the caret is aiming for survives crossing short
                // lines, which is why it is remembered rather than recomputed.
                let goal = r.sticky.unwrap_or_else(|| self.column_of(r.head));
                let target = if up { line.saturating_sub(1) } else { (line + 1).min(last_line) };
                let text = self.line_text(target);
                let offset = grapheme::char_off_at_width(&text, goal, self.tab_width);
                (self.line_start(target) + offset, goal)
            })
            .collect();

        let mut index = 0;
        self.selections.transform(|r| {
            let (head, goal) = moved[index];
            index += 1;
            let mut next = if extend { r.with_head(head) } else { Range::caret(head) };
            next.sticky = Some(goal);
            next
        });
        self.history.commit();
    }

    /// Move every caret to the first char of its line.
    pub fn move_line_start(&mut self, extend: bool) {
        let targets: Vec<usize> = self
            .selections
            .ranges()
            .iter()
            .map(|r| self.line_start(self.line_of(r.head)))
            .collect();
        self.move_to(&targets, extend);
    }

    /// Move every caret to the end of its line, before the newline.
    pub fn move_line_end(&mut self, extend: bool) {
        let targets: Vec<usize> =
            self.selections.ranges().iter().map(|r| self.line_end(self.line_of(r.head))).collect();
        self.move_to(&targets, extend);
    }

    fn move_to(&mut self, targets: &[usize], extend: bool) {
        let mut index = 0;
        self.selections.transform(|r| {
            let head = targets[index];
            index += 1;
            if extend { r.with_head(head) } else { Range::caret(head) }
        });
        self.history.commit();
    }

    /// Select the whole buffer.
    pub fn select_all(&mut self) {
        let len = self.len_chars();
        self.selections = Selections::single(Range::new(0, len));
        self.history.commit();
    }

    // ── several carets ──────────────────────────────────────────────────────

    /// Add a caret on the line above every selection, or below.
    ///
    /// Each new caret lands on the column its selection is aiming for, so a
    /// column of carets walked down past short lines comes out straight again
    /// rather than collapsing against the ragged edge.
    ///
    /// Nothing is added for a selection already on the first or last line;
    /// pressing the key at the top of a file should not stack carets on each
    /// other.
    pub fn add_caret_vertically(&mut self, up: bool) {
        let last_line = self.len_lines() - 1;
        let mut ranges: Vec<Range> = self.selections.ranges().to_vec();
        let mut added: Vec<Range> = Vec::new();
        for range in self.selections.ranges() {
            let line = self.line_of(range.head);
            if (up && line == 0) || (!up && line == last_line) {
                continue;
            }
            let target = if up { line - 1 } else { line + 1 };
            let goal = range.sticky.unwrap_or_else(|| self.column_of(range.head));
            let text = self.line_text(target);
            let offset = grapheme::char_off_at_width(&text, goal, self.tab_width);
            let mut caret = Range::caret(self.line_start(target) + offset);
            caret.sticky = Some(goal);
            added.push(caret);
        }
        if added.is_empty() {
            return;
        }
        // The newest caret is the one being driven, so pressing again carries
        // on from where it got to.
        let primary = ranges.len() + added.len() - 1;
        ranges.append(&mut added);
        self.selections = Selections::new(ranges, primary);
        self.history.commit();
    }

    /// Select the word the primary caret is in, or — when it already covers
    /// something — add the next occurrence of that text and drive it.
    ///
    /// Returns whether anything happened, so a keystroke that found nothing
    /// can say so rather than looking like it worked.
    pub fn add_next_occurrence(&mut self) -> bool {
        let primary = self.selections.primary();
        if primary.from() == primary.to() {
            let (from, to) = self.word_range(primary.head);
            if from == to {
                return false;
            }
            self.selections.set_primary(Range::new(from, to));
            self.history.commit();
            return true;
        }

        let Some(next) = self.occurrence_after(primary) else { return false };
        let mut ranges: Vec<Range> = self.selections.ranges().to_vec();
        if ranges.contains(&next) {
            // Every occurrence is taken already; wrapping onto one of them
            // would move the primary around for ever and add nothing.
            return false;
        }
        ranges.push(next);
        let primary = ranges.len() - 1;
        self.selections = Selections::new(ranges, primary);
        self.history.commit();
        true
    }

    /// Select every occurrence of what the primary selection covers.
    ///
    /// From a bare caret the word it sits in is selected first, so one press
    /// does what two would.
    pub fn add_all_occurrences(&mut self) -> bool {
        if self.selections.primary().from() == self.selections.primary().to()
            && !self.add_next_occurrence()
        {
            return false;
        }
        let primary = self.selections.primary();
        let needle = self.slice(primary.from(), primary.to());
        if needle.is_empty() {
            return false;
        }
        let text = self.rope.to_string();
        let mut ranges: Vec<Range> = Vec::new();
        let mut at = 0;
        while let Some(found) = text[at..].find(&needle) {
            let byte = at + found;
            let from = self.rope.byte_to_char(byte);
            ranges.push(Range::new(from, from + needle.chars().count()));
            at = byte + needle.len();
        }
        if ranges.is_empty() {
            return false;
        }
        // Whichever was being driven stays the one being driven.
        let primary = ranges.iter().position(|range| *range == primary).unwrap_or(0);
        self.selections = Selections::new(ranges, primary);
        self.history.commit();
        true
    }

    /// Turn every selection that spans lines into one selection per line.
    ///
    /// A selection within one line is left alone: there is nothing to split,
    /// and replacing it with itself would only look like the key had failed.
    pub fn split_into_lines(&mut self) -> bool {
        let mut ranges: Vec<Range> = Vec::new();
        let mut split = false;
        for range in self.selections.ranges() {
            let (from, to) = (range.from(), range.to());
            let (first, last) = (self.line_of(from), self.line_of(to));
            if first == last {
                ranges.push(*range);
                continue;
            }
            split = true;
            for line in first..=last {
                let start = self.line_start(line).max(from);
                let end = self.line_end(line).min(to);
                if start <= end {
                    ranges.push(Range::new(start, end));
                }
            }
        }
        if !split {
            return false;
        }
        self.selections = Selections::new(ranges, 0);
        self.history.commit();
        true
    }

    /// The first occurrence of what `range` covers that starts after it,
    /// wrapping back to the start of the buffer.
    fn occurrence_after(&self, range: Range) -> Option<Range> {
        let needle = self.slice(range.from(), range.to());
        if needle.is_empty() {
            return None;
        }
        let text = self.rope.to_string();
        let after = self.rope.char_to_byte(range.to());
        let found = text[after..]
            .find(&needle)
            .map(|at| after + at)
            .or_else(|| text[..after].find(&needle))?;
        let from = self.rope.byte_to_char(found);
        Some(Range::new(from, from + needle.chars().count()))
    }

    /// The text between two char indices.
    fn slice(&self, from: usize, to: usize) -> String {
        let end = to.min(self.len_chars());
        self.rope.slice(from.min(end)..end).to_string()
    }

    // ── pointer selection ───────────────────────────────────────────────────

    /// The word, space run or symbol run under `char_idx`, as `(start, end)`:
    /// what a double-click selects.
    ///
    /// Word boundaries for code: `snake_case` and `größe` are one word each,
    /// `self.value` is three pieces, `::` is one, and an emoji sequence is
    /// one. The newline is never part of a word.
    ///
    /// # Panics
    ///
    /// Panics if `char_idx` is past the end of the buffer.
    #[must_use]
    pub fn word_range(&self, char_idx: usize) -> (usize, usize) {
        let line = self.line_of(char_idx);
        let start = self.line_start(line);
        let (from, to) = grapheme::word_bounds(&self.line_text(line), char_idx - start);
        (start + from, start + to)
    }

    /// The whole of `line` including its newline, as `(start, end)` — what a
    /// triple-click selects, so that deleting it removes the line rather than
    /// leaving an empty one behind.
    ///
    /// # Panics
    ///
    /// Panics if `line` is out of range.
    #[must_use]
    pub fn line_range(&self, line: usize) -> (usize, usize) {
        let start = self.line_start(line);
        let end =
            if line + 1 < self.len_lines() { self.line_start(line + 1) } else { self.len_chars() };
        (start, end)
    }

    /// Display columns `line` occupies, excluding its newline.
    ///
    /// # Panics
    ///
    /// Panics if `line` is out of range.
    #[must_use]
    pub fn line_width(&self, line: usize) -> usize {
        let text = self.line_text(line);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        grapheme::width_to(text, text.chars().count(), self.tab_width)
    }

    /// The char index on `line` at display column `column`: the start of the
    /// cluster covering it, or the end of the line when the line is shorter.
    ///
    /// # Panics
    ///
    /// Panics if `line` is out of range.
    #[must_use]
    pub fn char_at_column(&self, line: usize, column: usize) -> usize {
        let text = self.line_text(line);
        self.line_start(line) + grapheme::char_off_at_width(&text, column, self.tab_width)
    }

    /// A column (box) selection between two `(line, display column)` corners.
    ///
    /// One range per line, anchored at the anchor's column and heading to the
    /// head's, so the direction of the drag is kept. A line that does not reach
    /// the left edge of the box gets no range at all rather than a caret stuck
    /// at its end — typing into a box must not append to the short lines it
    /// passes over. If no line reaches the box, the anchor's own position is
    /// the single caret, because a buffer always has one.
    ///
    /// The primary is the range on the head's line, where the pointer is.
    ///
    /// # Panics
    ///
    /// Panics if either line is out of range.
    #[must_use]
    pub fn column_selection(&self, anchor: (usize, usize), head: (usize, usize)) -> Selections {
        let (first, last) = (anchor.0.min(head.0), anchor.0.max(head.0));
        let left = anchor.1.min(head.1);
        let zero_width = anchor.1 == head.1;

        let mut ranges = Vec::new();
        let mut primary = 0;
        for line in first..=last {
            // A box with width wants lines that reach into it; a line ending
            // exactly at its left edge would get only a caret at its end. A
            // zero-width box is a column of carets, and a line ending at that
            // column is exactly where one belongs.
            let width = self.line_width(line);
            if width < left || (width == left && !zero_width) {
                continue;
            }
            if line == head.0 || ranges.is_empty() {
                primary = ranges.len();
            }
            ranges.push(Range::new(
                self.char_at_column(line, anchor.1),
                self.char_at_column(line, head.1),
            ));
        }

        if ranges.is_empty() {
            return Selections::single(Range::caret(self.char_at_column(anchor.0, anchor.1)));
        }
        Selections::new(ranges, primary)
    }

    /// Move the text in `from..to` to `dest`, or copy it there, as one undo
    /// step, leaving it selected in its new place.
    ///
    /// Moving text into itself, or to either of its own edges, changes
    /// nothing: there is nowhere for it to go.
    ///
    /// # Panics
    ///
    /// Panics if the range or `dest` is past the end of the buffer.
    pub fn move_text(&mut self, from: usize, to: usize, dest: usize, copy: bool) {
        let (from, to) = (from.min(to), from.max(to));
        if from == to || (!copy && dest >= from && dest <= to) {
            return;
        }
        let text = self.rope.slice(from..to).to_string();
        let len = to - from;

        let mut edits = vec![Edit::insert(dest, text)];
        if !copy {
            edits.push(Edit::delete(from, to));
        }
        // Its own undo step, never folded into typing before or after it.
        self.history.commit();
        self.edit(edits);

        // Where the text landed, in the buffer as it now stands.
        let start = if !copy && dest > to { dest - len } else { dest };
        self.selections = Selections::single(Range::new(start, start + len));
        if let Some(tip) = self.history.open_tip() {
            tip.after = self.selections.clone();
        }
        self.history.commit();
    }

    // ── saving ──────────────────────────────────────────────────────────────

    /// Write the buffer back to its own path.
    ///
    /// # Errors
    ///
    /// [`SaveError::NoPath`] when the buffer has never had a path,
    /// [`SaveError::ChangedOnDisk`] when the file moved underneath us, and
    /// [`SaveError::Io`] for any filesystem failure.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let path = self.path.clone().ok_or(SaveError::NoPath)?;
        self.save_as(&path)
    }

    /// Write the buffer to `path` and adopt it.
    ///
    /// The write is atomic: a temporary file in the same directory is written
    /// and flushed, then renamed over the target, so an interrupted save cannot
    /// truncate the original.
    ///
    /// # Errors
    ///
    /// [`SaveError::ChangedOnDisk`] when writing over the buffer's own path and
    /// that file changed since it was read, and [`SaveError::Io`] for any
    /// filesystem failure.
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<(), SaveError> {
        let path = path.as_ref();
        let io_err = |source: io::Error| SaveError::Io { path: path.to_path_buf(), source };

        // A symlink should be written through, not replaced by a regular file.
        let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        if self.path.as_deref() == Some(path)
            && let (Some(recorded), Ok(current)) = (self.stamp, DiskStamp::of(&target))
            && recorded != current
        {
            return Err(SaveError::ChangedOnDisk { path: target });
        }

        let directory = target.parent().unwrap_or_else(|| Path::new("."));
        let name = target.file_name().map_or_else(|| "nun".into(), std::ffi::OsStr::to_os_string);
        let temporary =
            directory.join(format!(".{}.nun-{}.tmp", name.to_string_lossy(), std::process::id()));

        // Write, flush to the platform, then swap it in.
        {
            use std::io::Write as _;
            let mut file = fs::File::create(&temporary).map_err(io_err)?;
            file.write_all(&self.to_bytes()).map_err(io_err)?;
            file.sync_all().map_err(io_err)?;
        }

        if let Ok(meta) = fs::metadata(&target) {
            // Keep the original mode; a fresh temp file would otherwise take
            // the process umask and quietly change permissions.
            let _ = fs::set_permissions(&temporary, meta.permissions());
        }

        fs::rename(&temporary, &target).map_err(|e| {
            let _ = fs::remove_file(&temporary);
            io_err(e)
        })?;

        self.path = Some(target.clone());
        self.stamp = DiskStamp::of(&target).ok();
        self.saved_at = self.history.position();
        self.history.commit();
        Ok(())
    }
}

/// Union overlapping deletions into one.
///
/// Selections never overlap, but what each one deletes can: a caret on a
/// cluster's far edge deletes the whole cluster, which may reach back over
/// another caret inside it, and a selection ending part-way into a cluster
/// shares it with a caret just after. One edit per
/// span keeps a revision's edits disjoint, which undo depends on.
fn merge_deletes(mut edits: Vec<Edit>) -> Vec<Edit> {
    edits.sort_by_key(|edit| edit.start);
    let mut merged: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match merged.last_mut() {
            Some(last) if edit.start < last.end => last.end = last.end.max(edit.end),
            _ => merged.push(edit),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(buffer: &Buffer) -> String {
        buffer.text().to_string()
    }

    #[test]
    fn inserts_deletes_and_replaces_by_char_index() {
        let mut b = Buffer::from_text("hello world");
        b.edit(vec![Edit::insert(5, ",")]);
        assert_eq!(text_of(&b), "hello, world");
        b.edit(vec![Edit::delete(0, 6)]);
        assert_eq!(text_of(&b), " world");
        b.edit(vec![Edit::replace(1, 6, "there")]);
        assert_eq!(text_of(&b), " there");
    }

    #[test]
    fn disjoint_edits_in_one_revision_all_land() {
        let mut b = Buffer::from_text("a b c");
        b.edit(vec![Edit::replace(0, 1, "X"), Edit::replace(4, 5, "Z")]);
        assert_eq!(text_of(&b), "X b Z");
        assert!(b.undo());
        assert_eq!(text_of(&b), "a b c", "one revision undoes as one unit");
    }

    #[test]
    fn a_run_of_typing_is_one_undo_step() {
        let mut b = Buffer::from_text("");
        for ch in ["h", "e", "l", "l", "o"] {
            b.insert(ch);
        }
        assert_eq!(text_of(&b), "hello");
        assert!(b.undo());
        assert_eq!(text_of(&b), "", "five keystrokes undo together");
        assert!(b.redo());
        assert_eq!(text_of(&b), "hello");
    }

    #[test]
    fn a_newline_breaks_the_typing_run() {
        let mut b = Buffer::from_text("");
        b.insert("ab");
        b.insert("\n");
        b.insert("cd");
        assert!(b.undo());
        assert_eq!(text_of(&b), "ab\n", "the run after the newline undoes alone");
        assert!(b.undo());
        assert_eq!(text_of(&b), "ab");
    }

    #[test]
    fn a_run_of_backspaces_is_one_undo_step() {
        let mut b = Buffer::from_text("hello");
        b.set_selections(Selections::single(Range::caret(5)));
        for _ in 0..3 {
            b.delete_backward();
        }
        assert_eq!(text_of(&b), "he");
        assert!(b.undo());
        assert_eq!(text_of(&b), "hello", "three backspaces undo together");
    }

    #[test]
    fn moving_the_caret_ends_the_undo_group() {
        let mut b = Buffer::from_text("");
        b.insert("ab");
        b.move_left(false);
        b.insert("X");
        assert!(b.undo());
        assert_eq!(text_of(&b), "ab", "the edit after the move undoes alone");
    }

    #[test]
    fn undo_restores_the_selection_as_well_as_the_text() {
        let mut b = Buffer::from_text("hello");
        b.set_selections(Selections::single(Range::new(0, 5)));
        b.insert("X");
        assert_eq!(text_of(&b), "X");
        b.undo();
        assert_eq!(b.selections().primary(), Range::new(0, 5));
    }

    #[test]
    fn redo_is_discarded_by_a_new_edit() {
        let mut b = Buffer::from_text("");
        b.insert("a");
        b.commit_undo_group();
        b.undo();
        b.insert("b");
        assert!(!b.redo(), "the redo branch is gone once history diverges");
        assert_eq!(text_of(&b), "b");
    }

    #[test]
    fn undo_on_an_untouched_buffer_reports_nothing_to_do() {
        let mut b = Buffer::from_text("x");
        assert!(!b.undo());
        assert!(!b.redo());
    }

    #[test]
    fn crlf_is_detected_and_restored_on_write() {
        let (b, report) = Buffer::from_bytes(b"one\r\ntwo\r\n");
        assert_eq!(report.line_ending, LineEnding::Crlf);
        assert_eq!(b.text().to_string(), "one\ntwo\n", "held as LF internally");
        assert_eq!(b.to_bytes(), b"one\r\ntwo\r\n", "written back as CRLF");
    }

    #[test]
    fn mixed_line_endings_are_reported_not_hidden() {
        let (_, report) = Buffer::from_bytes(b"one\r\ntwo\nthree\r\n");
        assert!(report.mixed_line_endings);
        assert_eq!(report.line_ending, LineEnding::Crlf, "the dominant ending wins");
    }

    #[test]
    fn a_lone_cr_is_not_a_line_break() {
        let (b, report) = Buffer::from_bytes(b"one\rtwo");
        assert_eq!(report.line_ending, LineEnding::Lf);
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.to_bytes(), b"one\rtwo", "the carriage return survives");
    }

    #[test]
    fn a_bom_is_preserved_and_kept_out_of_the_text() {
        let (b, report) = Buffer::from_bytes("\u{feff}hi".as_bytes());
        assert!(report.had_bom);
        assert_eq!(b.text().to_string(), "hi", "the mark is not text");
        assert_eq!(b.to_bytes(), "\u{feff}hi".as_bytes(), "and comes back on write");
    }

    #[test]
    fn invalid_utf8_is_flagged_rather_than_silently_accepted() {
        let (b, report) = Buffer::from_bytes(&[0x68, 0x69, 0xff]);
        assert!(report.lossy);
        assert!(b.is_lossy());
    }

    #[test]
    fn an_empty_buffer_has_one_line_and_no_chars() {
        let b = Buffer::new();
        assert_eq!(b.len_chars(), 0);
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.line_end(0), 0);
    }

    #[test]
    fn a_file_without_a_trailing_newline_round_trips() {
        let (b, _) = Buffer::from_bytes(b"no newline");
        assert_eq!(b.to_bytes(), b"no newline");
    }

    #[test]
    fn line_end_stops_before_the_newline() {
        let b = Buffer::from_text("ab\ncd\n");
        assert_eq!(b.line_end(0), 2);
        assert_eq!(b.line_start(1), 3);
        assert_eq!(b.line_end(1), 5);
    }

    // ── movement over real text ─────────────────────────────────────────────

    #[test]
    fn right_arrow_steps_over_a_family_emoji_in_one_go() {
        let mut b = Buffer::from_text("a👨‍👩‍👧b");
        b.set_selections(Selections::single(Range::caret(1)));
        b.move_right(false);
        assert_eq!(b.selections().primary().head, 6, "one stop, not five");
    }

    #[test]
    fn left_arrow_steps_back_over_a_combining_mark() {
        let mut b = Buffer::from_text("e\u{0301}x");
        b.set_selections(Selections::single(Range::caret(2)));
        b.move_left(false);
        assert_eq!(b.selections().primary().head, 0);
    }

    #[test]
    fn backspace_removes_a_whole_cluster() {
        let mut b = Buffer::from_text("e\u{0301}");
        b.set_selections(Selections::single(Range::caret(2)));
        b.delete_backward();
        assert_eq!(text_of(&b), "", "the mark does not survive its base character");
    }

    #[test]
    fn horizontal_movement_crosses_lines() {
        let mut b = Buffer::from_text("ab\ncd");
        b.set_selections(Selections::single(Range::caret(3)));
        b.move_left(false);
        assert_eq!(b.selections().primary().head, 2, "lands on the newline");
        b.move_left(false);
        assert_eq!(b.selections().primary().head, 1);
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut b = Buffer::from_text("ab");
        b.set_selections(Selections::single(Range::caret(0)));
        b.move_left(false);
        assert_eq!(b.selections().primary().head, 0);
        b.set_selections(Selections::single(Range::caret(2)));
        b.move_right(false);
        assert_eq!(b.selections().primary().head, 2);
    }

    #[test]
    fn an_unextended_move_collapses_a_selection_to_its_edge() {
        let mut b = Buffer::from_text("hello");
        b.set_selections(Selections::single(Range::new(1, 4)));
        b.move_left(false);
        assert_eq!(b.selections().primary(), Range::caret(1));
    }

    #[test]
    fn the_sticky_column_survives_a_short_line() {
        let mut b = Buffer::from_text("aaaaaa\nbb\ncccccc");
        b.set_selections(Selections::single(Range::caret(6))); // end of a long line
        b.move_down(false);
        assert_eq!(b.selections().primary().head, 9, "clamped to the short line");
        b.move_down(false);
        assert_eq!(b.column_of(b.selections().primary().head), 6, "and returns to column 6");
    }

    #[test]
    fn vertical_movement_uses_display_columns_not_char_counts() {
        let mut b = Buffer::from_text("日本語\nabcdef");
        b.set_selections(Selections::single(Range::caret(2))); // after two wide chars
        assert_eq!(b.column_of(2), 4);
        b.move_down(false);
        assert_eq!(b.selections().primary().head, 8, "column 4 of the second line");
    }

    #[test]
    fn extending_keeps_the_anchor() {
        let mut b = Buffer::from_text("hello");
        b.set_selections(Selections::single(Range::caret(1)));
        b.move_right(true);
        b.move_right(true);
        assert_eq!(b.selections().primary(), Range::new(1, 3));
    }

    // ── multiple carets ─────────────────────────────────────────────────────

    #[test]
    fn typing_applies_at_every_caret() {
        let mut b = Buffer::from_text("a\nb\nc");
        b.set_selections(Selections::new(
            vec![Range::caret(0), Range::caret(2), Range::caret(4)],
            0,
        ));
        b.insert(">");
        assert_eq!(text_of(&b), ">a\n>b\n>c");
        assert_eq!(b.selections().len(), 3, "every caret survives the edit");
    }

    #[test]
    fn carets_that_collide_merge_rather_than_double_editing() {
        let mut b = Buffer::from_text("ab");
        b.set_selections(Selections::new(vec![Range::caret(1), Range::caret(2)], 0));
        b.delete_backward();
        assert_eq!(text_of(&b), "", "both chars go, neither twice");
        assert_eq!(b.selections().len(), 1);
    }

    #[test]
    fn select_all_covers_the_buffer() {
        let mut b = Buffer::from_text("hello");
        b.select_all();
        assert_eq!(b.selections().primary(), Range::new(0, 5));
    }

    // ── saving ──────────────────────────────────────────────────────────────

    #[test]
    fn modified_tracks_the_saved_position() {
        let mut b = Buffer::from_text("x");
        assert!(!b.is_modified());
        b.insert("y");
        assert!(b.is_modified());
        b.undo();
        assert!(!b.is_modified(), "undone back to the saved state is clean again");
    }
}
