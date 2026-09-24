//! Going to where a symbol is defined, finding where it is used, and getting
//! back again.
//!
//! * Ctrl-click on a symbol goes to its definition, and Alt-Ctrl-click opens
//!   it in the pane beside. F12 and Ctrl+K F12 do the same from the caret.
//! * Holding Ctrl over a symbol underlines it — but only once its server has
//!   said it has a definition, so the underline is a promise the click keeps.
//!   The server is asked once the pointer has settled on a word, and the
//!   question is cancelled the moment it moves to another.
//! * A symbol defined in several places offers them in the palette rather
//!   than picking one.
//! * Its references are listed in the sidebar, grouped by file, in the same
//!   rows as a project search; clicking one goes there.
//! * Every one of those jumps is remembered, so Back and Forward retrace them
//!   across files and panes.
//!
//! Nothing here waits on a server. Each question is sent, its id kept, and
//! the answer acted on when it arrives, if it is still the answer wanted.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nun_core::Selections;
use nun_lsp::types::request::{GotoDefinition, References};
use nun_lsp::types::{
    self as lsp, GotoDefinitionParams, GotoDefinitionResponse, OneOf, ReferenceContext,
    ReferenceParams,
};
use nun_lsp::{Encoding, RequestId, Response};
use nun_theme::Role;
use nun_ui::{Glyph, HitState, PaletteEntry, ReferencesView, SearchRow};
use nun_workspace::Job;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

use super::palette::{Pick, Row};
use super::panes::DocId;
use super::{App, Focus, Outcome, SidebarView, Target};

/// How long the pointer rests on a word, with Ctrl held, before its server is
/// asked whether it has a definition. Short, because the underline is the
/// answer to "can I click this"; long enough that sweeping across a line asks
/// about the word it stops on rather than every word it crosses.
const SETTLE: Duration = Duration::from_millis(80);

/// How many places Back remembers. A jump list is for retracing a train of
/// thought, not an afternoon.
const MOST_JUMPS: usize = 100;

/// Chars of a long line shown before the reference in it, when the line has to
/// be cut to bring the reference into the panel.
const LEAD: usize = 24;

/// A place a server named: a file, and a range in it in the server's units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) path: PathBuf,
    pub(super) range: lsp::Range,
    /// The units `range` counts in: the answering server's.
    pub(super) encoding: Encoding,
}

/// Where to show a definition once it is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Open {
    /// In the pane being worked in.
    Here,
    /// In the pane beside it, split off if there is none.
    Beside,
}

/// Where the caret was, to come back to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spot {
    pane: usize,
    doc: DocId,
    /// The file, for when the document has been closed since.
    path: Option<PathBuf>,
    line: usize,
    /// In chars from the start of the line.
    column: usize,
}

/// Places gone from and come back from.
#[derive(Debug, Default)]
struct Jumps {
    back: Vec<Spot>,
    forward: Vec<Spot>,
}

impl Jumps {
    /// A jump is being made from `here`: it is where Back goes, and whatever
    /// Forward had is a branch not taken.
    fn record(&mut self, here: Spot) {
        if self.back.last() != Some(&here) {
            self.back.push(here);
        }
        if self.back.len() > MOST_JUMPS {
            self.back.remove(0);
        }
        self.forward.clear();
    }
}

/// A question a person is waiting on the answer to.
#[derive(Debug)]
enum Asked {
    Definition { id: RequestId, open: Open, word: String },
    References { id: RequestId, word: String },
}

impl Asked {
    const fn id(&self) -> RequestId {
        match self {
            Self::Definition { id, .. } | Self::References { id, .. } => *id,
        }
    }
}

/// The word under the pointer while Ctrl is held.
#[derive(Debug)]
struct Link {
    doc: DocId,
    /// The chars to underline: the word, or the span the server said links.
    word: Range<usize>,
    state: LinkState,
}

#[derive(Debug)]
enum LinkState {
    /// Waiting until this for the pointer to settle.
    Settling(Instant),
    /// Asked, and the version of the document it was asked about.
    Asking(RequestId, Option<i32>),
    /// Somewhere to go. Never empty.
    Found(Vec<Place>),
    /// Nowhere: not a symbol with a definition, or the server could not say.
    Nothing,
}

/// One file's references.
#[derive(Debug)]
struct Group {
    path: PathBuf,
    label: String,
    hits: Vec<Hit>,
}

/// One reference, and the line it is on once that has been read.
#[derive(Debug)]
struct Hit {
    place: Place,
    /// The line as shown, possibly cut to bring the reference into view.
    text: String,
    matched: Vec<Range<u32>>,
}

/// A row of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    File(usize),
    Hit(usize, usize),
}

/// What the references panel lists.
#[derive(Debug, Default)]
pub(super) struct Listing {
    groups: Vec<Group>,
    collapsed: BTreeSet<usize>,
    rows: Vec<Line>,
    summary: String,
    widest: u32,
    scroll: usize,
    selected: Option<usize>,
    /// Which listing asked for the lines being read, so lines read for an
    /// older one are dropped.
    generation: u64,
}

impl Listing {
    fn relist(&mut self) {
        self.rows.clear();
        for (at, group) in self.groups.iter().enumerate() {
            self.rows.push(Line::File(at));
            if !self.collapsed.contains(&at) {
                self.rows.extend((0..group.hits.len()).map(|hit| Line::Hit(at, hit)));
            }
        }
    }

    fn view_rows(&self, window: Range<usize>) -> Vec<SearchRow<'_>> {
        let end = window.end.min(self.rows.len());
        let first = window.start.min(end);
        self.rows[first..end]
            .iter()
            .map(|line| match *line {
                Line::File(at) => {
                    let group = &self.groups[at];
                    SearchRow::File {
                        path: &group.label,
                        hits: group.hits.len(),
                        collapsed: self.collapsed.contains(&at),
                        state: HitState::Plain,
                    }
                }
                Line::Hit(group, hit) => {
                    let hit = &self.groups[group].hits[hit];
                    SearchRow::Hit {
                        line: hit.place.range.start.line.saturating_add(1),
                        text: &hit.text,
                        matched: &hit.matched,
                        state: HitState::Plain,
                    }
                }
            })
            .collect()
    }
}

/// Everything navigation keeps between events.
#[derive(Debug, Default)]
pub(super) struct Navigation {
    jumps: Jumps,
    asked: Option<Asked>,
    link: Option<Link>,
    pub(super) listing: Listing,
    /// The project's root as opened, and as resolved on disk.
    root: Option<(PathBuf, PathBuf)>,
}

impl App {
    // ── asking ──────────────────────────────────────────────────────────────

    /// Whether the focused document's server can find definitions.
    fn can_define(&self) -> bool {
        let id = self.doc().id;
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.capabilities(id))
            .is_some_and(|caps| offers(caps.definition_provider.as_ref()))
    }

    /// Whether it can find references.
    fn can_find_references(&self) -> bool {
        let id = self.doc().id;
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.capabilities(id))
            .is_some_and(|caps| offers(caps.references_provider.as_ref()))
    }

    /// The identifier at `at` in the focused document, as a char range.
    fn word_at(&self, at: usize) -> Option<Range<usize>> {
        let buffer = &self.doc().buffer;
        let ch = buffer.rope().get_char(at)?;
        if !(ch.is_alphanumeric() || ch == '_') {
            return None;
        }
        let (from, to) = buffer.word_range(at);
        (from < to).then_some(from..to)
    }

    /// The text of `range` in the focused document.
    fn text_of(&self, range: Range<usize>) -> String {
        self.doc().buffer.rope().slice(range).to_string()
    }

    /// The word at `at`, for saying what was asked about.
    fn name_at(&self, at: usize) -> String {
        self.word_at(at)
            .map_or_else(|| "that".to_string(), |word| format!("`{}`", self.text_of(word)))
    }

    /// Ask for the definition of whatever is at the caret.
    pub(super) fn go_to_definition(&mut self, open: Open) -> Outcome {
        let at = self.doc().buffer.selections().primary().head;
        self.ask_definition(at, open)
    }

    /// Ask for the definition of whatever is at `at`, going there when the
    /// answer comes — at once, if the underline already has it.
    fn ask_definition(&mut self, at: usize, open: Open) -> Outcome {
        if let Some(places) = self.link_places_at(at) {
            let word = self.name_at(at);
            return self.definition_found(&places, open, &word);
        }
        if !self.can_define() {
            self.message = Some(self.no_server("go to definitions"));
            return Outcome::Redraw;
        }
        let id = self.doc().id;
        let Some(position) =
            self.lsp.as_ref().and_then(|lsp| lsp.position_params(id, self.doc().buffer.rope(), at))
        else {
            return Outcome::Continue;
        };
        let params = GotoDefinitionParams {
            text_document_position_params: position,
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
        };
        let word = self.name_at(at);
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        match lsp.request::<GotoDefinition>(id, params) {
            Ok(request) => {
                self.forget_asked();
                self.navigation.asked = Some(Asked::Definition { id: request, open, word });
                Outcome::Continue
            }
            Err(error) => {
                self.message = Some(format!("Cannot go to the definition: {error}."));
                Outcome::Redraw
            }
        }
    }

    /// List the references to whatever is at the caret.
    pub(super) fn find_references(&mut self) -> Outcome {
        if !self.can_find_references() {
            self.message = Some(self.no_server("find references"));
            return Outcome::Redraw;
        }
        let id = self.doc().id;
        let at = self.doc().buffer.selections().primary().head;
        let Some(position) =
            self.lsp.as_ref().and_then(|lsp| lsp.position_params(id, self.doc().buffer.rope(), at))
        else {
            return Outcome::Continue;
        };
        let params = ReferenceParams {
            text_document_position: position,
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
            context: ReferenceContext { include_declaration: true },
        };
        let word = self.name_at(at);
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        match lsp.request::<References>(id, params) {
            Ok(request) => {
                self.forget_asked();
                self.message = Some(format!("Finding references to {word}…"));
                self.navigation.asked = Some(Asked::References { id: request, word });
            }
            Err(error) => self.message = Some(format!("Cannot find references: {error}.")),
        }
        Outcome::Redraw
    }

    /// Why a feature needing a server is not there, in the status line's words.
    pub(super) fn no_server(&self, what: &str) -> String {
        let id = self.doc().id;
        match self.lsp.as_ref().and_then(|lsp| lsp.indicator(id)) {
            Some(indicator) if self.lsp.as_ref().and_then(|lsp| lsp.capabilities(id)).is_some() => {
                format!("{} cannot {what}.", indicator.label())
            }
            Some(indicator) => format!("Cannot {what} yet: {}.", indicator.label()),
            None => format!("This file has no language server to {what} with."),
        }
    }

    /// Stop waiting for the last question.
    fn forget_asked(&mut self) {
        if let Some(asked) = self.navigation.asked.take()
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(asked.id());
        }
    }

    /// A server answered something. `None` when it was not an answer to
    /// anything asked here.
    pub(super) fn navigation_answered(&mut self, response: &Response) -> Option<Outcome> {
        let encoding =
            self.lsp.as_ref().and_then(|lsp| lsp.encoding(response.doc)).unwrap_or_default();
        if self.navigation.asked.as_ref().is_some_and(|asked| asked.id() == response.id) {
            let asked = self.navigation.asked.take()?;
            return Some(match asked {
                Asked::Definition { open, word, .. } => match response.parse::<GotoDefinition>() {
                    Ok(found) => self.definition_found(&places_of(found, encoding), open, &word),
                    Err(error) => self.say(format!("Could not go to the definition: {error}.")),
                },
                Asked::References { word, .. } => match response.parse::<References>() {
                    Ok(found) => {
                        let places = found
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|location| {
                                place_of(&location.uri, location.range, encoding)
                            })
                            .collect();
                        self.references_found(places, &word)
                    }
                    Err(error) => self.say(format!("Could not find references: {error}.")),
                },
            });
        }
        let asking = match self.navigation.link.as_ref().map(|link| &link.state) {
            Some(LinkState::Asking(id, version)) if *id == response.id => *version,
            _ => return None,
        };
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(response.doc));
        let found = match response.parse::<GotoDefinition>() {
            // An answer about text that has changed since may be about another
            // word altogether.
            Ok(found) if current == asking => found,
            _ => None,
        };
        Some(self.link_found(found, encoding))
    }

    fn say(&mut self, message: String) -> Outcome {
        self.message = Some(message);
        Outcome::Redraw
    }

    // ── going there ─────────────────────────────────────────────────────────

    /// A definition was found: go there, or offer the choice.
    pub(super) fn definition_found(&mut self, places: &[Place], open: Open, word: &str) -> Outcome {
        match places {
            [] => self.say(format!("No definition found for {word}.")),
            [place] => {
                self.go_to(place, open);
                Outcome::Redraw
            }
            _ => {
                self.resolve_root();
                let rows = self.place_rows(places, open);
                let count = places.len();
                self.open_choices(format!("{count} definitions of {word} — pick one"), rows)
            }
        }
    }

    /// Rows for the palette, one per place.
    fn place_rows(&self, places: &[Place], open: Open) -> Vec<Row> {
        places
            .iter()
            .map(|place| {
                let open_now = self.open_doc_of(&place.path).is_some();
                Row {
                    entry: PaletteEntry {
                        label: format!(
                            "{}:{}",
                            self.label_of(&place.path),
                            place.range.start.line + 1
                        ),
                        matched: Vec::new(),
                        hint: if open_now { "open".into() } else { String::new() },
                    },
                    pick: Pick::Place(place.clone(), open),
                }
            })
            .collect()
    }

    /// A place was picked in the palette.
    pub(super) fn pick_place(&mut self, place: &Place, open: Open) -> Outcome {
        self.go_to(place, open);
        Outcome::Redraw
    }

    /// Go to `place`, remembering where this was. Returns whether it got
    /// there: the file may have gone.
    fn go_to(&mut self, place: &Place, open: Open) -> bool {
        let here = self.here();
        self.clear_link();
        let path = self.open_doc_of(&place.path).unwrap_or_else(|| place.path.clone());
        match open {
            Open::Here => self.open_in_tab(&path),
            Open::Beside => self.open_beside(&path),
        }
        if self.doc().buffer.path() != Some(path.as_path()) {
            return false;
        }
        let at = place.encoding.char_index(self.doc().buffer.rope(), place.range.start);
        self.navigation.jumps.record(here);
        self.focus = Focus::Editor;
        self.doc_mut().buffer.set_selections(Selections::single(nun_core::Range::caret(at)));
        self.show_with_context(at);
        true
    }

    /// The path of the open document that is the same file as `path`, if
    /// one is. A server names files by their resolved path, which need not
    /// be the one the file was opened by — a symlinked folder, a relative
    /// path, macOS's `/private` — and opening it a second time under another
    /// name would give two buffers racing each other to save one file.
    ///
    /// The server's own name for each document is the one asked about, so
    /// nothing here touches the disk.
    fn open_doc_of(&self, path: &Path) -> Option<PathBuf> {
        self.docs.iter().find_map(|doc| {
            let open = doc.buffer.path()?;
            let resolved = self
                .lsp
                .as_ref()
                .and_then(|lsp| lsp.identifier(doc.id))
                .and_then(|identifier| nun_lsp::uri::to_path(&identifier.uri));
            (open == path || resolved.as_deref() == Some(path)).then(|| open.to_path_buf())
        })
    }

    /// Resolve the project's root once, for naming the files a server names
    /// by their resolved paths. One lookup for the session, not one per row.
    fn resolve_root(&mut self) {
        let Some(root) = self.workspace_root() else { return };
        if self.navigation.root.as_ref().is_none_or(|(raw, _)| *raw != root) {
            let resolved = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            self.navigation.root = Some((root, resolved));
        }
    }

    /// Open `path` in the pane beside this one: the next pane across, or a
    /// new one split off to the right when there is only this one.
    ///
    /// A file already showing in another pane is gone to where it is. One
    /// showing in this pane stays here: a document is in one place at a time,
    /// and a second copy of it would race the first to save.
    fn open_beside(&mut self, path: &Path) {
        let origin = self.panes.focus();
        let existing = self
            .docs
            .iter()
            .find(|doc| doc.buffer.path().is_some_and(|open| super::same_file(open, path)))
            .map(|doc| doc.id);
        if let Some(id) = existing
            && let Some((pane, index)) = self.panes.find(id)
            && (pane != origin || self.panes.focused().current() == Some(id))
        {
            self.select_tab(pane, index);
            return;
        }

        let ids: Vec<usize> = self.panes.all().iter().map(|pane| pane.id).collect();
        let fresh = if ids.len() > 1 {
            let at = ids.iter().position(|id| *id == origin).unwrap_or(0);
            self.panes.set_focus(ids[(at + 1) % ids.len()]);
            None
        } else {
            self.split_pane(nun_ui::Dir::Beside);
            self.panes.focused().current()
        };
        let target = self.panes.focus();
        // Open in the background of this pane: moved over, rather than
        // opened twice.
        if let Some(id) = existing
            && let Some((pane, index)) = self.panes.find(id)
        {
            let at = self.panes.get(target).map_or(0, |state| state.tabs.len());
            self.panes.move_tab(pane, index, target, at);
            // The empty buffer a split starts with has nothing to keep.
            if let Some(fresh) = fresh
                && let Some(index) = self
                    .panes
                    .get(target)
                    .and_then(|state| state.tabs.iter().position(|tab| *tab == fresh))
            {
                self.drop_tab(target, index);
            }
        }
        self.open_in_tab(path);
    }

    /// Where the caret is, as a place to come back to.
    fn here(&self) -> Spot {
        let buffer = &self.doc().buffer;
        let head = buffer.selections().primary().head;
        let line = buffer.line_of(head);
        Spot {
            pane: self.panes.focus(),
            doc: self.doc().id,
            path: buffer.path().map(Path::to_path_buf),
            line,
            column: head - buffer.line_start(line),
        }
    }

    /// Go back to `spot`: its pane, its document — reopened if it was closed
    /// — and its place in it. Returns whether it could.
    fn visit(&mut self, spot: &Spot) -> bool {
        self.panes.set_focus(spot.pane);
        if let Some((pane, index)) = self.panes.find(spot.doc) {
            self.select_tab(pane, index);
        } else if let Some(path) = &spot.path {
            self.open_in_tab(path);
            if self.doc().buffer.path() != Some(path.as_path()) {
                return false;
            }
        } else {
            return false;
        }
        let buffer = &self.doc().buffer;
        let line = spot.line.min(buffer.len_lines().saturating_sub(1));
        let at = (buffer.line_start(line) + spot.column).min(buffer.line_end(line));
        self.clear_link();
        self.focus = Focus::Editor;
        self.doc_mut().buffer.set_selections(Selections::single(nun_core::Range::caret(at)));
        self.show_with_context(at);
        true
    }

    /// Go back to where the last jump was made from.
    pub(super) fn jump_back(&mut self) -> Outcome {
        self.retrace(true)
    }

    /// Go forward again to where Back came from.
    pub(super) fn jump_forward(&mut self) -> Outcome {
        self.retrace(false)
    }

    fn retrace(&mut self, back: bool) -> Outcome {
        let here = self.here();
        loop {
            let jumps = &mut self.navigation.jumps;
            let next = if back { jumps.back.pop() } else { jumps.forward.pop() };
            let Some(spot) = next else {
                let way = if back { "back" } else { "forward" };
                return self.say(format!("Nothing to go {way} to."));
            };
            // A place that can no longer be reached is skipped rather than
            // stopping the walk at it.
            if spot != here && self.visit(&spot) {
                let jumps = &mut self.navigation.jumps;
                if back {
                    jumps.forward.push(here);
                } else {
                    jumps.back.push(here);
                }
                return Outcome::Redraw;
            }
        }
    }

    /// Whether there is anywhere to go back, or forward, to.
    pub(super) fn can_retrace(&self, back: bool) -> bool {
        let jumps = &self.navigation.jumps;
        if back { !jumps.back.is_empty() } else { !jumps.forward.is_empty() }
    }

    /// What the text's menu offers for getting about: the definition and the
    /// references when the server can find them, Back and Forward when there
    /// is somewhere to go.
    pub(super) fn navigation_offers(&self) -> Vec<crate::commands::Command> {
        use crate::commands::Command;
        let mut commands = Vec::new();
        if self.can_define() {
            commands.extend([Command::GoToDefinition, Command::OpenDefinitionBeside]);
        }
        if self.can_find_references() {
            commands.push(Command::FindReferences);
        }
        if self.can_retrace(true) {
            commands.push(Command::GoBack);
        }
        if self.can_retrace(false) {
            commands.push(Command::GoForward);
        }
        commands
    }

    // ── the pointer ─────────────────────────────────────────────────────────

    /// The left button went down in the text with Ctrl held. `None` when
    /// this is not a go-to-definition click, and the press should be taken
    /// the way it was before there were language servers: a double-click
    /// grows the selection, and so does a click inside what that reached.
    pub(super) fn definition_click(&mut self, at: usize, count: u8, alt: bool) -> Option<Outcome> {
        if count > 1 {
            // The first click of this double-click asked for a definition;
            // a double-click is a different gesture, and wants no jump.
            if matches!(self.navigation.asked, Some(Asked::Definition { .. })) {
                self.forget_asked();
            }
            return None;
        }
        let primary = self.doc().buffer.selections().primary();
        let inside = at >= primary.from() && at <= primary.to();
        let known = self.link_places_at(at).is_some();
        if (self.can_shrink() && inside)
            || self.word_at(at).is_none()
            || !(known || self.can_define())
        {
            return None;
        }
        // The click lands where it was made, so Back comes back to it.
        let places = self.link_places_at(at);
        self.doc_mut().buffer.set_selections(Selections::single(nun_core::Range::caret(at)));
        let open = if alt { Open::Beside } else { Open::Here };
        Some(match places {
            Some(places) => {
                let word = self.name_at(at);
                self.definition_found(&places, open, &word)
            }
            None => self.ask_definition(at, open).and(Outcome::Redraw),
        })
    }

    /// Any mouse report: keep the underline on the word under a pointer
    /// with Ctrl held, and nowhere else.
    pub(super) fn link_pointer(&mut self, mouse: MouseEvent, now: Instant) -> Outcome {
        let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
        match mouse.kind {
            MouseEventKind::Moved if ctrl => self.link_motion(mouse.column, mouse.row, now),
            // The press that follows the pointer uses what it found.
            MouseEventKind::Down(MouseButton::Left) if ctrl => Outcome::Continue,
            _ => self.clear_link(),
        }
    }

    fn link_motion(&mut self, column: u16, row: u16, now: Instant) -> Outcome {
        let over_text = self.hits.at(column, row).is_some_and(|hit| hit.target == Target::Text);
        let at = (over_text && self.finder.is_none() && self.menu.is_none())
            .then(|| self.position_at(column, row))
            .flatten();
        let Some(at) = at else { return self.clear_link() };
        let doc = self.doc().id;
        // Still on the same word: nothing has changed.
        if self
            .navigation
            .link
            .as_ref()
            .is_some_and(|link| link.doc == doc && link.word.contains(&at))
        {
            return Outcome::Continue;
        }
        let cleared = self.clear_link();
        if let Some(word) = self.word_at(at).filter(|_| self.can_define()) {
            self.navigation.link =
                Some(Link { doc, word, state: LinkState::Settling(now + SETTLE) });
        }
        cleared
    }

    /// Take the underline away, and stop asking about it.
    pub(super) fn clear_link(&mut self) -> Outcome {
        let Some(link) = self.navigation.link.take() else { return Outcome::Continue };
        match link.state {
            LinkState::Asking(id, _) => {
                if let Some(lsp) = self.lsp.as_mut() {
                    lsp.cancel(id);
                }
                Outcome::Continue
            }
            LinkState::Found(_) => Outcome::Redraw,
            LinkState::Settling(_) | LinkState::Nothing => Outcome::Continue,
        }
    }

    /// When the pointer will have settled, if it is settling.
    pub(super) fn link_deadline(&self) -> Option<Instant> {
        match self.navigation.link.as_ref()?.state {
            LinkState::Settling(due) => Some(due),
            _ => None,
        }
    }

    /// The pointer has settled: ask about the word under it.
    pub(super) fn link_tick(&mut self, now: Instant) -> Outcome {
        let Some(link) = self.navigation.link.as_ref() else { return Outcome::Continue };
        let LinkState::Settling(due) = link.state else { return Outcome::Continue };
        if now < due {
            return Outcome::Continue;
        }
        let (doc, at) = (link.doc, link.word.start);
        if doc != self.doc().id {
            self.navigation.link = None;
            return Outcome::Continue;
        }
        let params = self.lsp.as_ref().and_then(|lsp| {
            Some((lsp.position_params(doc, self.doc().buffer.rope(), at)?, lsp.version(doc)))
        });
        let state = match params {
            Some((position, version)) => {
                let params = GotoDefinitionParams {
                    text_document_position_params: position,
                    work_done_progress_params: lsp::WorkDoneProgressParams::default(),
                    partial_result_params: lsp::PartialResultParams::default(),
                };
                match self.lsp.as_mut().map(|lsp| lsp.request::<GotoDefinition>(doc, params)) {
                    Some(Ok(id)) => LinkState::Asking(id, version),
                    _ => LinkState::Nothing,
                }
            }
            None => LinkState::Nothing,
        };
        if let Some(link) = self.navigation.link.as_mut() {
            link.state = state;
        }
        Outcome::Continue
    }

    /// The server said where the word under the pointer is defined, if
    /// anywhere.
    fn link_found(&mut self, found: Option<GotoDefinitionResponse>, encoding: Encoding) -> Outcome {
        let origin = match &found {
            Some(GotoDefinitionResponse::Link(links)) => {
                links.first().and_then(|link| link.origin_selection_range)
            }
            _ => None,
        };
        let places = places_of(found, encoding);
        let span = origin.map(|range| encoding.char_range(self.doc().buffer.rope(), range));
        let Some(link) = self.navigation.link.as_mut() else { return Outcome::Continue };
        if places.is_empty() {
            link.state = LinkState::Nothing;
            return Outcome::Continue;
        }
        // The server knows better than a word boundary what the symbol is —
        // `r#type`, a path, an operator — as long as it covers the pointer.
        if let Some(span) = span.filter(|span| !span.is_empty() && span.contains(&link.word.start))
        {
            link.word = span;
        }
        link.state = LinkState::Found(places);
        Outcome::Redraw
    }

    /// Where the underlined word at `at` goes, if it is underlined.
    fn link_places_at(&self, at: usize) -> Option<Vec<Place>> {
        let link = self.navigation.link.as_ref()?;
        match &link.state {
            LinkState::Found(places) if link.doc == self.doc().id && link.word.contains(&at) => {
                Some(places.clone())
            }
            _ => None,
        }
    }

    /// Whether the pointer needs following with no button held: whenever a
    /// Ctrl-hover could find something to underline.
    pub(super) fn wants_link_motion(&self) -> bool {
        self.can_define()
    }

    /// Underline the word, once its server has said it goes somewhere.
    pub(super) fn render_link(&self, cells: &mut Cells) {
        let Some(link) = self.navigation.link.as_ref() else { return };
        if !matches!(link.state, LinkState::Found(_)) || link.doc != self.doc().id {
            return;
        }
        let (text, _) = self.areas();
        let buffer = &self.doc().buffer;
        let word_line = buffer.line_of(link.word.start);
        let style = self.palette.ink(Role::Accent).add_modifier(Modifier::UNDERLINED);
        for row in 0..text.height {
            if self.line_at_row(usize::from(row)) != Some(word_line) {
                continue;
            }
            let y = text.y + row;
            for x in text.left()..text.right() {
                if self.position_at(x, y).is_some_and(|at| link.word.contains(&at)) {
                    let cell = &mut cells[(x, y)];
                    cell.set_style(cell.style().patch(style));
                }
            }
        }
    }

    // ── references ──────────────────────────────────────────────────────────

    /// A server listed the references to `word`.
    pub(super) fn references_found(&mut self, mut places: Vec<Place>, word: &str) -> Outcome {
        if places.is_empty() {
            return self.say(format!("No references to {word} found."));
        }
        places.sort_by(|a, b| {
            (&a.path, a.range.start.line, a.range.start.character).cmp(&(
                &b.path,
                b.range.start.line,
                b.range.start.character,
            ))
        });
        places.dedup();
        self.resolve_root();
        let count = places.len();
        // With no folder open there is no sidebar to list them in; the
        // palette will do, and it is still a list to click.
        if self.sidebar.is_none() {
            let rows = self.place_rows(&places, Open::Here);
            return self.open_choices(format!("{count} references to {word}"), rows);
        }

        let mut groups: Vec<Group> = Vec::new();
        for place in places {
            if groups.last().is_none_or(|group| group.path != place.path) {
                groups.push(Group {
                    label: self.label_of(&place.path),
                    path: place.path.clone(),
                    hits: Vec::new(),
                });
            }
            if let Some(group) = groups.last_mut() {
                group.hits.push(Hit { place, text: String::new(), matched: Vec::new() });
            }
        }

        let files = groups.len();
        let listing = &mut self.navigation.listing;
        let generation = listing.generation + 1;
        *listing = Listing {
            widest: groups
                .iter()
                .flat_map(|group| &group.hits)
                .map(|hit| hit.place.range.start.line.saturating_add(1))
                .max()
                .unwrap_or(1),
            groups,
            summary: format!(
                "{count} {} to {word} in {files} {}",
                if count == 1 { "reference" } else { "references" },
                if files == 1 { "file" } else { "files" }
            ),
            generation,
            ..Listing::default()
        };
        listing.relist();

        // Lines of files that are open come from their buffers, which may be
        // newer than the disk; the rest are read off the main thread.
        let mut wanted = Vec::new();
        for at in 0..self.navigation.listing.groups.len() {
            let path = self.navigation.listing.groups[at].path.clone();
            let open = self.open_doc_of(&path).and_then(|open| {
                self.docs
                    .iter()
                    .find(|doc| doc.buffer.path().is_some_and(|path| super::same_file(path, &open)))
            });
            if let Some(doc) = open {
                let lines: Vec<(u32, String)> = self.navigation.listing.groups[at]
                    .hits
                    .iter()
                    .map(|hit| hit.place.range.start.line)
                    .filter(|line| (*line as usize) < doc.buffer.len_lines())
                    .map(|line| (line, doc.buffer.line_text(line as usize)))
                    .collect();
                let ellipsis = self.palette.glyph(Glyph::Ellipsis);
                fill_lines(&mut self.navigation.listing.groups[at], &lines, ellipsis);
            } else {
                let lines = self.navigation.listing.groups[at]
                    .hits
                    .iter()
                    .map(|hit| hit.place.range.start.line)
                    .collect();
                wanted.push((path, lines));
            }
        }
        if !wanted.is_empty() {
            self.send_job(Job::Lines { generation, wanted });
        }

        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.visible = true;
        }
        self.leave_edit_preview();
        self.sidebar_view = SidebarView::References;
        self.message = None;
        self.relayout();
        Outcome::Redraw
    }

    /// Lines read for the panel came back.
    pub(super) fn reference_lines(
        &mut self,
        generation: u64,
        lines: Vec<(PathBuf, Vec<(u32, String)>)>,
    ) -> Outcome {
        let ellipsis = self.palette.glyph(Glyph::Ellipsis);
        let listing = &mut self.navigation.listing;
        if generation != listing.generation {
            return Outcome::Continue;
        }
        for (path, lines) in lines {
            if let Some(group) = listing.groups.iter_mut().find(|group| group.path == path) {
                fill_lines(group, &lines, ellipsis);
            }
        }
        Outcome::Redraw
    }

    /// Where the panel goes, while it is the sidebar's view.
    pub(super) fn references_area(&self) -> Option<Rect> {
        (self.sidebar_view == SidebarView::References).then(|| self.tree_area()).flatten()
    }

    /// Lay out its hit regions, from the geometry it draws with.
    pub(super) fn layout_references(&self, hits: &mut nun_input::HitMap<Target>) {
        let Some(area) = self.references_area() else { return };
        hits.push(super::cells(area), Target::ReferencesEmpty, false);
        if let Some(cell) = ReferencesView::back_area(area) {
            hits.push(super::cells(cell), Target::ReferencesBack, true);
        }
        let rows = ReferencesView::rows_area(area);
        let listing = &self.navigation.listing;
        for (offset, index) in
            (listing.scroll..listing.rows.len()).take(usize::from(rows.height)).enumerate()
        {
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows.y + offset, height: 1, ..rows };
            hits.push(super::cells(line), Target::ReferencesRow(index), true);
        }
    }

    /// Draw it.
    pub(super) fn render_references(&self, cells: &mut Cells) {
        let Some(area) = self.references_area() else { return };
        let listing = &self.navigation.listing;
        let first = listing.scroll;
        let last = first.saturating_add(ReferencesView::visible_rows(area));
        let within = |row: Option<usize>| {
            row.filter(|row| (first..last).contains(row)).map(|row| row - first)
        };
        let hovered = match self.hover.current() {
            Some(Target::ReferencesRow(row)) => Some(row),
            _ => None,
        };
        let back = self.hover.current() == Some(Target::ReferencesBack);
        let rows = listing.view_rows(first..last);
        let summary =
            if back { ReferencesView::BACK_DESCRIPTION } else { listing.summary.as_str() };
        ReferencesView::new(&rows, summary, &self.palette)
            .widest_line(listing.widest)
            .selected(within(listing.selected))
            .hovered(within(hovered))
            .hovered_back(back)
            .render(area, cells);
    }

    /// The left button went down in the panel.
    pub(super) fn references_press(&mut self, target: Target) -> Outcome {
        match target {
            Target::ReferencesBack => self.show_file_tree(),
            Target::ReferencesRow(row) => self.pick_reference(row),
            _ => Outcome::Continue,
        }
    }

    /// Act on one row: a file folds, a reference is gone to.
    fn pick_reference(&mut self, row: usize) -> Outcome {
        let listing = &mut self.navigation.listing;
        let Some(line) = listing.rows.get(row).copied() else { return Outcome::Continue };
        listing.selected = Some(row);
        match line {
            Line::File(group) => {
                if !listing.collapsed.remove(&group) {
                    listing.collapsed.insert(group);
                }
                listing.relist();
                let last = listing.rows.len().saturating_sub(1);
                listing.selected = listing.selected.map(|at| at.min(last));
                self.follow_reference();
                Outcome::Redraw
            }
            Line::Hit(group, hit) => {
                let place = listing.groups[group].hits[hit].place.clone();
                self.go_to(&place, Open::Here);
                Outcome::Redraw
            }
        }
    }

    /// Go to the next reference in the list, or the one before.
    pub(super) fn step_reference(&mut self, delta: isize) -> Outcome {
        let listing = &self.navigation.listing;
        let hits: Vec<usize> = (0..listing.rows.len())
            .filter(|at| matches!(listing.rows[*at], Line::Hit(..)))
            .collect();
        if hits.is_empty() {
            let find = self.binding_for(crate::commands::Command::FindReferences);
            return self.say(format!("No references are listed. {find} lists them."));
        }
        let now = listing.selected.and_then(|row| hits.iter().position(|at| *at == row));
        let count = isize::try_from(hits.len()).unwrap_or(isize::MAX);
        let next = match now {
            Some(now) => (isize::try_from(now).unwrap_or(0) + delta).rem_euclid(count),
            None if delta < 0 => count - 1,
            None => 0,
        };
        let row = hits[usize::try_from(next).unwrap_or(0)];
        let outcome = self.pick_reference(row);
        self.follow_reference();
        outcome
    }

    /// Keep the selected row in view.
    fn follow_reference(&mut self) {
        let rows = self.references_area().map_or(0, ReferencesView::visible_rows);
        let listing = &mut self.navigation.listing;
        let Some(selected) = listing.selected else { return };
        if selected < listing.scroll {
            listing.scroll = selected;
        } else if rows > 0 && selected >= listing.scroll + rows {
            listing.scroll = selected + 1 - rows;
        }
    }

    /// The wheel over the panel.
    pub(super) fn references_scroll(&mut self, down: bool) -> Outcome {
        let rows = self.references_area().map_or(0, ReferencesView::visible_rows);
        let listing = &mut self.navigation.listing;
        let most = listing.rows.len().saturating_sub(rows);
        listing.scroll =
            if down { (listing.scroll + 3).min(most) } else { listing.scroll.saturating_sub(3) };
        Outcome::Redraw
    }

    /// How a file is named in a list: from the project's root when it is in
    /// the project, whole otherwise.
    fn label_of(&self, path: &Path) -> String {
        let roots = self.navigation.root.iter().flat_map(|(raw, resolved)| [raw, resolved]);
        let inside = self.workspace_root().into_iter().chain(roots.cloned()).find_map(|root| {
            path.strip_prefix(root).ok().map(|inside| inside.display().to_string())
        });
        inside.unwrap_or_else(|| path.display().to_string())
    }
}

/// Whether a capability is offered: `true`, or options, which mean yes.
const fn offers<T>(provider: Option<&OneOf<bool, T>>) -> bool {
    matches!(provider, Some(OneOf::Left(true) | OneOf::Right(_)))
}

/// A place from a location's two halves; `None` for a URI that is not a file.
fn place_of(uri: &lsp::Uri, range: lsp::Range, encoding: Encoding) -> Option<Place> {
    Some(Place { path: nun_lsp::uri::to_path(uri)?, range, encoding })
}

/// Every place a definition answer names, whichever of its three shapes it
/// came in. A link names the whole of what it points at and, inside that,
/// the part to land on; it is the part to land on that is gone to.
fn places_of(found: Option<GotoDefinitionResponse>, encoding: Encoding) -> Vec<Place> {
    let mut places: Vec<Place> = match found {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(location)) => {
            place_of(&location.uri, location.range, encoding).into_iter().collect()
        }
        Some(GotoDefinitionResponse::Array(locations)) => locations
            .into_iter()
            .filter_map(|location| place_of(&location.uri, location.range, encoding))
            .collect(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .filter_map(|link| place_of(&link.target_uri, link.target_selection_range, encoding))
            .collect(),
    };
    // Some servers name one definition twice — once per crate that
    // re-exports it — and a chooser offering the same line twice is noise.
    let mut seen = Vec::new();
    places.retain(|place| {
        let key = (place.path.clone(), place.range.start);
        let new = !seen.contains(&key);
        seen.push(key);
        new
    });
    places
}

/// Put the lines read for a group into its hits, with the reference in each
/// picked out, and `ellipsis` in front of a line cut short.
fn fill_lines(group: &mut Group, lines: &[(u32, String)], ellipsis: &str) {
    for hit in &mut group.hits {
        let start = hit.place.range.start;
        let Some((_, text)) = lines.iter().find(|(line, _)| *line == start.line) else { continue };
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let encoding = hit.place.encoding;
        let from = encoding.column(text, start.character);
        let end = hit.place.range.end;
        let to = if end.line == start.line {
            encoding.column(text, end.character).max(from)
        } else {
            text.chars().count()
        };
        let (shown, matched) = window(text, from, to, ellipsis);
        hit.text = shown;
        hit.matched = vec![matched];
    }
}

/// `text`, cut so the chars `from..to` are near its start when they would
/// otherwise be far along a long line, with those chars' range in what is
/// kept. `ellipsis` stands in for what was cut, and the range moves by the
/// chars it is made of, which need not be one.
fn window(text: &str, from: usize, to: usize, ellipsis: &str) -> (String, Range<u32>) {
    let cut = from.saturating_sub(LEAD);
    let mut shown = String::new();
    let (mut from, mut to) = (from, to);
    if cut > 0 {
        shown.push_str(ellipsis);
        let chars = ellipsis.chars().count();
        from = from - cut + chars;
        to = to - cut + chars;
    }
    shown.extend(text.chars().skip(cut));
    let at = |chars: usize| u32::try_from(chars).unwrap_or(u32::MAX);
    (shown, at(from)..at(to))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc::Receiver;

    use crossterm::event::{KeyCode, KeyEvent};
    use nun_lsp::types::{Location, LocationLink, Position};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use nun_workspace::Done;
    use tempfile::TempDir;

    use super::*;
    use crate::commands::{Command, KeySet, defaults};

    const MAIN: &str = "fn main() {\n    helper();\n    other();\n}\n";
    const LIB: &str = "// the library\npub fn helper() {}\n";

    /// An editor on a small project, with the tree's worker attached and
    /// pumped the way the event loop pumps it.
    struct Tester {
        app: App,
        done: Receiver<Done>,
        dir: TempDir,
    }

    impl Tester {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("src")).unwrap();
            fs::write(dir.path().join("src/main.rs"), MAIN).unwrap();
            fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
            let mut app = App::new(
                nun_core::Buffer::new(),
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 100, 20));
            let (sender, done) = std::sync::mpsc::channel();
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                false,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            app.open_in_tab(&dir.path().join("src/main.rs"));
            app.focus = Focus::Editor;
            app.relayout();
            let mut tester = Self { app, done, dir };
            tester.settle();
            tester
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn place(&self, name: &str, line: u32, character: u32) -> Place {
            let at = Position { line, character };
            Place {
                path: self.path(name),
                range: lsp::Range { start: at, end: Position { line, character: character + 6 } },
                encoding: Encoding::Utf16,
            }
        }

        /// Let the worker catch up.
        fn settle(&mut self) {
            for marker in 0..32 {
                self.app.echo(marker);
                let mut handled = 0;
                loop {
                    let message = self
                        .done
                        .recv_timeout(Duration::from_secs(10))
                        .expect("the worker answered");
                    if message == Done::Echo(marker) {
                        break;
                    }
                    self.app.handle(Event::Workspace(message));
                    handled += 1;
                }
                if handled == 0 && !self.app.is_loading() {
                    return;
                }
            }
        }

        fn caret(&self) -> (usize, usize) {
            let buffer = &self.app.doc().buffer;
            let head = buffer.selections().primary().head;
            let line = buffer.line_of(head);
            (line, head - buffer.line_start(line))
        }

        fn file(&self) -> String {
            let path = self.app.doc().buffer.path().expect("a file is open");
            path.file_name().unwrap().to_string_lossy().into_owned()
        }

        fn caret_at(&mut self, line: usize, column: usize) {
            let at = self.app.doc().buffer.line_start(line) + column;
            self.app
                .doc_mut()
                .buffer
                .set_selections(Selections::single(nun_core::Range::caret(at)));
        }

        fn click(&mut self, column: u16, row: u16, modifiers: KeyModifiers) {
            for kind in
                [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)]
            {
                self.app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers }));
            }
        }

        /// Where the focused text starts on screen, gutter and all.
        fn text_origin(&self) -> (u16, u16) {
            let (text, _) = self.app.areas();
            (text.x + self.app.gutter_width(), text.y)
        }
    }

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn uri(path: &Path) -> lsp::Uri {
        nun_lsp::uri::from_path(path).unwrap()
    }

    // ── what servers answer ─────────────────────────────────────────────────

    #[test]
    fn a_location_and_a_location_link_go_to_the_same_kind_of_place() {
        let path = PathBuf::from("/tmp/nun-a.rs");
        let range = lsp::Range { start: at(3, 4), end: at(3, 9) };
        let location = Location { uri: uri(&path), range };
        let scalar =
            places_of(Some(GotoDefinitionResponse::Scalar(location.clone())), Encoding::Utf8);
        assert_eq!(scalar, vec![Place { path: path.clone(), range, encoding: Encoding::Utf8 }]);

        let array = places_of(Some(GotoDefinitionResponse::Array(vec![location])), Encoding::Utf8);
        assert_eq!(array, scalar);

        // A link lands on its selection range, not on the whole of what it
        // points at.
        let link = LocationLink {
            origin_selection_range: None,
            target_uri: uri(&path),
            target_range: lsp::Range { start: at(1, 0), end: at(5, 1) },
            target_selection_range: range,
        };
        let links = places_of(Some(GotoDefinitionResponse::Link(vec![link])), Encoding::Utf8);
        assert_eq!(links, scalar);
        assert!(places_of(None, Encoding::Utf8).is_empty());
    }

    #[test]
    fn the_same_place_named_twice_is_offered_once() {
        let path = PathBuf::from("/tmp/nun-a.rs");
        let range = lsp::Range { start: at(3, 4), end: at(3, 9) };
        let location = Location { uri: uri(&path), range };
        let places = places_of(
            Some(GotoDefinitionResponse::Array(vec![location.clone(), location])),
            Encoding::Utf16,
        );
        assert_eq!(places.len(), 1);
    }

    #[test]
    fn a_reference_on_a_long_line_is_brought_into_view() {
        let (shown, matched) = window("short", 0, 5, "…");
        assert_eq!((shown.as_str(), matched), ("short", 0..5));

        let line = format!("{}needle rest", "x".repeat(100));
        let (shown, matched) = window(&line, 100, 106, "…");
        assert!(shown.starts_with('…'));
        let picked: String =
            shown.chars().skip(matched.start as usize).take(matched.len()).collect();
        assert_eq!(picked, "needle");

        // An ellipsis of two chars moves the range by two.
        let (shown, matched) = window(&line, 100, 106, "~\u{301}");
        assert!(shown.starts_with("~\u{301}"));
        let picked: String =
            shown.chars().skip(matched.start as usize).take(matched.len()).collect();
        assert_eq!(picked, "needle");
    }

    #[test]
    fn a_reference_is_picked_out_in_chars_whatever_the_server_counts_in() {
        // `a😀` is three UTF-16 units before `name`, and five bytes.
        let text = "a😀 name";
        for (encoding, start) in [(Encoding::Utf16, 4), (Encoding::Utf8, 6), (Encoding::Utf32, 3)] {
            let place = Place {
                path: PathBuf::from("/x"),
                range: lsp::Range { start: at(0, start), end: at(0, start + 4) },
                encoding,
            };
            let mut group = Group { path: PathBuf::from("/x"), label: "x".into(), hits: vec![] };
            group.hits.push(Hit { place, text: String::new(), matched: vec![] });
            fill_lines(&mut group, &[(0, text.to_string())], "…");
            let hit = &group.hits[0];
            let picked: String = hit
                .text
                .chars()
                .skip(hit.matched[0].start as usize)
                .take(hit.matched[0].len())
                .collect();
            // UTF-8's end is counted in bytes too, so `name` is four of them.
            assert_eq!(picked, "name", "{encoding:?}");
        }
    }

    // ── going and coming back ───────────────────────────────────────────────

    #[test]
    fn a_definition_in_a_file_not_open_opens_it_and_back_comes_back() {
        let mut t = Tester::new();
        t.caret_at(1, 6);
        let place = t.place("src/lib.rs", 1, 7);
        t.app.definition_found(&[place], Open::Here, "`helper`");
        assert_eq!(t.file(), "lib.rs");
        assert_eq!(t.caret(), (1, 7));

        t.app.run(Command::GoBack);
        assert_eq!(t.file(), "main.rs");
        assert_eq!(t.caret(), (1, 6));

        t.app.run(Command::GoForward);
        assert_eq!(t.file(), "lib.rs");
        assert_eq!(t.caret(), (1, 7));

        t.app.run(Command::GoForward);
        assert_eq!(t.app.message(), Some("Nothing to go forward to."));
    }

    #[test]
    fn a_file_named_by_another_path_is_not_opened_twice() {
        let mut t = Tester::new();
        // The server's name for the file is the resolved one, whichever
        // name it was opened by.
        let spec = nun_lsp::ServerSpec {
            command: "nun-test-no-such-server".into(),
            args: Vec::new(),
            optional: true,
        };
        let servers = std::collections::BTreeMap::from([("rust".to_string(), spec)]);
        let (sender, events) = std::sync::mpsc::channel();
        let report = Box::new(move |event| {
            let _ = sender.send(event);
        });
        t.app.attach_lsp(nun_lsp::Lsp::start(servers, None, report).unwrap());
        let id = t.app.doc().id;
        while t.app.lsp.as_ref().and_then(|lsp| lsp.identifier(id)).is_none() {
            let event = events.recv_timeout(Duration::from_secs(10)).expect("it was attached");
            t.app.handle(Event::Lsp(event));
        }
        let resolved = fs::canonicalize(t.path("src/main.rs")).unwrap();
        let docs = t.app.docs.len();
        let place = Place { path: resolved, ..t.place("src/main.rs", 2, 4) };
        t.app.definition_found(&[place], Open::Here, "`other`");
        assert_eq!(t.app.docs.len(), docs);
        assert_eq!(t.caret(), (2, 4));
    }

    #[test]
    fn several_definitions_are_offered_rather_than_one_picked() {
        let mut t = Tester::new();
        let places = [t.place("src/lib.rs", 1, 7), t.place("src/main.rs", 0, 3)];
        t.app.definition_found(&places, Open::Here, "`helper`");
        assert_eq!(t.file(), "main.rs", "nothing was gone to yet");
        let palette = t.app.finder.as_ref().expect("a chooser is open");
        let labels: Vec<&str> = palette.rows.iter().map(|row| row.entry.label.as_str()).collect();
        assert_eq!(labels, ["src/lib.rs:2", "src/main.rs:1"]);

        // Typing narrows it, as anywhere else in the palette.
        for ch in "lib".chars() {
            t.app.handle(Event::Key(KeyEvent::from(KeyCode::Char(ch))));
        }
        assert_eq!(t.app.finder.as_ref().unwrap().rows.len(), 1);
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Enter)));
        assert!(t.app.finder.is_none());
        assert_eq!(t.file(), "lib.rs");
        assert_eq!(t.caret(), (1, 7));
    }

    #[test]
    fn a_definition_beside_splits_and_back_crosses_the_panes() {
        let mut t = Tester::new();
        t.caret_at(1, 6);
        let origin = t.app.panes.focus();
        let place = t.place("src/lib.rs", 1, 7);
        t.app.definition_found(std::slice::from_ref(&place), Open::Beside, "`helper`");
        assert_eq!(t.app.panes.len(), 2);
        assert_ne!(t.app.panes.focus(), origin);
        assert_eq!(t.file(), "lib.rs");
        assert_eq!(t.app.panes.focused().tabs.len(), 1, "the split's empty buffer gave way");

        t.app.run(Command::GoBack);
        assert_eq!(t.app.panes.focus(), origin);
        assert_eq!((t.file().as_str(), t.caret()), ("main.rs", (1, 6)));

        // The pane beside is used again rather than split once more.
        t.app.definition_found(&[place], Open::Beside, "`helper`");
        assert_eq!(t.app.panes.len(), 2);
        assert_eq!(t.file(), "lib.rs");
    }

    #[test]
    fn a_jump_list_entry_whose_file_was_closed_reopens_it() {
        let mut t = Tester::new();
        t.caret_at(2, 4);
        t.app.definition_found(&[t.place("src/lib.rs", 1, 7)], Open::Here, "`x`");
        // Close main.rs, which is where Back goes.
        let main = t.app.panes.focused().tabs.iter().position(|id| {
            t.app.doc_by(*id).and_then(|doc| doc.buffer.path())
                == Some(t.path("src/main.rs").as_path())
        });
        t.app.close_tab(t.app.panes.focus(), main.unwrap());
        t.app.run(Command::GoBack);
        assert_eq!((t.file().as_str(), t.caret()), ("main.rs", (2, 4)));
    }

    #[test]
    fn with_no_server_the_keys_say_why_nothing_happened() {
        let mut t = Tester::new();
        t.app.run(Command::GoToDefinition);
        assert_eq!(
            t.app.message(),
            Some("This file has no language server to go to definitions with.")
        );
        t.app.run(Command::FindReferences);
        assert_eq!(
            t.app.message(),
            Some("This file has no language server to find references with.")
        );
    }

    // ── ctrl-hover and ctrl-click ───────────────────────────────────────────

    /// Put the underline on `helper` on line 1, as a server that answered
    /// would have.
    fn link_helper(t: &mut Tester, origin: Option<lsp::Range>) {
        let start = t.app.doc().buffer.line_start(1) + 4;
        let doc = t.app.doc().id;
        t.app.navigation.link =
            Some(Link { doc, word: start..start + 6, state: LinkState::Settling(Instant::now()) });
        let target = t.place("src/lib.rs", 1, 7);
        let link = LocationLink {
            origin_selection_range: origin,
            target_uri: uri(&target.path),
            target_range: target.range,
            target_selection_range: target.range,
        };
        t.app.link_found(Some(GotoDefinitionResponse::Link(vec![link])), Encoding::Utf16);
    }

    fn underlined(t: &Tester) -> String {
        let mut cells = Cells::empty(t.app.viewport);
        t.app.render(t.app.viewport, &mut cells);
        let (_, y) = t.text_origin();
        let row = y + 1;
        (0..t.app.viewport.width)
            .filter(|x| cells[(*x, row)].modifier.contains(Modifier::UNDERLINED))
            .map(|x| cells[(x, row)].symbol().to_string())
            .collect()
    }

    #[test]
    fn ctrl_hover_underlines_only_what_the_server_says_goes_somewhere() {
        let mut t = Tester::new();
        assert_eq!(underlined(&t), "");

        // Asked, and the answer was nothing: no promise to make.
        let start = t.app.doc().buffer.line_start(1) + 4;
        let doc = t.app.doc().id;
        t.app.navigation.link =
            Some(Link { doc, word: start..start + 6, state: LinkState::Settling(Instant::now()) });
        t.app.link_found(None, Encoding::Utf16);
        assert_eq!(underlined(&t), "");

        link_helper(&mut t, None);
        assert_eq!(underlined(&t), "helper");

        // Any key takes it away.
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Right)));
        assert_eq!(underlined(&t), "");
    }

    #[test]
    fn the_server_can_say_how_much_of_the_line_is_the_link() {
        let mut t = Tester::new();
        link_helper(&mut t, Some(lsp::Range { start: at(1, 4), end: at(1, 12) }));
        assert_eq!(underlined(&t), "helper()");
    }

    #[test]
    fn moving_off_the_word_or_letting_go_of_ctrl_takes_the_underline_away() {
        let mut t = Tester::new();
        link_helper(&mut t, None);
        let (x, y) = t.text_origin();
        let moved = |column, modifiers| {
            Event::Mouse(MouseEvent { kind: MouseEventKind::Moved, column, row: y + 1, modifiers })
        };
        // Along the same word, it stays.
        t.app.handle(moved(x + 6, KeyModifiers::CONTROL));
        assert_eq!(underlined(&t), "helper");
        t.app.handle(moved(x + 6, KeyModifiers::NONE));
        assert_eq!(underlined(&t), "");

        link_helper(&mut t, None);
        t.app.handle(moved(x + 1, KeyModifiers::CONTROL));
        assert_eq!(underlined(&t), "", "whitespace is not a symbol");
        assert!(t.app.navigation.link.is_none());
    }

    #[test]
    fn ctrl_click_on_an_underlined_symbol_goes_to_its_definition() {
        let mut t = Tester::new();
        link_helper(&mut t, None);
        let (x, y) = t.text_origin();
        t.click(x + 6, y + 1, KeyModifiers::CONTROL);
        assert_eq!((t.file().as_str(), t.caret()), ("lib.rs", (1, 7)));

        // Back comes back to the symbol that was clicked.
        t.app.run(Command::GoBack);
        assert_eq!((t.file().as_str(), t.caret()), ("main.rs", (1, 6)));
    }

    #[test]
    fn alt_ctrl_click_opens_it_beside() {
        let mut t = Tester::new();
        link_helper(&mut t, None);
        let (x, y) = t.text_origin();
        t.click(x + 6, y + 1, KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(t.app.panes.len(), 2);
        assert_eq!(t.file(), "lib.rs");
    }

    #[test]
    fn with_no_server_ctrl_click_is_the_click_it_always_was() {
        let mut t = Tester::new();
        let (x, y) = t.text_origin();
        t.click(x + 6, y + 1, KeyModifiers::CONTROL);
        assert_eq!((t.file().as_str(), t.caret()), ("main.rs", (1, 6)));
        assert_eq!(t.app.panes.len(), 1);
    }

    // ── references ──────────────────────────────────────────────────────────

    fn list_helper(t: &mut Tester) {
        let places = vec![
            t.place("src/main.rs", 1, 4),
            t.place("src/lib.rs", 1, 7),
            t.place("src/main.rs", 1, 4),
        ];
        t.app.references_found(places, "`helper`");
        t.settle();
    }

    fn rows(t: &Tester) -> Vec<String> {
        let listing = &t.app.navigation.listing;
        listing
            .view_rows(0..listing.rows.len())
            .into_iter()
            .map(|row| match row {
                SearchRow::File { path, hits, .. } => format!("[{path}] {hits}"),
                SearchRow::Hit { line, text, matched, .. } => {
                    let picked: String = text
                        .chars()
                        .skip(matched[0].start as usize)
                        .take(matched[0].len())
                        .collect();
                    format!("{line}: {} <{picked}>", text.trim())
                }
                SearchRow::After { .. } | SearchRow::Operation { .. } => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn references_are_listed_by_file_with_their_lines() {
        let mut t = Tester::new();
        list_helper(&mut t);
        assert_eq!(
            rows(&t),
            [
                "[src/lib.rs] 1",
                "2: pub fn helper() {} <helper>",
                "[src/main.rs] 1",
                "2: helper(); <helper>",
            ]
        );
        assert!(t.app.references_area().is_some(), "the panel has the sidebar");
        assert_eq!(t.app.navigation.listing.summary, "2 references to `helper` in 2 files");
    }

    #[test]
    fn clicking_a_reference_goes_there_and_a_file_folds() {
        let mut t = Tester::new();
        list_helper(&mut t);
        let area = t.app.references_area().unwrap();
        let rows_area = ReferencesView::rows_area(area);
        t.click(rows_area.x + 3, rows_area.y + 1, KeyModifiers::NONE);
        assert_eq!((t.file().as_str(), t.caret()), ("lib.rs", (1, 7)));

        t.click(rows_area.x + 3, rows_area.y, KeyModifiers::NONE);
        assert_eq!(rows(&t).len(), 3, "the file's references folded away");

        t.app.run(Command::GoBack);
        assert_eq!(t.file(), "main.rs");
    }

    #[test]
    fn the_keyboard_steps_through_the_references() {
        let mut t = Tester::new();
        list_helper(&mut t);
        t.app.run(Command::NextReference);
        assert_eq!(t.file(), "lib.rs");
        t.app.run(Command::NextReference);
        assert_eq!((t.file().as_str(), t.caret()), ("main.rs", (1, 4)));
        t.app.run(Command::NextReference);
        assert_eq!(t.file(), "lib.rs", "it wraps round");
        t.app.run(Command::PreviousReference);
        assert_eq!(t.file(), "main.rs");
    }

    #[test]
    fn the_back_button_gives_the_sidebar_back_to_the_tree() {
        let mut t = Tester::new();
        list_helper(&mut t);
        let area = t.app.references_area().unwrap();
        let back = ReferencesView::back_area(area).unwrap();
        t.click(back.x, back.y, KeyModifiers::NONE);
        assert!(t.app.references_area().is_none());
    }

    #[test]
    fn lines_read_for_an_older_listing_are_dropped() {
        let mut t = Tester::new();
        list_helper(&mut t);
        let stale = t.app.navigation.listing.generation - 1;
        t.app.reference_lines(stale, vec![(t.path("src/lib.rs"), vec![(1, "stale".into())])]);
        assert!(rows(&t)[1].contains("pub fn helper"));
    }

    #[test]
    fn the_text_menu_offers_the_way_back() {
        let mut t = Tester::new();
        t.app.definition_found(&[t.place("src/lib.rs", 1, 7)], Open::Here, "`x`");
        assert_eq!(t.app.navigation_offers(), [Command::GoBack]);
    }
}
