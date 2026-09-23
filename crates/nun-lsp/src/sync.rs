//! Keeping a server's copy of a document the same as the buffer.
//!
//! The buffer keeps a journal of every edit, in the order it made them, each
//! in the coordinates of the text just before it (`Buffer::take_edits`). That
//! is exactly the protocol's shape for incremental changes, so each edit
//! becomes one change — provided its char offsets are turned into positions
//! against the text *as it stood when that edit was made*, not the text before
//! the batch or after it. The shadow is that text: a copy of what the server
//! has, advanced one edit at a time as the changes are worked out.
//!
//! Two things make an incremental change unsafe, and both fall back to sending
//! the whole text, which cannot be wrong:
//!
//! - A carriage return anywhere in the text. The protocol breaks lines at a
//!   lone `\r` and nun does not, so a position after one names a different
//!   place to each of them.
//! - A journal that does not fit the shadow — which would be a bug, but a bug
//!   that costs one large message rather than a server silently editing a
//!   different document from the one on screen.

use lsp_types::TextDocumentContentChangeEvent;
use nun_core::Edit;
use ropey::Rope;

use crate::position::Encoding;

/// How a server wants to hear about changes, from its capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SyncKind {
    /// Not at all.
    None,
    /// The whole text, every time.
    Full,
    /// Only what changed.
    #[default]
    Incremental,
}

/// The server's copy of one document.
#[derive(Debug, Clone)]
pub(crate) struct Shadow {
    text: Rope,
    /// How many `\r` the text holds, kept up to date edit by edit so that
    /// knowing whether there are any never means reading the whole text.
    returns: usize,
}

impl Shadow {
    /// A shadow of `text`, as a server opening it will have it.
    pub(crate) fn new(text: Rope) -> Self {
        let returns = count_returns(text.chars());
        Self { text, returns }
    }

    /// The text the server has.
    pub(crate) const fn text(&self) -> &Rope {
        &self.text
    }

    /// Take the text as it is now, sending nothing: for when the server does
    /// not have the document open, and will be sent all of it when it does.
    pub(crate) fn reset(&mut self, text: &Rope) {
        *self = Self::new(text.clone());
    }

    /// The changes that take the server from the shadow to `text`, which
    /// `edits` produced, and the shadow moved on to match.
    ///
    /// Empty for a server that wants none, or when nothing changed.
    pub(crate) fn changes(
        &mut self,
        edits: &[Edit],
        text: &Rope,
        encoding: Encoding,
        sync: SyncKind,
    ) -> Vec<TextDocumentContentChangeEvent> {
        match sync {
            SyncKind::None => {
                self.reset(text);
                Vec::new()
            }
            SyncKind::Full => {
                self.reset(text);
                vec![whole(text)]
            }
            SyncKind::Incremental => match self.follow(edits, encoding) {
                // Replaying the journal has made the shadow the same text as
                // the buffer, which the length checks cheaply and the tests
                // check exhaustively. Taking the buffer's rope rather than
                // keeping the replayed one lets the two share their storage.
                Some(changes) if self.text.len_chars() == text.len_chars() => {
                    self.text = text.clone();
                    changes
                }
                _ => {
                    self.reset(text);
                    vec![whole(text)]
                }
            },
        }
    }

    /// Apply each edit to the shadow, describing it as it goes. `None` when
    /// the edits cannot be described exactly, or do not fit.
    fn follow(
        &mut self,
        edits: &[Edit],
        encoding: Encoding,
    ) -> Option<Vec<TextDocumentContentChangeEvent>> {
        let mut changes = Vec::with_capacity(edits.len());
        let mut exact = true;
        for edit in edits {
            if edit.start > edit.end || edit.end > self.text.len_chars() {
                return None;
            }
            // Measured before the edit, against the text it was made to.
            exact &= self.returns == 0;
            if exact {
                changes.push(TextDocumentContentChangeEvent {
                    range: Some(encoding.range(&self.text, edit.start..edit.end)),
                    range_length: None,
                    text: edit.text.clone(),
                });
            }
            let removed = count_returns(self.text.slice(edit.start..edit.end).chars());
            self.text.remove(edit.start..edit.end);
            self.text.insert(edit.start, &edit.text);
            self.returns = self.returns - removed + count_returns(edit.text.chars());
        }
        exact.then_some(changes)
    }
}

/// The whole text as one change.
fn whole(text: &Rope) -> TextDocumentContentChangeEvent {
    TextDocumentContentChangeEvent { range: None, range_length: None, text: text.to_string() }
}

fn count_returns(chars: impl Iterator<Item = char>) -> usize {
    chars.filter(|&char| char == '\r').count()
}

#[cfg(test)]
pub(crate) mod tests {
    use lsp_types::Position;
    use nun_core::{Buffer, Range, Selections};
    use proptest::prelude::*;

    use super::*;

    /// A server's copy of a document, kept the way the protocol says to,
    /// written without anything from nun: lines broken at `\n`, `\r\n` and a
    /// lone `\r`, offsets counted in the negotiated unit over a plain string.
    /// If the sync layer and this agree after every edit, the server has the
    /// buffer's text.
    #[derive(Debug, Clone)]
    pub(crate) struct Mirror {
        pub(crate) text: String,
        encoding: Encoding,
    }

    impl Mirror {
        pub(crate) fn new(text: &str, encoding: Encoding) -> Self {
            Self { text: text.to_string(), encoding }
        }

        pub(crate) fn apply(&mut self, change: &TextDocumentContentChangeEvent) {
            match change.range {
                None => self.text.clone_from(&change.text),
                Some(range) => {
                    let start = self.offset(range.start);
                    let end = self.offset(range.end);
                    assert!(start <= end, "an inverted range: {range:?}");
                    self.text.replace_range(start..end, &change.text);
                }
            }
        }

        /// The byte offset of a position.
        fn offset(&self, position: Position) -> usize {
            let bytes = self.text.as_bytes();
            let mut line = 0;
            let mut at = 0;
            while line < position.line {
                match bytes[at..].iter().position(|&b| b == b'\n' || b == b'\r') {
                    None => return self.text.len(),
                    Some(found) => {
                        at += found;
                        at += if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                            2
                        } else {
                            1
                        };
                        line += 1;
                    }
                }
            }
            let mut units = 0;
            for (index, char) in self.text[at..].char_indices() {
                if units >= position.character as usize || char == '\n' || char == '\r' {
                    return at + index;
                }
                units += match self.encoding {
                    Encoding::Utf8 => char.len_utf8(),
                    Encoding::Utf16 => char.len_utf16(),
                    Encoding::Utf32 => 1,
                };
            }
            self.text.len()
        }
    }

    /// Something a person can do to a buffer.
    #[derive(Debug, Clone)]
    enum Op {
        Type(String),
        Backspace,
        Delete,
        Undo,
        Redo,
        /// Put a caret at each of these fractions of the way through the text.
        Carets(Vec<u8>),
        /// Select from one fraction of the way through to another.
        Select(u8, u8),
        CaretBelow,
    }

    fn piece() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a".to_string()),
            Just(" ".to_string()),
            Just("\n".to_string()),
            Just("😀".to_string()),
            Just("e\u{301}".to_string()),
            Just("中".to_string()),
            Just("\u{1F469}\u{200D}\u{1F4BB}".to_string()),
            Just("\t".to_string()),
        ]
    }

    fn op(with_returns: bool) -> impl Strategy<Value = Op> {
        let typed = if with_returns {
            proptest::collection::vec(
                prop_oneof![piece(), Just("\r\n".into()), Just("\r".into())],
                1..4,
            )
            .boxed()
        } else {
            proptest::collection::vec(piece(), 1..4).boxed()
        };
        prop_oneof![
            4 => typed.prop_map(|pieces| Op::Type(pieces.concat())),
            2 => Just(Op::Backspace),
            1 => Just(Op::Delete),
            2 => Just(Op::Undo),
            1 => Just(Op::Redo),
            2 => proptest::collection::vec(any::<u8>(), 1..5).prop_map(Op::Carets),
            1 => (any::<u8>(), any::<u8>()).prop_map(|(from, to)| Op::Select(from, to)),
            1 => Just(Op::CaretBelow),
        ]
    }

    fn at(buffer: &Buffer, fraction: u8) -> usize {
        buffer.len_chars() * usize::from(fraction) / 255
    }

    fn perform(buffer: &mut Buffer, op: &Op) {
        match op {
            Op::Type(text) => buffer.insert(text),
            Op::Backspace => buffer.delete_backward(),
            Op::Delete => buffer.delete_forward(),
            Op::Undo => {
                buffer.undo();
            }
            Op::Redo => {
                buffer.redo();
            }
            Op::Carets(fractions) => {
                let ranges = fractions.iter().map(|&f| Range::caret(at(buffer, f))).collect();
                buffer.set_selections(Selections::new(ranges, 0));
            }
            Op::Select(from, to) => {
                let range = Range::new(at(buffer, *from), at(buffer, *to));
                buffer.set_selections(Selections::single(range));
            }
            Op::CaretBelow => buffer.add_caret_vertically(false),
        }
    }

    const ENCODINGS: [Encoding; 3] = [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32];

    /// Replay `ops` on a buffer holding `start`, following along through the
    /// sync layer in every encoding, and check each mirror after every step.
    /// How many steps went out as whole text.
    fn replay(start: &[u8], ops: &[Op]) -> Result<usize, TestCaseError> {
        let (mut buffer, _) = Buffer::from_bytes(start);
        buffer.keep_edits(true);
        let initial = buffer.rope().to_string();
        let mut followers: Vec<(Shadow, Mirror)> = ENCODINGS
            .iter()
            .map(|&encoding| (Shadow::new(buffer.rope().clone()), Mirror::new(&initial, encoding)))
            .collect();
        let mut whole_sends = 0;
        for op in ops {
            perform(&mut buffer, op);
            let edits = buffer.take_edits();
            let text = buffer.rope();
            for (shadow, mirror) in &mut followers {
                let changes = shadow.changes(&edits, text, mirror.encoding, SyncKind::Incremental);
                if changes.iter().any(|change| change.range.is_none()) {
                    whole_sends += 1;
                }
                for change in &changes {
                    mirror.apply(change);
                }
                prop_assert_eq!(
                    &mirror.text,
                    &text.to_string(),
                    "{:?} after {:?}",
                    mirror.encoding,
                    op
                );
            }
        }
        Ok(whole_sends)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// The acceptance criterion: whatever is done to the buffer — typing
        /// at several carets, deleting, undoing, redoing, replacing a
        /// selection — the server ends up with exactly the buffer's text, in
        /// every encoding, sent as edits rather than whole.
        #[test]
        fn a_server_following_the_changes_has_the_buffer_text(
            ops in proptest::collection::vec(op(false), 1..30),
        ) {
            let whole = replay("fn main() {\n    let 😀 = \"e\u{301}中\";\n}\n".as_bytes(), &ops)?;
            prop_assert_eq!(whole, 0, "nothing here needed the whole text");
        }

        /// A file with CRLF endings is held with LF, and that is the text the
        /// server is sent and edited in.
        #[test]
        fn a_crlf_file_is_followed_exactly(ops in proptest::collection::vec(op(false), 1..30)) {
            let whole = replay(b"one\r\ntwo \xf0\x9f\x98\x80\r\nthree\r\n", &ops)?;
            prop_assert_eq!(whole, 0);
        }

        /// Carriage returns typed into the text cannot be described as edits
        /// the protocol reads the same way, so the text goes whole — and is
        /// still exactly right.
        #[test]
        fn carriage_returns_fall_back_to_the_whole_text(
            ops in proptest::collection::vec(op(true), 1..30),
        ) {
            // A lone \r from the file itself, which loading keeps.
            replay(b"a\rb\nc", &ops)?;
        }
    }

    #[test]
    fn an_edit_is_described_against_the_text_it_was_made_to() {
        // Two carets typing an emoji: the higher one is applied first, so the
        // lower one's position is unmoved by it — and a UTF-16 server counts
        // the emoji as two units.
        let mut buffer = Buffer::from_text("ab\ncd");
        buffer.keep_edits(true);
        buffer.set_selections(Selections::new(vec![Range::caret(1), Range::caret(4)], 0));
        buffer.insert("😀");
        let edits = buffer.take_edits();
        let mut shadow = Shadow::new(Rope::from_str("ab\ncd"));
        let changes = shadow.changes(&edits, buffer.rope(), Encoding::Utf16, SyncKind::Incremental);
        let starts: Vec<_> = changes
            .iter()
            .map(|change| change.range.map(|range| (range.start.line, range.start.character)))
            .collect();
        assert_eq!(starts, [Some((1, 1)), Some((0, 1))]);
        assert_eq!(shadow.text(), buffer.rope());
    }

    #[test]
    fn a_full_sync_server_gets_the_whole_text_and_a_silent_one_gets_nothing() {
        let mut buffer = Buffer::from_text("ab");
        buffer.keep_edits(true);
        buffer.insert("x");
        let edits = buffer.take_edits();

        let mut shadow = Shadow::new(Rope::from_str("ab"));
        let full = shadow.changes(&edits, buffer.rope(), Encoding::Utf16, SyncKind::Full);
        assert_eq!(full.len(), 1);
        assert_eq!((full[0].range, full[0].text.as_str()), (None, "xab"));

        let mut shadow = Shadow::new(Rope::from_str("ab"));
        assert!(shadow.changes(&edits, buffer.rope(), Encoding::Utf16, SyncKind::None).is_empty());
        assert_eq!(shadow.text(), buffer.rope(), "the shadow still moves on");
    }

    #[test]
    fn a_journal_that_does_not_fit_sends_the_whole_text_rather_than_nonsense() {
        let mut shadow = Shadow::new(Rope::from_str("ab"));
        let text = Rope::from_str("something else entirely");
        let changes =
            shadow.changes(&[Edit::delete(5, 9)], &text, Encoding::Utf16, SyncKind::Incremental);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].range, None);
        assert_eq!(shadow.text(), &text);

        // Edits that fit but land on a different length are just as wrong.
        let mut shadow = Shadow::new(Rope::from_str("ab"));
        let changes =
            shadow.changes(&[Edit::insert(0, "x")], &text, Encoding::Utf16, SyncKind::Incremental);
        assert_eq!(changes[0].range, None);
    }

    #[test]
    fn deleting_the_last_carriage_return_goes_back_to_edits() {
        let mut shadow = Shadow::new(Rope::from_str("a\rb"));
        let mut buffer = Buffer::from_text("a\rb");
        buffer.keep_edits(true);
        buffer.set_selections(Selections::single(Range::new(1, 2)));
        buffer.delete_backward();
        let changes = shadow.changes(
            &buffer.take_edits(),
            buffer.rope(),
            Encoding::Utf16,
            SyncKind::Incremental,
        );
        assert_eq!(changes[0].range, None, "measured before the edit, when there was a \\r");

        buffer.insert("x");
        let changes = shadow.changes(
            &buffer.take_edits(),
            buffer.rope(),
            Encoding::Utf16,
            SyncKind::Incremental,
        );
        assert!(changes[0].range.is_some(), "and after it is gone, edits again");
    }
}
