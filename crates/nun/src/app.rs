//! The editor: state, one event at a time, and what to draw.
//!
//! Deliberately free of terminal I/O. `App` takes an event and mutates itself;
//! `main` does the reading and the drawing. That keeps the whole of the editor's
//! behaviour testable without a tty, which is how the keymap and the scrolling
//! are checked below.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nun_core::{Buffer, Range, SaveError, Selections};
use nun_input::{
    Chords, Clicks, Code, HitMap, Hover, Key, Keymap, Mods, PLATFORM_THRESHOLD, Resolved, Sequence,
};
use nun_theme::Role;
use nun_ui::text_width;
use nun_ui::{EditorView, Event, Glyph, Menu, Palette, TreeButton, TreeView};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::commands::Command;

mod card;
mod code_actions;
mod completion;
mod diagnostics;
mod folds;
mod format;
mod hover;
mod lsp;
mod navigation;
mod palette;
mod panes;
mod pointer;
mod prompt;
mod rename;
mod search;
mod sidebar;
mod syntax;
mod tabs;
mod workspace_edit;

/// What the editor wants the caller to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Carry on.
    Continue,
    /// Redraw and carry on.
    Redraw,
    /// Put the terminal back, stop, and re-enter on resume.
    Suspend,
    /// Shut down.
    Quit,
}

impl Outcome {
    /// The stronger of two outcomes, for when several events are handled
    /// before one frame: a quit anywhere in a burst wins, then a suspend, then
    /// a redraw.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Quit, _) | (_, Self::Quit) => Self::Quit,
            (Self::Suspend, _) | (_, Self::Suspend) => Self::Suspend,
            (Self::Redraw, _) | (_, Self::Redraw) => Self::Redraw,
            _ => Self::Continue,
        }
    }
}

/// How long an unfinished chord waits for its next key.
const CHORD_TIMEOUT: Duration = Duration::from_millis(1000);

/// How long the pointer rests on a hover target before its dwell fires.
const HOVER_DWELL: Duration = Duration::from_millis(500);

/// Everything on screen a pointer can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The text of the buffer.
    Text,
    /// The line-number gutter beside it.
    Gutter,
    /// The mark in the gutter beside a line that has code actions.
    Lightbulb,
    /// The column of the gutter the fold arrows are drawn in.
    FoldArrow,
    /// The status line.
    Status,
    /// A tab: which pane, and which tab of it.
    Tab(usize, usize),
    /// A tab's close cross.
    TabClose(usize, usize),
    /// The tab strip beside the tabs.
    TabStrip,
    /// The divider between two panes, by its index in the layout's list.
    Divider(usize),
    /// The file-tree toggle at the left of the status line.
    StatusFiles,
    /// The button at the left of the status line that opens the palette.
    StatusSearch,
    /// The name of the file's language server in the status line, which
    /// restarts it.
    StatusLsp,
    /// The palette itself.
    Palette,
    /// One of its rows.
    PaletteRow(usize),
    /// Everywhere outside it, which a click there closes it from.
    PaletteOutside,
    /// The Undo offered after a file operation.
    StatusUndo,
    /// The cross that closes the open file when there is no tab strip.
    StatusClose,
    /// A button of the question in the status line.
    PromptButton(usize),
    /// The sidebar's header row.
    TreeHeader,
    /// One of the header's buttons.
    TreeButton(TreeButton),
    /// A row of the file tree.
    TreeRow(usize),
    /// The sidebar below its last row.
    TreeEmpty,
    /// The sidebar's right edge, dragged to resize it.
    SidebarEdge,
    /// The search panel's header row.
    SearchHeader,
    /// The button in it that goes back to the file tree.
    SearchBack,
    /// The row the query is typed into.
    SearchQuery,
    /// The row the replacement is typed into.
    SearchReplace,
    /// The button that applies the replace.
    SearchApply,
    /// The cell that strikes one hit out of the replace, or puts it back.
    SearchMarker(usize),
    /// One of the search toggles.
    SearchButton(nun_ui::SearchButton),
    /// A row of the results: a file, or one of its matching lines.
    SearchRow(usize),
    /// The panel below its last row.
    SearchEmpty,
    /// The references panel's button back to the file tree.
    ReferencesBack,
    /// A row of the references: a file, or one reference in it.
    ReferencesRow(usize),
    /// The rest of the references panel.
    ReferencesEmpty,
    /// An item of the open menu.
    MenuItem(usize),
    /// Everywhere outside the open menu, which a click there closes.
    MenuOutside,
    /// A pane's diagnostic rail, away from its marks.
    Rail(usize),
    /// A row of a pane's rail with marks on it.
    RailMark(usize, u16),
    /// An underlined diagnostic in a pane: which pane, and which of its
    /// document's diagnostics. Pressed, it is the text under it.
    Diagnostic(usize, usize),
    /// The card, away from its buttons.
    Card,
    /// One of the card's buttons.
    CardButton(usize),
    /// A link in the card, by its index in the card's links.
    CardLink(usize),
    /// The diagnostic counts in the status line.
    StatusProblems,
    /// The completion popup, and the documentation beside it.
    Completion,
    /// One of its rows, by its place in the list.
    CompletionRow(usize),
    /// Somewhere in the preview of an edit across files.
    EditPreview(workspace_edit::Spot),
}

impl Target {
    /// What a press on this lands on: an underline is the text beneath it.
    const fn pressed(self) -> Self {
        match self {
            Self::Diagnostic(..) => Self::Text,
            target => target,
        }
    }

    const fn in_card(self) -> bool {
        matches!(self, Self::Card | Self::CardButton(_) | Self::CardLink(_))
    }

    const fn in_diagnostics(self) -> bool {
        matches!(self, Self::Rail(_) | Self::RailMark(..) | Self::StatusProblems)
    }

    const fn in_tabs(self) -> bool {
        matches!(self, Self::Tab(..) | Self::TabClose(..) | Self::TabStrip)
    }

    const fn in_completion(self) -> bool {
        matches!(self, Self::Completion | Self::CompletionRow(_))
    }

    const fn in_sidebar(self) -> bool {
        matches!(
            self,
            Self::TreeHeader
                | Self::TreeButton(_)
                | Self::TreeRow(_)
                | Self::TreeEmpty
                | Self::SidebarEdge
        )
    }

    /// Whether this is part of the search panel.
    const fn in_search(self) -> bool {
        matches!(
            self,
            Self::SearchHeader
                | Self::SearchBack
                | Self::SearchQuery
                | Self::SearchReplace
                | Self::SearchApply
                | Self::SearchMarker(_)
                | Self::SearchButton(_)
                | Self::SearchRow(_)
                | Self::SearchEmpty
        )
    }

    /// Whether this is part of the references panel.
    const fn in_references(self) -> bool {
        matches!(self, Self::ReferencesBack | Self::ReferencesRow(_) | Self::ReferencesEmpty)
    }
}

/// Which view the sidebar is showing. The two share its columns, so only one
/// of them is laid out, drawn or clicked at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SidebarView {
    /// The file tree.
    #[default]
    Files,
    /// The project search panel.
    Search,
    /// The references to a symbol.
    References,
    /// The preview of an edit across files — a rename, a code action — in
    /// the search panel's clothes.
    EditPreview,
}

/// What has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The text.
    #[default]
    Editor,
    /// The file tree.
    Sidebar,
    /// The project search panel, where typing goes to the query.
    Search,
    /// The preview of an edit across files.
    EditPreview,
}

/// One open file: its text, and where the view on it is.
///
/// A document's scroll belongs to the document, not to the screen, so
/// coming back to a tab comes back to where you were in it.
#[derive(Debug)]
pub struct Document {
    /// Which document this is, whichever pane or tab it moves to.
    id: panes::DocId,
    buffer: Buffer,
    scroll: usize,
    /// Its highlight runs, and what the parser has been told.
    syntax: syntax::Highlighting,
}

/// Where the one-time hint is: see [`App::hint`].
#[derive(Debug, Default, PartialEq, Eq)]
enum Hint {
    #[default]
    Absent,
    Waiting(String),
    Seen,
}

/// The running editor.
#[derive(Debug)]
pub struct App {
    /// Every open document, in no particular order: the panes say which are
    /// where, and in which order.
    docs: Vec<Document>,
    /// The panes, and how they divide the screen.
    panes: panes::Panes,
    /// The id the next document opened will take.
    next_doc: panes::DocId,
    palette: Palette,
    viewport: Rect,
    /// A transient message, shown until the next key.
    message: Option<String>,
    /// Things worth saying once, shown one at a time after `message`.
    notices: VecDeque<String>,
    /// A notice worth saying only once ever, followed until it has been seen
    /// so it can be remembered as said.
    hint: Hint,
    quit_confirmed: bool,
    keymap: Keymap<Command>,
    chords: Chords,
    /// What is where, as of the last layout.
    hits: HitMap<Target>,
    /// Which hover target the pointer is over.
    hover: Hover<Target>,
    /// Presses, counted into single, double and triple clicks.
    clicks: Clicks,
    /// A drag in progress.
    drag: Option<pointer::Drag>,
    /// Scrolling under a drag held past the edge of the text.
    autoscroll: Option<pointer::Autoscroll>,
    /// The file tree, when a folder is open.
    sidebar: Option<sidebar::Sidebar>,
    focus: Focus,
    /// A question in the status line.
    prompt: Option<prompt::Prompt>,
    /// A menu on screen.
    menu: Option<sidebar::OpenMenu>,
    /// Whether the status line offers to undo the file operation it reports.
    undo_offer: bool,
    /// Whether that offer is a redo, because the last change was an undo.
    last_undone: bool,
    /// A file asked for that should be opened once it has been created.
    open_when_created: Option<std::path::PathBuf>,
    /// A tab being dragged along the strip, or to another pane.
    tab_drag: Option<tabs::TabDrag>,
    /// A divider being dragged, by its index.
    divider_drag: Option<usize>,
    /// The palette, while it is open.
    finder: Option<palette::Palette>,
    /// How often each file has been opened from the palette.
    frecency: palette::Frecency,
    /// How many files the project has, once they have been counted.
    file_count: Option<usize>,
    /// How many palette searches have been asked for, ever. Monotonic, so an
    /// answer can always be matched to the keystroke that asked.
    searches: u64,
    /// The parser, once it has somewhere to post its answers.
    syntax: Option<nun_syntax::Worker>,
    /// When the parser is next due to be told what changed.
    syntax_deadline: Option<Instant>,
    /// Searching the project, and what it has found.
    search: search::Search,
    /// The outline of the file the palette was last asked about.
    symbols: syntax::Outline,
    /// Which of its two views the sidebar is showing.
    sidebar_view: SidebarView,
    /// When the typing has settled enough to start a walk.
    search_deadline: Option<Instant>,
    /// What is remembered from one session to the next.
    session: crate::session::Session,
    /// The language servers, once there is somewhere to post their news.
    lsp: Option<nun_lsp::Lsp>,
    /// Formatting asked of them, and where it happens on save.
    formatting: format::Formatting,
    /// Definitions, references, and the way back from them.
    navigation: navigation::Navigation,
    /// What the servers have said is wrong, and where.
    diagnostics: diagnostics::Diagnostics,
    /// The card on screen.
    card: Option<card::Card>,
    /// The completion popup, and the tab-stops of a snippet it inserted.
    completion: completion::Completion,
    /// A symbol being renamed, until the server's edit is in hand.
    rename: rename::Renaming,
    /// Resting on a symbol to see what it is.
    hovering: hover::Hovering,
    /// Edits across files — renames, code actions — their preview, and the
    /// last one applied.
    edits: workspace_edit::Edits,
    /// Quick fixes and code actions offered, and asked for.
    code_actions: code_actions::CodeActions,
}

impl App {
    /// The document being edited: the active tab of the focused pane.
    fn doc(&self) -> &Document {
        let id = self.panes.focused().current();
        id.and_then(|id| self.docs.iter().find(|doc| doc.id == id))
            .expect("the focused pane always shows a document")
    }

    /// The document being edited, to change.
    fn doc_mut(&mut self) -> &mut Document {
        let id = self.panes.focused().current();
        id.and_then(move |id| self.docs.iter_mut().find(|doc| doc.id == id))
            .expect("the focused pane always shows a document")
    }

    /// One document by id.
    fn doc_by(&self, id: panes::DocId) -> Option<&Document> {
        self.docs.iter().find(|doc| doc.id == id)
    }

    /// An editor over `buffer`, driven by `keymap`.
    #[must_use]
    pub fn new(buffer: Buffer, palette: Palette, keymap: Keymap<Command>) -> Self {
        let viewport = Rect::new(0, 0, 80, 24);
        let mut app = Self {
            docs: vec![Document {
                id: 0,
                buffer,
                scroll: 0,
                syntax: syntax::Highlighting::default(),
            }],
            panes: panes::Panes::single(vec![0]),
            next_doc: 1,
            palette,
            viewport,
            message: None,
            notices: VecDeque::new(),
            hint: Hint::Absent,
            quit_confirmed: false,
            keymap,
            chords: Chords::new(CHORD_TIMEOUT),
            hits: HitMap::new(cells(viewport)),
            hover: Hover::new(HOVER_DWELL),
            clicks: Clicks::new(PLATFORM_THRESHOLD),
            drag: None,
            autoscroll: None,
            sidebar: None,
            focus: Focus::Editor,
            prompt: None,
            menu: None,
            undo_offer: false,
            last_undone: false,
            open_when_created: None,
            tab_drag: None,
            divider_drag: None,
            finder: None,
            frecency: palette::Frecency::new(),
            file_count: None,
            searches: 0,
            syntax: None,
            syntax_deadline: None,
            search: search::Search::default(),
            symbols: syntax::Outline::default(),
            sidebar_view: SidebarView::Files,
            search_deadline: None,
            session: crate::session::Session::default(),
            lsp: None,
            formatting: format::Formatting::default(),
            navigation: navigation::Navigation::default(),
            diagnostics: diagnostics::Diagnostics::default(),
            card: None,
            completion: completion::Completion::default(),
            rename: rename::Renaming::default(),
            hovering: hover::Hovering::default(),
            edits: workspace_edit::Edits::default(),
            code_actions: code_actions::CodeActions::default(),
        };
        app.relayout();
        app
    }

    /// The buffer being edited. Only the tests read it; everything else goes
    /// through `doc()`.
    #[cfg(test)]
    pub fn buffer(&self) -> &Buffer {
        &self.doc().buffer
    }

    /// The first visible line of the buffer being edited.
    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.doc().scroll
    }

    /// The transient message shown in the status line, if any.
    #[cfg(test)]
    pub fn message(&self) -> Option<&str> {
        self.shown_message()
    }

    /// Use `threshold` as the longest gap that still joins presses into a
    /// double or triple click.
    pub fn set_double_click(&mut self, threshold: Duration) {
        self.clicks = Clicks::new(threshold);
    }

    /// Columns the line-number gutter takes.
    fn gutter_width(&self) -> u16 {
        EditorView::new(&self.doc().buffer, &self.palette).gutter_width()
    }

    /// The folder the file tree is rooted at, when one is open.
    fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.sidebar.as_ref().map(|sidebar| sidebar.tree.root().to_path_buf())
    }

    /// Ask the worker for something.
    fn send_job(&self, job: nun_workspace::Job) {
        if let Some(sidebar) = self.sidebar.as_ref() {
            sidebar.send(job);
        }
    }

    /// Say something once in the status line.
    ///
    /// Notices queue: each stays until a key is pressed, then the next one
    /// shows. The status line holds one message, and a notice that is
    /// immediately replaced by another was never really said.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.notices.push_back(message.into());
    }

    /// Say something that need never be said again: a notice like any other,
    /// except that once it has been seen — dismissed by a key or a click —
    /// [`App::hint_seen`] says so, for the caller to remember.
    pub fn hint(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.notices.push_back(message.clone());
        self.hint = Hint::Waiting(message);
    }

    /// Whether the hint has been seen and dismissed.
    #[must_use]
    pub fn hint_seen(&self) -> bool {
        self.hint == Hint::Seen
    }

    /// The message the status line is showing, if any.
    fn shown_message(&self) -> Option<&str> {
        self.message.as_deref().or_else(|| self.notices.front().map(String::as_str))
    }

    /// A key was pressed: whatever message was showing has been seen.
    fn acknowledge(&mut self) {
        self.undo_offer = false;
        if self.message.take().is_none() {
            let dismissed = self.notices.pop_front();
            if matches!((&self.hint, dismissed), (Hint::Waiting(hint), Some(gone)) if *hint == gone)
            {
                self.hint = Hint::Seen;
            }
        }
    }

    /// Tell the editor how much room it has.
    pub fn set_viewport(&mut self, viewport: Rect) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.relayout();
        }
    }

    /// Where the focused pane's text and the status line go.
    fn areas(&self) -> (Rect, Rect) {
        let area = self.viewport;
        let text = self.text_area_of(self.panes.focus()).unwrap_or(Rect::new(area.x, area.y, 0, 0));
        let status =
            Rect { y: area.bottom().saturating_sub(1), height: area.height.min(1), ..area };
        (text, status)
    }

    /// Record what is where, for resolving the pointer.
    ///
    /// Regions are pushed in paint order, so anything drawn on top — an overlay,
    /// a menu — is pushed later and captures the pointer above what is beneath.
    fn relayout(&mut self) {
        let (text, status) = self.areas();
        let gutter =
            EditorView::new(&self.doc().buffer, &self.palette).gutter_width().min(text.width);

        let mut hits = HitMap::new(cells(self.viewport));
        hits.push(cells(Rect { width: gutter, ..text }), Target::Gutter, false);
        hits.push(
            cells(Rect { x: text.x + gutter, width: text.width - gutter, ..text }),
            Target::Text,
            false,
        );
        hits.push(cells(status), Target::Status, false);
        // Before the panel is laid out or drawn: both ask what the rows say,
        // and an After row says what its hit becomes.
        self.refresh_search_previews();
        self.layout_sidebar(&mut hits);
        self.layout_search(&mut hits);
        self.layout_references(&mut hits);
        self.layout_edit_preview(&mut hits);
        self.layout_panes(&mut hits);
        self.layout_diagnostics(&mut hits);
        self.layout_card(&mut hits);
        self.layout_completion(&mut hits);
        self.layout_palette(&mut hits);

        let parts = self.status_parts(status);
        if let Some(files) = parts.files {
            hits.push(cells(files), Target::StatusFiles, true);
        }
        if let Some(search) = parts.search {
            // Clickable, but not a hover target: one small button is no reason
            // to have the terminal report every pointer movement all session.
            hits.push(cells(search), Target::StatusSearch, false);
        }
        if let Some(undo) = parts.undo {
            hits.push(cells(undo), Target::StatusUndo, true);
        }
        if let Some(close) = parts.close {
            // Clickable, but not a hover target: one small button is no reason
            // to have the terminal report every pointer movement all session.
            hits.push(cells(close), Target::StatusClose, false);
        }
        if let Some(lsp) = parts.lsp {
            // Not a hover target either, for the same reason: it is on screen
            // whenever a server is running, which is most of the time.
            hits.push(cells(lsp), Target::StatusLsp, false);
        }
        if let Some(problems) = parts.problems {
            hits.push(cells(problems), Target::StatusProblems, true);
        }
        if let Some(prompt) = &self.prompt {
            let area = self.prompt_area(status);
            for (index, area) in prompt.button_areas(area).into_iter().enumerate() {
                if area.width > 0 {
                    hits.push(cells(area), Target::PromptButton(index), true);
                }
            }
        }

        // The menu sits on top of everything, and everything outside it closes
        // it: a click beside a menu dismisses it rather than landing below.
        if let Some(menu) = &self.menu {
            hits.push(cells(self.viewport), Target::MenuOutside, false);
            for index in 0..menu.items.len() {
                let Ok(offset) = u16::try_from(index) else { break };
                let row = Rect { y: menu.area.y + offset, height: 1, ..menu.area };
                hits.push(cells(row), Target::MenuItem(index), true);
            }
        }
        self.hits = hits;
    }

    /// Where the status line's own buttons go.
    fn status_parts(&self, status: Rect) -> StatusParts {
        if self.prompt.is_some() || status.height == 0 {
            return StatusParts {
                files: None,
                search: None,
                undo: None,
                close: None,
                lsp: None,
                problems: None,
                text: status.x,
            };
        }
        let files = self.sidebar.as_ref().map(|_| Rect { width: 3.min(status.width), ..status });
        let after_files = files.map_or(status.x, Rect::right);
        // The palette is the one thing every command is reachable through, so
        // it has a button of its own rather than only a key.
        let search =
            (status.width > 12).then(|| Rect::new(after_files, status.y, 3, status.height.min(1)));
        let text = search.map_or(after_files, Rect::right);
        let undo = self.undo_offer.then(|| {
            let (left, _) = self.status();
            let x = text + u16::try_from(text_width(&left) + 2).unwrap_or(u16::MAX);
            Rect::new(x, status.y, u16::try_from(self.undo_label().len()).unwrap_or(6), 1)
        });
        let undo = undo.filter(|undo| undo.right() <= status.right());
        // With a strip on screen the tabs carry their own crosses; without
        // one, this is how the open file is closed with the mouse.
        let close = (self.strip_area(self.panes.focus()).is_none() && status.width > 10)
            .then(|| Rect::new(status.right() - 3, status.y, 3, 1));
        // The server's name sits just left of the caret's position, where the
        // file's language is named. Left out when there is no room for it
        // beside what the left half says.
        let lsp = self.lsp_label().and_then(|(label, _)| {
            let (left, right) = self.status();
            let width = u16::try_from(text_width(&label)).ok()?;
            let edge = close.map_or(status.right(), |close| close.x);
            let x = edge.checked_sub(u16::try_from(text_width(&right) + 1).ok()? + width)?;
            let used = text + u16::try_from(text_width(&left) + 1).ok()?;
            (x > undo.map_or(used, Rect::right)).then(|| Rect::new(x, status.y, width, 1))
        });
        let problems = self.problems_part(status, text, undo, close, lsp);
        StatusParts { files, search, undo, close, lsp, problems, text }
    }

    /// Whether anything on screen reacts to the pointer merely passing over it.
    ///
    /// The terminal's any-motion reporting is turned on only while this holds.
    /// That includes any time a language server could say a Ctrl-hovered
    /// symbol has a definition: nothing but motion reports carry the Ctrl.
    pub fn wants_motion(&self) -> bool {
        self.hits.has_hover_targets() || self.wants_link_motion() || self.wants_hover_motion()
    }

    /// When the editor next needs waking with no input, if ever.
    pub fn deadline(&self) -> Option<Instant> {
        let autoscroll = self.autoscroll.map(|scroll| scroll.next);
        [
            self.hover.deadline(),
            self.chords.deadline(),
            autoscroll,
            self.syntax_deadline(),
            self.search_deadline(),
            self.formatting.deadline(),
            self.link_deadline(),
            self.hover_deadline(),
            self.bulb_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A deadline passed with no input.
    pub fn tick(&mut self, now: Instant) -> Outcome {
        let before = (self.doc().scroll, self.doc().id);
        let chord = match self.chords.expire(&self.keymap, now) {
            Some(Resolved::Unbound(keys)) => {
                self.message = Some(format!("{} is not bound to anything", Sequence(&keys)));
                Outcome::Redraw
            }
            Some(resolved) => self.resolved(resolved, None),
            None => Outcome::Continue,
        };
        // A chord that ran a command got here without going through `handle`,
        // so the parser has not been told about anything it did.
        if chord == Outcome::Redraw {
            self.lsp_flush();
            self.syntax_changed(now);
            if before != (self.doc().scroll, self.doc().id) {
                self.syntax_scrolled(now);
            }
        }

        // Taking the dwell keeps the deadline from firing again.
        let dwell = match self.hover.dwell(now) {
            Some(target) => self.dwelt(target),
            None => Outcome::Continue,
        };

        let outcome = chord
            .and(dwell)
            .and(self.autoscroll_tick(now))
            .and(self.syntax_tick(now))
            .and(self.search_tick(now))
            .and(self.format_tick(now))
            .and(self.link_tick(now))
            .and(self.hover_tick(now))
            .and(self.bulb_follow(now))
            .and(self.bulb_tick(now));
        if outcome == Outcome::Redraw {
            self.relayout();
        }
        outcome
    }

    /// How many lines of text fit, once the status line and any tab strip
    /// have taken their rows.
    fn text_height(&self) -> usize {
        usize::from(self.areas().0.height)
    }

    /// Handle one event.
    pub fn handle(&mut self, event: Event) -> Outcome {
        self.handle_at(event, Instant::now())
    }

    /// Handle one event that arrived at `now`.
    pub fn handle_at(&mut self, event: Event, now: Instant) -> Outcome {
        let before = (self.doc().buffer.len_chars(), self.doc().scroll, self.doc().id);
        self.hover_before(&event);
        // Whatever Ctrl-hover underlined is stale once anything but the
        // pointer or a server has had a say.
        let unlinked = if matches!(event, Event::Key(_) | Event::Paste(_) | Event::Focus(false)) {
            self.clear_link()
        } else {
            Outcome::Continue
        };
        let outcome = self.dispatch(event, now).and(unlinked);
        // Whatever the event did to the text goes to the language servers
        // now, in the order it was done, and only then is anything asked
        // about it.
        self.lsp_flush();
        let outcome = outcome.and(self.completion_follow()).and(self.bulb_follow(now));
        // Anything that changed the text or moved the view changes what the
        // parser should be looking at.
        let after = (self.doc().buffer.len_chars(), self.doc().scroll, self.doc().id);
        if outcome == Outcome::Redraw {
            self.syntax_changed(now);
            if before.1 != after.1 || before.2 != after.2 {
                self.syntax_scrolled(now);
            }
        }
        if outcome == Outcome::Redraw {
            // The gutter widens as lines are added, and later milestones lay out
            // far more than this; anything that redraws may have moved things.
            self.relayout();
        }
        outcome
    }

    fn dispatch(&mut self, event: Event, now: Instant) -> Outcome {
        match event {
            Event::Key(key) => self.handle_key(key, now),
            Event::Mouse(mouse) => self
                .handle_mouse(mouse, now)
                .and(self.hover_pointer(mouse, now))
                .and(self.card_follow_pointer(mouse.column, mouse.row)),
            Event::Paste(text) if self.prompt.is_some() => {
                self.prompt_paste(&text);
                Outcome::Redraw
            }
            Event::Paste(text) if self.finder.is_some() => {
                self.palette_paste(&text);
                Outcome::Redraw
            }
            Event::Paste(text) => {
                // A paste is not the second half of a chord.
                self.chords.cancel();
                self.acknowledge();
                self.focus = Focus::Editor;
                self.doc_mut().buffer.insert(&pasted_line_endings(&text));
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Resize(width, height) => {
                self.set_viewport(Rect::new(0, 0, width, height));
                self.follow_caret();
                Outcome::Redraw
            }
            Event::Signal(nun_ui::Signal::Suspend) => Outcome::Suspend,
            Event::Signal(nun_ui::Signal::Continue | nun_ui::Signal::Resize) => Outcome::Redraw,
            Event::Signal(nun_ui::Signal::Terminate | nun_ui::Signal::Hangup) | Event::Closed => {
                Outcome::Quit
            }
            Event::Files { dir, error } => self.files_changed(&dir, error),
            Event::Workspace(done) => self.job_done(done),
            Event::Syntax(reply) => self.syntax_reply(reply),
            Event::Found(found) => self.search_found(found),
            Event::Lsp(event) => self.lsp_event(event),
            Event::Focus(true) => Outcome::Continue,
            // The pointer may be anywhere by the time focus comes back.
            Event::Focus(false) => {
                self.end_drag();
                if self.hover.clear(now).is_none() { Outcome::Continue } else { Outcome::Redraw }
            }
        }
    }

    fn handle_key(&mut self, event: KeyEvent, now: Instant) -> Outcome {
        // With the Kitty protocol negotiated the terminal reports releases and
        // repeats too. Acting on all three would type every character twice.
        if event.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }
        self.end_drag();
        if self.prompt.is_some() {
            return self.prompt_key(&event);
        }
        if self.finder.is_some() {
            return self.palette_key(&event);
        }
        if let Some(outcome) = self.card_key(&event) {
            return outcome;
        }
        if self.menu.take().is_some() && event.code == KeyCode::Esc {
            return Outcome::Redraw;
        }
        if let Some(outcome) = self.completion_key(&event) {
            return outcome;
        }
        let Some(key) = to_key(&event) else { return Outcome::Continue };

        let mut outcome = Outcome::Continue;
        for resolved in self.chords.feed(&self.keymap, key, now) {
            outcome = outcome.and(self.resolved(resolved, Some(&event)));
        }
        outcome
    }

    /// Act on what a keystroke, or a chord timing out, resolved to.
    ///
    /// `event` is the keystroke itself, when there is one: an unbound single
    /// key is always the one just pressed, and it is typed from the event as
    /// the terminal reported it, so layouts, Caps Lock and Option-composed
    /// characters come through untouched.
    fn resolved(&mut self, resolved: Resolved<Command>, event: Option<&KeyEvent>) -> Outcome {
        match resolved {
            // A panel with the keyboard has first say over a key bound to
            // something it cannot mean: Alt+Up moves the tree's selection
            // there, and Ctrl+D in the query adds nothing to a document the
            // person is not looking at.
            Resolved::Command(command) if command.acts_on_text() && self.focus != Focus::Editor => {
                let Some(event) = event else { return Outcome::Continue };
                self.acknowledge();
                // Only a key bound on its own is the panel's to have. The last
                // key of a chord — the Right of Ctrl+K Right — is not a Right
                // the person pressed at the tree.
                let alone =
                    to_key(event).is_some_and(|key| self.keymap.get(&[key]) == Some(&command));
                if !alone {
                    return Outcome::Continue;
                }
                match self.focus {
                    Focus::Search => self.search_key(event, Instant::now()),
                    Focus::Sidebar => self.sidebar_key(event),
                    Focus::EditPreview => self.edit_preview_key(event),
                    Focus::Editor => Outcome::Continue,
                }
            }
            Resolved::Command(command) => self.run(command),
            // The status line shows the chord so far.
            Resolved::Pending => Outcome::Redraw,
            Resolved::Unbound(keys) if keys.len() == 1 => match event {
                // The query is a text field: everything unbound belongs to it,
                // including the characters that would otherwise be typed into
                // the document behind it.
                Some(event) if self.focus == Focus::Search => {
                    self.acknowledge();
                    self.search_key(event, Instant::now())
                }
                Some(event) if self.focus == Focus::EditPreview => {
                    self.acknowledge();
                    self.edit_preview_key(event)
                }
                Some(event) if self.focus == Focus::Sidebar => {
                    self.acknowledge();
                    match self.sidebar_key(event) {
                        // The tree had no use for it: it is text, so the text
                        // takes the keyboard and types it rather than
                        // swallowing the keystroke.
                        Outcome::Continue => {
                            self.focus = Focus::Editor;
                            self.edit(event)
                        }
                        outcome => outcome,
                    }
                }
                Some(event) => self.edit(event),
                None => Outcome::Continue,
            },
            Resolved::Unbound(keys) => {
                self.acknowledge();
                self.message = Some(format!("{} is not bound to anything", Sequence(&keys)));
                Outcome::Redraw
            }
        }
    }

    /// Run a command.
    ///
    /// The layout is rebuilt afterwards: a command can open the sidebar, put a
    /// question in the status line or leave a toast, and the hit regions have
    /// to match what will be drawn.
    pub fn run(&mut self, command: Command) -> Outcome {
        let outcome = self.run_command(command);
        if outcome == Outcome::Redraw {
            self.relayout();
        }
        outcome
    }

    fn run_command(&mut self, command: Command) -> Outcome {
        // Anything but a second quit clears a pending quit confirmation.
        if command != Command::Quit {
            self.quit_confirmed = false;
        }
        self.acknowledge();

        let sidebar = self.focus == Focus::Sidebar;
        match command {
            Command::Quit => return self.request_quit(),
            Command::Save => return self.save(),
            // Undo means the thing that has the keyboard: with the tree
            // focused, the last file operation.
            Command::Undo if sidebar => return self.undo_file_op(),
            Command::Redo if sidebar => return self.redo_file_op(),
            Command::Undo => {
                self.doc_mut().buffer.undo();
            }
            Command::Redo => {
                self.doc_mut().buffer.redo();
            }
            Command::SelectAll => self.doc_mut().buffer.select_all(),
            Command::ToggleSidebar => return self.toggle_sidebar(),
            Command::NewFile => return self.ask_new(false),
            Command::NewFolder => return self.ask_new(true),
            Command::Rename => return self.ask_rename(),
            Command::Delete => return self.delete_selected(),
            Command::ToggleIgnored => return self.toggle_ignored(),
            Command::CloseTab => {
                let active = self.panes.focused().active;
                return self.close_tab(self.panes.focus(), active);
            }
            Command::SplitBeside => return self.split_pane(nun_ui::Dir::Beside),
            Command::SplitBelow => return self.split_pane(nun_ui::Dir::Below),
            Command::ClosePane => return self.close_pane(self.panes.focus()),
            Command::NextPane => return self.focus_next_pane(),
            Command::Palette => return self.open_palette(""),
            Command::Commands => return self.open_palette(">"),
            Command::SearchProject => return self.open_search(),
            Command::SearchRegex => return self.toggle_search(nun_ui::SearchButton::Regex),
            Command::SearchCase => return self.toggle_search(nun_ui::SearchButton::Case),
            Command::SearchWord => return self.toggle_search(nun_ui::SearchButton::Word),
            Command::SearchIgnored => return self.toggle_search(nun_ui::SearchButton::Ignored),
            Command::AddCaretAbove => self.doc_mut().buffer.add_caret_vertically(true),
            Command::AddCaretBelow => self.doc_mut().buffer.add_caret_vertically(false),
            Command::AddNextOccurrence => {
                if !self.doc_mut().buffer.add_next_occurrence() {
                    self.message = Some("No other occurrence of that.".into());
                }
            }
            Command::AddAllOccurrences => match self.doc_mut().buffer.add_all_occurrences() {
                nun_core::AllOccurrences::Selected(_) => {}
                nun_core::AllOccurrences::Nothing => {
                    self.message = Some("Nothing to select every occurrence of.".into());
                }
                nun_core::AllOccurrences::TooMany { limit } => {
                    self.message = Some(format!(
                        "More than {limit} occurrences; left the selection as it was."
                    ));
                }
            },
            // The view stays where it is: the selection grows around what is
            // being looked at, and following its far end would scroll away.
            Command::Fold => return self.fold_here(),
            Command::Unfold => return self.unfold_here(),
            Command::FoldAll => return self.fold_all(),
            Command::UnfoldAll => return self.unfold_all(),
            Command::GrowSelection => return self.grow_selection(),
            Command::RestartLanguageServer => return self.lsp_restart(),
            Command::FormatDocument => return self.format_document(),
            Command::GoToDefinition => return self.go_to_definition(navigation::Open::Here),
            Command::OpenDefinitionBeside => {
                return self.go_to_definition(navigation::Open::Beside);
            }
            Command::FindReferences => return self.find_references(),
            Command::GoBack => return self.jump_back(),
            Command::GoForward => return self.jump_forward(),
            Command::NextReference => return self.step_reference(1),
            Command::PreviousReference => return self.step_reference(-1),
            Command::NextDiagnostic => return self.step_diagnostic(true),
            Command::PreviousDiagnostic => return self.step_diagnostic(false),
            Command::Complete => return self.complete_here(),
            Command::RenameSymbol => return self.start_rename(),
            Command::UndoRename => return self.undo_edit(),
            Command::ShowHover => return self.show_hover(),
            Command::CodeActions => return self.code_actions_here(),
            Command::ShrinkSelection => return self.shrink_selection(),
            Command::SplitIntoLines => {
                if !self.doc_mut().buffer.split_into_lines() {
                    self.message = Some("The selection is already on one line.".into());
                }
            }

            Command::NextTab => return self.step_tab(1),
            Command::PreviousTab => return self.step_tab(-1),
        }
        self.follow_caret();
        Outcome::Redraw
    }

    /// An editing key: typing, moving, deleting. Not rebindable.
    fn edit(&mut self, key: &KeyEvent) -> Outcome {
        self.quit_confirmed = false;
        self.acknowledge();

        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            // Not a bound command: a binding for this would have to be Escape,
            // and Escape belongs to the sidebar and the search panel, where it
            // means "leave". Here it steps down one level at a time: several
            // carets become the primary alone, selection and all, and a second
            // press drops that selection to a caret (the arm further down).
            // The mouse already does both — a plain click puts one caret
            // somewhere and takes the others away.
            KeyCode::Esc if self.doc().buffer.selections().len() > 1 => {
                let mut selections = self.doc().buffer.selections().clone();
                selections.collapse_to_primary();
                self.doc_mut().buffer.set_selections(selections);
            }
            KeyCode::Char(ch) if !control => {
                let mut text = [0u8; 4];
                self.doc_mut().buffer.insert(ch.encode_utf8(&mut text));
            }
            KeyCode::Enter => self.doc_mut().buffer.insert("\n"),
            KeyCode::Tab => self.doc_mut().buffer.insert("\t"),
            KeyCode::Backspace => self.doc_mut().buffer.delete_backward(),
            KeyCode::Delete => self.doc_mut().buffer.delete_forward(),

            KeyCode::Left => self.doc_mut().buffer.move_left(shift),
            KeyCode::Right => self.doc_mut().buffer.move_right(shift),
            KeyCode::Up => self.doc_mut().buffer.move_up(shift),
            KeyCode::Down => self.doc_mut().buffer.move_down(shift),
            KeyCode::Home => self.doc_mut().buffer.move_line_start(shift),
            KeyCode::End => self.doc_mut().buffer.move_line_end(shift),
            KeyCode::PageUp => {
                for _ in 0..self.text_height() {
                    self.doc_mut().buffer.move_up(shift);
                }
            }
            KeyCode::PageDown => {
                for _ in 0..self.text_height() {
                    self.doc_mut().buffer.move_down(shift);
                }
            }
            KeyCode::Esc => {
                let head = self.doc().buffer.selections().primary().head;
                self.doc_mut().buffer.set_selections(Selections::single(Range::caret(head)));
            }
            // An unbound Ctrl chord, or a key nun does not use. The message it
            // may have cleared still needs a redraw.
            _ => return Outcome::Redraw,
        }

        self.follow_caret();
        Outcome::Redraw
    }

    /// Mouse support here is deliberately minimal — click to place the caret and
    /// the wheel to scroll. The full gesture set is cairn `0015` to `0017`; this
    /// is enough that nothing shipped so far is keyboard-only.
    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent, now: Instant) -> Outcome {
        let hit = self.hits.at(mouse.column, mouse.row);

        // Hover follows every report, whatever else the event does, so leaving
        // a target by dragging out of it is still a leave.
        let crossing = self.hover.update(hit.filter(|hit| hit.hover).map(|hit| hit.target), now);
        let hovered = if crossing.is_none() { Outcome::Continue } else { Outcome::Redraw };
        let hovered = hovered.and(self.link_pointer(mouse, now));

        let target = hit.map(|hit| hit.target.pressed());
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_card) =>
            {
                self.card_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if self.finder.is_some() => {
                self.palette_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_completion) =>
            {
                self.completion_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if matches!(target, Some(Target::EditPreview(_))) =>
            {
                self.edit_preview_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_search) =>
            {
                self.search_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_references) =>
            {
                self.references_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_sidebar) =>
            {
                self.sidebar_scroll(mouse.kind == MouseEventKind::ScrollDown)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if target.is_some_and(Target::in_tabs) =>
            {
                let pane = self
                    .panes
                    .layout()
                    .pane_at(self.panes_area(), mouse.column, mouse.row)
                    .unwrap_or_else(|| self.panes.focus());
                self.tab_scroll_by(mouse.kind == MouseEventKind::ScrollDown, pane)
            }
            // A question or a menu on screen takes every button, not only the
            // left one: closing a tab behind a prompt would answer it about
            // the wrong tab.
            MouseEventKind::Down(MouseButton::Middle)
                if self.prompt.is_none() && self.menu.is_none() =>
            {
                match target {
                    Some(target) => self.tab_middle_click(target),
                    None => hovered,
                }
            }
            // By lines in view: a folded region is one line to scroll past.
            MouseEventKind::ScrollUp => {
                let scroll = self.doc().buffer.hidden().step(self.doc().scroll, -3);
                self.doc_mut().scroll = scroll;
                Outcome::Redraw
            }
            MouseEventKind::ScrollDown => {
                let scroll = self.doc().buffer.hidden().step(self.doc().scroll, 3);
                self.doc_mut().scroll = scroll;
                Outcome::Redraw
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Reaching for the mouse abandons a half-typed chord, so the
                // next key is typed rather than taken as its second half.
                self.chords.cancel();
                self.quit_confirmed = false;
                let Some(target) = target else { return Outcome::Redraw };
                self.click(mouse, target, now)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.menu = None;
                match target {
                    Some(target) if target.in_sidebar() => self.sidebar_menu(mouse, target),
                    Some(Target::Text) if self.prompt.is_none() && self.finder.is_none() => {
                        self.chords.cancel();
                        self.text_menu(mouse)
                    }
                    _ => hovered,
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self
                .divider_drag_to(mouse.column, mouse.row)
                .or_else(|| self.tab_drag_to(mouse.column, mouse.row))
                .or_else(|| self.sidebar_drag(mouse.column, mouse.row))
                .unwrap_or_else(|| self.drag_to(mouse.column, mouse.row, now))
                .and(hovered),
            MouseEventKind::Up(MouseButton::Left) => self
                .divider_release()
                .or_else(|| self.tab_release())
                .or_else(|| self.sidebar_release())
                .unwrap_or_else(|| self.release(mouse))
                .and(hovered),
            _ => hovered,
        }
    }

    /// The left button went down on `target`.
    fn click(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        target: Target,
        now: Instant,
    ) -> Outcome {
        // A menu or a question on screen takes the click first.
        if let Some(menu) = self.menu.take() {
            if let Target::MenuItem(index) = target {
                return self.run(menu.commands[index]);
            }
            return Outcome::Redraw;
        }
        if let Target::PromptButton(index) = target
            && let Some(answer) = self.prompt.as_ref().map(|prompt| prompt.buttons[index].1)
        {
            return self.answer(answer);
        }
        if self.prompt.is_some() {
            // Clicking elsewhere does not answer the question; it stays up.
            return Outcome::Continue;
        }
        if let Some(outcome) = self.card_click(target) {
            return outcome;
        }

        if self.finder.is_some() {
            let split = mouse.modifiers.contains(KeyModifiers::ALT)
                || mouse.kind == MouseEventKind::Down(MouseButton::Middle);
            return match target {
                // Alt-click, like Alt+Enter, opens the file in a split.
                Target::PaletteRow(index) => self.pick_row(index, split),
                Target::Palette => Outcome::Continue,
                // A click anywhere else puts the palette away, as with any
                // overlay.
                _ => self.close_palette(),
            };
        }

        match target {
            target if target.in_completion() => {
                self.completion_click(target, mouse.modifiers.contains(KeyModifiers::SHIFT))
            }
            Target::StatusSearch => {
                self.acknowledge();
                self.open_palette("")
            }
            // The same button both ways: after an undo it offers the redo.
            Target::StatusClose => {
                self.acknowledge();
                self.close_tab(self.panes.focus(), self.panes.focused().active)
            }
            Target::StatusLsp => {
                self.acknowledge();
                self.lsp_restart()
            }
            target if target.in_diagnostics() => {
                self.acknowledge();
                self.diagnostics_press(target, mouse.row)
            }
            Target::StatusUndo if self.edit_undo_offered() => self.undo_edit(),
            Target::StatusUndo if self.last_undone => self.redo_file_op(),
            Target::StatusUndo => self.undo_file_op(),
            Target::StatusFiles => {
                self.acknowledge();
                self.toggle_sidebar()
            }
            Target::EditPreview(spot) => {
                self.acknowledge();
                self.edit_preview_press(spot)
            }
            target if target.in_search() => {
                self.acknowledge();
                self.search_press(target, mouse.column, now)
            }
            target if target.in_references() => {
                self.acknowledge();
                self.references_press(target)
            }
            target if target.in_sidebar() => {
                self.acknowledge();
                self.sidebar_press(mouse, target)
            }
            target if target.in_tabs() => {
                self.acknowledge();
                self.tab_press(mouse, target)
            }
            Target::Divider(index) => {
                self.acknowledge();
                // A double-click on a divider evens the two sides.
                let double = self.clicks.press(mouse.column, mouse.row, now) > 1;
                self.divider_press(index, double)
            }
            _ => {
                self.acknowledge();
                self.focus = Focus::Editor;
                // A click in a pane's text works that pane, so it takes the
                // keyboard before the caret is placed in it.
                if let Some(pane) =
                    self.panes.layout().pane_at(self.panes_area(), mouse.column, mouse.row)
                {
                    self.panes.set_focus(pane);
                }
                self.press(mouse, target, now)
            }
        }
    }

    /// Which char index a screen cell in the text region corresponds to.
    fn position_at(&self, column: u16, row: u16) -> Option<usize> {
        // The view measures from the left edge of the gutter, not the text.
        let (area, _) = self.areas();
        EditorView::new(&self.doc().buffer, &self.palette)
            .scrolled_to(self.doc().scroll)
            .position_at(area, column, row)
    }

    /// Scroll the minimum distance needed to keep the caret on screen.
    fn follow_caret(&mut self) {
        let height = self.text_height();
        if height == 0 {
            return;
        }
        let buffer = &self.doc().buffer;
        let line = buffer.line_of(buffer.selections().primary().head);
        // Counted in lines in view, since that is what rows show; and a view
        // scrolled into what has since been folded stands on its header.
        let hidden = buffer.hidden();
        let scroll = hidden.in_view(self.doc().scroll);
        self.doc_mut().scroll = if line < scroll {
            line
        } else if hidden.rows_between(scroll, line) >= height {
            hidden.step(line, 1 - isize::try_from(height).unwrap_or(isize::MAX))
        } else {
            scroll
        };
    }

    /// The line drawn on row `row` of the focused text, counted from its top.
    fn line_at_row(&self, row: usize) -> Option<usize> {
        EditorView::new(&self.doc().buffer, &self.palette)
            .scrolled_to(self.doc().scroll)
            .line_at_row(row)
    }

    fn request_quit(&mut self) -> Outcome {
        self.save_before_quitting();
        let unsaved = self.docs.iter().filter(|doc| doc.buffer.is_modified()).count();
        if unsaved > 0 && !self.quit_confirmed {
            self.quit_confirmed = true;
            let save = self.binding_for(Command::Save);
            let quit = self.binding_for(Command::Quit);
            // Every unsaved tab is counted, so quitting never throws away a
            // file that is not the one on screen without saying so.
            let what = if unsaved == 1 {
                "Unsaved changes".to_string()
            } else {
                format!("{unsaved} files have unsaved changes")
            };
            self.message = Some(format!("{what}. {save} to save, or {quit} again to discard."));
            return Outcome::Redraw;
        }
        Outcome::Quit
    }

    fn save(&mut self) -> Outcome {
        let id = self.doc().id;
        // Formatting first, when the file's language asks for it, puts the
        // save off until the server has answered.
        if !self.format_then_save(id, false) {
            self.message = Some(self.write(id).unwrap_or_else(|failed| failed));
        }
        Outcome::Redraw
    }

    /// Write a document to disk, and say how that went.
    fn write(&mut self, id: panes::DocId) -> Result<String, String> {
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return Err("That file is no longer open.".into());
        };
        if document.buffer.is_lossy() {
            return Err(
                "Refusing to save: this file was not valid UTF-8 and would be damaged.".into()
            );
        }
        match document.buffer.save() {
            Ok(()) => {
                let saved = format!("Saved {}", display_path(document.buffer.path()));
                self.lsp_saved(id);
                Ok(saved)
            }
            Err(SaveError::NoPath) => Err("No path to save to.".into()),
            Err(SaveError::ChangedOnDisk { path }) => Err(format!(
                "{} changed on disk. Reopen it to see what changed.",
                display_path(Some(&path))
            )),
            Err(error) => Err(format!("Could not save: {error}")),
        }
    }

    /// A status-line button's glyph, with a cell of padding either side.
    fn button_glyph(&self, glyph: Glyph) -> String {
        format!(" {} ", self.palette.glyph(glyph))
    }

    /// What the button beside a file-operation message offers: undoing it, or
    /// putting back what was just undone.
    const fn undo_label(&self) -> &'static str {
        if self.last_undone { " Redo " } else { " Undo " }
    }

    /// How to ask for `command` from the keyboard, as the status line says it.
    fn binding_for(&self, command: Command) -> String {
        self.keymap
            .sequences_for(&command)
            .first()
            .map_or_else(|| command.title().to_string(), |keys| Sequence(keys).to_string())
    }

    /// The status line's text, left and right halves.
    #[must_use]
    pub fn status(&self) -> (String, String) {
        let pending = self.chords.pending();
        let chord = (!pending.is_empty())
            .then(|| format!("{} {}", Sequence(pending), self.palette.glyph(Glyph::Ellipsis)));
        let left =
            chord.or_else(|| self.shown_message().map(str::to_string)).unwrap_or_else(|| {
                format!(
                    "{}{}",
                    display_path(self.doc().buffer.path()),
                    if self.doc().buffer.is_modified() {
                        format!(" {}", self.palette.glyph(Glyph::TabModified))
                    } else {
                        String::new()
                    }
                )
            });

        let caret = self.doc().buffer.selections().primary().head;
        let line = self.doc().buffer.line_of(caret);
        let selections = self.doc().buffer.selections().len();
        let carets = if selections > 1 { format!("{selections} carets  ") } else { String::new() };

        let language =
            Self::language_of(self.doc()).map_or_else(String::new, |name| format!("{name}  "));
        let right = format!(
            "{carets}{language}Ln {}, Col {}  {}",
            line + 1,
            self.doc().buffer.column_of(caret) + 1,
            match self.doc().buffer.line_ending() {
                nun_core::LineEnding::Lf => "LF",
                nun_core::LineEnding::Crlf => "CRLF",
            }
        );
        (left, right)
    }

    /// Draw the editor, the sidebar, the status line, and any menu.
    pub fn render(&self, area: Rect, cells: &mut Cells) {
        // Drawn into whatever rectangle the caller gives, which is the
        // viewport in the editor and a fixed grid in the tests. The sidebar
        // takes columns from the left and the tab strip a row from the top,
        // the same way the layout pass reckons them.
        let status =
            Rect { y: area.bottom().saturating_sub(1), height: area.height.min(1), ..area };
        self.render_panes(area, cells);
        self.render_link(cells);
        self.render_sidebar(cells);
        self.render_search(cells);
        self.render_references(cells);
        self.render_card(cells);
        self.render_completion(cells);
        self.render_edit_preview(cells);

        if status.height > 0 {
            self.render_status(status, cells);
        }
        // A prompt drawn beside what it asks about goes over the text, with
        // the status line left as it was underneath.
        if let Some(prompt) = &self.prompt {
            let area = self.prompt_area(status);
            if area != status {
                self.render_prompt(prompt, area, cells);
            }
        }

        self.render_palette(cells);
        if let Some(menu) = &self.menu {
            let hovered = match self.hover.current() {
                Some(Target::MenuItem(index)) => Some(index),
                _ => None,
            };
            Menu::new(&menu.items, &self.palette).hovered(hovered).render(menu.area, cells);
        }
    }

    fn render_sidebar(&self, cells: &mut Cells) {
        let (Some(sidebar), Some(area), Some(tree)) =
            (self.sidebar.as_ref(), self.sidebar_area(), self.tree_area())
        else {
            return;
        };
        // The divider belongs to the sidebar rather than to either view, so it
        // is drawn whichever one is in it.
        self.render_sidebar_edge(area, cells);
        if self.sidebar_view != SidebarView::Files {
            return;
        }
        let (hovered, button) = match self.hover.current() {
            Some(Target::TreeRow(row)) => (Some(row), None),
            Some(Target::TreeButton(button)) => (None, Some(button)),
            _ => (None, None),
        };
        TreeView::new(&sidebar.title, sidebar.tree.rows(), &self.palette)
            .scrolled_to(sidebar.scroll)
            .selected(sidebar.selected)
            .hovered(hovered)
            .hovered_button(button)
            .drop_target(sidebar.drag.and_then(|drag| drag.target))
            .showing_ignored(sidebar.tree.show_ignored())
            .focused(self.focus == Focus::Sidebar)
            .render(tree, cells);
    }

    /// The divider, which is also the handle for resizing.
    fn render_sidebar_edge(&self, area: Rect, cells: &mut Cells) {
        let resizing = self.sidebar.as_ref().is_some_and(|sidebar| sidebar.resizing);
        let edge = area.right().saturating_sub(1);
        let style = if resizing || self.hover.current() == Some(Target::SidebarEdge) {
            self.palette.fg(Role::LineStrong)
        } else {
            self.palette.fg(Role::Line)
        };
        for y in area.top()..area.bottom() {
            cells[(edge, y)].set_symbol(self.palette.glyph(Glyph::RuleVertical)).set_style(style);
        }
    }

    fn render_status(&self, area: Rect, cells: &mut Cells) {
        if let Some(prompt) = &self.prompt
            && self.prompt_area(area) == area
        {
            self.render_prompt(prompt, area, cells);
            return;
        }
        let style = if self.shown_message().is_some() || !self.chords.pending().is_empty() {
            self.palette.on(Role::Accent, Role::OnAccent)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };

        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(style);
        }

        let parts = self.status_parts(area);
        let button = |target: Target| {
            if self.hover.current() == Some(target) {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Overlay, Role::Text)
            }
        };
        if let Some(files) = parts.files {
            let glyph = if self.sidebar_area().is_some() {
                Glyph::SidebarShown
            } else {
                Glyph::SidebarHidden
            };
            write_at(cells, files, files.x, &self.button_glyph(glyph), button(Target::StatusFiles));
        }
        if let Some(search) = parts.search {
            let glyph = self.button_glyph(Glyph::SearchIcon);
            write_at(cells, search, search.x, &glyph, button(Target::StatusSearch));
        }

        let (left, right) = self.status();
        write_at(cells, area, parts.text, &left, style);
        if let Some(undo) = parts.undo {
            write_at(cells, undo, undo.x, self.undo_label(), button(Target::StatusUndo));
        }
        if let Some(close) = parts.close {
            let glyph = self.button_glyph(Glyph::TabClose);
            write_at(cells, close, close.x, &glyph, button(Target::StatusClose));
        }
        if let Some(lsp) = parts.lsp {
            self.render_lsp(lsp, cells);
        }
        if let Some(problems) = parts.problems {
            self.render_problems(problems, cells);
        }

        let width = u16::try_from(text_width(&right)).unwrap_or(0);
        // Dropped entirely rather than overlapping when the two halves would
        // collide on a narrow terminal.
        let right_edge = parts.close.map_or(area.right(), |close| close.x);
        let used = parts.undo.map_or_else(
            || parts.text + u16::try_from(text_width(&left)).unwrap_or(0),
            Rect::right,
        );
        if let Some(start) = right_edge.checked_sub(width + 1)
            && start > used
        {
            write_at(cells, area, start, &right, style);
        }
    }

    fn render_prompt(&self, prompt: &prompt::Prompt, area: Rect, cells: &mut Cells) {
        let style = self.palette.on(Role::Accent, Role::OnAccent);
        for x in area.left()..area.right() {
            cells[(x, area.y)].set_char(' ').set_style(style);
        }
        let text = prompt.text();
        write_at(cells, area, area.x, &text, style);
        // The field's caret, just after what has been typed.
        if prompt.field.is_some() {
            let x = area.x + u16::try_from(text_width(&text)).unwrap_or(0);
            if x < area.right() {
                cells[(x, area.y)]
                    .set_char(' ')
                    .set_style(self.palette.on(Role::Ground, Role::Text));
            }
        }
        for (index, (label, _)) in prompt.buttons.iter().enumerate() {
            let Some(button) =
                prompt.button_areas(area).get(index).copied().filter(|b| b.width > 0)
            else {
                continue;
            };
            let style = if self.hover.current() == Some(Target::PromptButton(index)) {
                self.palette.on(Role::Ground, Role::Accent)
            } else {
                self.palette.on(Role::Overlay, Role::Text)
            };
            write_at(cells, button, button.x, &format!(" {label} "), style);
        }
    }
}

/// Where the status line's parts go.
#[derive(Debug, Clone, Copy)]
struct StatusParts {
    /// The file-tree toggle, when a folder is open.
    files: Option<Rect>,
    /// The button that opens the palette.
    search: Option<Rect>,
    /// The Undo button after a file operation.
    undo: Option<Rect>,
    /// The cross that closes the open file, when there is no tab strip to
    /// close it from.
    close: Option<Rect>,
    /// The name of the file's language server, which restarts it.
    lsp: Option<Rect>,
    /// The diagnostic counts, which go to the next one.
    problems: Option<Rect>,
    /// Where the text starts.
    text: u16,
}

/// A terminal keystroke as a key the keymap understands.
///
/// `None` for keys nun has no use for: media keys, lone modifiers, Caps Lock.
fn to_key(event: &KeyEvent) -> Option<Key> {
    let code = match event.code {
        KeyCode::Char(ch) => Code::Char(ch),
        KeyCode::F(n) => Code::F(n),
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab | KeyCode::BackTab => Code::Tab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Insert => Code::Insert,
        KeyCode::Esc => Code::Esc,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        _ => return None,
    };

    let mut mods = Mods::NONE;
    for (flag, modifier) in [
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::ALT, Mods::ALT),
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::SUPER, Mods::CMD),
    ] {
        if event.modifiers.contains(flag) {
            mods |= modifier;
        }
    }
    if event.code == KeyCode::BackTab {
        mods |= Mods::SHIFT;
    }
    Some(Key::new(code, mods))
}

/// ratatui's rectangle as nun-input's.
const fn cells(area: Rect) -> nun_input::Rect {
    nun_input::Rect::new(area.x, area.y, area.width, area.height)
}

/// Write `text` from `start`, a grapheme at a time, stopping at the edge of
/// `area` rather than splitting a wide character across it. It takes
/// [`text_width`] cells, which is what anything placed after it measures it
/// with.
fn write_at(cells: &mut Cells, area: Rect, start: u16, text: &str, style: ratatui::style::Style) {
    let mut x = start;
    for (cluster, width) in nun_ui::clusters(text) {
        let Ok(width) = u16::try_from(width) else { break };
        let Some(end) = x.checked_add(width) else { break };
        if end > area.right() {
            break;
        }
        cells[(x, area.y)].set_symbol(cluster).set_style(style);
        for covered in x + 1..end {
            cells[(covered, area.y)].set_symbol(" ").set_style(style);
        }
        x = end;
    }
}

/// Whether two paths name the same file, however each is spelled: relative
/// from the command line, in full from the tree, or through a symlink. A
/// file open under one spelling and looked for under another is still open.
fn same_file(one: &std::path::Path, other: &std::path::Path) -> bool {
    one == other
        || matches!(
            (std::fs::canonicalize(one), std::fs::canonicalize(other)),
            (Ok(one), Ok(other)) if one == other
        )
}

fn display_path(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || "[no name]".to_string(),
        |path| {
            // Show it relative to where nun was started, which is almost always
            // shorter and is what the user typed.
            std::env::current_dir()
                .ok()
                .and_then(|cwd| path.strip_prefix(cwd).ok())
                .unwrap_or(path)
                .display()
                .to_string()
        },
    )
}

/// Pasted text with every line ending made `\n`, the only one the buffer holds.
///
/// Terminals are not consistent about what a pasted newline is: xterm and
/// others send `\r`, and a paste copied from a CRLF source carries `\r\n`.
/// Left alone, either puts a carriage return in the buffer, which draws a
/// pasted block as one line and moves every language-server position after it,
/// since the protocol ends a line at a lone `\r` and nun does not.
fn pasted_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n").into()
    } else {
        text.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nun_theme::{Probe, derive};
    use ratatui::style::Color;

    fn app_with(buffer: Buffer) -> App {
        App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        )
    }

    fn app_over(text: &str) -> App {
        let mut app = app_with(Buffer::from_text(text));
        app.set_viewport(Rect::new(0, 0, 40, 6));
        app
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn chord(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn text_of(app: &App) -> String {
        app.buffer().text().to_string()
    }

    #[test]
    fn typing_inserts_and_the_arrows_move() {
        let mut app = app_over("");
        for ch in "hello".chars() {
            app.handle(key(KeyCode::Char(ch)));
        }
        assert_eq!(text_of(&app), "hello");

        app.handle(key(KeyCode::Left));
        app.handle(key(KeyCode::Char('X')));
        assert_eq!(text_of(&app), "hellXo");
    }

    #[test]
    fn enter_and_backspace_do_the_obvious_thing() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Enter));
        app.handle(key(KeyCode::Char('b')));
        assert_eq!(text_of(&app), "a\nb");

        app.handle(key(KeyCode::Backspace));
        app.handle(key(KeyCode::Backspace));
        assert_eq!(text_of(&app), "a");
    }

    #[test]
    fn a_key_release_is_ignored_rather_than_typed_twice() {
        // With the Kitty protocol negotiated the terminal reports releases too.
        let mut app = app_over("");
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;

        app.handle(key(KeyCode::Char('x')));
        app.handle(Event::Key(release));

        assert_eq!(text_of(&app), "x", "a press and its release must type one character");
    }

    #[test]
    fn shift_and_an_arrow_extend_the_selection() {
        let mut app = app_over("hello");
        app.handle(chord(KeyCode::Right, KeyModifiers::SHIFT));
        app.handle(chord(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(app.buffer().selections().primary().len(), 2);
    }

    #[test]
    fn undo_and_redo_are_bound_the_way_people_expect() {
        let mut app = app_over("");
        for ch in "abc".chars() {
            app.handle(key(KeyCode::Char(ch)));
        }
        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "");

        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        assert_eq!(text_of(&app), "abc");

        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        app.handle(chord(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "abc", "Ctrl+Y redoes too");
    }

    fn app_with_bindings(text: &str, user: &[(&str, &str)]) -> App {
        let user = user.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        let (keymap, problems) = crate::commands::keymap(crate::commands::KeySet::Full, &user);
        assert!(problems.is_empty(), "{problems:?}");
        let mut app =
            App::new(Buffer::from_text(text), Palette::new(derive(&Probe::builtin_dark())), keymap);
        app.set_viewport(Rect::new(0, 0, 40, 6));
        app
    }

    fn ctrl(ch: char) -> Event {
        chord(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_chord_shows_itself_while_it_waits_and_runs_when_finished() {
        let mut app = app_with_bindings("abc", &[("ctrl+k ctrl+a", "edit.select_all")]);
        let now = Instant::now();

        assert_eq!(app.handle_at(ctrl('k'), now), Outcome::Redraw);
        assert!(app.status().0.starts_with("Ctrl+K"), "{:?}", app.status());
        assert!(app.deadline().is_some(), "waiting for the second key");

        app.handle_at(ctrl('a'), now);
        assert_eq!(app.buffer().selections().primary().len(), 3);
        assert_eq!(app.deadline(), None);
        assert!(!app.status().0.starts_with("Ctrl+K"));
    }

    #[test]
    fn a_chord_nobody_finishes_times_out_and_says_so() {
        let mut app = app_with_bindings("abc", &[("ctrl+k ctrl+a", "edit.select_all")]);
        let now = Instant::now();
        app.handle_at(ctrl('k'), now);

        let deadline = app.deadline().unwrap();
        assert_eq!(app.tick(deadline), Outcome::Redraw);
        assert_eq!(app.message(), Some("Ctrl+K is not bound to anything"));
        assert_eq!(app.deadline(), None);
        assert!(app.buffer().selections().primary().is_empty(), "nothing ran");
    }

    #[test]
    fn a_chord_times_out_to_its_prefix_binding() {
        let mut app = app_with_bindings(
            "abc",
            &[("ctrl+k", "edit.select_all"), ("ctrl+k ctrl+z", "edit.undo")],
        );
        let now = Instant::now();
        app.handle_at(ctrl('k'), now);
        let deadline = app.deadline().unwrap();
        app.tick(deadline);
        assert_eq!(app.buffer().selections().primary().len(), 3, "the prefix binding ran");
    }

    #[test]
    fn typing_after_a_chord_prefix_does_not_type() {
        let mut app = app_with_bindings("", &[("ctrl+k ctrl+a", "edit.select_all")]);
        app.handle(ctrl('k'));
        app.handle(key(KeyCode::Char('x')));
        assert_eq!(text_of(&app), "", "the x finished (and broke) the chord");
        assert_eq!(app.message(), Some("Ctrl+K X is not bound to anything"));
    }

    #[test]
    fn a_click_abandons_a_half_typed_chord() {
        let mut app = app_with_bindings("", &[("ctrl+k ctrl+a", "edit.select_all")]);
        app.handle(ctrl('k'));
        app.handle(click(3, 0));
        app.handle(key(KeyCode::Char('x')));
        assert_eq!(text_of(&app), "x", "typed, not taken as the chord's second key");
        assert_eq!(app.deadline(), None);
    }

    #[test]
    fn a_rebound_key_does_the_new_thing() {
        let mut app = app_with_bindings("abc", &[("ctrl+s", "edit.select_all")]);
        app.handle(ctrl('s'));
        assert_eq!(app.buffer().selections().primary().len(), 3);
    }

    #[test]
    fn cmd_bindings_work_when_the_terminal_reports_cmd() {
        let mut app = app_over("abc");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(app.buffer().selections().primary().len(), 3);
    }

    #[test]
    fn notices_are_shown_one_at_a_time() {
        let mut app = app_over("");
        app.warn("first");
        app.warn("second");
        assert_eq!(app.message(), Some("first"));
        app.handle(key(KeyCode::Right));
        assert_eq!(app.message(), Some("second"));
        app.handle(key(KeyCode::Right));
        assert_eq!(app.message(), None);
    }

    #[test]
    fn a_hint_is_seen_once_a_click_dismisses_it() {
        let mut app = app_over("abc");
        app.set_viewport(Rect::new(0, 0, 40, 10));
        app.warn("first");
        app.hint("once");
        app.handle(click(5, 1));
        assert_eq!(app.message(), Some("once"));
        assert!(!app.hint_seen(), "only the notice in front of it was dismissed");
        app.handle(click(5, 1));
        assert_eq!(app.message(), None);
        assert!(app.hint_seen());
    }

    #[test]
    fn the_quit_prompt_names_the_keys_actually_bound() {
        let mut app = app_with_bindings("", &[]);
        app.handle(key(KeyCode::Char('x')));
        app.handle(ctrl('q'));
        let message = app.message().unwrap();
        assert!(message.contains("Ctrl+S to save"), "{message}");
        assert!(message.contains("Ctrl+Q again"), "{message}");
    }

    #[test]
    fn select_all_then_type_replaces_everything() {
        let mut app = app_over("old content");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Char('n')));
        assert_eq!(text_of(&app), "n");
    }

    #[test]
    fn escape_collapses_a_selection_to_a_caret() {
        let mut app = app_over("hello");
        app.handle(chord(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Esc));
        assert!(app.buffer().selections().primary().is_empty());
    }

    #[test]
    fn a_paste_arrives_as_one_undoable_unit() {
        let mut app = app_over("");
        app.handle(Event::Paste("pasted text".into()));
        assert_eq!(text_of(&app), "pasted text");
        app.handle(chord(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(text_of(&app), "", "a paste undoes in one step, not per character");
    }

    #[test]
    fn a_paste_with_carriage_returns_lands_as_lines() {
        let mut app = app_over("");
        app.handle(Event::Paste("one\rtwo\r".into()));
        assert_eq!(text_of(&app), "one\ntwo\n", "a lone CR is a newline");

        let mut app = app_over("");
        app.handle(Event::Paste("one\r\ntwo\r\n".into()));
        assert_eq!(text_of(&app), "one\ntwo\n", "CRLF is one newline, not two");

        let mut app = app_over("");
        app.handle(Event::Paste("a\r\r\nb\n\rc".into()));
        assert_eq!(text_of(&app), "a\n\nb\n\nc", "each ending counts once, whatever the mix");
    }

    #[test]
    fn a_crlf_file_saves_a_pasted_crlf_block_without_doubling() {
        let (buffer, _) = Buffer::from_bytes(b"x\r\n");
        let mut app = app_with(buffer);
        app.handle(Event::Paste("one\r\ntwo\r\n".into()));
        assert_eq!(app.buffer().to_bytes(), b"one\r\ntwo\r\nx\r\n");
    }

    // ── quitting ────────────────────────────────────────────────────────────

    #[test]
    fn quitting_a_clean_buffer_is_immediate() {
        let mut app = app_over("untouched");
        assert_eq!(app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)), Outcome::Quit);
    }

    #[test]
    fn quitting_with_unsaved_changes_asks_first() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('x')));

        assert_eq!(app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)), Outcome::Redraw);
        assert!(app.message().unwrap().contains("Unsaved changes"));

        assert_eq!(
            app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Outcome::Quit,
            "asking twice means you meant it"
        );
    }

    #[test]
    fn typing_after_a_quit_prompt_cancels_it() {
        let mut app = app_over("");
        app.handle(key(KeyCode::Char('x')));
        app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL));
        app.handle(key(KeyCode::Char('y')));

        assert_eq!(
            app.handle(chord(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Outcome::Redraw,
            "the confirmation must not survive an intervening edit"
        );
    }

    #[test]
    fn a_lossy_file_refuses_to_save_rather_than_destroying_it() {
        let (buffer, _) = Buffer::from_bytes(&[0x68, 0xff]);
        let mut app = app_with(buffer);
        app.set_viewport(Rect::new(0, 0, 40, 6));

        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message().unwrap().contains("Refusing to save"));
    }

    #[test]
    fn saving_a_buffer_with_no_path_says_so() {
        let mut app = app_over("x");
        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(app.message(), Some("No path to save to."));
    }

    // ── scrolling ───────────────────────────────────────────────────────────

    #[test]
    fn the_view_follows_the_caret_down_and_back_up() {
        let mut app = app_over(&"line\n".repeat(40));
        assert_eq!(app.scroll(), 0);

        for _ in 0..10 {
            app.handle(key(KeyCode::Down));
        }
        assert_eq!(app.scroll(), 6, "scrolled the minimum needed to keep the caret visible");

        for _ in 0..10 {
            app.handle(key(KeyCode::Up));
        }
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn page_down_moves_by_the_visible_height() {
        let mut app = app_over(&"line\n".repeat(40));
        app.handle(key(KeyCode::PageDown));
        let line = app.buffer().line_of(app.buffer().selections().primary().head);
        assert_eq!(line, 5, "one screenful, less the status row");
    }

    #[test]
    fn a_resize_keeps_the_caret_on_screen() {
        let mut app = app_over(&"line\n".repeat(40));
        for _ in 0..20 {
            app.handle(key(KeyCode::Down));
        }
        app.handle(Event::Resize(40, 4));
        let line = app.buffer().line_of(app.buffer().selections().primary().head);
        assert!(line >= app.scroll() && line < app.scroll() + 3, "caret left the viewport");
    }

    // ── mouse ───────────────────────────────────────────────────────────────

    fn click(column: u16, row: u16) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn wheel(down: bool) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind: if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp },
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn clicking_places_the_caret() {
        let mut app = app_over("hello\nworld\n");
        // Gutter is one digit plus two columns of padding.
        app.handle(click(3 + 2, 1));
        assert_eq!(app.buffer().selections().primary().head, 8, "row 1, two chars in");
    }

    #[test]
    fn clicking_past_a_wide_character_lands_after_it_not_inside_it() {
        let mut app = app_over("日本語\n");
        app.handle(click(3 + 3, 0));
        assert_eq!(app.buffer().selections().primary().head, 1, "after the first wide cluster");
    }

    #[test]
    fn a_click_past_column_223_lands_where_it_was_made() {
        // The legacy mouse encoding tops out at column 223. SGR does not, and
        // nothing between the terminal and the buffer may narrow it again.
        let mut app = app_with(Buffer::from_text(&"x".repeat(400)));
        app.set_viewport(Rect::new(0, 0, 420, 6));

        app.handle(click(3 + 300, 0));
        assert_eq!(app.buffer().selections().primary().head, 300);
    }

    #[test]
    fn a_click_below_row_223_lands_where_it_was_made() {
        let mut app = app_with(Buffer::from_text(&"line\n".repeat(400)));
        app.set_viewport(Rect::new(0, 0, 40, 302));

        // Three digits of line number plus two columns of padding.
        app.handle(click(5 + 2, 250));
        let head = app.buffer().selections().primary().head;
        assert_eq!(app.buffer().line_of(head), 250);
    }

    #[test]
    fn pointer_motion_on_its_own_costs_no_frame() {
        // With hover tracking on, motion arrives for every cell the pointer
        // crosses. Until something reacts to it, none of it may redraw.
        let mut app = app_over("hello\n");
        let motion = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.handle(motion), Outcome::Continue);
    }

    #[test]
    fn clicking_the_status_line_does_not_reach_the_text_beneath_it() {
        // Viewport is six rows: five of text and the status line on row 5. The
        // buffer is long enough that row 5 would otherwise be a line of text.
        let mut app = app_over(&"line\n".repeat(20));
        app.handle(click(5, 5));
        assert_eq!(app.buffer().selections().primary().head, 0);
    }

    #[test]
    fn a_resize_moves_the_status_line_and_the_clicks_follow_it() {
        let mut app = app_over(&"line\n".repeat(20));
        app.handle(Event::Resize(40, 10));
        app.handle(click(3 + 1, 5));
        assert_eq!(app.buffer().line_of(app.buffer().selections().primary().head), 5);
    }

    #[test]
    fn an_idle_editor_has_nothing_to_wake_up_for() {
        let app = app_over("hello");
        assert_eq!(app.deadline(), None);
        assert!(!app.wants_motion(), "nothing on screen reacts to hover yet");
    }

    #[test]
    fn clicking_in_the_gutter_selects_the_line() {
        let mut app = app_over("hello\nworld\n");
        app.handle(click(0, 0));
        let primary = app.buffer().selections().primary();
        assert_eq!((primary.from(), primary.to()), (0, 6), "the line and its newline");
    }

    #[test]
    fn the_wheel_scrolls_without_moving_the_caret() {
        let mut app = app_over(&"line\n".repeat(40));
        let caret = app.buffer().selections().primary().head;

        app.handle(wheel(true));
        assert_eq!(app.scroll(), 3);
        assert_eq!(app.buffer().selections().primary().head, caret, "scrolling is not navigation");

        app.handle(wheel(false));
        assert_eq!(app.scroll(), 0);
    }

    // ── status line ─────────────────────────────────────────────────────────

    #[test]
    fn the_status_line_shows_unsaved_state_and_position() {
        let mut app = app_over("hello\nworld");
        let (left, right) = app.status();
        assert_eq!(left, "[no name]");
        assert!(right.contains("Ln 1, Col 1"));

        app.handle(key(KeyCode::Char('x')));
        let (left, right) = app.status();
        assert!(left.ends_with(" •"), "an unsaved buffer is marked: {left}");
        assert!(right.contains("Ln 1, Col 2"));
    }

    #[test]
    fn the_status_line_counts_multiple_carets() {
        let mut buffer = Buffer::from_text("a\nb\nc");
        buffer.set_selections(Selections::new(
            vec![Range::caret(0), Range::caret(2), Range::caret(4)],
            0,
        ));
        let app = app_with(buffer);
        assert!(app.status().1.contains("3 carets"));
    }

    #[test]
    fn the_status_line_reports_crlf_when_that_is_what_will_be_written() {
        let (buffer, _) = Buffer::from_bytes(b"one\r\ntwo\r\n");
        let app = app_with(buffer);
        assert!(app.status().1.contains("CRLF"));
    }

    #[test]
    fn a_message_takes_over_the_status_line_and_is_cleared_by_the_next_key() {
        let mut app = app_over("x");
        app.handle(chord(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.message().is_some());
        app.handle(key(KeyCode::Right));
        assert!(app.message().is_none());
    }

    // ── rendering ───────────────────────────────────────────────────────────

    #[test]
    fn the_editor_and_its_status_line_both_get_drawn() {
        let app = app_over("hello\nworld\n");
        let mut harness = nun_ui::Harness::new(30, 4);
        harness.draw(TestView(&app));

        let text = harness.to_text();
        assert!(text.starts_with("1  hello\n2  world"), "{text}");
        assert!(text.contains("[no name]"), "the status line is missing: {text}");
    }

    #[test]
    fn the_status_line_is_painted_across_its_whole_width() {
        let app = app_over("hi");
        let mut harness = nun_ui::Harness::new(30, 3);
        harness.draw(TestView(&app));

        let cells = harness.cells();
        for x in 0..30 {
            assert_ne!(cells[(x, 2)].bg, Color::Reset, "status cell {x} was left unpainted");
        }
    }

    #[test]
    fn a_viewport_one_row_tall_still_renders_the_status_line() {
        let app = app_over("hello");
        let mut harness = nun_ui::Harness::new(20, 1);
        harness.draw(TestView(&app));
        assert!(harness.to_text().contains("[no name]"));
    }

    struct TestView<'a>(&'a App);

    impl Widget for TestView<'_> {
        fn render(self, area: Rect, cells: &mut Cells) {
            self.0.render(area, cells);
        }
    }
}
