//! A file's changes against an older version of it, side by side or unified.
//!
//! The view is a list of rows, worked out once per diff by [`align`]: the
//! lines both versions share, a header for each hunk, and the changed lines
//! between. Side by side, a hunk's old and new lines are paired row for row
//! and the shorter side is padded with filler rows, so the two columns always
//! show the same place in the file and scroll as one: there is only one
//! scroll offset, into the rows. Unified, a hunk is its old lines and then its
//! new ones, and needs no filler.
//!
//! Text is drawn the way the editor draws it — syntax colours from the same
//! roles — over a wash of [`Role::AddedWash`] or [`Role::RemovedWash`], with
//! the words that changed within a line washed more strongly.
//!
//! Pure layout and drawing: what the rows are, where each button is, and
//! what goes in each cell. Which hunk is staged, and what a click does, is
//! the editor's business.

use std::collections::HashMap;
use std::ops::Range;

use nun_syntax::Span;
use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use ropey::Rope;

use crate::clip::{self, text_width};
use crate::glyph::Glyph;
use crate::style::Palette;

/// How the two versions are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffLayout {
    /// The old version on the left, the new on the right.
    #[default]
    Split,
    /// One column: each hunk's old lines, then its new ones.
    Unified,
}

impl DiffLayout {
    /// The other one.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Split => Self::Unified,
            Self::Unified => Self::Split,
        }
    }
}

/// One hunk, as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// Its lines in the old version, counting from zero.
    pub before: Range<u32>,
    /// Its lines in the new version.
    pub after: Range<u32>,
    /// Whether it can be staged as it stands, so its header offers to.
    pub stageable: bool,
}

/// One row of the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    /// A line both versions have, and where it is in each.
    Context {
        /// Its line in the old version.
        before: u32,
        /// Its line in the new version.
        after: u32,
    },
    /// The row above a hunk, which names it and offers to stage it.
    Header {
        /// Which hunk, by its place in the list.
        hunk: usize,
    },
    /// A changed line. Side by side a row can hold one of each, or one and
    /// filler; unified, exactly one side is there.
    Change {
        /// Which hunk it belongs to.
        hunk: usize,
        /// Its line in the old version, if this row has one.
        before: Option<u32>,
        /// Its line in the new version, if this row has one.
        after: Option<u32>,
    },
}

impl DiffRow {
    /// The line of the new version this row is at or next to, for going to
    /// it in the editor: a removed line has none of its own, so it goes to
    /// where the removal was.
    #[must_use]
    pub fn after_line(self, hunks: &[DiffHunk]) -> Option<u32> {
        match self {
            Self::Context { after, .. } | Self::Change { after: Some(after), .. } => Some(after),
            Self::Header { hunk } | Self::Change { hunk, after: None, .. } => {
                hunks.get(hunk).map(|hunk| hunk.after.start)
            }
        }
    }

    /// Which hunk it belongs to, if any.
    #[must_use]
    pub const fn hunk(self) -> Option<usize> {
        match self {
            Self::Context { .. } => None,
            Self::Header { hunk } | Self::Change { hunk, .. } => Some(hunk),
        }
    }
}

/// The rows for a diff between an old text of `before` lines and a new one
/// of `after` lines, whose changes are `hunks`, in order.
///
/// Lines are counted the way git counts them: a final newline ends the last
/// line rather than starting another. See [`line_count`].
#[must_use]
pub fn align(hunks: &[DiffHunk], before: u32, after: u32, layout: DiffLayout) -> Vec<DiffRow> {
    let mut rows = Vec::with_capacity(after.max(before) as usize + hunks.len() * 2);
    let (mut old, mut new) = (0u32, 0u32);
    let context =
        |rows: &mut Vec<DiffRow>, old: &mut u32, new: &mut u32, to_old: u32, to_new: u32| {
            // The two gaps are equal for any diff git produces; the shorter is
            // taken so a malformed one can never walk off the end of a side.
            let run = to_old.saturating_sub(*old).min(to_new.saturating_sub(*new));
            rows.extend(
                (0..run).map(|step| DiffRow::Context { before: *old + step, after: *new + step }),
            );
            *old += run;
            *new += run;
        };
    for (index, hunk) in hunks.iter().enumerate() {
        context(&mut rows, &mut old, &mut new, hunk.before.start, hunk.after.start);
        rows.push(DiffRow::Header { hunk: index });
        let (removed, added) = (hunk.before.clone(), hunk.after.clone());
        match layout {
            DiffLayout::Split => {
                let height = removed.len().max(added.len());
                for step in 0..u32::try_from(height).unwrap_or(u32::MAX) {
                    let line = |range: &Range<u32>| {
                        Some(range.start + step).filter(|line| range.contains(line))
                    };
                    rows.push(DiffRow::Change {
                        hunk: index,
                        before: line(&removed),
                        after: line(&added),
                    });
                }
            }
            DiffLayout::Unified => {
                rows.extend(removed.clone().map(|line| DiffRow::Change {
                    hunk: index,
                    before: Some(line),
                    after: None,
                }));
                rows.extend(added.clone().map(|line| DiffRow::Change {
                    hunk: index,
                    before: None,
                    after: Some(line),
                }));
            }
        }
        old = old.max(removed.end);
        new = new.max(added.end);
    }
    context(&mut rows, &mut old, &mut new, before, after);
    rows
}

/// How many lines `text` has, counted the way git and [`align`] count them:
/// a final newline ends a line rather than starting one, and an empty text
/// has none.
#[must_use]
pub fn line_count(text: &Rope) -> u32 {
    let lines = text.len_lines();
    let chars = text.len_chars();
    let ended = chars == 0 || text.char(chars - 1) == '\n';
    u32::try_from(lines - usize::from(ended)).unwrap_or(u32::MAX)
}

/// The row that shows line `line` of the new version, or the nearest one
/// before it — for keeping the same place when the layout changes, and for
/// opening the view where the caret is.
#[must_use]
pub fn row_of_after(rows: &[DiffRow], line: u32) -> usize {
    let at = |row: &DiffRow| match *row {
        DiffRow::Context { after, .. } | DiffRow::Change { after: Some(after), .. } => Some(after),
        _ => None,
    };
    let mut best = 0;
    for (index, row) in rows.iter().enumerate() {
        match at(row) {
            Some(after) if after == line => return index,
            Some(after) if after > line => break,
            Some(_) => best = index,
            None => {}
        }
    }
    best
}

/// The header row of hunk `hunk`.
#[must_use]
pub fn header_row(rows: &[DiffRow], hunk: usize) -> Option<usize> {
    rows.iter().position(|row| *row == DiffRow::Header { hunk })
}

/// Where the words that changed within each line are, by line, as char
/// ranges into the line — one map for each side.
pub type Emphasis = HashMap<u32, Vec<Range<usize>>>;

/// One version of the file, as the view draws it.
#[derive(Debug, Clone, Copy)]
pub struct DiffSide<'a> {
    /// The whole text.
    pub text: &'a Rope,
    /// Its highlight runs, as char offsets into the whole text, in order.
    pub spans: &'a [Span],
    /// The words that changed, by line.
    pub emphasis: &'a Emphasis,
}

/// Something in the view the pointer can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSpot {
    /// The header row, away from its buttons.
    Header,
    /// One of the two layout buttons.
    Layout(DiffLayout),
    /// One of the two base buttons: the index, or `HEAD` when `true`.
    Base(bool),
    /// The cross that closes the view.
    Close,
    /// A hunk's Stage button.
    Stage(usize),
    /// A row, by its place in the rows.
    Row(usize),
    /// Below the last row.
    Empty,
}

/// What a hunk's header offers, as it is drawn.
const STAGE: &str = " Stage ";

/// The buttons in the header, and what each says, right to left.
const BUTTONS: [(DiffSpot, &str); 4] = [
    (DiffSpot::Base(true), " HEAD "),
    (DiffSpot::Base(false), " Index "),
    (DiffSpot::Layout(DiffLayout::Unified), " Unified "),
    (DiffSpot::Layout(DiffLayout::Split), " Split "),
];

/// A diff, drawn into a pane.
#[derive(Debug)]
pub struct DiffView<'a> {
    palette: &'a Palette,
    title: &'a str,
    note: Option<&'a str>,
    layout: DiffLayout,
    head: bool,
    rows: &'a [DiffRow],
    hunks: &'a [DiffHunk],
    before: Option<DiffSide<'a>>,
    after: Option<DiffSide<'a>>,
    scroll: usize,
    current: Option<usize>,
    hovered: Option<DiffSpot>,
    focused: bool,
    tab_width: usize,
}

impl<'a> DiffView<'a> {
    /// A view titled `title`, with nothing in it yet.
    #[must_use]
    pub const fn new(title: &'a str, palette: &'a Palette) -> Self {
        Self {
            palette,
            title,
            note: None,
            layout: DiffLayout::Split,
            head: false,
            rows: &[],
            hunks: &[],
            before: None,
            after: None,
            scroll: 0,
            current: None,
            hovered: None,
            focused: false,
            tab_width: 4,
        }
    }

    /// The rows, the hunks they refer to, and the two versions.
    #[must_use]
    pub const fn showing(
        mut self,
        rows: &'a [DiffRow],
        hunks: &'a [DiffHunk],
        before: DiffSide<'a>,
        after: DiffSide<'a>,
    ) -> Self {
        self.rows = rows;
        self.hunks = hunks;
        self.before = Some(before);
        self.after = Some(after);
        self
    }

    /// Something to say in place of the rows: why there are none.
    #[must_use]
    pub const fn note(mut self, note: Option<&'a str>) -> Self {
        self.note = note;
        self
    }

    /// Which layout, and whether the old version is `HEAD` rather than the
    /// index — for the header's buttons.
    #[must_use]
    pub const fn laid_out(mut self, layout: DiffLayout, head: bool) -> Self {
        self.layout = layout;
        self.head = head;
        self
    }

    /// The first row shown.
    #[must_use]
    pub const fn scrolled_to(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }

    /// The hunk the keyboard is on.
    #[must_use]
    pub const fn current(mut self, hunk: Option<usize>) -> Self {
        self.current = hunk;
        self
    }

    /// What the pointer is over.
    #[must_use]
    pub const fn hovered(mut self, spot: Option<DiffSpot>) -> Self {
        self.hovered = spot;
        self
    }

    /// Whether it has the keyboard.
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// How many columns a tab takes.
    #[must_use]
    pub const fn tab_width(mut self, width: usize) -> Self {
        self.tab_width = if width == 0 { 1 } else { width };
        self
    }

    /// How many rows fit below the header.
    #[must_use]
    pub const fn visible_rows(area: Rect) -> usize {
        area.height.saturating_sub(1) as usize
    }

    /// Everything the pointer can land on, in paint order: a later one is on
    /// top of an earlier one.
    #[must_use]
    pub fn spots(
        area: Rect,
        rows: &[DiffRow],
        hunks: &[DiffHunk],
        scroll: usize,
    ) -> Vec<(Rect, DiffSpot)> {
        if area.height == 0 || area.width == 0 {
            return Vec::new();
        }
        let mut spots = vec![(Rect { height: 1, ..area }, DiffSpot::Header)];
        spots.extend(Self::buttons(area));
        let body = Rect { y: area.y + 1, height: area.height - 1, ..area };
        spots.push((body, DiffSpot::Empty));
        for (offset, index) in (scroll..rows.len()).take(Self::visible_rows(area)).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let row = Rect { y: body.y + offset, height: 1, ..body };
            spots.push((row, DiffSpot::Row(index)));
            if let DiffRow::Header { hunk } = rows[index]
                && hunks.get(hunk).is_some_and(|hunk| hunk.stageable)
                && let Some(button) = Self::stage_area(row)
            {
                spots.push((button, DiffSpot::Stage(hunk)));
            }
        }
        spots
    }

    /// The header's buttons, as many as fit, the close cross first.
    fn buttons(area: Rect) -> Vec<(Rect, DiffSpot)> {
        let width = |label: &str| u16::try_from(text_width(label)).unwrap_or(u16::MAX);
        // Room is kept for the title to say at least which file it is.
        let floor = area.x + 8.min(area.width / 3);
        let Some(mut right) = area.right().checked_sub(3).filter(|x| *x >= floor) else {
            return Vec::new();
        };
        let mut out = vec![(Rect::new(right, area.y, 3, 1), DiffSpot::Close)];
        // Each pair goes in whole or not at all: half a toggle is a button
        // that only ever goes one way.
        for pair in BUTTONS.chunks(2) {
            let (first, second) = (width(pair[0].1), width(pair[1].1));
            let Some(x) = right.checked_sub(first + second + 1).filter(|x| *x >= floor) else {
                break;
            };
            out.push((Rect::new(x + second, area.y, first, 1), pair[0].0));
            out.push((Rect::new(x, area.y, second, 1), pair[1].0));
            right = x;
        }
        out
    }

    /// Where a hunk header's Stage button goes, if the row has room for it.
    fn stage_area(row: Rect) -> Option<Rect> {
        let width = u16::try_from(STAGE.len()).unwrap_or(u16::MAX);
        (row.width > width + 12).then(|| Rect::new(row.right() - width - 1, row.y, width, 1))
    }

    fn render_header(&self, area: Rect, cells: &mut Cells) {
        let ground = self.palette.on(Role::Raised, Role::Text);
        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(ground);
        }
        let buttons = Self::buttons(area);
        let left = buttons.iter().map(|(rect, _)| rect.x).min().unwrap_or(area.right());
        let hunks = match self.hunks.len() {
            _ if self.note.is_some() || self.before.is_none() => String::new(),
            1 => " · 1 change".to_string(),
            count => format!(" · {count} changes"),
        };
        let against = if self.head { "HEAD" } else { "the index" };
        let title = format!(" {} against {against}{hunks}", self.title);
        let role = if self.focused { Role::Accent } else { Role::Text };
        let style = self.palette.on(Role::Raised, role).add_modifier(Modifier::BOLD);
        let room = left.saturating_sub(area.x + 1);
        clip::write(
            cells,
            area.x,
            area.y,
            room,
            &title,
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );

        for (rect, spot) in buttons {
            let label = match spot {
                DiffSpot::Close => format!(" {} ", self.palette.glyph(Glyph::DiffClose)),
                _ => BUTTONS
                    .iter()
                    .find(|(button, _)| *button == spot)
                    .map_or_else(String::new, |(_, label)| (*label).to_string()),
            };
            let on = match spot {
                DiffSpot::Layout(layout) => layout == self.layout,
                DiffSpot::Base(head) => head == self.head,
                _ => false,
            };
            let style = if self.hovered == Some(spot) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else if on {
                self.palette.on(Role::Ground, Role::Text).add_modifier(Modifier::BOLD)
            } else {
                self.palette.on(Role::Raised, Role::Dim)
            };
            clip::write(cells, rect.x, rect.y, rect.width, &label, style, "");
        }
    }

    /// How wide the line numbers are: as wide as the longer side's last one.
    fn digits(&self) -> u16 {
        let most = [self.before, self.after]
            .into_iter()
            .flatten()
            .map(|side| line_count(side.text))
            .max()
            .unwrap_or(0);
        u16::try_from(most.to_string().len()).unwrap_or(u16::MAX).max(2)
    }

    fn render_row(&self, index: usize, area: Rect, cells: &mut Cells) {
        match self.rows[index] {
            DiffRow::Header { hunk } => self.render_hunk_header(hunk, area, cells),
            DiffRow::Context { before, after } => match self.layout {
                DiffLayout::Split => {
                    let (left, right) = halves(area);
                    self.render_line(Some(before), Which::Before, Wash::None, left, cells);
                    self.render_divider(area, left, cells);
                    self.render_line(Some(after), Which::After, Wash::None, right, cells);
                }
                DiffLayout::Unified => self.render_unified(Some(before), Some(after), area, cells),
            },
            DiffRow::Change { before, after, .. } => match self.layout {
                DiffLayout::Split => {
                    let (left, right) = halves(area);
                    self.render_line(before, Which::Before, Wash::Removed, left, cells);
                    self.render_divider(area, left, cells);
                    self.render_line(after, Which::After, Wash::Added, right, cells);
                }
                DiffLayout::Unified => self.render_unified(before, after, area, cells),
            },
        }
    }

    fn render_divider(&self, row: Rect, left: Rect, cells: &mut Cells) {
        if left.right() < row.right() {
            cells[(left.right(), row.y)]
                .set_symbol(self.palette.glyph(Glyph::RuleVertical))
                .set_style(self.palette.fg(Role::Line));
        }
    }

    fn render_hunk_header(&self, hunk: usize, area: Rect, cells: &mut Cells) {
        let Some(found) = self.hunks.get(hunk) else { return };
        let current = self.current == Some(hunk);
        let ground = self.palette.on(Role::Sunken, Role::Dim);
        for x in area.left()..area.right() {
            cells[(x, area.y)]
                .set_symbol(self.palette.glyph(Glyph::RuleHorizontal))
                .set_style(self.palette.on(Role::Sunken, Role::Line));
        }
        // Numbered from one, the way git's own hunk headers are.
        // A side with no lines is placed after the line before the gap, as
        // git places it.
        let span = |range: &Range<u32>| match range.len() {
            0 => format!("{},0", range.start),
            1 => format!("{}", range.start + 1),
            count => format!("{},{count}", range.start + 1),
        };
        let label = format!(" -{} +{} ", span(&found.before), span(&found.after));
        let style = if current {
            self.palette.on(Role::Sunken, Role::Accent).add_modifier(Modifier::BOLD)
        } else {
            ground
        };
        let stage = found.stageable.then(|| Self::stage_area(area)).flatten();
        let room = stage.map_or(area.right(), |stage| stage.x).saturating_sub(area.x + 1);
        clip::write(cells, area.x + 1, area.y, room, &label, style, "");
        if let Some(stage) = stage {
            let style = if self.hovered == Some(DiffSpot::Stage(hunk)) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Text)
            };
            clip::write(cells, stage.x, stage.y, stage.width, STAGE, style, "");
        }
    }

    /// One side of a split row: its number, its sign, and its text, or
    /// filler where it has no line.
    fn render_line(
        &self,
        line: Option<u32>,
        which: Which,
        wash: Wash,
        area: Rect,
        cells: &mut Cells,
    ) {
        if area.width == 0 {
            return;
        }
        let Some(line) = line else {
            self.render_filler(area, cells);
            return;
        };
        let digits = self.digits();
        let base = self.base_style(wash);
        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(base);
        }
        let number = format!("{:>width$}", line + 1, width = usize::from(digits));
        let gutter = self.palette.gutter(false).patch(self.wash_of(wash));
        clip::write(cells, area.x, area.y, digits.min(area.width), &number, gutter, "");
        let sign_x = area.x + digits + 1;
        if sign_x < area.right() {
            self.render_sign(wash, sign_x, area.y, cells);
        }
        let text = Rect {
            x: (area.x + digits + 3).min(area.right()),
            width: area.width.saturating_sub(digits + 3),
            ..area
        };
        self.render_text(line, which, wash, text, cells);
    }

    /// A unified row: both numbers, a sign, and whichever line it has.
    fn render_unified(
        &self,
        before: Option<u32>,
        after: Option<u32>,
        area: Rect,
        cells: &mut Cells,
    ) {
        let wash = match (before, after) {
            (Some(_), None) => Wash::Removed,
            (None, Some(_)) => Wash::Added,
            _ => Wash::None,
        };
        let digits = self.digits();
        let base = self.base_style(wash);
        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(base);
        }
        let gutter = self.palette.gutter(false).patch(self.wash_of(wash));
        for (column, line) in [(0, before), (digits + 1, after)] {
            if let Some(line) = line {
                let number = format!("{:>width$}", line + 1, width = usize::from(digits));
                let room = area.width.saturating_sub(column).min(digits);
                clip::write(cells, area.x + column, area.y, room, &number, gutter, "");
            }
        }
        let sign_x = area.x + 2 * digits + 2;
        if sign_x < area.right() {
            self.render_sign(wash, sign_x, area.y, cells);
        }
        let gutter_width = 2 * digits + 4;
        let text = Rect {
            x: (area.x + gutter_width).min(area.right()),
            width: area.width.saturating_sub(gutter_width),
            ..area
        };
        match (before, after) {
            (_, Some(line)) => self.render_text(line, Which::After, wash, text, cells),
            (Some(line), None) => self.render_text(line, Which::Before, wash, text, cells),
            (None, None) => {}
        }
    }

    fn render_sign(&self, wash: Wash, x: u16, y: u16, cells: &mut Cells) {
        let (glyph, role) = match wash {
            Wash::Added => (Glyph::DiffAdded, Role::Added),
            Wash::Removed => (Glyph::DiffRemoved, Role::Removed),
            Wash::None => return,
        };
        let style = self.base_style(wash).patch(self.palette.ink(role));
        cells[(x, y)].set_symbol(self.palette.glyph(glyph)).set_style(style);
    }

    fn render_filler(&self, area: Rect, cells: &mut Cells) {
        let style = self.palette.on(Role::Sunken, Role::Line);
        let hatch = self.palette.glyph(Glyph::DiffFiller);
        for x in area.left()..area.right() {
            cells[(x, area.y)].set_symbol(hatch).set_style(style);
        }
    }

    /// The text of one line, syntax-coloured, with its changed words washed.
    fn render_text(&self, line: u32, which: Which, wash: Wash, area: Rect, cells: &mut Cells) {
        let side = match which {
            Which::Before => self.before,
            Which::After => self.after,
        };
        let Some(side) = side else { return };
        if area.width == 0 || line as usize >= side.text.len_lines() {
            return;
        }
        let start = side.text.line_to_char(line as usize);
        let (shown, source) = expand(side.text.line(line as usize), self.tab_width);
        let start_u32 = u32::try_from(start).unwrap_or(u32::MAX);
        let end_u32 = start_u32.saturating_add(u32::try_from(source.len()).unwrap_or(u32::MAX));
        let first = side.spans.partition_point(|span| span.end <= start_u32);
        let spans: Vec<&Span> =
            side.spans[first..].iter().take_while(|span| span.start < end_u32).collect();
        let emphasis = side.emphasis.get(&line).map_or(&[][..], Vec::as_slice);
        let strong = match wash {
            Wash::Added => Some(Role::AddedEmphasis),
            Wash::Removed => Some(Role::RemovedEmphasis),
            Wash::None => None,
        };
        let base = self.base_style(wash);
        let style_of = |range: Range<u32>| {
            let Some(&at) = source.get(range.start as usize) else { return base };
            let mut style = base;
            let absolute = start_u32.saturating_add(at);
            if let Some(span) = spans.iter().find(|span| (span.start..span.end).contains(&absolute))
            {
                style = style.patch(self.palette.ink(crate::syntax::role_of(span.capture)));
            }
            if let Some(strong) = strong
                && emphasis.iter().any(|range| range.contains(&(at as usize)))
            {
                style = style.patch(self.palette.wash(strong));
            }
            style
        };
        clip::write_styled(
            cells,
            area.x,
            area.y,
            area.width,
            &shown,
            base,
            self.palette.glyph(Glyph::Ellipsis),
            style_of,
        );
    }

    fn wash_of(&self, wash: Wash) -> Style {
        match wash {
            Wash::Added => self.palette.wash(Role::AddedWash),
            Wash::Removed => self.palette.wash(Role::RemovedWash),
            Wash::None => Style::default(),
        }
    }

    fn base_style(&self, wash: Wash) -> Style {
        self.palette.text().patch(self.wash_of(wash))
    }

    fn render_note(&self, area: Rect, cells: &mut Cells) {
        let Some(note) = self.note else { return };
        if area.height == 0 {
            return;
        }
        let style = self.palette.fg(Role::Dim);
        clip::write(
            cells,
            area.x + 2.min(area.width),
            area.y + 1.min(area.height - 1),
            area.width.saturating_sub(4),
            note,
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );
    }
}

/// Which version a line is from.
#[derive(Debug, Clone, Copy)]
enum Which {
    Before,
    After,
}

/// What a row is washed with.
#[derive(Debug, Clone, Copy)]
enum Wash {
    None,
    Added,
    Removed,
}

/// A split row's two sides, and the column between them.
fn halves(area: Rect) -> (Rect, Rect) {
    let left = area.width.saturating_sub(1) / 2;
    let right_x = (area.x + left + 1).min(area.right());
    (Rect { width: left, ..area }, Rect { x: right_x, width: area.right() - right_x, ..area })
}

/// A line as it is drawn — tabs expanded, its line break dropped — and, for
/// each char drawn, the char of the line it came from.
fn expand(line: ropey::RopeSlice<'_>, tab_width: usize) -> (String, Vec<u32>) {
    let mut shown = String::with_capacity(line.len_bytes());
    let mut source = Vec::with_capacity(line.len_chars());
    let mut column = 0;
    for (index, ch) in line.chars().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        match ch {
            '\n' => break,
            '\t' => {
                let run = tab_width - column % tab_width;
                for _ in 0..run {
                    shown.push(' ');
                    source.push(index);
                }
                column += run;
            }
            ch => {
                shown.push(ch);
                source.push(index);
                // Close enough for tab stops: a wide character before a tab
                // is rare, and the editor makes the same approximation.
                column += 1;
            }
        }
    }
    (shown, source)
}

impl Widget for DiffView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let ground = self.palette.text();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(ground);
            }
        }
        self.render_header(Rect { height: 1, ..area }, cells);
        let body = Rect { y: area.y + 1, height: area.height - 1, ..area };
        if self.note.is_some() || self.before.is_none() {
            self.render_note(body, cells);
            return;
        }
        for (offset, index) in
            (self.scroll..self.rows.len()).take(Self::visible_rows(area)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            self.render_row(index, Rect { y: body.y + offset, height: 1, ..body }, cells);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Harness;
    use nun_theme::{Probe, derive};

    fn hunk(before: Range<u32>, after: Range<u32>) -> DiffHunk {
        DiffHunk { before, after, stageable: true }
    }

    #[test]
    fn side_by_side_pads_the_shorter_side_of_a_hunk_with_filler() {
        // a b c d  →  a B1 B2 B3 d: one line became three.
        let hunks = [hunk(1..3, 1..4)];
        let rows = align(&hunks, 4, 5, DiffLayout::Split);
        assert_eq!(
            rows,
            vec![
                DiffRow::Context { before: 0, after: 0 },
                DiffRow::Header { hunk: 0 },
                DiffRow::Change { hunk: 0, before: Some(1), after: Some(1) },
                DiffRow::Change { hunk: 0, before: Some(2), after: Some(2) },
                DiffRow::Change { hunk: 0, before: None, after: Some(3) },
                DiffRow::Context { before: 3, after: 4 },
            ]
        );
    }

    #[test]
    fn unified_lists_the_old_lines_then_the_new() {
        let hunks = [hunk(1..2, 1..3)];
        let rows = align(&hunks, 3, 4, DiffLayout::Unified);
        assert_eq!(
            rows[1..5],
            [
                DiffRow::Header { hunk: 0 },
                DiffRow::Change { hunk: 0, before: Some(1), after: None },
                DiffRow::Change { hunk: 0, before: None, after: Some(1) },
                DiffRow::Change { hunk: 0, before: None, after: Some(2) },
            ]
        );
        assert_eq!(rows[5], DiffRow::Context { before: 2, after: 3 });
    }

    #[test]
    fn every_line_of_both_sides_appears_once_and_in_order() {
        // Additions, removals, and a change at each end of the file.
        let hunks = [hunk(0..1, 0..2), hunk(3..5, 4..4), hunk(6..6, 5..7), hunk(8..9, 9..9)];
        for layout in [DiffLayout::Split, DiffLayout::Unified] {
            let rows = align(&hunks, 9, 9, layout);
            let side = |pick: fn(&DiffRow) -> Option<u32>| -> Vec<u32> {
                rows.iter().filter_map(pick).collect()
            };
            let before = side(|row| match *row {
                DiffRow::Context { before, .. } => Some(before),
                DiffRow::Change { before, .. } => before,
                DiffRow::Header { .. } => None,
            });
            let after = side(|row| match *row {
                DiffRow::Context { after, .. } => Some(after),
                DiffRow::Change { after, .. } => after,
                DiffRow::Header { .. } => None,
            });
            assert_eq!(before, (0..9).collect::<Vec<_>>(), "{layout:?}");
            assert_eq!(after, (0..9).collect::<Vec<_>>(), "{layout:?}");
        }
    }

    #[test]
    fn a_malformed_diff_never_runs_past_either_side() {
        let rows = align(&[hunk(5..9, 2..3)], 3, 3, DiffLayout::Split);
        for row in rows {
            if let DiffRow::Context { before, after } = row {
                assert!(before < 3 && after < 3);
            }
        }
    }

    #[test]
    fn lines_are_counted_the_way_git_counts_them() {
        assert_eq!(line_count(&Rope::from_str("")), 0);
        assert_eq!(line_count(&Rope::from_str("a")), 1);
        assert_eq!(line_count(&Rope::from_str("a\n")), 1);
        assert_eq!(line_count(&Rope::from_str("a\nb")), 2);
        assert_eq!(line_count(&Rope::from_str("a\n\n")), 2);
    }

    #[test]
    fn a_line_is_found_in_either_layout() {
        let hunks = [hunk(1..2, 1..3)];
        for layout in [DiffLayout::Split, DiffLayout::Unified] {
            let rows = align(&hunks, 3, 4, layout);
            let row = row_of_after(&rows, 2);
            assert_eq!(rows[row].after_line(&hunks), Some(2), "{layout:?}");
            assert_eq!(rows[row_of_after(&rows, 3)], DiffRow::Context { before: 2, after: 3 });
        }
        let rows = align(&hunks, 3, 4, DiffLayout::Split);
        assert_eq!(header_row(&rows, 0), Some(1));
        assert_eq!(rows[1].after_line(&hunks), Some(1));
    }

    fn palette() -> Palette {
        Palette::new(derive(&Probe::builtin_dark()))
    }

    struct Fixture {
        before: Rope,
        after: Rope,
        hunks: Vec<DiffHunk>,
        none: Emphasis,
        emphasis: Emphasis,
    }

    impl Fixture {
        fn new() -> Self {
            let before = Rope::from_str("one\nlet x = 1;\nthree\n");
            let after = Rope::from_str("one\nlet x = 2;\nnew\nthree\n");
            let mut emphasis = Emphasis::new();
            emphasis.insert(1, std::iter::once(8..9).collect());
            Self { before, after, hunks: vec![hunk(1..2, 1..3)], none: Emphasis::new(), emphasis }
        }

        fn draw(&self, layout: DiffLayout, width: u16) -> Harness {
            let rows = align(&self.hunks, 3, 4, layout);
            let palette = palette();
            let mut screen = Harness::new(width, 7);
            screen.draw(
                DiffView::new("main.rs", &palette)
                    .showing(
                        &rows,
                        &self.hunks,
                        DiffSide { text: &self.before, spans: &[], emphasis: &self.none },
                        DiffSide { text: &self.after, spans: &[], emphasis: &self.emphasis },
                    )
                    .laid_out(layout, false),
            );
            screen
        }
    }

    #[test]
    fn side_by_side_shows_both_versions_level_with_filler_where_one_has_nothing() {
        let fixture = Fixture::new();
        let screen = fixture.draw(DiffLayout::Split, 80);
        let text = screen.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("main.rs against the index"), "{text}");
        assert!(lines[0].contains("Split") && lines[0].contains("HEAD"), "{text}");
        assert!(lines[2].contains("-2"), "the hunk header numbers from one: {text}");
        assert!(lines[2].contains("Stage"), "{text}");
        assert!(lines[3].contains("let x = 1;") && lines[3].contains("let x = 2;"), "{text}");
        // The old side has no third line: the row is hatched, not blank.
        let hatch = palette().glyph(Glyph::DiffFiller).to_string();
        assert!(lines[4].starts_with(&hatch.repeat(5)), "{text}");
        assert!(lines[4].contains("new"), "{text}");
        assert!(lines[5].contains("three"), "{text}");
    }

    #[test]
    fn changed_lines_are_washed_and_the_changed_words_more_strongly() {
        let fixture = Fixture::new();
        let screen = fixture.draw(DiffLayout::Split, 60);
        let palette = palette();
        let cells = screen.cells();
        let row = 3;
        let find = |needle: char, from: u16| {
            (from..60).find(|x| cells[(*x, row)].symbol() == needle.to_string()).unwrap()
        };
        let wash = palette.wash(Role::AddedWash).bg;
        let strong = palette.wash(Role::AddedEmphasis).bg;
        let right = find('l', 30);
        assert_eq!(cells[(right, row)].bg, wash.unwrap(), "the line is washed");
        let two = find('2', right);
        assert_eq!(cells[(two, row)].bg, strong.unwrap(), "the changed word stands out");
        let left = find('l', 0);
        assert_eq!(cells[(left, row)].bg, palette.wash(Role::RemovedWash).bg.unwrap());
        // A line both versions share has no wash at all.
        let context = find('t', 30);
        assert_eq!(cells[(context, 5)].bg, palette.ground());
    }

    #[test]
    fn unified_signs_each_changed_line() {
        let fixture = Fixture::new();
        let screen = fixture.draw(DiffLayout::Unified, 40);
        let text = screen.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[3].contains("- let x = 1;"), "{text}");
        assert!(lines[4].contains("+ let x = 2;"), "{text}");
        assert!(lines[5].contains("+ new"), "{text}");
    }

    #[test]
    fn every_spot_is_inside_the_view_and_buttons_do_not_overlap() {
        let area = Rect::new(3, 2, 70, 8);
        let hunks = [hunk(1..2, 1..3)];
        let rows = align(&hunks, 3, 4, DiffLayout::Split);
        let spots = DiffView::spots(area, &rows, &hunks, 0);
        for (rect, spot) in &spots {
            assert!(area.contains(rect.as_position()), "{spot:?} at {rect:?}");
            assert!(rect.right() <= area.right(), "{spot:?} at {rect:?}");
        }
        let buttons: Vec<Rect> = spots
            .iter()
            .filter(|(_, spot)| {
                !matches!(spot, DiffSpot::Header | DiffSpot::Empty | DiffSpot::Row(_))
            })
            .map(|(rect, _)| *rect)
            .collect();
        for (index, one) in buttons.iter().enumerate() {
            for other in &buttons[index + 1..] {
                assert!(!one.intersects(*other), "{one:?} overlaps {other:?}");
            }
        }
        assert!(spots.iter().any(|(_, spot)| *spot == DiffSpot::Stage(0)));
        assert!(spots.iter().any(|(_, spot)| *spot == DiffSpot::Layout(DiffLayout::Unified)));
        // Narrow, the toggles give way before the cross does.
        let narrow = DiffView::spots(Rect::new(0, 0, 20, 4), &rows, &hunks, 0);
        assert!(narrow.iter().any(|(_, spot)| *spot == DiffSpot::Close));
    }

    #[test]
    fn a_wide_or_combining_line_is_never_split_and_tabs_expand() {
        let before = Rope::from_str("\tx\n");
        let after = Rope::from_str("中文e\u{301}\n");
        let hunks = vec![hunk(0..1, 0..1)];
        let rows = align(&hunks, 1, 1, DiffLayout::Unified);
        let none = Emphasis::new();
        let palette = palette();
        let mut screen = Harness::new(30, 5);
        screen.draw(
            DiffView::new("a", &palette)
                .showing(
                    &rows,
                    &hunks,
                    DiffSide { text: &before, spans: &[], emphasis: &none },
                    DiffSide { text: &after, spans: &[], emphasis: &none },
                )
                .laid_out(DiffLayout::Unified, false),
        );
        let text = screen.to_text();
        assert!(text.contains("-     x"), "a tab is spaces to the next stop: {text}");
        assert!(text.contains("中文e\u{301}"), "{text}");
    }
}
