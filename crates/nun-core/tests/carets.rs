//! Several carets at once: making them, and what happens to them afterwards.
//!
//! The model has been plural since the rope landed, so these are about the
//! operations that *create* carets and about the invariants that hold once
//! there are more than one of them.

use nun_core::{Buffer, Range, Selections};

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
    assert!(buffer.add_all_occurrences());
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
