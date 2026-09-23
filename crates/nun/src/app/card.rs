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
    /// The labels alone, as the widget takes them.
    labels: Vec<String>,
    /// How far the body is scrolled.
    scroll: usize,
    /// The hover target that opened it, which it stays open over. `None`
    /// for a card the keyboard opened.
    opener: Option<Target>,
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
        Self { anchor, body, buttons, labels, scroll: 0, opener }
    }

    fn popover<'a>(&'a self, palette: &'a nun_ui::Palette) -> Popover<'a> {
        Popover::new(&self.body, palette).buttons(&self.labels).scrolled_to(self.scroll)
    }
}

impl App {
    /// Show `card`, in place of any other.
    pub(super) fn show_card(&mut self, card: Card) {
        self.card = Some(card);
    }

    /// Put the card away. Whether there was one.
    pub(super) fn close_card(&mut self) -> bool {
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
        let Some(area) = self.card_area() else { return };
        // Hover targets both, so moving from the anchor into the card, and
        // about inside it, keeps it open.
        hits.push(super::cells(area), Target::Card, true);
        for (index, button) in self.card_buttons().into_iter().enumerate() {
            if button.width > 0 {
                hits.push(super::cells(button), Target::CardButton(index), true);
            }
        }
    }

    /// Draw the card.
    pub(super) fn render_card(&self, cells: &mut Cells) {
        use ratatui::widgets::Widget as _;

        let (Some(card), Some(area)) = (self.card.as_ref(), self.card_area()) else { return };
        let hovered = match self.hover.current() {
            Some(Target::CardButton(index)) => Some(index),
            _ => None,
        };
        card.popover(&self.palette).hovered(hovered).render(area, cells);
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
            Target::CardButton(index) => {
                let command = card.buttons.get(index).map(|(_, command)| *command);
                self.close_card();
                Some(command.map_or(Outcome::Redraw, |command| self.run(command)))
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

    /// The pointer moved. A card it opened goes once it is over neither the
    /// card nor what opened it.
    pub(super) fn card_follow_pointer(&mut self) -> Outcome {
        let Some(opener) = self.card.as_ref().and_then(|card| card.opener) else {
            return Outcome::Continue;
        };
        match self.hover.current() {
            Some(Target::Card | Target::CardButton(_)) => Outcome::Continue,
            Some(target) if target == opener => Outcome::Continue,
            _ => {
                self.close_card();
                Outcome::Redraw
            }
        }
    }
}
