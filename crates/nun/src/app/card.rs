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
//!
//! Alt+Page Up and Alt+Page Down scroll a card, a page at a time, and keep
//! it open; the wheel does the same over it. They are the only keys a card
//! keeps for itself, they are bound to nothing else, and a card that
//! overflows says so in its bottom row. Every button on a card already has
//! a key of its own — F8 and Shift+F8 step, Code actions lists the fixes —
//! so the buttons are not given more.
//! A hover card is a pointer card whose opener is the text it is about
//! rather than a hover target, so it stays while the pointer is on that text.
//!
//! A card's links are drawn as links and followed with a click, whatever the
//! terminal; OSC 8 is added on top where it is wanted.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_ui::{Paragraph, Popover};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;

use super::panes::DocId;
use super::{App, Outcome, Target};
use crate::commands::Command;

/// What a card that does not fit says about its scroll keys.
const SCROLL_HINT: &str = "Alt+PgUp/PgDn scrolls";

/// Rows the wheel scrolls a card by.
const WHEEL_ROWS: usize = 3;

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
    /// The hunk it is about, which its buttons act on.
    hunk: Option<super::gutter::Aim>,
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
            hunk: None,
        }
    }

    /// A card about a hunk: its buttons act on that hunk, wherever the
    /// caret is.
    pub(super) fn about_hunk(mut self, hunk: super::gutter::Aim) -> Self {
        self.hunk = Some(hunk);
        self
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
            .overflow_hint(SCROLL_HINT)
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
        self.card_scroll_by(down, |_| WHEEL_ROWS).unwrap_or(Outcome::Continue)
    }

    /// Scroll the card up or down by `rows`, which is given the rows of body
    /// in view. `None` when there is no card on screen to scroll.
    fn card_scroll_by(&mut self, down: bool, rows: impl FnOnce(usize) -> usize) -> Option<Outcome> {
        let (card, area) = (self.card.as_ref()?, self.card_area()?);
        let popover = card.popover(&self.palette);
        let (most, rows) =
            (popover.most_scroll(area), rows(usize::from(popover.body_height(area))));
        let card = self.card.as_mut()?;
        let scroll =
            if down { (card.scroll + rows).min(most) } else { card.scroll.saturating_sub(rows) };
        if scroll == card.scroll {
            return Some(Outcome::Continue);
        }
        card.scroll = scroll;
        Some(Outcome::Redraw)
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
                let hunk = card.hunk.clone();
                self.close_card();
                self.changes.aim(hunk);
                let outcome = command.map_or(Outcome::Redraw, |command| self.run(command));
                self.changes.aim(None);
                Some(outcome)
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

    /// A key, with a card open. Alt+Page Up and Down scroll it a page,
    /// keeping a row from the last in view, and leave it open. Escape puts it
    /// away and is spent doing so; any other key puts it away and then does
    /// what it does.
    pub(super) fn card_key(&mut self, event: &KeyEvent) -> Option<Outcome> {
        if self.card.is_none() || matches!(event.code, KeyCode::Modifier(_)) {
            return None;
        }
        let page = match (event.code, event.modifiers) {
            (KeyCode::PageUp, KeyModifiers::ALT) => Some(false),
            (KeyCode::PageDown, KeyModifiers::ALT) => Some(true),
            _ => None,
        };
        if let Some(down) = page
            && let Some(outcome) = self.card_scroll_by(down, |rows| rows.saturating_sub(1).max(1))
        {
            return Some(outcome);
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nun_core::Buffer;
    use nun_theme::{Probe, Role, derive};
    use nun_ui::Event;
    use nun_ui::{Palette, Run};
    use ratatui::buffer::Buffer as Cells;
    use ratatui::layout::Rect;

    use super::*;

    /// An editor with a keyboard card up, saying `words` words about the
    /// top left corner.
    fn with_card(words: usize) -> App {
        let mut app = App::new(
            Buffer::from_text("one\ntwo\n"),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 24));
        let body = vec![vec![Run::new("word ".repeat(words), Role::Text)]];
        app.show_card(Card::new(Anchor::Screen(Rect::new(0, 0, 1, 1)), body, Vec::new(), None));
        app
    }

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Outcome {
        app.handle(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn scroll(app: &App) -> usize {
        app.card.as_ref().expect("the card is still up").scroll
    }

    fn most_scroll(app: &App) -> usize {
        let card = app.card.as_ref().unwrap();
        card.popover(&app.palette).most_scroll(app.card_area().unwrap())
    }

    fn bottom_row(app: &App) -> String {
        let area = app.card_area().unwrap();
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        (area.x..area.right()).map(|x| cells[(x, area.bottom() - 1)].symbol()).collect()
    }

    #[test]
    fn alt_page_down_scrolls_a_keyboard_card_to_its_end_and_keeps_it_open() {
        let mut app = with_card(600);
        let most = most_scroll(&app);
        assert!(most > 13, "more than a page to scroll: {most}");
        assert!(bottom_row(&app).trim_end().ends_with(SCROLL_HINT), "says how to scroll");

        assert_eq!(key(&mut app, KeyCode::PageDown, KeyModifiers::ALT), Outcome::Redraw);
        assert_eq!(scroll(&app), 12, "a page, less the row kept in view");
        while scroll(&app) < most {
            assert_eq!(key(&mut app, KeyCode::PageDown, KeyModifiers::ALT), Outcome::Redraw);
        }
        assert_eq!(key(&mut app, KeyCode::PageDown, KeyModifiers::ALT), Outcome::Continue);
        assert_eq!(scroll(&app), most, "stops at the end");

        key(&mut app, KeyCode::PageUp, KeyModifiers::ALT);
        assert_eq!(scroll(&app), most - 12);
    }

    #[test]
    fn any_other_key_still_closes_the_card_and_does_what_it_does() {
        let mut app = with_card(600);
        key(&mut app, KeyCode::PageDown, KeyModifiers::ALT);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(app.card.is_none());
        assert_eq!(app.doc().buffer.line_text(0), "xone\n");

        // Alt+Down is Add a caret below, and still is with a card up.
        let mut app = with_card(600);
        key(&mut app, KeyCode::Down, KeyModifiers::ALT);
        assert!(app.card.is_none());
        assert_eq!(app.doc().buffer.selections().len(), 2);

        // Page Down with another modifier is not a card's.
        let mut app = with_card(600);
        key(&mut app, KeyCode::PageDown, KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert!(app.card.is_none());
    }

    #[test]
    fn a_card_that_fits_says_nothing_about_scrolling() {
        let mut app = with_card(3);
        assert!(!bottom_row(&app).contains("Alt"));
        assert_eq!(key(&mut app, KeyCode::PageDown, KeyModifiers::ALT), Outcome::Continue);
        assert_eq!(scroll(&app), 0);
    }
}
