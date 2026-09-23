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

use std::num::NonZeroU16;

use nun_theme::Role;
use ratatui::buffer::{Buffer as Cells, CellDiffOption};
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
    /// Whether it is italic.
    pub italic: bool,
    /// The link it is the text of, as an index into the card's links. Drawn
    /// underlined, and clickable.
    pub link: Option<usize>,
}

impl Run {
    /// Text in `role`.
    #[must_use]
    pub fn new(text: impl Into<String>, role: Role) -> Self {
        Self { text: text.into(), role, bold: false, italic: false, link: None }
    }

    /// Text in `role`, bold.
    #[must_use]
    pub fn bold(text: impl Into<String>, role: Role) -> Self {
        Self { bold: true, ..Self::new(text, role) }
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
    /// Where each link goes, by the index a run's `link` names.
    links: &'a [String],
    /// Whether to tell the terminal, with OSC 8, which cells are links.
    hyperlinks: bool,
    hovered_link: Option<usize>,
}

/// One row of wrapped body: pieces of runs, each with the run it came from.
type Row<'t> = Vec<(&'t str, &'t Run)>;

impl<'a> Popover<'a> {
    /// A card holding `body`.
    #[must_use]
    pub const fn new(body: &'a [Paragraph], palette: &'a Palette) -> Self {
        Self {
            body,
            buttons: &[],
            palette,
            scroll: 0,
            hovered: None,
            links: &[],
            hyperlinks: false,
            hovered_link: None,
        }
    }

    /// Where the body's links go, by the index each run's `link` names.
    #[must_use]
    pub const fn links(mut self, links: &'a [String]) -> Self {
        self.links = links;
        self
    }

    /// Also mark each link's cells with OSC 8, so a terminal that knows it
    /// can show where the link goes, or copy it. Nothing depends on it: the
    /// link is drawn as a link and clicked through [`Popover::link_areas`]
    /// either way, and a terminal that does not know OSC 8 swallows it.
    #[must_use]
    pub const fn hyperlinks(mut self, on: bool) -> Self {
        self.hyperlinks = on;
        self
    }

    /// The link under the pointer.
    #[must_use]
    pub const fn hovered_link(mut self, link: Option<usize>) -> Self {
        self.hovered_link = link;
        self
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
                let width = u16::try_from(drawn_width(label) + 2).unwrap_or(u16::MAX);
                if x.saturating_add(width) > right {
                    return Rect::new(x, y, 0, 0);
                }
                let at = Rect::new(x, y, width, 1);
                x += width + 1;
                at
            })
            .collect()
    }

    /// Where each link's text is, in a card at `area`: one rectangle per row
    /// of it in view, with the index of the link.
    #[must_use]
    pub fn link_areas(&self, area: Rect) -> Vec<(usize, Rect)> {
        let mut areas: Vec<(usize, Rect)> = Vec::new();
        self.lay_out(area, |x, y, _, width, run| {
            let Some(link) = run.link else { return };
            match areas.last_mut() {
                Some((last, rect)) if *last == link && rect.y == y && rect.right() == x => {
                    rect.width += width;
                }
                _ => areas.push((link, Rect::new(x, y, width, 1))),
            }
        });
        areas
    }

    /// Every grapheme of the body in view in a card at `area`, where it is
    /// drawn: its column and row, the grapheme, its width, and its run. The
    /// one place the drawing and the link areas both come from.
    fn lay_out(&self, area: Rect, mut put: impl FnMut(u16, u16, &'a str, u16, &'a Run)) {
        let rows = self.wrap(area.width);
        let height = usize::from(self.body_height(area));
        let scroll = self.scroll.min(rows.len().saturating_sub(height));
        let right = area.right().saturating_sub(PADDING);
        for (offset, row) in rows.into_iter().skip(scroll).take(height).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let y = area.top() + offset;
            let mut x = area.left() + PADDING;
            for (piece, run) in row {
                for grapheme in piece.graphemes(true) {
                    let width = u16::try_from(grapheme.width()).unwrap_or(1);
                    if width == 0 || x + width > right {
                        continue;
                    }
                    put(x, y, grapheme, width, run);
                    x += width;
                }
            }
        }
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
        let buttons: usize = self
            .buttons
            .iter()
            .map(|label| drawn_width(label) + 3)
            .sum::<usize>()
            .saturating_sub(1);
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

impl Popover<'_> {
    /// `grapheme` wrapped in OSC 8 for link `link`, when that is wanted and
    /// the link can be said safely.
    fn hyperlink(&self, link: usize, grapheme: &str) -> Option<String> {
        if !self.hyperlinks {
            return None;
        }
        let uri = osc8_uri(self.links.get(link)?)?;
        Some(format!("\x1b]8;id=nun-{link};{uri}\x1b\\{grapheme}\x1b]8;;\x1b\\"))
    }
}

/// The longest URI said in OSC 8. Terminals cap it, some at about two
/// thousand bytes; a longer link is still drawn and still clicks.
const MOST_OSC8_URI: usize = 2000;

/// `uri` made safe to put inside an OSC 8 sequence, or `None` if it cannot
/// be.
///
/// The URI comes from a language server, so it is untrusted: an ESC, a BEL
/// or a C1 byte in it would end the sequence early and whatever followed
/// would reach the terminal as commands. Every byte outside printable ASCII
/// is percent-encoded, which is what a URI does with them anyway.
///
/// A URI with a `;` in it is not said at all. Alacritty splits the sequence
/// at every `;` and keeps a fixed number of pieces, so a terminal-side click
/// could open a URI cut short; encoding it would make it a different URI.
/// nun's own link still works.
fn osc8_uri(uri: &str) -> Option<String> {
    use std::fmt::Write as _;

    if uri.contains(';') {
        return None;
    }

    let mut safe = String::with_capacity(uri.len());
    for byte in uri.bytes() {
        if (0x21..=0x7e).contains(&byte) {
            safe.push(char::from(byte));
        } else {
            let _ = write!(safe, "%{byte:02X}");
        }
    }
    (!safe.is_empty() && safe.len() <= MOST_OSC8_URI).then_some(safe)
}

/// Split `text` after as many graphemes as fit in `room` columns — at least
/// one, so a single character wider than the card still makes progress.
/// How many columns `text` takes as it is drawn: a grapheme at a time, which
/// is not always what the whole string measures — a label a server wrote can
/// hold a ligature that the string's width counts once and its graphemes
/// twice.
fn drawn_width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

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

        let rows = self.body_rows(area.width);
        let height = usize::from(self.body_height(area));
        let scroll = self.scroll.min(rows.saturating_sub(height));
        self.lay_out(area, |x, y, grapheme, width, run| {
            let hovered = run.link.is_some() && run.link == self.hovered_link;
            let mut style = if hovered {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Overlay, run.role)
            };
            if run.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if run.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if run.link.is_some() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let cell = &mut cells[(x, y)];
            cell.set_style(style);
            match run.link.and_then(|link| self.hyperlink(link, grapheme)) {
                Some(marked) => {
                    // The escape travels inside the cell and is not text:
                    // the width is the grapheme's, said outright, and the
                    // cell opens and closes its own link so a partial
                    // redraw can never leave one open across a jump.
                    cell.set_symbol(&marked);
                    if let Some(width) = NonZeroU16::new(width) {
                        cell.set_diff_option(CellDiffOption::ForcedWidth(width));
                    }
                }
                None => {
                    cell.set_symbol(grapheme);
                }
            }
        });

        // More above or below is said in the padding column, where it takes
        // nothing from the text.
        let edge = area.right() - 1;
        let hint = self.palette.on(Role::Overlay, Role::Dim);
        if scroll > 0 {
            cells[(edge, area.top())].set_char('▴').set_style(hint);
        }
        if scroll + height < rows && height > 0 {
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
                let width = u16::try_from(grapheme.width()).unwrap_or(1);
                if x.saturating_add(width) > at.right() {
                    break;
                }
                cells[(x, at.y)].set_symbol(grapheme).set_style(style);
                x += width;
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
    fn a_button_is_measured_as_it_is_drawn_and_never_drawn_past() {
        let palette = palette();
        let body = body("unused variable");
        // Two graphemes to its width in one piece, four drawn one by one.
        let buttons = vec!["لالا".to_string(), "Next".to_string()];
        let popover = Popover::new(&body, &palette).buttons(&buttons);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        let areas = popover.button_areas(card);
        assert_eq!(areas[0].width, 6, "four columns and a space each side");
        let mut cells = Cells::empty(SCREEN);
        popover.render(card, &mut cells);
        assert_eq!(cells[(areas[1].x + 1, areas[1].y)].symbol(), "N", "not drawn over");
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

    fn linked() -> (Vec<Paragraph>, Vec<String>) {
        let mut link = Run::new("the docs", Role::Accent);
        link.link = Some(0);
        (
            vec![vec![Run::new("See ", Role::Text), link, Run::new(".", Role::Text)]],
            vec!["https://example.com/a\x1b]8\x07b c".to_string()],
        )
    }

    #[test]
    fn a_link_is_drawn_underlined_where_the_layout_says_it_is() {
        let palette = palette();
        let (body, links) = linked();
        let popover = Popover::new(&body, &palette).links(&links);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        let areas = popover.link_areas(card);
        assert_eq!(areas, [(0, Rect::new(card.x + 1 + 4, card.y, 8, 1))]);

        let mut cells = Cells::empty(SCREEN);
        popover.render(card, &mut cells);
        let (_, at) = areas[0];
        let drawn: String = (at.x..at.right()).map(|x| cells[(x, at.y)].symbol()).collect();
        assert_eq!(drawn, "the docs", "no escapes unless asked for");
        assert!(cells[(at.x, at.y)].modifier.contains(Modifier::UNDERLINED));
        assert!(!cells[(at.x - 1, at.y)].modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn a_link_wrapped_over_two_rows_has_an_area_on_each() {
        let palette = palette();
        let mut link = Run::new("one two three four five six seven", Role::Accent);
        link.link = Some(0);
        let body = vec![vec![link]];
        let popover = Popover::new(&body, &palette);
        let card = Rect::new(0, 0, 16, 5);
        let areas = popover.link_areas(card);
        assert!(areas.len() >= 2, "{areas:?}");
        assert!(areas.iter().all(|(link, _)| *link == 0));
    }

    #[test]
    fn osc8_marks_each_cell_on_its_own_with_a_clean_uri() {
        let palette = palette();
        let (body, links) = linked();
        let popover = Popover::new(&body, &palette).links(&links).hyperlinks(true);
        let card = popover.place(Rect::new(0, 0, 1, 1), SCREEN).unwrap();
        let (_, at) = popover.link_areas(card)[0];
        let mut cells = Cells::empty(SCREEN);
        popover.render(card, &mut cells);
        let cell = &cells[(at.x, at.y)];
        assert_eq!(
            cell.symbol(),
            "\x1b]8;id=nun-0;https://example.com/a%1B]8%07b%20c\x1b\\t\x1b]8;;\x1b\\",
            "opened and closed in the one cell, nothing in the URI able to end it"
        );
        assert_eq!(cell.diff_option, CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap()));
        assert_eq!(cells[(at.x - 1, at.y)].symbol(), " ", "only the link's cells");
    }

    #[test]
    fn a_uri_is_made_printable_or_refused() {
        assert_eq!(osc8_uri("https://a.b/c?d=e"), Some("https://a.b/c?d=e".into()));
        assert_eq!(osc8_uri("x\u{9c}y\u{7}"), Some("x%C2%9Cy%07".into()));
        assert_eq!(osc8_uri(""), None);
        assert_eq!(osc8_uri("https://a.b/c;d"), None);
        assert_eq!(osc8_uri(&"a".repeat(MOST_OSC8_URI + 1)), None);
    }

    proptest! {
        /// Whatever a server puts in a link, the URI said in OSC 8 is only
        /// printable ASCII, so it cannot end the sequence it is in.
        #[test]
        fn a_uri_never_carries_a_control_byte(uri in any::<String>()) {
            if let Some(safe) = osc8_uri(&uri) {
                prop_assert!(safe.bytes().all(|byte| (0x21..=0x7e).contains(&byte)));
            }
        }

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
