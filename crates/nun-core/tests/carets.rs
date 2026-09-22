//! Several carets at once: making them, and what happens to them afterwards.
//!
//! The model has been plural since the rope landed, so these are about the
//! operations that *create* carets and about the invariants that hold once
//! there are more than one of them.

use nun_core::{AllOccurrences, Buffer, MOST_OCCURRENCES, Range, Selections};

/// A buffer holding `text`, with one caret at `at`.
fn buffer(text: &str, at: usize) -> Buffer {
    let mut buffer = Buffer::from_text(text);
    buffer.set_selections(Selections::single(Range::caret(at)));
    buffer
}

/// Every selection as `(from, to)`, in order.
fn spans(buffer: &Buffer) -> Vec<(usize, usize)> {
    buffer.selections().ranges().iter().map(|range| (range.from(), range.to())).collect()
}

#[test]
fn a_caret_added_below_lands_on_the_column_above_it() {
    let mut buffer = buffer("alpha\nbeta\n", 3);
    buffer.add_caret_vertically(false);
    assert_eq!(spans(&buffer), [(3, 3), (9, 9)], "same column on the next line");
}

#[test]
fn a_column_of_carets_survives_a_short_line_between_long_ones() {
    // The column each caret aims for is remembered, so walking down past a
    // short line comes out straight again rather than collapsing against it.
    let mut buffer = buffer("alphabet\nno\nalphabet\n", 7);
    buffer.add_caret_vertically(false);
    buffer.add_caret_vertically(false);
    let last = spans(&buffer).last().copied().unwrap();
    assert_eq!(last, (19, 19), "back out at column seven, not stuck at two");
}

#[test]
fn a_caret_at_the_top_adds_nothing_rather_than_stacking() {
    let mut buffer = buffer("alpha\nbeta\n", 2);
    buffer.add_caret_vertically(true);
    assert_eq!(spans(&buffer), [(2, 2)], "nowhere to go, and no second caret on the same spot");
}

#[test]
fn the_first_press_selects_the_word_and_the_next_finds_another() {
    let mut buffer = buffer("let cat = cat + 1;\n", 5);
    assert!(buffer.add_next_occurrence(), "the word under the caret");
    assert_eq!(spans(&buffer), [(4, 7)]);

    assert!(buffer.add_next_occurrence(), "and the next one along");
    assert_eq!(spans(&buffer), [(4, 7), (10, 13)]);
}

#[test]
fn looking_for_another_occurrence_wraps_and_then_stops() {
    let mut buffer = buffer("cat\ncat\n", 0);
    assert!(buffer.add_next_occurrence(), "the word");
    assert!(buffer.add_next_occurrence(), "the second");
    assert_eq!(spans(&buffer), [(0, 3), (4, 7)]);
    assert!(
        !buffer.add_next_occurrence(),
        "every occurrence is taken, so it says so rather than cycling"
    );
    assert_eq!(spans(&buffer), [(0, 3), (4, 7)], "and nothing moved");
}

#[test]
fn all_occurrences_at_once_keeps_the_one_being_driven() {
    let mut buffer = buffer("cat dog cat cat\n", 8);
    assert!(matches!(buffer.add_all_occurrences(), AllOccurrences::Selected(_)));
    assert_eq!(spans(&buffer), [(0, 3), (8, 11), (12, 15)]);
    assert_eq!(buffer.selections().primary(), Range::new(8, 11), "the one under the caret");
}

#[test]
fn splitting_a_selection_gives_one_per_line() {
    let mut buffer = Buffer::from_text("one\ntwo\nthree\n");
    buffer.set_selections(Selections::single(Range::new(1, 11)));
    assert!(buffer.split_into_lines());
    assert_eq!(spans(&buffer), [(1, 3), (4, 7), (8, 11)]);
}

#[test]
fn splitting_something_on_one_line_does_nothing_and_says_so() {
    let mut buffer = Buffer::from_text("one\ntwo\n");
    buffer.set_selections(Selections::single(Range::new(0, 3)));
    assert!(!buffer.split_into_lines(), "there is nothing to split");
    assert_eq!(spans(&buffer), [(0, 3)]);
}

#[test]
fn carets_that_collide_after_an_edit_merge_and_keep_a_primary() {
    // Two carets a character apart, both deleting backwards, land on the same
    // spot. One caret must come out of that, and it must still be primary.
    let mut buffer = Buffer::from_text("ab\n");
    buffer.set_selections(Selections::new(vec![Range::caret(1), Range::caret(2)], 1));
    buffer.delete_backward();
    assert_eq!(buffer.text().to_string(), "\n", "both characters went");
    assert_eq!(buffer.selections().len(), 1, "and the carets merged");
    assert_eq!(buffer.selections().primary(), Range::caret(0));
}

#[test]
fn typing_with_five_hundred_carets_stays_quick() {
    // One caret per line, a character typed into every one of them. The point
    // is that this is a single edit rather than five hundred.
    let mut buffer = Buffer::from_text(&"placeholder\n".repeat(500));
    let carets: Vec<Range> = (0..500).map(|line| Range::caret(buffer.line_start(line))).collect();
    buffer.set_selections(Selections::new(carets, 0));

    let started = std::time::Instant::now();
    for _ in 0..10 {
        buffer.insert("x");
    }
    let each = started.elapsed() / 10;

    assert_eq!(buffer.selections().len(), 500, "every caret survived");
    assert!(buffer.line_text(0).starts_with("xxxxxxxxxx"), "{:?}", buffer.line_text(0));
    assert!(each < std::time::Duration::from_millis(8), "a keystroke took {each:?}");
}

#[test]
fn a_run_of_typing_at_several_carets_is_one_undo_step() {
    // Undo takes back the same amount of typing whether there is one caret or
    // three, or the caret count leaks into what every other command means.
    let mut buffer = Buffer::from_text("a\nb\nc\n");
    buffer.set_selections(Selections::new(
        vec![Range::caret(1), Range::caret(3), Range::caret(5)],
        0,
    ));
    for ch in ["x", "y", "z"] {
        buffer.insert(ch);
    }
    assert_eq!(buffer.text().to_string(), "axyz\nbxyz\ncxyz\n");
    assert!(buffer.undo());
    assert_eq!(buffer.text().to_string(), "a\nb\nc\n", "three keystrokes, one step");
    assert_eq!(spans(&buffer), [(1, 1), (3, 3), (5, 5)], "with every caret put back");
    assert!(buffer.redo());
    assert_eq!(buffer.text().to_string(), "axyz\nbxyz\ncxyz\n");
}

#[test]
fn a_run_of_backspaces_at_several_carets_is_one_undo_step() {
    let mut buffer = Buffer::from_text("abc\ndef\n");
    buffer.set_selections(Selections::new(vec![Range::caret(3), Range::caret(7)], 1));
    buffer.delete_backward();
    buffer.delete_backward();
    assert_eq!(buffer.text().to_string(), "a\nd\n");
    assert!(buffer.undo());
    assert_eq!(buffer.text().to_string(), "abc\ndef\n", "two backspaces, one step");
}

#[test]
fn carets_merging_mid_run_starts_a_new_undo_step() {
    // Two carets backspacing towards each other meet; from then on there is
    // one edit per keystroke rather than two, and the run cannot carry on.
    let mut buffer = Buffer::from_text("ab\n");
    buffer.set_selections(Selections::new(vec![Range::caret(1), Range::caret(2)], 0));
    buffer.insert("x");
    buffer.delete_backward();
    buffer.delete_backward();
    let after = buffer.text().to_string();
    while buffer.undo() {}
    assert_eq!(buffer.text().to_string(), "ab\n");
    while buffer.redo() {}
    assert_eq!(buffer.text().to_string(), after);
}

// ── what the guard found ────────────────────────────────────────────────────

#[test]
fn going_up_drives_the_caret_at_the_top_not_the_one_at_the_bottom() {
    // The view follows the primary, so if going up leaves the primary at the
    // bottom the new carets march off the top of the screen unseen.
    let mut buffer = buffer(&"line\n".repeat(20), 50);
    for _ in 0..5 {
        buffer.add_caret_vertically(true);
    }
    let lines: Vec<usize> =
        buffer.selections().ranges().iter().map(|r| buffer.line_of(r.head)).collect();
    assert_eq!(lines, [5, 6, 7, 8, 9, 10]);
    assert_eq!(buffer.line_of(buffer.selections().primary().head), 5, "the frontier, not line 9");
}

#[test]
fn going_down_drives_the_caret_at_the_bottom() {
    let mut buffer = buffer(&"line\n".repeat(20), 50);
    for _ in 0..5 {
        buffer.add_caret_vertically(false);
    }
    assert_eq!(buffer.line_of(buffer.selections().primary().head), 15);
}

#[test]
fn a_selection_made_leftwards_is_still_the_one_being_driven() {
    // Range equality tells a leftward drag from a rightward one, which is
    // right for dragging and wrong for asking "is this the same text".
    let mut buffer = Buffer::from_text("cat dog cat cat\n");
    buffer.set_selections(Selections::single(Range::new(11, 8)));
    assert!(matches!(buffer.add_all_occurrences(), AllOccurrences::Selected(_)));
    assert_eq!(
        (buffer.selections().primary().from(), buffer.selections().primary().to()),
        (8, 11),
        "not the first match in the file"
    );
}

#[test]
fn an_occurrence_already_taken_leftwards_is_not_added_again() {
    let mut buffer = Buffer::from_text("cat cat\n");
    buffer.set_selections(Selections::new(vec![Range::new(0, 3), Range::new(7, 4)], 0));
    assert!(
        !buffer.add_next_occurrence(),
        "both are taken, so it says so rather than reporting a caret it did not add"
    );
    assert_eq!(buffer.selections().len(), 2);
}

#[test]
fn a_match_inside_a_grapheme_cluster_is_not_a_match() {
    // Searching finds byte runs, and UTF-8 makes those char boundaries for
    // free — but not cluster boundaries. Selecting the `e` of `he` must not
    // find the `e` that starts `é`.
    let mut buffer = Buffer::from_text("he\ne\u{301}x\n");
    buffer.set_selections(Selections::single(Range::new(1, 2)));
    assert!(!buffer.add_next_occurrence(), "the only other `e` is inside a cluster");
    assert_eq!(spans(&buffer), [(1, 2)], "and nothing was added");
}

#[test]
fn half_a_flag_is_not_an_occurrence_of_the_other_half() {
    // Two regional indicators make one flag. Matching one of them alone would
    // select half a cluster, and typing over it would leave the other half
    // glued to whatever was typed.
    let flags = "\u{1F1EE}\u{1F1F8} \u{1F1EE}\u{1F1F1}\n";
    let mut buffer = Buffer::from_text(flags);
    buffer.set_selections(Selections::single(Range::new(0, 1)));
    assert!(!buffer.add_next_occurrence(), "the other IS is inside the second flag");
    assert_eq!(spans(&buffer), [(0, 1)]);
}

#[test]
fn every_occurrence_skips_the_ones_inside_clusters() {
    let mut buffer = Buffer::from_text("e he\ne\u{301}\n");
    buffer.set_selections(Selections::single(Range::new(0, 1)));
    assert!(matches!(buffer.add_all_occurrences(), AllOccurrences::Selected(_)));
    assert_eq!(spans(&buffer), [(0, 1), (3, 4)], "not the `e` that starts `é`");
}

// ── what the second review found ────────────────────────────────────────────

#[test]
fn a_rejected_match_does_not_hide_a_real_one_overlapping_it() {
    // éée, decomposed. Looking for the trailing "ée" first finds the "e" at
    // 0, whose end falls inside the second é; the real occurrence at 2 starts
    // inside that rejected match and has to be found anyway.
    let text = "e\u{301}e\u{301}e\n";
    let mut buffer = Buffer::from_text(text);
    buffer.set_selections(Selections::single(Range::new(2, 5)));
    assert_eq!(buffer.add_all_occurrences(), AllOccurrences::Selected(1));
    assert_eq!(spans(&buffer), [(2, 5)], "the selection is its own occurrence");

    let mut buffer = Buffer::from_text("e\u{301}e\u{301}e\ne\u{301}e\n");
    buffer.set_selections(Selections::single(Range::new(6, 9)));
    assert!(buffer.add_next_occurrence(), "the one on the first line is real");
    assert_eq!(spans(&buffer), [(2, 5), (6, 9)]);
}

#[test]
fn an_occurrence_overlapping_a_selection_is_skipped_not_merged() {
    // Wrapping round `aaaa` from [1,3) first finds [0,2), which overlaps the
    // selection; taking it would merge the two into [0,3), which is not an
    // occurrence of anything.
    let mut buffer = Buffer::from_text("aaaa\n");
    buffer.set_selections(Selections::single(Range::new(1, 3)));
    assert!(!buffer.add_next_occurrence(), "every other `aa` overlaps it");
    assert_eq!(spans(&buffer), [(1, 3)]);
}

#[test]
fn every_occurrence_of_an_overlapping_needle_keeps_the_one_in_hand() {
    let mut buffer = Buffer::from_text("aaaaaa\n");
    buffer.set_selections(Selections::single(Range::new(1, 3)));
    assert_eq!(buffer.add_all_occurrences(), AllOccurrences::Selected(2));
    // [3,5) touches the one held, and selections that meet are one selection.
    assert_eq!(spans(&buffer), [(1, 3), (4, 6)], "left to right around the one held");
    assert_eq!(buffer.selections().primary(), Range::new(1, 3), "which is still driven");
}

#[test]
fn too_many_occurrences_selects_nothing_and_says_so() {
    let text = "x ".repeat(MOST_OCCURRENCES + 1);
    let mut buffer = Buffer::from_text(&text);
    buffer.set_selections(Selections::single(Range::new(0, 1)));
    assert_eq!(buffer.add_all_occurrences(), AllOccurrences::TooMany { limit: MOST_OCCURRENCES });
    assert_eq!(spans(&buffer), [(0, 1)], "nothing changed");
}

#[test]
fn splitting_whole_lines_leaves_no_caret_on_the_line_after() {
    let mut buffer = Buffer::from_text("one\ntwo\nthree\n");
    buffer.set_selections(Selections::single(Range::new(0, 8)));
    assert!(buffer.split_into_lines());
    assert_eq!(spans(&buffer), [(0, 3), (4, 7)], "not a stray caret at the start of `three`");
}

#[test]
fn a_column_of_carets_lands_by_display_column_over_wide_text_and_tabs() {
    // Column three is after `abc`, before 本 (日 is two columns wide), and
    // inside the tab — which the caret cannot be, so it lands after it.
    let mut buffer = buffer("abc\n日本語\n\tx\n", 3);
    buffer.add_caret_vertically(false);
    buffer.add_caret_vertically(false);
    let columns: Vec<usize> =
        buffer.selections().ranges().iter().map(|r| buffer.column_of(r.head)).collect();
    assert_eq!(spans(&buffer)[1], (5, 5), "before 本");
    assert_eq!(columns[..2], [3, 2], "{columns:?}");
}
