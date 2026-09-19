//! Clicking and dragging in the text.
//!
//! This is the premise of the editor, so it follows the conventions people
//! already have in their hands rather than inventing any:
//!
//! * A click places the caret; Shift-click extends from the anchor.
//! * A double-click selects a word and a triple-click a line; dragging after
//!   either extends by that unit, not by character.
//! * Alt-click adds a caret; Alt-drag makes a column selection, replacing
//!   whatever was selected.
//! * Dragging an existing selection moves the text; Ctrl held at the drop
//!   copies it instead.
//! * A drag that reaches the edge of the text scrolls, faster the further past
//!   the edge the pointer is, and stops at the ends of the buffer.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseEvent};
use nun_core::{Range, Selections};

use super::{App, Outcome, Target};

/// What a drag in progress is extending.
#[derive(Debug, Clone)]
pub(super) enum Mode {
    /// Character by character from a fixed anchor.
    Char { anchor: usize },
    /// Word by word, never shrinking below the word first clicked.
    Word { from: usize, to: usize },
    /// Line by line, never shrinking below the line first clicked.
    Line { from: usize, to: usize },
    /// Alt held: a caret added to `keep`, the selections already there, until
    /// the pointer moves; then a box from the `(line, column)` it began at.
    Column { anchor: (usize, usize), keep: Vec<Range> },
    /// Carrying the selected text `from..to`. `drop` is where it would land,
    /// once the pointer has actually moved off the cell it was pressed on.
    Move { from: usize, to: usize, press: (u16, u16), at: usize, drop: Option<usize> },
}

/// A drag in progress.
#[derive(Debug, Clone)]
pub(super) struct Drag {
    pub(super) mode: Mode,
    /// Where the pointer last was, for re-applying the drag as the view
    /// scrolls underneath a pointer that is standing still.
    pointer: (u16, u16),
}

/// Scrolling while a drag holds the pointer past the edge of the text.
#[derive(Debug, Clone, Copy)]
pub(super) struct Autoscroll {
    up: bool,
    interval: Duration,
    pub(super) next: Instant,
}

/// Slowest autoscroll, one line per this, with the pointer one row past.
const AUTOSCROLL_BASE: Duration = Duration::from_millis(100);
/// Fastest, however far past the edge the pointer goes.
const AUTOSCROLL_FLOOR: Duration = Duration::from_millis(16);

impl App {
    /// The left button went down on `target`.
    pub(super) fn press(&mut self, mouse: MouseEvent, target: Target, now: Instant) -> Outcome {
        let (column, row) = (mouse.column, mouse.row);
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let alt = mouse.modifiers.contains(KeyModifiers::ALT);
        let count = self.clicks.press(column, row, now);
        let at = self.pointer_position(column, row);
        let primary = self.buffer.selections().primary();

        let mode = match target {
            Target::Gutter => {
                let line = self.buffer.line_of(at);
                let (from, to) = self.buffer.line_range(line);
                if shift {
                    // Extend by whole lines from the line the anchor is on.
                    let (anchor_from, anchor_to) =
                        self.buffer.line_range(self.buffer.line_of(primary.anchor));
                    Mode::Line { from: anchor_from, to: anchor_to }
                } else {
                    Mode::Line { from, to }
                }
            }
            // The selections already there stay; the drag adds to them.
            Target::Text if alt => Mode::Column {
                anchor: self.pointer_column(column, row),
                keep: self.buffer.selections().ranges().to_vec(),
            },
            Target::Text if shift => Mode::Char { anchor: primary.anchor },
            Target::Text
                if count == 1
                    && !primary.is_empty()
                    && at >= primary.from()
                    && at < primary.to() =>
            {
                Mode::Move {
                    from: primary.from(),
                    to: primary.to(),
                    press: (column, row),
                    at,
                    drop: None,
                }
            }
            Target::Text => match count {
                2 => {
                    let (from, to) = self.buffer.word_range(at);
                    Mode::Word { from, to }
                }
                3 => {
                    let (from, to) = self.buffer.line_range(self.buffer.line_of(at));
                    Mode::Line { from, to }
                }
                _ => Mode::Char { anchor: at },
            },
            // Everything else on screen handles its own press.
            _ => return Outcome::Continue,
        };

        self.drag = Some(Drag { mode, pointer: (column, row) });
        self.drag_to(column, row, now);
        Outcome::Redraw
    }

    /// The pointer moved with the button held.
    pub(super) fn drag_to(&mut self, column: u16, row: u16, now: Instant) -> Outcome {
        let Some(mut drag) = self.drag.take() else { return Outcome::Continue };
        drag.pointer = (column, row);
        let at = self.pointer_position(column, row);

        match &mut drag.mode {
            Mode::Char { anchor } => {
                self.buffer.set_selections(Selections::single(Range::new(*anchor, at)));
            }
            Mode::Word { from, to } => {
                let (word_from, word_to) = self.buffer.word_range(at);
                let range = extend_by_unit(*from, *to, word_from, word_to);
                self.buffer.set_selections(Selections::single(range));
            }
            Mode::Line { from, to } => {
                let (line_from, line_to) = self.buffer.line_range(self.buffer.line_of(at));
                let range = extend_by_unit(*from, *to, line_from, line_to);
                self.buffer.set_selections(Selections::single(range));
            }
            Mode::Column { anchor, keep } => {
                let head = self.pointer_column(column, row);
                let selections = if head == *anchor {
                    // A press that has not moved is Alt-click: one more caret,
                    // exactly where the click landed, with the others kept.
                    let mut ranges = keep.clone();
                    ranges.push(Range::caret(at));
                    let primary = ranges.len() - 1;
                    Selections::new(ranges, primary)
                } else {
                    // Once it moves it is a column selection, which stands on
                    // its own, as it does in every editor that has one.
                    self.buffer.column_selection(*anchor, head)
                };
                self.buffer.set_selections(selections);
            }
            Mode::Move { press, drop, .. } => {
                if drop.is_some() || (column, row) != *press {
                    *drop = Some(at);
                }
            }
        }

        self.drag = Some(drag);
        self.update_autoscroll(row, now);
        Outcome::Redraw
    }

    /// The button came up, ending any drag.
    pub(super) fn release(&mut self, mouse: MouseEvent) -> Outcome {
        self.autoscroll = None;
        let Some(drag) = self.drag.take() else { return Outcome::Continue };

        if let Mode::Move { from, to, at, drop, .. } = drag.mode {
            match drop {
                Some(dest) => {
                    let copy = mouse.modifiers.contains(KeyModifiers::CONTROL);
                    self.buffer.move_text(from, to, dest, copy);
                }
                // Pressed on the selection and let go without moving: an
                // ordinary click, which places the caret there.
                None => self.buffer.set_selections(Selections::single(Range::caret(at))),
            }
        }
        self.buffer.commit_undo_group();
        self.follow_caret();
        Outcome::Redraw
    }

    /// Forget a drag whose release never arrived — the button let go outside
    /// the window, say. Left alone, autoscroll would keep re-applying it over
    /// whatever the keyboard does next.
    pub(super) fn end_drag(&mut self) {
        self.drag = None;
        self.autoscroll = None;
    }

    /// Where dragged text would land, for drawing.
    pub(super) fn drop_marker(&self) -> Option<usize> {
        match self.drag.as_ref().map(|drag| &drag.mode) {
            Some(Mode::Move { drop, .. }) => *drop,
            _ => None,
        }
    }

    /// Start, re-aim or stop autoscroll for a pointer on `row`.
    fn update_autoscroll(&mut self, row: u16, now: Instant) {
        let (text, _) = self.areas();
        let (top, bottom) = (text.top(), text.bottom());

        // How many rows past the edge of the text the pointer is. At the very
        // top of the screen there is no row above to move to, so the edge row
        // itself counts as one.
        let (up, distance) = if row < top {
            (true, top - row)
        } else if row == top && top == self.viewport.top() {
            (true, 1)
        } else if row >= bottom {
            (false, row - bottom + 1)
        } else {
            self.autoscroll = None;
            return;
        };

        let interval = (AUTOSCROLL_BASE / u32::from(distance)).max(AUTOSCROLL_FLOOR);
        let next = self.autoscroll.map_or(now + interval, |scroll| scroll.next);
        self.autoscroll = Some(Autoscroll { up, interval, next });
        if !self.can_scroll(up) {
            self.autoscroll = None;
        }
    }

    /// Scroll one line if autoscroll is due, and carry the drag along with it.
    pub(super) fn autoscroll_tick(&mut self, now: Instant) -> Outcome {
        let Some(scroll) = self.autoscroll else { return Outcome::Continue };
        if now < scroll.next {
            return Outcome::Continue;
        }
        if !self.can_scroll(scroll.up) {
            self.autoscroll = None;
            return Outcome::Continue;
        }

        if scroll.up {
            self.scroll -= 1;
        } else {
            self.scroll += 1;
        }
        self.autoscroll = Some(Autoscroll { next: now + scroll.interval, ..scroll });
        if !self.can_scroll(scroll.up) {
            // At the end of the buffer: nothing more to scroll to, so no reason
            // to keep waking up.
            self.autoscroll = None;
        }

        // The pointer has not moved, but the text under it has.
        if let Some((column, row)) = self.drag.as_ref().map(|drag| drag.pointer) {
            self.drag_to_keeping_autoscroll(column, row, now);
        }
        Outcome::Redraw
    }

    fn drag_to_keeping_autoscroll(&mut self, column: u16, row: u16, now: Instant) {
        let autoscroll = self.autoscroll;
        self.drag_to(column, row, now);
        self.autoscroll = autoscroll;
    }

    /// Whether the view can scroll one more line in that direction.
    fn can_scroll(&self, up: bool) -> bool {
        if up { self.scroll > 0 } else { self.scroll < self.max_scroll() }
    }

    /// The furthest down autoscroll goes: the last line at the bottom of the
    /// view, not the top — there is nothing below it to select.
    fn max_scroll(&self) -> usize {
        self.buffer.len_lines().saturating_sub(self.text_height())
    }

    /// The buffer position under the pointer, clamped into the text: above
    /// the text is its first visible line, below it the last, left of it the
    /// start of the line. A drag keeps meaning something wherever it goes.
    fn pointer_position(&self, column: u16, row: u16) -> usize {
        let (text, _) = self.areas();
        let gutter = self.gutter_width();
        let column = column.max(text.left() + gutter);
        let row = row.clamp(text.top(), text.bottom().saturating_sub(1).max(text.top()));
        self.position_at(column, row).unwrap_or_else(|| self.buffer.len_chars())
    }

    /// The `(line, display column)` under the pointer, for column selection.
    /// The column is not clamped to the line: a box may be wider than the
    /// line it starts on.
    fn pointer_column(&self, column: u16, row: u16) -> (usize, usize) {
        let (text, _) = self.areas();
        let gutter = self.gutter_width();
        let row = row.clamp(text.top(), text.bottom().saturating_sub(1).max(text.top()));
        let last = self.buffer.len_lines().saturating_sub(1);
        let line = (self.scroll + usize::from(row - text.top())).min(last);
        let column = usize::from(column.saturating_sub(text.left() + gutter));
        (line, column)
    }
}

/// Extend a unit-wise selection: from the first unit clicked (`from..to`)
/// to the unit under the pointer (`unit_from..unit_to`), whichever way the
/// pointer has gone, never shrinking below the first unit.
const fn extend_by_unit(from: usize, to: usize, unit_from: usize, unit_to: usize) -> Range {
    if unit_from < from {
        Range::new(to, unit_from)
    } else {
        Range::new(from, if unit_to > to { unit_to } else { to })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{KeySet, defaults};
    use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEventKind};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use ratatui::layout::Rect;

    /// One digit of line number plus two columns of padding.
    const GUTTER: u16 = 3;

    fn app(text: &str, height: u16) -> App {
        let mut app = App::new(
            Buffer::from_text(text),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 40, height));
        app
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent { kind, column, row, modifiers })
    }

    /// A pointer that remembers the time, so presses can be spaced out.
    struct Pointer {
        now: Instant,
    }

    impl Pointer {
        fn new() -> Self {
            Self { now: Instant::now() }
        }

        fn wait(&mut self, ms: u64) {
            self.now += Duration::from_millis(ms);
        }

        fn press(&mut self, app: &mut App, column: u16, row: u16, modifiers: KeyModifiers) {
            let event = mouse(MouseEventKind::Down(MouseButton::Left), column, row, modifiers);
            app.handle_at(event, self.now);
        }

        fn drag(&mut self, app: &mut App, column: u16, row: u16) {
            let event =
                mouse(MouseEventKind::Drag(MouseButton::Left), column, row, KeyModifiers::NONE);
            app.handle_at(event, self.now);
        }

        fn release(&mut self, app: &mut App, column: u16, row: u16, modifiers: KeyModifiers) {
            let event = mouse(MouseEventKind::Up(MouseButton::Left), column, row, modifiers);
            app.handle_at(event, self.now);
        }

        fn click(&mut self, app: &mut App, column: u16, row: u16) {
            self.press(app, column, row, KeyModifiers::NONE);
            self.release(app, column, row, KeyModifiers::NONE);
        }
    }

    fn selected(app: &App) -> String {
        let range = app.buffer().selections().primary();
        app.buffer().text().to_string().chars().skip(range.from()).take(range.len()).collect()
    }

    fn text(app: &App) -> String {
        app.buffer().text().to_string()
    }

    // ── clicks ──────────────────────────────────────────────────────────────

    #[test]
    fn double_click_selects_a_word_and_triple_click_the_line() {
        let mut app = app("let value = 1;\nnext\n", 10);
        let mut pointer = Pointer::new();

        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "");
        pointer.wait(100);
        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "value");
        pointer.wait(100);
        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "let value = 1;\n");
        pointer.wait(100);
        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "", "a fourth click goes round to a caret");
    }

    #[test]
    fn two_slow_clicks_are_two_single_clicks() {
        let mut app = app("let value = 1;", 10);
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 5, 0);
        pointer.wait(2000);
        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "");
    }

    #[test]
    fn the_multi_click_threshold_is_configurable() {
        let mut app = app("let value = 1;", 10);
        app.set_double_click(Duration::from_millis(900));
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 5, 0);
        pointer.wait(800);
        pointer.click(&mut app, GUTTER + 5, 0);
        assert_eq!(selected(&app), "value", "800 ms is inside a 900 ms threshold");
    }

    #[test]
    fn shift_click_extends_from_the_anchor() {
        let mut app = app("one two three", 10);
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 4, 0);
        pointer.wait(1000);
        pointer.press(&mut app, GUTTER + 8, 0, KeyModifiers::SHIFT);
        pointer.release(&mut app, GUTTER + 8, 0, KeyModifiers::SHIFT);
        assert_eq!(selected(&app), "two ");
    }

    // ── drags ───────────────────────────────────────────────────────────────

    #[test]
    fn dragging_selects_character_by_character() {
        let mut app = app("one two three", 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER + 1, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 6, 0);
        assert_eq!(selected(&app), "ne tw");
        pointer.drag(&mut app, GUTTER, 0);
        assert_eq!(selected(&app), "o", "dragging back past the anchor reverses");
        assert!(
            app.buffer().selections().primary().head < app.buffer().selections().primary().anchor
        );
    }

    #[test]
    fn dragging_after_a_double_click_extends_by_whole_words() {
        let mut app = app("alpha beta gamma delta", 10);
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 7, 0);
        pointer.wait(50);
        pointer.press(&mut app, GUTTER + 7, 0, KeyModifiers::NONE);
        assert_eq!(selected(&app), "beta");

        pointer.drag(&mut app, GUTTER + 12, 0);
        assert_eq!(selected(&app), "beta gamma", "the whole word under the pointer");
        pointer.drag(&mut app, GUTTER + 1, 0);
        assert_eq!(selected(&app), "alpha beta", "backwards, still keeping the first word");
    }

    #[test]
    fn dragging_after_a_triple_click_extends_by_whole_lines() {
        let mut app = app("one\ntwo\nthree\nfour\n", 10);
        let mut pointer = Pointer::new();
        for _ in 0..2 {
            pointer.click(&mut app, GUTTER, 1);
            pointer.wait(50);
        }
        pointer.press(&mut app, GUTTER, 1, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 1, 2);
        assert_eq!(selected(&app), "two\nthree\n");
        pointer.drag(&mut app, GUTTER, 0);
        assert_eq!(selected(&app), "one\ntwo\n");
    }

    #[test]
    fn dragging_down_the_gutter_selects_whole_lines() {
        let mut app = app("one\ntwo\nthree\n", 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, 0, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, 0, 1);
        assert_eq!(selected(&app), "one\ntwo\n");
    }

    // ── alt: carets and columns ─────────────────────────────────────────────

    #[test]
    fn alt_click_adds_a_caret_and_typing_goes_to_both() {
        let mut app = app("abc\nabc", 10);
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 1, 0);
        pointer.wait(1000);
        pointer.press(&mut app, GUTTER + 1, 1, KeyModifiers::ALT);
        pointer.release(&mut app, GUTTER + 1, 1, KeyModifiers::ALT);
        assert_eq!(app.buffer().selections().len(), 2);

        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('X'))));
        assert_eq!(text(&app), "aXbc\naXbc");
    }

    #[test]
    fn alt_drag_makes_one_caret_per_line_skipping_short_lines() {
        let mut app = app("abcdef\nab\nabcdef\nabcdef", 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER + 3, 0, KeyModifiers::ALT);
        pointer.drag(&mut app, GUTTER + 5, 3);
        let ranges = app.buffer().selections().ranges().to_vec();
        assert_eq!(ranges.len(), 3, "the two-char line is skipped: {ranges:?}");

        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('_'))));
        assert_eq!(text(&app), "abc_f\nab\nabc_f\nabc_f");
    }

    // ── moving text ─────────────────────────────────────────────────────────

    #[test]
    fn dragging_a_selection_moves_it() {
        let mut app = app("one two three", 10);
        let mut pointer = Pointer::new();
        pointer.click(&mut app, GUTTER + 4, 0);
        pointer.wait(50);
        pointer.click(&mut app, GUTTER + 4, 0);
        assert_eq!(selected(&app), "two");

        pointer.wait(1000);
        pointer.press(&mut app, GUTTER + 5, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 13, 0);
        assert_eq!(app.drop_marker(), Some(13), "the drop point is shown");
        pointer.release(&mut app, GUTTER + 13, 0, KeyModifiers::NONE);
        assert_eq!(text(&app), "one  threetwo");
        assert_eq!(selected(&app), "two", "and stays selected where it landed");
    }

    #[test]
    fn ctrl_at_the_drop_copies_instead() {
        let mut app = app("ab", 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 1, 0);
        pointer.release(&mut app, GUTTER + 1, 0, KeyModifiers::NONE);
        assert_eq!(selected(&app), "a");

        pointer.wait(1000);
        pointer.press(&mut app, GUTTER, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 2, 0);
        pointer.release(&mut app, GUTTER + 2, 0, KeyModifiers::CONTROL);
        assert_eq!(text(&app), "aba");
    }

    #[test]
    fn pressing_on_a_selection_without_moving_is_just_a_click() {
        let mut app = app("one two", 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 0, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 7, 0);
        pointer.release(&mut app, GUTTER + 7, 0, KeyModifiers::NONE);
        pointer.wait(1000);

        pointer.click(&mut app, GUTTER + 2, 0);
        assert_eq!(text(&app), "one two", "nothing moved");
        assert_eq!(app.buffer().selections().primary(), Range::caret(2));
    }

    // ── autoscroll ──────────────────────────────────────────────────────────

    #[test]
    fn dragging_past_the_bottom_scrolls_and_keeps_extending() {
        // Ten rows: nine of text and the status line.
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER + 1, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER + 1, 9);

        let deadline = app.deadline().expect("autoscroll is waiting to fire");
        app.tick(deadline);
        assert_eq!(app.scroll(), 1);
        let head = app.buffer().selections().primary().head;
        assert_eq!(app.buffer().line_of(head), 9, "the selection followed the scroll");
    }

    #[test]
    fn autoscroll_is_faster_the_further_past_the_edge() {
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);

        pointer.drag(&mut app, GUTTER, 9);
        let near = app.autoscroll.unwrap().interval;
        pointer.drag(&mut app, GUTTER, 12);
        let far = app.autoscroll.unwrap().interval;
        assert!(far < near, "{far:?} should be shorter than {near:?}");
    }

    #[test]
    fn autoscroll_stops_at_the_end_of_the_buffer() {
        let mut app = app(&"line\n".repeat(12), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 9);

        let mut ticks = 0;
        while let Some(deadline) = app.deadline() {
            app.tick(deadline);
            ticks += 1;
            assert!(ticks < 100, "autoscroll never stopped");
        }
        assert_eq!(app.scroll(), 13 - 9, "the last line sits at the bottom, not the top");
    }

    #[test]
    fn autoscroll_up_stops_at_the_top() {
        let mut app = app(&"line\n".repeat(40), 10);
        app.handle(mouse(MouseEventKind::ScrollDown, 5, 5, KeyModifiers::NONE));
        assert_eq!(app.scroll(), 3);

        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 4, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 0);
        while let Some(deadline) = app.deadline() {
            app.tick(deadline);
        }
        assert_eq!(app.scroll(), 0);
        assert_eq!(app.buffer().selections().primary().head, 0, "selected up to the very top");
    }

    #[test]
    fn letting_go_stops_autoscroll() {
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 9);
        pointer.release(&mut app, GUTTER, 9, KeyModifiers::NONE);
        assert_eq!(app.deadline(), None);
    }

    #[test]
    fn a_key_ends_a_drag_whose_release_never_arrived() {
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 9);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Right)));
        assert_eq!(app.deadline(), None, "autoscroll stopped");
        let caret = app.buffer().selections().primary();
        let deadline = Instant::now() + Duration::from_secs(1);
        app.tick(deadline);
        assert_eq!(app.buffer().selections().primary(), caret, "the keyboard's caret stands");
    }

    #[test]
    fn losing_focus_ends_a_drag() {
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 9);
        app.handle(Event::Focus(false));
        assert_eq!(app.deadline(), None);
    }

    #[test]
    fn coming_back_inside_stops_autoscroll() {
        let mut app = app(&"line\n".repeat(40), 10);
        let mut pointer = Pointer::new();
        pointer.press(&mut app, GUTTER, 2, KeyModifiers::NONE);
        pointer.drag(&mut app, GUTTER, 9);
        pointer.drag(&mut app, GUTTER, 5);
        assert_eq!(app.deadline(), None);
    }
}
