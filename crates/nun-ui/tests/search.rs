//! The project-search panel, drawn without a terminal.
//!
//! The layout constants the panel draws with are private to it, so this file
//! spells the same numbers out as literals. That is the point of testing it
//! from outside: a test deriving a column from the constant the code used
//! would agree with any value of it, including a wrong one.

use std::ops::Range;

use nun_theme::{Probe, Role, derive};
use nun_ui::{
    Field, Harness, HitState, Palette, SearchButton, SearchRow, SearchView, Toggles, changed_rows,
};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/// The first result row: the header, the query, the replacement, the toggles
/// and the summary sit above it.
const FIRST: u16 = 5;

/// Columns the query row's prompt occupies, before the query itself.
const PROMPT_COLS: u16 = 2;

/// Where the text of a hit row starts: a column of margin, one level of
/// indent under its file, the narrowest line-number gutter, and a space.
const HIT_TEXT_X: u16 = 7;

/// The column a hit's include marker sits in.
const MARKER_X: u16 = 1;

/// The header button that hands the sidebar back to the file tree.
const BACK: &str = "▤";

/// The replace row's button, which writes the replacement into the files.
const APPLY: &str = "⇓";

/// What an empty query field says when nobody is typing into it.
const PLACEHOLDER: &str = "Search the project";

fn palette() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn rows<'a>() -> Vec<SearchRow<'a>> {
    vec![
        SearchRow::File { path: "src/main.rs", hits: 2, collapsed: false, state: HitState::Plain },
        SearchRow::Hit { line: 7, text: "fn main() {", matched: &[], state: HitState::Plain },
        SearchRow::Hit { line: 91, text: "    render();", matched: &[], state: HitState::Plain },
        SearchRow::File { path: "README.md", hits: 1, collapsed: true, state: HitState::Plain },
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
    let mut harness = Harness::new(24, 9);
    harness.draw(
        SearchView::new("main", &rows, &palette).summary(Some("3 hits in 2 files")).focused(true),
    );

    assert_eq!(
        harness.to_text(),
        " SEARCH               ▤\n\
         ⌕ main\n\
         → Replace with        ⇓\n\
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
    assert_eq!(SearchView::replace_area(area), Rect::new(0, 2, 24, 1));
    assert_eq!(SearchView::toggles_area(area), Rect::new(0, 3, 24, 1));
    assert_eq!(SearchView::summary_area(area), Rect::new(0, 4, 24, 1));
    assert_eq!(SearchView::rows_area(area), Rect::new(0, 5, 24, 4));
    assert_eq!(SearchView::visible_rows(area), 4);
}

#[test]
fn a_panel_too_short_for_a_band_gives_it_no_rows() {
    for height in 0..=4u16 {
        let area = Rect::new(0, 0, 24, height);
        let bands = [
            SearchView::header_area(area),
            SearchView::query_area(area),
            SearchView::replace_area(area),
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
    for height in 1..=6u16 {
        let mut harness = Harness::new(24, height);
        harness.draw(
            SearchView::new("q", &rows, &palette)
                .replacement("zz")
                .summary(Some("later"))
                .toggles(Toggles { regex: true, ..Toggles::default() }),
        );
        let text = harness.to_text();
        assert!(text.contains("SEARCH"), "height {height}: {text:?}");
        assert_eq!(text.contains('q'), height >= 2, "height {height}: {text:?}");
        assert_eq!(text.contains("zz"), height >= 3, "height {height}: {text:?}");
        assert_eq!(text.contains('*'), height >= 4, "height {height}: {text:?}");
        assert_eq!(text.contains("later"), height >= 5, "height {height}: {text:?}");
        assert_eq!(text.contains("src/main.rs"), height >= 6, "height {height}: {text:?}");
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
    let area = Rect::new(0, 0, 24, 9);
    assert_eq!(SearchView::row_at(area, 0, 4, 4), None, "the chrome is not a row");
    assert_eq!(SearchView::row_at(area, 0, FIRST, 4), Some(0));
    assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 4), Some(3));
    assert_eq!(SearchView::row_at(area, 12, FIRST + 1, 20), Some(13), "scrolled");
    assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 3), None, "past the last row");
    assert_eq!(SearchView::row_at(area, 0, 9, 40), None, "below the panel");
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
            state: HitState::Plain,
        })
        .collect();
    let palette = palette();
    let mut harness = Harness::new(24, 8);
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
    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("main", &rows, &palette));
    let text = harness.to_text();
    assert!(text.contains("▾ src/main.rs"), "{text}");
    assert!(text.contains("▸ README.md"), "{text}");
}

#[test]
fn a_file_row_right_aligns_its_hit_count() {
    let rows =
        vec![SearchRow::File { path: "a.rs", hits: 128, collapsed: false, state: HitState::Plain }];
    let palette = palette();
    let mut harness = Harness::new(24, 7);
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
        harness.draw(SearchView::new(query, &[], &palette).editing(Some(Field::Query), caret));
        let x = caret_column(&harness);
        assert_eq!(
            SearchView::caret_at(area, Field::Query, query, caret, x),
            caret,
            "caret {caret} at {x}"
        );
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
    harness.draw(SearchView::new(&query, &[], &palette).editing(Some(Field::Query), chars));

    // Room for the text is the panel less the prompt, less the column the
    // caret keeps for itself.
    let room = usize::from(area.width - PROMPT_COLS);
    let first = chars + 1 - room;
    assert_eq!(caret_column(&harness), area.width - 1, "the caret sits at the right edge");
    assert_eq!(
        SearchView::caret_at(area, Field::Query, &query, chars, area.width - 1),
        chars,
        "the end"
    );
    assert_eq!(
        SearchView::caret_at(area, Field::Query, &query, chars, PROMPT_COLS),
        first,
        "the leftmost char"
    );
    assert_eq!(
        SearchView::caret_at(area, Field::Query, &query, chars, PROMPT_COLS + 5),
        first + 5,
        "and on"
    );
    assert_eq!(
        SearchView::caret_at(area, Field::Query, &query, chars, 0),
        first,
        "a click on the prompt"
    );

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
    harness.draw(SearchView::new(&query, &[], &palette).editing(Some(Field::Query), caret));

    // Every column holding query text answers with the character drawn on it.
    // The clip mark and the caret's own cell are not query text.
    let mut checked = 0;
    for x in PROMPT_COLS..area.width {
        let drawn = harness.cells()[(x, 1)].symbol().to_string();
        if drawn == "…" || drawn == " " {
            continue;
        }
        let offset = SearchView::caret_at(area, Field::Query, &query, caret, x);
        let under = query.chars().nth(offset).map(String::from);
        assert_eq!(under.as_deref(), Some(drawn.as_str()), "column {x}");
        checked += 1;
    }
    assert!(checked > 10, "the query filled the row: {checked} columns");
    assert_eq!(
        SearchView::caret_at(area, Field::Query, &query, caret, caret_column(&harness)),
        caret
    );

    // And the answer genuinely turns on the caret: the window an end-anchored
    // row would show starts somewhere else entirely.
    assert_ne!(
        SearchView::caret_at(area, Field::Query, &query, caret, PROMPT_COLS),
        SearchView::caret_at(area, Field::Query, &query, query.chars().count(), PROMPT_COLS),
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
        harness.draw(SearchView::new(query, &[], &palette).editing(Some(Field::Query), caret));
        let expected = PROMPT_COLS + column;
        assert_eq!(caret_column(&harness), expected, "caret {caret}");
        assert_eq!(SearchView::caret_at(area, Field::Query, query, caret, expected), caret);
    }
}

#[test]
fn an_empty_query_shows_a_placeholder_until_it_is_typed_into() {
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("", &[], &palette));
    assert!(harness.to_text().contains(PLACEHOLDER));

    let mut harness = Harness::new(24, 8);
    harness.draw(SearchView::new("", &[], &palette).editing(Some(Field::Query), 0));
    assert!(!harness.to_text().contains(PLACEHOLDER), "the caret is not typed over");
    assert_eq!(caret_column(&harness), PROMPT_COLS);
}

/// A file with one hit in each of the three states, and the `After` row the
/// included one is followed by.
fn diff_rows<'a>() -> Vec<SearchRow<'a>> {
    vec![
        SearchRow::File { path: "a.rs", hits: 3, collapsed: false, state: HitState::Plain },
        SearchRow::Hit { line: 1, text: "let old = 1;", matched: &[], state: HitState::Included },
        SearchRow::After { line: 1, text: "let new = 1;" },
        SearchRow::Hit { line: 2, text: "let old = 2;", matched: &[], state: HitState::Excluded },
        SearchRow::Hit { line: 3, text: "let old = 3;", matched: &[], state: HitState::Plain },
    ]
}

/// The ink one row of the results is drawn in, taken from a column of its
/// text rather than its marker, so the two are checked separately.
fn row_ink(harness: &Harness, row: u16) -> Color {
    harness.cells()[(HIT_TEXT_X, FIRST + row)].fg
}

#[test]
fn the_three_hit_states_draw_their_own_marker_and_colour() {
    let rows = diff_rows();
    let palette = palette();
    let mut harness = Harness::new(30, 11);
    harness.draw(SearchView::new("old", &rows, &palette).replacement("new"));

    let marker = |row: u16| harness.cells()[(MARKER_X, FIRST + row)].symbol().to_string();
    assert_eq!(marker(1), "-", "an included hit is a line going away");
    assert_eq!(marker(2), "+", "the after row is the line arriving");
    assert_eq!(marker(3), "·", "an excluded hit is neither");
    assert_eq!(marker(4), " ", "a plain hit is not part of a diff");

    let ink = |role| palette.ink(role).fg.expect("a role is a colour");
    assert_eq!(row_ink(&harness, 1), ink(Role::Removed));
    assert_eq!(row_ink(&harness, 2), ink(Role::Added));
    assert_eq!(row_ink(&harness, 3), ink(Role::Faint));
    assert_eq!(row_ink(&harness, 4), ink(Role::Text), "a plain hit is drawn as it was");
}

#[test]
fn an_after_row_lines_up_under_the_hit_it_replaces() {
    let rows = diff_rows();
    let palette = palette();
    let mut harness = Harness::new(30, 11);
    harness.draw(SearchView::new("old", &rows, &palette).replacement("new"));
    let text = harness.to_text();
    let lines: Vec<&str> = text.lines().skip(usize::from(FIRST)).collect();

    assert_eq!(lines[1], " -   1 let old = 1;");
    assert_eq!(lines[2], " +   1 let new = 1;", "the same line number, so the eye reads down");
    assert_eq!(lines[3], " ·   2 let old = 2;");
    assert_eq!(lines[4], "     3 let old = 3;");
}

#[test]
fn the_marker_is_its_own_hit_region_only_where_there_is_one_to_click() {
    let rows = diff_rows();
    let area = Rect::new(0, 0, 30, 11);

    let marker = |row| SearchView::marker_area(area, &rows, row, 0);
    assert_eq!(marker(0), None, "a file row is not included or excluded");
    assert_eq!(marker(1), Some(Rect::new(MARKER_X, FIRST + 1, 1, 1)), "the included hit");
    assert_eq!(marker(2), None, "an after row is not a thing to toggle");
    assert_eq!(marker(3), Some(Rect::new(MARKER_X, FIRST + 3, 1, 1)), "the excluded hit");
    assert_eq!(marker(4), None, "nothing is being replaced on a plain hit");
    assert_eq!(marker(5), None, "past the last row");

    // It sits inside the row it belongs to, and on the cell that draws it.
    let cell = SearchView::marker_area(area, &rows, 1, 0).expect("the included hit has one");
    assert_eq!(SearchView::row_at(area, 0, cell.y, rows.len()), Some(1));
}

#[test]
fn a_marker_scrolled_off_the_panel_has_no_hit_region() {
    let rows = diff_rows();
    // Four result rows fit; row 1 is the first included hit.
    let area = Rect::new(0, 0, 30, 9);
    assert_eq!(SearchView::visible_rows(area), 4);
    assert!(SearchView::marker_area(area, &rows, 3, 0).is_some(), "the last row that fits");
    assert_eq!(SearchView::marker_area(area, &rows, 4, 0), None, "one row past the window");

    // Scrolled, the same row moves up into view and the one above it leaves.
    assert_eq!(SearchView::marker_area(area, &rows, 1, 2), None, "scrolled off the top");
    let cell = SearchView::marker_area(area, &rows, 3, 2).expect("now the second row shown");
    assert_eq!(cell.y, FIRST + 1);

    // And a panel with no room for results has no markers at all.
    let short = Rect::new(0, 0, 30, 5);
    assert_eq!(SearchView::marker_area(short, &rows, 1, 0), None, "no results band");
    let narrow = Rect::new(0, 0, 1, 11);
    assert_eq!(SearchView::marker_area(narrow, &rows, 1, 0), None, "no column to spare");
}

#[test]
fn a_diff_draws_at_every_size_without_reaching_past_the_panel() {
    let rows = diff_rows();
    let palette = palette();
    for height in 1..=8u16 {
        for width in 1..=14u16 {
            let mut harness = Harness::new(width, height);
            harness.draw(
                SearchView::new("old", &rows, &palette)
                    .replacement("a much longer replacement than fits")
                    .summary(Some("3 hits in 1 file"))
                    .selected(Some(1))
                    .hovered(Some(2))
                    .hovered_apply(true)
                    .editing(Some(Field::Replace), 30),
            );
            // Nothing panicked, and the panel painted every cell it was given
            // rather than leaving the terminal's own colour showing through.
            for y in 0..height {
                for x in harness.visible_cells(y) {
                    assert_ne!(
                        harness.cells()[(x, y)].bg,
                        Color::Reset,
                        "({x}, {y}) at {width}x{height}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_replace_row_prompts_differently_from_the_query() {
    let palette = palette();
    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("abc", &[], &palette).replacement("xyz"));
    let cells = harness.cells();
    assert_eq!(cells[(0, 1)].symbol(), "⌕", "the query is searched for");
    assert_eq!(cells[(0, 2)].symbol(), "→", "the replacement is what it becomes");
    assert_eq!(cells[(1, 2)].symbol(), " ");
    assert_eq!(cells[(PROMPT_COLS, 2)].symbol(), "x", "and its text starts after the prompt");
}

#[test]
fn an_empty_replacement_says_what_the_field_is_for() {
    let palette = palette();
    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("abc", &[], &palette));
    assert!(harness.to_text().contains("Replace with"));

    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("abc", &[], &palette).editing(Some(Field::Replace), 0));
    let text = harness.to_text();
    assert!(!text.contains("Replace with"), "the caret is not typed over: {text:?}");
}

#[test]
fn only_the_field_with_the_keyboard_carries_a_caret() {
    let palette = palette();
    let accent = palette.on(Role::Accent, Role::OnAccent).bg.expect("the caret has a wash");
    let lit = |harness: &Harness, row: u16| {
        (0..harness.area().width).any(|x| harness.cells()[(x, row)].bg == accent)
    };

    let mut harness = Harness::new(24, 9);
    harness.draw(
        SearchView::new("abc", &[], &palette).replacement("xyz").editing(Some(Field::Query), 1),
    );
    assert!(lit(&harness, 1) && !lit(&harness, 2), "the query has it");

    let mut harness = Harness::new(24, 9);
    harness.draw(
        SearchView::new("abc", &[], &palette).replacement("xyz").editing(Some(Field::Replace), 1),
    );
    assert!(!lit(&harness, 1) && lit(&harness, 2), "the replacement has it");

    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("abc", &[], &palette).replacement("xyz").editing(None, 0));
    assert!(!lit(&harness, 1) && !lit(&harness, 2), "neither does");
}

#[test]
fn caret_at_round_trips_in_the_replace_field_too() {
    let text = "let x";
    let palette = palette();
    let area = Rect::new(0, 0, 24, 9);
    for caret in 0..=text.chars().count() {
        let mut harness = Harness::new(24, 9);
        harness.draw(
            SearchView::new("q", &[], &palette)
                .replacement(text)
                .editing(Some(Field::Replace), caret),
        );
        let accent = palette.on(Role::Accent, Role::OnAccent).bg.expect("a wash");
        let x = (0..area.width)
            .find(|x| harness.cells()[(*x, 2)].bg == accent)
            .expect("the caret is drawn on the replace row");
        assert_eq!(SearchView::caret_at(area, Field::Replace, text, caret, x), caret, "at {x}");
    }
}

#[test]
fn caret_at_is_exact_with_the_replacement_scrolled_and_the_caret_mid_text() {
    // Distinct characters throughout, so an assertion about which of them a
    // column holds means something.
    let text: String = ('a'..='z').chain('A'..='N').collect();
    let palette = palette();
    let area = Rect::new(0, 0, 20, 9);
    let caret = 20;
    let mut harness = Harness::new(20, 9);
    harness.draw(
        SearchView::new("q", &[], &palette).replacement(&text).editing(Some(Field::Replace), caret),
    );

    let mut checked = 0;
    for x in PROMPT_COLS..area.width {
        let drawn = harness.cells()[(x, 2)].symbol().to_string();
        if drawn == "…" || drawn == " " || drawn == "⇓" {
            continue;
        }
        let offset = SearchView::caret_at(area, Field::Replace, &text, caret, x);
        assert_eq!(text.chars().nth(offset).map(String::from).as_deref(), Some(drawn.as_str()));
        checked += 1;
    }
    assert!(checked > 5, "the replacement filled the row: {checked} columns");

    // The replace field is narrower than the query one by the apply button's
    // columns, so the same text and caret scroll to a different place.
    assert_ne!(
        SearchView::caret_at(area, Field::Replace, &text, caret, PROMPT_COLS),
        SearchView::caret_at(area, Field::Query, &text, caret, PROMPT_COLS),
    );
}

#[test]
fn the_apply_button_never_overlaps_the_replacement() {
    let long = "x".repeat(40);
    let palette = palette();
    for width in 1..=20u16 {
        let area = Rect::new(0, 0, width, 9);
        let mut harness = Harness::new(width, 9);
        harness.draw(SearchView::new("q", &[], &palette).replacement(&long));

        let Some(cell) = SearchView::apply_area(area) else {
            assert!(!harness.to_text().contains(APPLY), "width {width}");
            continue;
        };
        assert_eq!(harness.cells()[(cell.x, cell.y)].symbol(), APPLY, "width {width}");
        assert_eq!(cell.y, SearchView::replace_area(area).y, "it sits on the replace row");
        // A blank between the text and the button, however long the text.
        assert_eq!(harness.cells()[(cell.x - 1, cell.y)].symbol(), " ", "width {width}");
    }

    assert_eq!(SearchView::apply_area(Rect::new(0, 0, 3, 9)), None, "too narrow");
    assert_eq!(SearchView::apply_area(Rect::new(0, 0, 24, 2)), None, "no replace row");
}

#[test]
fn the_apply_button_is_the_one_control_that_is_not_drawn_in_the_accent() {
    let palette = palette();
    let area = Rect::new(0, 0, 24, 9);
    let cell = SearchView::apply_area(area).expect("wide enough");

    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("q", &[], &palette));
    let warn = palette.ink(Role::Warn).fg.expect("a role is a colour");
    assert_eq!(harness.cells()[(cell.x, cell.y)].fg, warn);

    // Hovering does not light it up the way the reversible buttons light up:
    // it keeps the warning colour and takes the quieter wash.
    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("q", &[], &palette).hovered_apply(true));
    let accent = palette.on(Role::Accent, Role::OnAccent).bg.expect("a wash");
    assert_eq!(harness.cells()[(cell.x, cell.y)].fg, warn, "still a warning");
    assert_ne!(harness.cells()[(cell.x, cell.y)].bg, accent, "not an invitation");
    assert_eq!(harness.cells()[(cell.x, cell.y)].bg, palette.cursor_line().bg.expect("a wash"));
}

#[test]
fn an_included_hit_keeps_its_match_picked_out() {
    let matched = [4..5, 5..7];
    let rows = vec![SearchRow::Hit {
        line: 1,
        text: "let old = 1;",
        matched: &matched,
        state: HitState::Included,
    }];
    let palette = palette();
    let mut harness = Harness::new(40, 7);
    harness.draw(SearchView::new("old", &rows, &palette).replacement("new"));

    let accent = palette.ink(Role::Accent).fg.expect("a role is a colour");
    let lit: Vec<u16> = harness
        .visible_cells(FIRST)
        .into_iter()
        .filter(|x| harness.cells()[(*x, FIRST)].fg == accent)
        .collect();
    assert_eq!(lit, vec![HIT_TEXT_X + 4, HIT_TEXT_X + 5, HIT_TEXT_X + 6]);
}

/// The columns of the first result row drawn in the accent.
///
/// A wide character counts once: a terminal never writes the cell it covers,
/// so that cell is not the panel's to colour and the diff never carries it.
fn highlighted(text: &str, matched: Range<u32>) -> Vec<u16> {
    let matched = [matched];
    let rows = vec![SearchRow::Hit { line: 1, text, matched: &matched, state: HitState::Plain }];
    let palette = palette();
    let mut harness = Harness::new(40, 7);
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
    let rows = vec![SearchRow::Hit { line: 1, text, matched: &[], state: HitState::Plain }];
    let palette = palette();
    let mut harness = Harness::new(40, 7);
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
    let rows =
        vec![SearchRow::Hit { line: 1, text: "abcdef", matched: &matched, state: HitState::Plain }];
    let palette = palette();
    let mut harness = Harness::new(40, 7);
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
        SearchRow::Hit { line: 3, text: "a", matched: &[], state: HitState::Plain },
        SearchRow::Hit { line: 14_872, text: "b", matched: &[], state: HitState::Plain },
    ];
    let palette = palette();
    let mut harness = Harness::new(30, 8);
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
        SearchRow::Hit { line: 3, text: "a", matched: &[], state: HitState::Plain },
        SearchRow::Hit { line: 14_872, text: "b", matched: &[], state: HitState::Plain },
    ];
    let mut harness = Harness::new(30, 8);
    harness.draw(SearchView::new("x", &whole, &palette));
    let expected = harness.to_text();

    // The same two rows, handed over as a window with the list's own answer
    // for the widest line number, draw identically.
    let mut harness = Harness::new(30, 8);
    harness.draw(SearchView::new("x", &whole, &palette).widest_line(14_872));
    assert_eq!(harness.to_text(), expected, "widest_line agrees with measuring the list");
}

#[test]
fn a_window_of_short_line_numbers_keeps_room_for_the_long_ones_outside_it() {
    let palette = palette();
    // The window holds nothing wider than two digits, but the list it came
    // from runs to five, so the text must still start where it does there.
    let window = vec![SearchRow::Hit { line: 42, text: "a", matched: &[], state: HitState::Plain }];
    let mut harness = Harness::new(30, 8);
    harness.draw(SearchView::new("x", &window, &palette).widest_line(14_872));
    let line = harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
    assert_eq!(line, "      42 a", "sized for five digits, not two");

    // And without it the same window shrinks to its own contents, which is
    // exactly the jitter widest_line exists to prevent.
    let mut harness = Harness::new(30, 8);
    harness.draw(SearchView::new("x", &window, &palette));
    let line = harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
    assert_eq!(line, "    42 a", "the floor of three digits, measured from the window");
}

#[test]
fn the_gutter_floor_holds_however_short_the_line_numbers_are() {
    let palette = palette();
    let rows = vec![SearchRow::Hit { line: 1, text: "a", matched: &[], state: HitState::Plain }];
    for view in [
        SearchView::new("x", &rows, &palette),
        SearchView::new("x", &rows, &palette).widest_line(1),
    ] {
        let mut harness = Harness::new(30, 8);
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

#[test]
fn a_preview_with_actions_draws_them_where_the_toggles_go_and_says_where_they_are() {
    let palette = palette();
    let area = Rect::new(0, 0, 30, 8);
    let actions = ["Rename", "Cancel"];
    let mut harness = Harness::new(30, 8);
    harness.draw(SearchView::new("cat", &[], &palette).title("RENAME").actions(&actions));
    let text = harness.to_text();
    assert!(text.lines().next().unwrap_or_default().contains("RENAME"), "{text}");
    let toggles = text.lines().nth(3).unwrap_or_default();
    assert_eq!(toggles, "  Rename   Cancel", "trailing blanks are trimmed");
    for glyph in SearchButton::ALL.map(SearchButton::glyph) {
        assert!(!toggles.contains(palette.glyph(glyph)), "no toggles: {toggles:?}");
    }

    let rename = SearchView::action_area(area, &actions, 0).unwrap();
    let cancel = SearchView::action_area(area, &actions, 1).unwrap();
    assert_eq!((rename.x, rename.y, rename.width), (1, 3, 8));
    assert_eq!(cancel.x, rename.right() + 1);
    assert_eq!(SearchView::action_area(area, &actions, 2), None);
    assert_eq!(
        SearchView::action_area(Rect::new(0, 0, 12, 8), &actions, 1),
        None,
        "one that does not fit is not there"
    );
}

#[test]
fn a_file_that_is_in_or_out_has_a_mark_at_the_edge_that_can_be_clicked() {
    let rows = vec![
        SearchRow::File { path: "a.rs", hits: 1, collapsed: false, state: HitState::Included },
        SearchRow::File { path: "b.rs", hits: 1, collapsed: false, state: HitState::Excluded },
        SearchRow::File { path: "c.rs", hits: 1, collapsed: false, state: HitState::Plain },
    ];
    let palette = palette();
    let area = Rect::new(0, 0, 24, 9);
    let mut harness = Harness::new(24, 9);
    harness.draw(SearchView::new("x", &rows, &palette));
    let lines: Vec<String> = harness.to_text().lines().map(str::to_string).collect();
    let first = usize::from(FIRST);
    assert!(lines[first].starts_with("✓▾ a.rs"), "{:?}", lines[first]);
    assert!(lines[first + 1].starts_with("·▾ b.rs"), "{:?}", lines[first + 1]);
    assert!(lines[first + 2].starts_with(" ▾ c.rs"), "{:?}", lines[first + 2]);

    let mark = SearchView::marker_area(area, &rows, 0, 0).unwrap();
    assert_eq!((mark.x, mark.y), (0, FIRST));
    assert!(SearchView::marker_area(area, &rows, 1, 0).is_some());
    assert_eq!(SearchView::marker_area(area, &rows, 2, 0), None, "a plain file has none");
}
