//! Line hunks between two texts, and what each line of the newer one is.
//!
//! Both sides are the text as the editor holds it: decoded, without a byte
//! order mark, with every line ending a `\n`. The older side comes from git
//! and is decoded with [`nun_core::Buffer::from_bytes`] — the same rules a file
//! is loaded with — so a CRLF file that has not been touched diffs as
//! unchanged, and a line number here is a line number in the buffer.
//!
//! Lines are diffed with the ends they carry, so the last line gaining or
//! losing its newline is a change to that line, as it is to git.

use std::ops::Range;
use std::sync::Arc;

use gix::diff::blob::{Algorithm, Diff as Lines, InternedInput, sources};
use nun_core::{Edit, LineEnding};
use ropey::Rope;

/// One run of changed lines: `before` in the old text became `after` in the
/// new one. Line numbers count from zero; either range can be empty, never
/// both.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hunk {
    /// The lines of the old text it replaces.
    pub before: Range<u32>,
    /// The lines of the new text that replace them.
    pub after: Range<u32>,
}

/// What sort of change a hunk is, for choosing how to mark it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HunkKind {
    /// Lines that were not there before.
    Added,
    /// Lines that replace others.
    Modified,
    /// Lines that are gone, with nothing in their place.
    Removed,
}

impl Hunk {
    /// What sort of change it is.
    #[must_use]
    pub const fn kind(&self) -> HunkKind {
        if self.before.start == self.before.end {
            HunkKind::Added
        } else if self.after.start == self.after.end {
            HunkKind::Removed
        } else {
            HunkKind::Modified
        }
    }

    /// The line a gutter should mark it on. A removal has no lines of its
    /// own, so it is shown on the line that now follows the gap — or on the
    /// last line, when what was removed was the end of the file.
    #[must_use]
    pub const fn anchor(&self, lines: u32) -> u32 {
        if self.after.start < lines || lines == 0 { self.after.start } else { lines - 1 }
    }
}

/// What one line of the new text is, as far as the old one is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineMark {
    /// The line is new.
    Added,
    /// The line replaces one or more old lines.
    Modified,
    /// Old lines were removed between this line and the one above it.
    RemovedAbove,
    /// Old lines were removed after this line, which is the last.
    RemovedBelow,
}

/// The old text of a file, decoded for diffing and remembered for staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    text: String,
    /// Where each line of `text` starts, in bytes, with its length last.
    starts: Vec<usize>,
    ending: LineEnding,
    bom: bool,
    /// Whether the bytes were not UTF-8, so the text is not a faithful copy
    /// and must never be written back.
    lossy: bool,
}

impl Base {
    /// Decode `bytes` the way a buffer decodes a file.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Self {
        let (buffer, report) = nun_core::Buffer::from_bytes(bytes);
        Self::new(buffer.rope().to_string(), report.line_ending, report.had_bom, report.lossy)
    }

    /// A base holding `text` exactly, already decoded. Written back with `\n`
    /// endings and no byte order mark.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::new(text.to_string(), LineEnding::Lf, false, false)
    }

    fn new(text: String, ending: LineEnding, bom: bool, lossy: bool) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at + 1).filter(|at| *at < text.len()));
        starts.push(text.len());
        if text.is_empty() {
            starts.truncate(1);
        }
        Self { text, starts, ending, bom, lossy }
    }

    /// The whole text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many lines it has. A final newline ends a line rather than
    /// starting one.
    #[must_use]
    pub fn line_count(&self) -> u32 {
        u32::try_from(self.starts.len() - 1).unwrap_or(u32::MAX)
    }

    /// Lines `range`, with their line endings, as one string.
    #[must_use]
    pub fn lines(&self, range: Range<u32>) -> &str {
        let last = self.starts.len() - 1;
        let start = self.starts[(range.start as usize).min(last)];
        let end = self.starts[(range.end as usize).min(last)];
        &self.text[start..end.max(start)]
    }

    /// Whether the bytes it was decoded from were not valid UTF-8.
    #[must_use]
    pub const fn is_lossy(&self) -> bool {
        self.lossy
    }

    /// `text` encoded the way this base was: its line ending, and its byte
    /// order mark if it had one.
    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let mut out = String::with_capacity(text.len() + 3);
        if self.bom {
            out.push('\u{feff}');
        }
        match self.ending {
            LineEnding::Lf => out.push_str(text),
            LineEnding::Crlf => out.push_str(&text.replace('\n', "\r\n")),
        }
        out.into_bytes()
    }
}

/// The hunks between a base and a text, and the base they were taken from.
///
/// Cheap to share: the editor holds one per open document behind an [`Arc`],
/// and the lookups a gutter needs are a binary search each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    base: Arc<Base>,
    hunks: Vec<Hunk>,
    /// How many lines the new text has, counted the way [`Base::line_count`]
    /// counts them.
    lines: u32,
}

impl Diff {
    /// Diff `text` against `base`.
    #[must_use]
    pub fn new(base: Arc<Base>, text: &str) -> Self {
        let hunks = line_hunks(base.text(), text);
        let lines = Base::from_text(text).line_count();
        Self { base, hunks, lines }
    }

    /// Diff `text` against `base`, where `text` is a rope.
    #[must_use]
    pub fn of_rope(base: Arc<Base>, text: &Rope) -> Self {
        Self::new(base, &text.to_string())
    }

    /// The hunks, in order.
    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// The old text.
    #[must_use]
    pub fn base(&self) -> &Base {
        &self.base
    }

    /// The old text, to share.
    #[must_use]
    pub fn base_shared(&self) -> Arc<Base> {
        Arc::clone(&self.base)
    }

    /// What line `line` of the new text is, if it changed. The one line that
    /// can carry two marks is the last, with lines removed both just above
    /// and after it; this gives the first, and [`Diff::marks`] gives both.
    #[must_use]
    pub fn mark(&self, line: u32) -> Option<LineMark> {
        let hunk = &self.hunks[self.hunk_index_at(line)?];
        Some(self.mark_in(hunk, line))
    }

    /// The marks for lines `range` of the new text, in order — what a gutter
    /// drawing those lines needs, for the cost of the hunks they touch.
    pub fn marks(&self, range: Range<u32>) -> impl Iterator<Item = (u32, LineMark)> + '_ {
        let first = self.hunks.partition_point(|hunk| self.last_line(hunk) < range.start);
        self.hunks[first..]
            .iter()
            .take_while(move |hunk| hunk.anchor(self.lines) < range.end)
            .flat_map(move |hunk| {
                let lines = if hunk.after.is_empty() {
                    let at = hunk.anchor(self.lines);
                    at..at + 1
                } else {
                    hunk.after.clone()
                };
                let range = range.clone();
                lines
                    .filter(move |line| range.contains(line))
                    .map(move |line| (line, self.mark_in(hunk, line)))
            })
    }

    /// The hunk that line `line` of the new text belongs to, if any —
    /// including a removal marked on it.
    #[must_use]
    pub fn hunk_at(&self, line: u32) -> Option<&Hunk> {
        self.hunk_index_at(line).map(|index| &self.hunks[index])
    }

    /// Which hunk line `line` belongs to, by its position in [`Diff::hunks`].
    /// Where the last line carries two removals, the first of them.
    #[must_use]
    pub fn hunk_index_at(&self, line: u32) -> Option<usize> {
        let index = self.hunks.partition_point(|hunk| self.last_line(hunk) < line);
        let hunk = self.hunks.get(index)?;
        let covers = if hunk.after.is_empty() {
            hunk.anchor(self.lines) == line
        } else {
            hunk.after.contains(&line)
        };
        covers.then_some(index)
    }

    /// The first hunk starting after line `line`, wrapping to the first.
    #[must_use]
    pub fn next_hunk(&self, line: u32) -> Option<&Hunk> {
        self.hunks.iter().find(|hunk| hunk.anchor(self.lines) > line).or_else(|| self.hunks.first())
    }

    /// The last hunk starting before line `line`, wrapping to the last.
    #[must_use]
    pub fn previous_hunk(&self, line: u32) -> Option<&Hunk> {
        self.hunks
            .iter()
            .rev()
            .find(|hunk| hunk.anchor(self.lines) < line)
            .or_else(|| self.hunks.last())
    }

    /// What the hunk's lines were before, with their line endings: what a
    /// popover shows as the previous text, and what a revert puts back.
    #[must_use]
    pub fn previous_text(&self, hunk: &Hunk) -> &str {
        self.base.lines(hunk.before.clone())
    }

    /// The edit that puts `hunk` back the way the base had it, in `text` —
    /// the rope this diff was taken from. Char offsets, like every edit.
    #[must_use]
    pub fn revert(&self, hunk: &Hunk, text: &Rope) -> Edit {
        let lines = text.len_lines();
        let char_of = |line: u32| {
            let line = line as usize;
            if line >= lines { text.len_chars() } else { text.line_to_char(line) }
        };
        let start = char_of(hunk.after.start);
        let end = char_of(hunk.after.end);
        Edit::replace(start, end, self.previous_text(hunk))
    }

    /// What the base becomes with `hunk` taken from `text` and every other
    /// hunk left as the base has it — the text staging that hunk writes.
    #[must_use]
    pub fn staged(&self, hunk: &Hunk, text: &str) -> String {
        let current = Base::from_text(text);
        let mut out = String::with_capacity(self.base.text.len() + text.len());
        out.push_str(self.base.lines(0..hunk.before.start));
        out.push_str(current.lines(hunk.after.clone()));
        out.push_str(self.base.lines(hunk.before.end..u32::MAX));
        out
    }

    fn last_line(&self, hunk: &Hunk) -> u32 {
        if hunk.after.is_empty() { hunk.anchor(self.lines) } else { hunk.after.end - 1 }
    }

    fn mark_in(&self, hunk: &Hunk, line: u32) -> LineMark {
        match hunk.kind() {
            HunkKind::Added => LineMark::Added,
            HunkKind::Modified => LineMark::Modified,
            HunkKind::Removed if hunk.after.start >= self.lines && self.lines > 0 => {
                debug_assert_eq!(line, self.lines - 1);
                LineMark::RemovedBelow
            }
            HunkKind::Removed => LineMark::RemovedAbove,
        }
    }
}

/// The line hunks between `before` and `after`.
#[must_use]
pub fn line_hunks(before: &str, after: &str) -> Vec<Hunk> {
    let input = InternedInput::new(sources::lines(before), sources::lines(after));
    let mut diff = Lines::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks().map(|hunk| Hunk { before: hunk.before, after: hunk.after }).collect()
}

/// Which parts of two versions of a line differ, word by word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    /// Char ranges of the old line that are not in the new one.
    pub before: Vec<Range<usize>>,
    /// Char ranges of the new line that are not in the old one.
    pub after: Vec<Range<usize>>,
}

/// The words that changed between `before` and `after`, for highlighting the
/// changes within a modified line rather than the whole of it.
///
/// A word is a run of letters, digits and underscores, or a run of spaces;
/// anything else is a word on its own. Ranges are in chars, merged where they
/// touch.
#[must_use]
pub fn inline_changes(before: &str, after: &str) -> Inline {
    let input = InternedInput::new(sources::words(before), sources::words(after));
    let mut diff = Lines::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);
    let ranges = |tokens: &[gix::diff::blob::Token], changed: &dyn Fn(u32) -> bool| {
        let mut out: Vec<Range<usize>> = Vec::new();
        let mut at = 0;
        for (index, token) in tokens.iter().enumerate() {
            let width = input.interner[*token].chars().count();
            if changed(u32::try_from(index).unwrap_or(u32::MAX)) {
                match out.last_mut() {
                    Some(last) if last.end == at => last.end = at + width,
                    _ => out.push(at..at + width),
                }
            }
            at += width;
        }
        out
    };
    Inline {
        before: ranges(&input.before, &|index| diff.is_removed(index)),
        after: ranges(&input.after, &|index| diff.is_added(index)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn diff(base: &str, text: &str) -> Diff {
        Diff::new(Arc::new(Base::from_text(base)), text)
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        assert!(diff("a\nb\n", "a\nb\n").is_empty());
        assert!(diff("", "").is_empty());
    }

    #[test]
    fn each_kind_of_change_is_marked_on_the_new_lines() {
        let d = diff("a\nb\nc\nd\n", "a\nB\nc\nnew\nd\n");
        assert_eq!(d.mark(0), None);
        assert_eq!(d.mark(1), Some(LineMark::Modified));
        assert_eq!(d.mark(2), None);
        assert_eq!(d.mark(3), Some(LineMark::Added));
        assert_eq!(d.mark(4), None);

        let removed = diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(removed.hunks(), &[Hunk { before: 1..2, after: 1..1 }]);
        assert_eq!(removed.mark(0), None);
        assert_eq!(removed.mark(1), Some(LineMark::RemovedAbove));
        assert_eq!(removed.previous_text(&removed.hunks()[0]), "b\n");
    }

    #[test]
    fn the_last_line_can_carry_removals_above_and_below() {
        let d = diff("a\nb\nc\nd\n", "a\nc\n");
        assert_eq!(d.hunks().len(), 2);
        assert_eq!(
            d.marks(0..10).collect::<Vec<_>>(),
            vec![(1, LineMark::RemovedAbove), (1, LineMark::RemovedBelow)]
        );
        assert_eq!(d.mark(1), Some(LineMark::RemovedAbove));
        assert_eq!(d.hunk_index_at(1), Some(0));
    }

    #[test]
    fn a_removal_at_the_end_is_marked_on_the_last_line() {
        let d = diff("a\nb\nc\n", "a\n");
        assert_eq!(d.mark(0), Some(LineMark::RemovedBelow));
        assert_eq!(d.marks(0..10).collect::<Vec<_>>(), vec![(0, LineMark::RemovedBelow)]);
        // Everything gone: the one empty line there is carries it.
        let d = diff("a\n", "");
        assert_eq!(d.mark(0), Some(LineMark::RemovedAbove));
    }

    #[test]
    fn losing_the_final_newline_changes_the_last_line() {
        let d = diff("a\nb\n", "a\nb");
        assert_eq!(d.mark(1), Some(LineMark::Modified));
        assert_eq!(d.mark(0), None);
    }

    #[test]
    fn marks_for_a_window_are_those_lines_only() {
        let d = diff("1\n2\n3\n4\n5\n6\n", "1\nX\n3\n4\nY\nZ\n6\n");
        let marks: Vec<_> = d.marks(3..6).collect();
        assert_eq!(marks, vec![(4, LineMark::Modified), (5, LineMark::Modified)]);
        let all: Vec<_> = d.marks(0..100).map(|(line, _)| line).collect();
        assert_eq!(all, vec![1, 4, 5]);
    }

    #[test]
    fn hunks_are_found_from_any_of_their_lines_and_wrap_when_jumping() {
        let d = diff("1\n2\n3\n4\n5\n", "1\nX\n3\n4\nY\n");
        assert_eq!(d.hunk_at(1).map(|h| h.after.clone()), Some(1..2));
        assert_eq!(d.hunk_at(2), None);
        assert_eq!(d.next_hunk(1).map(|h| h.after.start), Some(4));
        assert_eq!(d.next_hunk(4).map(|h| h.after.start), Some(1));
        assert_eq!(d.previous_hunk(4).map(|h| h.after.start), Some(1));
        assert_eq!(d.previous_hunk(1).map(|h| h.after.start), Some(4));
    }

    #[test]
    fn crlf_bases_decode_to_the_buffers_lines_and_encode_back() {
        let base = Base::decode(b"\xef\xbb\xbfone\r\ntwo\r\n");
        assert_eq!(base.text(), "one\ntwo\n");
        assert_eq!(base.line_count(), 2);
        let d = Diff::new(Arc::new(base), "one\ntwo\n");
        assert!(d.is_empty());
        assert_eq!(d.base().encode("one\n2\n"), b"\xef\xbb\xbfone\r\n2\r\n");
    }

    #[test]
    fn staging_one_hunk_leaves_the_others_as_they_were() {
        let base = "a\nb\nc\nd\ne\n";
        let text = "a\nB\nc\nd\nE\n";
        let d = diff(base, text);
        assert_eq!(d.hunks().len(), 2);
        assert_eq!(d.staged(&d.hunks()[0], text), "a\nB\nc\nd\ne\n");
        assert_eq!(d.staged(&d.hunks()[1], text), "a\nb\nc\nd\nE\n");
    }

    #[test]
    fn reverting_works_in_chars_across_wide_text() {
        let base = "é\n日本\n🙂\n";
        let text = "é\n日本語\n🙂\n";
        let d = diff(base, text);
        let rope = Rope::from_str(text);
        let edit = d.revert(&d.hunks()[0], &rope);
        assert_eq!(edit, Edit::replace(2, 6, "日本\n"));
    }

    #[test]
    fn inline_changes_are_char_ranges_of_changed_words() {
        let inline = inline_changes("let café = 1;", "let café = 22;");
        assert_eq!(inline.before, vec![11..12]);
        assert_eq!(inline.after, vec![11..13]);
        let inline = inline_changes("a b", "a b");
        assert!(inline.before.is_empty() && inline.after.is_empty());
    }

    /// Short lines from a tiny alphabet, so random texts share lines often
    /// enough to make interesting diffs.
    fn text() -> impl Strategy<Value = String> {
        let line = prop_oneof![Just("a"), Just("b"), Just("é"), Just("日本"), Just("")];
        (proptest::collection::vec(line, 0..12), any::<bool>()).prop_map(|(lines, newline)| {
            let mut text = lines.join("\n");
            if newline && !text.is_empty() {
                text.push('\n');
            }
            text
        })
    }

    fn apply(rope: &mut Rope, edit: &Edit) {
        rope.remove(edit.start..edit.end);
        rope.insert(edit.start, &edit.text);
    }

    proptest! {
        #[test]
        fn reverting_every_hunk_gives_back_the_base(base in text(), now in text()) {
            let d = diff(&base, &now);
            let mut rope = Rope::from_str(&now);
            // From the end, so each edit's offsets are still good.
            for hunk in d.hunks().iter().rev() {
                let edit = d.revert(hunk, &rope);
                apply(&mut rope, &edit);
            }
            prop_assert_eq!(rope.to_string(), base);
        }

        #[test]
        fn staging_every_hunk_in_turn_gives_the_text(base in text(), now in text()) {
            let mut staged = base;
            for _ in 0..64 {
                let d = diff(&staged, &now);
                let Some(hunk) = d.hunks().first() else { break };
                staged = d.staged(hunk, &now);
            }
            prop_assert_eq!(staged, now);
        }

        #[test]
        fn every_hunk_is_marked_somewhere(base in text(), now in text()) {
            let d = diff(&base, &now);
            let marked: Vec<u32> = d.marks(0..u32::MAX).map(|(line, _)| line).collect();
            for (index, hunk) in d.hunks().iter().enumerate() {
                let lines = Base::from_text(&now).line_count();
                let line = hunk.anchor(lines);
                prop_assert!(marked.contains(&line));
                // Two removals can share the last line; the first is found.
                let found = d.hunk_index_at(line);
                let shared = |first: usize| first + 1 == index && d.hunks()[first].anchor(lines) == line;
                prop_assert!(found == Some(index) || found.is_some_and(shared));
            }
        }
    }
}
