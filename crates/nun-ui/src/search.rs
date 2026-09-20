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

    /// Columns the line numbers need, taken over every row rather than the
    /// visible ones so the text does not step sideways as the list scrolls.
    fn gutter(&self) -> u16 {
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

#[cfg(test)]
mod tests {
    use nun_theme::{Probe, derive};
    use ratatui::style::Color;

    use super::*;
    use crate::harness::Harness;

    fn palette() -> Palette {
        Palette::new(derive(&Probe::builtin_dark()))
    }

    fn rows<'a>() -> Vec<SearchRow<'a>> {
        vec![
            SearchRow::File { path: "src/main.rs", hits: 2, collapsed: false },
            SearchRow::Hit { line: 7, text: "fn main() {", matched: &[] },
            SearchRow::Hit { line: 91, text: "    render();", matched: &[] },
            SearchRow::File { path: "README.md", hits: 1, collapsed: true },
        ]
    }

    /// The column the query row's caret sits on.
    fn caret_column(harness: &Harness) -> u16 {
        let bg = palette().on(Role::Accent, Role::OnAccent).bg.expect("the caret has a wash");
        (0..harness.area().width)
            .find(|x| harness.cells()[(*x, 1)].bg == bg)
            .expect("the caret is drawn")
    }

    /// The first result row, which the panel's four chrome rows sit above.
    const FIRST: u16 = HEAD_ROWS;

    #[test]
    fn the_prompt_is_as_wide_as_the_geometry_says_it_is() {
        assert_eq!(PROMPT.width(), usize::from(PROMPT_COLS));
    }

    #[test]
    fn the_panel_draws_its_chrome_then_its_rows() {
        let rows = rows();
        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(
            SearchView::new("main", &rows, &palette)
                .summary(Some("3 hits in 2 files"))
                .focused(true),
        );

        assert_eq!(
            harness.to_text(),
            " SEARCH               ▤\n\
             ⌕ main\n\
             \u{20}* A ▭ ○\n\
             \u{20}3 hits in 2 files\n\
             \u{20}▾ src/main.rs        2\n\
             \u{20}    7 fn main() {\n\
             \u{20}   91     render();\n\
             \u{20}▸ README.md          1"
        );
    }

    #[test]
    fn every_band_agrees_with_the_row_it_draws() {
        let area = Rect::new(0, 0, 24, 9);
        assert_eq!(SearchView::header_area(area), Rect::new(0, 0, 24, 1));
        assert_eq!(SearchView::query_area(area), Rect::new(0, 1, 24, 1));
        assert_eq!(SearchView::toggles_area(area), Rect::new(0, 2, 24, 1));
        assert_eq!(SearchView::summary_area(area), Rect::new(0, 3, 24, 1));
        assert_eq!(SearchView::rows_area(area), Rect::new(0, 4, 24, 5));
        assert_eq!(SearchView::visible_rows(area), 5);
    }

    #[test]
    fn a_panel_too_short_for_a_band_gives_it_no_rows() {
        for height in 0..=3u16 {
            let area = Rect::new(0, 0, 24, height);
            let bands = [
                SearchView::header_area(area),
                SearchView::query_area(area),
                SearchView::toggles_area(area),
                SearchView::summary_area(area),
            ];
            for (index, band) in bands.iter().enumerate() {
                let expected = u16::from(u16::try_from(index).unwrap_or(0) < height);
                assert_eq!(band.height, expected, "band {index} at height {height}");
            }
            assert_eq!(SearchView::rows_area(area).height, 0, "height {height}");
            assert_eq!(SearchView::visible_rows(area), 0, "height {height}");
        }
    }

    #[test]
    fn a_squeezed_panel_draws_only_the_bands_it_has_room_for() {
        let rows = rows();
        let palette = palette();
        for height in 0..=4u16 {
            let mut harness = Harness::new(24, height.max(1));
            harness.draw(
                SearchView::new("q", &rows, &palette)
                    .summary(Some("later"))
                    .toggles(Toggles { regex: true, ..Toggles::default() }),
            );
            // Drawn into a screen one row tall when the panel has none, so the
            // assertion below is about what the panel drew, not what fitted.
            if height == 0 {
                continue;
            }
            let text = harness.to_text();
            assert!(text.contains("SEARCH"), "height {height}: {text:?}");
            assert_eq!(text.contains('q'), height >= 2, "height {height}: {text:?}");
            assert_eq!(text.contains('*'), height >= 3, "height {height}: {text:?}");
            assert_eq!(text.contains("later"), height >= 4, "height {height}: {text:?}");
        }
    }

    #[test]
    fn a_panel_with_no_area_draws_nothing() {
        let rows = rows();
        let palette = palette();
        let mut harness = Harness::new(24, 8);
        let before = harness.snapshot();
        harness.draw(SearchView::new("q", &rows, &palette));
        let after = harness.snapshot();
        assert_ne!(crate::changed_rows(&before, &after), Vec::<u16>::new());

        // A zero-width or zero-height area must be a no-op rather than a panic
        // or a row of stray cells.
        let mut cells = Cells::empty(Rect::new(0, 0, 24, 8));
        SearchView::new("q", &rows, &palette).render(Rect::new(0, 0, 0, 8), &mut cells);
        SearchView::new("q", &rows, &palette).render(Rect::new(0, 0, 24, 0), &mut cells);
        assert_eq!(cells, Cells::empty(Rect::new(0, 0, 24, 8)));
    }

    #[test]
    fn row_at_round_trips_with_the_scroll() {
        let area = Rect::new(0, 0, 24, 8);
        assert_eq!(SearchView::row_at(area, 0, 3, 4), None, "the chrome is not a row");
        assert_eq!(SearchView::row_at(area, 0, FIRST, 4), Some(0));
        assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 4), Some(3));
        assert_eq!(SearchView::row_at(area, 12, FIRST + 1, 20), Some(13), "scrolled");
        assert_eq!(SearchView::row_at(area, 0, FIRST + 3, 3), None, "past the last row");
        assert_eq!(SearchView::row_at(area, 0, 8, 40), None, "below the panel");
    }

    #[test]
    fn only_the_visible_window_of_results_is_drawn() {
        let lines: Vec<String> = (0..1000).map(|i| format!("hit {i}")).collect();
        let rows: Vec<SearchRow<'_>> = lines
            .iter()
            .enumerate()
            .map(|(i, text)| SearchRow::Hit {
                line: u32::try_from(i + 1).unwrap_or(1),
                text,
                matched: &[],
            })
            .collect();
        let palette = palette();
        let mut harness = Harness::new(24, 7);
        harness.draw(SearchView::new("hit", &rows, &palette).scrolled_to(500));
        let text = harness.to_text();
        assert!(text.contains("hit 500") && text.contains("hit 502"), "{text}");
        assert!(!text.contains("hit 503"), "{text}");
    }

    #[test]
    fn an_empty_result_list_draws_only_the_chrome() {
        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("nothing", &[], &palette).summary(Some("No matches")));
        let text = harness.to_text();
        assert!(text.contains("No matches"), "{text}");
        assert_eq!(text.lines().skip(usize::from(FIRST)).collect::<String>(), "");
    }

    #[test]
    fn the_toggles_sit_where_the_geometry_says_and_light_up() {
        let area = Rect::new(0, 0, 24, 8);
        let cells: Vec<Rect> = SearchButton::ALL
            .into_iter()
            .map(|button| SearchView::button_area(area, button).expect("wide enough"))
            .collect();
        for pair in cells.windows(2) {
            assert_eq!(pair[1].x, pair[0].x + 2, "one cell and one space apart");
            assert_eq!(pair[0].y, SearchView::toggles_area(area).y);
        }

        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(
            SearchView::new("q", &[], &palette)
                .toggles(Toggles { case: true, ..Toggles::default() }),
        );
        let lit = palette.on(Role::Accent, Role::OnAccent).bg.expect("a lit toggle has a wash");
        let case = SearchView::button_area(area, SearchButton::Case).expect("wide enough");
        assert_eq!(harness.cells()[(case.x, case.y)].bg, lit);
        let regex = SearchView::button_area(area, SearchButton::Regex).expect("wide enough");
        assert_ne!(harness.cells()[(regex.x, regex.y)].bg, lit, "an unlit toggle is not washed");
    }

    #[test]
    fn the_header_keeps_a_way_back_to_the_tree() {
        let area = Rect::new(0, 0, 24, 8);
        let cell = SearchView::back_area(area).expect("wide enough");
        assert_eq!(cell, Rect::new(22, 0, 1, 1), "where the tree's own buttons sit");

        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("q", &[], &palette).hovered_back(true));
        assert_eq!(harness.cells()[(cell.x, cell.y)].symbol(), BACK);
        let lit = palette.on(Role::Accent, Role::OnAccent).bg.expect("a hovered button is washed");
        assert_eq!(harness.cells()[(cell.x, cell.y)].bg, lit);

        assert_eq!(SearchView::back_area(Rect::new(0, 0, 3, 8)), None, "too narrow");
        assert_eq!(SearchView::back_area(Rect::new(0, 0, 24, 0)), None, "no header");
    }

    #[test]
    fn the_back_button_never_overlaps_the_title() {
        let palette = palette();
        for width in 1..=16u16 {
            let area = Rect::new(0, 0, width, 5);
            let mut harness = Harness::new(width, 5);
            harness.draw(SearchView::new("q", &[], &palette));
            let header = harness.to_text().lines().next().unwrap_or_default().to_string();

            let Some(cell) = SearchView::back_area(area) else {
                assert!(!header.contains(BACK), "width {width}: {header:?}");
                continue;
            };
            assert_eq!(harness.cells()[(cell.x, cell.y)].symbol(), BACK, "width {width}");
            // The title yields four columns to it, so there is always a blank
            // between the two however far the title had to be clipped.
            assert_eq!(harness.cells()[(cell.x - 1, cell.y)].symbol(), " ", "width {width}");
            assert_eq!(header.matches(BACK).count(), 1, "width {width}: {header:?}");
        }
    }

    #[test]
    fn a_panel_narrower_than_its_buttons_drops_them() {
        let area = Rect::new(0, 0, 4, 8);
        assert!(SearchView::button_area(area, SearchButton::Regex).is_some());
        assert!(SearchView::button_area(area, SearchButton::Case).is_some());
        assert_eq!(SearchView::button_area(area, SearchButton::Word), None, "too narrow");
        assert_eq!(SearchView::button_area(area, SearchButton::Ignored), None, "too narrow");
        assert_eq!(SearchView::button_area(Rect::new(0, 0, 24, 2), SearchButton::Regex), None);

        // And the panel draws at that width without reaching past its edge.
        let rows = rows();
        let palette = palette();
        let mut harness = Harness::new(4, 8);
        harness.draw(SearchView::new("a much longer query", &rows, &palette).summary(Some("x")));
        assert!(!harness.to_text().is_empty());
    }

    #[test]
    fn a_collapsed_file_points_its_disclosure_the_other_way() {
        let rows = rows();
        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("main", &rows, &palette));
        let text = harness.to_text();
        assert!(text.contains("▾ src/main.rs"), "{text}");
        assert!(text.contains("▸ README.md"), "{text}");
    }

    #[test]
    fn a_file_row_right_aligns_its_hit_count() {
        let rows = vec![SearchRow::File { path: "a.rs", hits: 128, collapsed: false }];
        let palette = palette();
        let mut harness = Harness::new(24, 5);
        harness.draw(SearchView::new("x", &rows, &palette));
        let line =
            harness.to_text().lines().nth(usize::from(FIRST)).unwrap_or_default().to_string();
        assert!(line.ends_with("128"), "{line:?}");
        assert_eq!(line.width(), 23, "one column of margin on the right");
    }

    #[test]
    fn caret_at_round_trips_with_the_caret_the_query_row_draws() {
        let query = "let x";
        let palette = palette();
        let area = Rect::new(0, 0, 24, 8);
        for caret in 0..=query.chars().count() {
            let mut harness = Harness::new(24, 8);
            harness.draw(SearchView::new(query, &[], &palette).editing(true, caret));
            let x = caret_column(&harness);
            assert_eq!(
                SearchView::caret_at(area, query, caret, x),
                caret,
                "caret {caret} at column {x}"
            );
        }
    }

    #[test]
    fn caret_at_round_trips_with_the_query_scrolled_horizontally() {
        // Distinct characters throughout, so an assertion about which of them
        // survived the scroll means something.
        let query: String = ('a'..='z').chain('A'..='N').collect();
        let chars = query.chars().count();
        let palette = palette();
        let area = Rect::new(0, 0, 20, 8);
        let mut harness = Harness::new(20, 8);
        harness.draw(SearchView::new(&query, &[], &palette).editing(true, chars));

        // Room for the text is the panel less the prompt, less the column the
        // caret keeps for itself.
        let room = usize::from(area.width - PROMPT_COLS);
        let first = chars + 1 - room;
        assert_eq!(caret_column(&harness), area.width - 1, "the caret sits at the right edge");
        assert_eq!(SearchView::caret_at(area, &query, chars, area.width - 1), chars, "the end");
        assert_eq!(
            SearchView::caret_at(area, &query, chars, PROMPT_COLS),
            first,
            "the leftmost char"
        );
        assert_eq!(
            SearchView::caret_at(area, &query, chars, PROMPT_COLS + 5),
            first + 5,
            "and onward"
        );
        assert_eq!(SearchView::caret_at(area, &query, chars, 0), first, "a click on the prompt");

        let text = harness.to_text().lines().nth(1).unwrap_or_default().to_string();
        assert!(text.ends_with('N') && !text.contains('a'), "the tail is shown: {text:?}");
    }

    #[test]
    fn caret_at_is_exact_with_the_caret_in_the_middle_of_a_scrolled_query() {
        // Distinct characters throughout, so an assertion about which of them
        // a column holds means something.
        let query: String = ('a'..='z').chain('A'..='N').collect();
        let palette = palette();
        let area = Rect::new(0, 0, 20, 8);
        let caret = 20;
        let mut harness = Harness::new(20, 8);
        harness.draw(SearchView::new(&query, &[], &palette).editing(true, caret));

        // Every column holding query text answers with the character drawn on
        // it. The clip mark and the caret's own cell are not query text.
        let mut checked = 0;
        for x in PROMPT_COLS..area.width {
            let drawn = harness.cells()[(x, 1)].symbol().to_string();
            if drawn == "…" || drawn == " " {
                continue;
            }
            let offset = SearchView::caret_at(area, &query, caret, x);
            let under = query.chars().nth(offset).map(String::from);
            assert_eq!(under.as_deref(), Some(drawn.as_str()), "column {x}");
            checked += 1;
        }
        assert!(checked > 10, "the query filled the row: {checked} columns");
        assert_eq!(SearchView::caret_at(area, &query, caret, caret_column(&harness)), caret);

        // And the answer genuinely turns on the caret: the window an
        // end-anchored row would show starts somewhere else entirely.
        assert_ne!(
            SearchView::caret_at(area, &query, caret, PROMPT_COLS),
            SearchView::caret_at(area, &query, query.chars().count(), PROMPT_COLS),
        );
    }

    #[test]
    fn a_wide_character_does_not_shift_the_caret() {
        let query = "日本x";
        let palette = palette();
        let area = Rect::new(0, 0, 24, 8);
        for caret in 0..=3 {
            let mut harness = Harness::new(24, 8);
            harness.draw(SearchView::new(query, &[], &palette).editing(true, caret));
            let expected = area.x + PROMPT_COLS + u16::try_from(column_of(query, caret)).unwrap();
            assert_eq!(caret_column(&harness), expected, "caret {caret}");
            assert_eq!(SearchView::caret_at(area, query, caret, expected), caret);
        }
    }

    #[test]
    fn an_empty_query_shows_a_placeholder_until_it_is_typed_into() {
        let palette = palette();
        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("", &[], &palette));
        assert!(harness.to_text().contains(PLACEHOLDER));

        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("", &[], &palette).editing(true, 0));
        assert!(!harness.to_text().contains(PLACEHOLDER), "the caret is not typed over");
        assert_eq!(caret_column(&harness), PROMPT_COLS);
    }

    /// Where the text of a hit row starts, for a list whose line numbers all
    /// fit the minimum gutter.
    const HIT_TEXT_X: u16 = 1 + INDENT + MIN_GUTTER + 1;

    /// The columns of the first result row drawn in the accent.
    ///
    /// A wide character counts once: a terminal never writes the cell it
    /// covers, so that cell is not the panel's to colour and the diff never
    /// carries it.
    fn highlighted(text: &str, matched: Range<u32>) -> Vec<u16> {
        let matched = [matched];
        let rows = vec![SearchRow::Hit { line: 1, text, matched: &matched }];
        let palette = palette();
        let mut harness = Harness::new(40, 5);
        harness.draw(SearchView::new("x", &rows, &palette));
        let accent = palette.ink(Role::Accent).fg.expect("the accent is a colour");
        harness
            .visible_cells(FIRST)
            .into_iter()
            .filter(|x| harness.cells()[(*x, FIRST)].fg == accent)
            .collect()
    }

    /// What is drawn at one column of the first result row.
    fn symbol_at(text: &str, x: u16) -> String {
        let rows = vec![SearchRow::Hit { line: 1, text, matched: &[] }];
        let palette = palette();
        let mut harness = Harness::new(40, 5);
        harness.draw(SearchView::new("x", &rows, &palette));
        harness.cells()[(x, FIRST)].symbol().to_string()
    }

    #[test]
    fn a_match_lands_on_the_columns_it_covers() {
        assert_eq!(
            highlighted("hello world", 6..11),
            (HIT_TEXT_X + 6..HIT_TEXT_X + 11).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_wide_character_does_not_shift_the_highlight() {
        // 日 and 本 are two columns each, so the match on 語 — the third char —
        // lands four columns in, not two, which is where counting chars would
        // have put it.
        assert_eq!(highlighted("日本語x", 2..3), vec![HIT_TEXT_X + 4]);
        assert_eq!(symbol_at("日本語x", HIT_TEXT_X + 4), "語");
        assert_eq!(highlighted("日本語x", 3..4), vec![HIT_TEXT_X + 6]);
        assert_eq!(symbol_at("日本語x", HIT_TEXT_X + 6), "x");
    }

    #[test]
    fn a_combining_mark_does_not_shift_the_highlight() {
        // "e" and its acute are two chars and one column, and the match on
        // either of them lights that one column.
        let text = "e\u{301}x";
        assert_eq!(highlighted(text, 0..1), vec![HIT_TEXT_X]);
        assert_eq!(highlighted(text, 1..2), vec![HIT_TEXT_X], "the mark belongs to the letter");
        assert_eq!(highlighted(text, 2..3), vec![HIT_TEXT_X + 1]);
    }

    #[test]
    fn an_emoji_does_not_shift_the_highlight() {
        // Counting chars would put the b one column too far left.
        assert_eq!(highlighted("a👍b", 2..3), vec![HIT_TEXT_X + 3]);
        assert_eq!(symbol_at("a👍b", HIT_TEXT_X + 3), "b");
        assert_eq!(highlighted("a👍b", 1..2), vec![HIT_TEXT_X + 1]);
        assert_eq!(symbol_at("a👍b", HIT_TEXT_X + 1), "👍");
    }

    #[test]
    fn several_matches_on_one_line_are_all_picked_out() {
        let matched = [1..2, 4..6];
        let rows = vec![SearchRow::Hit { line: 1, text: "abcdef", matched: &matched }];
        let palette = palette();
        let mut harness = Harness::new(40, 5);
        harness.draw(SearchView::new("x", &rows, &palette));
        let accent = palette.ink(Role::Accent).fg.expect("the accent is a colour");
        let lit: Vec<u16> = harness
            .visible_cells(FIRST)
            .into_iter()
            .filter(|x| harness.cells()[(*x, FIRST)].fg == accent)
            .collect();
        assert_eq!(lit, vec![HIT_TEXT_X + 1, HIT_TEXT_X + 4, HIT_TEXT_X + 5]);
    }

    #[test]
    fn a_match_running_past_the_end_of_the_line_is_clipped() {
        // The engine windows a long line, and a range can survive the window.
        assert_eq!(highlighted("abc", 1..99), vec![HIT_TEXT_X + 1, HIT_TEXT_X + 2]);
        assert_eq!(highlighted("abc", 40..99), Vec::<u16>::new());
        assert_eq!(highlighted("", 0..5), Vec::<u16>::new());
    }

    #[test]
    fn the_gutter_widens_to_the_longest_line_number() {
        let rows = vec![
            SearchRow::Hit { line: 3, text: "a", matched: &[] },
            SearchRow::Hit { line: 14_872, text: "b", matched: &[] },
        ];
        let palette = palette();
        let mut harness = Harness::new(30, 6);
        harness.draw(SearchView::new("x", &rows, &palette));
        let text = harness.to_text();
        let lines: Vec<&str> = text.lines().skip(usize::from(FIRST)).collect();
        assert_eq!(lines[0], "       3 a");
        assert_eq!(lines[1], "   14872 b", "the text of both starts at the same column");
    }

    #[test]
    fn the_selection_wash_follows_the_focus() {
        let rows = rows();
        let palette = palette();
        let focused = palette.on(Role::Selection, Role::Text).bg.expect("a wash");
        let unfocused = palette.cursor_line().bg.expect("a wash");

        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("m", &rows, &palette).selected(Some(0)).focused(true));
        assert_eq!(harness.cells()[(0, FIRST)].bg, focused);

        let mut harness = Harness::new(24, 8);
        harness.draw(SearchView::new("m", &rows, &palette).selected(Some(0)));
        assert_eq!(harness.cells()[(0, FIRST)].bg, unfocused);
    }

    #[test]
    fn every_cell_is_painted() {
        let rows = rows();
        let palette = palette();
        let mut harness = Harness::new(24, 10);
        harness.draw(SearchView::new("main", &rows, &palette).summary(Some("3 hits")));
        let cells = harness.cells();
        for y in 0..10 {
            for x in harness.visible_cells(y) {
                assert_ne!(cells[(x, y)].bg, Color::Reset, "({x}, {y})");
            }
        }
    }
}
