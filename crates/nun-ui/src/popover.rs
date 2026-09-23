//! A card that floats beside something on screen: a diagnostic's message, a
//! hover card, anything short that is about one place.
//!
//! Three promises, which are what make a card usable rather than in the way:
//!
//! * **It never covers what it is about.** The card goes wholly below its
//!   anchor, or wholly above it — never over it, even when neither side has
//!   room for all of it. It is cut short and scrolls instead.
//! * **It stays on screen.** It is placed inside the bounds it is given,
//!   sliding left rather than running off the right edge.
//! * **The layout and the drawing agree.** [`Popover::place`] and
//!   [`Popover::button_areas`] are what the binary pushes into its hit map,
//!   and the drawing uses the same arithmetic, so a click lands on the button
//!   under it.
//!
//! The body is paragraphs of styled runs, wrapped to the card's width at word
//! boundaries. Buttons, if any, sit on the last row and are always visible;
//! the body scrolls above them.

use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::style::Palette;

/// The widest a card gets, padding included. Past this, prose is hard to
/// read and the card starts to feel like a panel.
pub const MOST_WIDTH: u16 = 72;

/// The tallest a card gets, buttons included. Past this it scrolls.
pub const MOST_HEIGHT: u16 = 14;

/// Columns of padding each side of the text.
const PADDING: u16 = 1;

/// A stretch of text in one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The text. A newline in it starts a new row.
    pub text: String,
    /// The role it is drawn in.
    pub role: Role,
    /// Whether it is bold.
    pub bold: bool,
}

impl Run {
    /// Text in `role`.
    #[must_use]
    pub fn new(text: impl Into<String>, role: Role) -> Self {
        Self { text: text.into(), role, bold: false }
    }

    /// Text in `role`, bold.
    #[must_use]
    pub fn bold(text: impl Into<String>, role: Role) -> Self {
        Self { text: text.into(), role, bold: true }
    }
}

/// Runs wrapped together as one block. An empty paragraph is a blank row.
pub type Paragraph = Vec<Run>;

/// A card, drawn.
#[derive(Debug)]
pub struct Popover<'a> {
    body: &'a [Paragraph],
    buttons: &'a [String],
    palette: &'a Palette,
    scroll: usize,
    hovered: Option<usize>,
}

/// One row of wrapped body: pieces of runs, each with the run it came from.
type Row<'t> = Vec<(&'t str, &'t Run)>;

impl<'a> Popover<'a> {
    /// A card holding `body`.
    #[must_use]
    pub const fn new(body: &'a [Paragraph], palette: &'a Palette) -> Self {
        Self { body, buttons: &[], palette, scroll: 0, hovered: None }
    }

    /// Buttons along the bottom row, in order.
    #[must_use]
    pub const fn buttons(mut self, labels: &'a [String]) -> Self {
        self.buttons = labels;
        self
    }

    /// Scroll the body down by `rows`. Clamped when drawn.
    #[must_use]
    pub const fn scrolled_to(mut self, rows: usize) -> Self {
        self.scroll = rows;
        self
    }

    /// The button under the pointer.
    #[must_use]
    pub const fn hovered(mut self, button: Option<usize>) -> Self {
        self.hovered = button;
        self
    }

    /// Where the card goes, for an anchor at `anchor` on a screen whose
    /// usable part is `bounds`. `None` when there is no room on either side
    /// of the anchor, or nothing to show.
    ///
    /// Below the anchor if the whole card fits there, else above if it fits
    /// there, else on whichever side has more room, cut to fit.
    #[must_use]
    pub fn place(&self, anchor: Rect, bounds: Rect) -> Option<Rect> {
        if bounds.width <= PADDING * 2 || bounds.height == 0 {
            return None;
        }
        let width = self.natural_width().min(MOST_WIDTH).min(bounds.width);
        let height = self.height_at(width);
        if height == 0 {
            return None;
        }

        let below = bounds.bottom().saturating_sub(anchor.bottom().max(bounds.top()));
        let above = anchor.top().min(bounds.bottom()).saturating_sub(bounds.top());
        let (y, height) = if height <= below {
            (anchor.bottom().max(bounds.top()), height)
        } else if height <= above {
            (anchor.top().min(bounds.bottom()) - height, height)
        } else if below >= above {
            (anchor.bottom().max(bounds.top()), below)
        } else {
            (bounds.top(), above)
        };
        // Buttons and at least one row of body, or nothing.
        let least = if self.buttons.is_empty() { 1 } else { 2 };
        if height < least {
            return None;
        }
        let x = anchor.x.min(bounds.right().saturating_sub(width)).max(bounds.x);
        Some(Rect::new(x, y, width, height))
    }

    /// Rows the body wraps to at a card `width` wide.
    #[must_use]
    pub fn body_rows(&self, width: u16) -> usize {
        self.wrap(width).len()
    }

    /// The furthest the body can scroll in a card at `area`.
    #[must_use]
    pub fn most_scroll(&self, area: Rect) -> usize {
        self.body_rows(area.width).saturating_sub(usize::from(self.body_height(area)))
    }

    /// Where each button is, in a card at `area`. A button that does not fit
    /// has an empty rectangle, so the indices still line up.
    #[must_use]
    pub fn button_areas(&self, area: Rect) -> Vec<Rect> {
        if self.buttons.is_empty() || area.height == 0 {
            return Vec::new();
        }
        let y = area.bottom() - 1;
        let mut x = area.x + PADDING;
        let right = area.right().saturating_sub(PADDING);
        self.buttons
            .iter()
            .map(|label| {
                let width = u16::try_from(label.width() + 2).unwrap_or(u16::MAX);
                if x.saturating_add(width) > right {
                    return Rect::new(x, y, 0, 0);
                }
                let at = Rect::new(x, y, width, 1);
                x += width + 1;
                at
            })
            .collect()
    }

    /// Rows of body that fit in a card at `area`, after the buttons.
    fn body_height(&self, area: Rect) -> u16 {
        area.height.saturating_sub(u16::from(!self.buttons.is_empty()))
    }

    /// The width the content wants, padding included.
    fn natural_width(&self) -> u16 {
        let body = self
            .body
            .iter()
            .flat_map(|paragraph| {
                // A paragraph's rows, split at its newlines, measured whole.
                let text: String = paragraph.iter().map(|run| run.text.as_str()).collect();
                text.split('\n').map(UnicodeWidthStr::width).collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0);
        let buttons: usize =
            self.buttons.iter().map(|label| label.width() + 3).sum::<usize>().saturating_sub(1);
        let text = u16::try_from(body.max(buttons)).unwrap_or(u16::MAX);
        text.saturating_add(PADDING * 2)
    }

    /// The height the card wants at `width`, buttons included.
    fn height_at(&self, width: u16) -> u16 {
        let rows = u16::try_from(self.body_rows(width)).unwrap_or(u16::MAX);
        rows.saturating_add(u16::from(!self.buttons.is_empty())).min(MOST_HEIGHT)
    }

    /// The body wrapped to a card `width` wide.
    fn wrap(&self, width: u16) -> Vec<Row<'a>> {
        let room = usize::from(width.saturating_sub(PADDING * 2)).max(1);
        let mut rows = Vec::new();
        for paragraph in self.body {
            let mut row: Row<'a> = Vec::new();
            let mut used = 0;
            let mut any = false;
            for run in paragraph {
                for (index, line) in run.text.split('\n').enumerate() {
                    if index > 0 {
                        rows.push(std::mem::take(&mut row));
                        used = 0;
                    }
                    any = true;
                    for word in line.split_word_bounds() {
                        let mut word = word;
                        loop {
                            let width = word.width();
                            if used + width <= room {
                                row.push((word, run));
                                used += width;
                                break;
                            }
                            if used > 0 {
                                rows.push(std::mem::take(&mut row));
                                used = 0;
                                // A space that would start a row is dropped.
                                if word.trim().is_empty() {
                                    break;
                                }
                                continue;
                            }
                            // A word wider than the card on its own: break it
                            // between graphemes.
                            let (head, tail) = split_at_width(word, room);
                            row.push((head, run));
                            rows.push(std::mem::take(&mut row));
                            word = tail;
                            if word.is_empty() {
                                break;
                            }
                        }
                    }
                }
            }
            if any || paragraph.is_empty() || !row.is_empty() {
                rows.push(row);
            }
        }
        rows
    }
}

/// Split `text` after as many graphemes as fit in `room` columns — at least
/// one, so a single character wider than the card still makes progress.
fn split_at_width(text: &str, room: usize) -> (&str, &str) {
    let mut used = 0;
    let mut end = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let width = grapheme.width();
        if used + width > room && end > 0 {
            break;
        }
        used += width;
        end = index + grapheme.len();
    }
    text.split_at(end)
}

impl Widget for Popover<'_> {
    fn render(self, area: Rect, cells: &mut Cells) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let surface = self.palette.on(Role::Overlay, Role::Text);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_char(' ').set_style(surface);
            }
        }

        let rows = self.wrap(area.width);
        let height = usize::from(self.body_height(area));
        let scroll = self.scroll.min(rows.len().saturating_sub(height));
        let right = area.right().saturating_sub(PADDING);
        for (offset, row) in rows.iter().skip(scroll).take(height).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let y = area.top() + offset;
            let mut x = area.left() + PADDING;
            for (piece, run) in row {
                let mut style = self.palette.on(Role::Overlay, run.role);
                if run.bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                for grapheme in piece.graphemes(true) {
                    let width = u16::try_from(grapheme.width()).unwrap_or(1);
                    if width == 0 || x + width > right {
                        continue;
                    }
                    cells[(x, y)].set_symbol(grapheme).set_style(style);
                    x += width;
                }
            }
        }

        // More above or below is said in the padding column, where it takes
        // nothing from the text.
        let edge = area.right() - 1;
        let hint = self.palette.on(Role::Overlay, Role::Dim);
        if scroll > 0 {
            cells[(edge, area.top())].set_char('▴').set_style(hint);
        }
        if scroll + height < rows.len() && height > 0 {
            let Ok(last) = u16::try_from(height - 1) else { return };
            cells[(edge, area.top() + last)].set_char('▾').set_style(hint);
        }

        for (index, (label, at)) in self.buttons.iter().zip(self.button_areas(area)).enumerate() {
            if at.width == 0 {
                continue;
            }
            let style = if self.hovered == Some(index) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, Role::Text)
            };
            let mut x = at.x;
            for grapheme in format!(" {label} ").graphemes(true) {
                cells[(x, at.y)].set_symbol(grapheme).set_style(style);
                x += u16::try_from(grapheme.width()).unwrap_or(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nun_theme::{Probe, derive};
    use proptest::prelude::*;

    use super::*;

    fn palette() -> Palette {
        Palette::new(derive(&Probe::builtin_dark()))
    }

    fn body(text: &str) -> Vec<Paragraph> {
        text.split("\n\n").map(|p| vec![Run::new(p, Role::Text)]).collect()
    }

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };

    #[test]
    fn a_card_goes_below_its_anchor_when_it_fits() {
        let palette = palette();
        let body = body("mismatched types");
        let card = Popover::new(&body, &palette).place(Rect::new(10, 5, 4, 1), SCREEN).unwrap();
        assert_eq!((card.x, card.y), (10, 6));
        assert_eq!(card.width, 16 + 2, "as wide as its text, plus padding");
    }

    #[test]
    fn a_card_near_the_bottom_goes_above() {
        let palette = palette();
        let body = body("one\n two\n three");
        let card = Popover::new(&body, &palette).place(Rect::new(0, 22, 3, 1), SCREEN).unwrap();
        assert_eq!(card.bottom(), 22, "ends right above the anchor");
    }

    #[test]
    fn a_card_near_the_right_edge_slides_left_to_stay_on_screen() {
        let palette = palette();
        let body = body("a message that is quite long");
        let card = Popover::new(&body, &palette).place(Rect::new(78, 3, 2, 1), SCREEN).unwrap();
        assert_eq!(card.right(), SCREEN.right());
    }

    #[test]
    fn a_long_message_wraps_at_words_and_scrolls() {
        let palette = palette();
        let long = "word ".repeat(200);
        let body = body(&long);
        let popover = Popover::new(&body, &palette);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        assert_eq!(card.width, MOST_WIDTH);
        assert_eq!(card.height, MOST_HEIGHT);
        assert!(popover.most_scroll(card) > 0);

        let mut cells = Cells::empty(SCREEN);
        Popover::new(&body, &palette).render(card, &mut cells);
        let first: String = (card.x..card.right()).map(|x| cells[(x, card.y)].symbol()).collect();
        assert!(first.trim_end().ends_with("word"), "no word is cut in half: {first:?}");
        assert_eq!(cells[(card.right() - 1, card.bottom() - 1)].symbol(), "▾", "more below");
    }

    #[test]
    fn buttons_sit_on_the_last_row_where_the_layout_says() {
        let palette = palette();
        let body = body("unused variable");
        let buttons = vec!["Next".to_string(), "Previous".to_string()];
        let popover = Popover::new(&body, &palette).buttons(&buttons);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        let areas = popover.button_areas(card);
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].y, card.bottom() - 1);

        let mut cells = Cells::empty(SCREEN);
        Popover::new(&body, &palette).buttons(&buttons).render(card, &mut cells);
        let drawn: String =
            (areas[1].x..areas[1].right()).map(|x| cells[(x, areas[1].y)].symbol()).collect();
        assert_eq!(drawn, " Previous ");
    }

    #[test]
    fn with_no_room_either_side_there_is_no_card() {
        let palette = palette();
        let body = body("x");
        let bounds = Rect::new(0, 0, 80, 1);
        assert_eq!(Popover::new(&body, &palette).place(Rect::new(0, 0, 1, 1), bounds), None);
    }

    #[test]
    fn wide_characters_wrap_without_being_split() {
        let palette = palette();
        let body = body(&"中".repeat(100));
        let popover = Popover::new(&body, &palette);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        let mut cells = Cells::empty(SCREEN);
        popover.render(card, &mut cells);
        let row: String = (card.x..card.right()).map(|x| cells[(x, card.y)].symbol()).collect();
        assert!(row.contains('中'));
    }

    proptest! {
        /// Wherever the anchor is and whatever the card holds, the card is on
        /// screen and does not overlap the anchor.
        #[test]
        fn a_card_never_covers_its_anchor_and_never_leaves_the_screen(
            x in 0u16..80, y in 0u16..24, width in 1u16..20,
            words in 1usize..300, buttons in 0usize..3,
            bounds_top in 0u16..4, bounds_height in 1u16..24,
        ) {
            let palette = palette();
            let body = body(&"lorem ipsum ".repeat(words));
            let labels: Vec<String> = (0..buttons).map(|i| format!("Button {i}")).collect();
            let bounds = Rect::new(0, bounds_top, 80, bounds_height.min(24 - bounds_top));
            let anchor = Rect::new(x, y, width.min(80 - x), 1).intersection(bounds);
            prop_assume!(!anchor.is_empty());
            let popover = Popover::new(&body, &palette).buttons(&labels);
            if let Some(card) = popover.place(anchor, bounds) {
                prop_assert!(bounds.contains(card.as_position()));
                prop_assert!(card.right() <= bounds.right() && card.bottom() <= bounds.bottom());
                prop_assert!(!card.intersects(anchor), "{card:?} covers {anchor:?}");
                for button in popover.button_areas(card) {
                    prop_assert!(button.width == 0 || card.contains(button.as_position()));
                }
            }
        }
    }
}
