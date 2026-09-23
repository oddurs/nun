//! Writing a line of text into cells, clipped to the room it has.
//!
//! One writer for every widget, because clipping is where width bugs live: a
//! wide character split across the edge, a combining mark separated from its
//! base, an ellipsis that pushes the text one cell too far. Each of those was
//! once fixed in one widget's copy of this and not in another's.

use std::ops::Range;

use ratatui::buffer::{Buffer as Cells, CellWidth};
use ratatui::style::Style;
use unicode_segmentation::UnicodeSegmentation;

/// What stands in for a control character, which would move the terminal's
/// cursor rather than draw.
const REPLACEMENT: &str = "\u{fffd}";

/// `text` a grapheme cluster at a time, as the writers here draw it: each
/// cluster with the cells it takes, a control character swapped for `�`, and
/// a cluster that takes no cells — a zero-width space, a bidirectional mark, a
/// combining mark with nothing to sit on — left out, since a cell has to hold
/// something that advances the cursor.
///
/// Widths are ratatui's own, which is what its diff moves the cursor by, so
/// what is measured here is what reaches the terminal.
pub fn clusters(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.graphemes(true).filter_map(|cluster| {
        let cluster = if cluster.chars().any(char::is_control) { REPLACEMENT } else { cluster };
        let cells = usize::from(cluster.cell_width());
        (cells > 0).then_some((cluster, cells))
    })
}

/// The cells `text` takes as [`clusters`] draws it.
#[must_use]
pub fn text_width(text: &str) -> usize {
    clusters(text).map(|(_, cells)| cells).sum()
}

/// Write `text` at `(x, y)` in at most `room` columns, clipping at a cluster
/// rather than splitting one, and ending with `ellipsis` when it had to clip.
pub(crate) fn write(
    cells: &mut Cells,
    x: u16,
    y: u16,
    room: u16,
    text: &str,
    style: Style,
    ellipsis: &str,
) {
    write_styled(cells, x, y, room, text, style, ellipsis, |_| style);
}

/// Write `text` like [`write`], each cluster in the style `style_of` gives the
/// chars it is made of, as a range of char indices into `text`.
///
/// The range is what makes a highlight land on the right columns when the
/// line holds a wide character, a combining mark or an emoji: highlights are
/// counted in chars and the screen in columns, and this walk is the only
/// honest way between the two. The ellipsis is drawn in `style`.
#[allow(clippy::too_many_arguments)] // Each one is a separate thing to draw.
pub(crate) fn write_styled(
    cells: &mut Cells,
    x: u16,
    y: u16,
    room: u16,
    text: &str,
    style: Style,
    ellipsis: &str,
    style_of: impl Fn(Range<u32>) -> Style,
) {
    let room = usize::from(room);
    // Summed a cluster at a time, the way it is drawn: the whole string's
    // width can differ from its clusters' (a lam-alef pair is one ligature to
    // unicode-width and two clusters here).
    let fits = text_width(text) <= room;
    let ellipsis_cells = text_width(ellipsis);
    let budget = if fits { room } else { room.saturating_sub(ellipsis_cells) };

    let mut column = 0usize;
    let mut chars = 0u32;
    for cluster in text.graphemes(true) {
        let from = chars;
        chars = chars.saturating_add(u32::try_from(cluster.chars().count()).unwrap_or(1));
        let Some((symbol, width)) = clusters(cluster).next() else { continue };
        if column + width > budget {
            break;
        }
        put(cells, x, y, column, symbol, width, style_of(from..chars));
        column += width;
    }
    if !fits && ellipsis_cells > 0 && ellipsis_cells <= room {
        for (symbol, width) in clusters(ellipsis) {
            put(cells, x, y, column, symbol, width, style);
            column += width;
        }
    }
}

/// One cluster at `column` past `x`, with the cells a wide one covers blanked
/// so nothing left over from the last frame shows through them.
fn put(
    cells: &mut Cells,
    x: u16,
    y: u16,
    column: usize,
    cluster: &str,
    width: usize,
    style: Style,
) {
    let Ok(offset) = u16::try_from(column) else { return };
    cells[(x + offset, y)].set_symbol(cluster).set_style(style);
    for extra in 1..width {
        let Ok(extra) = u16::try_from(column + extra) else { break };
        cells[(x + extra, y)].set_symbol(" ").set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn line(room: u16, text: &str, ellipsis: &str) -> String {
        let mut cells = Cells::empty(Rect::new(0, 0, 10, 1));
        write(&mut cells, 0, 0, room, text, Style::default(), ellipsis);
        (0..10).map(|x| cells[(x, 0)].symbol().to_string()).collect::<String>().trim_end().into()
    }

    #[test]
    fn text_that_fits_is_written_whole() {
        assert_eq!(line(5, "hello", "…"), "hello");
    }

    #[test]
    fn text_that_does_not_fit_ends_in_the_ellipsis_it_was_given() {
        assert_eq!(line(4, "hello", "…"), "hel…");
        assert_eq!(line(4, "hello", "~"), "hel~");
    }

    #[test]
    fn a_wide_character_is_never_split_by_the_edge() {
        // 中 is two cells: with three to spend and one kept for the ellipsis,
        // the second 中 does not fit and is dropped whole.
        assert_eq!(line(3, "中中x", "…"), "中 …");
    }

    #[test]
    fn a_combining_mark_stays_with_its_base() {
        assert_eq!(line(2, "e\u{301}xyz", "…"), "e\u{301}…");
    }

    #[test]
    fn a_zero_width_cluster_takes_no_cell_even_at_an_exact_fit() {
        // Three cells of room and three of text: the zero-width space after
        // them must not land in a fourth.
        let mut cells = Cells::empty(Rect::new(0, 0, 3, 1));
        write(&mut cells, 0, 0, 3, "abc\u{200b}", Style::default(), "…");
        let drawn: String = (0..3).map(|x| cells[(x, 0)].symbol().to_string()).collect();
        assert_eq!(drawn, "abc");
    }

    #[test]
    fn a_control_character_is_drawn_as_a_replacement_not_sent() {
        assert_eq!(line(5, "a\tb", "…"), "a\u{fffd}b");
        assert_eq!(line(5, "a\r\nb", "…"), "a\u{fffd}b");
    }

    #[test]
    fn whether_text_fits_is_measured_the_way_it_is_drawn() {
        // unicode-width reads "لالا" as two ligatures, two cells; drawn a
        // cluster at a time it is four, so three cells of room must clip it.
        assert_eq!(line(3, "لالا", "…"), "لا…");
    }

    #[test]
    fn a_halfwidth_sound_mark_gets_the_cell_the_terminal_gives_it() {
        assert_eq!(text_width("\u{ff76}\u{ff9e}"), 2);
        assert_eq!(line(3, "\u{ff76}\u{ff9e}\u{ff72}", "…"), "\u{ff76}\u{ff9e} \u{ff72}");
    }

    #[test]
    fn an_ellipsis_of_several_chars_is_drawn_whole() {
        assert_eq!(line(3, "hello", "~\u{301}"), "he~\u{301}");
    }

    #[test]
    fn no_room_writes_nothing() {
        assert_eq!(line(0, "hello", "…"), "");
    }

    #[test]
    fn styles_follow_char_ranges_not_columns() {
        let mut cells = Cells::empty(Rect::new(0, 0, 10, 1));
        let marked = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
        // Char 1 is the combining mark: it colours the cluster it belongs to.
        write_styled(&mut cells, 0, 0, 10, "e\u{301}中x", Style::default(), "…", |range| {
            if range.contains(&1) { marked } else { Style::default() }
        });
        assert_eq!(cells[(0, 0)].modifier, marked.add_modifier);
        assert!(cells[(1, 0)].modifier.is_empty(), "中 is not marked");
    }
}
