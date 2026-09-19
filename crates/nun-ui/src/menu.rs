//! A small floating menu, opened where the pointer is.
//!
//! The binary decides what the items mean; this draws them and says where
//! they are, so the layout pass and the drawing agree on every row.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// One entry: what it says, and the key that does the same thing, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// The label.
    pub label: String,
    /// The keyboard equivalent, shown right-aligned and dimmed.
    pub hint: String,
}

/// A menu, drawn.
#[derive(Debug)]
pub struct Menu<'a> {
    items: &'a [MenuItem],
    palette: &'a Palette,
    hovered: Option<usize>,
}

impl<'a> Menu<'a> {
    /// A menu of `items`.
    #[must_use]
    pub const fn new(items: &'a [MenuItem], palette: &'a Palette) -> Self {
        Self { items, palette, hovered: None }
    }

    /// The item under the pointer.
    #[must_use]
    pub const fn hovered(mut self, item: Option<usize>) -> Self {
        self.hovered = item;
        self
    }

    /// Where a menu of `items` opened at `(x, y)` goes on `screen`.
    ///
    /// It opens down and to the right of the pointer, and flips to open up or
    /// to the left rather than running off the edge.
    #[must_use]
    pub fn area(items: &[MenuItem], x: u16, y: u16, screen: Rect) -> Rect {
        let widest = items.iter().map(|item| item.label.width() + item.hint.width() + 4).max();
        let width =
            u16::try_from(widest.unwrap_or(4).max(12)).unwrap_or(u16::MAX).min(screen.width);
        let height = u16::try_from(items.len()).unwrap_or(u16::MAX).min(screen.height);

        let x = if x.saturating_add(width) > screen.right() { screen.right() - width } else { x };
        let y = if y.saturating_add(height) > screen.bottom() {
            y.saturating_sub(height.saturating_sub(1)).max(screen.y)
        } else {
            y
        };
        Rect::new(x, y, width, height)
    }

    /// The item drawn at screen row `y` of a menu at `area`.
    #[must_use]
    pub fn item_at(area: Rect, y: u16, items: usize) -> Option<usize> {
        if y < area.y || y >= area.bottom() {
            return None;
        }
        let index = usize::from(y - area.y);
        (index < items).then_some(index)
    }
}

impl Widget for Menu<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        for (index, item) in self.items.iter().enumerate().take(usize::from(area.height)) {
            let Ok(offset) = u16::try_from(index) else { break };
            let y = area.y + offset;
            let (style, hint_style) = if self.hovered == Some(index) {
                let on = self.palette.on(Role::Accent, Role::OnAccent);
                (on, on)
            } else {
                (
                    self.palette.on(Role::Overlay, Role::Text),
                    self.palette.on(Role::Overlay, Role::Dim),
                )
            };
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(style);
            }
            cells.set_stringn(
                area.x + 1,
                y,
                &item.label,
                usize::from(area.width.saturating_sub(2)),
                style,
            );
            let hint_width = u16::try_from(item.hint.width()).unwrap_or(0);
            if let Some(hint_x) = area.right().checked_sub(hint_width + 1)
                && !item.hint.is_empty()
            {
                cells.set_stringn(hint_x, y, &item.hint, usize::from(hint_width), hint_style);
            }
        }
    }
}
