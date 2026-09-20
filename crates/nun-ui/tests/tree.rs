//! The sidebar, drawn without a terminal.

use std::path::PathBuf;

use nun_theme::{Probe, derive};
use nun_ui::{Harness, Palette, TreeButton, TreeView};
use nun_workspace::{Kind, Row};
use ratatui::layout::Rect;

fn palette() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn row(depth: usize, name: &str, kind: Kind, expanded: bool) -> Row {
    Row {
        depth,
        name: name.to_string(),
        path: PathBuf::from(name),
        kind,
        expanded,
        ignored: false,
        error: None,
    }
}

fn rows() -> Vec<Row> {
    vec![
        row(0, "src", Kind::Dir, true),
        row(1, "main.rs", Kind::File, false),
        row(1, "日本語.rs", Kind::File, false),
        row(0, "target", Kind::Dir, false),
        row(0, "README.md", Kind::File, false),
    ]
}

#[test]
fn the_tree_draws_a_header_and_indented_rows() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 6);
    harness.draw(TreeView::new("nun", &rows, &palette));

    assert_eq!(
        harness.to_text(),
        " NUN            + ▪ ○ ⌕\n\
         \u{20}▾ src\n\
         \u{20}  ▸ main.rs\n\
         \u{20}    日本語.rs\n\
         \u{20}▸ target\n\
         \u{20}  README.md"
            .replace("▸ main", "  main")
    );
}

#[test]
fn a_name_too_long_for_the_sidebar_ends_in_an_ellipsis() {
    let rows = vec![row(0, "a_very_long_file_name.rs", Kind::File, false)];
    let palette = palette();
    let mut harness = Harness::new(14, 2);
    harness.draw(TreeView::new("x", &rows, &palette));
    let line = harness.to_text().lines().nth(1).unwrap().to_string();
    assert!(line.ends_with('…'), "{line:?}");
}

#[test]
fn only_the_visible_window_is_drawn() {
    let rows: Vec<Row> = (0..1000).map(|i| row(0, &format!("f{i}"), Kind::File, false)).collect();
    let palette = palette();
    let mut harness = Harness::new(20, 4);
    harness.draw(TreeView::new("x", &rows, &palette).scrolled_to(500));
    let text = harness.to_text();
    assert!(text.contains("f500") && text.contains("f502"), "{text}");
    assert!(!text.contains("f503"));
}

#[test]
fn geometry_matches_what_is_drawn() {
    let area = Rect::new(0, 0, 24, 6);
    assert_eq!(TreeView::row_at(area, 0, 0, 5), None, "the header is not a row");
    assert_eq!(TreeView::row_at(area, 0, 1, 5), Some(0));
    assert_eq!(TreeView::row_at(area, 3, 1, 5), Some(3), "scrolled");
    assert_eq!(TreeView::row_at(area, 0, 5, 3), None, "past the last row");
    assert_eq!(TreeView::visible_rows(area), 5);

    let new_file = TreeView::button_area(area, TreeButton::NewFile).unwrap();
    let toggle = TreeView::button_area(area, TreeButton::ToggleIgnored).unwrap();
    let search = TreeView::button_area(area, TreeButton::Search).unwrap();
    assert_eq!(new_file.y, 0);
    assert!(toggle.x > new_file.x);
    assert!(search.x > toggle.x, "the magnifier is rightmost");
    assert_eq!(
        TreeView::button_area(Rect::new(0, 0, 4, 6), TreeButton::NewFile),
        None,
        "too narrow"
    );
}

#[test]
fn every_cell_is_painted() {
    let rows = rows();
    let palette = palette();
    let mut harness = Harness::new(24, 8);
    harness.draw(TreeView::new("nun", &rows, &palette));
    let cells = harness.cells();
    for y in 0..8 {
        for x in harness.visible_cells(y) {
            assert_ne!(cells[(x, y)].bg, ratatui::style::Color::Reset, "({x}, {y})");
        }
    }
}
