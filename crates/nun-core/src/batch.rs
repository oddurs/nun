//! Edits that come from outside the editor — a formatter, a rename — made
//! ready to apply as one step, and the selections carried across them.
//!
//! Such a batch differs from typing in three ways. It arrives unsorted, in
//! whatever order its author listed it, with the order meaning something where
//! two inserts share a position. It is coarse: a formatter may send the whole
//! file back as one replacement when it changed three spaces. And the carets it
//! lands on were not the ones that asked for it, so "after what was inserted",
//! right for a caret that typed the text, is wrong for one a formatter moved
//! past.
//!
//! So a batch is first put in order and checked, then each edit is cut down to
//! what it actually changes, and a position inside what an edit replaces is
//! carried across by the text around it rather than by its offset.

use ropey::Rope;

use crate::edit::Edit;

/// Why a batch of edits could not be applied. Nothing is changed when one is
/// refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// An edit reaches past the end of the text, or ends before it starts.
    #[error("an edit of {start}..{end} does not fit a text {len} chars long")]
    OutOfRange {
        /// Where it starts.
        start: usize,
        /// Where it ends.
        end: usize,
        /// How long the text is.
        len: usize,
    },
    /// Two edits replace some of the same text, so neither order is right.
    #[error("edits of {first:?} and {second:?} overlap")]
    Overlap {
        /// The earlier, as `(start, end)`.
        first: (usize, usize),
        /// The later.
        second: (usize, usize),
    },
}

/// A batch made ready to apply.
#[derive(Debug)]
pub(crate) struct Batch {
    /// The edits as given, sorted, joined where they share a position, and
    /// with their line endings made `\n`.
    whole: Vec<Edit>,
    /// The same cut down to what they change: what is applied.
    pub pieces: Vec<Edit>,
}

/// Put a batch in order, check it, and cut each edit down to what it changes.
///
/// Every edit is in the coordinates of `text` as it stands, as a language
/// server's are. Carriage returns in the new text become `\n`, which is all a
/// buffer holds. Edits are sorted by where they start, keeping their order
/// otherwise, and inserts sharing a position are joined in that order, as the
/// protocol says. What comes back is sorted, disjoint and free of no-ops, and
/// applying it gives exactly the text applying the batch would.
pub(crate) fn prepare(text: &Rope, edits: Vec<Edit>) -> Result<Batch, BatchError> {
    let len = text.len_chars();
    let mut edits: Vec<Edit> = edits
        .into_iter()
        .map(|edit| {
            if edit.start > edit.end || edit.end > len {
                return Err(BatchError::OutOfRange { start: edit.start, end: edit.end, len });
            }
            Ok(edit)
        })
        .collect::<Result<_, _>>()?;
    // Stable, so inserts at one position keep the order they were given in.
    edits.sort_by_key(|edit| edit.start);

    let mut joined: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match joined.last_mut() {
            // Only the last of several edits at one position may replace
            // anything; the rest are inserts in front of it.
            Some(last) if last.start == edit.start && last.start == last.end => {
                last.text.push_str(&edit.text);
                last.end = edit.end;
            }
            // An insert where the last edit starts goes after its text.
            Some(last) if last.start == edit.start && edit.start == edit.end => {
                last.text.push_str(&edit.text);
            }
            Some(last) if last.end > edit.start => {
                return Err(BatchError::Overlap {
                    first: (last.start, last.end),
                    second: (edit.start, edit.end),
                });
            }
            _ => joined.push(edit),
        }
    }

    // Only now, so that a `\r\n` split between two inserts at one position
    // is still one line break.
    for edit in &mut joined {
        edit.text = line_feeds(std::mem::take(&mut edit.text));
    }
    let mut pieces = Vec::with_capacity(joined.len());
    for edit in &joined {
        refine(text, edit, &mut pieces);
    }
    Ok(Batch { whole: joined, pieces })
}

/// Whether `new` is what `old` already is. A line break is a line break: a
/// lone `\r` the buffer was loaded with is not rewritten because a server
/// sent back the `\n` it reads it as.
fn same(old: char, new: char) -> bool {
    old == new || (old == '\r' && new == '\n')
}

/// The chars of `chars` that are not whitespace, in order.
fn shape(chars: &[char]) -> impl Iterator<Item = char> + '_ {
    chars.iter().filter(|c| !c.is_whitespace()).copied()
}

/// `\r\n` and a lone `\r` as `\n`: the protocol breaks lines at all three, and
/// a buffer holds only the last.
fn line_feeds(text: String) -> String {
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text }
}

/// Cut one edit down to the pieces that change something, pushing them in
/// order onto `out`.
///
/// What the old and new text share at either end is left alone. If what is
/// left differs only in whitespace — nearly everything a formatter does — it
/// becomes one edit per stretch of whitespace that changed, so every other
/// char stays where it is, and so does anything anchored to it: a caret, a
/// fold, a diagnostic. Otherwise it stays one edit.
fn refine(text: &Rope, edit: &Edit, out: &mut Vec<Edit>) {
    let old: Vec<char> = text.slice(edit.start..edit.end).chars().collect();
    let new: Vec<char> = edit.text.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| same(**a, **b)).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| same(**a, **b))
        .count();
    let old = &old[prefix..old.len() - suffix];
    let new = &new[prefix..new.len() - suffix];
    let start = edit.start + prefix;
    if old.is_empty() && new.is_empty() {
        return;
    }

    if !shape(old).eq(shape(new)) {
        out.push(Edit::replace(start, start + old.len(), new.iter().collect::<String>()));
        return;
    }

    // Walk both in step: a run of whitespace in each, then the one char
    // they agree on, until both run out together — which they do, having the
    // same chars in the same order once whitespace is set aside.
    let (mut i, mut j) = (0, 0);
    loop {
        let old_run = old[i..].iter().take_while(|c| c.is_whitespace()).count();
        let new_run = new[j..].iter().take_while(|c| c.is_whitespace()).count();
        let (was, now) = (&old[i..i + old_run], &new[j..j + new_run]);
        let unchanged = was.len() == now.len() && was.iter().zip(now).all(|(a, b)| same(*a, *b));
        if !unchanged {
            // Even inside a run, keep what the two share at either end: an
            // indent that went from eight spaces to four loses four, rather
            // than being rewritten whole.
            let head = was.iter().zip(now).take_while(|(a, b)| same(**a, **b)).count();
            let tail = was[head..]
                .iter()
                .rev()
                .zip(now[head..].iter().rev())
                .take_while(|(a, b)| same(**a, **b))
                .count();
            let at = start + i + head;
            out.push(Edit::replace(
                at,
                at + was.len() - head - tail,
                now[head..now.len() - tail].iter().collect::<String>(),
            ));
        }
        i += old_run;
        j += new_run;
        if i >= old.len() || j >= new.len() {
            break;
        }
        i += 1;
        j += 1;
    }
}

/// Carries positions across a prepared batch.
#[derive(Debug)]
pub(crate) struct Carry<'a> {
    /// The text before the batch.
    text: &'a Rope,
    /// The pieces that are applied.
    edits: &'a [Edit],
    /// `before[k]` is the net change of every piece below the `k`th.
    before: Vec<isize>,
    /// The edits as given, before they were cut down.
    whole: &'a [Edit],
    /// The same as `before`, for them.
    whole_before: Vec<isize>,
}

/// `before[k]` for `edits`: the net change of every edit below the `k`th.
fn net_before(edits: &[Edit]) -> Vec<isize> {
    let mut before = Vec::with_capacity(edits.len());
    let mut net: isize = 0;
    for edit in edits {
        before.push(net);
        net += edit.inserted().cast_signed() - edit.removed().cast_signed();
    }
    before
}

impl<'a> Carry<'a> {
    /// Ready to carry positions in `text` across `batch`.
    pub(crate) fn new(text: &'a Rope, batch: &'a Batch) -> Self {
        Self {
            text,
            edits: &batch.pieces,
            before: net_before(&batch.pieces),
            whole: &batch.whole,
            whole_before: net_before(&batch.whole),
        }
    }

    /// Where `pos` ends up once the edits have been applied.
    ///
    /// A position outside every edit moves by what the edits before it did.
    /// One that an edit starts at, ends at, or replaces is placed by the text
    /// around it, the way a person would find their place again after a
    /// reformat:
    ///
    /// - At the end of what an edit replaced, it stays against the unchanged
    ///   text that follows — the end of the edit as it was given, not as it
    ///   was cut down, so a caret at the end of a name a rename lengthened is
    ///   at the end of the new name.
    /// - Otherwise it counts the non-whitespace chars between the start of the
    ///   edit and itself, and goes just past that many in the new text. A
    ///   caret after the third token of a line that was rewrapped is still
    ///   after the third token.
    /// - With none to count, it stays against the text before the edit when
    ///   that is not whitespace — the end of a word someone was typing — and
    ///   otherwise goes to the first thing the edit put there that is not
    ///   whitespace: an indent that changed leaves the caret at the start of
    ///   the code, not stranded in front of the new indent.
    pub(crate) fn map(&self, pos: usize) -> usize {
        let given = self.whole.partition_point(|edit| edit.end < pos);
        if let Some(edit) = self.whole.get(given)
            && edit.end == pos
            && edit.end > edit.start
        {
            return edit.end_after().saturating_add_signed(self.whole_before[given]);
        }
        let at = self.edits.partition_point(|edit| edit.start <= pos);
        let Some(k) = at.checked_sub(1) else { return pos };
        let edit = &self.edits[k];
        let moved = |to: usize| to.saturating_add_signed(self.before[k]);

        if pos > edit.end {
            return moved(pos - edit.removed() + edit.inserted());
        }
        if pos == edit.end && edit.end > edit.start {
            return moved(edit.end_after());
        }
        let passed =
            self.text.slice(edit.start..pos).chars().filter(|c| !c.is_whitespace()).count();
        if passed > 0 {
            let past = edit
                .text
                .chars()
                .enumerate()
                .filter(|(_, c)| !c.is_whitespace())
                .nth(passed - 1)
                .map_or(edit.inserted(), |(i, _)| i + 1);
            return moved(edit.start + past);
        }
        let after_word = pos == edit.start && pos > 0 && !self.text.char(pos - 1).is_whitespace();
        if after_word {
            return moved(edit.start);
        }
        let indent = edit.text.chars().take_while(|c| c.is_whitespace()).count();
        moved(edit.start + indent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edits: &[Edit]) -> String {
        let mut rope = Rope::from_str(text);
        for edit in edits.iter().rev() {
            rope.remove(edit.start..edit.end);
            rope.insert(edit.start, &edit.text);
        }
        rope.to_string()
    }

    fn prepared(text: &str, edits: Vec<Edit>) -> Vec<Edit> {
        prepare(&Rope::from_str(text), edits).unwrap().pieces
    }

    /// Where `pos` in `text` goes across `edits`.
    fn carried(text: &str, edits: Vec<Edit>, pos: usize) -> usize {
        let rope = Rope::from_str(text);
        let batch = prepare(&rope, edits).unwrap();
        Carry::new(&rope, &batch).map(pos)
    }

    #[test]
    fn a_whole_file_replacement_becomes_the_whitespace_that_changed() {
        let text = "fn main(){\n        let x=1;\n}\n";
        let formatted = "fn main() {\n    let x = 1;\n}\n";
        let edits = prepared(text, vec![Edit::replace(0, text.chars().count(), formatted)]);
        assert_eq!(apply(text, &edits), formatted);
        assert_eq!(
            edits,
            vec![
                Edit::insert(9, " "),
                Edit::delete(15, 19),
                Edit::insert(24, " "),
                Edit::insert(25, " "),
            ]
        );
    }

    #[test]
    fn a_change_to_more_than_whitespace_is_trimmed_to_what_differs() {
        let text = "call(a, b)\n";
        let edits = prepared(text, vec![Edit::replace(0, 11, "call(a, b,)\n")]);
        assert_eq!(edits, vec![Edit::insert(9, ",")]);
    }

    #[test]
    fn inserts_at_one_position_keep_their_order() {
        let text = "ab";
        let edits = prepared(
            text,
            vec![Edit::insert(1, "1"), Edit::replace(1, 2, "B"), Edit::insert(1, "2")],
        );
        // The replace was listed between the two inserts, but only the last
        // edit at a position can replace anything: this is its text order.
        assert_eq!(apply(text, &edits), "a1B2");
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let rope = Rope::from_str("abcdef");
        let refused = prepare(&rope, vec![Edit::replace(3, 5, "x"), Edit::replace(1, 4, "y")]);
        assert_eq!(
            refused.map(|batch| batch.pieces),
            Err(BatchError::Overlap { first: (1, 4), second: (3, 5) })
        );
    }

    #[test]
    fn an_edit_past_the_end_is_refused() {
        let rope = Rope::from_str("abc");
        let refused = prepare(&rope, vec![Edit::delete(2, 4)]);
        assert_eq!(
            refused.map(|batch| batch.pieces),
            Err(BatchError::OutOfRange { start: 2, end: 4, len: 3 })
        );
    }

    #[test]
    fn carriage_returns_arrive_as_line_feeds() {
        let edits = prepared("x", vec![Edit::insert(1, "\r\na\rb\r\n")]);
        assert_eq!(edits, vec![Edit::insert(1, "\na\nb\n")]);
    }

    #[test]
    fn a_line_break_split_between_two_inserts_is_one_line_break() {
        let edits = prepared("ab", vec![Edit::insert(1, "x\r"), Edit::insert(1, "\ny")]);
        assert_eq!(apply("ab", &edits), "ax\nyb");
    }

    #[test]
    fn a_lone_carriage_return_sent_back_as_it_was_is_left_alone() {
        // The buffer keeps a lone `\r` it was loaded with; a server reads it
        // as a line break and sends back `\n`, or the `\r` itself.
        assert!(prepared("a\rb", vec![Edit::replace(0, 3, "a\rb")]).is_empty());
        assert!(prepared("a\rb", vec![Edit::replace(0, 3, "a\nb")]).is_empty());
        assert_eq!(
            prepared("a\r  b", vec![Edit::replace(0, 5, "a\nb")]),
            vec![Edit::delete(2, 4)],
            "only what else changed"
        );
    }

    #[test]
    fn edits_that_change_nothing_disappear() {
        assert!(prepared("same", vec![Edit::replace(0, 4, "same")]).is_empty());
    }

    #[test]
    fn a_caret_in_a_reindented_line_stays_on_its_token() {
        let text = "{\n        foo(bar);\n}";
        let formatted = "{\n    foo(bar);\n}";
        let edits = || vec![Edit::replace(0, text.chars().count(), formatted)];
        assert_eq!(carried(text, edits(), 14), 10, "before `bar`");
        assert_eq!(carried(text, edits(), 2), 2, "the start of the line is untouched");
        assert_eq!(carried(text, edits(), 8), 6, "inside the indent that went");
    }

    #[test]
    fn a_caret_at_the_end_of_a_word_stays_there() {
        let text = "x=1";
        let edits = || vec![Edit::replace(0, 3, "x = 1")];
        assert_eq!(carried(text, edits(), 1), 1, "still just after x");
        assert_eq!(carried(text, edits(), 2), 3, "still just after =");
    }

    #[test]
    fn a_caret_at_the_end_of_a_renamed_name_is_at_the_end_of_the_new_one() {
        // Cut down, this is an insert of `bar` after `foo`, and a caret
        // there would stay against the `foo`. It was at the end of what the
        // edit replaced, so it goes to the end of what replaced it.
        assert_eq!(carried("foo;", vec![Edit::replace(0, 3, "foobar")], 3), 6);
        assert_eq!(carried("foo;", vec![Edit::replace(0, 3, "fo")], 3), 2);
    }

    #[test]
    fn a_caret_inside_a_rewritten_span_keeps_its_count_of_tokens() {
        // Not whitespace only: the comma is new, so this stays one edit and
        // the caret is placed by counting.
        let text = "f(a,b)";
        let edits = || vec![Edit::replace(0, 6, "f(\n    a,\n    b,\n)")];
        let formatted = apply(text, &prepared(text, edits()));
        let before_b = carried(text, edits(), 4);
        assert_eq!(&formatted[..before_b], "f(\n    a,");
    }

    proptest::proptest! {
        #[test]
        fn preparing_never_changes_the_result(
            text in "[a b\\n😀é\\u{301}中\\t]{0,30}",
            raw in proptest::collection::vec((0usize..40, 0usize..6, "(a| |\n|\r\n|😀|\u{301}){0,5}"), 0..6),
        ) {
            let len = text.chars().count();
            // Disjoint, in the order given: each edit takes a slice after the
            // last one's end.
            let mut edits = Vec::new();
            let mut floor = 0;
            for (gap, width, insert) in raw {
                let start = (floor + gap % 4).min(len);
                let end = (start + width).min(len);
                edits.push(Edit::replace(start, end, insert));
                floor = end;
            }
            let expected = {
                let mut normalised = edits.clone();
                for edit in &mut normalised {
                    edit.text = line_feeds(edit.text.clone());
                }
                apply(&text, &normalised)
            };
            let rope = Rope::from_str(&text);
            let batch = prepare(&rope, edits).unwrap();
            let prepared = &batch.pieces;
            proptest::prop_assert!(prepared.windows(2).all(|w| w[0].end <= w[1].start));
            proptest::prop_assert!(prepared.iter().all(|edit| !edit.is_noop()));
            proptest::prop_assert_eq!(apply(&text, prepared), expected.clone());
            let total = expected.chars().count();
            for pos in 0..=len {
                proptest::prop_assert!(Carry::new(&rope, &batch).map(pos) <= total);
            }
        }

        #[test]
        fn reformatting_whitespace_keeps_every_caret_on_its_char(
            words in proptest::collection::vec("[a-z😀中é]{1,3}", 1..6),
            // Words apart to begin with: two run together are one word, and a
            // caret between them may stay with either.
            gaps in proptest::collection::vec("[ \\n\\t]{1,4}", 7),
            fresh in proptest::collection::vec("[ \\n]{0,3}", 7),
            pick in 0usize..60,
        ) {
            let join = |gaps: &[String]| {
                let mut out = gaps[0].clone();
                for (word, gap) in words.iter().zip(&gaps[1..]) {
                    out.push_str(word);
                    out.push_str(gap);
                }
                out
            };
            let text = join(&gaps);
            let formatted = join(&fresh);
            let rope = Rope::from_str(&text);
            let batch = prepare(&rope, vec![Edit::replace(0, rope.len_chars(), formatted.clone())]).unwrap();
            let chars: Vec<char> = text.chars().collect();
            let pos = pick.min(chars.len());
            if pos < chars.len() && !chars[pos].is_whitespace() {
                // A caret on a char of a word is on the same char afterwards.
                let mapped = Carry::new(&rope, &batch).map(pos);
                let at: Vec<char> = formatted.chars().collect();
                let rank = |chars: &[char], upto: usize| chars[..upto].iter().filter(|c| !c.is_whitespace()).count();
                proptest::prop_assert_eq!(at.get(mapped), Some(&chars[pos]));
                proptest::prop_assert_eq!(rank(&at, mapped), rank(&chars, pos));
            }
        }
    }
}
