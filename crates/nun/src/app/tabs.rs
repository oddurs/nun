//! Open files as tabs.
//!
//! Opening a file that is already open goes to its tab rather than opening it
//! twice. Opening one over an untouched, unnamed buffer takes that buffer's
//! place, so starting nun on a folder and clicking a file does not leave an
//! empty tab behind.
//!
//! A tab is closed by its cross, by the middle button, or by `Ctrl+W`, and
//! closing one with unsaved changes asks first — with buttons, so the answer
//! is a click.

use std::path::Path;

use nun_ui::{Tab, TabStrip};
use nun_workspace::tab_labels;
use ratatui::layout::Rect;

use super::prompt::{Prompt, Purpose};
use super::{App, Document, Focus, Outcome, Target};

/// A tab being dragged along the strip.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabDrag {
    /// Which tab was picked up.
    pub(super) from: usize,
    press: (u16, u16),
    moved: bool,
    /// Where it would land.
    pub(super) drop_at: Option<usize>,
}

impl App {
    /// The tab strip's row, when there is more than one file open.
    ///
    /// One file needs no strip, and the row is better spent on text. Closing
    /// that one file still has a mouse path: the cross at the end of the
    /// status line, which is there exactly while the strip is not.
    pub(super) fn tabs_area(&self) -> Option<Rect> {
        if self.docs.len() < 2 {
            return None;
        }
        let beside = self.sidebar_area().map_or(0, |sidebar| sidebar.width);
        let width = self.viewport.width.saturating_sub(beside);
        (width > 0 && self.viewport.height > 2)
            .then(|| Rect::new(self.viewport.x + beside, self.viewport.y, width, 1))
    }

    /// What each tab says, disambiguating files that share a name.
    pub(super) fn tab_items(&self) -> Vec<Tab> {
        let paths: Vec<Option<&Path>> = self.docs.iter().map(|doc| doc.buffer.path()).collect();
        tab_labels(&paths)
            .into_iter()
            .zip(&self.docs)
            .map(|(label, doc)| Tab { label, modified: doc.buffer.is_modified() })
            .collect()
    }

    /// Lay out the strip's hit regions.
    pub(super) fn layout_tabs(&self, hits: &mut nun_input::HitMap<Target>) {
        let Some(area) = self.tabs_area() else { return };
        hits.push(super::cells(area), Target::TabStrip, false);

        let tabs = self.tab_items();
        for (index, at) in TabStrip::layout(&tabs, area, self.tab_scroll).into_iter().enumerate() {
            if at.width == 0 {
                continue;
            }
            hits.push(super::cells(at), Target::Tab(index), true);
            // The cross sits on top of its own tab, so a click on it closes
            // rather than selects.
            if let Some(close) = TabStrip::close_area(at) {
                hits.push(super::cells(close), Target::TabClose(index), true);
            }
        }
    }

    /// Keep the active tab in view.
    pub(super) fn follow_tab(&mut self) {
        let Some(area) = self.tabs_area() else {
            self.tab_scroll = 0;
            return;
        };
        let tabs = self.tab_items();
        self.tab_scroll = TabStrip::scroll_to(&tabs, area, self.active, self.tab_scroll);
    }

    /// Open `path`, in its own tab.
    pub(super) fn open_in_tab(&mut self, path: &Path) {
        if let Some(index) = self.docs.iter().position(|doc| doc.buffer.path() == Some(path)) {
            self.select_tab(index);
            return;
        }
        match crate::open(path) {
            Ok((mut buffer, report)) => {
                buffer.set_tab_width(self.doc().buffer.tab_width());
                let document = Document { buffer, scroll: 0 };
                // An untouched, unnamed buffer is the one nun started with; a
                // file opened into it replaces it rather than sitting beside
                // an empty tab.
                if self.doc().buffer.path().is_none() && !self.doc().buffer.is_modified() {
                    *self.doc_mut() = document;
                } else {
                    self.docs.insert(self.active + 1, document);
                    self.active += 1;
                }
                self.end_drag();
                if report.lossy {
                    self.message = Some(
                        "This file is not valid UTF-8. Saving it would destroy the original bytes."
                            .into(),
                    );
                }
                self.reveal_in_tree(path);
                self.follow_tab();
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    /// Show tab `index`.
    pub(super) fn select_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        self.active = index;
        self.end_drag();
        self.follow_tab();
        let path = self.doc().buffer.path().map(Path::to_path_buf);
        if let Some(path) = path {
            self.reveal_in_tree(&path);
        }
    }

    /// Move `delta` tabs along, wrapping at either end.
    pub(super) fn step_tab(&mut self, delta: isize) -> Outcome {
        let count = self.docs.len();
        if count < 2 {
            return Outcome::Continue;
        }
        let count_i = isize::try_from(count).unwrap_or(isize::MAX);
        let active = isize::try_from(self.active).unwrap_or(0);
        let next = (active + delta).rem_euclid(count_i);
        self.select_tab(usize::try_from(next).unwrap_or(0));
        self.focus = Focus::Editor;
        Outcome::Redraw
    }

    /// Close tab `index`, asking first if it has unsaved changes.
    pub(super) fn close_tab(&mut self, index: usize) -> Outcome {
        let Some(doc) = self.docs.get(index) else { return Outcome::Continue };
        if doc.buffer.is_modified() {
            let name = super::display_path(doc.buffer.path());
            self.select_tab(index);
            self.prompt = Some(Prompt::unsaved(Purpose::UnsavedThenClose(index), &name));
            return Outcome::Redraw;
        }
        self.drop_tab(index)
    }

    /// Close tab `index`, changes and all.
    pub(super) fn drop_tab(&mut self, index: usize) -> Outcome {
        if index >= self.docs.len() {
            return Outcome::Continue;
        }
        self.docs.remove(index);
        if self.docs.is_empty() {
            // The last tab leaves an empty buffer rather than an empty screen:
            // closing a file is not quitting.
            self.docs.push(Document { buffer: nun_core::Buffer::new(), scroll: 0 });
            self.active = 0;
        } else if self.active > index {
            self.active -= 1;
        } else {
            self.active = self.active.min(self.docs.len() - 1);
        }
        self.end_drag();
        self.follow_tab();
        Outcome::Redraw
    }

    /// The path of every open file, in tab order.
    #[cfg(test)]
    pub(super) fn open_paths(&self) -> Vec<Option<std::path::PathBuf>> {
        self.docs.iter().map(|doc| doc.buffer.path().map(Path::to_path_buf)).collect()
    }

    // ── the pointer ─────────────────────────────────────────────────────────

    /// A press on the strip.
    pub(super) fn tab_press(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        target: Target,
    ) -> Outcome {
        self.focus = Focus::Editor;
        match target {
            Target::TabClose(index) => self.close_tab(index),
            Target::Tab(index) => {
                self.select_tab(index);
                self.tab_drag = Some(TabDrag {
                    from: index,
                    press: (mouse.column, mouse.row),
                    moved: false,
                    drop_at: None,
                });
                Outcome::Redraw
            }
            _ => Outcome::Redraw,
        }
    }

    /// The pointer moved with a tab held. `None` when no tab is being dragged.
    pub(super) fn tab_drag_to(&mut self, column: u16, row: u16) -> Option<Outcome> {
        // Whether a tab is being dragged is asked first: a drag must not fall
        // through to selecting text because the strip moved underneath it.
        self.tab_drag?;
        let area = self.tabs_area()?;
        let tabs = self.tab_items();
        let scroll = self.tab_scroll;
        let drag = self.tab_drag.as_mut()?;
        if (column, row) != drag.press {
            drag.moved = true;
        }
        drag.drop_at = drag.moved.then(|| TabStrip::drop_index(&tabs, area, scroll, column));
        Some(Outcome::Redraw)
    }

    /// The button came up after a tab drag. `None` when there was none.
    pub(super) fn tab_release(&mut self) -> Option<Outcome> {
        let drag = self.tab_drag.take()?;
        let Some(to) = drag.drop_at else { return Some(Outcome::Redraw) };

        // Dropping just after itself is where it already is.
        let to = if to > drag.from { to - 1 } else { to };
        if to != drag.from && drag.from < self.docs.len() {
            let doc = self.docs.remove(drag.from);
            self.docs.insert(to.min(self.docs.len()), doc);
            self.active = to.min(self.docs.len() - 1);
        }
        self.follow_tab();
        Some(Outcome::Redraw)
    }

    /// The middle button closes a tab, as it does everywhere else.
    pub(super) fn tab_middle_click(&mut self, target: Target) -> Outcome {
        match target {
            Target::Tab(index) | Target::TabClose(index) => self.close_tab(index),
            _ => Outcome::Continue,
        }
    }

    /// The wheel over the strip scrolls it.
    pub(super) fn tab_scroll_by(&mut self, right: bool) -> Outcome {
        let Some(area) = self.tabs_area() else { return Outcome::Continue };
        let tabs = self.tab_items();
        let most = TabStrip::total_width(&tabs).saturating_sub(area.width);
        self.tab_scroll =
            if right { (self.tab_scroll + 4).min(most) } else { self.tab_scroll.saturating_sub(4) };
        Outcome::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, KeySet, defaults};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use std::fs;
    use tempfile::TempDir;

    /// Create each file with its own name as contents, leaving alone any a
    /// test wrote itself.
    fn files(dir: &TempDir, names: &[&str]) {
        for name in names {
            let path = dir.path().join(name);
            if path.exists() {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, format!("{name}\n")).unwrap();
        }
    }

    fn app_with(dir: &TempDir, names: &[&str]) -> App {
        files(dir, names);
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 70, 12));
        for name in names {
            app.open_in_tab(&dir.path().join(name));
        }
        // Opening through the tree or a command lays out again; here the tabs
        // are opened directly, so the hit regions need catching up.
        app.relayout();
        app
    }

    fn labels(app: &App) -> Vec<String> {
        app.tab_items().into_iter().map(|tab| tab.label).collect()
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
        mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    }

    fn tab_at(app: &App, index: usize) -> Rect {
        let area = app.tabs_area().expect("the strip is shown");
        TabStrip::layout(&app.tab_items(), area, app.tab_scroll)[index]
    }

    #[test]
    fn one_file_needs_no_strip_and_a_second_brings_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        assert!(app.tabs_area().is_none(), "one file is not a tab strip");

        files(&dir, &["b.rs"]);
        app.open_in_tab(&dir.path().join("b.rs"));
        assert!(app.tabs_area().is_some());
        assert_eq!(labels(&app), ["a.rs", "b.rs"]);
    }

    #[test]
    fn with_no_strip_the_status_line_closes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        let close = app.status_parts(app.areas().1).close.expect("a cross is offered");
        click(&mut app, close.x + 1, close.y);
        assert_eq!(app.open_paths(), vec![None], "an empty buffer, not an empty screen");
    }

    #[test]
    fn tabs_that_would_say_the_same_thing_say_where_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(&dir, &["src/mod.rs", "tests/mod.rs", "build.rs"]);
        assert_eq!(labels(&app), ["src/mod.rs", "tests/mod.rs", "build.rs"]);
    }

    #[test]
    fn clicking_a_tab_shows_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()));
        assert_eq!(app.buffer().text().to_string(), "a.rs\n");
    }

    #[test]
    fn each_tab_keeps_its_own_place_in_its_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("long.rs"), "x\n".repeat(200)).unwrap();
        fs::write(dir.path().join("short.rs"), "y\n").unwrap();
        let mut app = app_with(&dir, &["long.rs", "short.rs"]);

        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        for _ in 0..40 {
            app.handle(Event::Key(KeyEvent::from(KeyCode::Down)));
        }
        let scrolled = app.scroll();
        assert!(scrolled > 0, "the view followed the caret down");

        let second = tab_at(&app, 1);
        click(&mut app, second.x + 1, second.y);
        assert_eq!(app.scroll(), 0);
        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        assert_eq!(app.scroll(), scrolled, "back where it was left");
    }

    #[test]
    fn the_cross_closes_a_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let close = TabStrip::close_area(tab_at(&app, 1)).unwrap();
        click(&mut app, close.x, close.y);
        assert_eq!(labels(&app), ["a.rs"]);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()));
    }

    #[test]
    fn the_middle_button_closes_a_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        let second = tab_at(&app, 1);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Middle), second.x + 1, second.y);
        assert_eq!(labels(&app), ["a.rs", "c.rs"]);
    }

    #[test]
    fn closing_a_tab_with_unsaved_changes_asks_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        assert!(app.buffer().is_modified());

        app.run(Command::CloseTab);
        assert_eq!(app.open_paths().len(), 2, "still there while it asks");

        // Don't save is the middle button.
        let prompt = app.prompt.clone().unwrap();
        let discard = prompt.button_areas(app.areas().1)[1];
        click(&mut app, discard.x + 1, discard.y);
        assert_eq!(labels(&app), ["a.rs"]);
        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "b.rs\n", "not saved");
    }

    #[test]
    fn saving_from_the_close_prompt_writes_the_file_and_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::CloseTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Enter)));

        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "zb.rs\n");
        assert_eq!(labels(&app), ["a.rs"]);
    }

    #[test]
    fn cancelling_the_close_prompt_keeps_the_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::CloseTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Esc)));
        assert_eq!(labels(&app), ["a.rs", "b.rs"]);
        assert!(app.buffer().is_modified());
    }

    #[test]
    fn closing_the_last_tab_leaves_an_empty_buffer_rather_than_quitting() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        assert_eq!(app.run(Command::CloseTab), Outcome::Redraw);
        assert_eq!(app.open_paths(), vec![None]);
        assert_eq!(app.buffer().text().to_string(), "");
    }

    #[test]
    fn quitting_counts_every_unsaved_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::PreviousTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));

        assert_eq!(app.run(Command::Quit), Outcome::Redraw);
        let message = app.message().unwrap();
        assert!(message.contains("2 files have unsaved changes"), "{message}");
        assert_eq!(app.run(Command::Quit), Outcome::Quit, "asking twice is enough");
    }

    #[test]
    fn the_next_and_previous_tab_wrap_around() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        assert_eq!(app.active, 2);
        app.run(Command::NextTab);
        assert_eq!(app.active, 0, "past the last is the first");
        app.run(Command::PreviousTab);
        assert_eq!(app.active, 2);
    }

    #[test]
    fn a_tab_dragged_along_the_strip_changes_places() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        let first = tab_at(&app, 0);
        let last = tab_at(&app, 2);

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), first.x + 1, first.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), last.right() - 1, last.y);
        assert_eq!(app.tab_drag.unwrap().drop_at, Some(3), "the drop indicator is past the last");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), last.right() - 1, last.y);

        assert_eq!(labels(&app), ["b.rs", "c.rs", "a.rs"]);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()), "it stays active");
    }

    #[test]
    fn a_tab_dropped_where_it_started_stays_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let first = tab_at(&app, 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), first.x + 1, first.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), first.x + 1, first.y);
        assert_eq!(labels(&app), ["a.rs", "b.rs"]);
    }

    #[test]
    fn the_strip_sits_above_the_text_rather_than_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.set_viewport(Rect::new(0, 0, 40, 6));
        let mut harness = nun_ui::Harness::new(40, 6);
        harness.draw(crate::AppView(&app));

        let drawn = harness.to_text();
        let rows: Vec<&str> = drawn.lines().collect();
        assert!(rows[0].contains("a.rs") && rows[0].contains("b.rs"), "the strip: {:?}", rows[0]);
        assert!(rows[1].contains("b.rs"), "the first line of the file: {:?}", rows[1]);

        // And the text is one row shorter for it.
        let with_tabs = app.text_height();
        app.drop_tab(1);
        assert_eq!(app.text_height(), with_tabs + 1, "the row comes back when the strip goes");
    }

    #[test]
    fn many_tabs_scroll_rather_than_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..12).map(|index| format!("file{index}.rs")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let app = app_with(&dir, &names);

        let area = app.tabs_area().unwrap();
        assert!(TabStrip::total_width(&app.tab_items()) > area.width, "they do not all fit");
        assert!(app.tab_scroll > 0, "the strip followed the one just opened");

        let active = TabStrip::layout(&app.tab_items(), area, app.tab_scroll)[app.active];
        assert!(active.width > 0, "the active tab is on screen");
    }
}
