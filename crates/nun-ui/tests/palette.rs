//! The palette, drawn without a terminal.

use nun_theme::{Probe, Role, derive};
use nun_ui::{Harness, Palette, PaletteEntry, PaletteView};
use ratatui::layout::Rect;

fn colours() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn entries(rows: &[(&str, &str)]) -> Vec<PaletteEntry> {
    rows.iter()
        .map(|(label, hint)| PaletteEntry {
            label: (*label).to_string(),
            matched: Vec::new(),
            hint: (*hint).to_string(),
        })
        .collect()
}

#[test]
fn it_shows_what_was_typed_and_the_rows_under_it() {
    let rows = entries(&[("src/main.rs", ""), ("README.md", "open")]);
    let colours = colours();
    let mut harness = Harness::new(60, 8);
    let area = Rect::new(0, 0, 60, 5);
    harness.draw(PaletteView::new(&colours, "mai", &rows).selected(0));

    let text = harness.to_text();
    assert!(text.starts_with(" mai"), "{text:?}");
    assert!(text.contains("src/main.rs"), "{text:?}");
    assert!(text.contains("README.md"), "{text:?}");
    assert!(text.contains("open"), "the hint is shown: {text:?}");
    assert_eq!(PaletteView::visible_rows(area), 3);
}

#[test]
fn an_empty_query_shows_what_the_mode_is_for() {
    let rows = entries(&[]);
    let colours = colours();
    let mut harness = Harness::new(60, 6);
    harness.draw(PaletteView::new(&colours, "", &rows).placeholder("Go to a file"));
    assert!(harness.to_text().contains("Go to a file"), "{:?}", harness.to_text());
}

#[test]
fn the_matched_characters_are_picked_out() {
    let rows =
        vec![PaletteEntry { label: "main.rs".into(), matched: vec![0, 1], hint: String::new() }];
    let colours = colours();
    let mut harness = Harness::new(40, 5);
    harness.draw(PaletteView::new(&colours, "ma", &rows).selected(9));

    let cells = harness.cells();
    let accent = colours.on(Role::Overlay, Role::Accent).fg.unwrap();
    assert_eq!(cells[(1, 2)].fg, accent, "m is marked");
    assert_eq!(cells[(2, 2)].fg, accent, "a is marked");
    assert_ne!(cells[(3, 2)].fg, accent, "i is not");
}

#[test]
fn the_selected_row_is_washed_in_the_accent() {
    let rows = entries(&[("a", ""), ("b", "")]);
    let colours = colours();
    let mut harness = Harness::new(40, 6);
    harness.draw(PaletteView::new(&colours, "", &rows).selected(1));

    let cells = harness.cells();
    let selected = colours.on(Role::Accent, Role::OnAccent).bg.unwrap();
    assert_eq!(cells[(1, 3)].bg, selected);
    assert_ne!(cells[(1, 2)].bg, selected);
}

#[test]
fn only_the_rows_that_fit_are_drawn_and_scrolling_moves_them() {
    let labels: Vec<String> = (0..50).map(|index| format!("file{index}")).collect();
    let rows: Vec<PaletteEntry> = labels
        .iter()
        .map(|label| PaletteEntry {
            label: label.clone(),
            matched: Vec::new(),
            hint: String::new(),
        })
        .collect();
    let colours = colours();

    let mut harness = Harness::new(40, 8);
    harness.draw(PaletteView::new(&colours, "", &rows).scrolled_to(20));
    let text = harness.to_text();
    assert!(text.contains("file20"), "{text:?}");
    assert!(!text.contains("file19"), "{text:?}");
}

#[test]
fn it_is_centred_near_the_top_and_never_bigger_than_the_screen() {
    let screen = Rect::new(0, 0, 100, 30);
    let area = PaletteView::area(screen, 40);
    assert!(area.width <= 90);
    assert!((area.x + area.width / 2).abs_diff(screen.width / 2) <= 1, "centred: {area:?}");
    assert!(area.y < screen.height / 4, "near the top");
    assert!(area.bottom() <= screen.bottom());

    let tiny = PaletteView::area(Rect::new(0, 0, 20, 6), 40);
    assert!(tiny.width <= 20 && tiny.height <= 6);
}

#[test]
fn geometry_matches_what_is_drawn() {
    let area = Rect::new(0, 0, 40, 6);
    assert_eq!(PaletteView::row_at(area, 0, 0, 10), None, "the query line is not a row");
    assert_eq!(PaletteView::row_at(area, 0, 1, 10), None, "nor the hairline");
    assert_eq!(PaletteView::row_at(area, 0, 2, 10), Some(0));
    assert_eq!(PaletteView::row_at(area, 5, 2, 10), Some(5), "scrolled");
    assert_eq!(PaletteView::row_at(area, 0, 9, 10), None, "past the bottom");

    assert_eq!(PaletteView::scroll_to(area, 0, 0), 0);
    assert_eq!(PaletteView::scroll_to(area, 9, 0), 9 + 1 - 4);
    assert_eq!(PaletteView::scroll_to(area, 1, 5), 1, "back up");
}

#[test]
fn a_long_row_is_clipped_rather_than_spilling() {
    let rows = entries(&[("a/very/long/path/that/goes/on/and/on/for/ever/file.rs", "Ctrl+P")]);
    let colours = colours();
    let mut harness = Harness::new(30, 5);
    harness.draw(PaletteView::new(&colours, "", &rows));

    let text = harness.to_text();
    assert!(text.contains('…'), "{text:?}");
    for line in text.lines() {
        assert!(line.chars().count() <= 30, "{line:?}");
    }
}
