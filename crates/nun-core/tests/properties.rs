//! Invariants that examples cannot pin down.

use nun_core::{Buffer, Edit, Range, Selections};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};

/// Integration tests have no `lib.rs` for proptest to sit its regression file
/// beside, and it warns on every run about it. Failures are reproduced from the
/// printed seed instead.
fn config() -> Config {
    Config { failure_persistence: Some(Box::new(FileFailurePersistence::Off)), ..Config::default() }
}

/// Text with the awkward cases in it: wide chars, combining marks, ZWJ
/// sequences, tabs and newlines, not just ASCII.
fn interesting_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("a".to_string()),
            Just("z".to_string()),
            Just(" ".to_string()),
            Just("\t".to_string()),
            Just("\n".to_string()),
            Just("日".to_string()),
            Just("e\u{0301}".to_string()),
            Just("👨‍👩‍👧".to_string()),
        ],
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(config())]

    /// Applying an edit and then its inverse is the identity on the text.
    #[test]
    fn apply_then_invert_restores_the_rope(
        text in interesting_text(),
        start in 0usize..40,
        span in 0usize..8,
        insert in interesting_text(),
    ) {
        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        let start = start.min(len);
        let end = (start + span).min(len);

        let original = buffer.text().to_string();
        let edit = Edit::replace(start, end, insert);
        let changes_something = !edit.is_noop();

        buffer.edit(vec![edit]);

        // A no-op records no revision, so there is correctly nothing to undo.
        if changes_something {
            prop_assert!(buffer.undo(), "a real edit must be undoable");
        }
        prop_assert_eq!(buffer.text().to_string(), original);
    }

    /// However many times undo is asked, the text returns to where it started
    /// and never panics on the way.
    #[test]
    fn undo_all_the_way_back_returns_the_original(
        text in interesting_text(),
        inserts in proptest::collection::vec((0usize..40, interesting_text()), 0..8),
    ) {
        let mut buffer = Buffer::from_text(&text);
        let original = buffer.text().to_string();

        for (at, what) in inserts {
            let at = at.min(buffer.len_chars());
            buffer.set_selections(Selections::single(Range::caret(at)));
            buffer.insert(&what);
        }
        while buffer.undo() {}

        prop_assert_eq!(buffer.text().to_string(), original);
    }

    /// Typing, replacing and deleting at several selections at once undoes
    /// and redoes exactly. Each edit in such a revision shifts the ones after
    /// it, which a single caret never exercises.
    #[test]
    fn edits_at_several_selections_undo_and_redo_exactly(
        text in interesting_text(),
        ranges in proptest::collection::vec((0usize..40, 0usize..6), 1..6),
        what in interesting_text(),
        delete in proptest::bool::ANY,
    ) {
        let mut buffer = Buffer::from_text(&text);
        let original = buffer.text().to_string();
        let len = buffer.len_chars();
        let ranges: Vec<Range> = ranges
            .iter()
            .map(|(at, span)| Range::new((*at).min(len), (at + span).min(len)))
            .collect();
        buffer.set_selections(Selections::new(ranges, 0));

        if delete { buffer.delete_backward() } else { buffer.insert(&what) }
        let edited = buffer.text().to_string();

        buffer.undo();
        prop_assert_eq!(buffer.text().to_string(), original);
        buffer.redo();
        prop_assert_eq!(buffer.text().to_string(), edited);
    }

    /// Selections stay sorted, disjoint and non-empty no matter what happens.
    #[test]
    fn selections_stay_sorted_and_disjoint(
        text in interesting_text(),
        carets in proptest::collection::vec(0usize..40, 1..6),
        edits in proptest::collection::vec((0usize..40, 0usize..4), 0..6),
    ) {
        let mut buffer = Buffer::from_text(&text);
        let len = buffer.len_chars();
        let ranges: Vec<Range> = carets.iter().map(|c| Range::caret((*c).min(len))).collect();
        buffer.set_selections(Selections::new(ranges, 0));

        for (at, span) in edits {
            let at = at.min(buffer.len_chars());
            let end = (at + span).min(buffer.len_chars());
            buffer.edit(vec![Edit::replace(at, end, "x")]);

            let ranges = buffer.selections().ranges();
            prop_assert!(!ranges.is_empty(), "a buffer always has a caret");
            for pair in ranges.windows(2) {
                prop_assert!(
                    pair[0].to() < pair[1].from(),
                    "selections must stay sorted and disjoint: {:?}", ranges
                );
            }
            for range in ranges {
                prop_assert!(range.to() <= buffer.len_chars(), "a selection ran past the end");
            }
        }
    }

    /// Every position a caret can reach is a grapheme boundary, so movement can
    /// never leave it inside a cluster.
    #[test]
    fn walking_right_only_ever_lands_on_boundaries(text in interesting_text()) {
        use unicode_segmentation::UnicodeSegmentation;

        let mut buffer = Buffer::from_text(&text);
        buffer.set_selections(Selections::single(Range::caret(0)));

        let rope_text = buffer.text().to_string();
        let mut boundaries = vec![0usize];
        let mut at = 0usize;
        for cluster in rope_text.graphemes(true) {
            at += cluster.chars().count();
            boundaries.push(at);
        }

        let mut guard = 0;
        loop {
            let head = buffer.selections().primary().head;
            prop_assert!(
                boundaries.contains(&head),
                "caret at {head} is inside a cluster of {rope_text:?}"
            );
            if head >= buffer.len_chars() { break; }
            buffer.move_right(false);
            guard += 1;
            prop_assert!(guard < 500, "movement failed to terminate");
        }
    }
}
