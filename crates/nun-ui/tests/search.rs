//! The project-search panel, drawn without a terminal.
//!
//! The layout constants the panel draws with are private to it, so this file
//! spells the same numbers out as literals. That is the point of testing it
//! from outside: a test deriving a column from the constant the code used
//! would agree with any value of it, including a wrong one.

use std::ops::Range;

use nun_theme::{Probe, Role, derive};
use nun_ui::{Harness, Palette, SearchButton, SearchRow, SearchView, Toggles, changed_rows};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/// The first result row: the header, the query, the toggles and the summary
/// sit above it.
const FIRST: u16 = 4;

/// Columns the query row's prompt occupies, before the query itself.
const PROMPT_COLS: u16 = 2;

/// Where the text of a hit row starts: a column of margin, one level of
/// indent under its file, the narrowest line-number gutter, and a space.
const HIT_TEXT_X: u16 = 7;

/// The header button that hands the sidebar back to the file tree.
const BACK: &str = "▤";

/// What an empty query field says when nobody is typing into it.
const PLACEHOLDER: &str = "Search the project";

fn palette() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn rows<'a>() -> Vec<SearchRow<'a>> {
    vec![
        SearchRow::File { path: "src/main.rs", hits: 2, collapsed: false },
        SearchRow::Hit { line: 7, text: "fn main() {", matched: &[] },
        SearchRow::Hit { line: 91, text: "    render();", matched: &[] },
        SearchRow::File { path: "README.md", hits: 1, collapsed: true },
    ]
}

/// The column the query row's caret sits on.
fn caret_column(harness: &Harness) -> u16 {
    let bg = palette().on(Role::Accent, Role::OnAccent).bg.expect("the caret has a wash");
    (0..harness.area().width)
        .find(|x| harness.cells()[(*x, 1)].bg == bg)
        .expect("the caret is drawn")
}

#[test]
fn the_query_row_starts_with_a_prompt_two_columns_wide() {
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("abc", &[], &palette));
    let cells = harness.cells();
    assert_eq!(cells[(0, 1)].symbol(), "⌕");
    assert_eq!(cells[(1, 1)].symbol(), " ");
    assert_eq!(cells[(PROMPT_COLS, 1)].symbol(), "a", "the query starts after the prompt");
}

#[test]
fn the_panel_draws_its_chrome_then_its_rows() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(
        SearchView::new("main", &rows, &palette).summary(Some("3 hits in 2 files")).focused(true),
    );

    assert_eq!(
        harness.to_text(),
        " SEARCH               ▤\n\
         ⌕ main\n\
         \u{20}* A ▭ ○\n\
         \u{20}3 hits in 2 files\n\
         \u{20}▾ src/main.rs        2\n\
         \u{20}    7 fn main() {\n\
         \u{20}   91     render();\n\
         \u{20}▸ README.md          1"
    );
}

#[test]
fn every_band_agrees_with_the_row_it_draws() {
    let area = Rect::new(0, 0, 24, 9);
    assert_eq!(SearchView::header_area(area), Rect::new(0, 0, 24, 1));
    assert_eq!(SearchView::query_area(area), Rect::new(0, 1, 24, 1));
    assert_eq!(SearchView::toggles_area(area), Rect::new(0, 2, 24, 1));
    assert_eq!(SearchView::summary_area(area), Rect::new(0, 3, 24, 1));
    assert_eq!(SearchView::rows_area(area), Rect::new(0, 4, 24, 5));
    assert_eq!(SearchView::visible_rows(area), 5);
}

#[test]
fn a_panel_too_short_for_a_band_gives_it_no_rows() {
    for height in 0..=3u16 {
        let area = Rect::new(0, 0, 24, height);
        let bands = [
            SearchView::header_area(area),
            SearchView::query_area(area),
            SearchView::toggles_area(area),
            SearchView::summary_area(area),
        ];
        for (index, band) in bands.iter().enumerate() {
            let expected = u16::from(u16::try_from(index).unwrap_or(0) < height);
            assert_eq!(band.height, expected, "band {index} at height {height}");
        }
        assert_eq!(SearchView::rows_area(area).height, 0, "height {height}");
        assert_eq!(SearchView::visible_rows(area), 0, "height {height}");
    }
}

#[test]
fn a_squeezed_panel_draws_only_the_bands_it_has_room_for() {
    let rows = rows();
    let palette = palette();
    for height in 1..=4u16 {
        let mut harness = Harness::new(24, height);
        harness.draw(
            SearchView::new("q", &rows, &palette)
                .summary(Some("later"))
                .toggles(Toggles { regex: true, ..Toggles::default() }),
        );
        let text = harness.to_text();
        assert!(text.contains("SEARCH"), "height {height}: {text:?}");
        assert_eq!(text.contains('q'), height >= 2, "height {height}: {text:?}");
        assert_eq!(text.contains('*'), height >= 3, "height {height}: {text:?}");
        assert_eq!(text.contains("later"), height >= 4, "height {height}: {text:?}");
    }
}

#[test]
fn a_panel_with_no_area_draws_nothing() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    let before = harness.snapshot();
    harness.draw(SearchView::new("q", &rows, &palette));
    let after = harness.snapshot();
    assert_ne!(changed_rows(&before, &after), Vec::<u16>::new());

    // A zero-width or zero-height area must be a no-op rather than a panic or
    // a row of stray cells.
    let mut cells = Cells::empty(Rect::new(0, 0, 24, 8));
    SearchView::new("q", &rows, &palette).render(Rect::new(0, 0, 0, 8), &mut cells);
    SearchView::new("q", &rows, &palette).render(Rect::new(0, 0, 24, 0), &mut cells);
    assert_eq!(cells, Cells::empty(Rect::new(0, 0, 24, 8)));
}

#[test]
fn row_at_round_trips_with_the_scroll() {
    let area = Rect::new(0, 0, 24, 8);
    assert_eq!(SearchView::row_at(area, 0, 3, 4), None, "the chrome is not a row");
    assert_eq!(SearchView::row_at(area, 0, FIRST, 4), Some(0));
    assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 4), Some(3));
    assert_eq!(SearchView::row_at(area, 12, FIRST + 1, 20), Some(13), "scrolled");
    assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 3), None, "past the last row");
    assert_eq!(SearchView::row_at(area, 0, 8, 40), None, "below the panel");
}

#[test]
fn only_the_visible_window_of_results_is_drawn() {
    let lines: Vec<String> = (0..1000).map(|i| format!("hit {i}")).collect();
    let rows: Vec<SearchRow<'_>> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| SearchRow::Hit {
            line: u32::try_from(i + 1).unwrap_or(1),
            text,
            matched: &[],
        })
        .collect();
    let palette = palette();
    let mut harness = Harness::new(24, 7);
    harness.draw(SearchView::new("hit", &rows, &palette).scrolled_to(500));
    let text = harness.to_text();
    assert!(text.contains("hit 500") && text.contains("hit 502"), "{text}");
    assert!(!text.contains("hit 503"), "{text}");
}

#[test]
fn an_empty_result_list_draws_only_the_chrome() {
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("nothing", &[], &palette).summary(Some("No matches")));
    let text = harness.to_text();
    assert!(text.contains("No matches"), "{text}");
    assert_eq!(text.lines().skip(usize::from(FIRST)).collect::<String>(), "");
}

#[test]
fn the_toggles_sit_where_the_geometry_says_and_light_up() {
    let area = Rect::new(0, 0, 24, 8);
    let cells: Vec<Rect> = SearchButton::ALL
        .into_iter()
        .map(|button| SearchView::button_area(area, button).expect("wide enough"))
        .collect();
    for pair in cells.windows(2) {
        assert_eq!(pair[1].x, pair[0].x + 2, "one cell and one space apart");
        assert_eq!(pair[0].y, SearchView::toggles_area(area).y);
    }

    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(
        SearchView::new("q", &[], &palette).toggles(Toggles { case: true, ..Toggles::default() }),
    );
    let lit = palette.on(Role::Accent, Role::OnAccent).bg.expect("a lit toggle has a wash");
    let case = SearchView::button_area(area, SearchButton::Case).expect("wide enough");
    assert_eq!(harness.cells()[(case.x, case.y)].bg, lit);
    let regex = SearchView::button_area(area, SearchButton::Regex).expect("wide enough");
    assert_ne!(harness.cells()[(regex.x, regex.y)].bg, lit, "an unlit toggle is not washed");
}

#[test]
fn the_header_keeps_a_way_back_to_the_tree() {
    let area = Rect::new(0, 0, 24, 8);
    let cell = SearchView::back_area(area).expect("wide enough");
    assert_eq!(cell, Rect::new(22, 0, 1, 1), "where the tree's own buttons sit");

    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("q", &[], &palette).hovered_back(true));
    assert_eq!(harness.cells()[(cell.x, cell.y)].symbol(), BACK);
    let lit = palette.on(Role::Accent, Role::OnAccent).bg.expect("a hovered button is washed");
    assert_eq!(harness.cells()[(cell.x, cell.y)].bg, lit);

    assert_eq!(SearchView::back_area(Rect::new(0, 0, 3, 8)), None, "too narrow");
    assert_eq!(SearchView::back_area(Rect::new(0, 0, 24, 0)), None, "no header");
}

#[test]
fn the_back_button_never_overlaps_the_title() {
    let palette = palette();
    for width in 1..=16u16 {
        let area = Rect::new(0, 0, width, 5);
        let mut harness = Harness::new(width, 5);
        harness.draw(SearchView::new("q", &[], &palette));
        let header = harness.to_text().lines().next().unwrap_or_default().to_string();

        let Some(cell) = SearchView::back_area(area) else {
            assert!(!header.contains(BACK), "width {width}: {header:?}");
            continue;
        };
        assert_eq!(harness.cells()[(cell.x, cell.y)].symbol(), BACK, "width {width}");
        // The title yields four columns to it, so there is always a blank
        // between the two however far the title had to be clipped.
        assert_eq!(harness.cells()[(cell.x - 1, cell.y)].symbol(), " ", "width {width}");
        assert_eq!(header.matches(BACK).count(), 1, "width {width}: {header:?}");
    }
}

#[test]
fn a_panel_narrower_than_its_buttons_drops_them() {
    let area = Rect::new(0, 0, 4, 8);
    assert!(SearchView::button_area(area, SearchButton::Regex).is_some());
    assert!(SearchView::button_area(area, SearchButton::Case).is_some());
    assert_eq!(SearchView::button_area(area, SearchButton::Word), None, "too narrow");
    assert_eq!(SearchView::button_area(area, SearchButton::Ignored), None, "too narrow");
    assert_eq!(SearchView::button_area(Rect::new(0, 0, 24, 2), SearchButton::Regex), None);

    // And the panel draws at that width without reaching past its edge.
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(4, 8);
    harness.draw(SearchView::new("a much longer query", &rows, &palette).summary(Some("x")));
    assert!(!harness.to_text().is_empty());
}

#[test]
fn a_collapsed_file_points_its_disclosure_the_other_way() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("main", &rows, &palette));
    let text = harness.to_text();
    assert!(text.contains("▾ src/main.rs"), "{text}");
    assert!(text.contains("▸ README.md"), "{text}");
}

#[test]
fn a_file_row_right_aligns_its_hit_count() {
    let rows = vec![SearchRow::File { path: "a.rs", hits: 128, collapsed: false }];
    let palette = palette();
    let mut harness = Harness::new(24, 5);
    harness.draw(SearchView::new("x", &rows, &palette));
    let line = harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
    assert!(line.ends_with("128"), "{line:?}");
    assert_eq!(line.width(), 23, "one column of margin on the right");
}

#[test]
fn caret_at_round_trips_with_the_caret_the_query_row_draws() {
    let query = "let x";
    let palette = palette();
    let area = Rect::new(0, 0, 24, 8);
    for caret in 0..=query.chars().count() {
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new(query, &[], &palette).editing(true, caret));
        let x = caret_column(&harness);
        assert_eq!(SearchView::caret_at(area, query, caret, x), caret, "caret {caret} at {x}");
    }
}

#[test]
fn caret_at_round_trips_with_the_query_scrolled_horizontally() {
    // Distinct characters throughout, so an assertion about which of them
    // survived the scroll means something.
    let query: String = ('a'..='z').chain('A'..='N').collect();
    let chars = query.chars().count();
    let palette = palette();
    let area = Rect::new(0, 0, 20, 8);
    let mut harness = Harness::new(20, 8);
    harness.draw(SearchView::new(&query, &[], &palette).editing(true, chars));

    // Room for the text is the panel less the prompt, less the column the
    // caret keeps for itself.
    let room = usize::from(area.width - PROMPT_COLS);
    let first = chars + 1 - room;
    assert_eq!(caret_column(&harness), area.width - 1, "the caret sits at the right edge");
    assert_eq!(SearchView::caret_at(area, &query, chars, area.width - 1), chars, "the end");
    assert_eq!(SearchView::caret_at(area, &query, chars, PROMPT_COLS), first, "the leftmost char");
    assert_eq!(SearchView::caret_at(area, &query, chars, PROMPT_COLS + 5), first + 5, "and on");
    assert_eq!(SearchView::caret_at(area, &query, chars, 0), first, "a click on the prompt");

    let text = harness.to_text().lines().nth(1).unwrap_or_default().to_string();
    assert!(text.ends_with('N') && !text.contains('a'), "the tail is shown: {text:?}");
}

#[test]
fn caret_at_is_exact_with_the_caret_in_the_middle_of_a_scrolled_query() {
    // Distinct characters throughout, so an assertion about which of them a
    // column holds means something.
    let query: String = ('a'..='z').chain('A'..='N').collect();
    let palette = palette();
    let area = Rect::new(0, 0, 20, 8);
    let caret = 20;
    let mut harness = Harness::new(20, 8);
    harness.draw(SearchView::new(&query, &[], &palette).editing(true, caret));

    // Every column holding query text answers with the character drawn on it.
    // The clip mark and the caret's own cell are not query text.
    let mut checked = 0;
    for x in PROMPT_COLS..area.width {
        let drawn = harness.cells()[(x, 1)].symbol().to_string();
        if drawn == "…" || drawn == " " {
            continue;
        }
        let offset = SearchView::caret_at(area, &query, caret, x);
        let under = query.chars().nth(offset).map(String::from);
        assert_eq!(under.as_deref(), Some(drawn.as_str()), "column {x}");
        checked += 1;
    }
    assert!(checked > 10, "the query filled the row: {checked} columns");
    assert_eq!(SearchView::caret_at(area, &query, caret, caret_column(&harness)), caret);

    // And the answer genuinely turns on the caret: the window an end-anchored
    // row would show starts somewhere else entirely.
    assert_ne!(
        SearchView::caret_at(area, &query, caret, PROMPT_COLS),
        SearchView::caret_at(area, &query, query.chars().count(), PROMPT_COLS),
    );
}

#[test]
fn a_wide_character_does_not_shift_the_caret() {
    // 日 and 本 are two columns each, so the caret steps across them two
    // columns at a time rather than one column per char.
    let query = "日本x";
    let palette = palette();
    let area = Rect::new(0, 0, 24, 8);
    for (caret, column) in [(0usize, 0u16), (1, 2), (2, 4), (3, 5)] {
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new(query, &[], &palette).editing(true, caret));
        let expected = PROMPT_COLS + column;
        assert_eq!(caret_column(&harness), expected, "caret {caret}");
        assert_eq!(SearchView::caret_at(area, query, caret, expected), caret);
    }
}

#[test]
fn an_empty_query_shows_a_placeholder_until_it_is_typed_into() {
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("", &[], &palette));
    assert!(harness.to_text().contains(PLACEHOLDER));

    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("", &[], &palette).editing(true, 0));
    assert!(!harness.to_text().contains(PLACEHOLDER), "the caret is not typed over");
    assert_eq!(caret_column(&harness), PROMPT_COLS);
}

/// The columns of the first result row drawn in the accent.
///
/// A wide character counts once: a terminal never writes the cell it covers,
/// so that cell is not the panel's to colour and the diff never carries it.
fn highlighted(text: &str, matched: Range<u32>) -> Vec<u16> {
    let matched = [matched];
    let rows = vec![SearchRow::Hit { line: 1, text, matched: &matched }];
    let palette = palette();
    let mut harness = Harness::new(40, 5);
    harness.draw(SearchView::new("x", &rows, &palette));
    let accent = palette.ink(Role::Accent).fg.expect("the accent is a colour");
    harness
        .visible_cells(FIRST)
        .into_iter()
        .filter(|x| harness.cells()[(*x, FIRST)].fg == accent)
        .collect()
}

/// What is drawn at one column of the first result row.
fn symbol_at(text: &str, x: u16) -> String {
    let rows = vec![SearchRow::Hit { line: 1, text, matched: &[] }];
    let palette = palette();
    let mut harness = Harness::new(40, 5);
    harness.draw(SearchView::new("x", &rows, &palette));
    harness.cells()[(x, FIRST)].symbol().to_string()
}

#[test]
fn a_match_lands_on_the_columns_it_covers() {
    assert_eq!(
        highlighted("hello world", 6..11),
        (HIT_TEXT_X + 6..HIT_TEXT_X + 11).collect::<Vec<_>>()
    );
}

#[test]
fn a_wide_character_does_not_shift_the_highlight() {
    // 日 and 本 are two columns each, so the match on 語 — the third char —
    // lands four columns in, not two, which is where counting chars would have
    // put it.
    assert_eq!(highlighted("日本語x", 2..3), vec![HIT_TEXT_X + 4]);
    assert_eq!(symbol_at("日本語x", HIT_TEXT_X + 4), "語");
    assert_eq!(highlighted("日本語x", 3..4), vec![HIT_TEXT_X + 6]);
    assert_eq!(symbol_at("日本語x", HIT_TEXT_X + 6), "x");
}

#[test]
fn a_combining_mark_does_not_shift_the_highlight() {
    // "e" and its acute are two chars and one column, and the match on either
    // of them lights that one column.
    let text = "e\u{301}x";
    assert_eq!(highlighted(text, 0..1), vec![HIT_TEXT_X]);
    assert_eq!(highlighted(text, 1..2), vec![HIT_TEXT_X], "the mark belongs to the letter");
    assert_eq!(highlighted(text, 2..3), vec![HIT_TEXT_X + 1]);
}

#[test]
fn an_emoji_does_not_shift_the_highlight() {
    // Counting chars would put the b one column too far left.
    assert_eq!(highlighted("a👍b", 2..3), vec![HIT_TEXT_X + 3]);
    assert_eq!(symbol_at("a👍b", HIT_TEXT_X + 3), "b");
    assert_eq!(highlighted("a👍b", 1..2), vec![HIT_TEXT_X + 1]);
    assert_eq!(symbol_at("a👍b", HIT_TEXT_X + 1), "👍");
}

#[test]
fn several_matches_on_one_line_are_all_picked_out() {
    let matched = [1..2, 4..6];
    let rows = vec![SearchRow::Hit { line: 1, text: "abcdef", matched: &matched }];
    let palette = palette();
    let mut harness = Harness::new(40, 5);
    harness.draw(SearchView::new("x", &rows, &palette));
    let accent = palette.ink(Role::Accent).fg.expect("the accent is a colour");
    let lit: Vec<u16> = harness
        .visible_cells(FIRST)
        .into_iter()
        .filter(|x| harness.cells()[(*x, FIRST)].fg == accent)
        .collect();
    assert_eq!(lit, vec![HIT_TEXT_X + 1, HIT_TEXT_X + 4, HIT_TEXT_X + 5]);
}

#[test]
fn a_match_running_past_the_end_of_the_line_is_clipped() {
    // The engine windows a long line, and a range can survive the window.
    assert_eq!(highlighted("abc", 1..99), vec![HIT_TEXT_X + 1, HIT_TEXT_X + 2]);
    assert_eq!(highlighted("abc", 40..99), Vec::<u16>::new());
    assert_eq!(highlighted("", 0..5), Vec::<u16>::new());
}

#[test]
fn the_gutter_widens_to_the_longest_line_number() {
    let rows = vec![
        SearchRow::Hit { line: 3, text: "a", matched: &[] },
        SearchRow::Hit { line: 14_872, text: "b", matched: &[] },
    ];
    let palette = palette();
    let mut harness = Harness::new(30, 6);
    harness.draw(SearchView::new("x", &rows, &palette));
    let text = harness.to_text();
    let lines: Vec<&str> = text.lines().skip(usize::from(FIRST)).collect();
    assert_eq!(lines[0], "       3 a");
    assert_eq!(lines[1], "   14872 b", "the text of both starts at the same column");
}

#[test]
fn a_window_of_the_results_gets_the_gutter_the_whole_list_would_have() {
    let palette = palette();
    let whole = vec![
        SearchRow::Hit { line: 3, text: "a", matched: &[] },
        SearchRow::Hit { line: 14_872, text: "b", matched: &[] },
    ];
    let mut harness = Harness::new(30, 6);
    harness.draw(SearchView::new("x", &whole, &palette));
    let expected = harness.to_text();

    // The same two rows, handed over as a window with the list's own answer
    // for the widest line number, draw identically.
    let mut harness = Harness::new(30, 6);
    harness.draw(SearchView::new("x", &whole, &palette).widest_line(14_872));
    assert_eq!(harness.to_text(), expected, "widest_line agrees with measuring the list");
}

#[test]
fn a_window_of_short_line_numbers_keeps_room_for_the_long_ones_outside_it() {
    let palette = palette();
    // The window holds nothing wider than two digits, but the list it came
    // from runs to five, so the text must still start where it does there.
    let window = vec![SearchRow::Hit { line: 42, text: "a", matched: &[] }];
    let mut harness = Harness::new(30, 6);
    harness.draw(SearchView::new("x", &window, &palette).widest_line(14_872));
    let line = harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
    assert_eq!(line, "      42 a", "sized for five digits, not two");

    // And without it the same window shrinks to its own contents, which is
    // exactly the jitter widest_line exists to prevent.
    let mut harness = Harness::new(30, 6);
    harness.draw(SearchView::new("x", &window, &palette));
    let line = harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
    assert_eq!(line, "    42 a", "the floor of three digits, measured from the window");
}

#[test]
fn the_gutter_floor_holds_however_short_the_line_numbers_are() {
    let palette = palette();
    let rows = vec![SearchRow::Hit { line: 1, text: "a", matched: &[] }];
    for view in [
        SearchView::new("x", &rows, &palette),
        SearchView::new("x", &rows, &palette).widest_line(1),
    ] {
        let mut harness = Harness::new(30, 6);
        harness.draw(view);
        let line =
            harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
        assert_eq!(line, "     1 a");
    }
}

#[test]
fn the_selection_wash_follows_the_focus() {
    let rows = rows();
    let palette = palette();
    let focused = palette.on(Role::Selection, Role::Text).bg.expect("a wash");
    let unfocused = palette.cursor_line().bg.expect("a wash");

    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("m", &rows, &palette).selected(Some(0)).focused(true));
    assert_eq!(harness.cells()[(0, FIRST)].bg, focused);

    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("m", &rows, &palette).selected(Some(0)));
    assert_eq!(harness.cells()[(0, FIRST)].bg, unfocused);
}

#[test]
fn every_cell_is_painted() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 10);
    harness.draw(SearchView::new("main", &rows, &palette).summary(Some("3 hits")));
    let cells = harness.cells();
    for y in 0..10 {
        for x in harness.visible_cells(y) {
            assert_ne!(cells[(x, y)].bg, Color::Reset, "({x}, {y})");
        }
    }
}
