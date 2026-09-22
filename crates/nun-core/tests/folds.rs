//! Folding: text out of sight, and the rules that keep it honest.

use nun_core::{Buffer, Range, Selections};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};

/// As in `properties.rs`: failures are reproduced from the printed seed rather
/// than from a regression file an integration test has nowhere to keep.
fn config() -> Config {
    Config { failure_persistence: Some(Box::new(FileFailurePersistence::Off)), ..Config::default() }
}

/// Ten numbered lines, the caret at the start.
fn ten() -> Buffer {
    let mut text = String::new();
    for n in 0..10 {
        text.push_str("line ");
        text.push_str(&n.to_string());
        text.push('\n');
    }
    Buffer::from_text(&text)
}

fn caret_line(buffer: &Buffer) -> usize {
    buffer.line_of(buffer.selections().primary().head)
}

#[test]
fn a_fold_hides_the_lines_under_its_header() {
    let mut buffer = ten();
    assert!(buffer.fold(2, 5));
    let hidden = buffer.hidden();
    assert!(!hidden.is_hidden(2), "the header stays");
    assert!((3..=5).all(|line| hidden.is_hidden(line)));
    assert!(!hidden.is_hidden(6));
    assert_eq!(buffer.folded(), [(2, 5)]);
}

#[test]
fn a_region_must_hide_at_least_one_real_line() {
    let mut buffer = ten();
    assert!(!buffer.fold(4, 4), "nothing under it");
    assert!(!buffer.fold(4, 3), "backwards");
    assert!(!buffer.fold(4, 99), "past the end");
    assert!(buffer.folded().is_empty());
}

#[test]
fn up_and_down_step_over_a_fold_whole() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    buffer.set_selections(Selections::single(Range::caret(buffer.line_start(2) + 3)));
    buffer.move_down(false);
    assert_eq!(caret_line(&buffer), 6, "from the header to the line after the fold");
    buffer.move_up(false);
    assert_eq!(caret_line(&buffer), 2, "and back to the header");
    assert_eq!(buffer.folded(), [(2, 5)], "without opening it");
}

#[test]
fn a_caret_moved_into_a_fold_opens_it() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    buffer.set_selections(Selections::single(Range::caret(buffer.line_start(4))));
    assert!(buffer.folded().is_empty(), "a caret out of sight would be typing blind");
}

#[test]
fn stepping_right_off_the_end_of_a_header_opens_its_fold() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    buffer.set_selections(Selections::single(Range::caret(buffer.line_end(2))));
    assert_eq!(buffer.folded(), [(2, 5)], "the end of the header is in view");
    buffer.move_right(false);
    assert!(buffer.folded().is_empty(), "the next line is not");
}

#[test]
fn folding_around_the_caret_moves_it_to_the_header() {
    let mut buffer = ten();
    buffer.set_selections(Selections::single(Range::caret(buffer.line_start(4) + 2)));
    buffer.fold(2, 5);
    assert_eq!(buffer.folded(), [(2, 5)], "folding is not undone by the caret it covers");
    assert_eq!(buffer.selections().primary().head, buffer.line_end(2));
}

#[test]
fn a_fold_survives_edits_outside_it() {
    let mut buffer = ten();
    buffer.fold(4, 6);
    // Above: a whole new line.
    buffer.set_selections(Selections::single(Range::caret(0)));
    buffer.insert("new\n");
    assert_eq!(buffer.folded(), [(5, 7)], "moved down with the text");
    // Below.
    let end = buffer.len_chars();
    buffer.set_selections(Selections::single(Range::caret(end)));
    buffer.insert("tail\n");
    assert_eq!(buffer.folded(), [(5, 7)]);
    // On the header line itself, which is in view.
    let header = buffer.line_end(5);
    buffer.set_selections(Selections::single(Range::caret(header)));
    buffer.insert(" {");
    assert_eq!(buffer.folded(), [(5, 7)]);
}

#[test]
fn an_edit_reaching_into_a_fold_opens_it() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    // A selection across the whole fold, deleted.
    let (from, to) = (buffer.line_start(2), buffer.line_start(7));
    buffer.set_selections(Selections::single(Range::new(from, to)));
    buffer.delete_backward();
    assert!(buffer.folded().is_empty());
}

#[test]
fn undo_moves_a_fold_along_with_the_text() {
    let mut buffer = ten();
    buffer.set_selections(Selections::single(Range::caret(0)));
    buffer.insert("a\nb\n");
    buffer.fold(5, 7);
    assert!(buffer.undo());
    assert_eq!(buffer.folded(), [(3, 5)], "the two lines above it went, and so did it");
    assert!(buffer.redo());
    assert_eq!(buffer.folded(), [(5, 7)]);
}

#[test]
fn a_fold_inside_a_fold_stays_folded_when_the_outer_one_opens() {
    let mut buffer = ten();
    buffer.fold(3, 4);
    buffer.fold(1, 6);
    assert_eq!(buffer.folded(), [(1, 6), (3, 4)]);
    assert!(buffer.unfold(1));
    assert_eq!(buffer.folded(), [(3, 4)]);
    assert!(buffer.is_folded(3));
}

#[test]
fn a_fold_at_the_end_leaves_its_header_as_the_last_line() {
    let mut buffer = Buffer::from_text("a\nb\nc");
    buffer.fold(0, 2);
    assert_eq!(buffer.hidden().last_in_view(), 0);
    buffer.set_selections(Selections::single(Range::caret(0)));
    buffer.move_down(false);
    assert_eq!(caret_line(&buffer), 0, "nowhere below to go");
}

#[test]
fn a_caret_is_not_added_out_of_sight() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    buffer.set_selections(Selections::single(Range::caret(buffer.line_start(2))));
    buffer.add_caret_vertically(false);
    let lines: Vec<usize> =
        buffer.selections().ranges().iter().map(|r| buffer.line_of(r.head)).collect();
    assert_eq!(lines, [2, 6], "below the fold, not inside it");
    assert_eq!(buffer.folded(), [(2, 5)]);
}

#[test]
fn folds_hold_across_wide_and_combined_text() {
    // Char offsets, not bytes: every line here is multi-byte.
    let mut buffer = Buffer::from_text("日本\ne\u{301}\n👨\u{200d}👩\u{200d}👧\nend\n");
    buffer.fold(0, 2);
    assert!(buffer.hidden().is_hidden(1) && buffer.hidden().is_hidden(2));
    buffer.set_selections(Selections::single(Range::caret(1)));
    buffer.insert("語");
    assert_eq!(buffer.folded(), [(0, 2)], "typing on the header keeps it");
}

#[test]
fn enter_at_the_end_of_a_folded_header_opens_the_fold_rather_than_moving_it() {
    // Otherwise the fold follows the new line break onto a blank line, and
    // the real header is left bare above it.
    let mut buffer = ten();
    buffer.fold(2, 5);
    buffer.set_selections(Selections::single(Range::caret(buffer.line_end(2))));
    buffer.insert("\n");
    assert!(buffer.folded().is_empty());
}

#[test]
fn select_all_leaves_a_fold_at_the_very_end_folded() {
    let mut buffer = Buffer::from_text("fn a() {\n  x\n}");
    buffer.fold(0, 2);
    buffer.select_all();
    assert_eq!(buffer.folded(), [(0, 2)]);
}

#[test]
fn selecting_a_folded_line_takes_the_fold_with_it_and_keeps_it_shut() {
    let mut buffer = ten();
    buffer.fold(2, 5);
    let (from, to) = buffer.line_range_in_view(2);
    assert_eq!((from, to), (buffer.line_start(2), buffer.line_start(6)));
    buffer.set_selections(Selections::single(Range::new(from, to)));
    assert_eq!(buffer.folded(), [(2, 5)], "the selection ends on a line in view");
}

#[test]
fn selecting_a_folded_line_that_runs_to_the_end_of_the_file_keeps_it_shut() {
    // No newline at the end, so no line after the fold for the selection to
    // end on: it ends where the fold does.
    let mut buffer = Buffer::from_text("fn main() {\n    x();\n}");
    buffer.fold(0, 2);
    let (from, to) = buffer.line_range_in_view(0);
    assert_eq!((from, to), (0, buffer.len_chars()));
    buffer.set_selections(Selections::single(Range::new(from, to)));
    assert_eq!(buffer.folded(), [(0, 2)]);
}

#[test]
fn a_selection_ending_where_a_fold_ends_mid_file_opens_it() {
    // What growing the selection over a folded block selects: its head is
    // on a hidden line, with a line in view after the fold.
    let mut buffer = Buffer::from_text("fn main() {\n    x();\n}\nfn b() {}\n");
    buffer.fold(0, 2);
    buffer.set_selections(Selections::single(Range::new(10, 22)));
    assert!(buffer.folded().is_empty());
}

#[test]
fn stepping_right_into_a_fold_of_one_empty_line_opens_it() {
    let mut buffer = Buffer::from_text("a {\n\nb\n");
    buffer.fold(0, 1);
    buffer.set_selections(Selections::single(Range::caret(3)));
    buffer.move_right(true);
    assert!(buffer.folded().is_empty());
}

#[test]
fn folding_thousands_of_regions_at_once_is_quick() {
    let mut text = String::new();
    for _ in 0..6000 {
        text.push_str("fn f() {\n    x();\n}\n");
    }
    let mut buffer = Buffer::from_text(&text);
    let regions: Vec<(usize, usize)> = (0..6000).map(|n| (n * 3, n * 3 + 2)).collect();
    let started = std::time::Instant::now();
    assert_eq!(buffer.fold_many(&regions), 6000);
    let took = started.elapsed();
    assert_eq!(buffer.hidden().rows_between(0, buffer.len_lines()), 6001);
    assert!(took < std::time::Duration::from_millis(500), "folding everything took {took:?}");
}

proptest! {
    #![proptest_config(config())]

    /// Whatever happens, no caret is ever left out of sight.
    #[test]
    fn no_caret_is_ever_hidden(
        ops in proptest::collection::vec((0u8..13, 0usize..12, 0usize..12), 1..40),
        wide in proptest::bool::ANY,
    ) {
        // Every line multi-byte, with a combining mark, a joined emoji and a
        // stray carriage return, when not plain ASCII.
        let mut buffer = if wide {
            Buffer::from_text(&"日本 e\u{301} 👨\u{200d}👩 \r\t語\n".repeat(10))
        } else {
            ten()
        };
        for (op, a, b) in ops {
            let lines = buffer.len_lines();
            match op {
                0 => { buffer.fold(a % lines, b % lines); }
                1 => { buffer.unfold(a % lines); }
                2 => buffer.move_down(b % 2 == 0),
                3 => buffer.move_up(b % 2 == 0),
                4 => buffer.move_right(false),
                5 => buffer.insert(if b % 3 == 0 { "\n" } else { "x" }),
                6 => buffer.delete_backward(),
                7 => { buffer.undo(); }
                8 => { buffer.redo(); }
                9 => buffer.delete_forward(),
                10 => buffer.add_caret_vertically(b % 2 == 0),
                11 => buffer.move_left(false),
                _ => {
                    let at = (a * 7 + b) % (buffer.len_chars() + 1);
                    buffer.set_selections(Selections::single(Range::caret(at)));
                }
            }
            let hidden = buffer.hidden();
            for range in buffer.selections().ranges() {
                let line = buffer.line_of(range.head);
                prop_assert!(!hidden.is_hidden(line), "a caret on hidden line {line}");
            }
            for (header, last) in buffer.folded() {
                prop_assert!(header < last, "a fold hiding nothing: {header}..{last}");
            }
        }
    }
}
