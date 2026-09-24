//! ratatui's crossterm backend, taught the curly underline.
//!
//! ratatui has no curly underline: `Modifier` has no flag for one, and its
//! crossterm backend only ever writes `SGR 4`. It also writes underline colour
//! whenever a cell has one, in the semicolon form, which a terminal that does
//! not know `58` misreads as a run of unrelated attributes. So nun draws
//! through this instead: everything but `draw` is crossterm's, and `draw`
//! handles underlines itself.
//!
//! The rule is one line. A cell that is underlined *and* has an underline
//! colour is asking for a curl, which it gets if [`Underlines::curly`] allows;
//! otherwise it gets a straight line. The colour is written only when
//! [`Underlines::colour`] allows. That makes this the single place where what
//! the terminal can do meets the bytes it is sent — widgets say what they want
//! and never ask.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::style::{
    Attribute, Color as CrosstermColor, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor,
};
use crossterm::terminal::{self, Clear};
use crossterm::{execute, queue};
use ratatui::backend::{Backend, ClearType, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};

use crate::underline::Underlines;

/// Which underline the terminal is currently drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    None,
    Straight,
    Curly,
}

/// A crossterm backend that draws curly, coloured underlines where the
/// terminal can.
#[derive(Debug)]
pub struct NunBackend<W: Write> {
    writer: W,
    underlines: Underlines,
}

impl<W: Write> NunBackend<W> {
    /// Draw to `writer`, with the underlines the terminal said it has.
    pub const fn new(writer: W, underlines: Underlines) -> Self {
        Self { writer, underlines }
    }

    /// Draw with `underlines` from now on: the configuration changed.
    pub const fn set_underlines(&mut self, underlines: Underlines) {
        self.underlines = underlines;
    }

    /// The writer, for tests that read back what was written.
    #[cfg(test)]
    const fn writer(&self) -> &W {
        &self.writer
    }

    /// The underline a cell gets on this terminal.
    fn line_of(&self, cell: &Cell) -> Line {
        if !cell.modifier.contains(Modifier::UNDERLINED) {
            Line::None
        } else if self.underlines.curly && cell.underline_color != Color::Reset {
            Line::Curly
        } else {
            Line::Straight
        }
    }

    /// The underline colour a cell gets on this terminal: none at all where
    /// colour is not understood, and none where there is no underline to
    /// colour.
    fn underline_colour_of(&self, cell: &Cell, line: Line) -> Color {
        if self.underlines.colour && line != Line::None {
            cell.underline_color
        } else {
            Color::Reset
        }
    }
}

/// What ends an OSC 8 link.
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// A cell's symbol that is one grapheme wrapped in its own OSC 8 link, split
/// into the sequence that opens the link and the grapheme. `None` for any
/// other symbol, which is written as it is.
fn split_link(symbol: &str) -> Option<(&str, &str)> {
    if !symbol.starts_with("\x1b]8;") {
        return None;
    }
    let end = symbol.find("\x1b\\")? + 2;
    let (open, rest) = symbol.split_at(end);
    let text = rest.strip_suffix(LINK_CLOSE)?;
    (!text.contains('\x1b')).then_some((open, text))
}

impl<W: Write> Write for NunBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write> Backend for NunBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut fg = Color::Reset;
        let mut bg = Color::Reset;
        let mut underline_colour = Color::Reset;
        // Underlining is tracked apart from the other modifiers: a cell
        // going from a straight line to a curl is no change to `Modifier` at
        // all, so a diff of modifiers would write nothing.
        let mut modifier = Modifier::empty();
        let mut line = Line::None;
        let mut last: Option<Position> = None;
        // The OSC 8 link the terminal is inside, if any. Each link cell
        // carries its own open and close, so a partial redraw can never leave
        // a link open; cells of one link written one after another are sent
        // as one link, which is shorter and which every terminal groups.
        let mut linked: Option<&str> = None;

        for (x, y, cell) in content {
            // Only move when this cell is not the one after the last.
            if !matches!(last, Some(at) if x == at.x + 1 && y == at.y) {
                if linked.take().is_some() {
                    self.writer.write_all(LINK_CLOSE.as_bytes())?;
                }
                queue!(self.writer, MoveTo(x, y))?;
            }
            last = Some(Position { x, y });

            let wanted = cell.modifier - Modifier::UNDERLINED;
            if wanted != modifier {
                queue_modifier_diff(&mut self.writer, modifier, wanted)?;
                modifier = wanted;
            }
            let wanted_line = self.line_of(cell);
            if wanted_line != line {
                self.writer.write_all(match wanted_line {
                    Line::None => b"\x1b[24m",
                    Line::Straight => b"\x1b[4m",
                    Line::Curly => b"\x1b[4:3m",
                })?;
                line = wanted_line;
            }
            if cell.fg != fg || cell.bg != bg {
                queue!(
                    self.writer,
                    SetColors(crossterm::style::Colors::new(
                        cell.fg.into_crossterm(),
                        cell.bg.into_crossterm()
                    ))
                )?;
                fg = cell.fg;
                bg = cell.bg;
            }
            let wanted_colour = self.underline_colour_of(cell, line);
            if wanted_colour != underline_colour {
                write_underline_colour(&mut self.writer, wanted_colour)?;
                underline_colour = wanted_colour;
            }

            if let Some((open, text)) = split_link(cell.symbol()) {
                if linked != Some(open) {
                    if linked.is_some() {
                        self.writer.write_all(LINK_CLOSE.as_bytes())?;
                    }
                    self.writer.write_all(open.as_bytes())?;
                    linked = Some(open);
                }
                queue!(self.writer, Print(text))?;
            } else {
                if linked.take().is_some() {
                    self.writer.write_all(LINK_CLOSE.as_bytes())?;
                }
                queue!(self.writer, Print(cell.symbol()))?;
            }
        }
        if linked.is_some() {
            self.writer.write_all(LINK_CLOSE.as_bytes())?;
        }

        queue!(
            self.writer,
            SetForegroundColor(CrosstermColor::Reset),
            SetBackgroundColor(CrosstermColor::Reset),
        )?;
        // Only a terminal that understands 58 is ever told to reset it.
        if underline_colour != Color::Reset {
            self.writer.write_all(b"\x1b[59m")?;
        }
        queue!(self.writer, SetAttribute(Attribute::Reset))
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Show)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        crossterm::cursor::position().map(|(x, y)| Position { x, y })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let Position { x, y } = position.into();
        execute!(self.writer, MoveTo(x, y))
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        let kind = match clear_type {
            ClearType::All => terminal::ClearType::All,
            ClearType::AfterCursor => terminal::ClearType::FromCursorDown,
            ClearType::BeforeCursor => terminal::ClearType::FromCursorUp,
            ClearType::CurrentLine => terminal::ClearType::CurrentLine,
            ClearType::UntilNewLine => terminal::ClearType::UntilNewLine,
        };
        execute!(self.writer, Clear(kind))
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        for _ in 0..n {
            queue!(self.writer, Print("\n"))?;
        }
        self.writer.flush()
    }

    fn size(&self) -> io::Result<Size> {
        let (width, height) = terminal::size()?;
        Ok(Size { width, height })
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let size = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size { width: size.columns, height: size.rows },
            pixels: Size { width: size.width, height: size.height },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Encodes a colour it is handed; it never chooses one. Every colour that
/// reaches here came from the ramp, through `Palette::underline`.
///
/// Write the underline colour in the colon form, which is the one a terminal
/// that knows 58 is sure to read as one parameter.
///
/// Only colours from the ramp reach here, and those are always RGB; an
/// indexed one is written in its own colon form, and anything else is left
/// alone rather than guessed at.
fn write_underline_colour(writer: &mut impl Write, colour: Color) -> io::Result<()> {
    match colour {
        Color::Rgb(r, g, b) => write!(writer, "\x1b[58:2::{r}:{g}:{b}m"),
        Color::Indexed(index) => write!(writer, "\x1b[58:5:{index}m"),
        Color::Reset => writer.write_all(b"\x1b[59m"),
        _ => Ok(()),
    }
}

/// Move the terminal from one set of modifiers to another, underlining
/// aside. The same sequence ratatui's backend writes, which it does not
/// export.
fn queue_modifier_diff(writer: &mut impl Write, from: Modifier, to: Modifier) -> io::Result<()> {
    let removed = from - to;
    if removed.contains(Modifier::REVERSED) {
        queue!(writer, SetAttribute(Attribute::NoReverse))?;
    }
    // Bold and dim are both turned off by one attribute, so whichever of them
    // is still wanted goes back on after it.
    let reset_intensity = removed.intersects(Modifier::BOLD | Modifier::DIM);
    if reset_intensity {
        queue!(writer, SetAttribute(Attribute::NormalIntensity))?;
        if to.contains(Modifier::DIM) {
            queue!(writer, SetAttribute(Attribute::Dim))?;
        }
        if to.contains(Modifier::BOLD) {
            queue!(writer, SetAttribute(Attribute::Bold))?;
        }
    }
    if removed.contains(Modifier::ITALIC) {
        queue!(writer, SetAttribute(Attribute::NoItalic))?;
    }
    if removed.contains(Modifier::CROSSED_OUT) {
        queue!(writer, SetAttribute(Attribute::NotCrossedOut))?;
    }
    if removed.contains(Modifier::HIDDEN) {
        queue!(writer, SetAttribute(Attribute::NoHidden))?;
    }
    if removed.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK) {
        queue!(writer, SetAttribute(Attribute::NoBlink))?;
    }

    let added = to - from;
    if added.contains(Modifier::REVERSED) {
        queue!(writer, SetAttribute(Attribute::Reverse))?;
    }
    if added.contains(Modifier::BOLD) && !reset_intensity {
        queue!(writer, SetAttribute(Attribute::Bold))?;
    }
    if added.contains(Modifier::ITALIC) {
        queue!(writer, SetAttribute(Attribute::Italic))?;
    }
    if added.contains(Modifier::DIM) && !reset_intensity {
        queue!(writer, SetAttribute(Attribute::Dim))?;
    }
    if added.contains(Modifier::CROSSED_OUT) {
        queue!(writer, SetAttribute(Attribute::CrossedOut))?;
    }
    if added.contains(Modifier::HIDDEN) {
        queue!(writer, SetAttribute(Attribute::Hidden))?;
    }
    if added.contains(Modifier::SLOW_BLINK) {
        queue!(writer, SetAttribute(Attribute::SlowBlink))?;
    }
    if added.contains(Modifier::RAPID_BLINK) {
        queue!(writer, SetAttribute(Attribute::RapidBlink))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use nun_theme::{Probe, Role, derive};
    use ratatui::buffer::Buffer as Cells;
    use ratatui::layout::Rect;

    use super::*;
    use crate::style::Palette;

    /// Draw `cells` through a backend for a terminal with `underlines`, and
    /// return exactly the bytes written.
    fn bytes(underlines: Underlines, cells: &Cells) -> String {
        let mut backend = NunBackend::new(Vec::new(), underlines);
        let content = cells.content().iter().enumerate().map(|(index, cell)| {
            let (x, y) = cells.pos_of(index);
            (x, y, cell)
        });
        backend.draw(content).unwrap();
        String::from_utf8(backend.writer().clone()).unwrap()
    }

    /// Four cells: plain, a diagnostic's underline twice, plain.
    fn marked() -> Cells {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let mut cells = Cells::empty(Rect::new(0, 0, 4, 1));
        for x in 0..4 {
            cells[(x, 0)].set_char('x').set_style(palette.text());
        }
        for x in 1..3 {
            let style = cells[(x, 0)].style().patch(palette.underline(Role::Error));
            cells[(x, 0)].set_style(style);
        }
        cells
    }

    #[test]
    fn a_link_in_a_card_reaches_the_terminal_one_closed_cell_at_a_time() {
        use ratatui::widgets::Widget as _;

        use crate::popover::{Popover, Run};

        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let mut link = Run::new("ab", Role::Accent);
        link.link = Some(0);
        let body = vec![vec![Run::new("x ", Role::Text), link, Run::new(" y", Role::Text)]];
        let links = vec!["https://e.x/".to_string()];
        let area = Rect::new(0, 0, 10, 1);
        let before = Cells::empty(area);
        let mut after = Cells::empty(area);
        Popover::new(&body, &palette).links(&links).hyperlinks(true).render(area, &mut after);

        let mut backend = NunBackend::new(Vec::new(), Underlines::PLAIN);
        backend.draw(before.diff(&after).into_iter()).unwrap();
        let out = String::from_utf8(backend.writer().clone()).unwrap();
        let open = "\x1b]8;id=nun-0;https://e.x/\x1b\\";
        let close = "\x1b]8;;\x1b\\";
        assert!(out.contains(&format!("{open}ab")), "one link for the run: {out:?}");
        assert_eq!(out.matches(open).count(), 1, "{out:?}");
        assert_eq!(out.matches(close).count(), 1, "{out:?}");
        assert!(out.find(close) < out.find(" y"), "closed before the text after: {out:?}");
        // The text after the link follows straight on: the terminal's cursor
        // moved one column per cell, escapes and all.
        let after_link = out.rfind(close).unwrap() + close.len();
        assert!(!out[after_link..].contains("\x1b[1;"), "no jump needed: {out:?}");
        assert!(out[after_link..].contains(" y"), "{out:?}");
    }

    #[test]
    fn a_link_is_closed_before_the_cursor_jumps_away() {
        let mut cells = Cells::empty(Rect::new(0, 0, 4, 2));
        let open = "\x1b]8;id=nun-0;https://e.x/\x1b\\";
        // The width said outright, as the card does: measured, the escapes
        // would count as text.
        let one = ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN);
        cells[(3, 0)].set_symbol(&format!("{open}a{LINK_CLOSE}")).set_diff_option(one);
        cells[(0, 1)].set_symbol(&format!("{open}b{LINK_CLOSE}")).set_diff_option(one);
        let before = Cells::empty(cells.area);
        let mut backend = NunBackend::new(Vec::new(), Underlines::PLAIN);
        backend.draw(before.diff(&cells).into_iter()).unwrap();
        let out = String::from_utf8(backend.writer().clone()).unwrap();
        let jump = out.find("\x1b[2;1H").unwrap_or_else(|| panic!("no jump: {out:?}"));
        assert!(out[..jump].ends_with(LINK_CLOSE), "{out:?}");
        assert!(out.ends_with(&format!("b{LINK_CLOSE}\x1b[39m\x1b[49m\x1b[0m")), "{out:?}");
    }

    #[test]
    fn a_terminal_with_the_curl_gets_it_in_colour_and_it_ends_where_the_mark_does() {
        let out = bytes(Underlines::FULL, &marked());
        let curl = out.find("\x1b[4:3m").expect("the curl is written");
        let colour = out.find("\x1b[58:2::").expect("the colour is written, colon form");
        let off = out.find("\x1b[24m").expect("the line stops after the mark");
        assert!(curl < colour && colour < off, "{out:?}");
        assert_eq!(out.matches("\x1b[4:3m").count(), 1, "set once for the run: {out:?}");
        assert!(!out.contains("\x1b[4m"), "no straight line anywhere: {out:?}");
        assert!(!out.contains("58;"), "never the semicolon form: {out:?}");
    }

    #[test]
    fn without_the_curl_the_line_is_straight_and_still_coloured_where_colour_is_known() {
        let out = bytes(Underlines { curly: false, colour: true }, &marked());
        assert!(out.contains("\x1b[4m"), "{out:?}");
        assert!(!out.contains("4:3"), "{out:?}");
        assert!(out.contains("\x1b[58:2::"), "{out:?}");
    }

    #[test]
    fn a_terminal_with_neither_is_never_sent_the_curl_or_the_colour() {
        let out = bytes(Underlines::PLAIN, &marked());
        assert!(out.contains("\x1b[4m"), "a plain underline is still drawn: {out:?}");
        assert!(!out.contains("4:3"), "{out:?}");
        assert!(!out.contains("58"), "{out:?}");
        assert!(!out.contains("59"), "not even the reset of it: {out:?}");
    }

    #[test]
    fn a_curl_straight_after_a_plain_underline_is_still_written() {
        // Two underlined cells in a row are no change of modifier, so a diff
        // of modifiers alone would leave the second one straight.
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let mut cells = Cells::empty(Rect::new(0, 0, 2, 1));
        cells[(0, 0)].set_char('a').set_style(palette.text().add_modifier(Modifier::UNDERLINED));
        cells[(1, 0)].set_char('b').set_style(palette.text().patch(palette.underline(Role::Warn)));
        let out = bytes(Underlines::FULL, &cells);
        let straight = out.find("\x1b[4m").expect("{out:?}");
        let curl = out.find("\x1b[4:3m").expect("{out:?}");
        assert!(straight < curl, "{out:?}");
    }

    #[test]
    fn other_modifiers_still_come_and_go() {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let mut cells = Cells::empty(Rect::new(0, 0, 2, 1));
        cells[(0, 0)].set_char('a').set_style(palette.text().add_modifier(Modifier::BOLD));
        cells[(1, 0)].set_char('b').set_style(palette.text());
        let out = bytes(Underlines::FULL, &cells);
        assert!(out.contains("\x1b[1m") && out.contains("\x1b[22m"), "{out:?}");
    }
}
