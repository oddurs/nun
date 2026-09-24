//! Rendering, checked without a terminal.

use std::time::Instant;

use nun_core::{Buffer, Range, Selections};
use nun_theme::{Probe, Role, derive};
use nun_ui::{Change, EditorView, Harness, Palette, Stop, changed_cells, changed_rows};

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
        "1   fn main() {\n\
         2       println!(\"hi\");\n\
         3   }\n\
         4\n"
    );
}

#[test]
fn the_gutter_widens_with_the_line_count() {
    let short = Buffer::from_text("a\n");
    let long = Buffer::from_text(&"x\n".repeat(150));
    let palette = palette();

    assert_eq!(EditorView::new(&short, &palette).gutter_width(), 4, "one digit plus padding");
    assert_eq!(EditorView::new(&long, &palette).gutter_width(), 6, "three digits plus padding");
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
    // Gutter is "1" plus three columns of padding.
    assert_eq!(cells[(4, 0)].symbol(), "日");
    assert_eq!(cells[(5, 0)].symbol(), " ", "the trailing half must not carry a stale glyph");
    assert_eq!(cells[(6, 0)].symbol(), "本");
}

#[test]
fn a_tab_advances_to_the_next_stop() {
    let mut buffer = Buffer::from_text("\tx\n");
    buffer.set_tab_width(4);
    let mut harness = Harness::new(20, 2);
    draw(&mut harness, &buffer, &palette());

    let cells = harness.cells();
    assert_eq!(cells[(4 + 4, 0)].symbol(), "x", "the tab occupied four columns");
}

#[test]
fn the_caret_is_drawn_where_the_selection_head_is() {
    let mut buffer = Buffer::from_text("abc\n");
    buffer.set_selections(Selections::single(Range::caret(1)));
    let mut harness = Harness::new(20, 2);
    let palette = palette();
    draw(&mut harness, &buffer, &palette);

    let accent = palette.ramp().get(Role::Accent);
    let cell = &harness.cells()[(4 + 1, 0)];
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
        harness.cells()[(4 + 2, 0)].bg,
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
    let cell = &harness.cells()[(4 + 2, 0)];
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
    assert_eq!(harness.to_text(), "3   three\n4   four");
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
    assert_eq!(small.to_text(), "1   one\n2   two");

    let mut large = Harness::new(30, 4);
    draw(&mut large, &buffer, &palette);
    assert_eq!(large.to_text(), "1   one\n2   two\n3   three\n4");
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

// ── carets ──────────────────────────────────────────────────────────────────

fn caret_columns(harness: &Harness, palette: &Palette, row: u16) -> Vec<u16> {
    let caret = palette.on(Role::Accent, Role::OnAccent);
    let cells = harness.cells();
    (0..harness.area().width).filter(|&x| cells[(x, row)].bg == caret.bg.unwrap()).collect()
}

#[test]
fn every_caret_is_drawn_and_the_one_being_driven_stands_out() {
    // With several carets, the one the arrows move has to be findable, or
    // every key press is a guess about where the text will appear. The others
    // are still drawn, and still solid.
    let mut buffer = Buffer::from_text("abc\nabc");
    buffer.set_selections(Selections::new(vec![Range::caret(1), Range::caret(5)], 0));
    let palette = palette();
    let mut harness = Harness::new(20, 3);
    draw(&mut harness, &buffer, &palette);

    assert_eq!(caret_columns(&harness, &palette, 0), vec![4 + 1], "the primary, in the accent");
    assert!(caret_columns(&harness, &palette, 1).is_empty(), "the other is not in the accent");

    let quiet = palette.on(Role::LineStrong, Role::Ground);
    let cells = harness.cells();
    let others: Vec<u16> =
        (0..harness.area().width).filter(|&x| cells[(x, 1)].bg == quiet.bg.unwrap()).collect();
    assert_eq!(others, vec![4 + 1], "but it is drawn");
}

#[test]
fn the_drop_marker_shows_where_dragged_text_would_land() {
    let buffer = Buffer::from_text("abcdef");
    let palette = palette();
    let mut harness = Harness::new(20, 2);
    harness.draw(EditorView::new(&buffer, &palette).with_drop_marker(Some(4)));
    assert_eq!(caret_columns(&harness, &palette, 0), vec![4, 4 + 4], "the caret, then the marker");
}

// ── folding ─────────────────────────────────────────────────────────────────

#[test]
fn a_folded_region_is_drawn_as_its_header_with_a_marker() {
    let mut buffer = Buffer::from_text("fn a() {\n    one();\n    two();\n}\nfn b() {}\n");
    let foldable = [nun_syntax::FoldRange { header: 0, last: 3 }];
    let palette = palette();
    let mut harness = Harness::new(24, 4);

    harness.draw(EditorView::new(&buffer, &palette).foldable(&foldable));
    assert_eq!(
        harness.to_text(),
        "1 ▾ fn a() {\n\
         2       one();\n\
         3       two();\n\
         4   }",
        "an arrow on the line that opens a region, and nowhere else"
    );

    buffer.set_selections(Selections::single(Range::caret(buffer.len_chars())));
    buffer.fold(0, 3);
    harness.draw(EditorView::new(&buffer, &palette).foldable(&foldable));
    assert_eq!(
        harness.to_text(),
        "1 ▸ fn a() {  ⋯\n\
         5   fn b() {}\n\
         6\n",
        "the hidden lines take no rows, and the numbers say what is missing"
    );
    let arrow = harness.cells()[(2, 0)].fg;
    assert_eq!(arrow, palette.fg(Role::Accent).fg.unwrap(), "a folded arrow is in the accent");
}

#[test]
fn the_code_action_mark_sits_between_the_arrows_and_the_text() {
    let buffer = Buffer::from_text("fn a() {\n    one();\n}\n");
    let foldable = [nun_syntax::FoldRange { header: 0, last: 2 }];
    let palette = palette();
    let mut harness = Harness::new(24, 3);
    let view = EditorView::new(&buffer, &palette).foldable(&foldable).with_lightbulb(Some(0));
    assert_eq!(view.lightbulb_column(), 3);
    harness.draw(view);
    assert_eq!(
        harness.to_text(),
        "1 ▾◊fn a() {\n\
         2       one();\n\
         3   }",
        "beside the arrow, on the one line, and the text not moved"
    );
    let mark = harness.cells()[(3, 0)].fg;
    assert_eq!(mark, palette.fg(Role::Accent).fg.unwrap(), "a role, not a colour of its own");
}

#[test]
fn a_changed_line_has_a_bar_after_its_number_in_the_colour_of_the_change() {
    let buffer = Buffer::from_text("fn a() {\n    one();\n}\nlast\n");
    let foldable = [nun_syntax::FoldRange { header: 0, last: 2 }];
    let changes = [(0, Change::Modified), (1, Change::Added), (3, Change::RemovedAbove)];
    let palette = palette();
    let mut harness = Harness::new(24, 4);
    let view = EditorView::new(&buffer, &palette).foldable(&foldable).with_changes(&changes);
    assert_eq!(view.change_column(), 1);
    harness.draw(view);
    let bar = |change: Change| palette.glyph(change.glyph()).to_string();
    assert_eq!(
        harness.to_text(),
        format!(
            "1{}▾ fn a() {{\n2{}      one();\n3   }}\n4{}  last",
            bar(Change::Modified),
            bar(Change::Added),
            bar(Change::RemovedAbove)
        ),
        "beside the numbers, clear of the arrows, and the text not moved"
    );
    let cells = harness.cells();
    for (row, change) in [(0, Change::Modified), (1, Change::Added), (3, Change::RemovedAbove)] {
        assert_eq!(cells[(1, row)].fg, palette.fg(change.role()).fg.unwrap(), "row {row}");
    }
    assert_ne!(bar(Change::Added), bar(Change::Modified), "told apart without their colours");
    assert_ne!(Change::Added.role(), Change::Modified.role());
    assert_ne!(Change::Modified.role(), Change::RemovedAbove.role());
}

#[test]
fn the_code_action_mark_is_one_cell_wherever_it_is_drawn() {
    use unicode_width::UnicodeWidthStr;
    // Narrow both where ambiguous characters are narrow and where they are
    // wide, so no terminal setting can push the text a cell to the right.
    let lightbulb = palette().glyph(nun_ui::Glyph::Lightbulb).to_string();
    assert_eq!(lightbulb.width(), 1);
    assert_eq!(lightbulb.width_cjk(), 1);
    assert_eq!(lightbulb.chars().count(), 1, "no variation selector to go wrong");
}

#[test]
fn a_click_below_a_fold_maps_to_the_line_drawn_there() {
    let mut buffer = Buffer::from_text("fn a() {\n    one();\n}\nlast\n");
    buffer.set_selections(Selections::single(Range::caret(buffer.len_chars())));
    buffer.fold(0, 2);
    let palette = palette();
    let view = EditorView::new(&buffer, &palette);
    let area = ratatui::layout::Rect::new(0, 0, 20, 4);
    // Row 1 shows `last`, line 3.
    assert_eq!(view.position_at(area, 4, 1), Some(buffer.line_start(3)));
    assert_eq!(view.line_at_row(1), Some(3));
}

#[test]
fn a_position_is_drawn_in_the_cell_that_maps_back_to_it() {
    // Wide, combining, tab and plain, on a line below a fold.
    let mut buffer = Buffer::from_text("fn a() {\n    one();\n}\n日e\u{301}\tx\n");
    buffer.set_selections(Selections::single(Range::caret(0)));
    buffer.fold(0, 2);
    let palette = palette();
    let view = EditorView::new(&buffer, &palette);
    let area = ratatui::layout::Rect::new(0, 0, 20, 4);
    let start = buffer.line_start(3);
    let gutter = view.gutter_width();
    // `日` at 0, `é` (two chars) at 2, the tab at 3, `x` at 4 after the tab.
    for (offset, column) in [(0, 0), (1, 2), (3, 3), (4, 4), (5, 5)] {
        let cell = view.cell_of(area, start + offset);
        assert_eq!(cell, Some((gutter + column, 1)), "char {offset}");
        if offset < 5 {
            assert_eq!(view.position_at(area, gutter + column, 1), Some(start + offset));
        }
    }
    assert_eq!(view.cell_of(area, buffer.line_start(1)), None, "folded away");
    let narrow = ratatui::layout::Rect::new(0, 0, gutter + 2, 4);
    assert_eq!(view.cell_of(narrow, start + 4), None, "past the right edge");
}

// ── snippet tab-stops ───────────────────────────────────────────────────────

#[test]
fn tab_stops_are_washed_the_current_one_and_its_mirror_more_strongly() {
    // `let x = x; y|` with `x` the current stop, mirrored, and `y` the next.
    let mut buffer = Buffer::from_text("let x = x; y\nz");
    buffer.set_selections(Selections::single(Range::caret(buffer.len_chars())));
    let stops = [
        Stop { start: 4, end: 5, current: true },
        Stop { start: 8, end: 9, current: true },
        Stop { start: 11, end: 12, current: false },
        // Empty, at the end of the line: its one cell is still marked.
        Stop { start: 12, end: 12, current: false },
    ];
    let palette = palette();
    let mut harness = Harness::new(20, 2);
    harness.draw(EditorView::new(&buffer, &palette).with_stops(&stops));

    let gutter = EditorView::new(&buffer, &palette).gutter_width();
    let bg = |column: u16| harness.cells()[(gutter + column, 0)].bg;
    let (current, other) = (palette.tabstop(true).bg, palette.tabstop(false).bg);
    assert_eq!(Some(bg(4)), current, "the current stop");
    assert_eq!(Some(bg(8)), current, "its mirror");
    assert_eq!(Some(bg(11)), other, "the next stop, more quietly");
    assert_eq!(Some(bg(12)), other, "an empty stop past the end of the line");
    assert_eq!(bg(3), palette.ground(), "nothing between them");
    assert_eq!(bg(13), palette.ground(), "and nothing after");
}

#[test]
fn a_selection_and_a_caret_read_over_a_tab_stop() {
    let mut buffer = Buffer::from_text("ab cd");
    buffer.set_selections(Selections::new(vec![Range::new(0, 2), Range::caret(3)], 0));
    let stops =
        [Stop { start: 0, end: 2, current: true }, Stop { start: 3, end: 5, current: false }];
    let palette = palette();
    let mut harness = Harness::new(20, 1);
    harness.draw(EditorView::new(&buffer, &palette).with_stops(&stops));

    let gutter = EditorView::new(&buffer, &palette).gutter_width();
    let cells = harness.cells();
    assert_eq!(Some(cells[(gutter, 0)].bg), palette.selection().bg, "the placeholder, selected");
    assert_ne!(Some(cells[(gutter + 3, 0)].bg), palette.tabstop(false).bg, "the caret wins");
    assert_eq!(Some(cells[(gutter + 4, 0)].bg), palette.tabstop(false).bg);
}

#[test]
fn a_tab_stop_below_a_fold_is_drawn_on_the_row_its_line_is_on() {
    let mut buffer = Buffer::from_text("fn a() {\n    one();\n}\nlet x\n");
    buffer.set_selections(Selections::single(Range::caret(buffer.len_chars())));
    buffer.fold(0, 2);
    let start = buffer.line_start(3) + 4;
    // One stop hidden in the fold, one below it.
    let stops = [
        Stop { start: 13, end: 16, current: false },
        Stop { start, end: start + 1, current: true },
    ];
    let palette = palette();
    let mut harness = Harness::new(20, 3);
    harness.draw(EditorView::new(&buffer, &palette).with_stops(&stops));

    let gutter = EditorView::new(&buffer, &palette).gutter_width();
    let quiet = palette.tabstop(false).bg.expect("a wash");
    assert_eq!(Some(harness.cells()[(gutter + 4, 1)].bg), palette.tabstop(true).bg);
    for y in 0..3 {
        for x in 0..20 {
            assert_ne!(harness.cells()[(x, y)].bg, quiet, "the folded stop drew at ({x}, {y})");
        }
    }
}
