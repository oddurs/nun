//! The command palette: one overlay, several modes.
//!
//! What it searches is decided by the first character typed — nothing for
//! files, `>` for commands, `:` for a line number, `?` for help. One overlay
//! with prefixes beats six dialogs, and it means there is one thing to learn
//! and one place to look.
//!
//! Each row shows what it is and, where there is one, the key that does the
//! same thing — which is what makes the palette the keymap reference too.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::clip;
use crate::glyph::Glyph;
use crate::style::Palette;

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What it says.
    pub label: String,
    /// Char offsets of `label` that matched what was typed.
    pub matched: Vec<u32>,
    /// The key that does the same thing, or a note about the row.
    pub hint: String,
}

/// The palette, drawn.
#[derive(Debug)]
pub struct PaletteView<'a> {
    palette: &'a Palette,
    query: &'a str,
    placeholder: &'a str,
    entries: &'a [Entry],
    selected: usize,
    scroll: usize,
    hovered: Option<usize>,
}

/// Rows of results, at most.
pub const MOST_ROWS: u16 = 12;

impl<'a> PaletteView<'a> {
    /// A palette showing `entries` for `query`.
    #[must_use]
    pub const fn new(palette: &'a Palette, query: &'a str, entries: &'a [Entry]) -> Self {
        Self { palette, query, placeholder: "", entries, selected: 0, scroll: 0, hovered: None }
    }

    /// What to show when nothing has been typed.
    #[must_use]
    pub const fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    /// Which row the keyboard is on.
    #[must_use]
    pub const fn selected(mut self, row: usize) -> Self {
        self.selected = row;
        self
    }

    /// The first row shown.
    #[must_use]
    pub const fn scrolled_to(mut self, row: usize) -> Self {
        self.scroll = row;
        self
    }

    /// The row under the pointer.
    #[must_use]
    pub const fn hovered(mut self, row: Option<usize>) -> Self {
        self.hovered = row;
        self
    }

    /// Where the palette goes on `screen`: centred, near the top, because
    /// that is where the eye already is when it opens.
    #[must_use]
    pub fn area(screen: Rect, rows: usize) -> Rect {
        let width = (screen.width * 3 / 4).clamp(20.min(screen.width), 90);
        let rows = u16::try_from(rows).unwrap_or(MOST_ROWS).min(MOST_ROWS);
        // One row for what is typed, one line under it, then the results.
        let height = (rows + 2).min(screen.height);
        let x = screen.x + (screen.width.saturating_sub(width)) / 2;
        let y = screen.y + (screen.height / 8).min(screen.height.saturating_sub(height));
        Rect::new(x, y, width, height)
    }

    /// The row of results drawn at screen row `y`.
    #[must_use]
    pub fn row_at(area: Rect, scroll: usize, y: u16, rows: usize) -> Option<usize> {
        let top = area.y + 2;
        if y < top || y >= area.bottom() {
            return None;
        }
        let index = scroll + usize::from(y - top);
        (index < rows).then_some(index)
    }

    /// How many results fit.
    #[must_use]
    pub const fn visible_rows(area: Rect) -> usize {
        area.height.saturating_sub(2) as usize
    }

    /// The scroll that keeps `selected` in view.
    #[must_use]
    pub const fn scroll_to(area: Rect, selected: usize, scroll: usize) -> usize {
        let visible = Self::visible_rows(area);
        if selected < scroll {
            return selected;
        }
        if visible > 0 && selected >= scroll + visible {
            return selected + 1 - visible;
        }
        scroll
    }
}

impl Widget for PaletteView<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width < 4 || area.height < 2 {
            return;
        }
        let ground = self.palette.on(Role::Overlay, Role::Text);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(ground);
            }
        }

        // What has been typed, then the caret. With nothing typed the caret
        // comes first and the placeholder follows it, so the caret never sits
        // on top of the first letter of the suggestion.
        let caret = area.x
            + 1
            + u16::try_from(self.query.width()).unwrap_or(0).min(area.width.saturating_sub(3));
        if self.query.is_empty() {
            let room = area.right().saturating_sub(caret + 3);
            clip::write(
                cells,
                caret + 2,
                area.y,
                room,
                self.placeholder,
                self.palette.on(Role::Overlay, Role::Faint),
                self.palette.glyph(Glyph::Ellipsis),
            );
        } else {
            clip::write(
                cells,
                area.x + 1,
                area.y,
                area.width.saturating_sub(2),
                self.query,
                ground,
                self.palette.glyph(Glyph::Ellipsis),
            );
        }
        cells[(caret, area.y)]
            .set_char(' ')
            .set_style(self.palette.on(Role::Accent, Role::OnAccent));

        // A hairline under the query.
        let line = self.palette.on(Role::Overlay, Role::Line);
        for x in area.left()..area.right() {
            cells[(x, area.y + 1)]
                .set_symbol(self.palette.glyph(Glyph::RuleHorizontal))
                .set_style(line);
        }

        let visible = PaletteView::visible_rows(area);
        for (offset, index) in (self.scroll..self.entries.len()).take(visible).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let y = area.y + 2 + offset;
            let entry = &self.entries[index];

            let row_style = if index == self.selected {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else if self.hovered == Some(index) {
                self.palette.on(Role::Raised, Role::Text)
            } else {
                ground
            };
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(row_style);
            }

            // The hint first, so the label may run under it only if it must.
            let hint_width = u16::try_from(entry.hint.width()).unwrap_or(0);
            let room = area.width.saturating_sub(hint_width + 3);
            write_matched(
                cells,
                area.x + 1,
                y,
                room,
                entry,
                row_style,
                self.palette.on(Role::Overlay, Role::Accent),
                index == self.selected,
                self.palette.glyph(Glyph::Ellipsis),
            );
            if hint_width > 0
                && let Some(x) = area.right().checked_sub(hint_width + 1)
            {
                let hint_style = if index == self.selected {
                    row_style
                } else {
                    self.palette.on(Role::Overlay, Role::Dim)
                };
                clip::write(
                    cells,
                    x,
                    y,
                    hint_width,
                    &entry.hint,
                    hint_style,
                    self.palette.glyph(Glyph::Ellipsis),
                );
            }
        }
    }
}

/// Write a label with the characters that matched picked out.
#[allow(clippy::too_many_arguments)] // Each one is a separate thing to draw.
pub(crate) fn write_matched(
    cells: &mut Cells,
    x: u16,
    y: u16,
    room: u16,
    entry: &Entry,
    style: ratatui::style::Style,
    matched_style: ratatui::style::Style,
    selected: bool,
    ellipsis: &str,
) {
    clip::write_styled(cells, x, y, room, &entry.label, style, ellipsis, |chars| {
        let matched = chars.into_iter().any(|char| entry.matched.contains(&char));
        // On the selected row the wash already carries the accent, so the
        // matched characters are marked by weight instead.
        match (matched, selected) {
            (true, false) => matched_style,
            (true, true) => style.add_modifier(ratatui::style::Modifier::BOLD),
            (false, _) => style,
        }
    });
}
