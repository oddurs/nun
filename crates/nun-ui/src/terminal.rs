//! A terminal's screen, drawn.
//!
//! The emulator says what is in each cell; this puts it on screen in the
//! colours the program asked for (see [`Palette::content`]), with the
//! selection washed over it, the link under the pointer underlined, and the
//! cursor where the program left it — drawn only while the terminal has the
//! keyboard, so which terminal is being typed into is never in doubt.

use nun_term::{Emulator, Width};
use nun_theme::Role;
use ratatui::buffer::{Buffer as Cells, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

use crate::style::Palette;

/// What stands in for a character whose width the emulator and the screen
/// disagree about even on its own: drawn as it is, it would push the rest of
/// its row along.
const MISMATCH: &str = "\u{fffd}";

/// One terminal's screen.
#[derive(Debug)]
pub struct TerminalView<'a> {
    emulator: &'a Emulator,
    palette: &'a Palette,
    focused: bool,
    link: Option<(u16, u16, u16)>,
}

impl<'a> TerminalView<'a> {
    /// The screen of `emulator`.
    #[must_use]
    pub const fn new(emulator: &'a Emulator, palette: &'a Palette) -> Self {
        Self { emulator, palette, focused: false, link: None }
    }

    /// Whether it has the keyboard, which is when its cursor is drawn.
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// A link under the pointer, to underline: its row, and the columns from
    /// its first to one past its last.
    #[must_use]
    pub const fn link(mut self, link: Option<(u16, u16, u16)>) -> Self {
        self.link = link;
        self
    }
}

impl Widget for TerminalView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        let ground = self.palette.text();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].reset();
                cells[(x, y)].set_symbol(" ").set_style(ground);
            }
        }
        let cursor = self.emulator.cursor().filter(|_| self.focused);
        let mut symbol = String::new();
        self.emulator.visit(|row, col, cell| {
            if row >= area.height || col >= area.width {
                return;
            }
            let (x, y) = (area.x + col, area.y + row);
            let mut style = self.palette.content(cell.fg, cell.bg, cell.attrs);
            if cell.selected {
                style = style.patch(self.palette.selection());
            }
            if self.link.is_some_and(|(at, start, end)| at == row && (start..end).contains(&col)) {
                style =
                    style.patch(self.palette.ink(Role::Accent)).add_modifier(Modifier::UNDERLINED);
            }
            if cursor == Some((row, col)) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let wanted = match cell.width {
                // Covered by the wide character before it, which ratatui's
                // diff already skips; only the style is worth keeping.
                Width::Spacer => {
                    cells[(x, y)].set_style(style);
                    return;
                }
                Width::Single => 1,
                Width::Wide => 2,
            };
            symbol.clear();
            symbol.push(cell.ch);
            symbol.extend(cell.marks);
            let fits = wanted == 1 || x + 1 < area.right();
            if !fits {
                cells[(x, y)].set_symbol(" ").set_style(style);
                return;
            }
            if symbol.as_str().cell_width() != wanted {
                // The emulator measures the character and the screen the
                // cluster: `❤` with the emoji selector is one column to the
                // one and two to the other. The character alone is what the
                // grid made room for, so it is drawn as text; only a
                // character that does not fit even then is replaced.
                symbol.clear();
                symbol.push(cell.ch);
                if symbol.as_str().cell_width() != wanted {
                    symbol.clear();
                    symbol.push_str(MISMATCH);
                }
            }
            cells[(x, y)].set_symbol(&symbol).set_style(style);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nun_term::{Answers, Size};
    use nun_theme::{Probe, derive};
    use ratatui::style::Color;

    const ANSWERS: Answers = Answers { foreground: (255, 255, 255), background: (0, 0, 0) };

    fn draw(bytes: &[u8], focused: bool) -> Cells {
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let mut emulator = Emulator::new(Size::new(10, 2), 10);
        emulator.feed(bytes, ANSWERS);
        let area = Rect::new(0, 0, 10, 2);
        let mut cells = Cells::empty(area);
        TerminalView::new(&emulator, &palette).focused(focused).render(area, &mut cells);
        cells
    }

    #[test]
    fn the_programs_colours_reach_the_screen() {
        let cells = draw(b"\x1b[32mok\x1b[0m", false);
        assert_eq!(cells[(0, 0)].symbol(), "o");
        assert_eq!(cells[(0, 0)].fg, Color::Indexed(2));
        assert_ne!(cells[(2, 0)].fg, Color::Indexed(2));
    }

    #[test]
    fn a_wide_character_takes_its_two_cells() {
        let cells = draw("a日b".as_bytes(), false);
        assert_eq!(cells[(1, 0)].symbol(), "日");
        assert_eq!(cells[(3, 0)].symbol(), "b");
    }

    #[test]
    fn an_emoji_the_program_wrote_as_one_column_stays_one_column() {
        let cells = draw("\u{2764}\u{fe0f}!".as_bytes(), false);
        assert_eq!(cells[(0, 0)].symbol(), "\u{2764}", "drawn as text, not dropped");
        assert_eq!(cells[(1, 0)].symbol(), "!", "the rest of the row did not move");
    }

    #[test]
    fn the_cursor_shows_only_with_the_keyboard() {
        let focused = draw(b"$ ", true);
        assert!(focused[(2, 0)].modifier.contains(Modifier::REVERSED));
        let unfocused = draw(b"$ ", false);
        assert!(!unfocused[(2, 0)].modifier.contains(Modifier::REVERSED));
    }
}
