//! The tab strip.
//!
//! One row above the text, one tab per open file. A tab shows its label, a dot
//! when the file has unsaved changes, and a close cross when the pointer is on
//! it or it is the active one — a cross on every tab would be noise, and a
//! cross on none would leave closing to the keyboard.
//!
//! When the tabs do not fit, the strip scrolls rather than shrinking labels
//! into illegibility. Geometry is exposed so the layout pass and the drawing
//! agree on where every tab is.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// Widest a tab is allowed to be, however long its label.
const MAX_WIDTH: u16 = 28;
/// A tab's own padding: a space each side, plus the dot or cross column.
const PADDING: u16 = 4;

/// One tab's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// What it says.
    pub label: String,
    /// Whether the file has unsaved changes.
    pub modified: bool,
}

/// The strip, drawn.
#[derive(Debug)]
pub struct TabStrip<'a> {
    tabs: &'a [Tab],
    palette: &'a Palette,
    active: usize,
    hovered: Option<usize>,
    scroll: u16,
    /// Where a tab being dragged would be dropped.
    drop_at: Option<usize>,
}

impl<'a> TabStrip<'a> {
    /// A strip of `tabs` with `active` selected.
    #[must_use]
    pub const fn new(tabs: &'a [Tab], palette: &'a Palette, active: usize) -> Self {
        Self { tabs, palette, active, hovered: None, scroll: 0, drop_at: None }
    }

    /// The tab under the pointer.
    #[must_use]
    pub const fn hovered(mut self, tab: Option<usize>) -> Self {
        self.hovered = tab;
        self
    }

    /// How far the strip is scrolled, in cells.
    #[must_use]
    pub const fn scrolled_by(mut self, cells: u16) -> Self {
        self.scroll = cells;
        self
    }

    /// Where a dragged tab would land, drawn as a line between tabs.
    #[must_use]
    pub const fn drop_at(mut self, index: Option<usize>) -> Self {
        self.drop_at = index;
        self
    }

    /// How wide each tab is, in order.
    #[must_use]
    pub fn widths(tabs: &[Tab]) -> Vec<u16> {
        tabs.iter()
            .map(|tab| {
                let label = u16::try_from(tab.label.width()).unwrap_or(MAX_WIDTH);
                label.saturating_add(PADDING).min(MAX_WIDTH)
            })
            .collect()
    }

    /// Where each tab sits in `area`, given the strip's scroll. Tabs scrolled
    /// off either end come back empty.
    #[must_use]
    pub fn layout(tabs: &[Tab], area: Rect, scroll: u16) -> Vec<Rect> {
        let mut x = i32::from(area.x) - i32::from(scroll);
        let mut areas = Vec::with_capacity(tabs.len());
        for width in Self::widths(tabs) {
            let left = x.max(i32::from(area.x));
            let right = (x + i32::from(width)).min(i32::from(area.right()));
            let visible = u16::try_from((right - left).max(0)).unwrap_or(0);
            let left = u16::try_from(left.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            areas.push(Rect::new(left, area.y, visible, area.height.min(1)));
            x += i32::from(width);
        }
        areas
    }

    /// Total width of every tab, scrolled or not.
    #[must_use]
    pub fn total_width(tabs: &[Tab]) -> u16 {
        Self::widths(tabs).iter().fold(0u16, |total, width| total.saturating_add(*width))
    }

    /// The scroll that brings `index` fully into `area`, given the current one.
    #[must_use]
    pub fn scroll_to(tabs: &[Tab], area: Rect, index: usize, scroll: u16) -> u16 {
        let widths = Self::widths(tabs);
        let before: u16 = widths.iter().take(index).fold(0u16, |total, w| total.saturating_add(*w));
        let width = widths.get(index).copied().unwrap_or(0);
        if before < scroll {
            return before;
        }
        let end = before.saturating_add(width);
        if end > scroll.saturating_add(area.width) {
            return end.saturating_sub(area.width);
        }
        scroll
    }

    /// The close cross of a tab drawn at `area`, if it is wide enough to have
    /// one.
    #[must_use]
    pub fn close_area(area: Rect) -> Option<Rect> {
        (area.width >= PADDING).then(|| Rect::new(area.right() - 2, area.y, 1, 1))
    }

    /// Which gap between tabs a pointer at `x` is nearest: the index a tab
    /// dropped there would take.
    #[must_use]
    pub fn drop_index(tabs: &[Tab], area: Rect, scroll: u16, x: u16) -> usize {
        let areas = Self::layout(tabs, area, scroll);
        for (index, tab) in areas.iter().enumerate() {
            if tab.width > 0 && x < tab.x + tab.width / 2 {
                return index;
            }
        }
        tabs.len()
    }
}

impl Widget for TabStrip<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let strip = Rect { height: 1, ..area };
        let ground = self.palette.on(Role::Sunken, Role::Dim);
        for x in strip.left()..strip.right() {
            cells[(x, strip.y)].set_char(' ').set_style(ground);
        }

        for (index, (tab, at)) in
            self.tabs.iter().zip(TabStrip::layout(self.tabs, strip, self.scroll)).enumerate()
        {
            if at.width == 0 {
                continue;
            }
            let active = index == self.active;
            let style = if active {
                self.palette.on(Role::Ground, Role::Text)
            } else if self.hovered == Some(index) {
                self.palette.on(Role::Raised, Role::Text)
            } else {
                self.palette.on(Role::Sunken, Role::Dim)
            };
            for x in at.left()..at.right() {
                cells[(x, at.y)].set_char(' ').set_style(style);
            }

            let room = at.width.saturating_sub(PADDING);
            write_clipped(cells, at.x + 1, at.y, room, &tab.label, style);

            // The dot and the cross share a column: an unsaved tab shows the
            // dot until the pointer arrives to close it.
            if let Some(close) = TabStrip::close_area(at) {
                let show_close = active || self.hovered == Some(index);
                let (glyph, glyph_style) = match (show_close, tab.modified) {
                    (true, _) => ("×", style),
                    (false, true) => ("•", style.patch(self.palette.ink(Role::Accent))),
                    (false, false) => (" ", style),
                };
                cells[(close.x, close.y)].set_symbol(glyph).set_style(glyph_style);
            }
        }

        // Where a dragged tab would land.
        if let Some(index) = self.drop_at {
            let areas = TabStrip::layout(self.tabs, strip, self.scroll);
            let x = areas
                .get(index)
                .map_or_else(|| areas.last().map_or(strip.x, |last| last.right()), |at| at.x);
            if x >= strip.x && x < strip.right() {
                cells[(x, strip.y)].set_symbol("▏").set_style(self.palette.ink(Role::Accent));
            }
        }
    }
}

/// Write `text` in at most `room` columns, ending in `…` when it does not fit.
fn write_clipped(
    cells: &mut Cells,
    x: u16,
    y: u16,
    room: u16,
    text: &str,
    style: ratatui::style::Style,
) {
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
