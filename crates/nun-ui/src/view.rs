//! Drawing a buffer.

use nun_core::Buffer;
use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::changes::Change;
use crate::glyph::Glyph;
use crate::marks::Mark;
use crate::style::Palette;

/// Columns between the gutter digits and the text: the change bar, the fold
/// arrows, and the code-action mark.
///
/// The change bar's column is there whether or not git follows the file, so
/// the text never moves sideways when git answers, or when a file is staged
/// clean.
const GUTTER_PADDING: u16 = 3;

/// Where one of a live snippet's tab-stops is, in char indices, and whether
/// it is the one being edited. An empty stop marks the one cell at `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    /// Char index where it begins.
    pub start: usize,
    /// Char index where it ends.
    pub end: usize,
    /// Whether it is the stop being edited, or one of its mirrors.
    pub current: bool,
}

/// One buffer, drawn into a rectangle.
#[derive(Debug)]
pub struct EditorView<'a> {
    buffer: &'a Buffer,
    palette: &'a Palette,
    scroll: usize,
    marker: Option<usize>,
    highlights: &'a [nun_syntax::Span],
    foldable: &'a [nun_syntax::FoldRange],
    marks: &'a [Mark],
    stops: &'a [Stop],
    lightbulb: Option<usize>,
    changes: &'a [(usize, Change)],
}

impl<'a> EditorView<'a> {
    /// A view of `buffer`, scrolled so `scroll` is the top visible line.
    #[must_use]
    pub const fn new(buffer: &'a Buffer, palette: &'a Palette) -> Self {
        Self {
            buffer,
            palette,
            scroll: 0,
            marker: None,
            highlights: &[],
            foldable: &[],
            marks: &[],
            stops: &[],
            lightbulb: None,
            changes: &[],
        }
    }

    /// Underline these diagnostics, each in the colour of its severity. They
    /// may be in any order and may overlap; where they do, the worst wins.
    #[must_use]
    pub const fn marked(mut self, marks: &'a [Mark]) -> Self {
        self.marks = marks;
        self
    }

    /// Wash these snippet tab-stops, the current one more strongly. They may
    /// be in any order; where they overlap, the current one wins.
    #[must_use]
    pub const fn with_stops(mut self, stops: &'a [Stop]) -> Self {
        self.stops = stops;
        self
    }

    /// Mark `line` in the gutter as having code actions, in the column
    /// between the fold arrows and the text.
    #[must_use]
    pub const fn with_lightbulb(mut self, line: Option<usize>) -> Self {
        self.lightbulb = line;
        self
    }

    /// Mark these lines in the gutter as changed since git last had them, in
    /// the column just after the line numbers. In order of line; a line may
    /// be given twice — the last can have lines removed both above and
    /// below it — and the first given is drawn.
    #[must_use]
    pub const fn with_changes(mut self, changes: &'a [(usize, Change)]) -> Self {
        self.changes = changes;
        self
    }

    /// Put an arrow in the gutter beside every region that can be folded.
    /// `ranges` are in order of their header lines, as [`nun_syntax`] gives
    /// them.
    #[must_use]
    pub const fn foldable(mut self, ranges: &'a [nun_syntax::FoldRange]) -> Self {
        self.foldable = ranges;
        self
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

    /// The gutter column the fold arrows are drawn in, counted from the left
    /// edge of the view: the first column after the line numbers.
    #[must_use]
    pub fn arrow_column(&self) -> u16 {
        self.gutter_width() - 2
    }

    /// The gutter column the change bar is drawn in, counted from the left
    /// edge of the view: the first after the line numbers.
    #[must_use]
    pub fn change_column(&self) -> u16 {
        self.gutter_width() - GUTTER_PADDING
    }

    /// The gutter column the code-action mark is drawn in, counted from the
    /// left edge of the view: the last one, just before the text.
    #[must_use]
    pub fn lightbulb_column(&self) -> u16 {
        self.gutter_width() - 1
    }

    /// The line drawn on row `row` of the view, counted from its top, or
    /// `None` below the last line. Folded lines take no rows.
    #[must_use]
    pub fn line_at_row(&self, row: usize) -> Option<usize> {
        self.buffer.hidden().from(self.scroll).nth(row)
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

        let hidden = self.buffer.hidden();
        let line = hidden.step(self.scroll, isize::try_from(row - area.top()).unwrap_or(0));
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

    /// The cells the chars `from..to` are drawn in, one rectangle per row in
    /// view, for `area` as it would be drawn. An empty range is the one
    /// character at `from`; a range reaching a line's end includes the cell
    /// after its last character, where an underline there is drawn.
    ///
    /// What a hover over a diagnostic is resolved against, and what a card
    /// about it is kept clear of.
    #[must_use]
    pub fn screen_rects(&self, area: Rect, from: usize, to: usize) -> Vec<Rect> {
        let gutter = usize::from(self.gutter_width());
        let width = usize::from(area.width);
        let mut rects = Vec::new();
        for (row, line) in
            self.buffer.hidden().from(self.scroll).take(area.height.into()).enumerate()
        {
            let text = self.buffer.line_text(line);
            let mut span: Option<(usize, usize)> = None;
            let mut eol = (self.buffer.line_start(line), 0);
            for cell in self.cells_of(line, &text) {
                if overlaps(from, to, cell.char_index, cell.char_index + cell.chars) {
                    let start = span.map_or(cell.column, |(start, _)| start);
                    span = Some((start, cell.column + cell.width));
                }
                eol = (cell.char_index + cell.chars, cell.column + cell.width);
            }
            if overlaps(from, to, eol.0, eol.0 + 1) {
                let start = span.map_or(eol.1, |(start, _)| start);
                span = Some((start, eol.1 + 1));
            }
            let Some((start, end)) = span else { continue };
            let (start, end) = ((gutter + start).min(width), (gutter + end).min(width));
            if start >= end {
                continue;
            }
            let (Ok(x), Ok(w), Ok(y)) =
                (u16::try_from(start), u16::try_from(end - start), u16::try_from(row))
            else {
                continue;
            };
            rects.push(Rect::new(area.x + x, area.y + y, w, 1));
        }
        rects
    }

    /// The cell of `area` that char index `at` is drawn in: the inverse of
    /// [`EditorView::position_at`], for putting something beside the text.
    ///
    /// `None` when its line is scrolled out of view or folded away, or it is
    /// past the right edge.
    #[must_use]
    pub fn cell_of(&self, area: Rect, at: usize) -> Option<(u16, u16)> {
        let at = at.min(self.buffer.len_chars());
        let line = self.buffer.line_of(at);
        let row = self
            .buffer
            .hidden()
            .from(self.scroll)
            .take(usize::from(area.height))
            .position(|l| l == line)?;
        let text = self.buffer.line_text(line);
        let mut column = 0;
        for cell in self.cells_of(line, &text) {
            if cell.char_index >= at {
                break;
            }
            column = cell.column + cell.width;
        }
        let x = usize::from(area.left() + self.gutter_width()) + column;
        let x = u16::try_from(x).ok().filter(|x| *x < area.right())?;
        Some((x, area.top() + u16::try_from(row).ok()?))
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

/// Whether a mark over `start..end` touches the chars `from..to`. An empty
/// mark is the one character at its start.
const fn overlaps(start: usize, end: usize, from: usize, to: usize) -> bool {
    let end = if end > start { end } else { start + 1 };
    start < to && end > from
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
        // Worked out once for the frame. Folded lines take no rows, so rows
        // are walked through the lines in view rather than counted.
        let hidden = self.buffer.hidden();
        let folded: Vec<usize> =
            self.buffer.folded().into_iter().map(|(header, _)| header).collect();

        for (row, line) in hidden.from(self.scroll).take(area.height as usize).enumerate() {
            let Ok(row) = u16::try_from(row) else { continue };
            let y = area.top() + row;
            let is_caret_line = line == caret_line;

            if is_caret_line {
                for x in area.left()..area.right() {
                    cells[(x, y)].set_style(self.palette.cursor_line());
                }
            }

            self.draw_gutter(cells, area, y, line, is_caret_line);
            let folded_here = folded.binary_search(&line).is_ok();
            self.draw_change(cells, area, y, line);
            self.draw_arrow(cells, area, y, line, folded_here);
            if self.lightbulb == Some(line) {
                let x = area.left() + self.lightbulb_column();
                cells[(x, y)]
                    .set_symbol(self.palette.glyph(Glyph::Lightbulb))
                    .set_style(self.palette.fg(Role::Accent));
            }
            let end = self.draw_line(cells, area, y, line, gutter, &carets);
            if folded_here {
                self.draw_fold_marker(cells, area, y, end);
            }
        }
    }
}

impl EditorView<'_> {
    /// The arrow beside a region that can be folded, or has been.
    ///
    /// Only on lines that open a region the parser found: an arrow beside
    /// every line would say nothing. A folded one points at its text and
    /// takes the accent, since it is standing in for lines that are not there.
    fn draw_arrow(&self, cells: &mut Cells, area: Rect, y: u16, line: usize, folded: bool) {
        let x = area.left() + self.arrow_column();
        if x >= area.right() {
            return;
        }
        let foldable = self
            .foldable
            .binary_search_by_key(&u32::try_from(line).unwrap_or(u32::MAX), |range| range.header)
            .is_ok();
        let (glyph, role) = match (folded, foldable) {
            (true, _) => (Glyph::FoldClosed, Role::Accent),
            (false, true) => (Glyph::FoldOpen, Role::Faint),
            (false, false) => return,
        };
        cells[(x, y)].set_symbol(self.palette.glyph(glyph)).set_style(self.palette.fg(role));
    }

    /// The bar beside a changed line.
    fn draw_change(&self, cells: &mut Cells, area: Rect, y: u16, line: usize) {
        let at = self.changes.partition_point(|(changed, _)| *changed < line);
        let Some(&(changed, change)) = self.changes.get(at) else { return };
        let x = area.left() + self.change_column();
        if changed != line || x >= area.right() {
            return;
        }
        cells[(x, y)]
            .set_symbol(self.palette.glyph(change.glyph()))
            .set_style(self.palette.fg(change.role()));
    }

    /// The marker after a folded header's text, standing in for what is
    /// hidden: a small raised chip, one space after the line.
    fn draw_fold_marker(&self, cells: &mut Cells, area: Rect, y: u16, after: u16) {
        let style = self.palette.on(Role::Raised, Role::Dim);
        for (offset, symbol) in
            [" ", self.palette.glyph(Glyph::FoldHidden), " "].into_iter().enumerate()
        {
            let Ok(offset) = u16::try_from(offset + 1) else { break };
            let x = after.saturating_add(offset);
            if x >= area.right() {
                break;
            }
            cells[(x, y)].set_symbol(symbol).set_style(style);
        }
    }

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

    /// The tab-stop wash for the chars `from..to`, for a line spanning
    /// `line_from..line_to`: the stops that reach the line are found once,
    /// rather than per cell.
    fn stop_wash(
        &self,
        line_from: usize,
        line_to: usize,
    ) -> impl Fn(usize, usize) -> Option<ratatui::style::Style> + '_ {
        let stops: Vec<&Stop> = self
            .stops
            .iter()
            .filter(|stop| overlaps(stop.start, stop.end, line_from, line_to))
            .collect();
        move |from, to| {
            stops
                .iter()
                .filter(|stop| overlaps(stop.start, stop.end, from, to))
                .map(|stop| stop.current)
                .max()
                .map(|current| self.palette.tabstop(current))
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
    ) -> u16 {
        let is_caret = |at: usize| carets.binary_search(&at).is_ok();
        let primary = self.buffer.selections().primary().head;
        let caret_line = self.buffer.line_of(primary);
        // With several carets the one being driven has to be findable, or
        // every key press is a guess about where the text will appear. The
        // others are still carets and still solid; they are just not the one
        // the arrows move.
        // The drop marker is not a caret at all — it is where text is about to
        // land — so it keeps the loud treatment rather than being mistaken for
        // one of the quiet ones.
        let caret_style = |at: usize| {
            if at == primary || Some(at) == self.marker {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::LineStrong, Role::Text)
            }
        };
        let selections = self.buffer.selections();
        let text = self.buffer.line_text(line);

        // The runs are in order, so drawing walks them rather than searching:
        // find the first one that reaches this line and step along with the
        // clusters.
        let line_start = u32::try_from(self.buffer.line_start(line)).unwrap_or(u32::MAX);
        let mut run = self.highlights.partition_point(|span| span.end <= line_start);

        // Selections are sorted and disjoint, so like the runs they are
        // walked rather than searched: with five hundred of them, asking each
        // one about every cell is most of a frame.
        let ranges = selections.ranges();
        let mut range = ranges.partition_point(|r| r.to() <= self.buffer.line_start(line));

        // The marks that reach this line, found once rather than per cell.
        let line_from = self.buffer.line_start(line);
        let line_to = line_from + text.chars().count();
        let marks: Vec<&Mark> = self
            .marks
            .iter()
            .filter(|mark| overlaps(mark.start, mark.end, line_from, line_to + 1))
            .collect();
        let underline = |from: usize, to: usize| {
            marks
                .iter()
                .filter(|mark| overlaps(mark.start, mark.end, from, to))
                .map(|mark| mark.severity)
                .min()
                .map(|severity| self.palette.underline(severity.role()))
        };

        let wash = self.stop_wash(line_from, line_to + 1);

        let mut x = area.left() + gutter;
        let mut char_index = self.buffer.line_start(line);

        for LaidOut { cluster, width, char_index: at, chars, .. } in self.cells_of(line, &text) {
            if x >= area.right() {
                break;
            }
            char_index = at;

            while ranges.get(range).is_some_and(|r| r.to() <= char_index) {
                range += 1;
            }
            let selected = ranges.get(range).is_some_and(|r| r.from() <= char_index);

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
            if let Some(stop) = wash(char_index, char_index + chars) {
                style = style.patch(stop);
            }
            if selected {
                style = style.patch(self.palette.selection());
            }
            if is_caret(char_index) {
                style = caret_style(char_index);
            }
            // Last, so the caret keeps the underline it is standing on.
            if let Some(line) = underline(char_index, char_index + chars) {
                style = style.patch(line);
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

        // The caret may sit one past the last character on the line, and so
        // may a diagnostic: a missing semicolon is reported there.
        // So may an empty stop, at the end of a line or of the text.
        let past_end = underline(char_index, char_index + 1);
        let stop_past_end = wash(char_index, char_index + 1);
        let marked = past_end.is_some() || stop_past_end.is_some();
        if (is_caret(char_index) || marked) && x < area.right() {
            let mut style = cells[(x, y)].style();
            if let Some(stop) = stop_past_end {
                style = style.patch(stop);
            }
            if is_caret(char_index) {
                style = caret_style(char_index);
            }
            if let Some(line) = past_end {
                style = style.patch(line);
            }
            cells[(x, y)].set_symbol(" ").set_style(style);
        }
        x
    }
}
