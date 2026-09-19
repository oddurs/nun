//! The file tree sidebar.
//!
//! Draws a window of a [`nun_workspace::FileTree`]'s rows — only the rows that
//! fit, however large the tree — under a one-row header of buttons. Geometry is
//! exposed as associated functions so the binary lays out exactly the hit
//! regions this draws, from one source.

use nun_theme::Role;
use nun_workspace::{Kind, Row};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// Columns each level of nesting indents by.
const INDENT: u16 = 2;

/// A button in the sidebar's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeButton {
    /// Create a file in the selected directory.
    NewFile,
    /// Create a directory in the selected directory.
    NewDir,
    /// Show or hide ignored files.
    ToggleIgnored,
}

impl TreeButton {
    /// Every button, right to left as they sit in the header.
    pub const ALL: [Self; 3] = [Self::ToggleIgnored, Self::NewDir, Self::NewFile];

    /// What the button shows. One cell wide each, so the header's geometry
    /// never depends on a font.
    #[must_use]
    pub const fn glyph(self, showing_ignored: bool) -> &'static str {
        match self {
            Self::NewFile => "+",
            Self::NewDir => "▪",
            Self::ToggleIgnored if showing_ignored => "●",
            Self::ToggleIgnored => "○",
        }
    }

    /// What it does, for a tooltip or the status line on hover.
    #[must_use]
    pub const fn describe(self, showing_ignored: bool) -> &'static str {
        match self {
            Self::NewFile => "New file",
            Self::NewDir => "New folder",
            Self::ToggleIgnored if showing_ignored => "Hide ignored files",
            Self::ToggleIgnored => "Show ignored files",
        }
    }
}

/// A window of the tree, drawn.
#[derive(Debug)]
pub struct TreeView<'a> {
    title: &'a str,
    rows: &'a [Row],
    palette: &'a Palette,
    scroll: usize,
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_button: Option<TreeButton>,
    drop_target: Option<usize>,
    showing_ignored: bool,
    focused: bool,
}

impl<'a> TreeView<'a> {
    /// A view of `rows` under a header reading `title`.
    #[must_use]
    pub const fn new(title: &'a str, rows: &'a [Row], palette: &'a Palette) -> Self {
        Self {
            title,
            rows,
            palette,
            scroll: 0,
            selected: None,
            hovered: None,
            hovered_button: None,
            drop_target: None,
            showing_ignored: false,
            focused: false,
        }
    }

    /// The first row shown.
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

    /// The header button under the pointer.
    #[must_use]
    pub const fn hovered_button(mut self, button: Option<TreeButton>) -> Self {
        self.hovered_button = button;
        self
    }

    /// The directory row a dragged entry would be dropped into.
    #[must_use]
    pub const fn drop_target(mut self, row: Option<usize>) -> Self {
        self.drop_target = row;
        self
    }

    /// Whether ignored entries are being listed, for the toggle's glyph.
    #[must_use]
    pub const fn showing_ignored(mut self, showing: bool) -> Self {
        self.showing_ignored = showing;
        self
    }

    /// Whether the sidebar has the keyboard.
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The header row of `area`.
    #[must_use]
    pub fn header_area(area: Rect) -> Rect {
        Rect { height: area.height.min(1), ..area }
    }

    /// Where the rows go, below the header.
    #[must_use]
    pub fn rows_area(area: Rect) -> Rect {
        let header = area.height.min(1);
        Rect { y: area.y + header, height: area.height - header, ..area }
    }

    /// The cell a header button occupies, if the sidebar is wide enough to
    /// show it.
    #[must_use]
    pub fn button_area(area: Rect, button: TreeButton) -> Option<Rect> {
        let index = TreeButton::ALL.iter().position(|b| *b == button)?;
        // One column of margin on the right, then each button with a space
        // before it.
        let offset = u16::try_from(2 + index * 2).ok()?;
        let x = area.right().checked_sub(offset)?;
        (x > area.x + 1 && area.height > 0).then(|| Rect::new(x, area.y, 1, 1))
    }

    /// How many rows fit.
    #[must_use]
    pub fn visible_rows(area: Rect) -> usize {
        usize::from(Self::rows_area(area).height)
    }

    /// The tree row drawn at screen row `y`, given the scroll.
    #[must_use]
    pub fn row_at(area: Rect, scroll: usize, y: u16, rows: usize) -> Option<usize> {
        let rows_area = Self::rows_area(area);
        if y < rows_area.y || y >= rows_area.bottom() {
            return None;
        }
        let index = scroll + usize::from(y - rows_area.y);
        (index < rows).then_some(index)
    }
}

impl Widget for TreeView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let ground = self.palette.on(Role::Raised, Role::Text);
        fill(cells, area, ground);

        self.draw_header(cells, Self::header_area(area));

        let rows_area = Self::rows_area(area);
        for (offset, index) in
            (self.scroll..self.rows.len()).take(usize::from(rows_area.height)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows_area.y + offset, height: 1, ..rows_area };
            self.draw_row(cells, line, index);
        }
    }
}

impl TreeView<'_> {
    fn draw_header(&self, cells: &mut Cells, area: Rect) {
        if area.height == 0 {
            return;
        }
        // Headed in the accent while the sidebar has the keyboard, so which
        // half of the screen keys go to is visible even before anything in it
        // is selected.
        let role = if self.focused { Role::Accent } else { Role::Dim };
        let style = self.palette.on(Role::Raised, role).add_modifier(Modifier::BOLD);
        let title = self.title.to_uppercase();
        let limit = area.width.saturating_sub(8);
        put(cells, area.x + 1, area.y, limit, &title, style);

        for button in TreeButton::ALL {
            let Some(cell) = Self::button_area(area, button) else { continue };
            let style = if self.hovered_button == Some(button) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Dim)
            };
            put(cells, cell.x, cell.y, 1, button.glyph(self.showing_ignored), style);
        }
    }

    fn draw_row(&self, cells: &mut Cells, line: Rect, index: usize) {
        let row = &self.rows[index];

        let mut style = self.palette.on(Role::Raised, Role::Text);
        if row.ignored || row.error.is_some() {
            style = self.palette.on(Role::Raised, Role::Faint);
        }
        if self.hovered == Some(index) {
            style = style.patch(self.palette.cursor_line());
        }
        if self.selected == Some(index) {
            let wash = if self.focused { Role::Selection } else { Role::CursorLine };
            style = style.patch(self.palette.on(wash, Role::Text));
        }
        if self.drop_target == Some(index) {
            style = self.palette.on(Role::Accent, Role::OnAccent);
        }
        fill(cells, line, style);

        let depth = u16::try_from(row.depth).unwrap_or(u16::MAX);
        let indent = line.x.saturating_add(1).saturating_add(depth.saturating_mul(INDENT));
        let disclosure = match (row.kind, row.expanded) {
            (Kind::Dir, true) => "▾ ",
            (Kind::Dir, false) => "▸ ",
            (Kind::Symlink, _) => "↪ ",
            (Kind::File, _) => "  ",
        };
        let marker_style = if self.drop_target == Some(index) {
            style
        } else {
            style.patch(self.palette.ink(Role::Dim))
        };
        let room = line.right().saturating_sub(indent);
        put(cells, indent, line.y, room, disclosure, marker_style);

        let name_x = indent.saturating_add(2);
        let room = line.right().saturating_sub(name_x);
        let name = match &row.error {
            Some(error) => format!("{} — {error}", row.name),
            None => row.name.clone(),
        };
        put(cells, name_x, line.y, room, &name, style);
    }
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
