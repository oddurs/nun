//! Grapheme-cluster and display-width helpers, scoped to a single line.
//!
//! Buffers normalise to `\n` internally, and no grapheme cluster spans a line
//! feed once CRLF is gone, so every one of these can work on a line in
//! isolation. That is both simpler and far less error-prone than feeding a
//! chunk-wise cursor across the whole rope.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Char offset of the first grapheme boundary strictly after `char_off`.
///
/// An offset in the middle of a cluster advances to the end of that cluster,
/// which is what makes a right-arrow step over a family emoji rather than into
/// the middle of its zero-width joiners.
pub(crate) fn next_boundary(line: &str, char_off: usize) -> usize {
    let mut start = 0;
    for cluster in line.graphemes(true) {
        let len = cluster.chars().count();
        if char_off < start + len {
            return start + len;
        }
        start += len;
    }
    start
}

/// Char offset of the last grapheme boundary strictly before `char_off`.
pub(crate) fn prev_boundary(line: &str, char_off: usize) -> usize {
    let mut previous = 0;
    let mut start = 0;
    for cluster in line.graphemes(true) {
        if start >= char_off {
            break;
        }
        previous = start;
        start += cluster.chars().count();
    }
    previous
}

/// Display columns occupied by `line[..char_off]`.
///
/// Tabs advance to the next multiple of `tab_width`; combining marks are zero
/// columns and most CJK and emoji are two, which is why this cannot be a char
/// count.
pub(crate) fn width_to(line: &str, char_off: usize, tab_width: usize) -> usize {
    let mut chars = 0;
    let mut col = 0;
    for cluster in line.graphemes(true) {
        if chars >= char_off {
            break;
        }
        col += cluster_width(cluster, col, tab_width);
        chars += cluster.chars().count();
    }
    col
}

/// Char offset whose display column is nearest to `target_col` without passing it.
pub(crate) fn char_off_at_width(line: &str, target_col: usize, tab_width: usize) -> usize {
    let mut chars = 0;
    let mut col = 0;
    for cluster in line.graphemes(true) {
        // Not only a bare `\n`: a stray `\r` before the line ending makes
        // `\r\n` one cluster, and walking into it would land on the next line.
        if cluster.ends_with('\n') {
            break;
        }
        let width = cluster_width(cluster, col, tab_width);
        if col + width > target_col {
            return chars;
        }
        col += width;
        chars += cluster.chars().count();
    }
    chars
}

/// The word, space run or punctuation run containing `char_off`, as char
/// offsets `(start, end)` within `line`.
///
/// Word boundaries for code, not for prose. Unicode's word rules treat
/// `self.value` and `e.g` as single words, because a full stop between letters
/// joins them — right for a sentence, wrong for a field access. So each
/// grapheme cluster is classed as part of a word (letters, digits,
/// underscore, in any script), whitespace, or a symbol, and a piece is a run
/// of word clusters, a run of whitespace, or a run of one repeated symbol
/// (`::`, `==`). A cluster is classed by its first char, so a combining mark
/// stays with its letter and an emoji sequence stays whole.
///
/// The line's newline is never part of a piece; an offset at or past the end
/// of the line takes the last piece, and an empty line gives an empty range.
/// The scan stops at the end of the piece under the click, so a double-click
/// near the start of a very long line costs the start of the line.
pub(crate) fn word_bounds(line: &str, char_off: usize) -> (usize, usize) {
    let line = line.strip_suffix('\n').unwrap_or(line);

    let mut piece = (0, 0);
    let mut previous: Option<(&str, Class)> = None;
    for cluster in line.graphemes(true) {
        let class = Class::of(cluster);
        let joins = previous.is_some_and(|(last, last_class)| {
            last_class == class && (class != Class::Symbol || last == cluster)
        });
        if !joins {
            if char_off < piece.1 {
                return piece;
            }
            piece = (piece.1, piece.1);
        }
        piece.1 += cluster.chars().count();
        previous = Some((cluster, class));
    }
    piece
}

/// How a cluster behaves under a double-click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Symbol,
}

impl Class {
    fn of(cluster: &str) -> Self {
        // A keycap (`1️⃣`) or any emoji-presentation sequence starts with an
        // ordinary digit or letter but is a picture, not part of a name.
        if cluster.contains(['\u{20e3}', '\u{fe0f}']) {
            return Self::Symbol;
        }
        match cluster.chars().next() {
            Some(ch) if ch.is_alphanumeric() || ch == '_' => Self::Word,
            Some(ch) if ch.is_whitespace() => Self::Space,
            _ => Self::Symbol,
        }
    }
}

/// Columns one cluster occupies when it begins at column `col`.
fn cluster_width(cluster: &str, col: usize, tab_width: usize) -> usize {
    if cluster == "\t" {
        tab_width - (col % tab_width)
    } else {
        // `width()` reports 0 for control characters; a caret still has to be
        // able to sit on one, so give it a column.
        cluster.width().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_over_a_zwj_sequence_as_one_cluster() {
        let line = "a👨‍👩‍👧b";
        assert_eq!(next_boundary(line, 0), 1);
        // The family is five chars: three people and two joiners.
        assert_eq!(next_boundary(line, 1), 6);
        assert_eq!(prev_boundary(line, 6), 1);
    }

    #[test]
    fn steps_over_a_combining_mark_as_one_cluster() {
        let line = "e\u{0301}x"; // e + combining acute
        assert_eq!(next_boundary(line, 0), 2);
        assert_eq!(prev_boundary(line, 2), 0);
    }

    #[test]
    fn mid_cluster_offsets_snap_outward() {
        let line = "e\u{0301}x";
        assert_eq!(next_boundary(line, 1), 2, "advances to the end of the cluster");
        assert_eq!(prev_boundary(line, 1), 0, "retreats to the start of the cluster");
    }

    #[test]
    fn cjk_is_two_columns_and_marks_are_zero() {
        assert_eq!(width_to("日本語", 3, 4), 6);
        assert_eq!(width_to("e\u{0301}", 2, 4), 1);
    }

    #[test]
    fn tabs_advance_to_the_next_stop() {
        assert_eq!(width_to("\t", 1, 4), 4);
        assert_eq!(width_to("ab\t", 3, 4), 4);
        assert_eq!(width_to("abcd\t", 5, 4), 8);
    }

    #[test]
    fn column_maps_back_to_a_char_offset() {
        assert_eq!(char_off_at_width("日本語", 4, 4), 2);
        assert_eq!(char_off_at_width("日本語", 3, 4), 1, "lands before a wide cluster");
        assert_eq!(char_off_at_width("abc", 99, 4), 3, "clamps to the line length");
    }
}
