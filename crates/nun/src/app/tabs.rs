//! Open files as tabs, in panes.
//!
//! Opening a file that is already open goes to its tab rather than opening it
//! twice. Opening one over an untouched, unnamed buffer takes that buffer's
//! place, so starting nun on a folder and clicking a file does not leave an
//! empty tab behind.
//!
//! A tab is closed by its cross, by the middle button, or by `Ctrl+W`, and
//! closing one with unsaved changes asks first — with buttons, so the answer
//! is a click. Dragging a tab moves it: along its own strip to reorder, onto
//! another pane to move it there, or onto a pane's edge to split.

use std::path::Path;

use nun_ui::{Dir, Edge, Tab, TabStrip};
use nun_workspace::tab_labels;
use ratatui::layout::Rect;

use super::prompt::{Prompt, Purpose};
use super::{App, Document, Focus, Outcome, Target};

/// A tab being dragged.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabDrag {
    /// The pane it came from.
    pub(super) pane: usize,
    /// Which tab of that pane.
    pub(super) from: usize,
    press: (u16, u16),
    moved: bool,
    /// Where it would land.
    pub(super) drop: Option<Drop>,
}

/// Where a dragged tab would end up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Drop {
    /// In a pane's strip, at this index.
    Strip { pane: usize, at: usize },
    /// Splitting a pane along one of its edges.
    Split { pane: usize, edge: Edge },
}

impl App {
    /// The whole area the panes divide between them: below the sidebar's
    /// columns and above the status line.
    pub(super) fn panes_area(&self) -> Rect {
        self.panes_area_in(self.viewport)
    }

    /// The same, within whatever rectangle is being drawn into — the viewport
    /// in the editor, a smaller grid in the tests.
    pub(super) fn panes_area_in(&self, area: Rect) -> Rect {
        let beside = self.sidebar_area().map_or(0, |sidebar| sidebar.width).min(area.width);
        Rect::new(
            area.x + beside,
            area.y,
            area.width.saturating_sub(beside),
            area.height.saturating_sub(1),
        )
    }

    /// A pane's strip and text, given the rectangle the pane has.
    fn parts_of(&self, pane: usize, rect: Rect) -> (Option<Rect>, Rect) {
        let state = self.panes.get(pane);
        let tabs = state.map_or(0, |pane| pane.tabs.len());
        let named = state
            .and_then(super::panes::Pane::current)
            .and_then(|id| self.doc_by(id))
            .is_some_and(|doc| doc.buffer.path().is_some());
        // A strip is what names a file and what closes or drags it, so any
        // file that has a name gets one. An untitled scratch buffer has
        // nothing to label, so the row goes to the text instead.
        let wanted = tabs > 1 || self.panes.len() > 1 || named;
        let strip = (wanted && rect.height > 1).then_some(Rect { height: 1, ..rect });
        let above = u16::from(strip.is_some());
        let text = Rect { y: rect.y + above, height: rect.height.saturating_sub(above), ..rect };
        (strip, text)
    }

    /// Where one pane goes.
    pub(super) fn pane_area(&self, pane: usize) -> Option<Rect> {
        self.panes.rect_of(pane, self.panes_area())
    }

    /// The tab strip of `pane`, when it has one.
    ///
    /// A single pane holding a single file needs no strip, and the row is
    /// better spent on text; with more than one of either, every pane is
    /// labelled.
    pub(super) fn strip_area(&self, pane: usize) -> Option<Rect> {
        self.parts_of(pane, self.pane_area(pane)?).0
    }

    /// The text of `pane`: its rectangle, less its strip.
    pub(super) fn text_area_of(&self, pane: usize) -> Option<Rect> {
        Some(self.parts_of(pane, self.pane_area(pane)?).1)
    }

    /// What the tabs of `pane` say.
    pub(super) fn tab_items(&self, pane: usize) -> Vec<Tab> {
        let Some(state) = self.panes.get(pane) else { return Vec::new() };
        let docs: Vec<&Document> = state.tabs.iter().filter_map(|id| self.doc_by(*id)).collect();
        let paths: Vec<Option<&Path>> = docs.iter().map(|doc| doc.buffer.path()).collect();
        tab_labels(&paths)
            .into_iter()
            .zip(docs)
            .map(|(label, doc)| Tab { label, modified: doc.buffer.is_modified() })
            .collect()
    }

    /// Lay out every pane's strip and text.
    pub(super) fn layout_panes(&self, hits: &mut nun_input::HitMap<Target>) {
        for (pane, _) in self.panes.rects(self.panes_area()) {
            if let Some(text) = self.text_area_of(pane) {
                // Each pane's own gutter, which is as wide as its own file's
                // line numbers: a pane on a short file beside one on a long
                // file has its arrows a column further left.
                let gutter = self
                    .panes
                    .get(pane)
                    .and_then(super::panes::Pane::current)
                    .and_then(|id| self.doc_by(id))
                    .map_or_else(
                        || self.gutter_width(),
                        |doc| nun_ui::EditorView::new(&doc.buffer, &self.palette).gutter_width(),
                    )
                    .min(text.width);
                hits.push(super::cells(Rect { width: gutter, ..text }), Target::Gutter, false);
                // Over the gutter, one column wide: the arrows are part of it,
                // and a press with no arrow there is the gutter's after all.
                let arrow = gutter.saturating_sub(2);
                if arrow < gutter {
                    hits.push(
                        super::cells(Rect { x: text.x + arrow, width: 1, ..text }),
                        Target::FoldArrow,
                        false,
                    );
                }
                hits.push(
                    super::cells(Rect { x: text.x + gutter, width: text.width - gutter, ..text }),
                    Target::Text,
                    false,
                );
            }
            let Some(strip) = self.strip_area(pane) else { continue };
            hits.push(super::cells(strip), Target::TabStrip, false);

            let tabs = self.tab_items(pane);
            let scroll = self.panes.get(pane).map_or(0, |pane| pane.scroll);
            for (index, at) in TabStrip::layout(&tabs, strip, scroll).into_iter().enumerate() {
                if at.width == 0 {
                    continue;
                }
                hits.push(super::cells(at), Target::Tab(pane, index), true);
                // The cross sits on top of its own tab, so a click on it
                // closes rather than selects.
                if let Some(close) = TabStrip::close_area(at) {
                    hits.push(super::cells(close), Target::TabClose(pane, index), true);
                }
            }
        }

        // The dividers, which are drag targets.
        for (index, divider) in
            self.panes.layout().dividers(self.panes_area()).into_iter().enumerate()
        {
            hits.push(super::cells(divider.area), Target::Divider(index), true);
        }
    }

    /// Keep the active tab of the focused pane in view.
    pub(super) fn follow_tab(&mut self) {
        let focus = self.panes.focus();
        let Some(strip) = self.strip_area(focus) else {
            self.panes.focused_mut().scroll = 0;
            return;
        };
        let tabs = self.tab_items(focus);
        let pane = self.panes.focused_mut();
        pane.scroll = TabStrip::scroll_to(&tabs, strip, pane.active, pane.scroll);
    }

    /// Open `path` in the focused pane.
    pub(super) fn open_in_tab(&mut self, path: &Path) {
        if let Some(id) =
            self.docs.iter().find(|doc| doc.buffer.path() == Some(path)).map(|doc| doc.id)
            && let Some((pane, index)) = self.panes.find(id)
        {
            self.panes.set_focus(pane);
            self.panes.focused_mut().active = index;
            self.after_tab_change(path);
            return;
        }

        match crate::open(path) {
            Ok((mut buffer, report)) => {
                buffer.set_tab_width(self.doc().buffer.tab_width());
                // An untouched, unnamed buffer is the one nun started with; a
                // file opened into it replaces it rather than sitting beside
                // an empty tab.
                if self.doc().buffer.path().is_none() && !self.doc().buffer.is_modified() {
                    let doc = self.doc_mut();
                    doc.buffer = buffer;
                    doc.scroll = 0;
                    let id = doc.id;
                    // The scratch buffer nun starts with has no name, and so no
                    // language; the file that has just replaced it does.
                    self.syntax_open(id);
                    self.lsp_open(id);
                } else {
                    let id = self.next_doc;
                    self.next_doc += 1;
                    self.docs.push(Document {
                        id,
                        buffer,
                        scroll: 0,
                        syntax: super::syntax::Highlighting::default(),
                    });
                    self.panes.open(id);
                    self.syntax_open(id);
                    self.lsp_open(id);
                }
                self.end_drag();
                if report.lossy {
                    self.message = Some(
                        "This file is not valid UTF-8. Saving it would destroy the original bytes."
                            .into(),
                    );
                }
                self.after_tab_change(path);
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn after_tab_change(&mut self, path: &Path) {
        self.reveal_in_tree(path);
        self.follow_tab();
        self.forget_closed_docs();
    }

    /// An empty buffer, with the tab width the rest of the editor is using.
    ///
    /// Any open document will do for that: they all took it from the same
    /// configuration. Asked while a pane is momentarily empty — between
    /// closing its last tab and giving it a fresh one — there may be no
    /// focused document to ask, which is why this does not use one.
    fn empty_buffer(&self) -> nun_core::Buffer {
        let mut buffer = nun_core::Buffer::new();
        if let Some(doc) = self.docs.first() {
            buffer.set_tab_width(doc.buffer.tab_width());
        }
        buffer
    }

    /// Drop documents no pane holds any more.
    fn forget_closed_docs(&mut self) {
        let before: Vec<super::panes::DocId> =
            self.docs.iter().map(|document| document.id).collect();
        let open = self.panes.open_docs();
        for id in &before {
            if !open.contains(id) {
                self.remember_folds_of(*id);
            }
        }
        self.docs.retain(|doc| open.contains(&doc.id));
        self.forget_from_parser(&before);
    }

    /// Show tab `index` of `pane`.
    pub(super) fn select_tab(&mut self, pane: usize, index: usize) {
        if !self.panes.set_focus(pane) {
            return;
        }
        let state = self.panes.focused_mut();
        if index >= state.tabs.len() {
            return;
        }
        state.active = index;
        self.end_drag();
        self.follow_tab();
        if let Some(path) = self.doc().buffer.path().map(Path::to_path_buf) {
            self.reveal_in_tree(&path);
        }
    }

    /// Move `delta` tabs along in the focused pane, wrapping at either end.
    pub(super) fn step_tab(&mut self, delta: isize) -> Outcome {
        let pane = self.panes.focus();
        let count = self.panes.focused().tabs.len();
        if count < 2 {
            return Outcome::Continue;
        }
        let count_i = isize::try_from(count).unwrap_or(isize::MAX);
        let active = isize::try_from(self.panes.focused().active).unwrap_or(0);
        let next = (active + delta).rem_euclid(count_i);
        self.select_tab(pane, usize::try_from(next).unwrap_or(0));
        self.focus = Focus::Editor;
        Outcome::Redraw
    }

    /// Close tab `index` of `pane`, asking first if it has unsaved changes.
    pub(super) fn close_tab(&mut self, pane: usize, index: usize) -> Outcome {
        let modified = self
            .panes
            .get(pane)
            .and_then(|state| state.tabs.get(index))
            .and_then(|id| self.doc_by(*id))
            .map(|doc| (doc.buffer.is_modified(), super::display_path(doc.buffer.path())));
        let Some((modified, name)) = modified else { return Outcome::Continue };

        if modified {
            self.select_tab(pane, index);
            self.prompt = Some(Prompt::unsaved(Purpose::UnsavedThenClose(pane, index), &name));
            return Outcome::Redraw;
        }
        self.drop_tab(pane, index)
    }

    /// Close tab `index` of `pane`, changes and all.
    pub(super) fn drop_tab(&mut self, pane: usize, index: usize) -> Outcome {
        let Some((_, pane_closed)) = self.panes.close_tab(pane, index) else {
            return Outcome::Continue;
        };
        if !pane_closed && self.panes.get(pane).is_some_and(|state| state.tabs.is_empty()) {
            // The only pane, emptied: an empty buffer rather than an empty
            // screen, because closing a file is not quitting.
            let id = self.next_doc;
            self.next_doc += 1;
            let buffer = self.empty_buffer();
            self.docs.push(Document {
                id,
                buffer,
                scroll: 0,
                syntax: super::syntax::Highlighting::default(),
            });
            let state = self.panes.get_mut(pane).expect("it is still there");
            state.tabs.push(id);
            state.active = 0;
        }
        self.forget_closed_docs();
        self.end_drag();
        self.follow_tab();
        Outcome::Redraw
    }

    /// Tell the parser about documents that have gone.
    fn forget_from_parser(&mut self, before: &[super::panes::DocId]) {
        for id in before {
            if !self.docs.iter().any(|document| document.id == *id) {
                self.syntax_close(*id);
                self.lsp_close(*id);
            }
        }
    }

    /// The path of every open file, pane by pane.
    #[cfg(test)]
    pub(super) fn open_paths(&self) -> Vec<Option<std::path::PathBuf>> {
        self.panes
            .open_docs()
            .into_iter()
            .filter_map(|id| self.doc_by(id))
            .map(|doc| doc.buffer.path().map(Path::to_path_buf))
            .collect()
    }

    // ── panes ───────────────────────────────────────────────────────────────

    /// Put another pane beside or below this one.
    ///
    /// The new pane starts empty rather than with a second copy of the file
    /// this one is showing: two panes editing copies of one path would race
    /// each other on save. To work on a file over there, drag its tab onto the
    /// new pane, or open one from the tree.
    pub(super) fn split_pane(&mut self, dir: Dir) -> Outcome {
        let new = self.next_doc;
        self.next_doc += 1;
        self.docs.push(Document {
            id: new,
            buffer: self.empty_buffer(),
            scroll: 0,
            syntax: super::syntax::Highlighting::default(),
        });
        self.panes.split(self.panes.focus(), dir, false, vec![new]);
        self.focus = Focus::Editor;
        self.follow_tab();
        Outcome::Redraw
    }

    /// Close a pane. Whatever was open in it moves to the pane that takes its
    /// space: closing a pane is about the screen, not about the files, and it
    /// must never throw away unsaved work.
    pub(super) fn close_pane(&mut self, pane: usize) -> Outcome {
        if !self.panes.fold_into_neighbour(pane) {
            self.message = Some("That is the only pane.".into());
            return Outcome::Redraw;
        }
        self.follow_tab();
        Outcome::Redraw
    }

    /// Give the keyboard to the next pane across the screen.
    pub(super) fn focus_next_pane(&mut self) -> Outcome {
        let ids: Vec<usize> = self.panes.all().iter().map(|pane| pane.id).collect();
        if ids.len() < 2 {
            return Outcome::Continue;
        }
        let at = ids.iter().position(|id| *id == self.panes.focus()).unwrap_or(0);
        self.panes.set_focus(ids[(at + 1) % ids.len()]);
        self.focus = Focus::Editor;
        self.follow_tab();
        Outcome::Redraw
    }

    // ── the pointer ─────────────────────────────────────────────────────────

    /// A press on a strip.
    pub(super) fn tab_press(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        target: Target,
    ) -> Outcome {
        self.focus = Focus::Editor;
        match target {
            Target::TabClose(pane, index) => self.close_tab(pane, index),
            Target::Tab(pane, index) => {
                self.select_tab(pane, index);
                self.tab_drag = Some(TabDrag {
                    pane,
                    from: index,
                    press: (mouse.column, mouse.row),
                    moved: false,
                    drop: None,
                });
                Outcome::Redraw
            }
            Target::TabStrip => {
                if let Some(pane) =
                    self.panes.layout().pane_at(self.panes_area(), mouse.column, mouse.row)
                {
                    self.panes.set_focus(pane);
                }
                Outcome::Redraw
            }
            _ => Outcome::Redraw,
        }
    }

    /// The pointer moved with a tab held. `None` when no tab is being dragged.
    pub(super) fn tab_drag_to(&mut self, column: u16, row: u16) -> Option<Outcome> {
        let mut drag = self.tab_drag?;
        if (column, row) != drag.press {
            drag.moved = true;
        }
        drag.drop = drag.moved.then(|| self.drop_for(column, row)).flatten();
        self.tab_drag = Some(drag);
        Some(Outcome::Redraw)
    }

    /// Where a tab let go at `(column, row)` would land.
    fn drop_for(&self, column: u16, row: u16) -> Option<Drop> {
        let pane = self.panes.layout().pane_at(self.panes_area(), column, row)?;
        if let Some(strip) = self.strip_area(pane)
            && strip.contains((column, row).into())
        {
            let tabs = self.tab_items(pane);
            let scroll = self.panes.get(pane).map_or(0, |pane| pane.scroll);
            return Some(Drop::Strip {
                pane,
                at: TabStrip::drop_index(&tabs, strip, scroll, column),
            });
        }
        let text = self.text_area_of(pane)?;
        match Edge::at(text, column, row) {
            Edge::Middle => {
                let at = self.panes.get(pane).map_or(0, |pane| pane.tabs.len());
                Some(Drop::Strip { pane, at })
            }
            edge => Some(Drop::Split { pane, edge }),
        }
    }

    /// What the drop would look like, for drawing the preview.
    #[cfg(test)]
    pub(super) fn drop_preview(&self) -> Option<Rect> {
        self.drop_preview_in(self.panes_area())
    }

    /// The same, within the rectangle being drawn into.
    fn drop_preview_in(&self, whole: Rect) -> Option<Rect> {
        let (pane, edge) = match self.tab_drag?.drop? {
            Drop::Split { pane, edge } => (pane, Some(edge)),
            Drop::Strip { pane, .. } => (pane, None),
        };
        let rect = self.panes.rect_of(pane, whole)?;
        Some(edge.map_or(rect, |edge| edge.preview(rect)))
    }

    /// The button came up after a tab drag. `None` when there was none.
    pub(super) fn tab_release(&mut self) -> Option<Outcome> {
        let drag = self.tab_drag.take()?;
        let Some(drop) = drag.drop else { return Some(Outcome::Redraw) };

        match drop {
            Drop::Strip { pane, at } => {
                self.panes.move_tab(drag.pane, drag.from, pane, at);
            }
            Drop::Split { pane, edge } => {
                let Some((dir, first)) = edge.split() else { return Some(Outcome::Redraw) };
                let moved =
                    self.panes.get(drag.pane).and_then(|state| state.tabs.get(drag.from).copied());
                let Some(doc) = moved else { return Some(Outcome::Redraw) };

                // The pane it came from may be left empty, in which case it
                // closes — but not before the new one exists, or the layout
                // would lose the place the split was meant to go.
                let new = self.panes.split(pane, dir, first, vec![doc]);
                if new == pane {
                    return Some(Outcome::Redraw);
                }
                if let Some(index) = self
                    .panes
                    .get(drag.pane)
                    .and_then(|state| state.tabs.iter().position(|id| *id == doc))
                {
                    let state = self.panes.get_mut(drag.pane).expect("it is there");
                    state.tabs.remove(index);
                    state.active = state.active.min(state.tabs.len().saturating_sub(1));
                }
                // Dragging the only tab out of a pane leaves that pane empty.
                // It keeps its place with an empty buffer rather than
                // collapsing, which would undo the split just asked for.
                if self.panes.get(drag.pane).is_some_and(|state| state.tabs.is_empty()) {
                    let id = self.next_doc;
                    self.next_doc += 1;
                    let buffer = self.empty_buffer();
                    self.docs.push(Document {
                        id,
                        buffer,
                        scroll: 0,
                        syntax: super::syntax::Highlighting::default(),
                    });
                    let state = self.panes.get_mut(drag.pane).expect("it is there");
                    state.tabs.push(id);
                    state.active = 0;
                }
                self.panes.set_focus(new);
            }
        }
        self.forget_closed_docs();
        self.follow_tab();
        Some(Outcome::Redraw)
    }

    /// The middle button closes a tab, as it does everywhere else.
    pub(super) fn tab_middle_click(&mut self, target: Target) -> Outcome {
        match target {
            Target::Tab(pane, index) | Target::TabClose(pane, index) => self.close_tab(pane, index),
            _ => Outcome::Continue,
        }
    }

    /// The wheel over a strip scrolls it.
    pub(super) fn tab_scroll_by(&mut self, right: bool, pane: usize) -> Outcome {
        let Some(strip) = self.strip_area(pane) else { return Outcome::Continue };
        let tabs = self.tab_items(pane);
        let most = TabStrip::total_width(&tabs).saturating_sub(strip.width);
        let Some(state) = self.panes.get_mut(pane) else { return Outcome::Continue };
        state.scroll =
            if right { (state.scroll + 4).min(most) } else { state.scroll.saturating_sub(4) };
        Outcome::Redraw
    }

    // ── drawing ─────────────────────────────────────────────────────────────

    /// Draw every pane: its strip, its text, and the dividers between them.
    pub(super) fn render_panes(&self, area: Rect, cells: &mut ratatui::buffer::Buffer) {
        use ratatui::widgets::Widget as _;

        let whole = self.panes_area_in(area);
        for (pane, rect) in self.panes.rects(whole) {
            let (strip, text) = self.parts_of(pane, rect);
            if let Some(doc) = self
                .panes
                .get(pane)
                .and_then(super::panes::Pane::current)
                .and_then(|id| self.doc_by(id))
            {
                let focused = pane == self.panes.focus();
                nun_ui::EditorView::new(&doc.buffer, &self.palette)
                    .scrolled_to(doc.scroll)
                    .highlighted(App::spans_of(doc))
                    .foldable(&doc.syntax.folding.ranges)
                    .with_drop_marker(focused.then(|| self.drop_marker()).flatten())
                    .render(text, cells);
            }

            let Some(strip) = strip else { continue };
            let hovered = match self.hover.current() {
                Some(Target::Tab(over, index) | Target::TabClose(over, index)) if over == pane => {
                    Some(index)
                }
                _ => None,
            };
            let state = self.panes.get(pane);
            TabStrip::new(
                &self.tab_items(pane),
                &self.palette,
                state.map_or(0, |state| state.active),
            )
            .hovered(hovered)
            .scrolled_by(state.map_or(0, |state| state.scroll))
            .drop_at(self.strip_drop_for(pane))
            .render(strip, cells);
        }

        self.render_dividers(whole, cells);
        self.render_drop_preview(whole, cells);
    }

    /// Where a dragged tab would land in this pane's strip, if there.
    fn strip_drop_for(&self, pane: usize) -> Option<usize> {
        match self.tab_drag?.drop? {
            Drop::Strip { pane: over, at } if over == pane => Some(at),
            _ => None,
        }
    }

    fn render_dividers(&self, whole: Rect, cells: &mut ratatui::buffer::Buffer) {
        let dividers = self.panes.layout().dividers(whole);
        for (index, divider) in dividers.into_iter().enumerate() {
            let held = self.divider_drag == Some(index)
                || self.hover.current() == Some(Target::Divider(index));
            let style = if held {
                self.palette.fg(nun_theme::Role::LineStrong)
            } else {
                self.palette.fg(nun_theme::Role::Line)
            };
            let glyph = if divider.dir == Dir::Beside { '│' } else { '─' };
            for y in divider.area.top()..divider.area.bottom() {
                for x in divider.area.left()..divider.area.right() {
                    cells[(x, y)].set_char(glyph).set_style(style);
                }
            }
        }
    }

    /// The half of a pane a dropped tab would take, washed in the accent.
    fn render_drop_preview(&self, whole: Rect, cells: &mut ratatui::buffer::Buffer) {
        let Some(area) = self.drop_preview_in(whole) else { return };
        let matches_split =
            matches!(self.tab_drag.and_then(|drag| drag.drop), Some(Drop::Split { .. }));
        if !matches_split {
            return;
        }
        let style = self.palette.on(nun_theme::Role::Accent, nun_theme::Role::OnAccent);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                cells[(x, y)].set_style(style);
            }
        }
    }

    // ── dividers ────────────────────────────────────────────────────────────

    /// A press on a divider starts resizing it.
    pub(super) fn divider_press(&mut self, index: usize, double: bool) -> Outcome {
        let dividers = self.panes.layout().dividers(self.panes_area());
        let Some(divider) = dividers.get(index) else { return Outcome::Continue };
        if double {
            // A double-click evens the two sides.
            let path = divider.path.clone();
            let moved = self.panes.layout_mut().even(&path);
            return if moved { Outcome::Redraw } else { Outcome::Continue };
        }
        self.divider_drag = Some(index);
        Outcome::Redraw
    }

    /// The pointer moved with a divider held.
    pub(super) fn divider_drag_to(&mut self, column: u16, row: u16) -> Option<Outcome> {
        let index = self.divider_drag?;
        let whole = self.panes_area();
        let dividers = self.panes.layout().dividers(whole);
        let path = dividers.get(index)?.path.clone();
        let area = self.panes.layout().area_of(&path, whole)?;
        self.panes.layout_mut().drag_divider(&path, area, column, row);
        self.follow_tab();
        Some(Outcome::Redraw)
    }

    /// The button came up after a divider drag.
    pub(super) fn divider_release(&mut self) -> Option<Outcome> {
        self.divider_drag.take().map(|_| Outcome::Redraw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, KeySet, defaults};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use std::fs;
    use tempfile::TempDir;

    /// Create each file with its own name as contents, leaving alone any a
    /// test wrote itself.
    pub(super) fn files(dir: &TempDir, names: &[&str]) {
        for name in names {
            let path = dir.path().join(name);
            if path.exists() {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, format!("{name}\n")).unwrap();
        }
    }

    pub(super) fn app_with(dir: &TempDir, names: &[&str]) -> App {
        files(dir, names);
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 70, 12));
        for name in names {
            app.open_in_tab(&dir.path().join(name));
        }
        // Opening through the tree or a command lays out again; here the tabs
        // are opened directly, so the hit regions need catching up.
        app.relayout();
        app
    }

    /// The labels of the focused pane's tabs.
    fn labels(app: &App) -> Vec<String> {
        app.tab_items(app.panes.focus()).into_iter().map(|tab| tab.label).collect()
    }

    /// Which tab of the focused pane is showing.
    fn active(app: &App) -> usize {
        app.panes.focused().active
    }

    pub(super) fn press(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
    }

    pub(super) fn drag(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Drag(MouseButton::Left), column, row);
    }

    pub(super) fn release(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }));
    }

    pub(super) fn click(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
        mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    }

    /// Where a tab of the focused pane is drawn.
    pub(super) fn tab_at(app: &App, index: usize) -> Rect {
        let pane = app.panes.focus();
        let area = app.strip_area(pane).expect("the strip is shown");
        let scroll = app.panes.focused().scroll;
        TabStrip::layout(&app.tab_items(pane), area, scroll)[index]
    }

    #[test]
    fn a_file_with_a_name_has_a_tab_and_an_untitled_buffer_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        assert!(app.strip_area(app.panes.focus()).is_some(), "a named file has a tab");
        assert_eq!(labels(&app), ["a.rs"]);

        // Closing it leaves an untitled buffer, which has nothing to label.
        app.run(Command::CloseTab);
        assert!(app.strip_area(app.panes.focus()).is_none(), "the row goes back to the text");
    }

    #[test]
    fn with_no_strip_the_status_line_closes_the_file() {
        // An untitled buffer has no tab, so the status line carries the cross.
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &[]);
        app.set_viewport(Rect::new(0, 0, 70, 12));
        app.relayout();
        let close = app.status_parts(app.areas().1).close.expect("a cross is offered");
        click(&mut app, close.x + 1, close.y);
        assert_eq!(app.open_paths(), vec![None]);
    }

    #[test]
    fn tabs_that_would_say_the_same_thing_say_where_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(&dir, &["src/mod.rs", "tests/mod.rs", "build.rs"]);
        assert_eq!(labels(&app), ["src/mod.rs", "tests/mod.rs", "build.rs"]);
    }

    #[test]
    fn clicking_a_tab_shows_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()));
        assert_eq!(app.buffer().text().to_string(), "a.rs\n");
    }

    #[test]
    fn each_tab_keeps_its_own_place_in_its_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("long.rs"), "x\n".repeat(200)).unwrap();
        fs::write(dir.path().join("short.rs"), "y\n").unwrap();
        let mut app = app_with(&dir, &["long.rs", "short.rs"]);

        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        for _ in 0..40 {
            app.handle(Event::Key(KeyEvent::from(KeyCode::Down)));
        }
        let scrolled = app.scroll();
        assert!(scrolled > 0, "the view followed the caret down");

        let second = tab_at(&app, 1);
        click(&mut app, second.x + 1, second.y);
        assert_eq!(app.scroll(), 0);
        let first = tab_at(&app, 0);
        click(&mut app, first.x + 1, first.y);
        assert_eq!(app.scroll(), scrolled, "back where it was left");
    }

    #[test]
    fn the_cross_closes_a_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let close = TabStrip::close_area(tab_at(&app, 1)).unwrap();
        click(&mut app, close.x, close.y);
        assert_eq!(labels(&app), ["a.rs"]);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()));
    }

    #[test]
    fn the_middle_button_closes_a_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        let second = tab_at(&app, 1);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Middle), second.x + 1, second.y);
        assert_eq!(labels(&app), ["a.rs", "c.rs"]);
    }

    #[test]
    fn closing_a_tab_with_unsaved_changes_asks_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        assert!(app.buffer().is_modified());

        app.run(Command::CloseTab);
        assert_eq!(app.open_paths().len(), 2, "still there while it asks");

        // Don't save is the middle button.
        let prompt = app.prompt.clone().unwrap();
        let discard = prompt.button_areas(app.areas().1)[1];
        click(&mut app, discard.x + 1, discard.y);
        assert_eq!(labels(&app), ["a.rs"]);
        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "b.rs\n", "not saved");
    }

    #[test]
    fn saving_from_the_close_prompt_writes_the_file_and_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::CloseTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Enter)));

        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "zb.rs\n");
        assert_eq!(labels(&app), ["a.rs"]);
    }

    #[test]
    fn cancelling_the_close_prompt_keeps_the_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::CloseTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Esc)));
        assert_eq!(labels(&app), ["a.rs", "b.rs"]);
        assert!(app.buffer().is_modified());
    }

    #[test]
    fn closing_the_last_tab_leaves_an_empty_buffer_rather_than_quitting() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        assert_eq!(app.run(Command::CloseTab), Outcome::Redraw);
        assert_eq!(app.open_paths(), vec![None]);
        assert_eq!(app.buffer().text().to_string(), "");
    }

    #[test]
    fn quitting_counts_every_unsaved_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));
        app.run(Command::PreviousTab);
        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('z'))));

        assert_eq!(app.run(Command::Quit), Outcome::Redraw);
        let message = app.message().unwrap();
        assert!(message.contains("2 files have unsaved changes"), "{message}");
        assert_eq!(app.run(Command::Quit), Outcome::Quit, "asking twice is enough");
    }

    #[test]
    fn the_next_and_previous_tab_wrap_around() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        assert_eq!(active(&app), 2);
        app.run(Command::NextTab);
        assert_eq!(active(&app), 0, "past the last is the first");
        app.run(Command::PreviousTab);
        assert_eq!(active(&app), 2);
    }

    #[test]
    fn a_tab_dragged_along_the_strip_changes_places() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs", "c.rs"]);
        let first = tab_at(&app, 0);
        let last = tab_at(&app, 2);

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), first.x + 1, first.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), last.right() - 1, last.y);
        assert_eq!(
            app.tab_drag.unwrap().drop.map(|drop| match drop {
                Drop::Strip { at, .. } => at,
                Drop::Split { .. } => usize::MAX,
            }),
            Some(3),
            "the drop indicator is past the last"
        );
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), last.right() - 1, last.y);

        assert_eq!(labels(&app), ["b.rs", "c.rs", "a.rs"]);
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()), "it stays active");
    }

    #[test]
    fn a_tab_dropped_where_it_started_stays_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let first = tab_at(&app, 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), first.x + 1, first.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), first.x + 1, first.y);
        assert_eq!(labels(&app), ["a.rs", "b.rs"]);
    }

    #[test]
    fn the_strip_sits_above_the_text_rather_than_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.set_viewport(Rect::new(0, 0, 40, 6));
        let mut harness = nun_ui::Harness::new(40, 6);
        harness.draw(crate::AppView(&app));

        let drawn = harness.to_text();
        let rows: Vec<&str> = drawn.lines().collect();
        assert!(rows[0].contains("a.rs") && rows[0].contains("b.rs"), "the strip: {:?}", rows[0]);
        assert!(rows[1].contains("b.rs"), "the first line of the file: {:?}", rows[1]);

        // And the text is one row shorter for it: the strip stays while a
        // named file is open, and the row comes back with an untitled buffer.
        let with_tabs = app.text_height();
        app.drop_tab(app.panes.focus(), 1);
        app.drop_tab(app.panes.focus(), 0);
        assert_eq!(app.text_height(), with_tabs + 1);
    }

    #[test]
    fn many_tabs_scroll_rather_than_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..12).map(|index| format!("file{index}.rs")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let app = app_with(&dir, &names);

        let area = app.strip_area(app.panes.focus()).unwrap();
        assert!(
            TabStrip::total_width(&app.tab_items(app.panes.focus())) > area.width,
            "they do not all fit"
        );
        assert!(app.panes.focused().scroll > 0, "the strip followed the one just opened");

        let active =
            TabStrip::layout(&app.tab_items(app.panes.focus()), area, app.panes.focused().scroll)
                [active(&app)];
        assert!(active.width > 0, "the active tab is on screen");
    }
}

#[cfg(test)]
mod split_tests {
    use super::super::tabs::tests::*;
    use super::*;
    use crate::commands::Command;

    /// Drag the tab at `index` of the focused pane to `(column, row)` and let
    /// go there.
    fn drag_tab(app: &mut App, index: usize, column: u16, row: u16) {
        let at = tab_at(app, index);
        press(app, at.x + 1, at.y);
        drag(app, column, row);
        release(app, column, row);
    }

    #[test]
    fn a_split_puts_a_second_pane_beside_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);

        assert_eq!(app.panes.len(), 2);
        let rects = app.panes.rects(app.panes_area());
        assert_eq!(rects[0].1.y, rects[1].1.y, "side by side");
        assert!(rects[0].1.x < rects[1].1.x);
        assert_eq!(app.buffer().path(), None, "the new pane starts empty");
    }

    #[test]
    fn a_split_below_divides_the_other_way_and_nests() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);
        app.run(Command::SplitBelow);

        assert_eq!(app.panes.len(), 3);
        let rects = app.panes.rects(app.panes_area());
        assert_eq!(rects.len(), 3);
        // The two on the right are one above the other.
        let right: Vec<_> = rects.iter().filter(|(_, rect)| rect.x > 0).collect();
        assert_eq!(right.len(), 2);
        assert_ne!(right[0].1.y, right[1].1.y);
    }

    #[test]
    fn closing_the_last_tab_of_a_pane_closes_the_pane() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);
        assert_eq!(app.panes.len(), 2);

        app.run(Command::CloseTab);
        assert_eq!(app.panes.len(), 1, "an emptied pane goes, and its space with it");
        assert_eq!(app.panes.rects(app.panes_area())[0].1, app.panes_area());
    }

    #[test]
    fn closing_the_only_pane_is_refused_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::ClosePane);
        assert_eq!(app.panes.len(), 1);
        assert!(app.message().unwrap().contains("only pane"), "{:?}", app.message());
    }

    #[test]
    fn the_keyboard_moves_between_panes_and_clicking_one_focuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);
        let new = app.panes.focus();

        app.run(Command::NextPane);
        assert_ne!(app.panes.focus(), new);

        // Clicking in the other pane's text gives it the keyboard back.
        let rect = app.pane_area(new).unwrap();
        click(&mut app, rect.x + 5, rect.y + 2);
        assert_eq!(app.panes.focus(), new);
    }

    #[test]
    fn a_tab_dragged_onto_another_pane_moves_there_with_its_place_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        app.run(Command::SplitBeside);
        let right = app.panes.focus();
        app.run(Command::NextPane);
        let left = app.panes.focus();

        // Scroll the tab being dragged, so its state has something to keep.
        app.doc_mut().scroll = 0;
        let target = app.pane_area(right).unwrap();
        drag_tab(&mut app, 0, target.x + target.width / 2, target.y + target.height / 2);

        assert_eq!(app.panes.get(left).map(|pane| pane.tabs.len()), Some(1));
        assert_eq!(app.panes.focus(), right, "the keyboard follows the tab");
        assert_eq!(app.panes.get(right).map(|pane| pane.tabs.len()), Some(2));
    }

    #[test]
    fn a_tab_dragged_to_a_pane_edge_splits_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        assert_eq!(app.panes.len(), 1);

        let pane = app.pane_area(app.panes.focus()).unwrap();
        // The right-hand quarter: a drop there splits beside.
        drag_tab(&mut app, 0, pane.right() - 2, pane.y + pane.height / 2);

        assert_eq!(app.panes.len(), 2, "the drop made a pane");
        let rects = app.panes.rects(app.panes_area());
        assert!(rects[0].1.x < rects[1].1.x);
        assert_eq!(app.panes.focused().tabs.len(), 1, "the dragged tab, on its own");
    }

    #[test]
    fn the_drop_preview_shows_the_half_that_would_be_taken() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let pane = app.pane_area(app.panes.focus()).unwrap();
        let at = tab_at(&app, 0);

        press(&mut app, at.x + 1, at.y);
        drag(&mut app, pane.x + 1, pane.y + pane.height / 2);

        let preview = app.drop_preview().expect("a preview is shown");
        assert_eq!(preview.width, pane.width / 2, "the left half");
        assert_eq!(preview.x, pane.x);
        release(&mut app, pane.x + 1, pane.y + pane.height / 2);
    }

    #[test]
    fn dragging_a_divider_resizes_and_double_clicking_it_evens_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);

        let divider = app.panes.layout().dividers(app.panes_area())[0].area;
        press(&mut app, divider.x, divider.y + 2);
        drag(&mut app, 20, divider.y + 2);
        release(&mut app, 20, divider.y + 2);

        let rects = app.panes.rects(app.panes_area());
        assert!(rects[0].1.width.abs_diff(20) <= 1, "dragged to where it was put: {rects:?}");

        // A double-click on the divider puts them back to half and half.
        let divider = app.panes.layout().dividers(app.panes_area())[0].area;
        click(&mut app, divider.x, divider.y + 2);
        click(&mut app, divider.x, divider.y + 2);
        let rects = app.panes.rects(app.panes_area());
        let (first, second) = (rects[0].1.width, rects[1].1.width);
        assert!(first.abs_diff(second) <= 1, "{first} and {second}");
    }

    #[test]
    fn closing_a_pane_keeps_the_files_that_were_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let pane = app.pane_area(app.panes.focus()).unwrap();
        drag_tab(&mut app, 0, pane.right() - 2, pane.y + pane.height / 2);
        assert_eq!(app.panes.len(), 2);

        // Unsaved changes in the pane about to be closed.
        app.handle(nun_ui::Event::Key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('z'),
        )));
        assert!(app.buffer().is_modified());

        app.run(Command::ClosePane);
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.open_paths().len(), 2, "both files are still open");
        assert_eq!(
            app.docs.iter().filter(|doc| doc.buffer.is_modified()).count(),
            1,
            "and the unsaved one is still unsaved, not silently dropped"
        );
    }

    #[test]
    fn quitting_still_counts_unsaved_files_in_a_pane_that_was_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let pane = app.pane_area(app.panes.focus()).unwrap();
        drag_tab(&mut app, 0, pane.right() - 2, pane.y + pane.height / 2);
        app.handle(nun_ui::Event::Key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('z'),
        )));
        app.run(Command::ClosePane);

        assert_eq!(app.run(Command::Quit), Outcome::Redraw, "it asks rather than losing them");
    }

    #[test]
    fn dragging_the_only_tab_to_an_edge_splits_rather_than_collapsing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs"]);
        let pane = app.pane_area(app.panes.focus()).unwrap();
        drag_tab(&mut app, 0, pane.right() - 2, pane.y + pane.height / 2);

        assert_eq!(app.panes.len(), 2, "the split stands");
        assert_eq!(app.buffer().path(), Some(dir.path().join("a.rs").as_path()));
        let other: Vec<_> =
            app.panes.all().iter().filter(|it| it.id != app.panes.focus()).collect();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].tabs.len(), 1, "the pane it left keeps an empty buffer");
    }

    #[test]
    fn each_pane_draws_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, &["a.rs", "b.rs"]);
        let pane = app.pane_area(app.panes.focus()).unwrap();
        drag_tab(&mut app, 0, pane.right() - 2, pane.y + pane.height / 2);

        let mut harness = nun_ui::Harness::new(70, 12);
        app.set_viewport(Rect::new(0, 0, 70, 12));
        harness.draw(crate::AppView(&app));

        let drawn = harness.to_text();
        assert!(drawn.contains("a.rs"), "the left pane's file: {drawn}");
        assert!(drawn.contains("b.rs"), "the right pane's file: {drawn}");
        assert!(drawn.contains('│'), "and a divider between them");
    }
}
