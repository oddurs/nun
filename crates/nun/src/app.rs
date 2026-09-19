//! The editor: state, one event at a time, and what to draw.
//!
//! Deliberately free of terminal I/O. `App` takes an event and mutates itself;
//! `main` does the reading and the drawing. That keeps the whole of the editor's
//! behaviour testable without a tty, which is how the keymap and the scrolling
//! are checked below.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nun_core::{Buffer, Range, SaveError, Selections};
use nun_input::{
    Chords, Clicks, Code, HitMap, Hover, Key, Keymap, Mods, PLATFORM_THRESHOLD, Resolved, Sequence,
};
use nun_theme::Role;
use nun_ui::{EditorView, Event, Palette};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::commands::Command;

mod pointer;

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

impl Outcome {
    /// The stronger of two outcomes, for when several events are handled
    /// before one frame: a quit anywhere in a burst wins, then a suspend, then
    /// a redraw.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Quit, _) | (_, Self::Quit) => Self::Quit,
            (Self::Suspend, _) | (_, Self::Suspend) => Self::Suspend,
            (Self::Redraw, _) | (_, Self::Redraw) => Self::Redraw,
            _ => Self::Continue,
        }
    }
}

/// How long an unfinished chord waits for its next key.
const CHORD_TIMEOUT: Duration = Duration::from_millis(1000);

/// How long the pointer rests on a hover target before its dwell fires.
const HOVER_DWELL: Duration = Duration::from_millis(500);

/// Everything on screen a pointer can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The text of the buffer.
    Text,
    /// The line-number gutter beside it.
    Gutter,
    /// The status line.
    Status,
}

/// The running editor.
#[derive(Debug)]
pub struct App {
    buffer: Buffer,
    palette: Palette,
    scroll: usize,
    viewport: Rect,
    /// A transient message, shown until the next key.
    message: Option<String>,
    /// Things worth saying once, shown one at a time after `message`.
    notices: VecDeque<String>,
    quit_confirmed: bool,
    keymap: Keymap<Command>,
    chords: Chords,
    /// What is where, as of the last layout.
    hits: HitMap<Target>,
    /// Which hover target the pointer is over.
    hover: Hover<Target>,
    /// Presses, counted into single, double and triple clicks.
    clicks: Clicks,
    /// A drag in progress.
    drag: Option<pointer::Drag>,
    /// Scrolling under a drag held past the edge of the text.
    autoscroll: Option<pointer::Autoscroll>,
}

impl App {
    /// An editor over `buffer`, driven by `keymap`.
    #[must_use]
    pub fn new(buffer: Buffer, palette: Palette, keymap: Keymap<Command>) -> Self {
        let viewport = Rect::new(0, 0, 80, 24);
        let mut app = Self {
            buffer,
            palette,
            scroll: 0,
            viewport,
            message: None,
            notices: VecDeque::new(),
            quit_confirmed: false,
            keymap,
            chords: Chords::new(CHORD_TIMEOUT),
            hits: HitMap::new(cells(viewport)),
            hover: Hover::new(HOVER_DWELL),
            clicks: Clicks::new(PLATFORM_THRESHOLD),
            drag: None,
            autoscroll: None,
        };
        app.relayout();
        app
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
        self.shown_message()
    }

    /// Use `threshold` as the longest gap that still joins presses into a
    /// double or triple click.
    pub fn set_double_click(&mut self, threshold: Duration) {
        self.clicks = Clicks::new(threshold);
    }

    /// Columns the line-number gutter takes.
    fn gutter_width(&self) -> u16 {
        EditorView::new(&self.buffer, &self.palette).gutter_width()
    }

    /// Say something once in the status line.
    ///
    /// Notices queue: each stays until a key is pressed, then the next one
    /// shows. The status line holds one message, and a notice that is
    /// immediately replaced by another was never really said.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.notices.push_back(message.into());
    }

    /// The message the status line is showing, if any.
    fn shown_message(&self) -> Option<&str> {
        self.message.as_deref().or_else(|| self.notices.front().map(String::as_str))
    }

    /// A key was pressed: whatever message was showing has been seen.
    fn acknowledge(&mut self) {
        if self.message.take().is_none() {
            self.notices.pop_front();
        }
    }

    /// Tell the editor how much room it has.
    pub fn set_viewport(&mut self, viewport: Rect) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.relayout();
        }
    }

    /// Where the text and the status line go.
    fn areas(&self) -> (Rect, Rect) {
        let area = self.viewport;
        let text = Rect { height: area.height.saturating_sub(1), ..area };
        let status =
            Rect { y: area.bottom().saturating_sub(1), height: area.height.min(1), ..area };
        (text, status)
    }

    /// Record what is where, for resolving the pointer.
    ///
    /// Regions are pushed in paint order, so anything drawn on top — an overlay,
    /// a menu — is pushed later and captures the pointer above what is beneath.
    fn relayout(&mut self) {
        let (text, status) = self.areas();
        let gutter = EditorView::new(&self.buffer, &self.palette).gutter_width().min(text.width);

        let mut hits = HitMap::new(cells(self.viewport));
        hits.push(cells(Rect { width: gutter, ..text }), Target::Gutter, false);
        hits.push(
            cells(Rect { x: text.x + gutter, width: text.width - gutter, ..text }),
            Target::Text,
            false,
        );
        hits.push(cells(status), Target::Status, false);
        self.hits = hits;
    }

    /// Whether anything on screen reacts to the pointer merely passing over it.
    ///
    /// The terminal's any-motion reporting is turned on only while this holds.
    pub const fn wants_motion(&self) -> bool {
        self.hits.has_hover_targets()
    }

    /// When the editor next needs waking with no input, if ever.
    pub fn deadline(&self) -> Option<Instant> {
        let autoscroll = self.autoscroll.map(|scroll| scroll.next);
        [self.hover.deadline(), self.chords.deadline(), autoscroll].into_iter().flatten().min()
    }

    /// A deadline passed with no input.
    pub fn tick(&mut self, now: Instant) -> Outcome {
        let chord = match self.chords.expire(&self.keymap, now) {
            Some(Resolved::Unbound(keys)) => {
                self.message = Some(format!("{} is not bound to anything", Sequence(&keys)));
                Outcome::Redraw
            }
            Some(resolved) => self.resolved(resolved, None),
            None => Outcome::Continue,
        };

        // Nothing reacts to a dwell yet; the first hover card is milestone 4.
        // Taking it keeps the deadline from firing again.
        let dwell = match self.hover.dwell(now) {
            Some(_) => Outcome::Redraw,
            None => Outcome::Continue,
        };

        let outcome = chord.and(dwell).and(self.autoscroll_tick(now));
        if outcome == Outcome::Redraw {
            self.relayout();
        }
        outcome
    }

    /// How many lines of text fit, leaving a row for the status line.
    const fn text_height(&self) -> usize {
        self.viewport.height.saturating_sub(1) as usize
    }

    /// Handle one event.
    pub fn handle(&mut self, event: Event) -> Outcome {
        self.handle_at(event, Instant::now())
    }

    /// Handle one event that arrived at `now`.
    pub fn handle_at(&mut self, event: Event, now: Instant) -> Outcome {
        let outcome = self.dispatch(event, now);
        if outcome == Outcome::Redraw {
            // The gutter widens as lines are added, and later milestones lay out
            // far more than this; anything that redraws may have moved things.
            self.relayout();
        }
        outcome
    }

    fn dispatch(&mut self, event: Event, now: Instant) -> Outcome {
        match event {
            Event::Key(key) => self.handle_key(key, now),
            Event::Mouse(mouse) => self.handle_mouse(mouse, now),
            Event::Paste(text) => {
                // A paste is not the second half of a chord.
                self.chords.cancel();
                self.acknowledge();
                self.buffer.insert(&text);
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Resize(width, height) => {
                self.set_viewport(Rect::new(0, 0, width, height));
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Signal(nun_ui::Signal::Suspend) => Outcome::Suspend,
            Event::Signal(nun_ui::Signal::Continue | nun_ui::Signal::Resize) => Outcome::Redraw,
            Event::Signal(nun_ui::Signal::Terminate | nun_ui::Signal::Hangup) | Event::Closed => {
                Outcome::Quit
            }
            Event::Focus(true) => Outcome::Continue,
            // The pointer may be anywhere by the time focus comes back.
            Event::Focus(false) => {
                self.end_drag();
                if self.hover.clear(now).is_none() { Outcome::Continue } else { Outcome::Redraw }
            }
        }
    }

    fn handle_key(&mut self, event: KeyEvent, now: Instant) -> Outcome {
        // With the Kitty protocol negotiated the terminal reports releases and
        // repeats too. Acting on all three would type every character twice.
        if event.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }
        self.end_drag();
        let Some(key) = to_key(&event) else { return Outcome::Continue };

        let mut outcome = Outcome::Continue;
        for resolved in self.chords.feed(&self.keymap, key, now) {
            outcome = outcome.and(self.resolved(resolved, Some(&event)));
        }
        outcome
    }

    /// Act on what a keystroke, or a chord timing out, resolved to.
    ///
    /// `event` is the keystroke itself, when there is one: an unbound single
    /// key is always the one just pressed, and it is typed from the event as
    /// the terminal reported it, so layouts, Caps Lock and Option-composed
    /// characters come through untouched.
    fn resolved(&mut self, resolved: Resolved<Command>, event: Option<&KeyEvent>) -> Outcome {
        match resolved {
            Resolved::Command(command) => self.run(command),
            // The status line shows the chord so far.
            Resolved::Pending => Outcome::Redraw,
            Resolved::Unbound(keys) if keys.len() == 1 => match event {
                Some(event) => self.edit(event),
                None => Outcome::Continue,
            },
            Resolved::Unbound(keys) => {
                self.acknowledge();
                self.message = Some(format!("{} is not bound to anything", Sequence(&keys)));
                Outcome::Redraw
            }
        }
    }

    /// Run a command.
    pub fn run(&mut self, command: Command) -> Outcome {
        // Anything but a second quit clears a pending quit confirmation.
        if command != Command::Quit {
            self.quit_confirmed = false;
        }
        self.acknowledge();

        match command {
            Command::Quit => return self.request_quit(),
            Command::Save => return self.save(),
            Command::Undo => {
                self.buffer.undo();
            }
            Command::Redo => {
                self.buffer.redo();
            }
            Command::SelectAll => self.buffer.select_all(),
        }
        self.follow_caret();
        Outcome::Redraw
    }

    /// An editing key: typing, moving, deleting. Not rebindable.
    fn edit(&mut self, key: &KeyEvent) -> Outcome {
        self.quit_confirmed = false;
        self.acknowledge();

        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char(ch) if !control => {
                let mut text = [0u8; 4];
                self.buffer.insert(ch.encode_utf8(&mut text));
            }
            KeyCode::Enter => self.buffer.insert("\n"),
            KeyCode::Tab => self.buffer.insert("\t"),
            KeyCode::Backspace => self.buffer.delete_backward(),
            KeyCode::Delete => self.buffer.delete_forward(),

            KeyCode::Left => self.buffer.move_left(shift),
            KeyCode::Right => self.buffer.move_right(shift),
            KeyCode::Up => self.buffer.move_up(shift),
            KeyCode::Down => self.buffer.move_down(shift),
            KeyCode::Home => self.buffer.move_line_start(shift),
            KeyCode::End => self.buffer.move_line_end(shift),
            KeyCode::PageUp => {
                for _ in 0..self.text_height() {
                    self.buffer.move_up(shift);
                }
            }
            KeyCode::PageDown => {
                for _ in 0..self.text_height() {
                    self.buffer.move_down(shift);
                }
            }
            KeyCode::Esc => {
                let head = self.buffer.selections().primary().head;
                self.buffer.set_selections(Selections::single(Range::caret(head)));
            }
            // An unbound Ctrl chord, or a key nun does not use. The message it
            // may have cleared still needs a redraw.
            _ => return Outcome::Redraw,
        }

        self.follow_caret();
        Outcome::Redraw
    }

    /// Mouse support here is deliberately minimal — click to place the caret and
    /// the wheel to scroll. The full gesture set is cairn `0015` to `0017`; this
    /// is enough that nothing shipped so far is keyboard-only.
    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent, now: Instant) -> Outcome {
        let hit = self.hits.at(mouse.column, mouse.row);

        // Hover follows every report, whatever else the event does, so leaving
        // a target by dragging out of it is still a leave.
        let crossing = self.hover.update(hit.filter(|hit| hit.hover).map(|hit| hit.target), now);
        let hovered = if crossing.is_none() { Outcome::Continue } else { Outcome::Redraw };

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
                // Reaching for the mouse abandons a half-typed chord, so the
                // next key is typed rather than taken as its second half.
                self.chords.cancel();
                self.acknowledge();
                self.quit_confirmed = false;
                match hit {
                    Some(hit) => self.press(mouse, hit.target, now),
                    None => Outcome::Redraw,
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drag_to(mouse.column, mouse.row, now).and(hovered)
            }
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse).and(hovered),
            _ => hovered,
        }
    }

    /// Which char index a screen cell in the text region corresponds to.
    fn position_at(&self, column: u16, row: u16) -> Option<usize> {
        // The view measures from the left edge of the gutter, not the text.
        let (area, _) = self.areas();
        EditorView::new(&self.buffer, &self.palette)
            .scrolled_to(self.scroll)
            .position_at(area, column, row)
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
            let save = self.binding_for(Command::Save);
            let quit = self.binding_for(Command::Quit);
            self.message =
                Some(format!("Unsaved changes. {save} to save, or {quit} again to discard."));
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

    /// How to ask for `command` from the keyboard, as the status line says it.
    fn binding_for(&self, command: Command) -> String {
        self.keymap
            .sequences_for(&command)
            .first()
            .map_or_else(|| command.title().to_string(), |keys| Sequence(keys).to_string())
    }

    /// The status line's text, left and right halves.
    #[must_use]
    pub fn status(&self) -> (String, String) {
        let pending = self.chords.pending();
        let chord = (!pending.is_empty()).then(|| format!("{} …", Sequence(pending)));
        let left =
            chord.or_else(|| self.shown_message().map(str::to_string)).unwrap_or_else(|| {
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
            .with_drop_marker(self.drop_marker())
            .render(text_area, cells);

        if area.height == 0 {
            return;
        }
        self.render_status(Rect { y: area.bottom() - 1, height: 1, ..area }, cells);
    }

    fn render_status(&self, area: Rect, cells: &mut Cells) {
        let style = if self.shown_message().is_some() || !self.chords.pending().is_empty() {
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

/// A terminal keystroke as a key the keymap understands.
///
/// `None` for keys nun has no use for: media keys, lone modifiers, Caps Lock.
fn to_key(event: &KeyEvent) -> Option<Key> {
    let code = match event.code {
        KeyCode::Char(ch) => Code::Char(ch),
        KeyCode::F(n) => Code::F(n),
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab | KeyCode::BackTab => Code::Tab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Insert => Code::Insert,
        KeyCode::Esc => Code::Esc,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        _ => return None,
    };

    let mut mods = Mods::NONE;
    for (flag, modifier) in [
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::ALT, Mods::ALT),
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::SUPER, Mods::CMD),
    ] {
        if event.modifiers.contains(flag) {
            mods |= modifier;
        }
    }
    if event.code == KeyCode::BackTab {
        mods |= Mods::SHIFT;
    }
    Some(Key::new(code, mods))
}

/// ratatui's rectangle as nun-input's.
const fn cells(area: Rect) -> nun_input::Rect {
    nun_input::Rect::new(area.x, area.y, area.width, area.height)
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

    fn app_with(buffer: Buffer) -> App {
        App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        )
    }

    fn app_over(text: &str) -> App {
        let mut app = app_with(Buffer::from_text(text));
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

    fn app_with_bindings(text: &str, user: &[(&str, &str)]) -> App {
        let user = user.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        let (keymap, problems) = crate::commands::keymap(crate::commands::KeySet::Full, &user);
        assert!(problems.is_empty(), "{problems:?}");
        let mut app =
            App::new(Buffer::from_text(text), Palette::new(derive(&Probe::builtin_dark())), keymap);
        app.set_viewport(Rect::new(0, 0, 40, 6));
        app
    }

    fn ctrl(ch: char) -> Event {
        chord(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_chord_shows_itself_while_it_waits_and_runs_when_finished() {
        let mut app = app_with_bindings("abc", &[("ctrl+k ctrl+a", "edit.select_all")]);
        let now = Instant::now();

        assert_eq!(app.handle_at(ctrl('k'), now), Outcome::Redraw);
        assert!(app.status().0.starts_with("Ctrl+K"), "{:?}", app.status());
        assert!(app.deadline().is_some(), "waiting for the second key");

        app.handle_at(ctrl('a'), now);
        assert_eq!(app.buffer().selections().primary().len(), 3);
        assert_eq!(app.deadline(), None);
        assert!(!app.status().0.starts_with("Ctrl+K"));
    }

    #[test]
    fn a_chord_nobody_finishes_times_out_and_says_so() {
        let mut app = app_with_bindings("abc", &[("ctrl+k ctrl+a", "edit.select_all")]);
        let now = Instant::now();
        app.handle_at(ctrl('k'), now);

        let deadline = app.deadline().unwrap();
        assert_eq!(app.tick(deadline), Outcome::Redraw);
        assert_eq!(app.message(), Some("Ctrl+K is not bound to anything"));
        assert_eq!(app.deadline(), None);
        assert!(app.buffer().selections().primary().is_empty(), "nothing ran");
    }

    #[test]
    fn a_chord_times_out_to_its_prefix_binding() {
        let mut app = app_with_bindings(
            "abc",
            &[("ctrl+k", "edit.select_all"), ("ctrl+k ctrl+z", "edit.undo")],
        );
        let now = Instant::now();
        app.handle_at(ctrl('k'), now);
        let deadline = app.deadline().unwrap();
        app.tick(deadline);
        assert_eq!(app.buffer().selections().primary().len(), 3, "the prefix binding ran");
    }

    #[test]
    fn typing_after_a_chord_prefix_does_not_type() {
        let mut app = app_with_bindings("", &[("ctrl+k ctrl+a", "edit.select_all")]);
        app.handle(ctrl('k'));
        app.handle(key(KeyCode::Char('x')));
        assert_eq!(text_of(&app), "", "the x finished (and broke) the chord");
        assert_eq!(app.message(), Some("Ctrl+K X is not bound to anything"));
    }

    #[test]
    fn a_click_abandons_a_half_typed_chord() {
        let mut app = app_with_bindings("", &[("ctrl+k ctrl+a", "edit.select_all")]);
        app.handle(ctrl('k'));
        app.handle(click(3, 0));
        app.handle(key(KeyCode::Char('x')));
        assert_eq!(text_of(&app), "x", "typed, not taken as the chord's second key");
        assert_eq!(app.deadline(), None);
    }

    #[test]
    fn a_rebound_key_does_the_new_thing() {
        let mut app = app_with_bindings("abc", &[("ctrl+s", "edit.select_all")]);
        app.handle(ctrl('s'));
        assert_eq!(app.buffer().selections().primary().len(), 3);
    }

    #[test]
    fn cmd_bindings_work_when_the_terminal_reports_cmd() {
        let mut app = app_over("abc");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(app.buffer().selections().primary().len(), 3);
    }

    #[test]
    fn notices_are_shown_one_at_a_time() {
        let mut app = app_over("");
        app.warn("first");
        app.warn("second");
        assert_eq!(app.message(), Some("first"));
        app.handle(key(KeyCode::Right));
        assert_eq!(app.message(), Some("second"));
        app.handle(key(KeyCode::Right));
        assert_eq!(app.message(), None);
    }

    #[test]
    fn the_quit_prompt_names_the_keys_actually_bound() {
        let mut app = app_with_bindings("", &[]);
        app.handle(key(KeyCode::Char('x')));
        app.handle(ctrl('q'));
        let message = app.message().unwrap();
        assert!(message.contains("Ctrl+S to save"), "{message}");
        assert!(message.contains("Ctrl+Q again"), "{message}");
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
        let mut app = app_with(buffer);
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
    fn a_click_past_column_223_lands_where_it_was_made() {
        // The legacy mouse encoding tops out at column 223. SGR does not, and
        // nothing between the terminal and the buffer may narrow it again.
        let mut app = app_with(Buffer::from_text(&"x".repeat(400)));
        app.set_viewport(Rect::new(0, 0, 420, 6));

        app.handle(click(3 + 300, 0));
        assert_eq!(app.buffer().selections().primary().head, 300);
    }

    #[test]
    fn a_click_below_row_223_lands_where_it_was_made() {
        let mut app = app_with(Buffer::from_text(&"line\n".repeat(400)));
        app.set_viewport(Rect::new(0, 0, 40, 302));

        // Three digits of line number plus two columns of padding.
        app.handle(click(5 + 2, 250));
        let head = app.buffer().selections().primary().head;
        assert_eq!(app.buffer().line_of(head), 250);
    }

    #[test]
    fn pointer_motion_on_its_own_costs_no_frame() {
        // With hover tracking on, motion arrives for every cell the pointer
        // crosses. Until something reacts to it, none of it may redraw.
        let mut app = app_over("hello\n");
        let motion = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.handle(motion), Outcome::Continue);
    }

    #[test]
    fn clicking_the_status_line_does_not_reach_the_text_beneath_it() {
        // Viewport is six rows: five of text and the status line on row 5. The
        // buffer is long enough that row 5 would otherwise be a line of text.
        let mut app = app_over(&"line\n".repeat(20));
        app.handle(click(5, 5));
        assert_eq!(app.buffer().selections().primary().head, 0);
    }

    #[test]
    fn a_resize_moves_the_status_line_and_the_clicks_follow_it() {
        let mut app = app_over(&"line\n".repeat(20));
        app.handle(Event::Resize(40, 10));
        app.handle(click(3 + 1, 5));
        assert_eq!(app.buffer().line_of(app.buffer().selections().primary().head), 5);
    }

    #[test]
    fn an_idle_editor_has_nothing_to_wake_up_for() {
        let app = app_over("hello");
        assert_eq!(app.deadline(), None);
        assert!(!app.wants_motion(), "nothing on screen reacts to hover yet");
    }

    #[test]
    fn clicking_in_the_gutter_selects_the_line() {
        let mut app = app_over("hello\nworld\n");
        app.handle(click(0, 0));
        let primary = app.buffer().selections().primary();
        assert_eq!((primary.from(), primary.to()), (0, 6), "the line and its newline");
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
        let app = app_with(buffer);
        assert!(app.status().1.contains("3 carets"));
    }

    #[test]
    fn the_status_line_reports_crlf_when_that_is_what_will_be_written() {
        let (buffer, _) = Buffer::from_bytes(b"one\r\ntwo\r\n");
        let app = app_with(buffer);
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
