//! The tab strip, drawn without a terminal.

use nun_theme::{Probe, Role, derive};
use nun_ui::{Harness, Palette, Tab, TabStrip};
use ratatui::layout::Rect;

fn palette() -> Palette {
    Palette::new(derive(&Probe::builtin_dark()))
}

fn tabs(names: &[(&str, bool)]) -> Vec<Tab> {
    names
        .iter()
        .map(|(label, modified)| Tab { label: (*label).to_string(), modified: *modified })
        .collect()
}

#[test]
fn tabs_are_drawn_in_order_with_the_active_one_apart() {
    let tabs = tabs(&[("a.rs", false), ("b.rs", false)]);
    let palette = palette();
    let mut harness = Harness::new(40, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 1));

    let text = harness.to_text();
    assert!(text.starts_with(" a.rs"), "{text:?}");
    assert!(text.contains("b.rs"), "{text:?}");

    // The active tab is drawn on the editor's own ground, so it reads as
    // joined to the text below it.
    let active = TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[1];
    let cells = harness.cells();
    assert_eq!(cells[(active.x + 1, 0)].bg, palette.on(Role::Ground, Role::Text).bg.unwrap());
    let idle = TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[0];
    assert_ne!(cells[(idle.x + 1, 0)].bg, cells[(active.x + 1, 0)].bg);
}

#[test]
fn an_unsaved_tab_shows_a_dot_and_the_active_one_shows_its_cross() {
    let tabs = tabs(&[("a.rs", true), ("b.rs", true)]);
    let palette = palette();
    let mut harness = Harness::new(40, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 1));

    let first =
        TabStrip::close_area(TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[0]).unwrap();
    let second =
        TabStrip::close_area(TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[1]).unwrap();
    assert_eq!(harness.cells()[(first.x, 0)].symbol(), "•", "unsaved, and not under the pointer");
    assert_eq!(harness.cells()[(second.x, 0)].symbol(), "×", "the active tab offers its cross");
}

#[test]
fn a_hovered_tab_offers_its_cross_too() {
    let tabs = tabs(&[("a.rs", true), ("b.rs", false)]);
    let palette = palette();
    let mut harness = Harness::new(40, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 1).hovered(Some(0)));

    let first =
        TabStrip::close_area(TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[0]).unwrap();
    assert_eq!(harness.cells()[(first.x, 0)].symbol(), "×");
}

#[test]
fn a_long_label_is_clipped_rather_than_pushing_the_next_tab_along() {
    let tabs = tabs(&[("a_very_long_file_name_indeed.rs", false), ("b.rs", false)]);
    let areas = TabStrip::layout(&tabs, Rect::new(0, 0, 60, 1), 0);
    assert!(areas[0].width <= 28, "a tab has a limit: {}", areas[0].width);

    let palette = palette();
    let mut harness = Harness::new(60, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 0));
    assert!(harness.to_text().contains('…'));
}

#[test]
fn tabs_scrolled_off_the_left_are_not_drawn() {
    let tabs = tabs(&[("a.rs", false), ("b.rs", false), ("c.rs", false)]);
    let palette = palette();
    let mut harness = Harness::new(20, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 2).scrolled_by(16));

    let text = harness.to_text();
    assert!(text.contains("c.rs"), "{text:?}");
    assert!(!text.contains("a.rs"), "{text:?}");
}

#[test]
fn scrolling_follows_a_tab_that_is_off_either_end() {
    let tabs = tabs(&[("a.rs", false), ("b.rs", false), ("c.rs", false)]);
    let area = Rect::new(0, 0, 20, 1);
    let widths = TabStrip::widths(&tabs);

    let to_last = TabStrip::scroll_to(&tabs, area, 2, 0);
    assert_eq!(to_last, TabStrip::total_width(&tabs) - area.width);
    assert_eq!(TabStrip::scroll_to(&tabs, area, 0, to_last), 0, "and back to the first");
    assert_eq!(TabStrip::scroll_to(&tabs, area, 1, 0), 0, "one already in view does not move it");
    assert!(widths.iter().all(|width| *width > 0));
}

#[test]
fn the_drop_indicator_is_drawn_where_a_tab_would_land() {
    let tabs = tabs(&[("a.rs", false), ("b.rs", false)]);
    let palette = palette();
    let mut harness = Harness::new(40, 2);
    harness.draw(TabStrip::new(&tabs, &palette, 0).drop_at(Some(1)));

    let second = TabStrip::layout(&tabs, Rect::new(0, 0, 40, 1), 0)[1];
    assert_eq!(harness.cells()[(second.x, 0)].symbol(), "▏");
}

#[test]
fn the_drop_index_is_the_gap_the_pointer_is_nearest() {
    let tabs = tabs(&[("a.rs", false), ("b.rs", false)]);
    let area = Rect::new(0, 0, 40, 1);
    let areas = TabStrip::layout(&tabs, area, 0);

    assert_eq!(TabStrip::drop_index(&tabs, area, 0, areas[0].x), 0, "left of the first");
    assert_eq!(TabStrip::drop_index(&tabs, area, 0, areas[0].right() - 1), 1, "right of it");
    assert_eq!(TabStrip::drop_index(&tabs, area, 0, areas[1].right() + 5), 2, "past the last");
}

#[test]
fn every_cell_of_the_strip_is_painted() {
    let tabs = tabs(&[("a.rs", false)]);
    let palette = palette();
    let mut harness = Harness::new(30, 1);
    harness.draw(TabStrip::new(&tabs, &palette, 0));
    let cells = harness.cells();
    for x in harness.visible_cells(0) {
        assert_ne!(cells[(x, 0)].bg, ratatui::style::Color::Reset, "column {x}");
    }
}
