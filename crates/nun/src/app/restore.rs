//! Putting a folder back the way it was left, and keeping it written down.
//!
//! The format is [`crate::restore`]'s. Here is where the editor is turned
//! into it and back.
//!
//! What is written is a snapshot: small, taken on the main thread no more
//! than once every [`DEBOUNCE`] after something changed, compared with the
//! last one sent, and handed to a writer thread only if it differs. The disk
//! is never touched between keystrokes.
//!
//! Started with a file, in a folder that has a session, nun restores the
//! session and then goes to the file — its tab, if it was open, or a new one
//! beside the pane that had the keyboard. The file named is what was asked
//! for; the rest of the layout is what was there, and neither is lost to the
//! other.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nun_core::{Buffer, Range, Selections};
use nun_ui::{Dir, Layout};

use super::panes::{DocId, Pane, Panes};
use super::{App, Document, Outcome};
use crate::restore::{Folds, Job, Node, PaneState, State, TabState, VERSION, Writer};

/// How long after a change the session is written down.
pub(super) const DEBOUNCE: Duration = Duration::from_secs(1);

/// Keeping the session written down.
#[derive(Debug, Default)]
pub(super) struct Keeping {
    /// The writer thread. `None` keeps nothing: `--no-session`, and tests.
    writer: Option<Writer>,
    /// The folder, and the file its session goes in. `None` writes only the
    /// folds.
    target: Option<(PathBuf, PathBuf)>,
    /// When the next snapshot is due.
    deadline: Option<Instant>,
    /// What was last sent, so an unchanged session is not written again.
    sent: Option<State>,
    sent_folds: Option<Folds>,
    /// Panels as the session file had them, so kinds this build does not
    /// draw are carried through. The terminal's is replaced by what it is
    /// now.
    panels: BTreeMap<String, toml::Table>,
}

impl App {
    /// Keep this folder's session written down, in `file`, through `writer`.
    /// With no file only the folds are kept.
    pub fn keep_session(&mut self, root: PathBuf, file: Option<PathBuf>, writer: Writer) {
        self.keeping.target = file.map(|file| (root, file));
        self.keeping.writer = Some(writer);
    }

    /// Put back the panes, tabs, carets and scroll `state` describes, and
    /// the terminal panel's tabs and directories.
    ///
    /// Call it before the parser and the language servers are attached, which
    /// then open every document it restored. A file that has gone since is
    /// skipped and named in a notice. With nothing left to restore, the
    /// editor is left as it is.
    pub fn restore_session(&mut self, state: &State) {
        self.keeping.panels.clone_from(&state.panels);
        if let Some(table) = state.panels.get(super::panel::KIND) {
            self.restore_terminal(table);
        }
        let mut gone = Vec::new();
        let mut seen = BTreeSet::new();
        let mut restored: Vec<(Vec<Document>, usize)> = Vec::new();
        for pane in &state.panes {
            restored.push(self.restore_pane(pane, &mut gone, &mut seen));
        }
        if !gone.is_empty() {
            self.warn(gone_notice(&gone));
        }
        let Some(panes) = rebuild(&state.layout, state.focus, &restored) else { return };

        // The document nun was started with: the file named on the command
        // line, or the empty buffer beside a folder.
        let started = std::mem::take(&mut self.docs).into_iter().next();
        self.docs = restored.into_iter().flat_map(|(docs, _)| docs).collect();
        self.panes = panes;
        if let Some(started) = started.filter(|doc| doc.buffer.path().is_some()) {
            self.go_to_started(started);
        }
        self.relayout();
        self.follow_tab();
    }

    /// One pane's documents, loaded, and which of them was showing.
    fn restore_pane(
        &mut self,
        pane: &PaneState,
        gone: &mut Vec<PathBuf>,
        seen: &mut BTreeSet<PathBuf>,
    ) -> (Vec<Document>, usize) {
        let mut docs: Vec<Document> = Vec::new();
        let mut active = 0;
        for (index, tab) in pane.tabs.iter().enumerate() {
            // One file is one buffer: a session naming it twice is damaged,
            // and the second is dropped.
            if !seen.insert(tab.path.clone()) {
                continue;
            }
            let Some(document) = self.restore_tab(tab, gone) else { continue };
            if index <= pane.active {
                active = docs.len();
            }
            docs.push(document);
        }
        (docs, active)
    }

    /// One file, loaded, with its carets and scroll put back.
    fn restore_tab(&mut self, tab: &TabState, gone: &mut Vec<PathBuf>) -> Option<Document> {
        if !tab.path.is_file() {
            gone.push(tab.path.clone());
            return None;
        }
        let Ok((mut buffer, report)) = Buffer::load(&tab.path) else {
            gone.push(tab.path.clone());
            return None;
        };
        if report.lossy {
            self.warn(format!(
                "{} is not valid UTF-8. Saving it would destroy the original bytes.",
                super::display_path(Some(&tab.path))
            ));
        }
        if let Some(doc) = self.docs.first() {
            buffer.set_tab_width(doc.buffer.tab_width());
        }
        buffer.set_selections(selections(&buffer, &tab.selections, tab.primary));
        let scroll = tab.scroll.min(buffer.len_lines().saturating_sub(1));
        let id = self.next_doc;
        self.next_doc += 1;
        Some(Document { id, buffer, scroll, syntax: super::syntax::Highlighting::default() })
    }

    /// Show the file nun was started with: its restored tab, or a new one.
    fn go_to_started(&mut self, started: Document) {
        let path = started.buffer.path().map(Path::to_path_buf).unwrap_or_default();
        let open = self
            .docs
            .iter()
            .find(|doc| doc.buffer.path().is_some_and(|open| super::same_file(open, &path)))
            .and_then(|doc| self.panes.find(doc.id));
        if let Some((pane, index)) = open {
            self.panes.set_focus(pane);
            self.panes.focused_mut().active = index;
            return;
        }
        let id = started.id;
        self.docs.push(started);
        self.panes.open(id);
    }

    /// What would be written down now.
    fn snapshot(&self, root: &Path) -> State {
        let all = self.panes.all();
        let index_of = |id: usize| all.iter().position(|pane| pane.id == id).unwrap_or(0);
        let panes = all
            .iter()
            .map(|pane| {
                let named: Vec<(DocId, TabState)> = pane
                    .tabs
                    .iter()
                    .filter_map(|id| self.doc_by(*id))
                    .filter_map(|doc| Some((doc.id, tab_state(doc)?)))
                    .collect();
                let active = pane
                    .current()
                    .and_then(|id| named.iter().position(|(doc, _)| *doc == id))
                    .unwrap_or(0);
                PaneState { active, tabs: named.into_iter().map(|(_, tab)| tab).collect() }
            })
            .collect();
        State {
            version: VERSION,
            root: root.to_path_buf(),
            focus: index_of(self.panes.focus()),
            layout: node(self.panes.layout(), &index_of),
            panes,
            panels: self.panels_snapshot(),
        }
    }

    /// Each kind of panel's table: the terminal's as it is now, and the
    /// rest as they were read.
    fn panels_snapshot(&self) -> BTreeMap<String, toml::Table> {
        let mut tables = self.keeping.panels.clone();
        tables.remove(super::panel::KIND);
        if let Some(table) = self.terminal_snapshot() {
            tables.insert(super::panel::KIND.to_string(), table);
        }
        tables
    }

    /// Something changed: write the session down once things settle.
    pub(super) fn session_changed(&mut self, now: Instant) {
        if self.keeping.writer.is_some() && self.keeping.deadline.is_none() {
            self.keeping.deadline = Some(now + DEBOUNCE);
        }
    }

    /// When the session is next due to be written down.
    pub(super) const fn session_deadline(&self) -> Option<Instant> {
        self.keeping.deadline
    }

    /// Write the session down, if it is due.
    pub(super) fn session_tick(&mut self, now: Instant) -> Outcome {
        if self.keeping.deadline.is_some_and(|due| due <= now) {
            self.keeping.deadline = None;
            self.send_session();
        }
        Outcome::Continue
    }

    /// Hand what has changed since the last time to the writer. Nothing here
    /// asks the filesystem: the writer resolves the paths.
    fn send_session(&mut self) {
        if self.keeping.writer.is_none() {
            return;
        }
        let state = self.keeping.target.as_ref().and_then(|(root, file)| {
            let state = self.snapshot(root);
            (self.keeping.sent.as_ref() != Some(&state)).then(|| (file.clone(), state))
        });
        if let Some((_, state)) = &state {
            self.keeping.sent = Some(state.clone());
        }
        let open = self.docs.iter().filter_map(|doc| self.folds_to_remember(doc.id)).collect();
        let folds: Folds = (self.session.clone(), open);
        let folds = (self.keeping.sent_folds.as_ref() != Some(&folds)).then_some(folds);
        if let Some(folds) = &folds {
            self.keeping.sent_folds = Some(folds.clone());
        }
        if (state.is_some() || folds.is_some())
            && let Some(writer) = &self.keeping.writer
        {
            writer.send(Job { state, folds });
        }
    }

    /// Write down what has been remembered, for the next session, and wait
    /// for it to be written.
    ///
    /// # Errors
    ///
    /// Whatever writing the files ran into.
    pub fn save_session(&mut self) -> std::io::Result<()> {
        self.keeping.deadline = None;
        self.send_session();
        if let Some(writer) = self.keeping.writer.take() {
            return writer.finish();
        }
        self.remember_folds();
        self.session.save()
    }
}

/// The tab a document is written down as. None for one with no name, which
/// has nothing to reopen.
fn tab_state(doc: &Document) -> Option<TabState> {
    // As it was opened; the writer spells it out in full.
    let path = doc.buffer.path()?.to_path_buf();
    let buffer = &doc.buffer;
    let place = |at: usize| {
        let line = buffer.line_of(at);
        (line, at - buffer.line_start(line))
    };
    let selections = buffer
        .selections()
        .ranges()
        .iter()
        .map(|range| {
            let (anchor, head) = (place(range.anchor), place(range.head));
            [anchor.0, anchor.1, head.0, head.1]
        })
        .collect();
    Some(TabState {
        path,
        scroll: doc.scroll,
        selections,
        primary: buffer.selections().primary_index(),
    })
}

/// The selections written down, in `buffer` as it is now. A line past the end
/// is the last line, a column past the end of its line is the end of it, and
/// a column inside a grapheme is the start of that grapheme.
fn selections(buffer: &Buffer, written: &[[usize; 4]], primary: usize) -> Selections {
    let at = |line: usize, column: usize| {
        let line = line.min(buffer.len_lines().saturating_sub(1));
        let start = buffer.line_start(line);
        let wanted = start.saturating_add(column).min(buffer.line_end(line));
        if wanted == start {
            return start;
        }
        // One step back and one forward, whatever the column: walking the
        // line from its start would cost a minified file's whole length.
        // Past the start, so the step back cannot cross a line break.
        let before = buffer.prev_grapheme(wanted);
        if buffer.next_grapheme(before) == wanted { wanted } else { before }
    };
    let ranges: Vec<Range> = written
        .iter()
        .map(|&[anchor_line, anchor_column, head_line, head_column]| {
            Range::new(at(anchor_line, anchor_column), at(head_line, head_column))
        })
        .collect();
    if ranges.is_empty() {
        return Selections::default();
    }
    let primary = primary.min(ranges.len() - 1);
    Selections::new(ranges, primary)
}

/// The layout as written down, panes named by their place in the list.
fn node(layout: &Layout, index_of: &impl Fn(usize) -> usize) -> Node {
    match layout {
        Layout::Pane(id) => Node::Pane(index_of(*id)),
        Layout::Split { dir, ratio, first, second } => Node::Split {
            beside: *dir == Dir::Beside,
            ratio: *ratio,
            first: Box::new(node(first, index_of)),
            second: Box::new(node(second, index_of)),
        },
    }
}

/// The layout written down, with pane ids that are their places in the list.
fn layout(node: &Node) -> Layout {
    match node {
        Node::Pane(pane) => Layout::Pane(*pane),
        Node::Split { beside, ratio, first, second } => Layout::Split {
            dir: if *beside { Dir::Beside } else { Dir::Below },
            ratio: (*ratio).clamp(1, 999),
            first: Box::new(layout(first)),
            second: Box::new(layout(second)),
        },
    }
}

/// The panes, from the layout and what was restored into each. A pane
/// nothing could be restored into is closed, and its neighbour takes the
/// space. A layout that does not name each pane exactly once is not trusted:
/// everything goes in one pane instead. `None` when nothing was restored.
fn rebuild(written: &Node, focus: usize, restored: &[(Vec<Document>, usize)]) -> Option<Panes> {
    if restored.iter().all(|(docs, _)| docs.is_empty()) {
        return None;
    }
    let named = written.panes();
    let mut sorted = named.clone();
    sorted.sort_unstable();
    let trusted = sorted.iter().copied().eq(0..restored.len());

    let pane = |id: usize, tabs: Vec<DocId>, active: usize| Pane { id, tabs, active, scroll: 0 };
    if !trusted {
        let tabs = restored.iter().flat_map(|(docs, _)| docs.iter().map(|doc| doc.id)).collect();
        return Some(Panes::restored(Layout::single(0), vec![pane(0, tabs, 0)], 0));
    }
    let mut tree = layout(written);
    let mut list = Vec::new();
    for (id, (docs, active)) in restored.iter().enumerate() {
        if docs.is_empty() {
            tree.close(id);
        } else {
            list.push(pane(id, docs.iter().map(|doc| doc.id).collect(), *active));
        }
    }
    Some(Panes::restored(tree, list, focus))
}

/// What to say about files that were open last time and are not now.
fn gone_notice(gone: &[PathBuf]) -> String {
    let names: Vec<String> = gone.iter().map(|path| super::display_path(Some(path))).collect();
    match names.as_slice() {
        [one] => format!("{one} was open last time and is gone, so it was not reopened."),
        many => format!(
            "{} files open last time are gone, so they were not reopened: {}.",
            many.len(),
            many.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, KeySet, defaults};
    use crate::restore::Loaded;
    use nun_theme::{Probe, derive};
    use nun_ui::Palette;
    use ratatui::layout::Rect;
    use std::fs;
    use tempfile::TempDir;

    /// A folder holding `files`, each with the text given.
    fn folder(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    /// An editor started the way `main` starts one: on `buffer`.
    fn editor(buffer: Buffer) -> App {
        let mut app =
            App::new(buffer, Palette::new(derive(&Probe::builtin_dark())), defaults(KeySet::Full));
        app.set_viewport(Rect::new(0, 0, 80, 20));
        app
    }

    /// An editor started on a folder, with `names` opened in it.
    fn opened(dir: &TempDir, names: &[&str]) -> App {
        let mut app = editor(Buffer::new());
        for name in names {
            app.open_in_tab(&dir.path().join(name));
        }
        app
    }

    /// Through the text and back, the way it goes through the disk.
    fn round_trip(app: &App, root: &Path) -> State {
        let text = crate::restore::to_text(&app.snapshot(root)).unwrap();
        match crate::restore::parse(&text, root) {
            Loaded::Found(state) => state,
            other => panic!("{other:?} from\n{text}"),
        }
    }

    fn select(app: &mut App, ranges: &[(usize, usize)], primary: usize) {
        let ranges = ranges.iter().map(|&(anchor, head)| Range::new(anchor, head)).collect();
        app.doc_mut().buffer.set_selections(Selections::new(ranges, primary));
    }

    fn paths(app: &App) -> Vec<Vec<String>> {
        app.panes
            .all()
            .iter()
            .map(|pane| {
                pane.tabs
                    .iter()
                    .filter_map(|id| app.doc_by(*id)?.buffer.path())
                    .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_layout_and_the_carets_come_back_exactly() {
        let text = "fn a() {}\n// é 👩‍👩‍👧 中文 x\nlet x = 1;\n".repeat(20);
        let dir = folder(&[("a.rs", &text), ("b.rs", &text), ("c.rs", &text), ("d.rs", &text)]);
        let mut app = opened(&dir, &["a.rs", "b.rs"]);
        // Split below, then beside that, and drag the first divider.
        app.run(Command::SplitBelow);
        app.open_in_tab(&dir.path().join("c.rs"));
        app.run(Command::SplitBeside);
        app.open_in_tab(&dir.path().join("d.rs"));
        if let Layout::Split { ratio, .. } = app.panes.layout_mut() {
            *ratio = 317;
        }
        // Several carets, a selection running backwards, past the emoji.
        let line = app.doc().buffer.line_start(21);
        select(&mut app, &[(line + 14, line + 3), (line + 20, line + 20)], 1);
        app.doc_mut().scroll = 17;
        let root = dir.path().canonicalize().unwrap();
        let before = app.snapshot(&root);

        let mut again = editor(Buffer::new());
        again.restore_session(&round_trip(&app, &root));
        assert_eq!(again.snapshot(&root), before);
        assert_eq!(paths(&again), paths(&app));
        assert_eq!(again.panes.layout(), app.panes.layout(), "ratios and all");
        assert_eq!(again.doc().buffer.selections(), app.doc().buffer.selections());
        assert_eq!(again.doc().scroll, 17);
        assert_eq!(
            again.doc().buffer.path().map(|path| path.file_name().unwrap()),
            Some("d.rs".as_ref()),
            "the pane with the keyboard, and the tab showing in it"
        );
    }

    #[test]
    fn a_file_deleted_since_is_skipped_with_a_notice() {
        let dir = folder(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("c.rs", "c\n")]);
        let mut app = opened(&dir, &["a.rs", "b.rs"]);
        app.run(Command::SplitBeside);
        app.open_in_tab(&dir.path().join("c.rs"));
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);
        fs::remove_file(dir.path().join("b.rs")).unwrap();
        fs::remove_file(dir.path().join("c.rs")).unwrap();

        let mut again = editor(Buffer::new());
        again.restore_session(&state);
        assert_eq!(paths(&again), [["a.rs"]], "and the pane left empty closed");
        let notice = again.shown_message().unwrap();
        assert!(notice.contains("2 files") && notice.contains("b.rs"), "{notice}");
    }

    #[test]
    fn carets_in_a_file_that_got_shorter_are_clamped() {
        let dir = folder(&[("a.rs", "one\ntwo\nthree\nfour\n")]);
        let mut app = opened(&dir, &["a.rs"]);
        let far = app.doc().buffer.line_start(3) + 3;
        select(&mut app, &[(2, far)], 0);
        app.doc_mut().scroll = 3;
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);
        fs::write(dir.path().join("a.rs"), "one\ntw").unwrap();

        let mut again = editor(Buffer::new());
        again.restore_session(&state);
        let buffer = &again.doc().buffer;
        assert_eq!(buffer.selections().primary(), Range::new(2, 6), "to the end of the last line");
        assert_eq!(again.doc().scroll, 1);
    }

    #[test]
    fn a_column_inside_a_grapheme_lands_before_it() {
        // `e` and a combining acute are two chars and one grapheme.
        let dir = folder(&[("a.rs", "xe\u{301}y\n")]);
        let (buffer, _) = Buffer::load(dir.path().join("a.rs")).unwrap();
        assert_eq!(selections(&buffer, &[[0, 2, 0, 2]], 0).primary(), Range::caret(1));
        assert_eq!(selections(&buffer, &[[0, 3, 0, 3]], 0).primary(), Range::caret(3));
        assert_eq!(selections(&buffer, &[[9, 9, 9, 9]], 0).primary(), Range::caret(5));
        assert_eq!(selections(&buffer, &[], 7), Selections::default());
    }

    #[test]
    fn carets_in_a_file_that_became_empty_go_to_the_top() {
        let dir = folder(&[("a.rs", "one\ntwo\n")]);
        let mut app = opened(&dir, &["a.rs"]);
        select(&mut app, &[(5, 6)], 0);
        app.doc_mut().scroll = 1;
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);
        fs::write(dir.path().join("a.rs"), "").unwrap();

        let mut again = editor(Buffer::new());
        again.restore_session(&state);
        assert_eq!(again.doc().buffer.selections().primary(), Range::caret(0));
        assert_eq!(again.doc().scroll, 0);
    }

    #[test]
    fn columns_are_the_same_in_crlf_and_after_a_bom() {
        for text in ["ab\r\ncd\r\nef\r\n", "\u{feff}ab\ncd\nef\n"] {
            let dir = folder(&[("a.rs", text)]);
            let mut app = opened(&dir, &["a.rs"]);
            let line = app.doc().buffer.line_start(1);
            select(&mut app, &[(line + 1, line + 2)], 0);
            let root = dir.path().canonicalize().unwrap();
            let state = round_trip(&app, &root);
            assert_eq!(state.panes[0].tabs[0].selections, [[1, 1, 1, 2]], "{text:?}");
            let mut again = editor(Buffer::new());
            again.restore_session(&state);
            assert_eq!(again.doc().buffer.selections().primary(), Range::new(4, 5), "{text:?}");
        }
    }

    #[test]
    fn a_column_between_the_halves_of_a_flag_lands_before_it() {
        let dir = folder(&[("a.rs", "\u{1f1ee}\u{1f1f8}\u{1f1ee}\u{1f1f8}\n")]);
        let (buffer, _) = Buffer::load(dir.path().join("a.rs")).unwrap();
        assert_eq!(selections(&buffer, &[[0, 1, 0, 3]], 0).primary(), Range::new(0, 2));
        assert_eq!(selections(&buffer, &[[0, 4, 0, usize::MAX]], 0).primary(), Range::caret(4));
    }

    #[test]
    fn a_file_named_at_startup_is_gone_to_when_it_was_open() {
        let dir = folder(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("c.rs", "c\n")]);
        let mut app = opened(&dir, &["a.rs", "b.rs"]);
        app.run(Command::SplitBeside);
        app.open_in_tab(&dir.path().join("c.rs"));
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);

        // Relative, as typed: still the same file.
        let (buffer, _) = Buffer::load(dir.path().join("./a.rs")).unwrap();
        let mut again = editor(buffer);
        again.restore_session(&state);
        assert_eq!(paths(&again), [vec!["a.rs", "b.rs"], vec!["c.rs"]], "not opened twice");
        assert!(again.doc().buffer.path().unwrap().ends_with("a.rs"));
    }

    #[test]
    fn a_file_named_at_startup_that_was_not_open_opens_beside_the_rest() {
        let dir = folder(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("new.rs", "new\n")]);
        let app = opened(&dir, &["a.rs", "b.rs"]);
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);

        let (buffer, _) = Buffer::load(dir.path().join("new.rs")).unwrap();
        let mut again = editor(buffer);
        again.restore_session(&state);
        assert_eq!(paths(&again), [["a.rs", "b.rs", "new.rs"]]);
        assert!(again.doc().buffer.path().unwrap().ends_with("new.rs"));
    }

    #[test]
    fn nothing_to_restore_leaves_the_editor_as_it_started() {
        let dir = folder(&[("a.rs", "a\n")]);
        let app = opened(&dir, &["a.rs"]);
        let root = dir.path().canonicalize().unwrap();
        let state = round_trip(&app, &root);
        fs::remove_file(dir.path().join("a.rs")).unwrap();

        let mut again = editor(Buffer::from_text("scratch"));
        again.restore_session(&state);
        assert_eq!(again.doc().buffer.text().to_string(), "scratch");
    }

    #[test]
    fn a_layout_that_does_not_match_its_panes_puts_everything_in_one() {
        let dir = folder(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
        let mut app = opened(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);
        app.open_in_tab(&dir.path().join("b.rs"));
        let root = dir.path().canonicalize().unwrap();
        let mut state = round_trip(&app, &root);
        state.layout = Node::try_from("beside 500 (0, 0)".to_string()).unwrap();

        let mut again = editor(Buffer::new());
        again.restore_session(&state);
        assert_eq!(paths(&again), [["a.rs", "b.rs"]]);
    }

    #[test]
    fn the_terminal_panel_comes_back_even_when_no_file_does() {
        let dir = folder(&[("a.rs", "a\n")]);
        let root = dir.path().canonicalize().unwrap();
        let mut state = round_trip(&opened(&dir, &["a.rs"]), &root);
        assert!(state.panels.is_empty(), "no terminal, no table: {state:?}");
        fs::remove_file(dir.path().join("a.rs")).unwrap();
        let text = format!(
            "visible = true\nactive = 0\n[[tabs]]\nfocus = 0\ndirs = [{:?}]\n",
            root.to_str().unwrap()
        );
        state.panels.insert("terminal".into(), toml::from_str(&text).unwrap());
        state.panels.insert("future".into(), toml::from_str("x = 1").unwrap());

        let mut again = editor(Buffer::new());
        again.restore_session(&state);
        // Kept until shells can be started, and written down the same.
        assert_eq!(again.snapshot(&root).panels, state.panels);
    }

    #[test]
    fn untitled_buffers_are_not_written_down() {
        let dir = folder(&[("a.rs", "a\n")]);
        let mut app = opened(&dir, &["a.rs"]);
        app.run(Command::SplitBeside);
        let state = app.snapshot(dir.path());
        assert_eq!(state.panes.len(), 2);
        assert!(state.panes[1].tabs.is_empty(), "{state:?}");
    }

    #[test]
    fn the_session_is_written_after_things_settle_and_on_the_way_out() {
        let dir = folder(&[("a.rs", "one\ntwo\n")]);
        let root = dir.path().canonicalize().unwrap();
        let file = dir.path().join("state").join("here.toml");
        let mut app = opened(&dir, &["a.rs"]);
        app.keep_session(root.clone(), Some(file.clone()), Writer::start().unwrap());

        let now = Instant::now();
        app.session_changed(now);
        assert_eq!(app.deadline(), Some(now + DEBOUNCE));
        app.session_changed(now + DEBOUNCE / 2);
        assert_eq!(app.deadline(), Some(now + DEBOUNCE), "not put off by every change");
        app.tick(now + DEBOUNCE);
        assert_eq!(app.session_deadline(), None);

        select(&mut app, &[(5, 5)], 0);
        app.save_session().unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let Loaded::Found(state) = crate::restore::parse(&text, &root) else { panic!("{text}") };
        assert_eq!(state.panes[0].tabs[0].selections, [[1, 1, 1, 1]]);
    }
}
