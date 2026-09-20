//! The project-search panel.
//!
//! A query, the toggles that change what it means, and the matching lines
//! grouped under the files they came from — a window of them, however many the
//! engine found. Geometry is exposed as associated functions so the binary lays
//! out exactly the hit regions this draws, from one source.
//!
//! Nothing here runs the search. The panel is handed rows and draws them, which
//! is what keeps a slow walk of a large repository off the render path.

use std::ops::Range;

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// Columns a hit is indented under the file it belongs to.
const INDENT: u16 = 2;

/// The panel's name.
///
/// Uppercased in the source rather than at render time: there is nothing here
/// for it to vary with, unlike the tree, whose title is the workspace's name.
const TITLE: &str = "SEARCH";

/// What stands in front of the query.
const PROMPT: &str = "⌕ ";

/// The header's one button, which hands the sidebar back to the file tree.
///
/// A ruled square reads as a listing of rows, which is what the tree is from
/// one cell away, and it pairs with the magnifier the tree shows for coming
/// the other way: two marks for two views, neither of them an arrow that would
/// only say "back" without saying back to what.
const BACK: &str = "▤";

/// Columns [`PROMPT`] occupies. Held as a constant because the query row's
/// geometry must be answerable without measuring a string; a test holds the
/// constant and the string to each other.
const PROMPT_COLS: u16 = 2;

/// What an empty query field says when nobody is typing into it.
const PLACEHOLDER: &str = "Search the project";

/// Rows above the results: the header, the query, the toggles, the summary.
const HEAD_ROWS: u16 = 4;

/// Columns the line-number gutter never shrinks below, so a file whose hits are
/// all on single-digit lines still lines its text up with everything else.
const MIN_GUTTER: u16 = 3;

/// A toggle in the search panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchButton {
    /// Read the query as a regular expression rather than as literal text.
    Regex,
    /// Require the case to match.
    Case,
    /// Require the match to be a whole word.
    Word,
    /// Search files the ignore rules would otherwise skip.
    Ignored,
}

impl SearchButton {
    /// Every toggle, left to right as they sit in the row.
    pub const ALL: [Self; 4] = [Self::Regex, Self::Case, Self::Word, Self::Ignored];

    /// What the toggle shows. One cell wide each, so the row's geometry never
    /// depends on a font.
    ///
    /// Whether the toggle is lit is carried by colour rather than by a second
    /// glyph: four marks that each change shape would make a row of four read
    /// as eight different things.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            // The wildcard out of `.*`, which is the one piece of regex
            // notation that reads as regex outside a regex.
            Self::Regex => "*",
            // A capital letter is the distinction the toggle controls, so the
            // glyph is the thing itself rather than a sign for it.
            Self::Case => "A",
            // A box the match has to fill exactly, which is what a whole-word
            // match is.
            Self::Word => "▭",
            // The same ring the file tree hangs its own ignored toggle on, so
            // one mark means one thing across the whole sidebar.
            Self::Ignored => "○",
        }
    }

    /// What it does, for the status line on hover. `on` is whether it is lit,
    /// so this names the state a click would move to rather than the one it is
    /// already in.
    #[must_use]
    pub const fn describe(self, on: bool) -> &'static str {
        match self {
            Self::Regex if on => "Match literally",
            Self::Regex => "Match as a regular expression",
            Self::Case if on => "Ignore case",
            Self::Case => "Match case",
            Self::Word if on => "Match inside words",
            Self::Word => "Match whole words only",
            Self::Ignored if on => "Skip ignored files",
            Self::Ignored => "Search ignored files",
        }
    }
}

/// Which toggles are lit.
// Four independent switches, and the public shape of them is the four names.
// Packing them into flags to satisfy the lint would cost every caller a
// bitwise expression to say something a field already says.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Toggles {
    /// The query is a regular expression.
    pub regex: bool,
    /// The case has to match.
    pub case: bool,
    /// The match has to be a whole word.
    pub word: bool,
    /// Ignored files are searched too.
    pub ignored: bool,
}

impl Toggles {
    /// Whether one toggle is lit.
    #[must_use]
    pub const fn on(self, button: SearchButton) -> bool {
        match button {
            SearchButton::Regex => self.regex,
            SearchButton::Case => self.case,
            SearchButton::Word => self.word,
            SearchButton::Ignored => self.ignored,
        }
    }
}

/// One row of the results list.
///
/// The list is flat rather than a tree of files holding hits, because that is
/// what scrolling and hit-testing want: one index, one row, whatever it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRow<'a> {
    /// A file that has hits, and whether its hits are hidden.
    File {
        /// How the file is named in the panel, usually relative to the root.
        path: &'a str,
        /// How many hits it has, counting the ones a collapse is hiding.
        hits: usize,
        /// Whether its hits are hidden.
        collapsed: bool,
    },
    /// One matching line, belonging to the `File` row above it.
    Hit {
        /// Which line of the file it is, counting from one.
        line: u32,
        /// The line, possibly a window of a very long one.
        text: &'a str,
        /// Char offsets into `text` that matched, ordered and disjoint.
        matched: &'a [Range<u32>],
    },
}

/// The search panel, drawn.
///
/// `rows` may be the whole result list or only the window of it that is on
/// screen — a search over a large repository finds far more than a sidebar can
/// show, and building a row for every hit on every frame is work thrown away.
/// A caller passing a window scrolls the slice itself and hands this a scroll
/// of zero, with `selected` and `hovered` rebased into it, so nothing here has
/// to know which it was given.
///
/// One thing does: the line-number gutter is as wide as the widest line number
/// needs, and derived from a window it would change width as the window moved
/// over a longer number, stepping the hit text sideways while scrolling. A
/// caller passing a window therefore passes [`SearchView::widest_line`] as
/// well, which is the whole list's answer to a question the window cannot
/// answer for itself.
#[derive(Debug)]
pub struct SearchView<'a> {
    query: &'a str,
    rows: &'a [SearchRow<'a>],
    palette: &'a Palette,
    scroll: usize,
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_button: Option<SearchButton>,
    hovered_back: bool,
    toggles: Toggles,
    focused: bool,
    editing: bool,
    caret: usize,
    summary: Option<&'a str>,
    widest_line: Option<u32>,
}

impl<'a> SearchView<'a> {
    /// A panel showing `rows` for `query`.
    #[must_use]
    pub const fn new(query: &'a str, rows: &'a [SearchRow<'a>], palette: &'a Palette) -> Self {
        Self {
            query,
            rows,
            palette,
            scroll: 0,
            selected: None,
            hovered: None,
            hovered_button: None,
            hovered_back: false,
            toggles: Toggles { regex: false, case: false, word: false, ignored: false },
            focused: false,
            editing: false,
            caret: 0,
            summary: None,
            widest_line: None,
        }
    }

    /// The first result row shown.
    #[must_use]
    pub const fn scrolled_to(mut self, row: usize) -> Self {
        self.scroll = row;
        self
    }

    /// The row the keyboard acts on, drawn with the selection wash.
    #[must_use]
    pub const fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }

    /// The row under the pointer.
    #[must_use]
    pub const fn hovered(mut self, row: Option<usize>) -> Self {
        self.hovered = row;
        self
    }

    /// The toggle under the pointer.
    #[must_use]
    pub const fn hovered_button(mut self, button: Option<SearchButton>) -> Self {
        self.hovered_button = button;
        self
    }

    /// Whether the pointer is on the button that gives the sidebar back to
    /// the file tree.
    #[must_use]
    pub const fn hovered_back(mut self, hovered: bool) -> Self {
        self.hovered_back = hovered;
        self
    }

    /// Which toggles are lit.
    #[must_use]
    pub const fn toggles(mut self, toggles: Toggles) -> Self {
        self.toggles = toggles;
        self
    }

    /// Whether the panel has the keyboard.
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Whether the query field has the keyboard, and where its caret sits, as
    /// a char offset into `query`.
    #[must_use]
    pub const fn editing(mut self, editing: bool, caret: usize) -> Self {
        self.editing = editing;
        self.caret = caret;
        self
    }

    /// The line under the toggles: "42 hits in 7 files", "Searching…", or an
    /// error from the engine. `None` while nothing has been asked for.
    #[must_use]
    pub const fn summary(mut self, summary: Option<&'a str>) -> Self {
        self.summary = summary;
        self
    }

    /// The widest line number in the whole result list, so the gutter does not
    /// change width as the window moves over it.
    ///
    /// Only a caller passing a window of its results needs this; left unset,
    /// the gutter is measured from the rows it was handed, which is the same
    /// answer when those rows are the whole list.
    #[must_use]
    pub const fn widest_line(mut self, line: u32) -> Self {
        self.widest_line = Some(line);
        self
    }

    /// What the header's button does, for the status line on hover.
    ///
    /// A constant rather than a `describe` like [`SearchButton`]'s, because
    /// this button has no state for a description to vary with: it always does
    /// the one thing, and a method taking nothing to answer with a literal
    /// would be a method in name only.
    pub const BACK_DESCRIPTION: &'static str = "Show the file tree";

    /// The header row of `area`.
    #[must_use]
    pub fn header_area(area: Rect) -> Rect {
        band(area, 0)
    }

    /// The cell in the header that gives the sidebar back to the file tree,
    /// if the panel is wide enough to show it.
    ///
    /// It sits where the tree's own rightmost header button sits, so the two
    /// swap in place rather than the pointer having to go looking.
    #[must_use]
    pub fn back_area(area: Rect) -> Option<Rect> {
        let row = Self::header_area(area);
        let x = row.right().checked_sub(2)?;
        (x > row.x + 1 && row.height > 0).then(|| Rect::new(x, row.y, 1, 1))
    }

    /// The row the query is typed into.
    #[must_use]
    pub fn query_area(area: Rect) -> Rect {
        band(area, 1)
    }

    /// The row of toggles.
    #[must_use]
    pub fn toggles_area(area: Rect) -> Rect {
        band(area, 2)
    }

    /// The summary line under the toggles.
    #[must_use]
    pub fn summary_area(area: Rect) -> Rect {
        band(area, 3)
    }

    /// Where the results go, below all of that.
    #[must_use]
    pub fn rows_area(area: Rect) -> Rect {
        let head = HEAD_ROWS.min(area.height);
        Rect { y: area.y + head, height: area.height - head, ..area }
    }

    /// The cell one toggle occupies, if the panel is wide enough to show it.
    #[must_use]
    pub fn button_area(area: Rect, button: SearchButton) -> Option<Rect> {
        let row = Self::toggles_area(area);
        let index = SearchButton::ALL.iter().position(|b| *b == button)?;
        // One column of margin on the left, then each toggle with a space
        // after it.
        let offset = u16::try_from(1 + index * 2).ok()?;
        let x = row.x.checked_add(offset)?;
        (x < row.right() && row.height > 0).then(|| Rect::new(x, row.y, 1, 1))
    }

    /// How many result rows fit.
    #[must_use]
    pub fn visible_rows(area: Rect) -> usize {
        usize::from(Self::rows_area(area).height)
    }

    /// The result row drawn at screen row `y`, given the scroll.
    #[must_use]
    pub fn row_at(area: Rect, scroll: usize, y: u16, rows: usize) -> Option<usize> {
        let rows_area = Self::rows_area(area);
        if y < rows_area.y || y >= rows_area.bottom() {
            return None;
        }
        let index = scroll + usize::from(y - rows_area.y);
        (index < rows).then_some(index)
    }

    /// The char offset in `query` that a click at column `x` lands on.
    ///
    /// A click inside a cluster lands in front of it, so a caret never ends up
    /// between a letter and the mark that belongs to it. Columns left of the
    /// text, the prompt's own included, land on the first char shown.
    ///
    /// `caret` is where the caret sat when the row was drawn — the same one
    /// handed to [`SearchView::editing`]. It is needed because the row scrolls
    /// to keep the caret on screen, so which characters are under which
    /// columns is a question only the caret can answer.
    #[must_use]
    pub fn caret_at(area: Rect, query: &str, caret: usize, x: u16) -> usize {
        let row = Self::query_area(area);
        let text_x = row.x.saturating_add(PROMPT_COLS);
        let room = row.right().saturating_sub(text_x);
        let start = query_window(room, query, caret.min(query.chars().count()));
        let Some(target) = x.checked_sub(text_x) else { return start };
        let target = usize::from(target);

        let mut offset = 0usize;
        let mut column = 0usize;
        for cluster in query.graphemes(true) {
            let next = offset + cluster.chars().count();
            if offset < start {
                offset = next;
                continue;
            }
            if column + cluster.width() > target {
                return offset;
            }
            column += cluster.width();
            offset = next;
        }
        offset
    }
}

impl Widget for SearchView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let ground = self.palette.on(Role::Raised, Role::Text);
        fill(cells, area, ground);

        self.draw_header(cells, Self::header_area(area));
        self.draw_query(cells, Self::query_area(area));
        self.draw_toggles(cells, area);
        self.draw_summary(cells, Self::summary_area(area));

        let rows_area = Self::rows_area(area);
        let gutter = self.gutter();
        for (offset, index) in
            (self.scroll..self.rows.len()).take(usize::from(rows_area.height)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows_area.y + offset, height: 1, ..rows_area };
            self.draw_row(cells, line, index, gutter);
        }
    }
}

impl SearchView<'_> {
    fn draw_header(&self, cells: &mut Cells, area: Rect) {
        if area.height == 0 {
            return;
        }
        // Headed in the accent while the panel has the keyboard, so which half
        // of the screen keys go to is visible even before anything in it is
        // selected.
        let role = if self.focused { Role::Accent } else { Role::Dim };
        let style = self.palette.on(Role::Raised, role).add_modifier(Modifier::BOLD);
        let x = area.x.saturating_add(1);
        put(cells, x, area.y, area.width.saturating_sub(4), TITLE, style);

        let Some(cell) = Self::back_area(area) else { return };
        let style = if self.hovered_back {
            self.palette.on(Role::Accent, Role::OnAccent)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };
        put(cells, cell.x, cell.y, 1, BACK, style);
    }

    fn draw_query(&self, cells: &mut Cells, area: Rect) {
        if area.height == 0 {
            return;
        }
        // Washed while it has the keyboard, because a text field that looks
        // the same whether or not typing goes to it is a field people type
        // into by accident.
        let style = if self.editing {
            self.palette.on(Role::Selection, Role::Text)
        } else {
            self.palette.on(Role::Raised, Role::Text)
        };
        fill(cells, area, style);
        if area.width >= PROMPT_COLS {
            put(
                cells,
                area.x,
                area.y,
                PROMPT_COLS,
                PROMPT,
                style.patch(self.palette.ink(Role::Dim)),
            );
        }

        let text_x = area.x.saturating_add(PROMPT_COLS);
        let room = area.right().saturating_sub(text_x);
        if room == 0 {
            return;
        }
        if self.query.is_empty() && !self.editing {
            let faint = style.patch(self.palette.ink(Role::Faint));
            put(cells, text_x, area.y, room, PLACEHOLDER, faint);
            return;
        }

        // The window is anchored to the caret rather than to the start of the
        // query, so a query longer than the panel is still typed into at the
        // end of it rather than blind. It is anchored there whether or not the
        // field has the keyboard: a row that scrolled sideways the moment the
        // caret was drawn would move the text out from under the pointer on
        // the way to clicking it, and `caret_at` would have to guess which of
        // the two windows it was being asked about.
        let caret = self.caret.min(self.query.chars().count());
        let start = query_window(room, self.query, caret);
        let tail = from_char(self.query, start);
        put(cells, text_x, area.y, room, tail, style);
        if !self.editing {
            return;
        }

        let column = column_of(tail, caret - start);
        if column >= usize::from(room) {
            return;
        }
        let Ok(column) = u16::try_from(column) else { return };
        let caret_x = text_x.saturating_add(column);
        let caret_style = self.palette.on(Role::Accent, Role::OnAccent);
        // A caret sitting on a wide character covers both of its cells; half a
        // lit cell reads as a rendering fault rather than as a caret.
        let width = u16::try_from(cells[(caret_x, area.y)].symbol().width()).unwrap_or(1).max(1);
        for extra in 0..width {
            let cell_x = caret_x.saturating_add(extra);
            if cell_x < area.right() {
                cells[(cell_x, area.y)].set_style(caret_style);
            }
        }
    }

    fn draw_toggles(&self, cells: &mut Cells, area: Rect) {
        for button in SearchButton::ALL {
            let Some(cell) = Self::button_area(area, button) else { continue };
            let lit = self.toggles.on(button);
            let mut style = if lit {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Dim)
            };
            if self.hovered_button == Some(button) {
                // A lit toggle already carries the accent, so hover is marked
                // by weight there; washing it would say it had gone out.
                style = if lit {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style.patch(self.palette.cursor_line())
                };
            }
            put(cells, cell.x, cell.y, 1, button.glyph(), style);
        }
    }

    fn draw_summary(&self, cells: &mut Cells, area: Rect) {
        let Some(summary) = self.summary else { return };
        if area.height == 0 {
            return;
        }
        let x = area.x.saturating_add(1);
        let style = self.palette.on(Role::Raised, Role::Faint);
        put(cells, x, area.y, area.right().saturating_sub(x), summary, style);
    }

    /// Columns the line numbers need.
    ///
    /// From `widest_line` when the caller gave one, because only it knows what
    /// lies outside the window; otherwise measured over every row it was
    /// handed, which is the same answer when those rows are the whole list.
    fn gutter(&self) -> u16 {
        if let Some(line) = self.widest_line {
            return digits(line).max(MIN_GUTTER);
        }
        self.rows
            .iter()
            .filter_map(|row| match row {
                SearchRow::Hit { line, .. } => Some(digits(*line)),
                SearchRow::File { .. } => None,
            })
            .max()
            .unwrap_or(MIN_GUTTER)
            .max(MIN_GUTTER)
    }

    fn draw_row(&self, cells: &mut Cells, line: Rect, index: usize, gutter: u16) {
        let mut style = self.palette.on(Role::Raised, Role::Text);
        if self.hovered == Some(index) {
            style = style.patch(self.palette.cursor_line());
        }
        if self.selected == Some(index) {
            let wash = if self.focused { Role::Selection } else { Role::CursorLine };
            style = style.patch(self.palette.on(wash, Role::Text));
        }
        fill(cells, line, style);

        match self.rows[index] {
            SearchRow::File { path, hits, collapsed } => {
                self.draw_file(cells, line, style, path, hits, collapsed);
            }
            SearchRow::Hit { line: number, text, matched } => {
                self.draw_hit(cells, line, style, gutter, number, text, matched);
            }
        }
    }

    fn draw_file(
        &self,
        cells: &mut Cells,
        line: Rect,
        style: Style,
        path: &str,
        hits: usize,
        collapsed: bool,
    ) {
        let x = line.x.saturating_add(1);
        let disclosure = if collapsed { "▸ " } else { "▾ " };
        let room = line.right().saturating_sub(x);
        put(cells, x, line.y, room, disclosure, style.patch(self.palette.ink(Role::Dim)));

        let count = hits.to_string();
        let count_cols = u16::try_from(count.width()).unwrap_or(0);
        let path_x = x.saturating_add(2);
        // The count keeps its columns and the path yields to it: a truncated
        // path still says which file, a truncated count says nothing.
        let room = line.right().saturating_sub(path_x).saturating_sub(count_cols + 1);
        put(cells, path_x, line.y, room, path, style);

        if let Some(count_x) = line.right().checked_sub(count_cols + 1)
            && count_x >= path_x
        {
            put(
                cells,
                count_x,
                line.y,
                count_cols,
                &count,
                style.patch(self.palette.ink(Role::Dim)),
            );
        }
    }

    // Each one is a separate thing to draw, as in the palette's own row.
    #[allow(clippy::too_many_arguments)]
    fn draw_hit(
        &self,
        cells: &mut Cells,
        line: Rect,
        style: Style,
        gutter: u16,
        number: u32,
        text: &str,
        matched: &[Range<u32>],
    ) {
        let x = line.x.saturating_add(1).saturating_add(INDENT);
        let number = format!("{number:>width$}", width = usize::from(gutter));
        let room = line.right().saturating_sub(x).min(gutter);
        put(cells, x, line.y, room, &number, style.patch(self.palette.ink(Role::Dim)));

        let text_x = x.saturating_add(gutter).saturating_add(1);
        let room = line.right().saturating_sub(text_x);
        // The ramp has no role of its own for a match, and the accent is the
        // one colour nun has for "this is what you asked about".
        let matched_style = style.patch(self.palette.ink(Role::Accent));
        put_matched(cells, text_x, line.y, room, text, matched, style, matched_style);
    }
}

/// The `index`-th row of `area`, zero-height when `area` is not that tall.
fn band(area: Rect, index: u16) -> Rect {
    let offset = index.min(area.height);
    Rect { y: area.y + offset, height: area.height.saturating_sub(index).min(1), ..area }
}

/// Decimal digits in a line number.
fn digits(line: u32) -> u16 {
    u16::try_from(line.max(1).ilog10() + 1).unwrap_or(MIN_GUTTER)
}

/// `text` from char offset `chars` onward, empty once past its end.
fn from_char(text: &str, chars: usize) -> &str {
    text.char_indices().nth(chars).map_or("", |(byte, _)| &text[byte..])
}

/// The screen column char offset `chars` sits at.
///
/// A grapheme walk rather than arithmetic on the offset: a cluster may be
/// several chars wide in memory and one or two columns wide on screen, and the
/// two numbers have nothing to do with each other. An offset landing inside a
/// cluster reports that cluster's first column, and one past the end reports
/// the full width, which is how a range the engine windowed past the end of a
/// long line clips instead of panicking.
fn column_of(text: &str, chars: usize) -> usize {
    let mut offset = 0usize;
    let mut column = 0usize;
    for cluster in text.graphemes(true) {
        let next = offset + cluster.chars().count();
        if next > chars {
            break;
        }
        offset = next;
        column += cluster.width();
    }
    column
}

/// The char offset of the first char of `query` the query row draws, given a
/// caret at char offset `caret` and `room` columns to draw into.
///
/// The window starts on a cluster boundary — walking back from the caret until
/// one more cluster would not fit — so a wide character is never sliced down
/// the middle by the left edge. One column is held back for the caret itself,
/// which is why it is still on screen when it sits at the very end.
fn query_window(room: u16, query: &str, caret: usize) -> usize {
    let budget = usize::from(room).saturating_sub(1);

    let mut before: Vec<(usize, usize)> = Vec::new();
    let mut chars = 0usize;
    for cluster in query.graphemes(true) {
        let next = chars + cluster.chars().count();
        if next > caret {
            break;
        }
        before.push((next, cluster.width()));
        chars = next;
    }

    let mut used = 0usize;
    for &(end, width) in before.iter().rev() {
        used += width;
        if used > budget {
            return end;
        }
    }
    0
}

fn fill(cells: &mut Cells, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            cells[(x, y)].set_char(' ').set_style(style);
        }
    }
}

/// Write `text` at `(x, y)` in at most `room` columns, clipping at a cluster
/// rather than splitting one, and ending with `…` when it had to clip.
fn put(cells: &mut Cells, x: u16, y: u16, room: u16, text: &str, style: Style) {
    let room = usize::from(room);
    let fits = text.width() <= room;
    let budget = if fits { room } else { room.saturating_sub(1) };

    let mut column = 0usize;
    for cluster in text.graphemes(true) {
        let width = cluster.width();
        if column + width > budget {
            break;
        }
        let Ok(offset) = u16::try_from(column) else { break };
        cells[(x + offset, y)].set_symbol(cluster).set_style(style);
        for extra in 1..width {
            let Ok(extra) = u16::try_from(column + extra) else { break };
            cells[(x + extra, y)].set_symbol(" ").set_style(style);
        }
        column += width;
    }
    if !fits && room > 0 {
        let Ok(offset) = u16::try_from(column) else { return };
        cells[(x + offset, y)].set_symbol("…").set_style(style);
    }
}

/// Write `text` like [`put`], with the char ranges in `matched` picked out.
///
/// A cluster is matched when any of the chars it is made of falls inside a
/// range, which is what makes the highlight land on the right columns when the
/// line holds a wide character, a combining mark or an emoji: the ranges are
/// counted in chars and the screen is counted in columns, and the walk is the
/// only honest way between the two. A range running past the end of `text` —
/// which happens when the engine windowed a long line — simply stops matching.
#[allow(clippy::too_many_arguments)] // Each one is a separate thing to draw.
fn put_matched(
    cells: &mut Cells,
    x: u16,
    y: u16,
    room: u16,
    text: &str,
    matched: &[Range<u32>],
    style: Style,
    matched_style: Style,
) {
    let room = usize::from(room);
    let fits = text.width() <= room;
    let budget = if fits { room } else { room.saturating_sub(1) };

    let mut column = 0usize;
    let mut chars = 0u32;
    for cluster in text.graphemes(true) {
        let width = cluster.width();
        if column + width > budget {
            break;
        }
        let next = chars.saturating_add(u32::try_from(cluster.chars().count()).unwrap_or(1));
        let hit = matched.iter().any(|range| range.start < next && chars < range.end);
        let cell_style = if hit { matched_style } else { style };

        let Ok(offset) = u16::try_from(column) else { break };
        cells[(x + offset, y)].set_symbol(cluster).set_style(cell_style);
        for extra in 1..width {
            let Ok(extra) = u16::try_from(column + extra) else { break };
            cells[(x + extra, y)].set_symbol(" ").set_style(cell_style);
        }
        column += width;
        chars = next;
    }
    if !fits && room > 0 {
        let Ok(offset) = u16::try_from(column) else { return };
        cells[(x + offset, y)].set_symbol("…").set_style(style);
    }
}
