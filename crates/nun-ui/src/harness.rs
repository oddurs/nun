//! Rendering a frame with no terminal attached.
//!
//! A UI that can only be checked by looking at it will not stay correct, and
//! none of it is testable in CI without this. Frames render into an in-memory
//! cell grid; assertions are made against the text and the styles, and damage
//! is measured as the diff between two frames.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

/// A fixed-size screen in memory.
#[derive(Debug)]
pub struct Harness {
    terminal: Terminal<TestBackend>,
}

impl Harness {
    /// A screen `width` by `height` cells.
    ///
    /// # Panics
    ///
    /// If the test backend cannot be created, which does not happen in practice.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let terminal = Terminal::new(TestBackend::new(width, height))
            .expect("the in-memory backend cannot fail");
        Self { terminal }
    }

    /// Draw a widget over the whole screen.
    ///
    /// # Panics
    ///
    /// If drawing fails, which the in-memory backend does not do.
    pub fn draw(&mut self, widget: impl Widget) {
        self.terminal
            .draw(|frame| frame.render_widget(widget, frame.area()))
            .expect("the in-memory backend cannot fail");
    }

    /// The cells as they currently stand.
    #[must_use]
    pub fn cells(&self) -> &Cells {
        self.terminal.backend().buffer()
    }

    /// The screen area.
    #[must_use]
    pub fn area(&self) -> Rect {
        *self.cells().area()
    }

    /// The visible text, one line per row, trailing spaces trimmed.
    ///
    /// A double-width character covers the cell after it, which the terminal
    /// never draws on its own; it is skipped here too, so `日本` reads as
    /// `日本` and not with a space inside each character.
    #[must_use]
    pub fn to_text(&self) -> String {
        let cells = self.cells();
        let area = cells.area();
        (0..area.height)
            .map(|y| {
                let mut row = String::new();
                for x in visible_columns(cells, area.width, y) {
                    row.push_str(cells[(x, y)].symbol());
                }
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The cells a terminal would actually draw on row `y`: every column,
    /// except those covered by the double-width character before them.
    #[must_use]
    pub fn visible_cells(&self, y: u16) -> Vec<u16> {
        visible_columns(self.cells(), self.area().width, y)
    }

    /// The text with a parallel grid of one-character style keys beneath it.
    ///
    /// Snapshots capture style, not just characters, so a change that silently
    /// drops a colour is a visible diff rather than an invisible one.
    #[must_use]
    pub fn to_styled_text(&self) -> String {
        let cells = self.cells();
        let area = cells.area();

        // Assign a stable key per distinct style, in first-seen order.
        let mut seen: Vec<(Option<ratatui::style::Color>, Option<ratatui::style::Color>)> =
            Vec::new();
        let mut out = String::new();

        for y in 0..area.height {
            let mut text = String::new();
            let mut keys = String::new();
            for x in 0..area.width {
                let cell = &cells[(x, y)];
                text.push_str(cell.symbol());
                let style = (cell.fg.into(), cell.bg.into());
                let index = seen.iter().position(|s| *s == style).unwrap_or_else(|| {
                    seen.push(style);
                    seen.len() - 1
                });
                // Beyond 26 distinct styles the key repeats; a frame with that
                // many is not a useful snapshot anyway.
                keys.push(char::from(b'a' + u8::try_from(index % 26).unwrap_or(0)));
            }
            out.push_str(text.trim_end());
            out.push('\n');
            out.push_str(keys.trim_end());
            out.push('\n');
        }
        out
    }

    /// A snapshot of the cells, for comparing against a later frame.
    #[must_use]
    pub fn snapshot(&self) -> Cells {
        self.cells().clone()
    }
}

fn visible_columns(cells: &Cells, width: u16, y: u16) -> Vec<u16> {
    use unicode_width::UnicodeWidthStr;
    let mut columns = Vec::new();
    let mut x = 0;
    while x < width {
        columns.push(x);
        let step = u16::try_from(cells[(x, y)].symbol().width().max(1)).unwrap_or(1);
        x = x.saturating_add(step);
    }
    columns
}

/// Rows that differ between two frames.
///
/// This is what "damage tracking" means in practice: ratatui writes only the
/// cells that changed, so asserting on the diff is asserting on exactly what
/// would go down the wire.
#[must_use]
pub fn changed_rows(before: &Cells, after: &Cells) -> Vec<u16> {
    let mut rows: Vec<u16> = before.diff(after).into_iter().map(|(_, y, _)| y).collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// Cells that differ between two frames, as `(x, y)` pairs.
#[must_use]
pub fn changed_cells(before: &Cells, after: &Cells) -> Vec<(u16, u16)> {
    let mut cells: Vec<(u16, u16)> =
        before.diff(after).into_iter().map(|(x, y, _)| (x, y)).collect();
    cells.sort_unstable();
    cells
}
