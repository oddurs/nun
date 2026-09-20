//! Searching the project, and the panel that shows what it found.
//!
//! The search itself is [`nun_workspace::Grep`], on a thread of its own. Hits
//! stream back while the walk is still going and the panel grows as they
//! arrive, because the result someone wanted is usually among the first few
//! and making them wait for the last file in the repository would waste that.
//!
//! Typing always goes to the query. A panel where the arrow keys might mean
//! the text field or might mean the list is one you have to look at to use, so
//! up and down move the selection and left and right move the caret, and
//! neither ever changes its mind.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_ui::{SearchButton, SearchRow, SearchView, Toggles};
use nun_workspace::{Case, Found, Grep, Hit, Options};
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use super::{App, Focus, Outcome, SidebarView, Target};

/// How long after a keystroke the project is searched.
///
/// Much longer than the parser's debounce, because this one walks a directory
/// tree and reads every file in it: a burst of typing should cost one walk,
/// not one per character.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(150);

/// One file's hits, in the order the engine found them.
#[derive(Debug)]
pub(super) struct Group {
    /// The file, relative to the root that was searched.
    path: PathBuf,
    /// How it reads in the panel.
    label: String,
    /// The lines of it that matched.
    hits: Vec<Hit>,
}

/// What a row of the results list stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Line {
    /// The file at this index in `groups`.
    File(usize),
    /// A matching line: which group, and which of its hits.
    Hit(usize, usize),
}

/// The panel, and the search behind it.
#[derive(Debug, Default)]
pub(super) struct Search {
    /// The worker, once a search has been asked for. Dropping it stops the
    /// walk, which is why it outlives any one search.
    grep: Option<Grep>,
    /// What has been typed.
    pub(super) query: String,
    /// Where the caret sits in it, as a char offset.
    pub(super) caret: usize,
    /// Which of the toggles are lit.
    pub(super) toggles: Toggles,
    /// The files that have hits, in the order they were found.
    groups: Vec<Group>,
    /// Where each file sits in `groups`, so a hit finds its own group without
    /// a scan.
    index: HashMap<PathBuf, usize>,
    /// Files whose hits are folded away.
    collapsed: BTreeSet<PathBuf>,
    /// The rows as drawn: rebuilt when the results or the folding change,
    /// rather than on every frame.
    rows: Vec<Line>,
    pub(super) scroll: usize,
    pub(super) selected: Option<usize>,
    /// Which search the rows on screen belong to. An answer to an older one
    /// is somebody else's news.
    generation: u64,
    /// Whether a search is still running, for the summary line.
    running: bool,
    /// How many lines matched, and in how many files, once it finished.
    tally: Option<(usize, usize)>,
    /// What the engine said instead, when the query was not a valid regex.
    error: Option<String>,
    /// The summary line, rendered once when it changes rather than per frame.
    summary: Option<String>,
}

impl Search {
    /// The rows the panel draws, borrowed from the results.
    fn view_rows(&self) -> Vec<SearchRow<'_>> {
        self.rows
            .iter()
            .map(|line| match *line {
                Line::File(group) => {
                    let group = &self.groups[group];
                    SearchRow::File {
                        path: &group.label,
                        hits: group.hits.len(),
                        collapsed: self.collapsed.contains(&group.path),
                    }
                }
                Line::Hit(group, hit) => {
                    let hit = &self.groups[group].hits[hit];
                    SearchRow::Hit { line: hit.line, text: &hit.text, matched: &hit.matched }
                }
            })
            .collect()
    }

    /// Rebuild the row list from the groups and what is folded away.
    fn relist(&mut self) {
        self.rows.clear();
        for (at, group) in self.groups.iter().enumerate() {
            self.rows.push(Line::File(at));
            if self.collapsed.contains(&group.path) {
                continue;
            }
            self.rows.extend((0..group.hits.len()).map(|hit| Line::Hit(at, hit)));
        }
    }

    /// Say where the search has got to, in the words a panel uses.
    fn resummarise(&mut self) {
        self.summary = if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.running {
            let so_far: usize = self.groups.iter().map(|group| group.hits.len()).sum();
            Some(match so_far {
                0 => "Searching…".to_string(),
                found => format!("Searching… {found} so far"),
            })
        } else {
            self.tally.map(|(hits, files)| match (hits, files) {
                (0, _) => "No results".to_string(),
                (1, 1) => "1 result in 1 file".to_string(),
                (hits, 1) => format!("{hits} results in 1 file"),
                (hits, files) => format!("{hits} results in {files} files"),
            })
        };
    }

    /// Throw away what the last search found.
    fn forget(&mut self) {
        self.groups.clear();
        self.index.clear();
        self.rows.clear();
        self.scroll = 0;
        self.selected = None;
        self.tally = None;
        self.error = None;
    }

    /// What to ask the engine for, as the toggles currently stand.
    fn options(&self) -> Options {
        Options {
            query: self.query.clone(),
            regex: self.toggles.regex,
            // Smart case until the user says otherwise: it is right nearly
            // always, and the toggle is there for when it is not.
            case: if self.toggles.case { Case::Sensitive } else { Case::Smart },
            whole_word: self.toggles.word,
            include_ignored: self.toggles.ignored,
        }
    }
}

impl App {
    /// Search this project through `grep` from now on.
    ///
    /// Attached rather than built here so the worker can report into the same
    /// channel as the keyboard, which only the caller that owns it can do.
    pub fn attach_search(&mut self, grep: Grep) {
        self.search.grep = Some(grep);
    }

    /// Show the search panel and put the keyboard in its query.
    pub(super) fn open_search(&mut self) -> Outcome {
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.visible = true;
        } else {
            // With no folder open there is no project to search.
            self.warn("Open a folder to search it.".to_string());
            return Outcome::Redraw;
        }
        self.sidebar_view = SidebarView::Search;
        self.focus = Focus::Search;
        // Reopening with a query already there selects it in the sense that
        // typing replaces nothing but the caret is at the end, ready to refine.
        self.search.caret = self.search.query.chars().count();
        self.relayout();
        Outcome::Redraw
    }

    /// Swap the sidebar back to the file tree.
    pub(super) fn show_file_tree(&mut self) -> Outcome {
        self.sidebar_view = SidebarView::Files;
        if self.focus == Focus::Search {
            self.focus = Focus::Sidebar;
        }
        // The walk is worth nothing to a panel nobody can see.
        self.cancel_search();
        self.search.running = false;
        self.search.resummarise();
        self.relayout();
        Outcome::Redraw
    }

    /// Whether the sidebar is showing the search panel.
    pub(super) const fn searching(&self) -> bool {
        matches!(self.sidebar_view, SidebarView::Search)
    }

    /// Where the panel goes: the sidebar, less its divider.
    pub(super) fn search_area(&self) -> Option<Rect> {
        self.searching().then(|| self.tree_area()).flatten()
    }

    /// Lay out the panel's hit regions, from the same geometry it draws with.
    pub(super) fn layout_search(&self, hits: &mut nun_input::HitMap<Target>) {
        let Some(area) = self.search_area() else { return };
        hits.push(super::cells(SearchView::header_area(area)), Target::SearchHeader, false);
        if let Some(cell) = SearchView::back_area(area) {
            hits.push(super::cells(cell), Target::SearchBack, true);
        }
        hits.push(super::cells(SearchView::query_area(area)), Target::SearchQuery, true);
        for button in SearchButton::ALL {
            if let Some(cell) = SearchView::button_area(area, button) {
                hits.push(super::cells(cell), Target::SearchButton(button), true);
            }
        }

        let rows = SearchView::rows_area(area);
        hits.push(super::cells(rows), Target::SearchEmpty, false);
        let shown = SearchView::visible_rows(area);
        for offset in 0..shown {
            let index = self.search.scroll + offset;
            if index >= self.search.rows.len() {
                break;
            }
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows.y + offset, height: 1, ..rows };
            hits.push(super::cells(line), Target::SearchRow(index), true);
        }
    }

    /// Draw it.
    pub(super) fn render_search(&self, cells: &mut ratatui::buffer::Buffer) {
        let Some(area) = self.search_area() else { return };
        let (hovered, button) = match self.hover.current() {
            Some(Target::SearchRow(row)) => (Some(row), None),
            Some(Target::SearchButton(button)) => (None, Some(button)),
            _ => (None, None),
        };
        let hint = self.hover.current().and_then(|target| self.search_hint(target));
        let rows = self.search.view_rows();
        SearchView::new(&self.search.query, &rows, &self.palette)
            .scrolled_to(self.search.scroll)
            .selected(self.search.selected)
            .hovered(hovered)
            .hovered_button(button)
            .hovered_back(self.hover.current() == Some(Target::SearchBack))
            .toggles(self.search.toggles)
            .focused(self.focus == Focus::Search)
            .editing(self.focus == Focus::Search, self.search.caret)
            // Four one-cell marks are not self-explanatory, and the summary
            // line is already the panel's transient status: while the pointer
            // is over a toggle it says what that toggle does instead of how
            // the search went.
            .summary(hint.or(self.search.summary.as_deref()))
            .render(area, cells);
    }

    /// A key while the panel has the keyboard.
    pub(super) fn search_key(&mut self, key: &KeyEvent, now: Instant) -> Outcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);

        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                return Outcome::Redraw;
            }
            KeyCode::Enter => return self.open_selected_hit(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-self.search_page()),
            KeyCode::PageDown => self.move_selection(self.search_page()),
            KeyCode::Left => {
                self.search.caret = self.search.caret.saturating_sub(1);
            }
            KeyCode::Right => {
                let end = self.search.query.chars().count();
                self.search.caret = (self.search.caret + 1).min(end);
            }
            KeyCode::Home => self.search.caret = 0,
            KeyCode::End => self.search.caret = self.search.query.chars().count(),
            KeyCode::Backspace => {
                if self.search.caret > 0 {
                    let at = self.search.caret - 1;
                    self.remove_char(at);
                    self.search.caret = at;
                    self.restart_search(now);
                }
            }
            KeyCode::Delete => {
                if self.search.caret < self.search.query.chars().count() {
                    let at = self.search.caret;
                    self.remove_char(at);
                    self.restart_search(now);
                }
            }
            KeyCode::Char(ch) if !control => {
                let at = byte_of(&self.search.query, self.search.caret);
                self.search.query.insert(at, ch);
                self.search.caret += 1;
                self.restart_search(now);
            }
            _ => return Outcome::Continue,
        }
        Outcome::Redraw
    }

    /// Take the char at `at` out of the query.
    fn remove_char(&mut self, at: usize) {
        let from = byte_of(&self.search.query, at);
        let to = byte_of(&self.search.query, at + 1);
        self.search.query.replace_range(from..to, "");
    }

    /// How far a page key moves, in rows.
    fn search_page(&self) -> isize {
        let rows = self.search_area().map_or(1, SearchView::visible_rows);
        isize::try_from(rows.max(1)).unwrap_or(1)
    }

    /// Move the selection by `delta` rows, stopping at either end.
    fn move_selection(&mut self, delta: isize) {
        let rows = self.search.rows.len();
        if rows == 0 {
            self.search.selected = None;
            return;
        }
        let at = match self.search.selected {
            Some(at) => isize::try_from(at).unwrap_or(0).saturating_add(delta),
            // The first move from nothing selected lands on an end, so both
            // arrows do something sensible before anything is selected.
            None if delta < 0 => isize::try_from(rows).unwrap_or(1) - 1,
            None => 0,
        };
        let last = isize::try_from(rows).unwrap_or(1) - 1;
        let at = usize::try_from(at.clamp(0, last)).unwrap_or(0);
        self.search.selected = Some(at);
        self.follow_search_selection();
    }

    /// Keep the selected row on screen.
    fn follow_search_selection(&mut self) {
        let Some(at) = self.search.selected else { return };
        let visible = self.search_area().map_or(0, SearchView::visible_rows);
        if visible == 0 {
            return;
        }
        if at < self.search.scroll {
            self.search.scroll = at;
        } else if at >= self.search.scroll + visible {
            self.search.scroll = at + 1 - visible;
        }
    }

    /// The wheel over the panel.
    pub(super) fn search_scroll(&mut self, down: bool) -> Outcome {
        let visible = self.search_area().map_or(0, SearchView::visible_rows);
        let last = self.search.rows.len().saturating_sub(visible);
        self.search.scroll = if down {
            (self.search.scroll + 3).min(last)
        } else {
            self.search.scroll.saturating_sub(3)
        };
        Outcome::Redraw
    }

    /// A click in the panel.
    pub(super) fn search_press(&mut self, target: Target, column: u16, now: Instant) -> Outcome {
        match target {
            Target::SearchBack => return self.show_file_tree(),
            Target::SearchQuery => {
                self.focus = Focus::Search;
                if let Some(area) = self.search_area() {
                    self.search.caret =
                        SearchView::caret_at(area, &self.search.query, self.search.caret, column);
                }
            }
            Target::SearchButton(button) => {
                self.focus = Focus::Search;
                self.flip(button);
                self.restart_search(now);
            }
            Target::SearchHeader | Target::SearchEmpty => self.focus = Focus::Search,
            Target::SearchRow(at) => {
                self.focus = Focus::Search;
                self.search.selected = Some(at);
                return self.pick_search_row(at);
            }
            _ => return Outcome::Continue,
        }
        Outcome::Redraw
    }

    /// Flip one toggle.
    fn flip(&mut self, button: SearchButton) {
        let toggles = &mut self.search.toggles;
        match button {
            SearchButton::Regex => toggles.regex = !toggles.regex,
            SearchButton::Case => toggles.case = !toggles.case,
            SearchButton::Word => toggles.word = !toggles.word,
            SearchButton::Ignored => toggles.ignored = !toggles.ignored,
        }
    }

    /// Open whatever the selected row points at.
    fn open_selected_hit(&mut self) -> Outcome {
        match self.search.selected {
            Some(at) => self.pick_search_row(at),
            None => Outcome::Continue,
        }
    }

    /// Act on one row: a file folds, a hit opens.
    fn pick_search_row(&mut self, at: usize) -> Outcome {
        let Some(line) = self.search.rows.get(at).copied() else { return Outcome::Continue };
        match line {
            Line::File(group) => {
                let path = self.search.groups[group].path.clone();
                if !self.search.collapsed.remove(&path) {
                    self.search.collapsed.insert(path);
                }
                self.search.relist();
                // Folding can leave the selection past the end of the list.
                let last = self.search.rows.len().saturating_sub(1);
                self.search.selected = self.search.selected.map(|at| at.min(last));
                self.follow_search_selection();
                Outcome::Redraw
            }
            Line::Hit(group, hit) => {
                let Some(root) = self.workspace_root() else { return Outcome::Continue };
                let group = &self.search.groups[group];
                let Some(hit) = group.hits.get(hit) else { return Outcome::Continue };
                let (line, column) = (hit.line, hit.column);
                let path = root.join(&group.path);
                self.open_file(&path);
                self.place_caret_at(line, column);
                // The panel keeps the keyboard: finding one hit usually means
                // looking at the next one too.
                Outcome::Redraw
            }
        }
    }

    /// Put the caret on a one-based line and column of the open document.
    fn place_caret_at(&mut self, line: u32, column: u32) {
        let line = usize::try_from(line).unwrap_or(1).saturating_sub(1);
        let column = usize::try_from(column).unwrap_or(1).saturating_sub(1);
        let buffer = &self.doc().buffer;
        let line = line.min(buffer.len_lines().saturating_sub(1));
        // Clamped to the line: a hit reported against a file that has since
        // been edited should still land somewhere on the right line.
        let start = buffer.line_start(line);
        let end = buffer.line_end(line);
        let at = start.saturating_add(column).min(end);
        self.doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
        self.follow_caret();
    }

    /// The query changed, or a toggle did: the old answer is worthless.
    fn restart_search(&mut self, now: Instant) {
        self.search.forget();
        self.search.running = !self.search.query.is_empty();
        self.search.resummarise();
        // Nothing is asked for until the typing stops, but what is on screen
        // goes now: showing hits for a query nobody is looking at any more is
        // worse than showing none.
        self.search_deadline = Some(now + DEBOUNCE);
        self.cancel_search();
    }

    /// Stop whatever is in flight.
    ///
    /// Cancelling advances the engine's own idea of the newest generation, so
    /// ours has to move with it: the engine runs a search only when the two
    /// agree exactly, and a counter left behind would make the next search
    /// stale before it started.
    fn cancel_search(&mut self) {
        if let Some(grep) = self.search.grep.as_ref() {
            grep.cancel();
            self.search.generation += 1;
        }
    }

    /// When the search is next due to start.
    pub(super) const fn search_deadline(&self) -> Option<Instant> {
        self.search_deadline
    }

    /// The debounce fell due: start the walk.
    pub(super) fn search_tick(&mut self, now: Instant) -> Outcome {
        let Some(deadline) = self.search_deadline else { return Outcome::Continue };
        if now < deadline {
            return Outcome::Continue;
        }
        self.search_deadline = None;

        let Some(root) = self.workspace_root() else { return Outcome::Continue };
        if self.search.query.is_empty() {
            self.search.running = false;
            self.search.resummarise();
            return Outcome::Redraw;
        }

        // Without a worker there is nothing to ask. That is the case in the
        // tests that drive the panel without a thread behind it, and in a
        // terminal it is attached before the first frame.
        let Some(grep) = self.search.grep.as_ref() else { return Outcome::Continue };
        self.search.generation += 1;
        grep.search(root, self.search.options(), self.search.generation);
        Outcome::Continue
    }

    /// The engine has something to say.
    pub(super) fn search_found(&mut self, found: Found) -> Outcome {
        let generation = match &found {
            Found::Hits { generation, .. }
            | Found::Done { generation, .. }
            | Found::Failed { generation, .. } => *generation,
        };
        // An answer to a query the user has typed past is not news.
        if generation != self.search.generation {
            return Outcome::Continue;
        }

        match found {
            Found::Hits { hits, .. } => {
                for hit in hits {
                    self.absorb(hit);
                }
                self.search.relist();
            }
            Found::Done { hits, files, cancelled, .. } => {
                self.search.running = false;
                if !cancelled {
                    self.search.tally = Some((hits, files));
                }
            }
            Found::Failed { error, .. } => {
                self.search.running = false;
                self.search.error = Some(error);
            }
        }
        self.search.resummarise();
        Outcome::Redraw
    }

    /// File one hit under its group, making the group if it is the first.
    fn absorb(&mut self, hit: Hit) {
        let search = &mut self.search;
        let at = if let Some(at) = search.index.get(&hit.path) {
            *at
        } else {
            let at = search.groups.len();
            search.index.insert(hit.path.clone(), at);
            search.groups.push(Group {
                label: hit.path.display().to_string(),
                path: hit.path.clone(),
                hits: Vec::new(),
            });
            at
        };
        search.groups[at].hits.push(hit);
    }

    /// What the status line says about whatever the pointer is over.
    fn search_hint(&self, target: Target) -> Option<&'static str> {
        match target {
            Target::SearchBack => Some(SearchView::BACK_DESCRIPTION),
            Target::SearchButton(button) => Some(button.describe(self.search.toggles.on(button))),
            _ => None,
        }
    }
}

/// The byte offset of char `at` in `text`, or its length if `at` is past the
/// end. Char offsets are what the caret is kept in, and `String` wants bytes.
fn byte_of(text: &str, at: usize) -> usize {
    text.char_indices().nth(at).map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{KeySet, defaults};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use nun_workspace::Done;
    use std::fs;
    use std::sync::mpsc::Receiver;
    use tempfile::TempDir;

    /// An editor on a small project, with both workers attached and pumped the
    /// way the event loop pumps them.
    struct Tester {
        app: App,
        done: Receiver<Done>,
        found: Receiver<Found>,
    }

    impl Tester {
        fn new(dir: &TempDir) -> Self {
            let mut app = App::new(
                nun_core::Buffer::new(),
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 80, 24));

            let (sender, done) = std::sync::mpsc::channel();
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                true,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );

            let (sender, found) = std::sync::mpsc::channel();
            app.attach_search(Grep::new(Box::new(move |message| {
                let _ = sender.send(message);
            })));

            let mut tester = Self { app, done, found };
            tester.settle_jobs();
            tester
        }

        /// Let the file tree's worker catch up.
        fn settle_jobs(&mut self) {
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

        /// Fire the debounce, then take every answer up to the end of the
        /// search — which the engine promises always arrives, even for a
        /// search that was stale before it started.
        fn settle_search(&mut self) {
            self.app.tick(Instant::now() + DEBOUNCE * 4);
            loop {
                let message =
                    self.found.recv_timeout(Duration::from_secs(10)).expect("the search answered");
                let last = matches!(message, Found::Done { .. } | Found::Failed { .. });
                self.app.handle(Event::Found(message));
                if last {
                    return;
                }
            }
        }

        fn type_text(&mut self, text: &str) {
            for ch in text.chars() {
                self.app.handle(Event::Key(KeyEvent::from(KeyCode::Char(ch))));
            }
        }

        /// Search for `query`, from the panel, and wait for the answer.
        fn search(&mut self, query: &str) {
            self.app.run(crate::commands::Command::SearchProject);
            self.type_text(query);
            self.settle_search();
        }

        fn area(&self) -> Rect {
            self.app.search_area().expect("the panel is showing")
        }

        /// Click the result row at `index`, which must be on screen.
        fn click_row(&mut self, index: usize) {
            let rows = SearchView::rows_area(self.area());
            let offset = u16::try_from(index - self.app.search.scroll).unwrap();
            self.click(rows.x + 1, rows.y + offset);
        }

        fn click(&mut self, column: u16, row: u16) {
            for kind in
                [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)]
            {
                self.app.handle(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }));
            }
        }

        /// The rows as they read, for asserting on the shape of the list.
        fn rows(&self) -> Vec<String> {
            self.app
                .search
                .rows
                .iter()
                .map(|line| match *line {
                    Line::File(group) => {
                        format!("[{}]", self.app.search.groups[group].label)
                    }
                    Line::Hit(group, hit) => {
                        let hit = &self.app.search.groups[group].hits[hit];
                        format!("{}: {}", hit.line, hit.text)
                    }
                })
                .collect()
        }
    }

    fn project(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn typing_a_query_lists_the_lines_that_match_under_their_files() {
        let dir = project(&[
            ("src/one.rs", "fn alpha() {}\nfn beta() {}\n"),
            ("src/two.rs", "// alpha again\n"),
        ]);
        let mut t = Tester::new(&dir);
        t.search("alpha");

        let rows = t.rows();
        assert_eq!(rows.len(), 4, "two files and their one hit each: {rows:?}");
        assert!(rows[0].starts_with('['), "a file heads its hits");
        assert!(rows[1].starts_with("1: "), "and the line number is one-based");
        assert!(rows.iter().any(|row| row.contains("fn alpha() {}")));
        assert!(rows.iter().any(|row| row.contains("// alpha again")));
    }

    #[test]
    fn results_appear_without_the_editor_asking_for_them() {
        // The engine streams, so the panel must take Hits messages that arrive
        // before the Done that ends the search.
        let dir = project(&[("a.rs", "needle\n"), ("b.rs", "needle\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("needle");
        t.app.tick(Instant::now() + DEBOUNCE * 4);

        let first = t.found.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(first, Found::Hits { .. }), "hits come before the end");
        t.app.handle(Event::Found(first));
        assert!(!t.rows().is_empty(), "and are on screen before it");
    }

    #[test]
    fn clicking_a_hit_opens_its_file_at_that_line() {
        let dir = project(&[("src/deep.rs", "one\ntwo\nthe needle here\nfour\n")]);
        let mut t = Tester::new(&dir);
        t.search("needle");
        t.settle_jobs();

        t.click_row(1);
        t.settle_jobs();

        let doc = t.app.doc();
        assert_eq!(
            doc.buffer.path().and_then(|path| path.file_name()),
            Some(std::ffi::OsStr::new("deep.rs")),
            "the file opened"
        );
        let caret = doc.buffer.selections().primary().head;
        let line = doc.buffer.line_of(caret);
        assert_eq!(line, 2, "on the third line, counting from zero");
        let column = caret - doc.buffer.line_start(line);
        assert_eq!(column, 4, "at the match, not at the start of the line");
    }

    #[test]
    fn clicking_a_file_folds_its_hits_away_and_back() {
        let dir = project(&[("a.rs", "hit\nhit\nhit\n")]);
        let mut t = Tester::new(&dir);
        t.search("hit");
        assert_eq!(t.rows().len(), 4, "a file and three lines");

        t.click_row(0);
        assert_eq!(t.rows().len(), 1, "folded to its file");
        t.click_row(0);
        assert_eq!(t.rows().len(), 4, "and back again");
    }

    #[test]
    fn a_toggle_changes_what_is_searched() {
        let dir = project(&[("a.rs", "one\nlonely\n")]);
        let mut t = Tester::new(&dir);
        t.search("one");
        assert_eq!(t.rows().len(), 3, "`one` is in `lonely` too");

        let area = t.area();
        let cell = SearchView::button_area(area, SearchButton::Word).expect("the panel is wide");
        t.click(cell.x, cell.y);
        t.settle_search();

        let rows = t.rows();
        assert_eq!(rows.len(), 2, "whole words only: {rows:?}");
        assert!(rows[1].starts_with("1: "));
    }

    #[test]
    fn a_query_that_is_not_a_regex_says_so_rather_than_finding_nothing() {
        let dir = project(&[("a.rs", "anything\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        // A literal query takes a stray bracket at face value.
        t.type_text("a(b");
        t.settle_search();
        assert!(t.rows().is_empty());
        assert_eq!(t.app.search.error, None, "a literal is never a bad pattern");

        let area = t.area();
        let cell = SearchView::button_area(area, SearchButton::Regex).expect("the panel is wide");
        t.click(cell.x, cell.y);
        t.settle_search();
        assert!(t.app.search.error.is_some(), "as a regex it does not compile");
        assert!(
            t.app.search.summary.as_deref().is_some_and(|summary| !summary.is_empty()),
            "and the panel says so rather than looking empty"
        );
    }

    #[test]
    fn an_answer_to_a_query_the_user_has_typed_past_is_ignored() {
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        assert_eq!(t.rows().len(), 2);

        // An answer from an older generation, arriving late.
        let stale = Found::Hits {
            generation: t.app.search.generation - 1,
            hits: vec![Hit {
                path: "ghost.rs".into(),
                line: 1,
                column: 1,
                text: "ghost".into(),
                matched: std::iter::once(0..5).collect(),
            }],
        };
        assert_eq!(t.app.handle(Event::Found(stale)), Outcome::Continue);
        assert_eq!(t.rows().len(), 2, "the rows on screen are untouched");
    }

    #[test]
    fn the_tree_and_the_panel_take_turns_in_the_sidebar() {
        let dir = project(&[("a.rs", "x\n")]);
        let mut t = Tester::new(&dir);

        t.app.run(crate::commands::Command::SearchProject);
        assert!(t.app.searching(), "the panel has the sidebar");
        assert_eq!(t.app.focus, Focus::Search);
        assert!(t.app.search_area().is_some());

        // The button in the panel's header goes back.
        let area = t.area();
        let cell = SearchView::back_area(area).expect("the panel is wide");
        t.click(cell.x, cell.y);
        assert!(!t.app.searching(), "the tree has it again");
        assert!(t.app.search_area().is_none());

        // And the magnifier in the tree's header comes back.
        let tree = t.app.tree_area().expect("the tree is showing");
        let cell = nun_ui::TreeView::button_area(tree, nun_ui::TreeButton::Search).unwrap();
        t.click(cell.x, cell.y);
        assert!(t.app.searching());
    }

    #[test]
    fn typing_goes_to_the_query_and_never_to_the_document() {
        let dir = project(&[("a.rs", "x\n")]);
        let mut t = Tester::new(&dir);
        let before = t.app.doc().buffer.text().to_string();

        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("hello");
        assert_eq!(t.app.search.query, "hello");
        assert_eq!(t.app.doc().buffer.text().to_string(), before, "the file is untouched");

        // The caret moves in the query, and editing happens where it is.
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Left)));
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Left)));
        t.type_text("_");
        assert_eq!(t.app.search.query, "hel_lo");
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(t.app.search.query, "hello");
    }

    #[test]
    fn a_query_in_another_script_is_edited_by_characters_not_bytes() {
        let dir = project(&[("a.rs", "x\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("日本語");
        assert_eq!(t.app.search.caret, 3, "three characters, not nine bytes");

        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Left)));
        t.type_text("の");
        assert_eq!(t.app.search.query, "日本の語");

        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(t.app.search.query, "日本語");
    }

    #[test]
    fn leaving_the_panel_stops_the_search_it_was_running() {
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.app.run(crate::commands::Command::ToggleSidebar);
        assert!(!t.app.searching(), "Ctrl+B from the panel shows the tree");
        assert!(!t.app.search.running);
    }
}
