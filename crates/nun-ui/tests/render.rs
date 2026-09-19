//! Rendering, checked without a terminal.

use std::time::Instant;

use nun_core::{Buffer, Range, Selections};
use nun_theme::{Probe, Role, derive};
use nun_ui::{EditorView, Harness, Palette, changed_cells, changed_rows};

fn palette() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn draw(harness: &mut Harness, buffer: &Buffer, palette: &Palette) {
    harness.draw(EditorView::new(buffer, palette));
}

#[test]
fn a_buffer_renders_with_line_numbers() {
    let buffer = Buffer::from_text("fn main() {\n    println!(\"hi\");\n}\n");
    let mut harness = Harness::new(40, 5);
    draw(&mut harness, &buffer, &palette());

    assert_eq!(
        harness.to_text(),
        "1  fn main() {\n\
         2      println!(\"hi\");\n\
         3  }\n\
         4\n"
    );
}

#[test]
fn the_gutter_widens_with_the_line_count() {
    let short = Buffer::from_text("a\n");
    let long = Buffer::from_text(&"x\n".repeat(150));
    let palette = palette();

    assert_eq!(EditorView::new(&short, &palette).gutter_width(), 3, "one digit plus padding");
    assert_eq!(EditorView::new(&long, &palette).gutter_width(), 5, "three digits plus padding");
}

#[test]
fn every_cell_is_painted_so_nothing_shows_the_host_background_through() {
    let buffer = Buffer::from_text("short\n");
    let mut harness = Harness::new(30, 4);
    draw(&mut harness, &buffer, &palette());

    let cells = harness.cells();
    let area = harness.area();
    for y in 0..area.height {
        for x in 0..area.width {
            assert!(
                cells[(x, y)].bg != ratatui::style::Color::Reset,
                "cell ({x}, {y}) has no background of its own"
            );
        }
    }
}

#[test]
fn a_wide_character_occupies_two_cells() {
    let buffer = Buffer::from_text("日本\n");
    let mut harness = Harness::new(20, 2);
    draw(&mut harness, &buffer, &palette());

    let cells = harness.cells();
    // Gutter is "1" plus two columns of padding.
    assert_eq!(cells[(3, 0)].symbol(), "日");
    assert_eq!(cells[(4, 0)].symbol(), " ", "the trailing half must not carry a stale glyph");
    assert_eq!(cells[(5, 0)].symbol(), "本");
}

#[test]
fn a_tab_advances_to_the_next_stop() {
    let mut buffer = Buffer::from_text("\tx\n");
    buffer.set_tab_width(4);
    let mut harness = Harness::new(20, 2);
    draw(&mut harness, &buffer, &palette());

    let cells = harness.cells();
    assert_eq!(cells[(3 + 4, 0)].symbol(), "x", "the tab occupied four columns");
}

#[test]
fn the_caret_is_drawn_where_the_selection_head_is() {
    let mut buffer = Buffer::from_text("abc\n");
    buffer.set_selections(Selections::single(Range::caret(1)));
    let mut harness = Harness::new(20, 2);
    let palette = palette();
    draw(&mut harness, &buffer, &palette);

    let accent = palette.ramp().get(Role::Accent);
    let cell = &harness.cells()[(3 + 1, 0)];
    assert_eq!(cell.bg, ratatui::style::Color::Rgb(accent.r, accent.g, accent.b));
}

#[test]
fn the_caret_can_sit_one_past_the_end_of_a_line() {
    let mut buffer = Buffer::from_text("ab\n");
    buffer.set_selections(Selections::single(Range::caret(2)));
    let mut harness = Harness::new(20, 2);
    let palette = palette();
    draw(&mut harness, &buffer, &palette);

    let accent = palette.ramp().get(Role::Accent);
    assert_eq!(
        harness.cells()[(3 + 2, 0)].bg,
        ratatui::style::Color::Rgb(accent.r, accent.g, accent.b)
    );
}

#[test]
fn a_selection_is_washed_without_losing_its_foreground() {
    let mut buffer = Buffer::from_text("abcdef\n");
    buffer.set_selections(Selections::single(Range::new(1, 4)));
    let mut harness = Harness::new(20, 2);
    let palette = palette();
    draw(&mut harness, &buffer, &palette);

    let selection = palette.ramp().get(Role::Selection);
    let text = palette.ramp().get(Role::Text);
    let cell = &harness.cells()[(3 + 2, 0)];
    assert_eq!(cell.bg, ratatui::style::Color::Rgb(selection.r, selection.g, selection.b));
    assert_eq!(
        cell.fg,
        ratatui::style::Color::Rgb(text.r, text.g, text.b),
        "syntax colour survives"
    );
}

#[test]
fn scrolling_moves_the_window_not_the_numbering() {
    let buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    let palette = palette();
    let mut harness = Harness::new(20, 2);
    harness.draw(EditorView::new(&buffer, &palette).scrolled_to(2));
    assert_eq!(harness.to_text(), "3  three\n4  four");
}

#[test]
fn an_area_too_narrow_for_the_gutter_renders_nothing_rather_than_panicking() {
    let buffer = Buffer::from_text("hello\n");
    let mut harness = Harness::new(2, 2);
    draw(&mut harness, &buffer, &palette());
    assert_eq!(harness.to_text().trim(), "");
}

#[test]
fn a_zero_sized_area_is_survivable() {
    let buffer = Buffer::from_text("hello\n");
    let mut harness = Harness::new(0, 0);
    draw(&mut harness, &buffer, &palette());
}

#[test]
fn resizing_redraws_without_a_teardown() {
    let buffer = Buffer::from_text("one\ntwo\nthree\n");
    let palette = palette();

    let mut small = Harness::new(10, 2);
    draw(&mut small, &buffer, &palette);
    assert_eq!(small.to_text(), "1  one\n2  two");

    let mut large = Harness::new(30, 4);
    draw(&mut large, &buffer, &palette);
    assert_eq!(large.to_text(), "1  one\n2  two\n3  three\n4");
}

// ── damage ──────────────────────────────────────────────────────────────────

#[test]
fn a_single_character_insert_repaints_only_its_own_line() {
    let mut buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    buffer.set_selections(Selections::single(Range::caret(buffer.line_start(2))));

    let palette = palette();
    let mut harness = Harness::new(40, 6);
    draw(&mut harness, &buffer, &palette);
    let before = harness.snapshot();

    buffer.insert("X");
    draw(&mut harness, &buffer, &palette);

    assert_eq!(
        changed_rows(&before, harness.cells()),
        vec![2],
        "typing on line 3 must not repaint any other line"
    );
}

#[test]
fn a_single_character_insert_touches_only_the_cells_it_has_to() {
    let mut buffer = Buffer::from_text("abcdef\n");
    buffer.set_selections(Selections::single(Range::caret(6)));

    let palette = palette();
    let mut harness = Harness::new(40, 3);
    draw(&mut harness, &buffer, &palette);
    let before = harness.snapshot();

    buffer.insert("Z");
    draw(&mut harness, &buffer, &palette);

    // The new character, and the caret moving one place on. Everything else on
    // the line is identical and must not be rewritten.
    let changed = changed_cells(&before, harness.cells());
    assert!(changed.len() <= 2, "expected at most two cells to change, got {changed:?}");
}

#[test]
fn moving_the_caret_down_repaints_two_lines_and_no_more() {
    let mut buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    buffer.set_selections(Selections::single(Range::caret(0)));

    let palette = palette();
    let mut harness = Harness::new(40, 6);
    draw(&mut harness, &buffer, &palette);
    let before = harness.snapshot();

    buffer.move_down(false);
    draw(&mut harness, &buffer, &palette);

    assert_eq!(
        changed_rows(&before, harness.cells()),
        vec![0, 1],
        "only the line left and the line arrived at"
    );
}

#[test]
fn redrawing_an_unchanged_buffer_produces_no_damage_at_all() {
    let buffer = Buffer::from_text("one\ntwo\n");
    let palette = palette();
    let mut harness = Harness::new(40, 4);
    draw(&mut harness, &buffer, &palette);
    let before = harness.snapshot();

    draw(&mut harness, &buffer, &palette);

    assert!(
        changed_rows(&before, harness.cells()).is_empty(),
        "an idle repaint must write nothing"
    );
}

// ── budget ──────────────────────────────────────────────────────────────────

#[test]
fn a_full_frame_of_a_large_file_stays_well_inside_the_budget() {
    // Not the end-to-end latency budget — that is cairn 0044, measured from
    // input event to flushed frame. This is a floor: if laying out one screenful
    // is already slow, nothing downstream can rescue it.
    let buffer = Buffer::from_text(&"fn example() { let value = 42; }\n".repeat(10_000));
    let palette = palette();
    let mut harness = Harness::new(120, 50);

    draw(&mut harness, &buffer, &palette);

    let frames = 100;
    let started = Instant::now();
    for _ in 0..frames {
        draw(&mut harness, &buffer, &palette);
    }
    let per_frame = started.elapsed() / frames;

    assert!(
        per_frame.as_micros() < 2_000,
        "a 120x50 frame took {per_frame:?}, which leaves nothing for the rest of the budget"
    );
}

// ── cell to buffer position ─────────────────────────────────────────────────

/// Where `position_at` lands for each text column of line 0, gutter excluded.
fn positions(text: &str, columns: u16) -> Vec<Option<usize>> {
    let buffer = Buffer::from_text(text);
    let palette = palette();
    let view = EditorView::new(&buffer, &palette);
    let gutter = view.gutter_width();
    let area = ratatui::layout::Rect::new(0, 0, gutter + columns + 4, 3);
    (0..columns).map(|column| view.position_at(area, gutter + column, 0)).collect()
}

#[test]
fn the_gutter_is_not_a_buffer_position() {
    let buffer = Buffer::from_text("hello");
    let palette = palette();
    let area = ratatui::layout::Rect::new(0, 0, 20, 3);
    assert_eq!(EditorView::new(&buffer, &palette).position_at(area, 0, 0), None);
}

#[test]
fn every_cell_of_an_expanded_tab_lands_on_the_tab() {
    // Tab width 4: `a` in column 0, the tab fills 1 to 3, `b` is column 4.
    let at = positions("a\tb", 6);
    assert_eq!(at, vec![Some(0), Some(1), Some(1), Some(1), Some(2), Some(3)]);
}

#[test]
fn both_cells_of_a_wide_character_land_on_it() {
    let at = positions("日本x", 6);
    assert_eq!(at, vec![Some(0), Some(0), Some(1), Some(1), Some(2), Some(3)]);
}

#[test]
fn a_combining_mark_is_one_cell_with_its_base() {
    // `e` and U+0301 are one cluster of two chars in one column.
    let at = positions("e\u{301}x", 3);
    assert_eq!(at, vec![Some(0), Some(2), Some(3)]);
}

#[test]
fn a_joined_emoji_is_one_target_not_a_row_of_codepoints() {
    let family = "👩\u{200d}👩\u{200d}👧";
    let chars = family.chars().count();
    let at = positions(&format!("{family}x"), 3);
    assert_eq!(at, vec![Some(0), Some(0), Some(chars)]);
}

#[test]
fn a_click_past_the_end_of_a_line_lands_at_its_end_not_on_the_next() {
    let at = positions("ab\ncd", 5);
    assert_eq!(at[4], Some(2), "the end of line 0, before its newline");
}

#[test]
fn a_click_below_the_last_line_lands_on_the_last_line() {
    let buffer = Buffer::from_text("one\ntwo");
    let palette = palette();
    let area = ratatui::layout::Rect::new(0, 0, 20, 10);
    let view = EditorView::new(&buffer, &palette);
    assert_eq!(view.position_at(area, view.gutter_width(), 8), Some(4));
}

#[test]
fn scrolling_shifts_which_line_a_row_means() {
    let buffer = Buffer::from_text("zero\none\ntwo\n");
    let palette = palette();
    let area = ratatui::layout::Rect::new(0, 0, 20, 3);
    let view = EditorView::new(&buffer, &palette).scrolled_to(2);
    assert_eq!(view.position_at(area, view.gutter_width(), 0), Some(9));
}

#[test]
fn an_area_offset_from_the_origin_is_measured_from_its_own_corner() {
    // A second pane starts part-way across the screen.
    let buffer = Buffer::from_text("abc");
    let palette = palette();
    let area = ratatui::layout::Rect::new(30, 5, 20, 3);
    let view = EditorView::new(&buffer, &palette);
    let gutter = view.gutter_width();
    assert_eq!(view.position_at(area, 30 + gutter + 1, 5), Some(1));
    assert_eq!(view.position_at(area, 29, 5), None, "left of the area");
    assert_eq!(view.position_at(area, 30 + gutter, 4), None, "above it");
}

mod mapping {
    use super::*;
    use proptest::prelude::*;

    /// Clusters that each break an ASCII-only assumption somewhere.
    const PIECES: &[&str] =
        &["a", "\t", "日", "e\u{301}", "👩\u{200d}👩\u{200d}👧", "🇮🇸", "\u{7}", " ", "é", "ｗ"];

    proptest! {
        /// Clicking the first cell of every cluster lands exactly on that
        /// cluster, as measured by nun-core's own column arithmetic.
        #[test]
        fn clicking_where_a_cluster_is_drawn_lands_on_it(
            picks in prop::collection::vec(0..PIECES.len(), 0..16),
        ) {
            let text: String = picks.iter().map(|&i| PIECES[i]).collect();
            let buffer = Buffer::from_text(&text);
            let palette = palette();
            let view = EditorView::new(&buffer, &palette);
            let gutter = view.gutter_width();
            let area = ratatui::layout::Rect::new(0, 0, 200, 3);

            let mut position = 0;
            for &i in &picks {
                let column = u16::try_from(buffer.column_of(position)).unwrap();
                prop_assert_eq!(view.position_at(area, gutter + column, 0), Some(position));
                position += PIECES[i].chars().count();
            }
        }
    }
}
