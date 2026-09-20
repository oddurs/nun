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
    marker: Option<usize>,
    highlights: &'a [nun_syntax::Span],
}

impl<'a> EditorView<'a> {
    /// A view of `buffer`, scrolled so `scroll` is the top visible line.
    #[must_use]
    pub const fn new(buffer: &'a Buffer, palette: &'a Palette) -> Self {
        Self { buffer, palette, scroll: 0, marker: None, highlights: &[] }
    }

    /// Set the first visible line.
    #[must_use]
    pub const fn scrolled_to(mut self, line: usize) -> Self {
        self.scroll = line;
        self
    }

    /// Colour the text with these highlight runs, which must be in order and
    /// must not overlap — which is what [`nun_syntax`] hands back.
    #[must_use]
    pub const fn highlighted(mut self, spans: &'a [nun_syntax::Span]) -> Self {
        self.highlights = spans;
        self
    }

    /// Mark where dragged text would land if it were dropped now.
    #[must_use]
    pub const fn with_drop_marker(mut self, at: Option<usize>) -> Self {
        self.marker = at;
        self
    }

    /// Columns the line-number gutter needs for this buffer.
    #[must_use]
    pub fn gutter_width(&self) -> u16 {
        let digits = self.buffer.len_lines().to_string().len();
        u16::try_from(digits).unwrap_or(u16::MAX).saturating_add(GUTTER_PADDING)
    }

    /// The buffer position drawn at cell `(column, row)` of `area`.
    ///
    /// Uses the same walk over grapheme clusters and display widths as drawing,
    /// so a click lands on exactly the character painted under it: past a wide
    /// character rather than inside it, on the right side of an expanded tab,
    /// and at the end of the line for a click beyond its last character. A row
    /// below the last line resolves to the last line.
    ///
    /// Returns `None` for the gutter and for anything outside `area`.
    #[must_use]
    pub fn position_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let gutter = self.gutter_width();
        if column < area.left().saturating_add(gutter)
            || column >= area.right()
            || row < area.top()
            || row >= area.bottom()
        {
            return None;
        }

        let last = self.buffer.len_lines().saturating_sub(1);
        let line = (self.scroll + usize::from(row - area.top())).min(last);
        let target = usize::from(column - area.left() - gutter);

        let text = self.buffer.line_text(line);
        let mut end = self.buffer.line_start(line);
        for cell in self.cells_of(line, &text) {
            if cell.column + cell.width > target {
                return Some(cell.char_index);
            }
            end = cell.char_index + cell.chars;
        }
        Some(end)
    }

    /// The grapheme clusters of `line`, whose text is `text`, as they are
    /// laid out on screen.
    ///
    /// Drawing and [`EditorView::position_at`] both walk this, which is what
    /// stops a click and the glyph under it from ever disagreeing.
    fn cells_of<'t>(&self, line: usize, text: &'t str) -> impl Iterator<Item = LaidOut<'t>> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let tab_width = self.buffer.tab_width();

        let mut column = 0usize;
        let mut char_index = self.buffer.line_start(line);
        text.graphemes(true).map(move |cluster| {
            let width = if cluster == "\t" {
                tab_width - (column % tab_width)
            } else {
                cluster.width().max(1)
            };
            let chars = cluster.chars().count();
            let laid = LaidOut { cluster, column, width, char_index, chars };
            column += width;
            char_index += chars;
            laid
        })
    }
}

/// One grapheme cluster, placed.
struct LaidOut<'t> {
    cluster: &'t str,
    /// Display column from the start of the line.
    column: usize,
    /// Cells it occupies.
    width: usize,
    /// Char index of its first char.
    char_index: usize,
    /// Chars it is made of.
    chars: usize,
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
        // Every caret is drawn, not just the primary: with several, the ones
        // that are not drawn are the ones that surprise you when you type.
        let mut carets: Vec<usize> =
            self.buffer.selections().ranges().iter().map(|range| range.head).collect();
        carets.extend(self.marker);
        carets.sort_unstable();
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
            self.draw_line(cells, area, y, line, gutter, &carets);
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
        carets: &[usize],
    ) {
        let is_caret = |at: usize| carets.binary_search(&at).is_ok();
        let caret_line = self.buffer.line_of(self.buffer.selections().primary().head);
        let selections = self.buffer.selections();
        let text = self.buffer.line_text(line);

        // The runs are in order, so drawing walks them rather than searching:
        // find the first one that reaches this line and step along with the
        // clusters.
        let line_start = u32::try_from(self.buffer.line_start(line)).unwrap_or(u32::MAX);
        let mut run = self.highlights.partition_point(|span| span.end <= line_start);

        let mut x = area.left() + gutter;
        let mut char_index = self.buffer.line_start(line);

        for LaidOut { cluster, width, char_index: at, chars, .. } in self.cells_of(line, &text) {
            if x >= area.right() {
                break;
            }
            char_index = at;

            let selected = selections
                .ranges()
                .iter()
                .any(|range| char_index >= range.from() && char_index < range.to());

            let mut style = self.palette.text();

            // Syntax first, so the caret line, the selection and the caret
            // itself all wash over it rather than under it.
            let at = u32::try_from(char_index).unwrap_or(u32::MAX);
            while self.highlights.get(run).is_some_and(|span| span.end <= at) {
                run += 1;
            }
            if let Some(span) = self.highlights.get(run)
                && span.start <= at
            {
                style = style.patch(self.palette.ink(crate::syntax::role_of(span.capture)));
            }

            if line == caret_line {
                style = style.patch(self.palette.cursor_line());
            }
            if selected {
                style = style.patch(self.palette.selection());
            }
            if is_caret(char_index) {
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
            char_index += chars;
        }

        // The caret may sit one past the last character on the line.
        if is_caret(char_index) && x < area.right() {
            cells[(x, y)].set_symbol(" ").set_style(self.palette.on(Role::Accent, Role::OnAccent));
        }
    }
}
