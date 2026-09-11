//! Drawing a buffer.

use nun_core::Buffer;
use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// Space between the gutter digits and the text.
const GUTTER_PADDING: u16 = 2;

/// One buffer, drawn into a rectangle.
#[derive(Debug)]
pub struct EditorView<'a> {
    buffer: &'a Buffer,
    palette: &'a Palette,
    scroll: usize,
}

impl<'a> EditorView<'a> {
    /// A view of `buffer`, scrolled so `scroll` is the top visible line.
    #[must_use]
    pub const fn new(buffer: &'a Buffer, palette: &'a Palette) -> Self {
        Self { buffer, palette, scroll: 0 }
    }

    /// Set the first visible line.
    #[must_use]
    pub const fn scrolled_to(mut self, line: usize) -> Self {
        self.scroll = line;
        self
    }

    /// Columns the line-number gutter needs for this buffer.
    #[must_use]
    pub fn gutter_width(&self) -> u16 {
        let digits = self.buffer.len_lines().to_string().len();
        u16::try_from(digits).unwrap_or(u16::MAX).saturating_add(GUTTER_PADDING)
    }
}

impl Widget for EditorView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let ground = self.palette.text();
        // Paint the ground first: an unstyled cell shows the host terminal's
        // own background through, which is not necessarily nun's.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(ground);
            }
        }

        let gutter = self.gutter_width();
        if area.width <= gutter {
            return;
        }

        let caret = self.buffer.selections().primary().head;
        let caret_line = self.buffer.line_of(caret);
        let last_line = self.buffer.len_lines();

        for (row, line) in (self.scroll..last_line).take(area.height as usize).enumerate() {
            let Ok(row) = u16::try_from(row) else { continue };
            let y = area.top() + row;
            let is_caret_line = line == caret_line;

            if is_caret_line {
                for x in area.left()..area.right() {
                    cells[(x, y)].set_style(self.palette.cursor_line());
                }
            }

            self.draw_gutter(cells, area, y, line, is_caret_line);
            self.draw_line(cells, area, y, line, gutter, caret);
        }
    }
}

impl EditorView<'_> {
    fn draw_gutter(&self, cells: &mut Cells, area: Rect, y: u16, line: usize, current: bool) {
        let gutter = self.gutter_width();
        let label = (line + 1).to_string();
        let style = self.palette.gutter(current);

        // Right-aligned, one column of breathing room before the text.
        let Ok(label_width) = u16::try_from(label.len()) else { return };
        let Some(indent) = gutter.checked_sub(label_width + GUTTER_PADDING) else { return };

        for (offset, ch) in label.chars().enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let x = area.left() + indent + offset;
            if x >= area.right() {
                break;
            }
            cells[(x, y)].set_char(ch).set_style(style);
        }
    }

    fn draw_line(
        &self,
        cells: &mut Cells,
        area: Rect,
        y: u16,
        line: usize,
        gutter: u16,
        caret: usize,
    ) {
        let text = self.buffer.line_text(line);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let line_start = self.buffer.line_start(line);
        let tab_width = self.buffer.tab_width();
        let selections = self.buffer.selections();

        let mut x = area.left() + gutter;
        let mut column = 0usize;
        let mut char_index = line_start;

        for cluster in text.graphemes(true) {
            if x >= area.right() {
                break;
            }

            let width = if cluster == "\t" {
                tab_width - (column % tab_width)
            } else {
                cluster.width().max(1)
            };

            let selected = selections
                .ranges()
                .iter()
                .any(|range| char_index >= range.from() && char_index < range.to());

            let mut style = self.palette.text();
            if line == self.buffer.line_of(caret) {
                style = style.patch(self.palette.cursor_line());
            }
            if selected {
                style = style.patch(self.palette.selection());
            }
            if char_index == caret {
                style = self.palette.on(Role::Accent, Role::OnAccent);
            }

            let symbol = if cluster == "\t" { " " } else { cluster };
            cells[(x, y)].set_symbol(symbol).set_style(style);

            // A double-width cluster owns the cell after it; ratatui expects
            // that cell to carry an empty symbol rather than a stale one.
            for offset in 1..width {
                let Ok(offset) = u16::try_from(offset) else { break };
                let next = x + offset;
                if next >= area.right() {
                    break;
                }
                cells[(next, y)].set_symbol(" ").set_style(style);
            }

            let Ok(step) = u16::try_from(width) else { break };
            x += step;
            column += width;
            char_index += cluster.chars().count();
        }

        // The caret may sit one past the last character on the line.
        if char_index == caret && x < area.right() {
            cells[(x, y)].set_symbol(" ").set_style(self.palette.on(Role::Accent, Role::OnAccent));
        }
    }
}
