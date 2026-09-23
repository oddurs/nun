//! A card beside something on screen: what a diagnostic says, and in time
//! what a symbol is.
//!
//! There is one card at a time. It is about an [`Anchor`] — some characters
//! of a document, or a fixed place such as a rail row — and it is placed
//! afresh at every layout, so a card about text moves with the text as the
//! view scrolls and is not drawn while that text is out of view.
//!
//! How it goes away depends on how it came. One the pointer opened by
//! resting on something stays while the pointer is over that thing or over
//! the card, and goes when it leaves both. One the keyboard opened stays
//! until the next key or a click somewhere else. Either goes with Escape.
//! A hover card is a pointer card whose opener is the text it is about
//! rather than a hover target, so it stays while the pointer is on that text.
//!
//! A card's links are drawn as links and followed with a click, whatever the
//! terminal; OSC 8 is added on top where it is wanted.

use crossterm::event::{KeyCode, KeyEvent};
use nun_ui::{Paragraph, Popover};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;

use super::panes::DocId;
use super::{App, Outcome, Target};
use crate::commands::Command;

/// What a card is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Anchor {
    /// Characters `from..to` of `doc`, as drawn in `pane`.
    Text { pane: usize, doc: DocId, from: usize, to: usize },
    /// A place on screen.
    Screen(Rect),
}

/// A card on screen.
#[derive(Debug, Clone)]
pub(super) struct Card {
    anchor: Anchor,
    body: Vec<Paragraph>,
    /// What each button says, and the command it runs.
    buttons: Vec<(String, Command)>,
    /// How many buttons, at the front, are fixes a server offered rather
    /// than commands; `code_actions` knows what each does.
    fixes: usize,
    /// The labels alone, as the widget takes them: the fixes, then the
    /// commands.
    labels: Vec<String>,
    /// How far the body is scrolled.
    scroll: usize,
    /// The hover target that opened it, which it stays open over. `None`
    /// for a card the keyboard opened.
    opener: Option<Target>,
    /// Whether the pointer opened it by resting on its anchor, which it
    /// stays open over.
    held: bool,
    /// Where the body's links go.
    links: Vec<String>,
    /// Whether to mark the links with OSC 8 as well.
    hyperlinks: bool,
}

impl Card {
    /// A card about `anchor`, saying `body`, with `buttons` along its
    /// bottom. `opener` is the hover target the pointer opened it from, or
    /// `None` when a key did.
    pub(super) fn new(
        anchor: Anchor,
        body: Vec<Paragraph>,
        buttons: Vec<(String, Command)>,
        opener: Option<Target>,
    ) -> Self {
        let labels = buttons.iter().map(|(label, _)| label.clone()).collect();
        Self {
            anchor,
            body,
            buttons,
            fixes: 0,
            labels,
            scroll: 0,
            opener,
            held: false,
            links: Vec::new(),
            hyperlinks: false,
        }
    }

    /// A card the pointer opened by resting on the text of its anchor: it
    /// stays while the pointer is over that text or over the card.
    pub(super) const fn held_over_anchor(mut self) -> Self {
        self.held = true;
        self
    }

    /// Where the body's links go, and whether to tell the terminal too.
    pub(super) fn with_links(mut self, links: Vec<String>, hyperlinks: bool) -> Self {
        self.links = links;
        self.hyperlinks = hyperlinks;
        self
    }

    /// Whether it is a card the pointer opened on its anchor.
    pub(super) const fn is_held(&self) -> bool {
        self.held
    }

    /// Where link `index` goes.
    pub(super) fn link(&self, index: usize) -> Option<&str> {
        self.links.get(index).map(String::as_str)
    }

    /// What it is about.
    pub(super) const fn anchor(&self) -> Anchor {
        self.anchor
    }

    /// How many of its buttons are fixes. Only the tests ask.
    #[cfg(test)]
    pub(super) const fn fixes(&self) -> usize {
        self.fixes
    }

    /// Put buttons for fixes in front of the card's own, in place of any
    /// offered before.
    pub(super) fn offer_fixes(&mut self, labels: Vec<String>) {
        self.fixes = labels.len();
        self.labels = labels;
        self.labels.extend(self.buttons.iter().map(|(label, _)| label.clone()));
    }

    fn popover<'a>(&'a self, palette: &'a nun_ui::Palette) -> Popover<'a> {
        Popover::new(&self.body, palette)
            .buttons(&self.labels)
            .scrolled_to(self.scroll)
            .links(&self.links)
            .hyperlinks(self.hyperlinks)
    }
}

impl App {
    /// Show `card`, in place of any other.
    pub(super) fn show_card(&mut self, card: Card) {
        self.forget_card_fixes();
        self.card = Some(card);
    }

    /// Put the card away. Whether there was one.
    pub(super) fn close_card(&mut self) -> bool {
        self.forget_card_fixes();
        self.card.take().is_some()
    }

    /// Where the card's anchor is on screen now, or `None` when it is out of
    /// view. A range over several rows is the rectangle around all of them,
    /// so the card goes wholly above or below it.
    fn card_anchor(&self, anchor: Anchor) -> Option<Rect> {
        match anchor {
            Anchor::Screen(rect) => Some(rect),
            Anchor::Text { pane, doc, from, to } => {
                let id = self.panes.get(pane)?.current()?;
                if id != doc {
                    return None;
                }
                let document = self.doc_by(doc)?;
                let (text, _) = self.split_rail(document, self.text_area_of(pane)?);
                let rects = nun_ui::EditorView::new(&document.buffer, &self.palette)
                    .scrolled_to(document.scroll)
                    .screen_rects(text, from, to);
                rects.into_iter().reduce(Rect::union)
            }
        }
    }

    /// Where the card is, if there is one and there is room for it.
    pub(super) fn card_area(&self) -> Option<Rect> {
        let card = self.card.as_ref()?;
        let anchor = self.card_anchor(card.anchor)?;
        // Anywhere above the status line, over panes and sidebar alike.
        let bounds = Rect { height: self.viewport.height.saturating_sub(1), ..self.viewport };
        card.popover(&self.palette).place(anchor, bounds)
    }

    /// Where the card's buttons are.
    pub(super) fn card_buttons(&self) -> Vec<Rect> {
        let (Some(card), Some(area)) = (self.card.as_ref(), self.card_area()) else {
            return Vec::new();
        };
        card.popover(&self.palette).button_areas(area)
    }

    /// Put the card into the hit map, over everything but a menu.
    pub(super) fn layout_card(&self, hits: &mut nun_input::HitMap<Target>) {
        let (Some(card), Some(area)) = (self.card.as_ref(), self.card_area()) else { return };
        // Hover targets all, so moving from the anchor into the card, and
        // about inside it, keeps it open.
        hits.push(super::cells(area), Target::Card, true);
        for (index, button) in self.card_buttons().into_iter().enumerate() {
            if button.width > 0 {
                hits.push(super::cells(button), Target::CardButton(index), true);
            }
        }
        for (index, link) in card.popover(&self.palette).link_areas(area) {
            hits.push(super::cells(link), Target::CardLink(index), true);
        }
    }

    /// Draw the card.
    pub(super) fn render_card(&self, cells: &mut Cells) {
        use ratatui::widgets::Widget as _;

        let (Some(card), Some(area)) = (self.card.as_ref(), self.card_area()) else { return };
        let (hovered, link) = match self.hover.current() {
            Some(Target::CardButton(index)) => (Some(index), None),
            Some(Target::CardLink(index)) => (None, Some(index)),
            _ => (None, None),
        };
        card.popover(&self.palette).hovered(hovered).hovered_link(link).render(area, cells);
    }

    /// The wheel turned over the card.
    pub(super) fn card_scroll(&mut self, down: bool) -> Outcome {
        let most = match (self.card.as_ref(), self.card_area()) {
            (Some(card), Some(area)) => card.popover(&self.palette).most_scroll(area),
            _ => return Outcome::Continue,
        };
        let Some(card) = self.card.as_mut() else { return Outcome::Continue };
        let scroll = if down { (card.scroll + 3).min(most) } else { card.scroll.saturating_sub(3) };
        if scroll == card.scroll {
            return Outcome::Continue;
        }
        card.scroll = scroll;
        Outcome::Redraw
    }

    /// A press on `target`, with a card open. `Some` when the card took it;
    /// otherwise the card has been put away and the press goes on to what
    /// is under it.
    pub(super) fn card_click(&mut self, target: Target) -> Option<Outcome> {
        let card = self.card.as_ref()?;
        match target {
            Target::Card => Some(Outcome::Continue),
            Target::CardButton(index) if index < card.fixes => {
                self.close_card();
                Some(self.card_fix(index))
            }
            Target::CardButton(index) => {
                let command = card.buttons.get(index - card.fixes).map(|(_, command)| *command);
                self.close_card();
                Some(command.map_or(Outcome::Redraw, |command| self.run(command)))
            }
            Target::CardLink(index) => {
                let link = card.link(index).map(str::to_string);
                self.close_card();
                Some(link.map_or(Outcome::Redraw, |link| self.follow_link(&link)))
            }
            _ => {
                self.close_card();
                None
            }
        }
    }

    /// A key, with a card open. Escape puts it away and is spent doing so;
    /// any other key puts it away and then does what it does.
    pub(super) fn card_key(&mut self, event: &KeyEvent) -> Option<Outcome> {
        if self.card.is_none() || matches!(event.code, KeyCode::Modifier(_)) {
            return None;
        }
        self.close_card();
        (event.code == KeyCode::Esc).then_some(Outcome::Redraw)
    }

    /// The pointer moved to `column`, `row`. A card it opened goes once it
    /// is over neither the card nor what opened it.
    pub(super) fn card_follow_pointer(&mut self, column: u16, row: u16) -> Outcome {
        let Some(card) = self.card.as_ref() else { return Outcome::Continue };
        let opener = match (card.opener, card.held) {
            (Some(opener), _) => Some(opener),
            (None, true) => None,
            (None, false) => return Outcome::Continue,
        };
        let on_anchor = || {
            self.card_anchor(card.anchor)
                .is_some_and(|anchor| anchor.contains(ratatui::layout::Position::new(column, row)))
        };
        match self.hover.current() {
            Some(target) if target.in_card() => Outcome::Continue,
            Some(target) if Some(target) == opener => Outcome::Continue,
            _ if card.held && on_anchor() => Outcome::Continue,
            _ => {
                self.close_card();
                Outcome::Redraw
            }
        }
    }
}
