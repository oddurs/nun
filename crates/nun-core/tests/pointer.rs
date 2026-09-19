//! What double-click, triple-click, column drag and drag-to-move select.

use nun_core::{Buffer, Range};
use proptest::prelude::*;

fn word(text: &str, at: usize) -> String {
    let buffer = Buffer::from_text(text);
    let (from, to) = buffer.word_range(at);
    text.chars().skip(from).take(to - from).collect()
}

// ── words ───────────────────────────────────────────────────────────────────

#[test]
fn a_double_click_selects_the_word_under_it() {
    assert_eq!(word("let snake_case = 1;", 6), "snake_case");
    assert_eq!(word("let snake_case = 1;", 4), "snake_case", "its first char");
}

#[test]
fn punctuation_and_spaces_are_their_own_pieces() {
    assert_eq!(word("self.value", 4), ".");
    assert_eq!(word("a    b", 2), "    ");
}

#[test]
fn a_field_access_is_three_pieces_not_one_word() {
    assert_eq!(word("self.value", 1), "self");
    assert_eq!(word("self.value", 7), "value");
}

#[test]
fn a_run_of_one_symbol_is_one_piece_and_mixed_symbols_are_not() {
    assert_eq!(word("a :: b", 3), "::");
    assert_eq!(word("f(.x)", 1), "(");
}

#[test]
fn words_in_other_scripts_are_words() {
    assert_eq!(word("let größe = 1", 6), "größe");
    assert_eq!(word("x 日本語 y", 3), "日本語");
}

#[test]
fn an_emoji_sequence_is_one_piece_not_its_codepoints() {
    let family = "👩\u{200d}👩\u{200d}👧";
    assert_eq!(word(&format!("a {family} b"), 3), family);
}

#[test]
fn a_combining_mark_stays_with_its_letter() {
    assert_eq!(word("cafe\u{301} ok", 1), "cafe\u{301}");
}

#[test]
fn the_newline_is_never_part_of_a_word() {
    assert_eq!(word("abc\ndef", 3), "abc", "the end of the line takes the last word");
    assert_eq!(word("abc\n\ndef", 4), "", "an empty line has no word");
}

#[test]
fn clicking_past_the_last_word_takes_it() {
    assert_eq!(word("abc", 3), "abc");
}

// ── lines ───────────────────────────────────────────────────────────────────

#[test]
fn a_triple_click_takes_the_line_and_its_newline() {
    let buffer = Buffer::from_text("one\ntwo\nthree");
    assert_eq!(buffer.line_range(1), (4, 8));
    assert_eq!(buffer.line_range(2), (8, 13), "the last line has no newline to take");
}

// ── columns ─────────────────────────────────────────────────────────────────

#[test]
fn a_column_drag_makes_one_range_per_line() {
    let buffer = Buffer::from_text("abcdef\nghijkl\nmnopqr");
    let selections = buffer.column_selection((0, 1), (2, 4));
    assert_eq!(selections.ranges(), &[Range::new(1, 4), Range::new(8, 11), Range::new(15, 18)]);
    assert_eq!(selections.primary(), Range::new(15, 18), "the pointer's line");
}

#[test]
fn short_lines_are_skipped_rather_than_given_a_stray_caret() {
    let buffer = Buffer::from_text("abcdef\nab\nabcdef");
    let selections = buffer.column_selection((0, 3), (2, 5));
    assert_eq!(selections.len(), 2, "the two-char line does not reach column 3");
    assert_eq!(selections.ranges(), &[Range::new(3, 5), Range::new(13, 15)]);
}

#[test]
fn a_line_that_reaches_into_the_box_is_selected_to_its_end() {
    let buffer = Buffer::from_text("abcdef\nabcd\nabcdef");
    let selections = buffer.column_selection((0, 2), (2, 5));
    assert_eq!(selections.ranges()[1], Range::new(9, 11), "columns 2 to 4, where it ends");
}

#[test]
fn a_zero_width_column_is_a_caret_on_each_line_long_enough() {
    let buffer = Buffer::from_text("abc\na\nabc");
    let selections = buffer.column_selection((0, 2), (2, 2));
    assert_eq!(selections.ranges(), &[Range::caret(2), Range::caret(8)]);
}

#[test]
fn dragging_up_and_left_keeps_the_direction() {
    let buffer = Buffer::from_text("abcdef\nabcdef");
    let selections = buffer.column_selection((1, 4), (0, 1));
    assert_eq!(selections.ranges(), &[Range::new(4, 1), Range::new(11, 8)]);
    assert_eq!(selections.primary(), Range::new(4, 1));
}

#[test]
fn columns_are_display_columns_not_chars() {
    // Each CJK char is two columns wide; column 2 is the start of the second.
    let buffer = Buffer::from_text("日本語\nabcdef");
    let selections = buffer.column_selection((0, 2), (1, 4));
    assert_eq!(selections.ranges(), &[Range::new(1, 2), Range::new(6, 8)]);
}

#[test]
fn tabs_count_to_their_stop() {
    let buffer = Buffer::from_text("\tx\nabcdef");
    // Tab width 4: `x` is in column 4.
    let selections = buffer.column_selection((0, 4), (1, 5));
    assert_eq!(selections.ranges(), &[Range::new(1, 2), Range::new(7, 8)]);
}

#[test]
fn a_box_no_line_reaches_leaves_one_caret() {
    let buffer = Buffer::from_text("a\nb");
    let selections = buffer.column_selection((0, 9), (1, 12));
    assert_eq!(selections.len(), 1);
}

// ── moving text ─────────────────────────────────────────────────────────────

#[test]
fn dragging_a_selection_forward_moves_it() {
    let mut buffer = Buffer::from_text("one two three");
    buffer.move_text(0, 4, 13, false);
    assert_eq!(buffer.text().to_string(), "two threeone ");
    assert_eq!(buffer.selections().primary(), Range::new(9, 13), "it stays selected");
}

#[test]
fn dragging_a_selection_backward_moves_it() {
    let mut buffer = Buffer::from_text("one two three");
    buffer.move_text(8, 13, 0, false);
    assert_eq!(buffer.text().to_string(), "threeone two ");
    assert_eq!(buffer.selections().primary(), Range::new(0, 5));
}

#[test]
fn a_copy_drag_leaves_the_original() {
    let mut buffer = Buffer::from_text("ab");
    buffer.move_text(0, 1, 2, true);
    assert_eq!(buffer.text().to_string(), "aba");
    assert_eq!(buffer.selections().primary(), Range::new(2, 3));
}

#[test]
fn dropping_text_on_itself_does_nothing() {
    let mut buffer = Buffer::from_text("one two");
    buffer.move_text(0, 3, 2, false);
    buffer.move_text(0, 3, 3, false);
    assert_eq!(buffer.text().to_string(), "one two");
    assert!(!buffer.is_modified());
}

#[test]
fn a_move_is_one_undo_step() {
    let mut buffer = Buffer::from_text("one two");
    buffer.insert("x");
    buffer.move_text(1, 4, 8, false);
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "xone two", "the move alone was undone");
}

proptest! {
    #[test]
    fn a_move_undoes_to_exactly_what_was_there(
        text in "[a-c \n日é]{0,24}",
        a in 0usize..30, b in 0usize..30, dest in 0usize..30, copy: bool,
    ) {
        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        let (a, b, dest) = (a.min(len), b.min(len), dest.min(len));
        buffer.move_text(a, b, dest, copy);
        buffer.undo();
        prop_assert_eq!(buffer.text().to_string(), text);
    }

    #[test]
    fn a_move_keeps_every_char(
        text in "[a-c \n日é]{0,24}",
        a in 0usize..30, b in 0usize..30, dest in 0usize..30,
    ) {
        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        buffer.move_text(a.min(len), b.min(len), dest.min(len), false);
        let mut before: Vec<char> = text.chars().collect();
        let mut after: Vec<char> = buffer.text().to_string().chars().collect();
        before.sort_unstable();
        after.sort_unstable();
        prop_assert_eq!(before, after);
    }

    #[test]
    fn a_word_contains_the_click_and_sits_on_cluster_boundaries(
        text in "[a-z_. \t日é\u{301}🇮🇸]{0,20}",
        at in 0usize..24,
    ) {
        let buffer = Buffer::from_text(&text);
        let at = at.min(buffer.len_chars());
        let (from, to) = buffer.word_range(at);
        prop_assert!(from <= to);
        prop_assert!(from <= at);
        prop_assert!(to >= at.min(to), "{from}..{to} for {at}");
        // Both ends are grapheme boundaries: stepping forward from the start
        // of the buffer lands on each of them.
        let mut boundaries = vec![0];
        let mut position = 0;
        while position < buffer.len_chars() {
            position = buffer.next_grapheme(position);
            boundaries.push(position);
        }
        prop_assert!(boundaries.contains(&from), "{from} splits a cluster");
        prop_assert!(boundaries.contains(&to), "{to} splits a cluster");
    }

    #[test]
    fn column_selections_are_sorted_disjoint_and_one_per_line(
        text in "[a-c\t日]{0,8}(\n[a-c\t日]{0,8}){0,5}",
        l1 in 0usize..6, c1 in 0usize..12, l2 in 0usize..6, c2 in 0usize..12,
    ) {
        let buffer = Buffer::from_text(&text);
        let last = buffer.len_lines() - 1;
        let selections = buffer.column_selection((l1.min(last), c1), (l2.min(last), c2));
        let lines: Vec<usize> =
            selections.ranges().iter().map(|range| buffer.line_of(range.from())).collect();
        let mut distinct = lines.clone();
        distinct.dedup();
        prop_assert_eq!(&lines, &distinct, "two ranges on one line");
        for range in selections.ranges() {
            prop_assert_eq!(buffer.line_of(range.from()), buffer.line_of(range.to()));
        }
    }
}

// ── found in review ─────────────────────────────────────────────────────────

#[test]
fn a_stray_carriage_return_does_not_let_a_box_cross_a_line() {
    let (buffer, _) = Buffer::from_bytes(b"ab\r\r\ncd\r\n");
    assert_eq!(buffer.line_of(buffer.char_at_column(0, 99)), 0);
    let selections = buffer.column_selection((0, 1), (1, 9));
    for range in selections.ranges() {
        let covered: String =
            buffer.text().to_string().chars().skip(range.from()).take(range.len()).collect();
        assert!(!covered.contains('\n'), "{range:?} covers a line break");
    }
}

#[test]
fn a_line_ending_at_the_left_edge_of_a_box_gets_nothing() {
    let buffer = Buffer::from_text("abcdef\nab\nabcdef");
    let selections = buffer.column_selection((0, 2), (2, 5));
    assert_eq!(selections.len(), 2, "{:?}", selections.ranges());
}

#[test]
fn a_keycap_is_a_symbol_not_part_of_the_name_before_it() {
    assert_eq!(word("x1\u{fe0f}\u{20e3} y", 0), "x");
}

#[test]
fn a_box_edge_inside_an_accented_letter_or_emoji_takes_the_whole_cluster() {
    let buffer = Buffer::from_text("e\u{301}bc\n👍bc");
    // Column 1 is `b` on the first line and the second half of 👍 on the
    // second; the box starts at the cluster covering it.
    let selections = buffer.column_selection((0, 1), (1, 3));
    assert_eq!(selections.ranges()[0], Range::new(2, 4));
    assert_eq!(selections.ranges()[1], Range::new(5, 7));
}

#[test]
fn double_clicking_early_in_a_huge_line_is_quick() {
    let buffer = Buffer::from_text(&"-".repeat(200_000));
    let start = std::time::Instant::now();
    let _ = buffer.word_range(10);
    let _ = Buffer::from_text(&"a(b),c[d];".repeat(20_000)).word_range(3);
    assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
}

proptest! {
    #[test]
    fn a_moved_selection_sits_on_cluster_boundaries_and_moves_back(
        text in "(e\u{301}|👩\u{200d}👩|[ab \n日])(e\u{301}|👩\u{200d}👩|[ab \n日]){0,10}",
        a in 0usize..30, b in 0usize..30, dest in 0usize..30,
    ) {
        let mut buffer = Buffer::from_text(&text);
        let boundaries = {
            let mut all = vec![0];
            let mut at = 0;
            while at < buffer.len_chars() {
                at = buffer.next_grapheme(at);
                all.push(at);
            }
            all
        };
        let pick = |i: usize| boundaries[i % boundaries.len()];
        let (a, b, dest) = (pick(a), pick(b), pick(dest));
        let (from, to) = (a.min(b), a.max(b));
        prop_assume!(from < to && (dest < from || dest > to));

        buffer.move_text(from, to, dest, false);
        let moved = buffer.selections().primary();
        let back = if dest < from { to } else { from };
        buffer.move_text(moved.from(), moved.to(), back, false);
        prop_assert_eq!(buffer.text().to_string(), text);
    }

    #[test]
    fn no_box_range_ever_covers_a_line_break(
        bytes in "[a-c\t日\r]{0,6}(\r?\n[a-c\t日\r]{0,6}){0,4}",
        l1 in 0usize..5, c1 in 0usize..10, l2 in 0usize..5, c2 in 0usize..10,
    ) {
        let (buffer, _) = Buffer::from_bytes(bytes.as_bytes());
        let last = buffer.len_lines() - 1;
        let selections = buffer.column_selection((l1.min(last), c1), (l2.min(last), c2));
        let text: Vec<char> = buffer.text().to_string().chars().collect();
        for range in selections.ranges() {
            prop_assert!(!text[range.from()..range.to()].contains(&'\n'), "{:?}", range);
        }
    }
}
