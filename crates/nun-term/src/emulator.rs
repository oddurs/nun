//! The screen a program draws: a grid of cells with scrollback, parsed from
//! the bytes it writes.
//!
//! Everything here is in viewport coordinates — row 0 is the top row on
//! screen, whether that is the live screen or somewhere up in the scrollback
//! — because that is what a pointer lands on and what gets drawn. Nothing
//! reads or writes a file descriptor: [`Emulator::feed`] takes bytes and hands
//! back the [`Effect`]s they asked for, and whoever owns the pty carries them
//! out.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

use crate::pty::Size;

/// Lines of scrollback kept above the screen.
pub const SCROLLBACK: usize = 10_000;

/// What a program's output asked of the terminal around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Bytes to send back to the program: the answers to its queries — where
    /// the cursor is, what the terminal is, which colours it has.
    Reply(Vec<u8>),
    /// Text the program put on the clipboard, with OSC 52.
    Copy(String),
    /// The bell.
    Bell,
}

/// A colour a program asked for. Content, not chrome: the UI passes it
/// through rather than mapping it onto a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// Whatever the terminal's own foreground or background is.
    Default,
    /// One of the 256 indexed colours, the first sixteen of which are the
    /// terminal's own ANSI palette.
    Indexed(u8),
    /// An exact colour.
    Rgb(u8, u8, u8),
}

/// How a cell's text is set, as a set of flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attrs(u8);

impl Attrs {
    /// Bold.
    pub const BOLD: Self = Self(1);
    /// Faint.
    pub const DIM: Self = Self(1 << 1);
    /// Italic.
    pub const ITALIC: Self = Self(1 << 2);
    /// Underlined, in any of the ways a program can ask for.
    pub const UNDERLINE: Self = Self(1 << 3);
    /// Struck through.
    pub const STRIKE: Self = Self(1 << 4);
    /// Foreground and background swapped.
    pub const INVERSE: Self = Self(1 << 5);
    /// Not drawn.
    pub const HIDDEN: Self = Self(1 << 6);

    /// Whether every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    const fn with(self, other: Self, on: bool) -> Self {
        if on { Self(self.0 | other.0) } else { self }
    }
}

/// How much of a row a cell's character takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// One column.
    Single,
    /// Two columns: this one and the next.
    Wide,
    /// The second column of a wide character, drawn by the cell before it.
    Spacer,
}

/// One cell, as it should be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell<'a> {
    /// Its character.
    pub ch: char,
    /// Combining marks and joiners drawn with it.
    pub marks: &'a [char],
    /// Its text colour.
    pub fg: Ink,
    /// Its background.
    pub bg: Ink,
    /// How its text is set.
    pub attrs: Attrs,
    /// How wide it is.
    pub width: Width,
    /// Whether it is in the selection.
    pub selected: bool,
    /// Whether it is part of an OSC 8 hyperlink.
    pub linked: bool,
}

/// The modes a program has turned on that change what the keyboard, the
/// mouse and a paste should send it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes(u16);

impl Modes {
    /// The arrows send `ESC O` rather than `ESC [`.
    pub const APP_CURSOR: Self = Self(1);
    /// A paste is wrapped in `ESC [200~` and `ESC [201~`.
    pub const BRACKETED_PASTE: Self = Self(1 << 1);
    /// Presses and releases are reported.
    pub const MOUSE_CLICK: Self = Self(1 << 2);
    /// So is motion with a button held.
    pub const MOUSE_DRAG: Self = Self(1 << 3);
    /// So is all motion.
    pub const MOUSE_MOTION: Self = Self(1 << 4);
    /// In SGR's encoding.
    pub const SGR_MOUSE: Self = Self(1 << 5);
    /// In the UTF-8 extension of the original encoding.
    pub const UTF8_MOUSE: Self = Self(1 << 6);
    /// The program is on the alternate screen: full screen, no scrollback.
    pub const ALT_SCREEN: Self = Self(1 << 7);
    /// On the alternate screen, the wheel sends the arrows.
    pub const ALTERNATE_SCROLL: Self = Self(1 << 8);
    /// Focus coming and going is reported.
    pub const FOCUS: Self = Self(1 << 9);
    /// The cursor is shown.
    pub const SHOW_CURSOR: Self = Self(1 << 10);

    /// Whether every mode in `other` is on.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the program wants the mouse at all.
    #[must_use]
    pub const fn mouse(self) -> bool {
        self.0 & (Self::MOUSE_CLICK.0 | Self::MOUSE_DRAG.0 | Self::MOUSE_MOTION.0) != 0
    }

    /// These modes and `other`'s.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    fn of(mode: TermMode) -> Self {
        [
            (TermMode::APP_CURSOR, Self::APP_CURSOR),
            (TermMode::BRACKETED_PASTE, Self::BRACKETED_PASTE),
            (TermMode::MOUSE_REPORT_CLICK, Self::MOUSE_CLICK),
            (TermMode::MOUSE_DRAG, Self::MOUSE_DRAG),
            (TermMode::MOUSE_MOTION, Self::MOUSE_MOTION),
            (TermMode::SGR_MOUSE, Self::SGR_MOUSE),
            (TermMode::UTF8_MOUSE, Self::UTF8_MOUSE),
            (TermMode::ALT_SCREEN, Self::ALT_SCREEN),
            (TermMode::ALTERNATE_SCROLL, Self::ALTERNATE_SCROLL),
            (TermMode::FOCUS_IN_OUT, Self::FOCUS),
            (TermMode::SHOW_CURSOR, Self::SHOW_CURSOR),
        ]
        .into_iter()
        .filter(|(term, _)| mode.contains(*term))
        .fold(Self::default(), |modes, (_, ours)| modes.union(ours))
    }
}

/// What a press selects, by how many clicks it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Character by character.
    Chars,
    /// Word by word.
    Words,
    /// Whole lines.
    Lines,
}

/// The colours a program may ask the terminal about, answered from what the
/// panel is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answers {
    /// The default text colour.
    pub foreground: (u8, u8, u8),
    /// The default background.
    pub background: (u8, u8, u8),
}

/// Collects what the parser asks of the world, to be drained after each
/// feed. Single-threaded on purpose: the emulator lives on the main thread.
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

/// The size, as the emulator's grid measures it.
struct Dims(Size);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        usize::from(self.0.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.0.cols)
    }
}

/// A terminal screen.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    size: Size,
    title: Option<String>,
    cwd: Cwd,
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("size", &self.size)
            .field("title", &self.title)
            .field("cwd", &self.cwd.dir)
            .finish_non_exhaustive()
    }
}

impl Emulator {
    /// An empty screen of `size`, keeping `scrollback` lines above it.
    #[must_use]
    pub fn new(size: Size, scrollback: usize) -> Self {
        let events = Listener::default();
        // No Kitty keyboard protocol: keys are sent in the legacy encoding,
        // so a program asking for the protocol is told there is none and
        // falls back, rather than being sent keys it cannot read.
        let config =
            Config { scrolling_history: scrollback, kitty_keyboard: false, ..Config::default() };
        Self {
            term: Term::new(config, &Dims(size), events.clone()),
            parser: Processor::new(),
            events,
            size,
            title: None,
            cwd: Cwd::default(),
        }
    }

    /// Parse what the program wrote, and say what it asked for.
    pub fn feed(&mut self, bytes: &[u8], answers: Answers) -> Vec<Effect> {
        self.cwd.scan(bytes);
        self.parser.advance(&mut self.term, bytes);
        self.effects(answers)
    }

    /// The directory the shell last said it was in, with OSC 7, if it has.
    /// Paths in its output are relative to this.
    #[must_use]
    pub fn cwd(&self) -> Option<&std::path::Path> {
        self.cwd.dir.as_deref()
    }

    /// When a synchronized update the program started should be drawn even
    /// though it has not said it is finished, if one is open.
    #[must_use]
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Draw a synchronized update the program never finished.
    pub fn end_sync(&mut self, answers: Answers) -> Vec<Effect> {
        self.parser.stop_sync(&mut self.term);
        self.effects(answers)
    }

    fn effects(&mut self, answers: Answers) -> Vec<Effect> {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        let mut effects = Vec::new();
        for event in events {
            match event {
                Event::PtyWrite(text) => effects.push(Effect::Reply(text.into_bytes())),
                Event::ColorRequest(index, format) => {
                    let known = self.term.colors()[index].or_else(|| {
                        let (r, g, b) = match index {
                            i if i == NamedColor::Foreground as usize => answers.foreground,
                            i if i == NamedColor::Background as usize => answers.background,
                            i if i == NamedColor::Cursor as usize => answers.foreground,
                            _ => return None,
                        };
                        Some(Rgb { r, g, b })
                    });
                    // An indexed colour nun was never told is not answered:
                    // silence is a reply a program can time out on, and a
                    // guess is one it would believe.
                    if let Some(rgb) = known {
                        effects.push(Effect::Reply(format(rgb).into_bytes()));
                    }
                }
                Event::ClipboardStore(_, text) => effects.push(Effect::Copy(text)),
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                Event::Bell => effects.push(Effect::Bell),
                _ => {}
            }
        }
        effects
    }

    /// Make the screen `size`, reflowing what is on it.
    pub fn resize(&mut self, size: Size) {
        if size != self.size {
            self.size = size;
            self.term.resize(Dims(size));
        }
    }

    /// Its size.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    /// What the program called itself, with OSC 0 or 2.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The modes the program has on.
    #[must_use]
    pub fn modes(&self) -> Modes {
        Modes::of(*self.term.mode())
    }

    /// Scroll the view `lines` up into the scrollback, or down with a
    /// negative count. Stops at either end.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Back to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// How many lines up into the scrollback the view is.
    #[must_use]
    pub fn scrolled(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// How many lines of scrollback there are.
    #[must_use]
    pub fn history(&self) -> usize {
        self.term.grid().history_size()
    }

    /// The grid point on viewport row `row`, column `col`.
    fn point(&self, row: u16, col: u16) -> Point {
        let offset = i32::try_from(self.scrolled()).unwrap_or(i32::MAX);
        let col = usize::from(col).min(usize::from(self.size.cols).saturating_sub(1));
        Point::new(Line(i32::from(row) - offset), Column(col))
    }

    /// Visit every cell on screen, row by row.
    pub fn visit(&self, mut visit: impl FnMut(u16, u16, Cell<'_>)) {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let selection =
            self.term.selection.as_ref().and_then(|selection| selection.to_range(&self.term));
        for row in 0..self.size.rows {
            let line = self.point(row, 0).line;
            if line.0 < -i32::try_from(self.history()).unwrap_or(i32::MAX) {
                continue;
            }
            let cells = &grid[line];
            for col in 0..self.size.cols {
                let cell = &cells[Column(usize::from(col))];
                let (fg, fg_dim) = ink(cell.fg, colors);
                let (bg, _) = ink(cell.bg, colors);
                let flags = cell.flags;
                let attrs = Attrs::default()
                    .with(Attrs::BOLD, flags.contains(Flags::BOLD))
                    .with(Attrs::DIM, flags.contains(Flags::DIM) || fg_dim)
                    .with(Attrs::ITALIC, flags.contains(Flags::ITALIC))
                    .with(Attrs::UNDERLINE, flags.intersects(Flags::ALL_UNDERLINES))
                    .with(Attrs::STRIKE, flags.contains(Flags::STRIKEOUT))
                    .with(Attrs::INVERSE, flags.contains(Flags::INVERSE))
                    .with(Attrs::HIDDEN, flags.contains(Flags::HIDDEN));
                let width = if flags.contains(Flags::WIDE_CHAR) {
                    Width::Wide
                } else if flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    Width::Spacer
                } else {
                    Width::Single
                };
                // A leading spacer stands at the end of a row for a wide
                // character that did not fit; it draws as a blank.
                let width = if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    Width::Single
                } else {
                    width
                };
                let point = Point::new(line, Column(usize::from(col)));
                visit(
                    row,
                    col,
                    Cell {
                        ch: if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                            ' '
                        } else {
                            cell.c
                        },
                        marks: cell.zerowidth().unwrap_or(&[]),
                        fg,
                        bg,
                        attrs,
                        width,
                        selected: selection.is_some_and(|range| range.contains(point)),
                        linked: cell.hyperlink().is_some(),
                    },
                );
            }
        }
    }

    /// Where the cursor is on screen, when it is shown and the view is on
    /// the live screen.
    #[must_use]
    pub fn cursor(&self) -> Option<(u16, u16)> {
        if !self.modes().contains(Modes::SHOW_CURSOR) || self.scrolled() > 0 {
            return None;
        }
        let point = self.term.grid().cursor.point;
        let row = u16::try_from(point.line.0).ok()?;
        let col = u16::try_from(point.column.0).ok()?;
        (row < self.size.rows && col < self.size.cols).then_some((row, col))
    }

    /// The text of viewport row `row`, and the column each of its characters
    /// starts in. A wide character's second column has no character of its
    /// own, so the two can differ.
    #[must_use]
    pub fn row_text(&self, row: u16) -> (String, Vec<u16>) {
        let mut text = String::new();
        let mut columns = Vec::new();
        if row >= self.size.rows {
            return (text, columns);
        }
        let line = self.point(row, 0).line;
        let cells = &self.term.grid()[line];
        for col in 0..self.size.cols {
            let cell = &cells[Column(usize::from(col))];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
            columns.push(col);
            for mark in cell.zerowidth().unwrap_or(&[]) {
                text.push(*mark);
                columns.push(col);
            }
        }
        (text, columns)
    }

    /// The OSC 8 link at a cell, and the columns of its row that it spans.
    #[must_use]
    pub fn link_at(&self, row: u16, col: u16) -> Option<(String, u16, u16)> {
        if row >= self.size.rows || col >= self.size.cols {
            return None;
        }
        let line = self.point(row, 0).line;
        let cells = &self.term.grid()[line];
        let hyperlink = cells[Column(usize::from(col))].hyperlink()?;
        let same =
            |at: u16| cells[Column(usize::from(at))].hyperlink().as_ref() == Some(&hyperlink);
        let mut start = col;
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < self.size.cols && same(end) {
            end += 1;
        }
        Some((hyperlink.uri().to_string(), start, end))
    }

    /// Start a selection at a cell. `right` is whether the press was on the
    /// cell's right half, which decides whether the cell itself is in.
    pub fn select(&mut self, row: u16, col: u16, pick: Pick, right: bool) {
        let kind = match pick {
            Pick::Chars => SelectionType::Simple,
            Pick::Words => SelectionType::Semantic,
            Pick::Lines => SelectionType::Lines,
        };
        let side = if right { Side::Right } else { Side::Left };
        self.term.selection = Some(Selection::new(kind, self.point(row, col), side));
    }

    /// Move the selection's far end to a cell.
    pub fn extend(&mut self, row: u16, col: u16, right: bool) {
        let point = self.point(row, col);
        let side = if right { Side::Right } else { Side::Left };
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Select nothing.
    pub fn deselect(&mut self) {
        self.term.selection = None;
    }

    /// The selected text, if anything is selected.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.term.selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        self.term.selection_to_string().filter(|text| !text.is_empty())
    }

    /// Everything on screen and in the scrollback, as text, one line per
    /// row. Trailing blank rows are left off.
    #[must_use]
    pub fn contents(&self) -> String {
        let grid = self.term.grid();
        let top = -i32::try_from(self.history()).unwrap_or(i32::MAX);
        let bottom = i32::from(self.size.rows);
        let mut lines: Vec<String> = (top..bottom)
            .map(|line| {
                let cells = &grid[Line(line)];
                let mut text = String::new();
                for col in 0..usize::from(self.size.cols) {
                    let cell = &cells[Column(col)];
                    if !cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        text.push(cell.c);
                    }
                }
                text.trim_end().to_string()
            })
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }
}

/// Watches the output for OSC 7, which shells use to say which directory
/// they are in: `ESC ] 7 ; file://host/path` ended by BEL or `ESC \`. The
/// emulator itself drops it, and a sequence can arrive split across reads,
/// so this keeps what it has seen of one until it ends.
#[derive(Debug, Default)]
struct Cwd {
    dir: Option<PathBuf>,
    /// How much of `ESC ] 7 ;` has been matched, while looking for it.
    matched: usize,
    /// The body so far, once the introducer has been seen.
    body: Option<Vec<u8>>,
}

impl Cwd {
    const INTRODUCER: &[u8] = b"\x1b]7;";
    /// Longer than any path worth following; past it, the sequence is
    /// dropped rather than grown without end.
    const MOST: usize = 4096;

    fn scan(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if let Some(body) = self.body.as_mut() {
                match byte {
                    0x07 | 0x1b => {
                        // An ESC ends it: `ESC \` is the string terminator,
                        // and a terminal ends the sequence at any other escape
                        // just the same.
                        let body = self.body.take().unwrap_or_default();
                        if let Some(dir) = directory(&body) {
                            self.dir = Some(dir);
                        }
                        self.matched = usize::from(byte == 0x1b);
                    }
                    _ if body.len() >= Self::MOST => self.body = None,
                    _ => body.push(byte),
                }
                continue;
            }
            if byte == Self::INTRODUCER[self.matched] {
                self.matched += 1;
                if self.matched == Self::INTRODUCER.len() {
                    self.matched = 0;
                    self.body = Some(Vec::new());
                }
            } else {
                self.matched = usize::from(byte == 0x1b);
            }
        }
    }
}

/// The directory an OSC 7 body names: a `file:` URL whose host is this
/// machine or empty, percent-decoded. A directory on another host — the
/// shell is over ssh — is not one paths here are relative to.
fn directory(body: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(body).ok()?;
    let rest = text.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let local = host.is_empty()
        || host == "localhost"
        || rustix::system::uname()
            .nodename()
            .to_str()
            .is_ok_and(|name| name == host || name.split('.').next() == host.split('.').next());
    if !local {
        return None;
    }
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Ok(value) = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16)
        {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(decoded).ok()?))
}

/// A cell colour as [`Ink`], and whether it is a dim variant. A colour the
/// program set with OSC 4 is the colour it set.
fn ink(color: Color, colors: &Colors) -> (Ink, bool) {
    let exact = |index: usize| colors[index].map(|rgb| Ink::Rgb(rgb.r, rgb.g, rgb.b));
    match color {
        Color::Spec(rgb) => (Ink::Rgb(rgb.r, rgb.g, rgb.b), false),
        Color::Indexed(index) => (exact(usize::from(index)).unwrap_or(Ink::Indexed(index)), false),
        Color::Named(named) => {
            let index = named as usize;
            if let Ok(ansi) = u8::try_from(index)
                && ansi < 16
            {
                return (exact(index).unwrap_or(Ink::Indexed(ansi)), false);
            }
            let dim = named.to_bright();
            if dim != named && (dim as usize) < 16 {
                let base = u8::try_from(dim as usize).unwrap_or(0);
                return (exact(dim as usize).unwrap_or(Ink::Indexed(base)), true);
            }
            let dim = named == NamedColor::DimForeground;
            (exact(index).unwrap_or(Ink::Default), dim)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWERS: Answers =
        Answers { foreground: (0xdd, 0xdd, 0xdd), background: (0x11, 0x22, 0x33) };

    fn screen(cols: u16, rows: u16) -> Emulator {
        Emulator::new(Size::new(cols, rows), 100)
    }

    fn fed(cols: u16, rows: u16, bytes: &[u8]) -> Emulator {
        let mut emulator = screen(cols, rows);
        emulator.feed(bytes, ANSWERS);
        emulator
    }

    fn row(emulator: &Emulator, row: u16) -> String {
        emulator.row_text(row).0.trim_end().to_string()
    }

    fn cell_at(emulator: &Emulator, at: (u16, u16)) -> (char, Ink, Ink, Attrs, Width) {
        let mut found = None;
        emulator.visit(|row, col, cell| {
            if (row, col) == at {
                found = Some((cell.ch, cell.fg, cell.bg, cell.attrs, cell.width));
            }
        });
        found.expect("the cell is on screen")
    }

    #[test]
    fn text_lands_where_the_cursor_is_and_wraps() {
        let emulator = fed(5, 3, b"hello world\r\nnext");
        // The line break scrolled the first row into the scrollback.
        assert_eq!(row(&emulator, 0), " worl");
        assert_eq!(row(&emulator, 1), "d");
        assert_eq!(row(&emulator, 2), "next");
        assert_eq!(emulator.history(), 1);
        assert_eq!(emulator.cursor(), Some((2, 4)));
    }

    #[test]
    fn colours_pass_through_as_the_program_asked() {
        let emulator =
            fed(20, 2, b"\x1b[31ma\x1b[1;38;5;208mb\x1b[0;38;2;1;2;3;48;2;4;5;6mc\x1b[0md");
        assert_eq!(cell_at(&emulator, (0, 0)).1, Ink::Indexed(1));
        let (_, fg, _, attrs, _) = cell_at(&emulator, (0, 1));
        assert_eq!(fg, Ink::Indexed(208));
        assert!(attrs.contains(Attrs::BOLD));
        let (_, fg, bg, _, _) = cell_at(&emulator, (0, 2));
        assert_eq!((fg, bg), (Ink::Rgb(1, 2, 3), Ink::Rgb(4, 5, 6)));
        assert_eq!(cell_at(&emulator, (0, 3)).1, Ink::Default);
        assert_eq!(cell_at(&emulator, (0, 3)).2, Ink::Default);
    }

    #[test]
    fn a_palette_colour_the_program_redefined_is_the_colour_it_set() {
        let emulator = fed(10, 1, b"\x1b]4;1;rgb:ff/00/80\x07\x1b[31mx");
        assert_eq!(cell_at(&emulator, (0, 0)).1, Ink::Rgb(0xff, 0, 0x80));
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let emulator = fed(10, 1, "日本é\u{301}".as_bytes());
        assert_eq!(cell_at(&emulator, (0, 0)).4, Width::Wide);
        assert_eq!(cell_at(&emulator, (0, 1)).4, Width::Spacer);
        assert_eq!(cell_at(&emulator, (0, 2)).0, '本');
        let (text, columns) = emulator.row_text(0);
        assert!(text.starts_with("日本é\u{301}"), "{text:?}");
        assert_eq!(&columns[..4], &[0, 2, 4, 4], "the combining mark is in its base's column");
    }

    #[test]
    fn the_alternate_screen_comes_and_goes_leaving_the_shell_as_it_was() {
        let mut emulator = fed(10, 3, b"$ less\r\n");
        emulator.feed(b"\x1b[?1049h\x1b[H\x1b[2Jpage one", ANSWERS);
        assert!(emulator.modes().contains(Modes::ALT_SCREEN));
        assert_eq!(row(&emulator, 0), "page one");
        emulator.feed(b"\x1b[?1049l", ANSWERS);
        assert!(!emulator.modes().contains(Modes::ALT_SCREEN));
        assert_eq!(row(&emulator, 0), "$ less");
        assert_eq!(emulator.history(), 0, "the full-screen program left no scrollback");
    }

    #[test]
    fn a_scroll_region_scrolls_only_itself() {
        // What `less` and `vim` do: fix a status line at the bottom and
        // scroll the rest.
        let mut emulator = fed(10, 4, b"\x1b[?1049h\x1b[4;1Hstatus\x1b[1;3r\x1b[1;1Ha\r\nb\r\nc");
        emulator.feed(b"\r\nd", ANSWERS);
        let rows: Vec<String> = (0..4).map(|r| row(&emulator, r)).collect();
        assert_eq!(rows, ["b", "c", "d", "status"]);
    }

    #[test]
    fn queries_are_answered() {
        let mut emulator = fed(10, 5, b"\x1b[3;4H");
        assert_eq!(emulator.feed(b"\x1b[6n", ANSWERS), [Effect::Reply(b"\x1b[3;4R".to_vec())]);
        let effects = emulator.feed(b"\x1b]11;?\x07", ANSWERS);
        let [Effect::Reply(reply)] = effects.as_slice() else { panic!("{effects:?}") };
        let reply = String::from_utf8_lossy(reply);
        assert!(reply.starts_with("\x1b]11;rgb:1111/2222/3333"), "{reply:?}");
        // Primary device attributes: a program waits on this to know it is in
        // a terminal at all.
        let effects = emulator.feed(b"\x1b[c", ANSWERS);
        assert!(
            matches!(effects.as_slice(), [Effect::Reply(reply)] if reply.starts_with(b"\x1b[?"))
        );
    }

    #[test]
    fn an_unknown_palette_colour_is_not_guessed() {
        let mut emulator = screen(10, 1);
        assert_eq!(emulator.feed(b"\x1b]4;3;?\x07", ANSWERS), []);
    }

    #[test]
    fn the_clipboard_the_title_and_the_bell_are_effects() {
        let mut emulator = screen(10, 1);
        // "hi", base64.
        assert_eq!(emulator.feed(b"\x1b]52;c;aGk=\x07", ANSWERS), [Effect::Copy("hi".into())]);
        assert_eq!(emulator.feed(b"\x07", ANSWERS), [Effect::Bell]);
        emulator.feed(b"\x1b]2;cargo test\x07", ANSWERS);
        assert_eq!(emulator.title(), Some("cargo test"));
    }

    #[test]
    fn modes_follow_what_the_program_turns_on() {
        let mut emulator = screen(10, 1);
        assert!(emulator.modes().contains(Modes::SHOW_CURSOR));
        emulator.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?25l", ANSWERS);
        let modes = emulator.modes();
        for mode in [
            Modes::APP_CURSOR,
            Modes::BRACKETED_PASTE,
            Modes::MOUSE_DRAG,
            Modes::SGR_MOUSE,
            Modes::FOCUS,
        ] {
            assert!(modes.contains(mode), "{mode:?}");
        }
        assert!(modes.mouse());
        assert!(!modes.contains(Modes::SHOW_CURSOR));
        assert_eq!(emulator.cursor(), None);
    }

    #[test]
    fn the_view_scrolls_into_the_scrollback_and_back() {
        let lines = (0..10).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\r\n") + "\r\n";
        let mut emulator = fed(10, 3, lines.as_bytes());
        assert_eq!(row(&emulator, 0), "line 8");
        emulator.scroll(2);
        assert_eq!(emulator.scrolled(), 2);
        assert_eq!(row(&emulator, 0), "line 6");
        assert_eq!(emulator.cursor(), None, "no cursor up in the scrollback");
        emulator.scroll(100);
        assert_eq!(row(&emulator, 0), "line 0", "stops at the top");
        emulator.scroll_to_bottom();
        assert_eq!(emulator.scrolled(), 0);
        assert!(emulator.contents().starts_with("line 0\nline 1\n"));
    }

    #[test]
    fn a_selection_is_copied_as_text() {
        let mut emulator = fed(20, 2, b"one two three\r\nfour");
        emulator.select(0, 4, Pick::Chars, false);
        emulator.extend(0, 6, true);
        assert_eq!(emulator.selected_text().as_deref(), Some("two"));
        emulator.select(0, 9, Pick::Words, false);
        assert_eq!(emulator.selected_text().as_deref(), Some("three"));
        emulator.select(1, 0, Pick::Lines, false);
        assert_eq!(emulator.selected_text().as_deref(), Some("four\n"));
        let mut selected = 0;
        emulator.visit(|_, _, cell| selected += usize::from(cell.selected));
        assert!(selected >= 4, "{selected}");
        emulator.deselect();
        assert_eq!(emulator.selected_text(), None);
    }

    #[test]
    fn a_hyperlink_is_found_across_the_cells_it_covers() {
        let emulator =
            fed(30, 1, b"see \x1b]8;;https://example.com\x1b\\the docs\x1b]8;;\x1b\\ now");
        assert_eq!(emulator.link_at(0, 6), Some(("https://example.com".into(), 4, 12)));
        assert_eq!(emulator.link_at(0, 1), None);
    }

    #[test]
    fn a_resize_reflows_and_keeps_the_text() {
        let mut emulator = fed(10, 3, b"abcdefghij");
        emulator.resize(Size::new(5, 3));
        assert_eq!(emulator.size(), Size::new(5, 3));
        assert!(emulator.contents().contains("abcde\nfghij"), "{:?}", emulator.contents());
        emulator.resize(Size::new(20, 3));
        assert!(emulator.contents().contains("abcdefghij"), "{:?}", emulator.contents());
    }

    #[test]
    fn the_shells_directory_is_followed_through_osc_7() {
        let mut emulator = screen(10, 1);
        assert_eq!(emulator.cwd(), None);
        emulator.feed(b"\x1b]7;file:///tmp/a%20b\x07", ANSWERS);
        assert_eq!(emulator.cwd(), Some(std::path::Path::new("/tmp/a b")));
        // Split across reads, and ended with ST.
        emulator.feed(b"\x1b]7;file://local", ANSWERS);
        emulator.feed(b"host/srv\x1b\\", ANSWERS);
        assert_eq!(emulator.cwd(), Some(std::path::Path::new("/srv")));
        emulator.feed(b"\x1b]7;file://some.other.host.example/elsewhere\x07", ANSWERS);
        assert_eq!(emulator.cwd(), Some(std::path::Path::new("/srv")), "not a directory here");
    }

    /// A shell session as a real one writes it: a prompt with colours and a
    /// title, a command's output, then `less` taking over the screen and
    /// giving it back.
    const SESSION: &[u8] = b"\x1b]0;~/nun\x07\x1b[1;32m~/nun\x1b[0m $ cargo test\r\n\
        error[E0308]: mismatched types\r\n  --> src/main.rs:42:8\r\n\x1b[?1049h\x1b[?1h\x1b=\
        \x1b[H\x1b[2J\x1b[?25lREADME\r\n\x1b[7m(END)\x1b[27m\x1b[?1049l\x1b[?1l\x1b>\x1b[?25h\
        \x1b[1;32m~/nun\x1b[0m $ ";

    #[test]
    fn a_recorded_session_leaves_the_screen_the_shell_drew() {
        let emulator = fed(40, 6, SESSION);
        assert_eq!(emulator.title(), Some("~/nun"));
        assert_eq!(row(&emulator, 2), "  --> src/main.rs:42:8");
        assert_eq!(row(&emulator, 3), "~/nun $");
        assert!(!emulator.modes().contains(Modes::ALT_SCREEN));
        assert!(!emulator.modes().contains(Modes::APP_CURSOR));
        assert_eq!(emulator.cursor(), Some((3, 8)));
        assert_eq!(cell_at(&emulator, (3, 0)).1, Ink::Indexed(2));
    }

    proptest::proptest! {
        /// However the output is cut into reads, the screen comes out the
        /// same: a sequence split between two reads is still one sequence.
        #[test]
        fn chunking_never_changes_the_screen(cuts in proptest::collection::vec(0..SESSION.len(), 0..12)) {
            let whole = fed(40, 6, SESSION);
            let mut cuts = cuts;
            cuts.sort_unstable();
            let mut pieces = screen(40, 6);
            let mut start = 0;
            for cut in cuts.into_iter().chain([SESSION.len()]) {
                pieces.feed(&SESSION[start..cut.max(start)], ANSWERS);
                start = cut.max(start);
            }
            proptest::prop_assert_eq!(pieces.contents(), whole.contents());
            proptest::prop_assert_eq!(pieces.cursor(), whole.cursor());
            proptest::prop_assert_eq!(pieces.title(), whole.title());
        }

        #[test]
        fn any_bytes_at_all_are_survived(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400)) {
            let mut emulator = screen(12, 4);
            emulator.feed(&bytes, ANSWERS);
            emulator.visit(|_, _, _| {});
            let _ = emulator.contents();
            emulator.resize(Size::new(3, 2));
            emulator.visit(|_, _, _| {});
        }
    }
}
