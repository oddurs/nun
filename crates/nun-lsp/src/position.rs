//! Between nun's char indices and a language server's positions.
//!
//! nun indexes text by char, as ropey does. A server counts a position as a
//! line and an offset into it, and the unit of that offset is negotiated: UTF-8
//! bytes, UTF-16 code units (the protocol's default, and what a server that
//! says nothing means), or UTF-32 code units, which are chars. Getting this
//! wrong is invisible in ASCII and wrong everywhere else: an emoji is one char,
//! two UTF-16 units and four bytes, and every position after it on the line
//! moves by the difference.
//!
//! Lines are the buffer's lines, broken at `\n` only. That agrees with the
//! protocol, which also breaks at `\r\n` and a lone `\r`, for any text without
//! a carriage return in it — which is every buffer, since loading turns `\r\n`
//! into `\n`. Text that does hold a `\r` is kept in step with the server by
//! sending it whole rather than as edits (see `sync`), so the server's copy is
//! never wrong; only a position on a line after such a `\r` can be.

use lsp_types::{Position, PositionEncodingKind};
use nun_core::Edit;
use ropey::Rope;

/// The unit a server counts offsets into a line in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Encoding {
    /// Bytes of UTF-8.
    Utf8,
    /// UTF-16 code units: what the protocol assumes when nothing was agreed.
    #[default]
    Utf16,
    /// UTF-32 code units, which are chars — nun's own unit, so the cheapest.
    Utf32,
}

impl Encoding {
    /// The encodings nun can use, most preferred first, as it offers them to a
    /// server in `general.positionEncodings`.
    ///
    /// UTF-32 first because it is nun's own unit and needs no conversion;
    /// UTF-8 next because ropey converts to it in logarithmic time from an
    /// index it keeps anyway; UTF-16 last, though it is exact too.
    pub const PREFERRED: [Self; 3] = [Self::Utf32, Self::Utf8, Self::Utf16];

    /// What the protocol calls it.
    #[must_use]
    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
            Self::Utf32 => PositionEncodingKind::UTF32,
        }
    }

    /// The encoding a server chose, from its capabilities.
    ///
    /// A server that names none uses UTF-16, the protocol's default. One that
    /// names something nun did not offer has broken the negotiation; UTF-16 is
    /// still the best guess, since it is the one every server must support.
    #[must_use]
    pub fn chosen(kind: Option<&PositionEncodingKind>) -> Self {
        match kind.map(PositionEncodingKind::as_str) {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    /// Where a char index in `text` falls, as a position in this encoding.
    ///
    /// An index past the end is taken as the end.
    #[must_use]
    pub fn position(self, text: &Rope, char: usize) -> Position {
        let char = char.min(text.len_chars());
        let line = text.char_to_line(char);
        let start = text.line_to_char(line);
        let offset = match self {
            Self::Utf32 => char - start,
            Self::Utf16 => text.char_to_utf16_cu(char) - text.char_to_utf16_cu(start),
            Self::Utf8 => text.char_to_byte(char) - text.char_to_byte(start),
        };
        Position { line: to_u32(line), character: to_u32(offset) }
    }

    /// The char index of a position in this encoding.
    ///
    /// As the protocol says: a line past the last is the end of the text, and
    /// an offset past the end of its line is the end of that line. An offset
    /// that falls inside a char — half a surrogate pair, the middle of a
    /// multi-byte sequence — is taken as that char's start, so a server that
    /// gets its arithmetic wrong still lands on a boundary.
    #[must_use]
    pub fn char_index(self, text: &Rope, position: Position) -> usize {
        let line = position.line as usize;
        if line >= text.len_lines() {
            return text.len_chars();
        }
        let start = text.line_to_char(line);
        let end = line_end(text, line);
        let offset = position.character as usize;
        match self {
            Self::Utf32 => (start + offset).min(end),
            Self::Utf16 => {
                let base = text.char_to_utf16_cu(start);
                let limit = text.char_to_utf16_cu(end);
                text.utf16_cu_to_char((base + offset).min(limit))
            }
            Self::Utf8 => {
                let base = text.char_to_byte(start);
                let limit = text.char_to_byte(end);
                text.byte_to_char((base + offset).min(limit))
            }
        }
    }

    /// A char range in `text` as a protocol range.
    #[must_use]
    pub fn range(self, text: &Rope, range: std::ops::Range<usize>) -> lsp_types::Range {
        lsp_types::Range {
            start: self.position(text, range.start),
            end: self.position(text, range.end),
        }
    }

    /// A protocol range as a char range in `text`, never inverted.
    #[must_use]
    pub fn char_range(self, text: &Rope, range: lsp_types::Range) -> std::ops::Range<usize> {
        let start = self.char_index(text, range.start);
        let end = self.char_index(text, range.end);
        start.min(end)..end.max(start)
    }

    /// A server's edits to `text` as buffer edits, in the order it sent them.
    ///
    /// Every range is converted against `text` as it stands — the protocol
    /// puts all of one batch's edits in the coordinates of the text before
    /// any of them — so this is called before the first is applied, and the
    /// result goes whole to `Buffer::apply_batch`, which sorts it, joins
    /// inserts at one position in this order, and turns the `\r\n` a server
    /// may write into the `\n` a buffer holds. A range out of bounds is
    /// clamped, as for [`Encoding::char_index`].
    #[must_use]
    pub fn edits(self, text: &Rope, edits: &[lsp_types::TextEdit]) -> Vec<Edit> {
        edits
            .iter()
            .map(|edit| {
                let range = self.char_range(text, edit.range);
                Edit::replace(range.start, range.end, edit.new_text.clone())
            })
            .collect()
    }

    /// The char offset into `line` of a position's `character`: what
    /// [`Encoding::char_index`] answers, for a line held on its own rather
    /// than in a rope — one read from a file nobody has open, say.
    ///
    /// `line` is the line without its line ending. As with a rope, an offset
    /// past the end is the end, and one inside a char is that char's start.
    #[must_use]
    pub fn column(self, line: &str, character: u32) -> usize {
        let target = character as usize;
        let mut units = 0;
        let mut count = 0;
        for ch in line.chars() {
            units += match self {
                Self::Utf8 => ch.len_utf8(),
                Self::Utf16 => ch.len_utf16(),
                Self::Utf32 => 1,
            };
            if units > target {
                return count;
            }
            count += 1;
        }
        count
    }
}

/// The char index just past the last char of `line`, before its `\n` or its
/// `\r\n`.
///
/// A buffer never holds `\r\n`, but the text of a file nobody has open is
/// converted as it is on disk, and there the `\r` belongs to the line ending:
/// a server's "past the end of the line" must not land between it and the
/// `\n`.
fn line_end(text: &Rope, line: usize) -> usize {
    let start = text.line_to_char(line);
    let slice = text.line(line);
    let mut len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
    }
    start + len
}

/// A count as the protocol's unsigned integer. Nothing in a buffer nun can hold
/// comes near four billion, but saturating is still the honest answer if it did.
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Encoding; 3] = [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32];

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn ascii_is_the_same_in_every_encoding() {
        let text = Rope::from_str("fn main() {\n    x\n}");
        for encoding in ALL {
            assert_eq!(encoding.position(&text, 16), at(1, 4), "{encoding:?}");
            assert_eq!(encoding.char_index(&text, at(1, 4)), 16, "{encoding:?}");
        }
    }

    #[test]
    fn an_astral_char_is_one_char_two_utf16_units_and_four_bytes() {
        // 😀 is U+1F600: outside the basic plane, a surrogate pair in UTF-16.
        let text = Rope::from_str("a😀b");
        assert_eq!(Encoding::Utf32.position(&text, 2), at(0, 2));
        assert_eq!(Encoding::Utf16.position(&text, 2), at(0, 3));
        assert_eq!(Encoding::Utf8.position(&text, 2), at(0, 5));
        assert_eq!(Encoding::Utf32.char_index(&text, at(0, 2)), 2);
        assert_eq!(Encoding::Utf16.char_index(&text, at(0, 3)), 2);
        assert_eq!(Encoding::Utf8.char_index(&text, at(0, 5)), 2);
    }

    #[test]
    fn half_a_surrogate_pair_lands_on_the_char_it_is_half_of() {
        let text = Rope::from_str("a😀b");
        assert_eq!(Encoding::Utf16.char_index(&text, at(0, 2)), 1);
        // The same for the middle of a multi-byte sequence.
        assert_eq!(Encoding::Utf8.char_index(&text, at(0, 3)), 1);
    }

    #[test]
    fn a_combining_mark_is_a_char_of_its_own() {
        // e + U+0301 COMBINING ACUTE ACCENT: one grapheme, two chars, three
        // bytes. Positions count units, not graphemes.
        let text = Rope::from_str("e\u{301}x");
        assert_eq!(Encoding::Utf32.position(&text, 2), at(0, 2));
        assert_eq!(Encoding::Utf16.position(&text, 2), at(0, 2));
        assert_eq!(Encoding::Utf8.position(&text, 2), at(0, 3));
    }

    #[test]
    fn a_line_on_its_own_converts_as_the_rope_would() {
        let line = "a😀中e\u{301}x";
        let text = Rope::from_str(line);
        for encoding in ALL {
            for character in 0..16 {
                assert_eq!(
                    encoding.column(line, character),
                    encoding.char_index(&text, at(0, character)),
                    "{encoding:?} at {character}"
                );
            }
        }
    }

    #[test]
    fn cjk_is_one_utf16_unit_and_three_bytes() {
        let text = Rope::from_str("中文\n中");
        assert_eq!(Encoding::Utf16.position(&text, 2), at(0, 2));
        assert_eq!(Encoding::Utf8.position(&text, 2), at(0, 6));
        assert_eq!(Encoding::Utf8.char_index(&text, at(1, 3)), 4);
    }

    #[test]
    fn offsets_count_from_the_start_of_their_own_line() {
        let text = Rope::from_str("😀😀\n😀x");
        assert_eq!(Encoding::Utf16.position(&text, 4), at(1, 2));
        assert_eq!(Encoding::Utf16.char_index(&text, at(1, 2)), 4);
    }

    #[test]
    fn past_the_end_of_a_line_is_the_end_of_that_line() {
        let text = Rope::from_str("ab\ncd");
        for encoding in ALL {
            assert_eq!(encoding.char_index(&text, at(0, 99)), 2, "{encoding:?}: not onto line 2");
        }
    }

    #[test]
    fn past_the_end_of_a_crlf_line_is_before_its_carriage_return() {
        let text = Rope::from_str("ab\r\ncd");
        for encoding in ALL {
            assert_eq!(encoding.char_index(&text, at(0, 99)), 2, "{encoding:?}");
            assert_eq!(encoding.char_index(&text, at(0, 2)), 2, "{encoding:?}");
        }
    }

    #[test]
    fn past_the_last_line_is_the_end_of_the_text() {
        let text = Rope::from_str("ab\ncd");
        for encoding in ALL {
            assert_eq!(encoding.char_index(&text, at(7, 0)), 5, "{encoding:?}");
            assert_eq!(encoding.position(&text, 99), at(1, 2), "{encoding:?}");
        }
    }

    #[test]
    fn a_trailing_newline_makes_an_empty_last_line_as_the_protocol_does() {
        let text = Rope::from_str("ab\n");
        for encoding in ALL {
            assert_eq!(encoding.position(&text, 3), at(1, 0), "{encoding:?}");
            assert_eq!(encoding.char_index(&text, at(1, 0)), 3, "{encoding:?}");
        }
    }

    #[test]
    fn the_empty_text_has_one_line() {
        let text = Rope::new();
        for encoding in ALL {
            assert_eq!(encoding.position(&text, 0), at(0, 0));
            assert_eq!(encoding.char_index(&text, at(0, 0)), 0);
        }
    }

    #[test]
    fn a_crlf_file_is_counted_as_the_buffer_holds_it() {
        // Loading turns \r\n into \n, so the server is sent — and counts in —
        // the text as the buffer holds it.
        let (buffer, _) = nun_core::Buffer::from_bytes(b"a\r\n\xf0\x9f\x98\x80b\r\nc");
        let text = buffer.rope();
        assert_eq!(Encoding::Utf16.position(text, 4), at(1, 3), "the emoji is two units");
        assert_eq!(Encoding::Utf16.char_index(text, at(2, 0)), 5);
    }

    #[test]
    fn a_lone_carriage_return_is_where_nun_and_the_protocol_disagree() {
        // The known limit, pinned so that it is a decision rather than a
        // surprise: the protocol starts a line after a lone \r, nun does not.
        // The server's copy of such a text is still exact (sync sends it
        // whole); a position after the \r is what differs.
        let text = Rope::from_str("a\rb");
        assert_eq!(Encoding::Utf16.position(&text, 2), at(0, 2), "the protocol would say (1, 0)");
    }

    #[test]
    fn only_a_line_feed_breaks_a_line() {
        // The whole module leans on ropey breaking lines at \n and nowhere
        // else, which is a feature flag away from not being true: its Unicode
        // line breaks would count U+2028 as a line the server does not see.
        let text = Rope::from_str("a\u{2028}b\u{85}c\u{b}d\u{c}e\rf\ng");
        assert_eq!(text.len_lines(), 2, "ropey's extra line breaks are switched on");
    }

    #[test]
    fn ranges_round_trip_and_never_come_back_inverted() {
        let text = Rope::from_str("a😀b\nc");
        for encoding in ALL {
            let range = encoding.range(&text, 1..5);
            assert_eq!(encoding.char_range(&text, range), 1..5, "{encoding:?}");
            let backwards = lsp_types::Range { start: range.end, end: range.start };
            assert_eq!(encoding.char_range(&text, backwards), 1..5, "{encoding:?}");
        }
    }

    #[test]
    fn a_servers_edits_arrive_as_char_ranges_in_the_order_sent() {
        // An emoji before each edit on its line: two UTF-16 units, one char.
        let text = Rope::from_str("😀ab\n😀cd");
        let edit = |line, from, to, new_text: &str| lsp_types::TextEdit {
            range: lsp_types::Range { start: at(line, from), end: at(line, to) },
            new_text: new_text.to_string(),
        };
        let edits = Encoding::Utf16
            .edits(&text, &[edit(1, 3, 4, "D"), edit(0, 2, 3, "A\r\n"), edit(0, 2, 2, "0")]);
        assert_eq!(
            edits,
            vec![Edit::replace(6, 7, "D"), Edit::replace(1, 2, "A\r\n"), Edit::insert(1, "0")],
            "converted one by one against the text as it was, line endings and all"
        );
        let mut buffer = nun_core::Buffer::from_text("😀ab\n😀cd");
        buffer.apply_batch(edits).unwrap();
        assert_eq!(buffer.text().to_string(), "😀A\n0b\n😀cD");
    }

    #[test]
    fn the_chosen_encoding_defaults_to_utf16() {
        assert_eq!(Encoding::chosen(None), Encoding::Utf16);
        assert_eq!(Encoding::chosen(Some(&PositionEncodingKind::UTF8)), Encoding::Utf8);
        assert_eq!(Encoding::chosen(Some(&PositionEncodingKind::UTF32)), Encoding::Utf32);
        assert_eq!(Encoding::chosen(Some(&PositionEncodingKind::new("utf-7"))), Encoding::Utf16);
    }

    proptest::proptest! {
        #[test]
        fn every_char_index_round_trips_in_every_encoding(text in "[a\\n😀é\\u{301}中\\t]{0,40}") {
            let rope = Rope::from_str(&text);
            for encoding in ALL {
                for char in 0..=rope.len_chars() {
                    let position = encoding.position(&rope, char);
                    proptest::prop_assert_eq!(encoding.char_index(&rope, position), char);
                }
            }
        }

        #[test]
        fn positions_agree_with_counting_by_hand(text in "[a\\n😀é\\u{301}中]{0,40}", pick in 0usize..41) {
            let rope = Rope::from_str(&text);
            let char = pick.min(rope.len_chars());
            let before: String = text.chars().take(char).collect();
            let line = before.matches('\n').count();
            let tail = before.rsplit('\n').next().unwrap_or("");
            let expect = |units: usize| at(u32::try_from(line).unwrap(), u32::try_from(units).unwrap());
            proptest::prop_assert_eq!(Encoding::Utf8.position(&rope, char), expect(tail.len()));
            proptest::prop_assert_eq!(Encoding::Utf16.position(&rope, char), expect(tail.encode_utf16().count()));
            proptest::prop_assert_eq!(Encoding::Utf32.position(&rope, char), expect(tail.chars().count()));
        }
    }
}
