//! Panes: which files are in which part of the screen.
//!
//! The screen is divided by a tree ([`nun_ui::Layout`]); this is what hangs
//! off its leaves. Each pane has its own tabs, its own active one, and its own
//! scrolled tab strip, so splitting the screen gives you two independent
//! places to work rather than two views of one.

use nun_ui::{Dir, Layout};
use ratatui::layout::Rect;

/// Which document, whichever tab or pane it moves to.
pub(super) type DocId = u32;

/// One pane's contents.
#[derive(Debug, Clone)]
pub(super) struct Pane {
    /// Its id in the layout tree.
    pub(super) id: usize,
    /// The documents open in it, in tab order.
    pub(super) tabs: Vec<DocId>,
    /// Which of them is being edited.
    pub(super) active: usize,
    /// How far its tab strip is scrolled.
    pub(super) scroll: u16,
}

impl Pane {
    /// The document showing in it.
    pub(super) fn current(&self) -> Option<DocId> {
        self.tabs.get(self.active).copied()
    }
}

/// Every pane, and how they divide the screen.
#[derive(Debug, Clone)]
pub(super) struct Panes {
    layout: Layout,
    list: Vec<Pane>,
    focus: usize,
    next_id: usize,
}

impl Panes {
    /// One pane holding `tabs`.
    pub(super) fn single(tabs: Vec<DocId>) -> Self {
        Self {
            layout: Layout::single(0),
            list: vec![Pane { id: 0, tabs, active: 0, scroll: 0 }],
            focus: 0,
            next_id: 1,
        }
    }

    /// How the screen is divided.
    pub(super) const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// How the screen is divided, to change: dragging a divider.
    pub(super) const fn layout_mut(&mut self) -> &mut Layout {
        &mut self.layout
    }

    /// The pane with the keyboard.
    pub(super) fn focus(&self) -> usize {
        self.focus
    }

    /// Give the keyboard to `pane`, if it exists.
    pub(super) fn set_focus(&mut self, pane: usize) -> bool {
        let found = self.list.iter().any(|it| it.id == pane);
        if found {
            self.focus = pane;
        }
        found
    }

    /// The pane with the keyboard.
    pub(super) fn focused(&self) -> &Pane {
        self.get(self.focus).expect("the focused pane is always one of them")
    }

    /// The pane with the keyboard, to change.
    pub(super) fn focused_mut(&mut self) -> &mut Pane {
        let focus = self.focus;
        self.get_mut(focus).expect("the focused pane is always one of them")
    }

    pub(super) fn get(&self, pane: usize) -> Option<&Pane> {
        self.list.iter().find(|it| it.id == pane)
    }

    pub(super) fn get_mut(&mut self, pane: usize) -> Option<&mut Pane> {
        self.list.iter_mut().find(|it| it.id == pane)
    }

    /// Every pane, in layout order.
    pub(super) fn all(&self) -> &[Pane] {
        &self.list
    }

    /// How many there are.
    pub(super) fn len(&self) -> usize {
        self.list.len()
    }

    /// Where each pane goes inside `area`.
    pub(super) fn rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        self.layout.rects(area)
    }

    /// The rectangle of one pane.
    pub(super) fn rect_of(&self, pane: usize, area: Rect) -> Option<Rect> {
        self.layout.rect_of(pane, area)
    }

    /// Split `pane`, putting a new one holding `tabs` beside or below it.
    ///
    /// The new pane takes the keyboard, because splitting is something you do
    /// in order to work in the new place.
    pub(super) fn split(&mut self, pane: usize, dir: Dir, first: bool, tabs: Vec<DocId>) -> usize {
        let new = self.next_id;
        if !self.layout.split(pane, dir, new, first) {
            // A pane that is not in the tree cannot be split; adding one to
            // the list anyway would leave a pane nothing can draw or reach.
            return pane;
        }
        self.next_id += 1;
        self.list.push(Pane { id: new, tabs, active: 0, scroll: 0 });
        self.sort_by_layout();
        self.focus = new;
        new
    }

    /// Close `pane`, moving whatever is open in it into the pane that takes
    /// its space, so closing a pane never closes a file.
    ///
    /// Returns whether it closed.
    pub(super) fn fold_into_neighbour(&mut self, pane: usize) -> bool {
        if self.list.len() < 2 {
            return false;
        }
        let Some(tabs) = self.get(pane).map(|state| state.tabs.clone()) else { return false };
        let Some(other) = self.list.iter().map(|state| state.id).find(|id| *id != pane) else {
            return false;
        };
        if !self.close(pane) {
            return false;
        }
        if let Some(target) = self.get_mut(other) {
            target.active = target.tabs.len();
            target.tabs.extend(tabs);
            target.active = target.active.min(target.tabs.len().saturating_sub(1));
        }
        self.focus = other;
        true
    }

    /// Close `pane`; its sibling takes the space. The last pane stays.
    pub(super) fn close(&mut self, pane: usize) -> bool {
        if self.list.len() < 2 || !self.layout.close(pane) {
            return false;
        }
        self.list.retain(|it| it.id != pane);
        self.sort_by_layout();
        if self.focus == pane {
            self.focus = self.list.first().map_or(0, |it| it.id);
        }
        true
    }

    /// Move the tab at `index` of `from` into `to`, at `at`.
    ///
    /// Returns whether it moved. A pane left with no tabs closes, unless it is
    /// the only one.
    pub(super) fn move_tab(&mut self, from: usize, index: usize, to: usize, at: usize) -> bool {
        if from == to {
            let Some(pane) = self.get_mut(from) else { return false };
            if index >= pane.tabs.len() {
                return false;
            }
            let at = if at > index { at - 1 } else { at };
            let doc = pane.tabs.remove(index);
            let at = at.min(pane.tabs.len());
            pane.tabs.insert(at, doc);
            pane.active = at;
            return true;
        }

        // The target is checked before anything is removed: a document taken
        // out of one pane and not put into another would belong nowhere, and
        // the next tidy-up would delete it.
        if self.get(to).is_none() {
            return false;
        }
        let Some(source) = self.get_mut(from) else { return false };
        if index >= source.tabs.len() {
            return false;
        }
        let doc = source.tabs.remove(index);
        source.active = source.active.min(source.tabs.len().saturating_sub(1));
        let emptied = source.tabs.is_empty();

        let Some(target) = self.get_mut(to) else { return false };
        let at = at.min(target.tabs.len());
        target.tabs.insert(at, doc);
        target.active = at;
        self.focus = to;

        if emptied {
            self.close(from);
        }
        true
    }

    /// Put `doc` in the focused pane, next to what is showing.
    pub(super) fn open(&mut self, doc: DocId) {
        let pane = self.focused_mut();
        let at = (pane.active + 1).min(pane.tabs.len());
        pane.tabs.insert(at, doc);
        pane.active = at;
    }

    /// Close the tab at `index` of the focused pane, and the pane with it if
    /// that was its last tab and there are others.
    ///
    /// Returns the document that was closed, and whether the pane went too.
    pub(super) fn close_tab(&mut self, pane: usize, index: usize) -> Option<(DocId, bool)> {
        let only_pane = self.list.len() < 2;
        let target = self.get_mut(pane)?;
        if index >= target.tabs.len() {
            return None;
        }
        let doc = target.tabs.remove(index);
        if target.active > index {
            target.active -= 1;
        }
        target.active = target.active.min(target.tabs.len().saturating_sub(1));

        let emptied = target.tabs.is_empty();
        if emptied && !only_pane {
            self.close(pane);
            return Some((doc, true));
        }
        Some((doc, false))
    }

    /// Which documents are open anywhere.
    pub(super) fn open_docs(&self) -> Vec<DocId> {
        self.list.iter().flat_map(|pane| pane.tabs.iter().copied()).collect()
    }

    /// The pane holding `doc`, and which tab it is.
    pub(super) fn find(&self, doc: DocId) -> Option<(usize, usize)> {
        self.list.iter().find_map(|pane| {
            pane.tabs.iter().position(|it| *it == doc).map(|index| (pane.id, index))
        })
    }

    /// Keep the list in the order the layout draws them, so "the next pane"
    /// means the next one across the screen.
    fn sort_by_layout(&mut self) {
        let order = self.layout.panes();
        self.list
            .sort_by_key(|pane| order.iter().position(|id| *id == pane.id).unwrap_or(usize::MAX));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };

    fn two_panes() -> Panes {
        let mut panes = Panes::single(vec![1, 2]);
        panes.split(0, Dir::Beside, false, vec![3]);
        panes
    }

    #[test]
    fn splitting_makes_a_new_pane_and_gives_it_the_keyboard() {
        let panes = two_panes();
        assert_eq!(panes.len(), 2);
        assert_eq!(panes.focused().tabs, vec![3]);
        assert_eq!(panes.rects(SCREEN).len(), 2);
    }

    #[test]
    fn closing_a_pane_gives_the_keyboard_to_what_is_left() {
        let mut panes = two_panes();
        let new = panes.focus();
        assert!(panes.close(new));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes.focused().tabs, vec![1, 2]);
    }

    #[test]
    fn the_last_pane_never_closes() {
        let mut panes = Panes::single(vec![1]);
        assert!(!panes.close(0));
        assert_eq!(panes.len(), 1);
    }

    #[test]
    fn a_tab_moves_between_panes_and_takes_the_keyboard_with_it() {
        let mut panes = two_panes();
        let other = panes.focus();
        assert!(panes.move_tab(0, 0, other, 0));

        assert_eq!(panes.get(0).unwrap().tabs, vec![2]);
        assert_eq!(panes.get(other).unwrap().tabs, vec![1, 3]);
        assert_eq!(panes.focus(), other);
        assert_eq!(panes.focused().active, 0, "the moved tab is the one showing");
    }

    #[test]
    fn a_pane_emptied_by_a_move_closes() {
        let mut panes = Panes::single(vec![1]);
        let other = panes.split(0, Dir::Beside, false, vec![2]);
        assert!(panes.move_tab(0, 0, other, 0));

        assert_eq!(panes.len(), 1, "the pane it left had nothing else in it");
        assert_eq!(panes.focused().tabs, vec![1, 2]);
    }

    #[test]
    fn a_tab_reorders_within_its_own_pane() {
        let mut panes = Panes::single(vec![1, 2, 3]);
        assert!(panes.move_tab(0, 0, 0, 3));
        assert_eq!(panes.focused().tabs, vec![2, 3, 1]);
        assert_eq!(panes.focused().active, 2, "it stays the one showing");
    }

    #[test]
    fn closing_the_last_tab_of_a_pane_closes_the_pane() {
        let mut panes = two_panes();
        let other = panes.focus();
        assert_eq!(panes.close_tab(other, 0), Some((3, true)));
        assert_eq!(panes.len(), 1);
        assert_eq!(panes.focus(), 0);
    }

    #[test]
    fn closing_the_last_tab_of_the_only_pane_leaves_the_pane() {
        let mut panes = Panes::single(vec![1]);
        assert_eq!(panes.close_tab(0, 0), Some((1, false)));
        assert_eq!(panes.len(), 1);
        assert!(panes.focused().tabs.is_empty());
    }

    #[test]
    fn closing_a_tab_before_the_active_one_keeps_the_same_file_showing() {
        let mut panes = Panes::single(vec![1, 2, 3]);
        panes.focused_mut().active = 2;
        panes.close_tab(0, 0);
        assert_eq!(panes.focused().current(), Some(3));
    }

    #[test]
    fn opening_puts_the_file_beside_the_one_showing() {
        let mut panes = Panes::single(vec![1, 2]);
        panes.focused_mut().active = 0;
        panes.open(9);
        assert_eq!(panes.focused().tabs, vec![1, 9, 2]);
        assert_eq!(panes.focused().current(), Some(9));
    }

    #[test]
    fn a_document_can_be_found_wherever_it_is() {
        let panes = two_panes();
        assert_eq!(panes.find(2), Some((0, 1)));
        assert_eq!(panes.find(3).map(|(_, index)| index), Some(0));
        assert_eq!(panes.find(99), None);
        assert_eq!(panes.open_docs(), vec![1, 2, 3]);
    }

    #[test]
    fn panes_are_listed_in_the_order_they_are_drawn() {
        let mut panes = Panes::single(vec![1]);
        panes.split(0, Dir::Beside, true, vec![2]);
        let ids: Vec<usize> = panes.all().iter().map(|pane| pane.id).collect();
        let drawn: Vec<usize> = panes.rects(SCREEN).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, drawn, "the new pane went on the left, so it is listed first");
    }
}
