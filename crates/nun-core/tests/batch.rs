//! Edits from outside the editor, applied as one step: a formatter's answer,
//! a rename's.

use nun_core::{BatchError, Buffer, Edit, Range, Selections};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};

/// As in `properties.rs`: no `lib.rs` beside an integration test for a
/// regression file, so failures are reproduced from the printed seed.
fn config() -> Config {
    Config { failure_persistence: Some(Box::new(FileFailurePersistence::Off)), ..Config::default() }
}

fn whole(buffer: &Buffer, text: &str) -> Edit {
    Edit::replace(0, buffer.len_chars(), text)
}

#[test]
fn a_reformat_is_one_step_to_undo_and_redo() {
    let mut buffer = Buffer::from_text("fn f(){\nx}\n");
    buffer.set_selections(Selections::single(Range::caret(8)));
    assert_eq!(buffer.apply_batch(vec![whole(&buffer, "fn f() {\n    x\n}\n")]), Ok(true));
    assert_eq!(buffer.text().to_string(), "fn f() {\n    x\n}\n");
    assert_eq!(buffer.selections().primary(), Range::caret(13), "still in front of x");

    assert!(buffer.undo());
    assert_eq!(buffer.text().to_string(), "fn f(){\nx}\n");
    assert_eq!(buffer.selections().primary(), Range::caret(8));
    assert!(!buffer.undo(), "one step, however many pieces it was applied in");

    assert!(buffer.redo());
    assert_eq!(buffer.text().to_string(), "fn f() {\n    x\n}\n");
    assert_eq!(buffer.selections().primary(), Range::caret(13));
}

#[test]
fn typing_either_side_of_a_reformat_is_undone_separately() {
    let mut buffer = Buffer::from_text("a=1");
    buffer.set_selections(Selections::single(Range::caret(3)));
    buffer.insert("2");
    buffer.apply_batch(vec![whole(&buffer, "a = 12")]).unwrap();
    buffer.insert("3");
    assert_eq!(buffer.text().to_string(), "a = 123");

    buffer.undo();
    assert_eq!(buffer.text().to_string(), "a = 12", "the typing after it");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "a=12", "the reformat");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "a=1", "the typing before it");
}

#[test]
fn a_batch_that_changes_nothing_leaves_the_buffer_clean() {
    let mut buffer = Buffer::from_text("already\n");
    assert_eq!(buffer.apply_batch(vec![whole(&buffer, "already\n")]), Ok(false));
    assert_eq!(buffer.apply_batch(Vec::new()), Ok(false));
    assert!(!buffer.is_modified());
    assert!(!buffer.undo());
}

#[test]
fn a_refused_batch_changes_nothing() {
    let mut buffer = Buffer::from_text("abcdef");
    let refused = buffer.apply_batch(vec![
        Edit::insert(0, "x"),
        Edit::replace(1, 4, "y"),
        Edit::delete(3, 5),
    ]);
    assert_eq!(refused, Err(BatchError::Overlap { first: (1, 4), second: (3, 5) }));
    assert_eq!(buffer.text().to_string(), "abcdef");
    assert!(!buffer.is_modified());
}

#[test]
fn every_selection_is_carried_by_what_is_around_it() {
    // Three carets and a selection, each against a token that the reformat
    // moves by a different amount.
    let text = "let  a=[1,2];\n\tfoo( a );\n";
    let mut buffer = Buffer::from_text(text);
    let at = |needle: &str| text.find(needle).unwrap();
    buffer.set_selections(Selections::new(
        vec![
            Range::caret(at("a=")),
            Range::caret(at("2]")),
            Range::new(at("foo"), at("foo") + 3),
            Range::caret(at(" );")),
        ],
        2,
    ));
    let formatted = "let a = [1, 2];\n    foo(a);\n";
    buffer.apply_batch(vec![whole(&buffer, formatted)]).unwrap();
    assert_eq!(buffer.text().to_string(), formatted);
    let spots: Vec<(usize, usize)> =
        buffer.selections().ranges().iter().map(|range| (range.anchor, range.head)).collect();
    let now = |needle: &str| formatted.find(needle).unwrap();
    assert_eq!(
        spots,
        vec![
            (now("a ="), now("a =")),
            // Nothing came between `1,` and `2` until the reformat put a
            // space there: the caret stays with what it was typed after.
            (now(" 2]"), now(" 2]")),
            (now("foo"), now("foo") + 3),
            // It was after `a`, before the space the reformat took out.
            (now(");"), now(");")),
        ]
    );
    assert_eq!(buffer.selections().primary(), Range::new(now("foo"), now("foo") + 3));
}

#[test]
fn the_edits_of_a_crlf_file_come_back_with_line_feeds() {
    let (mut buffer, _) = Buffer::from_bytes(b"a\r\nb\r\n");
    buffer.apply_batch(vec![Edit::insert(2, "x\r\n")]).unwrap();
    assert_eq!(buffer.text().to_string(), "a\nx\nb\n");
    assert_eq!(buffer.to_bytes(), b"a\r\nx\r\nb\r\n", "and go back out as the file has them");
}

#[test]
fn a_caret_at_the_end_stays_at_the_end() {
    let mut buffer = Buffer::from_text("x  \n\n\n");
    let end = buffer.len_chars();
    buffer.set_selections(Selections::single(Range::caret(end)));
    buffer.apply_batch(vec![whole(&buffer, "x\n")]).unwrap();
    assert_eq!(buffer.selections().primary(), Range::caret(2));
}

#[test]
fn a_caret_never_lands_inside_a_cluster_the_batch_rebuilt() {
    // The batch turns `e` into `é` spelled with a combining mark: the old
    // caret after the `e` would stay between the two. It goes past both.
    let mut buffer = Buffer::from_text("cafe;");
    buffer.set_selections(Selections::single(Range::caret(4)));
    buffer.apply_batch(vec![Edit::replace(0, 4, "cafe\u{301}")]).unwrap();
    assert_eq!(buffer.selections().primary(), Range::caret(5));

    // A joiner and a second emoji added to the one the caret was after.
    let mut buffer = Buffer::from_text("👨 x");
    buffer.set_selections(Selections::single(Range::caret(1)));
    buffer.apply_batch(vec![Edit::replace(0, 1, "👨\u{200d}👩")]).unwrap();
    assert_eq!(buffer.selections().primary(), Range::caret(3));
}

/// Text with the awkward cases in it, spaced so a reformat has something to
/// change.
fn words() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        prop_oneof![
            Just("a".to_string()),
            Just("zz".to_string()),
            Just("日".to_string()),
            Just("e\u{0301}".to_string()),
            Just("👨‍👩‍👧".to_string()),
            Just("(".to_string()),
            Just(";".to_string()),
        ],
        1..12,
    )
}

fn spaced(words: &[String], gaps: &[String]) -> String {
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        out.push_str(word);
        out.push_str(&gaps[i % gaps.len()]);
    }
    out
}

proptest! {
    #![proptest_config(config())]

    /// Whatever the batch, undo puts back the text and the selections, redo
    /// the batch's result, and the selections stay sorted, disjoint and in
    /// the text.
    #[test]
    fn a_batch_undoes_and_redoes_exactly(
        words in words(),
        gaps in proptest::collection::vec("[ \\n\\t]{1,3}", 1..4),
        fresh in proptest::collection::vec("[ \\n]{0,2}", 1..4),
        carets in proptest::collection::vec(0usize..80, 1..5),
        keep_last in any::<bool>(),
    ) {
        let text = spaced(&words, &gaps);
        // Sometimes a word short, so the batch changes more than whitespace.
        let shorter = if keep_last { &words[..] } else { &words[..words.len() - 1] };
        let formatted = spaced(shorter, &fresh);

        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        let ranges = carets.iter().map(|&at| Range::caret(at.min(len))).collect();
        buffer.set_selections(Selections::new(ranges, 0));
        let before = buffer.selections().clone();

        let changed = buffer.apply_batch(vec![Edit::replace(0, len, formatted.clone())]).unwrap();
        prop_assert_eq!(changed, text != formatted);
        prop_assert_eq!(buffer.text().to_string(), formatted.clone());
        let after = buffer.selections().clone();
        let total = buffer.len_chars();
        prop_assert!(after.ranges().iter().all(|range| range.to() <= total));
        prop_assert!(after.ranges().windows(2).all(|pair| pair[0].to() < pair[1].from()));

        if changed {
            prop_assert!(buffer.undo());
            prop_assert_eq!(buffer.text().to_string(), text);
            prop_assert_eq!(buffer.selections(), &before);
            prop_assert!(buffer.redo());
            prop_assert_eq!(buffer.text().to_string(), formatted);
            prop_assert_eq!(buffer.selections(), &after);
        }
    }

    /// The same for a batch of several edits, inserts at one position among
    /// them, changing more than whitespace, given in any order.
    #[test]
    fn a_batch_of_many_edits_undoes_and_redoes_exactly(
        text in "(a| |\n|日|e\u{301}|👨‍👩‍👧){0,20}",
        raw in proptest::collection::vec((0usize..6, 0usize..4, "(b| |\n|\r\n|中){0,3}"), 0..6),
        carets in proptest::collection::vec(0usize..40, 1..4),
        reverse in any::<bool>(),
    ) {
        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        let mut edits = Vec::new();
        let mut floor = 0;
        for (gap, width, insert) in raw {
            let start = (floor + gap).min(len);
            let end = (start + width).min(len);
            edits.push(Edit::replace(start, end, insert));
            floor = end;
        }
        // Left to right, in the order given, which is the order they read in.
        let chars: Vec<char> = text.chars().collect();
        let mut expected = String::new();
        let mut done = 0;
        for edit in &edits {
            expected.extend(&chars[done..edit.start]);
            expected.push_str(&edit.text.replace("\r\n", "\n"));
            done = edit.end;
        }
        expected.extend(&chars[done..]);
        if reverse {
            // Last first, as a server may send them. A stable sort keeps
            // the inserts at one position in the order that means.
            edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
        }
        let ranges = carets.iter().map(|&at| Range::caret(at.min(len))).collect();
        buffer.set_selections(Selections::new(ranges, 0));
        let before = buffer.selections().clone();

        let changed = buffer.apply_batch(edits).unwrap();
        prop_assert_eq!(buffer.text().to_string(), expected.clone());
        let after = buffer.selections().clone();
        let total = buffer.len_chars();
        prop_assert!(after.ranges().iter().all(|range| range.to() <= total));
        if changed {
            prop_assert!(buffer.undo());
            prop_assert_eq!(buffer.text().to_string(), text);
            prop_assert_eq!(buffer.selections(), &before);
            prop_assert!(buffer.redo());
            prop_assert_eq!(buffer.text().to_string(), expected);
            prop_assert_eq!(buffer.selections(), &after);
        } else {
            prop_assert_eq!(text, expected);
        }
    }
}
