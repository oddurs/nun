//! The editor: state, one event at a time, and what to draw.
//!
//! Deliberately free of terminal I/O. `App` takes an event and mutates itself;
//! `main` does the reading and the drawing. That keeps the whole of the editor's
//! behaviour testable without a tty, which is how the keymap and the scrolling
//! are checked below.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nun_core::{Buffer, Range, SaveError, Selections};
use nun_theme::Role;
use nun_ui::{EditorView, Event, Palette};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

/// What the editor wants the caller to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Carry on.
    Continue,
    /// Redraw and carry on.
    Redraw,
    /// Put the terminal back, stop, and re-enter on resume.
    Suspend,
    /// Shut down.
    Quit,
}

/// The running editor.
#[derive(Debug)]
pub struct App {
    buffer: Buffer,
    palette: Palette,
    scroll: usize,
    viewport: Rect,
    message: Option<String>,
    quit_confirmed: bool,
}

impl App {
    /// An editor over `buffer`.
    #[must_use]
    pub fn new(buffer: Buffer, palette: Palette) -> Self {
        Self {
            buffer,
            palette,
            scroll: 0,
            viewport: Rect::new(0, 0, 80, 24),
            message: None,
            quit_confirmed: false,
        }
    }

    /// The buffer. Only the tests read it; `render` and `status` use the field
    /// directly, so this would otherwise be dead weight in the binary.
    #[cfg(test)]
    pub const fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// The first visible line.
    #[cfg(test)]
    pub const fn scroll(&self) -> usize {
        self.scroll
    }

    /// The transient message shown in the status line, if any.
    #[cfg(test)]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Show a message in the status line until the next keypress.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Tell the editor how much room it has.
    pub const fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// How many lines of text fit, leaving a row for the status line.
    const fn text_height(&self) -> usize {
        self.viewport.height.saturating_sub(1) as usize
    }

    /// Handle one event.
    pub fn handle(&mut self, event: Event) -> Outcome {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => {
                self.message = None;
                self.buffer.insert(&text);
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Resize(width, height) => {
                self.viewport = Rect::new(0, 0, width, height);
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Signal(nun_ui::Signal::Suspend) => Outcome::Suspend,
            Event::Signal(nun_ui::Signal::Continue | nun_ui::Signal::Resize) => Outcome::Redraw,
            Event::Signal(nun_ui::Signal::Terminate | nun_ui::Signal::Hangup) | Event::Closed => {
                Outcome::Quit
            }
            Event::Focus(_) => Outcome::Continue,
        }
    }

    #[allow(clippy::too_many_lines)] // A keymap is a flat list; splitting it hides it.
    fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        // With the Kitty protocol negotiated the terminal reports releases and
        // repeats too. Acting on all three would type every character twice.
        if key.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let quitting = matches!(key.code, KeyCode::Char('q' | 'w')) && control;

        // Any key that is not another quit attempt clears the confirmation.
        if !quitting {
            self.quit_confirmed = false;
        }
        self.message = None;

        match (key.code, control) {
            (KeyCode::Char('q' | 'w'), true) => return self.request_quit(),
            (KeyCode::Char('s'), true) => return self.save(),
            (KeyCode::Char('z'), true) if shift => {
                self.buffer.redo();
            }
            (KeyCode::Char('z'), true) => {
                self.buffer.undo();
            }
            (KeyCode::Char('y'), true) => {
                self.buffer.redo();
            }
            (KeyCode::Char('a'), true) => self.buffer.select_all(),

            (KeyCode::Char(ch), false) => {
                let mut text = [0u8; 4];
                self.buffer.insert(ch.encode_utf8(&mut text));
            }
            (KeyCode::Enter, _) => self.buffer.insert("\n"),
            (KeyCode::Tab, _) => self.buffer.insert("\t"),
            (KeyCode::Backspace, _) => self.buffer.delete_backward(),
            (KeyCode::Delete, _) => self.buffer.delete_forward(),

            (KeyCode::Left, _) => self.buffer.move_left(shift),
            (KeyCode::Right, _) => self.buffer.move_right(shift),
            (KeyCode::Up, _) => self.buffer.move_up(shift),
            (KeyCode::Down, _) => self.buffer.move_down(shift),
            (KeyCode::Home, _) => self.buffer.move_line_start(shift),
            (KeyCode::End, _) => self.buffer.move_line_end(shift),
            (KeyCode::PageUp, _) => {
                for _ in 0..self.text_height() {
                    self.buffer.move_up(shift);
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..self.text_height() {
                    self.buffer.move_down(shift);
                }
            }
            (KeyCode::Esc, _) => {
                let mut selections = self.buffer.selections().clone();
                selections.collapse_to_primary();
                let head = selections.primary().head;
                self.buffer.set_selections(Selections::single(Range::caret(head)));
            }
            _ => return Outcome::Continue,
        }

        self.follow_caret();
        Outcome::Redraw
    }

    /// Mouse support here is deliberately minimal — click to place the caret and
    /// the wheel to scroll. The full gesture set is cairn `0015` to `0017`; this
    /// is enough that nothing shipped so far is keyboard-only.
    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> Outcome {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
                Outcome::Redraw
            }
            MouseEventKind::ScrollDown => {
                let last = self.buffer.len_lines().saturating_sub(1);
                self.scroll = (self.scroll + 3).min(last);
                Outcome::Redraw
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.message = None;
                if let Some(position) = self.position_at(mouse.column, mouse.row) {
                    self.buffer.set_selections(Selections::single(Range::caret(position)));
                }
                Outcome::Redraw
            }
            _ => Outcome::Continue,
        }
    }

    /// Which char index a screen cell corresponds to.
    fn position_at(&self, column: u16, row: u16) -> Option<usize> {
        let gutter = EditorView::new(&self.buffer, &self.palette).gutter_width();
        if column < gutter || row as usize >= self.text_height() {
            return None;
        }

        let line = (self.scroll + row as usize).min(self.buffer.len_lines().saturating_sub(1));
        let target = (column - gutter) as usize;
        let text = self.buffer.line_text(line);
        let text = text.strip_suffix('\n').unwrap_or(&text);

        // Walk the line by display width so a click past a wide character lands
        // after it rather than inside it.
        let mut column_offset = 0usize;
        let mut chars = 0usize;
        for cluster in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
            let width = if cluster == "\t" {
                self.buffer.tab_width() - (column_offset % self.buffer.tab_width())
            } else {
                unicode_width::UnicodeWidthStr::width(cluster).max(1)
            };
            if column_offset + width > target {
                break;
            }
            column_offset += width;
            chars += cluster.chars().count();
        }
        Some(self.buffer.line_start(line) + chars)
    }

    /// Scroll the minimum distance needed to keep the caret on screen.
    fn follow_caret(&mut self) {
        let height = self.text_height();
        if height == 0 {
            return;
        }
        let line = self.buffer.line_of(self.buffer.selections().primary().head);
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + height {
            self.scroll = line - height + 1;
        }
    }

    fn request_quit(&mut self) -> Outcome {
        if self.buffer.is_modified() && !self.quit_confirmed {
            self.quit_confirmed = true;
            self.message =
                Some("Unsaved changes. Ctrl+S to save, or Ctrl+Q again to discard.".into());
            return Outcome::Redraw;
        }
        Outcome::Quit
    }

    fn save(&mut self) -> Outcome {
        if self.buffer.is_lossy() {
            self.message = Some(
                "Refusing to save: this file was not valid UTF-8 and would be damaged.".into(),
            );
            return Outcome::Redraw;
        }
        self.message = Some(match self.buffer.save() {
            Ok(()) => format!("Saved {}", display_path(self.buffer.path())),
            Err(SaveError::NoPath) => "No path to save to.".into(),
            Err(SaveError::ChangedOnDisk { path }) => {
                format!(
                    "{} changed on disk. Reopen it to see what changed.",
                    display_path(Some(&path))
                )
            }
            Err(error) => format!("Could not save: {error}"),
        });
        Outcome::Redraw
    }

    /// The status line's text, left and right halves.
    #[must_use]
    pub fn status(&self) -> (String, String) {
        let left = self.message.clone().unwrap_or_else(|| {
            format!(
                "{}{}",
                display_path(self.buffer.path()),
                if self.buffer.is_modified() { " •" } else { "" }
            )
        });

        let caret = self.buffer.selections().primary().head;
        let line = self.buffer.line_of(caret);
        let selections = self.buffer.selections().len();
        let carets = if selections > 1 { format!("{selections} carets  ") } else { String::new() };

        let right = format!(
            "{carets}Ln {}, Col {}  {}",
            line + 1,
            self.buffer.column_of(caret) + 1,
            match self.buffer.line_ending() {
                nun_core::LineEnding::Lf => "LF",
                nun_core::LineEnding::Crlf => "CRLF",
            }
        );
        (left, right)
    }

    /// Draw the editor and its status line.
    pub fn render(&self, area: Rect, cells: &mut Cells) {
        let text_area = Rect { height: area.height.saturating_sub(1), ..area };
        EditorView::new(&self.buffer, &self.palette)
            .scrolled_to(self.scroll)
            .render(text_area, cells);

        if area.height == 0 {
            return;
        }
        self.render_status(Rect { y: area.bottom() - 1, height: 1, ..area }, cells);
    }

    fn render_status(&self, area: Rect, cells: &mut Cells) {
        let style = if self.message.is_some() {
            self.palette.on(Role::Accent, Role::OnAccent)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };

        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(style);
        }

        let (left, right) = self.status();
        write_at(cells, area, area.left(), &left, style);

        let width = u16::try_from(right.chars().count()).unwrap_or(0);
        // Dropped entirely rather than overlapping when the two halves would
        // collide on a narrow terminal.
        if let Some(start) = area.right().checked_sub(width + 1)
            && start > area.left() + u16::try_from(left.chars().count()).unwrap_or(0)
        {
            write_at(cells, area, start, &right, style);
        }
    }
}

fn write_at(cells: &mut Cells, area: Rect, start: u16, text: &str, style: ratatui::style::Style) {
    for (offset, ch) in text.chars().enumerate() {
        let Ok(offset) = u16::try_from(offset) else { break };
        let Some(x) = start.checked_add(offset) else { break };
        if x >= area.right() {
            break;
        }
        cells[(x, area.y)].set_char(ch).set_style(style);
    }
}

fn display_path(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || "[no name]".to_string(),
        |path| {
            // Show it relative to where nun was started, which is almost always
            // shorter and is what the user typed.
            std::env::current_dir()
                .ok()
                .and_then(|cwd| path.strip_prefix(cwd).ok())
                .unwrap_or(path)
                .display()
                .to_string()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nun_theme::{Probe, derive};
    use ratatui::style::Color;

    fn app_over(text: &str) -> App {
        let mut app =
            App::new(Buffer::from_text(text), Palette::new(derive(&Probe::builtin_dark())));
        app.set_viewport(Rect::new(0, 0, 40, 6));
        app
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn chord(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn text_of(app: &App) -> String {
        app.buffer().text().to_string()
    }

    #[test]
    fn typing_inserts_and_the_arrows_move() {
        let mut app = app_over("");
        for ch in "hello".chars() {
            app.handle(key(KeyCode::Char(ch)));
        }
        assert_eq!(text_of(&app), "hello");

        app.handle(key(KeyCode::Left));
        app.handle(key(KeyCode::Char('X')));
        assert_eq!(text_of(&app), "hellXo");
    }

    #[test]
    fn enter_and_backspace_do_the_obvious_thing() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Enter));
        app.handle(key(KeyCode::Char('b')));
        assert_eq!(text_of(&app), "a\nb");

        app.handle(key(KeyCode::Backspace));
        app.handle(key(KeyCode::Backspace));
        assert_eq!(text_of(&app), "a");
    }

    #[test]
    fn a_key_release_is_ignored_rather_than_typed_twice() {
        // With the Kitty protocol negotiated the terminal reports releases too.
        let mut app = app_over("");
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;

        app.handle(key(KeyCode::Char('x')));
        app.handle(Event::Key(release));

        assert_eq!(text_of(&app), "x", "a press and its release must type one character");
    }

    #[test]
    fn shift_and_an_arrow_extend_the_selection() {
        let mut app = app_over("hello");
        app.handle(chord(KeyCode::Right, KeyModifiers::SHIFT));
        app.handle(chord(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(app.buffer().selections().primary().len(), 2);
    }

    #[test]
    fn undo_and_redo_are_bound_the_way_people_expect() {
        let mut app = app_over("");
        for ch in "abc".chars() {
            app.handle(key(KeyCode::Char(ch)));
        }
        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "");

        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        assert_eq!(text_of(&app), "abc");

        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        app.handle(chord(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "abc", "Ctrl+Y redoes too");
    }

    #[test]
    fn select_all_then_type_replaces_everything() {
        let mut app = app_over("old content");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Char('n')));
        assert_eq!(text_of(&app), "n");
    }

    #[test]
    fn escape_collapses_a_selection_to_a_caret() {
        let mut app = app_over("hello");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Esc));
        assert!(app.buffer().selections().primary().is_empty());
    }

    #[test]
    fn a_paste_arrives_as_one_undoable_unit() {
        let mut app = app_over("");
        app.handle(Event::Paste("pasted text".into()));
        assert_eq!(text_of(&app), "pasted text");
        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "", "a paste undoes in one step, not per character");
    }

    // ── quitting ────────────────────────────────────────────────────────────

    #[test]
    fn quitting_a_clean_buffer_is_immediate() {
        let mut app = app_over("untouched");
        assert_eq!(app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)), Outcome::Quit);
    }

    #[test]
    fn quitting_with_unsaved_changes_asks_first() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('x')));

        assert_eq!(app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)), Outcome::Redraw);
        assert!(app.message().unwrap().contains("Unsaved changes"));

        assert_eq!(
            app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Outcome::Quit,
            "asking twice means you meant it"
        );
    }

    #[test]
    fn typing_after_a_quit_prompt_cancels_it() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('x')));
        app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Char('y')));

        assert_eq!(
            app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Outcome::Redraw,
            "the confirmation must not survive an intervening edit"
        );
    }

    #[test]
    fn a_lossy_file_refuses_to_save_rather_than_destroying_it() {
        let (buffer, _) = Buffer::from_bytes(&[0x68, 0xff]);
        let mut app = App::new(buffer, Palette::new(derive(&Probe::builtin_dark())));
        app.set_viewport(Rect::new(0, 0, 40, 6));

        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message().unwrap().contains("Refusing to save"));
    }

    #[test]
    fn saving_a_buffer_with_no_path_says_so() {
        let mut app = app_over("x");
        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(app.message(), Some("No path to save to."));
    }

    // ── scrolling ───────────────────────────────────────────────────────────

    #[test]
    fn the_view_follows_the_caret_down_and_back_up() {
        let mut app = app_over(&"line\n".repeat(40));
        assert_eq!(app.scroll(), 0);

        for _ in 0..10 {
            app.handle(key(KeyCode::Down));
        }
        assert_eq!(app.scroll(), 6, "scrolled the minimum needed to keep the caret visible");

        for _ in 0..10 {
            app.handle(key(KeyCode::Up));
        }
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn page_down_moves_by_the_visible_height() {
        let mut app = app_over(&"line\n".repeat(40));
        app.handle(key(KeyCode::PageDown));
        let line = app.buffer().line_of(app.buffer().selections().primary().head);
        assert_eq!(line, 5, "one screenful, less the status row");
    }

    #[test]
    fn a_resize_keeps_the_caret_on_screen() {
        let mut app = app_over(&"line\n".repeat(40));
        for _ in 0..20 {
            app.handle(key(KeyCode::Down));
        }
        app.handle(Event::Resize(40, 4));
        let line = app.buffer().line_of(app.buffer().selections().primary().head);
        assert!(line >= app.scroll() && line < app.scroll() + 3, "caret left the viewport");
    }

    // ── mouse ───────────────────────────────────────────────────────────────

    fn click(column: u16, row: u16) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn wheel(down: bool) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind: if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp },
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn clicking_places_the_caret() {
        let mut app = app_over("hello\nworld\n");
        // Gutter is one digit plus two columns of padding.
        app.handle(click(3 + 2, 1));
        assert_eq!(app.buffer().selections().primary().head, 8, "row 1, two chars in");
    }

    #[test]
    fn clicking_past_a_wide_character_lands_after_it_not_inside_it() {
        let mut app = app_over("日本語\n");
        app.handle(click(3 + 3, 0));
        assert_eq!(app.buffer().selections().primary().head, 1, "after the first wide cluster");
    }

    #[test]
    fn clicking_in_the_gutter_does_not_move_the_caret() {
        let mut app = app_over("hello\n");
        app.handle(key(KeyCode::Right));
        let before = app.buffer().selections().primary().head;
        app.handle(click(0, 0));
        assert_eq!(app.buffer().selections().primary().head, before);
    }

    #[test]
    fn the_wheel_scrolls_without_moving_the_caret() {
        let mut app = app_over(&"line\n".repeat(40));
        let caret = app.buffer().selections().primary().head;

        app.handle(wheel(true));
        assert_eq!(app.scroll(), 3);
        assert_eq!(app.buffer().selections().primary().head, caret, "scrolling is not navigation");

        app.handle(wheel(false));
        assert_eq!(app.scroll(), 0);
    }

    // ── status line ─────────────────────────────────────────────────────────

    #[test]
    fn the_status_line_shows_unsaved_state_and_position() {
        let mut app = app_over("hello\nworld");
        let (left, right) = app.status();
        assert_eq!(left, "[no name]");
        assert!(right.contains("Ln 1, Col 1"));

        app.handle(key(KeyCode::Char('x')));
        let (left, right) = app.status();
        assert!(left.ends_with(" •"), "an unsaved buffer is marked: {left}");
        assert!(right.contains("Ln 1, Col 2"));
    }

    #[test]
    fn the_status_line_counts_multiple_carets() {
        let mut buffer = Buffer::from_text("a\nb\nc");
        buffer.set_selections(Selections::new(
            vec![Range::caret(0), Range::caret(2), Range::caret(4)],
            0,
        ));
        let app = App::new(buffer, Palette::new(derive(&Probe::builtin_dark())));
        assert!(app.status().1.contains("3 carets"));
    }

    #[test]
    fn the_status_line_reports_crlf_when_that_is_what_will_be_written() {
        let (buffer, _) = Buffer::from_bytes(b"one\r\ntwo\r\n");
        let app = App::new(buffer, Palette::new(derive(&Probe::builtin_dark())));
        assert!(app.status().1.contains("CRLF"));
    }

    #[test]
    fn a_message_takes_over_the_status_line_and_is_cleared_by_the_next_key() {
        let mut app = app_over("x");
        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message().is_some());
        app.handle(key(KeyCode::Right));
        assert!(app.message().is_none());
    }

    // ── rendering ───────────────────────────────────────────────────────────

    #[test]
    fn the_editor_and_its_status_line_both_get_drawn() {
        let app = app_over("hello\nworld\n");
        let mut harness = nun_ui::Harness::new(30, 4);
        harness.draw(TestView(&app));

        let text = harness.to_text();
        assert!(text.starts_with("1  hello\n2  world"), "{text}");
        assert!(text.contains("[no name]"), "the status line is missing: {text}");
    }

    #[test]
    fn the_status_line_is_painted_across_its_whole_width() {
        let app = app_over("hi");
        let mut harness = nun_ui::Harness::new(30, 3);
        harness.draw(TestView(&app));

        let cells = harness.cells();
        for x in 0..30 {
            assert_ne!(cells[(x, 2)].bg, Color::Reset, "status cell {x} was left unpainted");
        }
    }

    #[test]
    fn a_viewport_one_row_tall_still_renders_the_status_line() {
        let app = app_over("hello");
        let mut harness = nun_ui::Harness::new(20, 1);
        harness.draw(TestView(&app));
        assert!(harness.to_text().contains("[no name]"));
    }

    struct TestView<'a>(&'a App);

    impl Widget for TestView<'_> {
        fn render(self, area: Rect, cells: &mut Cells) {
            self.0.render(area, cells);
        }
    }
}
