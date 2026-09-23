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

use crate::clip;
use crate::glyph::Glyph;
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
    /// Swap the sidebar over to searching the project.
    Search,
}

impl TreeButton {
    /// Every button, right to left as they sit in the header.
    pub const ALL: [Self; 4] = [Self::Search, Self::ToggleIgnored, Self::NewDir, Self::NewFile];

    /// What the button shows. One cell wide each, so the header's geometry
    /// never depends on a font.
    #[must_use]
    pub const fn glyph(self, showing_ignored: bool) -> Glyph {
        match self {
            Self::NewFile => Glyph::TreeNewFile,
            Self::NewDir => Glyph::TreeNewFolder,
            Self::ToggleIgnored if showing_ignored => Glyph::TreeIgnoredShown,
            Self::ToggleIgnored => Glyph::TreeIgnoredHidden,
            // The same magnifier the status line's search button uses, so the
            // two ways into a search look like the same thing.
            Self::Search => Glyph::SearchIcon,
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
            Self::Search => "Search the project",
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
        // Room for the buttons on the right, which sit two columns apart
        // starting one in from the edge, plus a column between them and the
        // title so a long project name never runs into them.
        let buttons = u16::try_from(TreeButton::ALL.len()).unwrap_or(u16::MAX);
        let limit = area.width.saturating_sub(buttons.saturating_mul(2).saturating_add(2));
        clip::write(
            cells,
            area.x + 1,
            area.y,
            limit,
            self.title,
            style,
            self.palette.glyph(Glyph::Ellipsis),
        );

        for button in TreeButton::ALL {
            let Some(cell) = Self::button_area(area, button) else { continue };
            let style = if self.hovered_button == Some(button) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Dim)
            };
            clip::write(
                cells,
                cell.x,
                cell.y,
                1,
                self.palette.glyph(button.glyph(self.showing_ignored)),
                style,
                self.palette.glyph(Glyph::Ellipsis),
            );
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
            (Kind::Dir, true) => Some(Glyph::TreeExpanded),
            (Kind::Dir, false) => Some(Glyph::TreeCollapsed),
            (Kind::Symlink, _) => Some(Glyph::TreeSymlink),
            (Kind::File, _) => None,
        };
        let disclosure = disclosure
            .map_or_else(|| "  ".to_string(), |glyph| format!("{} ", self.palette.glyph(glyph)));
        let marker_style = if self.drop_target == Some(index) {
            style
        } else {
            style.patch(self.palette.ink(Role::Dim))
        };
        let room = line.right().saturating_sub(indent);
        clip::write(
            cells,
            indent,
            line.y,
            room,
            &disclosure,
            marker_style,
            self.palette.glyph(Glyph::Ellipsis),
        );

        let name_x = indent.saturating_add(2);
        let room = line.right().saturating_sub(name_x);
        let name = match &row.error {
            Some(error) => format!("{} — {error}", row.name),
            None => row.name.clone(),
        };
        clip::write(cells, name_x, line.y, room, &name, style, self.palette.glyph(Glyph::Ellipsis));
    }
}

fn fill(cells: &mut Cells, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            cells[(x, y)].set_char(' ').set_style(style);
        }
    }
}
