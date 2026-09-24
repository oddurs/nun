//! The project search-and-replace panel.
//!
//! A query, a replacement, the toggles that change what the query means, and
//! the matching lines grouped under the files they came from — a window of
//! them, however many the engine found. Geometry is exposed as associated
//! functions so the binary lays out exactly the hit regions this draws, from
//! one source.
//!
//! With a replacement typed, the results become a diff: each hit is drawn as
//! the line it is now and the line it would become, so what is about to be
//! written across the repository is on screen before it happens rather than
//! summarised afterwards. Any hit can be struck out of the batch on its own.
//!
//! The same panel previews a rename. A rename is a replace worked out by a
//! language server rather than by a query, so it is drawn the same way — the
//! old name, the new one, and every line it changes, before and after — with
//! a title of its own, a row of text buttons where the toggles would be, and
//! a mark on each file saying whether it is in.
//!
//! Nothing here runs the search, and nothing here writes a file. The panel is
//! handed rows and draws them, which is what keeps a slow walk of a large
//! repository off the render path.

use std::ops::Range;

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::clip;
use crate::glyph::Glyph;
use crate::style::Palette;

/// Columns a hit is indented under the file it belongs to.
const INDENT: u16 = 2;

/// The panel's name.
///
/// Uppercased in the source rather than at render time: there is nothing here
/// for it to vary with, unlike the tree, whose title is the workspace's name.
const TITLE: &str = "SEARCH";

/// Columns the apply button keeps to itself at the right of the replace row:
/// the cell it sits in, the margin outside it, and a blank inside it so the
/// replacement never runs up against it.
const APPLY_COLS: u16 = 3;

/// Columns the query's prompt occupies, and the replacement's with it: a
/// glyph and a space. Held as a constant because the two fields' geometry
/// must be answerable without measuring a string, which every glyph being
/// one cell wide makes true.
const PROMPT_COLS: u16 = 2;

/// What an empty query field says when nobody is typing into it.
const PLACEHOLDER: &str = "Search the project";

/// What an empty replacement field says when nobody is typing into it.
const REPLACE_PLACEHOLDER: &str = "Replace with";

/// The column a hit's include marker sits in: the margin left of the indent,
/// where a version-control gutter would be, so it never moves with the text.
const MARKER_COL: u16 = 1;

/// The column a file's include mark sits in: the very edge, since the column a
/// hit's marker takes is where a file row draws its disclosure triangle.
const FILE_MARKER_COL: u16 = 0;

/// Rows above the results: the header, the query, the replacement, the
/// toggles, the summary.
const HEAD_ROWS: u16 = 5;

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
    pub const fn glyph(self) -> Glyph {
        match self {
            Self::Regex => Glyph::SearchRegex,
            Self::Case => Glyph::SearchCase,
            Self::Word => Glyph::SearchWord,
            Self::Ignored => Glyph::SearchIgnored,
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

/// Which of the panel's two text fields has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    /// What is being searched for.
    #[default]
    Query,
    /// What it is being replaced with.
    Replace,
}

/// What is going to happen to one hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitState {
    /// Nothing is being replaced; this is a search result.
    Plain,
    /// It will be replaced when the replace is applied.
    Included,
    /// It has been excluded, and will be left alone.
    Excluded,
}

/// One row of the results list.
///
/// The list is flat rather than a tree of files holding hits, because that is
/// what scrolling and hit-testing want: one index, one row, whatever it is.
/// An [`SearchRow::After`] row is a row like any other for that purpose — it
/// takes an index, it can be scrolled past, and it counts towards the window.
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
        /// Whether the file as a whole is in what is about to be written.
        /// [`HitState::Plain`] when files are not included or left out one
        /// at a time, which is the search panel's case; the others draw a
        /// mark in the gutter column that [`SearchView::marker_area`] makes
        /// clickable.
        state: HitState,
    },
    /// A matching line as it stands.
    Hit {
        /// Which line of the file it is, counting from one.
        line: u32,
        /// The line, possibly a window of a very long one.
        text: &'a str,
        /// Char offsets into `text` that matched, ordered and disjoint.
        matched: &'a [Range<u32>],
        /// What is going to happen to it.
        state: HitState,
    },
    /// What the hit above it becomes. Only ever follows an included `Hit`.
    After {
        /// The same line of the same file the hit above it names.
        line: u32,
        /// The line as it would be written.
        text: &'a str,
    },
    /// Something done to a file as a whole — created, moved or deleted — as
    /// a sentence. It is part of what is applied and cannot be left out on
    /// its own, so it carries the included mark and nothing to click.
    Operation {
        /// What is done, in words.
        text: &'a str,
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
    title: &'a str,
    actions: &'a [&'a str],
    hovered_action: Option<usize>,
    query: &'a str,
    replacement: &'a str,
    rows: &'a [SearchRow<'a>],
    palette: &'a Palette,
    scroll: usize,
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_button: Option<SearchButton>,
    hovered_back: bool,
    hovered_apply: bool,
    toggles: Toggles,
    focused: bool,
    editing: Option<Field>,
    caret: usize,
    summary: Option<&'a str>,
    widest_line: Option<u32>,
}

impl<'a> SearchView<'a> {
    /// A panel showing `rows` for `query`.
    #[must_use]
    pub const fn new(query: &'a str, rows: &'a [SearchRow<'a>], palette: &'a Palette) -> Self {
        Self {
            title: TITLE,
            actions: &[],
            hovered_action: None,
            query,
            replacement: "",
            rows,
            palette,
            scroll: 0,
            selected: None,
            hovered: None,
            hovered_button: None,
            hovered_back: false,
            hovered_apply: false,
            toggles: Toggles { regex: false, case: false, word: false, ignored: false },
            focused: false,
            editing: None,
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

    /// Whether the pointer is on the button that applies the replacement.
    #[must_use]
    pub const fn hovered_apply(mut self, hovered: bool) -> Self {
        self.hovered_apply = hovered;
        self
    }

    /// What the query is being replaced with. Empty means nothing is.
    #[must_use]
    pub const fn replacement(mut self, text: &'a str) -> Self {
        self.replacement = text;
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

    /// Which field has the keyboard, and where its caret sits in that field,
    /// as a char offset into that field's text.
    ///
    /// The field without the keyboard has no caret of its own here, and is
    /// drawn from the start of its text. That is what [`SearchView::caret_at`]
    /// means by a caret of zero.
    #[must_use]
    pub const fn editing(mut self, field: Option<Field>, caret: usize) -> Self {
        self.editing = field;
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

    /// Head the panel with `title` instead of the search's own.
    #[must_use]
    pub const fn title(mut self, title: &'a str) -> Self {
        self.title = title;
        self
    }

    /// Put text buttons labelled `actions` where the toggles go, instead of
    /// the toggles.
    ///
    /// For a panel whose query is not something typed — a rename's is the
    /// symbol's name — and so has nothing for the toggles to change, but has
    /// a decision to offer with the mouse. [`SearchView::action_area`] says
    /// where each one is.
    #[must_use]
    pub const fn actions(mut self, actions: &'a [&'a str]) -> Self {
        self.actions = actions;
        self
    }

    /// Mark the action at this index as under the pointer.
    #[must_use]
    pub const fn hovered_action(mut self, action: Option<usize>) -> Self {
        self.hovered_action = action;
        self
    }

    /// Where the action at `index` of `actions` is drawn, if it fits.
    ///
    /// Laid out from the left of the toggles row, each label with a space
    /// either side and a column between them, so the first — the one that
    /// does the thing — is where the eye starts. One that does not fit is not
    /// drawn and has no area, and neither does any after it.
    #[must_use]
    pub fn action_area(area: Rect, actions: &[&str], index: usize) -> Option<Rect> {
        let row = Self::toggles_area(area);
        if row.height == 0 {
            return None;
        }
        let mut x = row.x.checked_add(1)?;
        for (at, label) in actions.iter().enumerate() {
            let width = u16::try_from(label.width() + 2).ok()?;
            let right = x.checked_add(width)?;
            if right > row.right() {
                return None;
            }
            if at == index {
                return Some(Rect::new(x, row.y, width, 1));
            }
            x = right.checked_add(1)?;
        }
        None
    }

    /// What the replace row's button does, for the status line on hover.
    ///
    /// It names the scope as well as the action: with hits struck out, what
    /// the button does and what the panel is showing are not the same thing.
    pub const APPLY_DESCRIPTION: &'static str = "Replace every included hit";

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

    /// The row the replacement is typed into.
    #[must_use]
    pub fn replace_area(area: Rect) -> Rect {
        band(area, 2)
    }

    /// The row of toggles.
    #[must_use]
    pub fn toggles_area(area: Rect) -> Rect {
        band(area, 3)
    }

    /// The summary line under the toggles.
    #[must_use]
    pub fn summary_area(area: Rect) -> Rect {
        band(area, 4)
    }

    /// The cell at the end of the replace row that writes the replacement into
    /// the files, if the panel is wide enough to show it.
    #[must_use]
    pub fn apply_area(area: Rect) -> Option<Rect> {
        let row = Self::replace_area(area);
        let x = row.right().checked_sub(2)?;
        (x > row.x + 1 && row.height > 0).then(|| Rect::new(x, row.y, 1, 1))
    }

    /// The cell that toggles whether a row is included, when the row has one.
    ///
    /// `None` for an `After` row, which is not a thing to include or exclude;
    /// for a `Plain` hit or file, where nothing is being chosen and so there is
    /// nothing to strike out; for a row that is not on screen; and when the
    /// panel cannot spare the cell. A file's mark is at the very edge, left of
    /// its disclosure triangle; a hit's is one column in.
    ///
    /// `rows` is the slice the panel was given, and `scroll` the scroll it was
    /// given with it, so this answers from the same two things `render` draws
    /// from.
    #[must_use]
    pub fn marker_area(
        area: Rect,
        rows: &[SearchRow<'_>],
        row: usize,
        scroll: usize,
    ) -> Option<Rect> {
        let column = match rows.get(row)? {
            SearchRow::Hit { state: HitState::Included | HitState::Excluded, .. } => MARKER_COL,
            SearchRow::File { state: HitState::Included | HitState::Excluded, .. } => {
                FILE_MARKER_COL
            }
            SearchRow::Hit { .. }
            | SearchRow::File { .. }
            | SearchRow::After { .. }
            | SearchRow::Operation { .. } => {
                return None;
            }
        };
        let rows_area = Self::rows_area(area);
        let offset = u16::try_from(row.checked_sub(scroll)?).ok()?;
        if offset >= rows_area.height {
            return None;
        }
        let x = rows_area.x.checked_add(column)?;
        (x < rows_area.right()).then(|| Rect::new(x, rows_area.y + offset, 1, 1))
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

    /// The char offset in `field`'s text that a click at column `x` lands on.
    ///
    /// A click inside a cluster lands in front of it, so a caret never ends up
    /// between a letter and the mark that belongs to it. Columns left of the
    /// text, the prompt's own included, land on the first char shown.
    ///
    /// `caret` is where the caret sat when the row was drawn, because the row
    /// scrolls to keep the caret on screen and which characters are under
    /// which columns is a question only the caret can answer. That is the
    /// caret handed to [`SearchView::editing`] when this field had the
    /// keyboard, and zero when the other one did — a field nobody is typing in
    /// is drawn from the start of its text.
    #[must_use]
    pub fn caret_at(area: Rect, field: Field, text: &str, caret: usize, x: u16) -> usize {
        let (text_x, room) = Self::field_columns(area, field);
        let start = query_window(room, text, caret.min(text.chars().count()));
        let Some(target) = x.checked_sub(text_x) else { return start };
        let target = usize::from(target);

        let mut offset = 0usize;
        let mut column = 0usize;
        for cluster in text.graphemes(true) {
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

    /// Where one field's own text goes: the column it starts at, and the
    /// columns it has.
    ///
    /// The apply button's columns are taken out of the replace row here, which
    /// is the only place that subtraction lives — so the text and the button
    /// cannot disagree about who owns the end of the row.
    fn field_columns(area: Rect, field: Field) -> (u16, u16) {
        let row = match field {
            Field::Query => Self::query_area(area),
            Field::Replace => Self::replace_area(area),
        };
        let text_x = row.x.saturating_add(PROMPT_COLS);
        let room = row.right().saturating_sub(text_x);
        let taken = match field {
            Field::Replace if Self::apply_area(area).is_some() => APPLY_COLS,
            Field::Query | Field::Replace => 0,
        };
        (text_x, room.saturating_sub(taken))
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
        self.draw_field(cells, area, Field::Query);
        self.draw_field(cells, area, Field::Replace);
        self.draw_apply(cells, area);
        self.draw_toggles(cells, area);
        self.draw_summary(cells, Self::summary_area(area));
        self.draw_rows(cells, Self::rows_area(area));
    }
}

impl SearchView<'_> {
    /// The result rows, into `rows_area`: this panel's list, and any other
    /// panel's that lists lines of files the same way.
    pub(crate) fn draw_rows(&self, cells: &mut Cells, rows_area: Rect) {
        let gutter = self.gutter();
        for (offset, index) in
            (self.scroll..self.rows.len()).take(usize::from(rows_area.height)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows_area.y + offset, height: 1, ..rows_area };
            self.draw_row(cells, line, index, gutter);
        }
    }

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
        clip::write(
            cells,
            x,
            area.y,
            area.width.saturating_sub(4),
            self.title,
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );

        let Some(cell) = Self::back_area(area) else { return };
        let style = if self.hovered_back {
            self.palette.on(Role::Accent, Role::OnAccent)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };
        clip::write(
            cells,
            cell.x,
            cell.y,
            1,
            self.palette.glyph(Glyph::SearchBack),
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );
    }

    fn draw_field(&self, cells: &mut Cells, area: Rect, field: Field) {
        let row = match field {
            Field::Query => Self::query_area(area),
            Field::Replace => Self::replace_area(area),
        };
        if row.height == 0 {
            return;
        }
        let (prompt, text, placeholder) = match field {
            Field::Query => (Glyph::SearchIcon, self.query, PLACEHOLDER),
            Field::Replace => (Glyph::SearchReplace, self.replacement, REPLACE_PLACEHOLDER),
        };
        let editing = self.editing == Some(field);

        // Washed while it has the keyboard, because a text field that looks
        // the same whether or not typing goes to it is a field people type
        // into by accident.
        let style = if editing {
            self.palette.on(Role::Selection, Role::Text)
        } else {
            self.palette.on(Role::Raised, Role::Text)
        };
        fill(cells, row, style);
        if row.width >= PROMPT_COLS {
            let prompt = format!("{} ", self.palette.glyph(prompt));
            clip::write(
                cells,
                row.x,
                row.y,
                PROMPT_COLS,
                &prompt,
                style.patch(self.palette.ink(Role::Dim)),
                self.palette.glyph(Glyph::Ellipsis),
            );
        }

        let (text_x, room) = Self::field_columns(area, field);
        if room == 0 {
            return;
        }
        if text.is_empty() && !editing {
            let faint = style.patch(self.palette.ink(Role::Faint));
            clip::write(
                cells,
                text_x,
                row.y,
                room,
                placeholder,
                faint,
                self.palette.glyph(Glyph::Ellipsis),
            );
            return;
        }

        // The window is anchored to the caret rather than to the start of the
        // text, so a query longer than the panel is still typed into at the
        // end of it rather than blind. A field nobody is typing in has no
        // caret of its own and so starts at the beginning, which is the same
        // rule with a caret of zero rather than a second rule.
        let caret = if editing { self.caret.min(text.chars().count()) } else { 0 };
        let start = query_window(room, text, caret);
        let tail = from_char(text, start);
        clip::write(cells, text_x, row.y, room, tail, style, self.palette.glyph(Glyph::Ellipsis));
        if !editing {
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
        let width = u16::try_from(cells[(caret_x, row.y)].symbol().width()).unwrap_or(1).max(1);
        for extra in 0..width {
            let cell_x = caret_x.saturating_add(extra);
            if cell_x < row.right() {
                cells[(cell_x, row.y)].set_style(caret_style);
            }
        }
    }

    fn draw_apply(&self, cells: &mut Cells, area: Rect) {
        let Some(cell) = Self::apply_area(area) else { return };
        // The one control in the panel that is not drawn in the accent. Every
        // other button here is reversible by pressing it again; this one
        // rewrites files, so it keeps the warning colour in both states rather
        // than lighting up invitingly under the pointer. Hover is marked the
        // way an unlit toggle marks it, with the cursor-line wash.
        let mut style = self.palette.on(Role::Raised, Role::Warn);
        if self.hovered_apply {
            style = style.patch(self.palette.cursor_line());
        }
        clip::write(
            cells,
            cell.x,
            cell.y,
            1,
            self.palette.glyph(Glyph::SearchApply),
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );
    }

    fn draw_toggles(&self, cells: &mut Cells, area: Rect) {
        if !self.actions.is_empty() {
            self.draw_actions(cells, area);
            return;
        }
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
            clip::write(
                cells,
                cell.x,
                cell.y,
                1,
                self.palette.glyph(button.glyph()),
                style,
                self.palette.glyph(Glyph::Ellipsis),
            );
        }
    }

    fn draw_actions(&self, cells: &mut Cells, area: Rect) {
        for (index, label) in self.actions.iter().enumerate() {
            let Some(cell) = Self::action_area(area, self.actions, index) else { break };
            // Drawn the way the status line's prompt buttons are, since they
            // are the same kind of thing: a decision, one click away.
            let style = if self.hovered_action == Some(index) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Overlay, Role::Text)
            };
            clip::write(
                cells,
                cell.x,
                cell.y,
                cell.width,
                &format!(" {label} "),
                style,
                self.palette.glyph(Glyph::Ellipsis),
            );
        }
    }

    fn draw_summary(&self, cells: &mut Cells, area: Rect) {
        let Some(summary) = self.summary else { return };
        if area.height == 0 {
            return;
        }
        let x = area.x.saturating_add(1);
        let style = self.palette.on(Role::Raised, Role::Faint);
        clip::write(
            cells,
            x,
            area.y,
            area.right().saturating_sub(x),
            summary,
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );
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
                SearchRow::Hit { line, .. } | SearchRow::After { line, .. } => Some(digits(*line)),
                SearchRow::File { .. } | SearchRow::Operation { .. } => None,
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
            SearchRow::File { path, hits, collapsed, state } => {
                self.draw_file(cells, line, style, path, hits, collapsed, state);
            }
            SearchRow::Hit { line: number, text, matched, state } => {
                self.draw_hit(cells, line, style, gutter, number, text, matched, state);
            }
            SearchRow::After { line: number, text } => {
                self.draw_after(cells, line, style, gutter, number, text);
            }
            SearchRow::Operation { text } => self.draw_operation(cells, line, style, text),
        }
    }

    /// A file operation: the included mark where a file's tick goes, and the
    /// sentence where its path goes.
    fn draw_operation(&self, cells: &mut Cells, line: Rect, style: Style, text: &str) {
        let ellipsis = self.palette.glyph(Glyph::Ellipsis);
        if let Some(x) = line.x.checked_add(FILE_MARKER_COL)
            && x < line.right()
        {
            let ink = style.patch(self.palette.ink(Role::Added));
            clip::write(
                cells,
                x,
                line.y,
                1,
                self.palette.glyph(Glyph::ReplaceIncluded),
                ink,
                ellipsis,
            );
        }
        let x = line.x.saturating_add(3);
        let room = line.right().saturating_sub(x);
        clip::write(cells, x, line.y, room, text, style, ellipsis);
    }

    /// The marker in a row's gutter column, and the ink the row's text takes.
    ///
    /// The two come from one place because they are one decision: the marker
    /// says what is happening to the line and the colour says the same thing
    /// again, and a row where they disagreed would be a row you cannot read.
    fn diff_marks(&self, state: Option<HitState>) -> (&str, Option<Role>) {
        match state {
            Some(HitState::Plain) => (" ", None),
            Some(HitState::Included) => {
                (self.palette.glyph(Glyph::ReplaceRemoved), Some(Role::Removed))
            }
            Some(HitState::Excluded) => {
                (self.palette.glyph(Glyph::ReplaceExcluded), Some(Role::Faint))
            }
            // `None` is an `After` row: the line as it would be written.
            None => (self.palette.glyph(Glyph::ReplaceAdded), Some(Role::Added)),
        }
    }

    // Each one is a separate thing to draw, as in the palette's own row.
    #[allow(clippy::too_many_arguments)]
    fn draw_file(
        &self,
        cells: &mut Cells,
        line: Rect,
        style: Style,
        path: &str,
        hits: usize,
        collapsed: bool,
        state: HitState,
    ) {
        // A file that is in gets a tick where its hits get their `-`, and one
        // that is out is dimmed as a whole, the way a struck-out hit is.
        let (mark, style) = match state {
            HitState::Plain => (None, style),
            HitState::Included => (
                Some((
                    self.palette.glyph(Glyph::ReplaceIncluded),
                    style.patch(self.palette.ink(Role::Added)),
                )),
                style,
            ),
            HitState::Excluded => {
                let faint = style.patch(self.palette.ink(Role::Faint));
                (Some((self.palette.glyph(Glyph::ReplaceExcluded), faint)), faint)
            }
        };
        if let Some((mark, ink)) = mark
            && let Some(x) = line.x.checked_add(FILE_MARKER_COL)
            && x < line.right()
        {
            clip::write(cells, x, line.y, 1, mark, ink, self.palette.glyph(Glyph::Ellipsis));
        }
        let x = line.x.saturating_add(1);
        let disclosure = if collapsed { Glyph::TreeCollapsed } else { Glyph::TreeExpanded };
        let disclosure = format!("{} ", self.palette.glyph(disclosure));
        let room = line.right().saturating_sub(x);
        clip::write(
            cells,
            x,
            line.y,
            room,
            &disclosure,
            style.patch(self.palette.ink(Role::Dim)),
            self.palette.glyph(Glyph::Ellipsis),
        );

        let count = hits.to_string();
        let count_cols = u16::try_from(count.width()).unwrap_or(0);
        let path_x = x.saturating_add(2);
        // The count keeps its columns and the path yields to it: a truncated
        // path still says which file, a truncated count says nothing.
        let room = line.right().saturating_sub(path_x).saturating_sub(count_cols + 1);
        clip::write(cells, path_x, line.y, room, path, style, self.palette.glyph(Glyph::Ellipsis));

        if let Some(count_x) = line.right().checked_sub(count_cols + 1)
            && count_x >= path_x
        {
            clip::write(
                cells,
                count_x,
                line.y,
                count_cols,
                &count,
                style.patch(self.palette.ink(Role::Dim)),
                self.palette.glyph(Glyph::Ellipsis),
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
        state: HitState,
    ) {
        let (marker, ink) = self.diff_marks(Some(state));
        let style = self.draw_line_start(cells, line, style, gutter, number, marker, ink);

        let text_x = text_column(line, gutter);
        let room = line.right().saturating_sub(text_x);
        // The ramp has no role of its own for a match, and the accent is the
        // one colour nun has for "this is what you asked about". It stays on
        // an excluded line too: where the match is, is why the line is here.
        let matched_style = style.patch(self.palette.ink(Role::Accent));
        put_matched(
            cells,
            text_x,
            line.y,
            room,
            text,
            matched,
            style,
            matched_style,
            self.palette.glyph(Glyph::Ellipsis),
        );
    }

    fn draw_after(
        &self,
        cells: &mut Cells,
        line: Rect,
        style: Style,
        gutter: u16,
        number: u32,
        text: &str,
    ) {
        let (marker, ink) = self.diff_marks(None);
        let style = self.draw_line_start(cells, line, style, gutter, number, marker, ink);

        let text_x = text_column(line, gutter);
        let room = line.right().saturating_sub(text_x);
        // Nothing is picked out of it: the whole line is what changed.
        clip::write(cells, text_x, line.y, room, text, style, self.palette.glyph(Glyph::Ellipsis));
    }

    /// The marker and the line number a hit or an after row starts with, and
    /// the style the rest of that row is drawn in.
    #[allow(clippy::too_many_arguments)] // Each one is a separate thing to draw.
    fn draw_line_start(
        &self,
        cells: &mut Cells,
        line: Rect,
        style: Style,
        gutter: u16,
        number: u32,
        marker: &str,
        ink: Option<Role>,
    ) -> Style {
        let ink = ink.map_or(style, |role| style.patch(self.palette.ink(role)));
        if let Some(x) = line.x.checked_add(MARKER_COL)
            && x < line.right()
        {
            clip::write(cells, x, line.y, 1, marker, ink, self.palette.glyph(Glyph::Ellipsis));
        }

        let x = line.x.saturating_add(1).saturating_add(INDENT);
        let number = format!("{number:>width$}", width = usize::from(gutter));
        let room = line.right().saturating_sub(x).min(gutter);
        clip::write(
            cells,
            x,
            line.y,
            room,
            &number,
            style.patch(self.palette.ink(Role::Dim)),
            self.palette.glyph(Glyph::Ellipsis),
        );
        ink
    }
}

/// Where a hit's or an after row's text starts, so the two line up under each
/// other and a diff reads down the column rather than across it.
fn text_column(line: Rect, gutter: u16) -> u16 {
    line.x.saturating_add(1).saturating_add(INDENT).saturating_add(gutter).saturating_add(1)
}

/// The `index`-th row of `area`, zero-height when `area` is not that tall.
pub(crate) fn band(area: Rect, index: u16) -> Rect {
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

pub(crate) fn fill(cells: &mut Cells, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            cells[(x, y)].set_char(' ').set_style(style);
        }
    }
}

/// Write `text` like [`clip::write`], with the char ranges in `matched`
/// picked out. A range running past the end of `text` — which happens when
/// the engine windowed a long line — simply stops matching.
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
    ellipsis: &str,
) {
    clip::write_styled(cells, x, y, room, text, style, ellipsis, |chars| {
        let hit = matched.iter().any(|range| range.start < chars.end && chars.start < range.end);
        if hit { matched_style } else { style }
    });
}
