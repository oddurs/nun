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
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_ui::{Field, HitState, SearchButton, SearchRow, SearchView, Toggles};
use nun_workspace::{Case, Found, Grep, Hit, Options, Recorded, Replacer};
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
    /// What the hit above it becomes, when a replacement is being made.
    After(usize, usize),
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
    /// What the matches are to be replaced with.
    pub(super) replacement: String,
    /// Where the caret sits in that, as a char offset.
    pub(super) replace_caret: usize,
    /// Which of the two fields the keyboard is in.
    pub(super) field: Field,
    /// Hits struck out of the replace, by the file and line they are on.
    ///
    /// Excluded rather than included, so that hits still arriving from a walk
    /// are in by default and the set stays empty in the ordinary case.
    excluded: BTreeSet<(PathBuf, u32)>,
    /// What the rows on screen would become, by row index.
    ///
    /// Only what is drawn: replacing across a repository can find tens of
    /// thousands of lines, and running a regex over all of them on every
    /// keystroke to draw twenty would cost the repository rather than the
    /// screen.
    previews: HashMap<usize, String>,
    /// When the search that found the current hits started, so a file written
    /// to since can be left alone rather than rewritten from a stale preview.
    searched_at: Option<std::time::SystemTime>,
    /// The compiled replacement, and what it was compiled from.
    ///
    /// Building one compiles a regex, which is not much but is not nothing
    /// either, and the previews are wanted on every frame. It depends on
    /// nothing but the options and the replacement, so it is kept until one
    /// of those changes.
    replacer: Option<(Options, String, Replacer)>,
    /// Which of the toggles are lit.
    pub(super) toggles: Toggles,
    /// The files that have hits, in the order they were found.
    groups: Vec<Group>,
    /// Where each file sits in `groups`, so a hit finds its own group without
    /// a scan.
    index: HashMap<PathBuf, usize>,
    /// Files whose hits are folded away.
    collapsed: BTreeSet<PathBuf>,
    /// The rows as drawn: kept up to date as hits arrive, rather than rebuilt
    /// on every frame or on every batch.
    rows: Vec<Line>,
    /// The widest line number any hit has, so the gutter the panel draws does
    /// not change width as the window moves over the list.
    widest: u32,
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
    ///
    /// Only the window asked for. A broad query on a large repository finds
    /// hundreds of thousands of lines, and building all of them to draw twenty
    /// would make a frame cost what the repository costs rather than what the
    /// screen costs.
    fn view_rows(&self, window: std::ops::Range<usize>) -> Vec<SearchRow<'_>> {
        // Clamped, not fetched: a window is what the screen can show, so it
        // runs past the end of the list far more often than not, and a range
        // lookup answers nothing at all for one that does.
        let end = window.end.min(self.rows.len());
        let first = window.start.min(end);
        self.rows[first..end]
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                let index = first + offset;
                match *line {
                    Line::File(group) => {
                        let group = &self.groups[group];
                        SearchRow::File {
                            path: &group.label,
                            hits: group.hits.len(),
                            collapsed: self.collapsed.contains(&group.path),
                        }
                    }
                    Line::Hit(group, hit) => {
                        let group = &self.groups[group];
                        let hit = &group.hits[hit];
                        SearchRow::Hit {
                            line: hit.line,
                            text: &hit.text,
                            matched: &hit.matched,
                            state: self.state_of(&group.path, hit.line),
                        }
                    }
                    Line::After(group, hit) => {
                        let hit = &self.groups[group].hits[hit];
                        SearchRow::After {
                            line: hit.line,
                            // A row whose preview has not been worked out yet
                            // shows the line unchanged rather than nothing; it is
                            // one frame, and a blank row reads as a deletion.
                            text: self
                                .previews
                                .get(&index)
                                .map_or(hit.text.as_str(), String::as_str),
                        }
                    }
                }
            })
            .collect()
    }

    /// Rebuild the row list from the groups and what is folded away.
    fn relist(&mut self) {
        let replacing = !self.replacement.is_empty();
        let mut rows = Vec::with_capacity(self.rows.len());
        for (at, group) in self.groups.iter().enumerate() {
            rows.push(Line::File(at));
            if self.collapsed.contains(&group.path) {
                continue;
            }
            for (index, hit) in group.hits.iter().enumerate() {
                rows.push(Line::Hit(at, index));
                if replacing && self.included(&group.path, hit.line) {
                    rows.push(Line::After(at, index));
                }
            }
        }
        self.rows = rows;
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
        self.collapsed.clear();
        // The strikes belonged to the old list. Carrying them into a new
        // query silently leaves lines unreplaced that nobody struck out.
        self.excluded.clear();
        self.widest = 0;
        self.scroll = 0;
        self.selected = None;
        self.tally = None;
        self.error = None;
    }

    /// The text of the field the keyboard is in.
    const fn text(&self) -> &String {
        match self.field {
            Field::Query => &self.query,
            Field::Replace => &self.replacement,
        }
    }

    /// The same, to edit.
    const fn text_mut(&mut self) -> &mut String {
        match self.field {
            Field::Query => &mut self.query,
            Field::Replace => &mut self.replacement,
        }
    }

    /// Where the caret is in whichever field has the keyboard.
    const fn caret_mut(&mut self) -> &mut usize {
        match self.field {
            Field::Query => &mut self.caret,
            Field::Replace => &mut self.replace_caret,
        }
    }

    /// The caret a field is drawn with: its own when it has the keyboard, and
    /// the start otherwise.
    ///
    /// The widget anchors a field's window to the caret it was drawn with, so
    /// a click is worked out against the same number the row was laid out
    /// from. Handing it a stored caret for a field nobody is typing in would
    /// land clicks in the wrong character.
    pub(super) const fn drawn_caret(&self, field: Field) -> usize {
        match (self.field, field) {
            (Field::Query, Field::Query) => self.caret,
            (Field::Replace, Field::Replace) => self.replace_caret,
            _ => 0,
        }
    }

    /// Take the char at `at` out of whichever field has the keyboard.
    fn remove_char(&mut self, at: usize) {
        let from = byte_of(self.text(), at);
        let to = byte_of(self.text(), at + 1);
        self.text_mut().replace_range(from..to, "");
    }

    /// Whether a hit is in the replace.
    fn included(&self, path: &std::path::Path, line: u32) -> bool {
        !self.excluded.contains(&(path.to_path_buf(), line))
    }

    /// What is going to happen to a hit.
    fn state_of(&self, path: &std::path::Path, line: u32) -> HitState {
        if self.replacement.is_empty() {
            // Nothing is being replaced, so there is nothing to take out of
            // it, and a row of markers nobody can act on is noise.
            HitState::Plain
        } else if self.included(path, line) {
            HitState::Included
        } else {
            HitState::Excluded
        }
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
        // A search that was abandoned half way through left no answer behind,
        // and an empty list under a query reads as "nothing matched". Asking
        // again is the only honest thing to show.
        if !self.search.query.is_empty()
            && self.search.tally.is_none()
            && self.search.error.is_none()
        {
            self.restart_search(Instant::now());
        }
        self.relayout();
        Outcome::Redraw
    }

    /// Swap the sidebar back to the file tree.
    pub(super) fn show_file_tree(&mut self) -> Outcome {
        self.sidebar_view = SidebarView::Files;
        if self.focus == Focus::Search {
            self.focus = Focus::Sidebar;
        }
        // The walk is worth nothing to a panel nobody can see. The deadline
        // goes with it: it is armed before the search starts, so cancelling
        // without disarming would start one anyway, a few milliseconds later,
        // for a panel that has gone.
        self.cancel_search();
        self.search_deadline = None;
        // Half a search is not an answer. Keeping what it had found would
        // show part of a walk on the way back in as though it were all of it.
        if self.search.running {
            self.search.forget();
        }
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
        hits.push(super::cells(SearchView::replace_area(area)), Target::SearchReplace, true);
        if let Some(cell) = SearchView::apply_area(area) {
            hits.push(super::cells(cell), Target::SearchApply, true);
        }
        for button in SearchButton::ALL {
            if let Some(cell) = SearchView::button_area(area, button) {
                hits.push(super::cells(cell), Target::SearchButton(button), true);
            }
        }

        let rows = SearchView::rows_area(area);
        hits.push(super::cells(rows), Target::SearchEmpty, false);
        let shown = SearchView::visible_rows(area);
        // The same window the frame draws, so the mark a click lands on is
        // the mark that is there.
        let view = self.search.view_rows(self.search.scroll..self.search.scroll + shown);
        for offset in 0..shown {
            let index = self.search.scroll + offset;
            if index >= self.search.rows.len() {
                break;
            }
            let within = offset;
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows.y + offset, height: 1, ..rows };
            hits.push(super::cells(line), Target::SearchRow(index), true);
            // On top of its row, so clicking the mark strikes the hit out
            // while clicking the line still opens the file. `view` is the
            // window, indexed from its own start and drawn with a zero
            // scroll, so it is asked where the row sits in the window rather
            // than where it sits in the list.
            if let Some(cell) = SearchView::marker_area(area, &view, within, 0) {
                hits.push(super::cells(cell), Target::SearchMarker(index), true);
            }
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
        // The widget is given the window and nothing else, so the selection
        // and the hover are rebased into it and the scroll has already been
        // taken. Anything outside is not drawn and so is neither.
        let first = self.search.scroll;
        let last = first.saturating_add(SearchView::visible_rows(area));
        let within = |row: Option<usize>| {
            row.filter(|row| (first..last).contains(row)).map(|row| row - first)
        };
        let rows = self.search.view_rows(first..last);
        SearchView::new(&self.search.query, &rows, &self.palette)
            .scrolled_to(0)
            .widest_line(self.search.widest)
            .selected(within(self.search.selected))
            .hovered(within(hovered))
            .hovered_button(button)
            .hovered_back(self.hover.current() == Some(Target::SearchBack))
            .toggles(self.search.toggles)
            .focused(self.focus == Focus::Search)
            .replacement(&self.search.replacement)
            .editing(
                (self.focus == Focus::Search).then_some(self.search.field),
                self.search.drawn_caret(self.search.field),
            )
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
            // Tab moves between the two fields rather than out of the panel:
            // there is nowhere else for it to go, and typing a replacement
            // after a query is the ordinary order of doing this.
            KeyCode::Tab | KeyCode::BackTab => self.search.field = other(self.search.field),
            KeyCode::Left => {
                let caret = self.search.caret_mut();
                *caret = caret.saturating_sub(1);
            }
            KeyCode::Right => {
                let end = self.search.text().chars().count();
                let caret = self.search.caret_mut();
                *caret = (*caret + 1).min(end);
            }
            KeyCode::Home => *self.search.caret_mut() = 0,
            KeyCode::End => *self.search.caret_mut() = self.search.text().chars().count(),
            KeyCode::Backspace => {
                let caret = *self.search.caret_mut();
                if caret > 0 {
                    self.search.remove_char(caret - 1);
                    *self.search.caret_mut() = caret - 1;
                    self.edited(now);
                }
            }
            KeyCode::Delete => {
                let caret = *self.search.caret_mut();
                if caret < self.search.text().chars().count() {
                    self.search.remove_char(caret);
                    self.edited(now);
                }
            }
            KeyCode::Char(ch) if !control => {
                let caret = *self.search.caret_mut();
                let at = byte_of(self.search.text(), caret);
                self.search.text_mut().insert(at, ch);
                *self.search.caret_mut() = caret + 1;
                self.edited(now);
            }
            _ => return Outcome::Continue,
        }
        Outcome::Redraw
    }

    /// One of the fields changed.
    ///
    /// Changing the query means searching again; changing the replacement
    /// changes only what the preview says the hits would become, and asking
    /// the filesystem for the same answer twice would be rude.
    fn edited(&mut self, now: Instant) {
        match self.search.field {
            Field::Query => self.restart_search(now),
            Field::Replace => self.search.relist(),
        }
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
            Target::SearchQuery | Target::SearchReplace => {
                self.focus = Focus::Search;
                let field =
                    if target == Target::SearchQuery { Field::Query } else { Field::Replace };
                self.search.field = field;
                if let Some(area) = self.search_area() {
                    // The caret the row was drawn with, which is the start for
                    // a field that did not have the keyboard — the widget
                    // anchors its window to that, so a click has to be worked
                    // out against the same number.
                    let drawn = self.search.drawn_caret(field);
                    let text = match field {
                        Field::Query => self.search.query.clone(),
                        Field::Replace => self.search.replacement.clone(),
                    };
                    let at = SearchView::caret_at(area, field, &text, drawn, column);
                    *self.search.caret_mut() = at;
                }
            }
            Target::SearchApply => return self.apply_replace(),
            Target::SearchMarker(row) => {
                self.focus = Focus::Search;
                return self.toggle_hit(row);
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

    /// Flip one toggle from the keyboard, and say which way it went.
    ///
    /// The buttons are the mouse path; this is what is left when the sidebar
    /// is too narrow to draw them. It works with the panel closed as well,
    /// since the toggles belong to the search rather than to its panel.
    pub(super) fn toggle_search(&mut self, button: SearchButton) -> Outcome {
        self.flip(button);
        if self.searching() {
            self.restart_search(Instant::now());
        }
        let name = match button {
            SearchButton::Regex => "Regular expressions",
            SearchButton::Case => "Match case",
            SearchButton::Word => "Whole words",
            SearchButton::Ignored => "Ignored files",
        };
        let state = if self.search.toggles.on(button) { "on" } else { "off" };
        self.message = Some(format!("Search: {name} {state}"));
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
            // A preview row stands for the hit above it, so clicking either
            // of them means the same thing.
            Line::Hit(group, hit) | Line::After(group, hit) => self.open_hit(group, hit),
        }
    }

    /// Open the file a hit is in, at its line and column.
    fn open_hit(&mut self, group: usize, hit: usize) -> Outcome {
        {
            let Some(root) = self.workspace_root() else { return Outcome::Continue };
            let group = &self.search.groups[group];
            let Some(hit) = group.hits.get(hit) else { return Outcome::Continue };
            let (line, column) = (hit.line, hit.column);
            let path = root.join(&group.path);
            self.open_file(&path);
            // Opening can fail — the file deleted since it was found, or
            // its folder gone — and it says so rather than throwing.
            // Moving the caret then would move it in whatever was already
            // open, which is someone else's file and someone else's place
            // in it.
            if self.doc().buffer.path() == Some(path.as_path()) {
                self.place_caret_at(line, column);
            }
            // The panel keeps the keyboard: finding one hit usually means
            // looking at the next one too.
            Outcome::Redraw
        }
    }

    /// Strike a hit out of the replace, or put it back.
    fn toggle_hit(&mut self, row: usize) -> Outcome {
        let Some(line) = self.search.rows.get(row).copied() else { return Outcome::Continue };
        let (group, hit) = match line {
            Line::Hit(group, hit) | Line::After(group, hit) => (group, hit),
            Line::File(_) => return Outcome::Continue,
        };
        let Some(hit) = self.search.groups[group].hits.get(hit) else { return Outcome::Continue };
        let key = (self.search.groups[group].path.clone(), hit.line);
        if !self.search.excluded.remove(&key) {
            self.search.excluded.insert(key);
        }
        // Striking one out takes its preview row away, and putting it back
        // brings one along, so the list is a different length either way.
        self.search.relist();
        let last = self.search.rows.len().saturating_sub(1);
        self.search.selected = self.search.selected.map(|at| at.min(last));
        self.follow_search_selection();
        Outcome::Redraw
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

        // Without a worker there is nothing to ask, and so nothing is coming.
        // Saying "Searching…" for the rest of the session would be worse than
        // saying nothing. A terminal attaches one before the first frame; this
        // is for a test, or for a caller that has not.
        let Some(grep) = self.search.grep.as_ref() else {
            self.search.running = false;
            self.search.resummarise();
            return Outcome::Redraw;
        };
        self.search.generation += 1;
        // Taken before the walk rather than after it: a file written to while
        // the walk was still running was not what the preview showed either.
        self.search.searched_at = Some(std::time::SystemTime::now());
        grep.search(root, self.search.options(), self.search.generation);
        Outcome::Continue
    }

    /// Work out what the rows on screen would become.
    ///
    /// Only the window: replacing across a repository can find tens of
    /// thousands of lines, and running a regex over all of them on every
    /// keystroke to draw twenty would cost what the repository costs rather
    /// than what the screen costs.
    pub(super) fn refresh_search_previews(&mut self) {
        self.search.previews.clear();
        if !self.searching() || self.search.replacement.is_empty() {
            return;
        }
        let Some(area) = self.search_area() else { return };
        let options = self.search.options();
        let stale = !matches!(
            self.search.replacer.as_ref(),
            Some((was, with, _)) if *was == options && *with == self.search.replacement
        );
        if stale {
            // The query does not compile; the summary already says so, and a
            // preview of nothing would be a second way of saying it.
            let Ok(built) = Replacer::new(&options, &self.search.replacement) else {
                self.search.replacer = None;
                return;
            };
            self.search.replacer = Some((options, self.search.replacement.clone(), built));
        }
        let Some((.., replacer)) = self.search.replacer.as_ref() else { return };
        let first = self.search.scroll;
        let last = first.saturating_add(SearchView::visible_rows(area));
        let mut previewed: Vec<(usize, String)> = Vec::new();
        for index in first..last.min(self.search.rows.len()) {
            if let Some(Line::After(group, hit)) = self.search.rows.get(index).copied()
                && let Some(hit) = self.search.groups[group].hits.get(hit)
            {
                // What is previewed is the text the panel has, which for a
                // very long line is a window of it. The write rewrites every
                // match in the whole line, so past that window the preview
                // shows fewer changes than the file will get — there being
                // nothing on screen to show them on.
                let after = replacer.line(&hit.text);
                previewed.push((index, after));
            }
        }
        self.search.previews.extend(previewed);
    }

    /// Rewrite every hit that has not been struck out.
    pub(super) fn apply_replace(&mut self) -> Outcome {
        if self.search.replacement.is_empty() {
            self.warn("Type what the matches should become first.".to_string());
            return Outcome::Redraw;
        }
        let Some(root) = self.workspace_root() else { return Outcome::Continue };
        let Some(searched_at) = self.search.searched_at else {
            self.warn("Search for something first.".to_string());
            return Outcome::Redraw;
        };
        // What has arrived is not the answer yet. Replacing it would rewrite
        // whatever part of the project the walk happened to have reached, say
        // it had replaced everything, and then fill the panel with the rest —
        // a list mixing done and not-done with nothing to tell them apart.
        // Pressing again would blame the first pass's own writes on somebody
        // else having changed the files.
        if self.search.running {
            self.warn("The search is still running, so this is not all of it yet.".to_string());
            return Outcome::Redraw;
        }

        // Each line goes with the text the preview was taken from, so the
        // write can ask whether it is still the line that was previewed
        // rather than only whether it still matches. A rewrite that leaves
        // line 42 matching while making it a different line 42 — a checkout,
        // a format on save — passes the second question and fails the first.
        let chosen: Vec<(PathBuf, Vec<Recorded>)> = self
            .search
            .groups
            .iter()
            .filter_map(|group| {
                let lines: Vec<Recorded> = group
                    .hits
                    .iter()
                    .filter(|hit| self.search.included(&group.path, hit.line))
                    .map(Recorded::of)
                    .collect();
                (!lines.is_empty()).then(|| (group.path.clone(), lines))
            })
            .collect();
        if chosen.is_empty() {
            self.warn("Every hit is struck out, so there is nothing to replace.".to_string());
            return Outcome::Redraw;
        }

        self.send_job(nun_workspace::Job::Replace {
            root,
            options: self.search.options(),
            replacement: self.search.replacement.clone(),
            chosen,
            searched_at,
        });
        Outcome::Redraw
    }

    /// A replace finished.
    pub(super) fn replace_done(
        &mut self,
        report: &nun_workspace::Report,
        change: Option<&nun_workspace::Change>,
    ) -> Outcome {
        if let Some(change) = change {
            self.after_op(change);
            // Offered the way a file operation is, because it is one — and
            // this is the one most worth being able to take back.
            self.undo_offer = true;
            self.last_undone = false;
        }

        // A file that is open still holds what it held before, and saving it
        // would put that back over the replace without saying so. Whichever
        // ones nobody has touched are reloaded; the rest are named, because
        // only the person editing them can say which version they meant.
        let written: Vec<PathBuf> = self.workspace_root().map_or_else(Vec::new, |root| {
            report
                .files
                .iter()
                .filter(|(_, outcome)| {
                    matches!(outcome, nun_workspace::Outcome::Changed(lines) if *lines > 0)
                })
                .map(|(path, _)| root.join(path))
                .collect()
        });
        let untaken = self.reload_written(&written);

        self.message = Some(say_what_happened(report, &untaken));
        Outcome::Redraw
    }

    /// Bring open documents back in line with files rewritten underneath them.
    ///
    /// A file that is open still holds what it held before, and saving it
    /// would put that back over the change with nothing said. Whichever ones
    /// nobody has touched are reloaded; the rest are named, because only the
    /// person editing them can say which version they meant.
    ///
    /// Used by the replace and by taking it back. An undo that restored the
    /// files but left the buffers showing the replacement would be a screen
    /// disagreeing with the disk, and the next save would undo the undo.
    pub(super) fn reload_written(&mut self, written: &[PathBuf]) -> Vec<String> {
        let mut untaken: Vec<String> = Vec::new();
        for full in written {
            let Some(document) = self
                .docs
                .iter_mut()
                .find(|document| document.buffer.path() == Some(full.as_path()))
            else {
                continue;
            };
            if document.buffer.is_modified() {
                untaken.push(name_of(full));
                continue;
            }
            let Ok((mut buffer, _)) = nun_core::Buffer::load(full) else { continue };
            // A document keeps its scroll beside the buffer and its caret
            // inside it, so a straight swap would leave the view where it was
            // and the caret at the top of the file, off the screen.
            let was = document.buffer.selections().primary().head;
            let at = was.min(buffer.len_chars());
            buffer.set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
            buffer.set_tab_width(document.buffer.tab_width());
            let id = document.id;
            document.buffer = buffer;
            self.syntax_open(id);
        }
        untaken
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
                let mut ordered = true;
                for hit in hits {
                    ordered &= self.absorb(hit);
                }
                if !ordered {
                    self.search.relist();
                }
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

    /// File one hit under its group, making the group if it is the first, and
    /// extend the row list with it.
    ///
    /// Returns whether the rows still describe the results. The walk reports a
    /// file at a time, so a hit almost always either opens a new group or
    /// extends the newest one, and both of those are a push. Only a hit
    /// landing in an older group needs a row inserted in the middle, and that
    /// is what a rebuild is for — rebuilding on every batch instead would make
    /// taking in a search quadratic in what it finds.
    fn absorb(&mut self, hit: Hit) -> bool {
        let search = &mut self.search;
        let line = hit.line;
        search.widest = search.widest.max(line);
        let fresh = !search.index.contains_key(&hit.path);
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
        if fresh {
            search.rows.push(Line::File(at));
        }
        let index = search.groups[at].hits.len();
        search.groups[at].hits.push(hit);

        // A folded file shows no lines, so there is no row to add.
        if search.collapsed.contains(&search.groups[at].path) {
            return true;
        }
        // Newest group: its rows are the tail of the list, so this goes there.
        if at + 1 == search.groups.len() {
            search.rows.push(Line::Hit(at, index));
            if !search.replacement.is_empty() && search.included(&search.groups[at].path, line) {
                search.rows.push(Line::After(at, index));
            }
            return true;
        }
        false
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

/// How a file reads in a message: its name, since a whole path is usually
/// longer than the line it has to fit on.
fn name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// A sentence for the status line: what a replace came to.
fn say_what_happened(report: &nun_workspace::Report, untaken: &[String]) -> String {
    let (lines, files) = (report.lines, report.changed());
    let mut said = match (lines, files) {
        (0, _) => "Nothing was replaced".to_string(),
        (1, 1) => "Replaced 1 line in 1 file".to_string(),
        (lines, 1) => format!("Replaced {lines} lines in 1 file"),
        (lines, files) => format!("Replaced {lines} lines in {files} files"),
    };
    let skipped = report.skipped();
    if skipped > 0 {
        let _ = write!(said, ", left {skipped} alone as they have changed since the search");
    }
    let failed = report.failed();
    if failed > 0 {
        let _ = write!(said, ", and could not write {failed}");
    }
    said.push('.');
    if !untaken.is_empty() {
        let _ = write!(
            said,
            " {} has unsaved changes and still shows the old text; saving it would put that back.",
            untaken.join(", ")
        );
    }
    said
}

/// The field that is not this one.
const fn other(field: Field) -> Field {
    match field {
        Field::Query => Field::Replace,
        Field::Replace => Field::Query,
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
                .enumerate()
                .map(|(index, line)| match *line {
                    Line::File(group) => {
                        format!("[{}]", self.app.search.groups[group].label)
                    }
                    Line::Hit(group, hit) => {
                        let hit = &self.app.search.groups[group].hits[hit];
                        format!("{}: {}", hit.line, hit.text)
                    }
                    Line::After(group, hit) => {
                        let hit = &self.app.search.groups[group].hits[hit];
                        let after = self
                            .app
                            .search
                            .previews
                            .get(&index)
                            .map_or(hit.text.as_str(), String::as_str);
                        format!("{}> {after}", hit.line)
                    }
                })
                .collect()
        }

        /// Type a replacement into the second field, and let the previews
        /// catch up the way a frame would.
        fn replace_with(&mut self, text: &str) {
            self.app.handle(Event::Key(KeyEvent::from(KeyCode::Tab)));
            self.type_text(text);
            self.app.refresh_search_previews();
        }

        /// Apply the replace and wait for the filesystem worker.
        fn apply(&mut self) {
            self.app.apply_replace();
            self.settle_jobs();
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
    fn every_toggle_is_reachable_from_the_keyboard_when_its_button_is_not() {
        // Narrow enough that the row of toggles no longer fits, so the last
        // of them is not drawn and has no hit region.
        let dir = project(&[("a.rs", "one\nlonely\n")]);
        let mut t = Tester::new(&dir);
        t.search("one");
        t.app.set_viewport(Rect::new(0, 0, 14, 20));
        let area = t.area();
        assert!(
            SearchView::button_area(area, SearchButton::Ignored).is_none(),
            "a button is gone at this width"
        );

        let ctrl_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        t.app.handle(Event::Key(ctrl_k));
        t.app.handle(Event::Key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT)));
        assert!(t.app.search.toggles.word, "Ctrl+K Shift+W turned whole words on");
        assert_eq!(t.app.message(), Some("Search: Whole words on"));
        t.settle_search();
        assert_eq!(t.rows().len(), 2, "and the search ran again with it: {:?}", t.rows());

        for (key, button) in
            [('r', SearchButton::Regex), ('c', SearchButton::Case), ('I', SearchButton::Ignored)]
        {
            t.app.handle(Event::Key(ctrl_k));
            let shift = if key.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            t.app.handle(Event::Key(KeyEvent::new(KeyCode::Char(key), shift)));
            assert!(t.app.search.toggles.on(button), "Ctrl+K {key} flipped {button:?}");
        }
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
                whole: true,
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
    fn closing_the_panel_does_not_leave_a_walk_queued_behind_it() {
        // The deadline is armed before the search starts, so cancelling
        // without disarming would start a walk of the whole project a few
        // milliseconds after the panel had gone — and every batch it found
        // would ask for a redraw of something nobody can see.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("alpha");
        assert!(t.app.search_deadline.is_some(), "typing arms it");

        t.app.run(crate::commands::Command::ToggleSidebar);
        assert!(!t.app.searching());
        assert_eq!(t.app.search_deadline, None, "and leaving disarms it");
        assert_eq!(t.app.tick(Instant::now() + DEBOUNCE * 4), Outcome::Continue);
    }

    #[test]
    fn reopening_after_closing_mid_search_asks_again() {
        // Closed half way through, the panel has a query and part of an
        // answer. Showing that on the way back in would be a lie of omission:
        // an empty list under a query reads as "nothing matched", and a
        // partial one reads as the whole of it.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("alpha");
        t.app.run(crate::commands::Command::ToggleSidebar);

        t.app.run(crate::commands::Command::SearchProject);
        assert!(t.app.search_deadline.is_some(), "it asks again rather than sitting there");
        t.settle_search();
        assert_eq!(t.rows().len(), 2, "and the answer is a whole one");
        assert!(t.app.search.summary.is_some(), "with a summary rather than silence");
    }

    #[test]
    fn a_hit_whose_file_has_gone_leaves_the_open_one_where_it_was() {
        // Results outlive the files they came from: a checkout, a build
        // clean, or deleting the folder from the tree while the list is up.
        // Opening then fails and says so, and the caret must not wander off
        // in whatever was already on screen.
        let dir = project(&[("keep.rs", "untouched\n"), ("gone/hit.rs", "one\ntwo\nalpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.open_file(&dir.path().join("keep.rs"));
        t.settle_jobs();
        t.search("alpha");

        fs::remove_dir_all(dir.path().join("gone")).unwrap();
        let hit = t
            .app
            .search
            .rows
            .iter()
            .position(|line| matches!(line, Line::Hit(..)))
            .expect("the hit is listed");
        t.click_row(hit);
        t.settle_jobs();

        let doc = t.app.doc();
        assert_eq!(
            doc.buffer.path().and_then(|path| path.file_name()),
            Some(std::ffi::OsStr::new("keep.rs")),
            "the file that is still there stayed open"
        );
        assert_eq!(doc.buffer.selections().primary().head, 0, "and its caret did not move");
    }

    #[test]
    fn a_frame_builds_only_the_rows_it_draws() {
        // A broad query on a large repository finds far more than fits. The
        // cost of a frame has to be the screen's, not the result list's.
        let dir = project(&[("a.rs", &"hit\n".repeat(40)), ("b.rs", &"hit\n".repeat(40))]);
        let mut t = Tester::new(&dir);
        t.search("hit");
        assert_eq!(t.app.search.rows.len(), 82, "two files and eighty lines");

        assert_eq!(t.app.search.view_rows(2..7).len(), 5, "only the window");
        assert!(t.app.search.view_rows(9_000..9_010).is_empty(), "past the end is empty");
        // A window taller than the list is the ordinary case, not the corner:
        // most searches find fewer lines than the panel can show. Answering
        // nothing for one of those would draw an empty panel over real results.
        assert_eq!(t.app.search.view_rows(80..9_000).len(), 2, "a window is clipped, not refused");
        assert_eq!(t.app.search.view_rows(0..9_000).len(), 82, "and so is one over the whole list");
    }

    #[test]
    fn the_rows_built_as_hits_arrive_are_the_rows_built_from_scratch() {
        // Hits extend the list as they come; folding rebuilds it. The two
        // have to agree, or a streamed search would show one thing until it
        // was folded and another afterwards.
        let dir =
            project(&[("a.rs", "hit\nhit\n"), ("b.rs", "hit\n"), ("c/d.rs", "hit\nno\nhit\n")]);
        let mut t = Tester::new(&dir);
        t.search("hit");

        let streamed = t.app.search.rows.clone();
        t.app.search.relist();
        assert_eq!(streamed, t.app.search.rows, "streamed and rebuilt disagree");
        assert_eq!(streamed.len(), 8, "three files and five lines");
    }

    #[test]
    fn a_file_first_seen_in_a_later_batch_still_gets_its_heading() {
        // The fast path decides whether a hit opens a group from the index it
        // was looked up in, and batches are only how the walk happens to
        // deliver things. A file arriving in its own batch must be headed the
        // same as one arriving beside others.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        let generation = t.app.search.generation;

        let hit = |path: &str, line: u32| Hit {
            path: path.into(),
            line,
            column: 1,
            text: "alpha".into(),
            whole: true,
            matched: std::iter::once(0..5).collect(),
        };
        for (path, line) in [("b.rs", 1), ("c.rs", 1), ("b.rs", 7), ("d.rs", 1)] {
            t.app.handle(Event::Found(Found::Hits { generation, hits: vec![hit(path, line)] }));
        }

        let streamed = t.app.search.rows.clone();
        t.app.search.relist();
        assert_eq!(streamed, t.app.search.rows, "a batch boundary changed the list");

        let headings: Vec<&String> = t
            .app
            .search
            .rows
            .iter()
            .filter_map(|line| match line {
                Line::File(group) => Some(&t.app.search.groups[*group].label),
                Line::Hit(..) | Line::After(..) => None,
            })
            .collect();
        assert_eq!(headings, ["a.rs", "b.rs", "c.rs", "d.rs"], "every file is headed once");
    }

    #[test]
    fn a_hit_for_a_file_already_left_behind_lands_in_it_rather_than_at_the_end() {
        // The walk reports a file at a time, so a hit almost always opens a
        // new group or extends the newest. One that arrives late for an older
        // file needs a row in the middle, which is the case the fast path
        // cannot take.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        let generation = t.app.search.generation;

        let hit = |path: &str, line: u32| Hit {
            path: path.into(),
            line,
            column: 1,
            text: "alpha".into(),
            whole: true,
            matched: std::iter::once(0..5).collect(),
        };
        // A second file, then a hit belonging to the first one after it.
        t.app.handle(Event::Found(Found::Hits {
            generation,
            hits: vec![hit("b.rs", 1), hit("a.rs", 9)],
        }));

        assert_eq!(t.app.search.groups[0].hits.len(), 2, "the late hit joined its file");
        let streamed = t.app.search.rows.clone();
        t.app.search.relist();
        assert_eq!(streamed, t.app.search.rows, "and the list was rebuilt to match");
        let rows = t.rows();
        assert_eq!(rows[2], "9: alpha", "under its own file, not under the newer one");
    }

    #[test]
    fn the_gutter_is_sized_for_the_whole_list_not_the_window() {
        // Otherwise the hit text would shift sideways as a four-digit line
        // number scrolled into view.
        let dir = project(&[("a.rs", &("no\n".repeat(1_200) + "alpha\n"))]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        assert_eq!(t.app.search.widest, 1_201, "tracked as the hits arrive");
    }

    #[test]
    fn a_panel_with_no_worker_behind_it_does_not_say_it_is_searching_for_ever() {
        // Typing marks the panel as searching before anything is asked for.
        // With nothing to ask, that has to be taken back rather than left on
        // screen as a search that never returns.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut app = App::new(
            nun_core::Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 24));
        let (sender, _done) = std::sync::mpsc::channel();
        app.open_folder(
            dir.path().to_path_buf(),
            dir.path().join(".trash"),
            true,
            Box::new(move |message| {
                let _ = sender.send(message);
            }),
        );

        app.run(crate::commands::Command::SearchProject);
        for ch in "alpha".chars() {
            app.handle(Event::Key(KeyEvent::from(KeyCode::Char(ch))));
        }
        assert!(app.search.running, "typing says a search is coming");

        app.tick(Instant::now() + DEBOUNCE * 4);
        assert!(!app.search.running, "and with no worker it is taken back");
    }

    #[test]
    fn a_replacement_previews_what_each_line_becomes() {
        let dir = project(&[("a.rs", "let alpha = 1;\nlet beta = alpha + alpha;\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");

        let rows = t.rows();
        // A preview under each hit, showing every match on the line replaced,
        // not just the first.
        assert!(rows.iter().any(|row| row == "1> let omega = 1;"), "{rows:?}");
        assert!(
            rows.iter().any(|row| row == "2> let beta = omega + omega;"),
            "both matches on the line: {rows:?}"
        );
        // And the file itself is untouched until it is applied.
        assert!(fs::read_to_string(dir.path().join("a.rs")).unwrap().contains("alpha"));
    }

    #[test]
    fn a_capture_group_in_the_replacement_is_previewed_as_it_will_be_written() {
        let dir = project(&[("a.rs", "fn alpha_one() {}\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.app.search.toggles.regex = true;
        t.type_text("alpha_(\\w+)");
        t.settle_search();
        t.replace_with("beta_$1");

        let rows = t.rows();
        assert!(rows.iter().any(|row| row == "1> fn beta_one() {}"), "{rows:?}");
    }

    #[test]
    fn a_dollar_in_a_literal_replacement_is_a_dollar() {
        // Only the regex mode interpolates. Getting this the wrong way round
        // would quietly eat someone's text.
        let dir = project(&[("a.rs", "let price = 0;\n")]);
        let mut t = Tester::new(&dir);
        t.search("price");
        t.replace_with("$1");

        let rows = t.rows();
        assert!(rows.iter().any(|row| row == "1> let $1 = 0;"), "{rows:?}");
    }

    #[test]
    fn striking_a_hit_out_takes_its_preview_with_it_and_leaves_the_line_alone() {
        let dir = project(&[("a.rs", "alpha\nalpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");
        assert_eq!(t.rows().len(), 5, "a file, two hits and two previews");

        // The marker of the first hit, which is the row after the file's.
        let area = t.area();
        let view = t.app.search.view_rows(0..SearchView::visible_rows(area));
        let cell = SearchView::marker_area(area, &view, 1, 0).expect("the hit has a marker");
        t.click(cell.x, cell.y);

        let rows = t.rows();
        assert_eq!(rows.len(), 4, "its preview went with it: {rows:?}");
        t.apply();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "alpha\nomega\n",
            "only the hit that was left in changed"
        );
    }

    #[test]
    fn applying_rewrites_every_file_and_undo_brings_them_all_back() {
        let dir = project(&[
            ("a.rs", "alpha one\n"),
            ("b.rs", "alpha two\nalpha three\n"),
            ("c.rs", "nothing here\n"),
        ]);
        let before: Vec<String> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|name| fs::read_to_string(dir.path().join(name)).unwrap())
            .collect();

        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");
        t.apply();

        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "omega one\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "omega two\nomega three\n"
        );
        assert!(t.app.message().unwrap().contains("3 lines"), "{:?}", t.app.message());

        // The status line offers to take it back, the way it does for any
        // other filesystem operation. One press, every file.
        assert!(t.app.undo_offer, "the offer is there to take");
        t.app.undo_file_op();
        t.settle_jobs();
        for (name, was) in ["a.rs", "b.rs", "c.rs"].iter().zip(&before) {
            assert_eq!(&fs::read_to_string(dir.path().join(name)).unwrap(), was, "{name}");
        }
    }

    #[test]
    fn a_file_written_to_since_the_search_is_left_alone_and_said_so() {
        let dir = project(&[("a.rs", "alpha\n"), ("b.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");

        // Somebody else gets there first.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(dir.path().join("b.rs"), "alpha, and something new\n").unwrap();
        t.apply();

        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "omega\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "alpha, and something new\n",
            "what was previewed is not what is there, so it was not written"
        );
        let said = t.app.message().unwrap();
        assert!(said.contains("changed since the search"), "{said}");
    }

    #[test]
    fn an_open_file_follows_the_replace_rather_than_undoing_it_on_the_next_save() {
        // The buffer still holds what it held before. Saving it would put that
        // back over the replace, and nothing would have said so.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.open_file(&dir.path().join("a.rs"));
        t.settle_jobs();

        t.search("alpha");
        t.replace_with("omega");
        t.apply();

        assert_eq!(
            t.app.doc().buffer.text().to_string(),
            "omega\n",
            "the open buffer was reloaded"
        );
    }

    #[test]
    fn an_open_file_with_unsaved_changes_is_named_rather_than_overwritten() {
        // Only the person editing it can say which version they meant.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.open_file(&dir.path().join("a.rs"));
        t.settle_jobs();
        t.app.doc_mut().buffer.insert("// mine\n");

        t.search("alpha");
        t.replace_with("omega");
        t.apply();

        assert!(
            t.app.doc().buffer.text().to_string().contains("// mine"),
            "the unsaved work is still there"
        );
        let said = t.app.message().unwrap();
        assert!(said.contains("unsaved changes"), "and it is named: {said}");
    }

    #[test]
    fn the_replacement_is_compiled_once_and_not_once_a_frame() {
        // Building it compiles a regex, and the previews are wanted on every
        // frame. It depends on nothing but the options and the replacement.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");

        let first = t.app.search.replacer.as_ref().map(|(_, with, _)| with.clone());
        assert_eq!(first.as_deref(), Some("omega"), "it was built");

        // A frame that changes nothing keeps it.
        let before = std::ptr::from_ref(t.app.search.replacer.as_ref().unwrap());
        t.app.refresh_search_previews();
        assert_eq!(
            std::ptr::from_ref(t.app.search.replacer.as_ref().unwrap()),
            before,
            "the same one was reused"
        );

        // Changing the replacement builds a new one.
        t.type_text("!");
        t.app.refresh_search_previews();
        assert_eq!(
            t.app.search.replacer.as_ref().map(|(_, with, _)| with.as_str()),
            Some("omega!"),
            "and a change rebuilds it"
        );
    }

    #[test]
    fn a_replacement_that_changes_nothing_says_so() {
        // Every chosen line comes out the same as it went in, so nothing is
        // written — and the sentence has to say that rather than claiming a
        // replace that did not happen.
        let dir = project(&[("a.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("alpha");
        t.apply();

        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "alpha\n");
        let said = t.app.message().unwrap();
        assert!(said.contains("Nothing was replaced"), "{said}");
    }

    #[test]
    fn a_mark_can_still_be_clicked_once_the_results_have_scrolled() {
        // The window is indexed from its own start; the row index is into the
        // whole list. Mixing the two costs every mark its mouse path once the
        // list has scrolled a page, which is the point at which anyone is
        // still looking at it.
        let dir = project(&[("a.rs", &"alpha\n".repeat(60))]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");

        let area = t.area();
        let shown = SearchView::visible_rows(area);
        t.app.search.scroll = shown + 4;
        t.app.relayout();

        // The first row of the window that is a hit, whichever it is.
        let first = t.app.search.scroll;
        let row = (first..first + shown)
            .find(|row| matches!(t.app.search.rows.get(*row), Some(Line::Hit(..))))
            .expect("a hit is on screen");
        let rows = SearchView::rows_area(area);
        let offset = u16::try_from(row - first).unwrap();
        let opened_before = t.app.doc().buffer.path().is_some();

        // Where the widget puts the mark, asked of the widget.
        let view = t.app.search.view_rows(first..first + shown);
        let cell = SearchView::marker_area(area, &view, row - first, 0).expect("a mark is drawn");
        assert_eq!(cell.y, rows.y + offset, "on the row it belongs to");
        t.click(cell.x, cell.y);
        assert!(!t.app.search.excluded.is_empty(), "the mark struck the hit out");
        assert_eq!(
            t.app.doc().buffer.path().is_some(),
            opened_before,
            "and did not open the file instead"
        );
    }

    #[test]
    fn a_new_query_does_not_inherit_the_last_ones_strikes() {
        // Otherwise a line comes back already struck out, showing a mark
        // nobody clicked, and is quietly left alone by the replace.
        let dir = project(&[("a.rs", "alpha beta\n")]);
        let mut t = Tester::new(&dir);
        t.search("alpha");
        t.replace_with("omega");

        let area = t.area();
        let view = t.app.search.view_rows(0..SearchView::visible_rows(area));
        let cell = SearchView::marker_area(area, &view, 1, 0).expect("the hit has a mark");
        t.click(cell.x, cell.y);
        assert!(!t.app.search.excluded.is_empty(), "struck out under this query");

        // A different query over the same line.
        t.app.search.field = Field::Query;
        for _ in 0.."alpha".len() {
            t.app.handle(Event::Key(KeyEvent::from(KeyCode::Backspace)));
        }
        t.type_text("beta");
        t.settle_search();
        assert!(t.app.search.excluded.is_empty(), "the strikes went with the old list");
    }

    #[test]
    fn taking_a_replace_back_brings_the_open_buffer_with_it() {
        // Restoring the file and leaving the buffer showing the replacement
        // is a screen that disagrees with the disk, and the next save would
        // undo the undo.
        let dir = project(&[("a.rs", "one\ntwo\nalpha\nfour\n")]);
        let mut t = Tester::new(&dir);
        t.app.open_file(&dir.path().join("a.rs"));
        t.settle_jobs();

        t.search("alpha");
        t.replace_with("omega");
        t.apply();
        assert!(t.app.doc().buffer.text().to_string().contains("omega"));

        t.app.undo_file_op();
        t.settle_jobs();

        let on_disk = fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert_eq!(t.app.doc().buffer.text().to_string(), on_disk, "the buffer followed it back");
        assert!(!t.app.doc().buffer.is_modified(), "and is not holding an edit nobody made");
    }

    #[test]
    fn a_reload_leaves_the_caret_where_it_was() {
        // The scroll lives beside the buffer and the caret inside it, so a
        // straight swap leaves the view put and the caret at the top of the
        // file, somewhere off it.
        let dir = project(&[("a.rs", "one\ntwo\nalpha\nfour\nfive\n")]);
        let mut t = Tester::new(&dir);
        t.app.open_file(&dir.path().join("a.rs"));
        t.settle_jobs();
        let at = 9;
        t.app
            .doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));

        t.search("alpha");
        t.replace_with("omega");
        t.apply();

        assert_eq!(
            t.app.doc().buffer.selections().primary().head,
            at,
            "the caret stayed where it was"
        );
    }

    #[test]
    fn a_replace_waits_for_the_search_to_finish() {
        // Applying mid-walk rewrites whatever part of the project has arrived
        // and reports it as the whole job, then fills the panel with the rest.
        let dir = project(&[("a.rs", "alpha\n"), ("b.rs", "alpha\n")]);
        let mut t = Tester::new(&dir);
        t.app.run(crate::commands::Command::SearchProject);
        t.type_text("alpha");
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Tab)));
        t.type_text("omega");

        // Started, nothing answered yet.
        t.app.tick(Instant::now() + DEBOUNCE * 4);
        assert!(t.app.search.running, "the walk is in flight");

        t.app.apply_replace();
        t.settle_jobs();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "alpha\n",
            "nothing was written"
        );
        let said = t.app.message().unwrap();
        assert!(said.contains("still running"), "and it says why: {said}");

        // Once it has finished, the same press does the whole job.
        t.settle_search();
        t.apply();
        assert_eq!(fs::read_to_string(dir.path().join("a.rs")).unwrap(), "omega\n");
        assert_eq!(fs::read_to_string(dir.path().join("b.rs")).unwrap(), "omega\n");
    }

    #[test]
    fn a_line_that_still_matches_but_is_not_the_line_previewed_is_left_alone() {
        // The case the timestamp exists to catch, and the one the query
        // re-check cannot see: a rewrite that leaves the chosen line number
        // matching while making it a different line. Through the panel, so
        // the text the preview was taken from is what travels to the write.
        let dir = project(&[("a.rs", "let cat = 1;\nlet dog = 2;\nlet cat = 3;\n")]);
        let mut t = Tester::new(&dir);
        t.search("cat");
        t.replace_with("COW");

        // Keep only the first hit; the third is deliberately left out.
        let area = t.area();
        let view = t.app.search.view_rows(0..SearchView::visible_rows(area));
        let third = t
            .app
            .search
            .rows
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line, Line::Hit(..)))
            .nth(1)
            .map(|(row, _)| row)
            .expect("two hits");
        let cell = SearchView::marker_area(area, &view, third, 0).expect("it has a mark");
        t.click(cell.x, cell.y);

        // Somebody reorders the file: line 1 still matches `cat`, but it is
        // the line that was struck out. Its modified time is put back to what
        // it was, so the timestamp check cannot see the change and only the
        // content check stands between the preview and the write — which is
        // the whole point of the test.
        let path = dir.path().join("a.rs");
        let was = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "let cat = 3;\nlet dog = 2;\nlet cat = 1;\n").unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(was)).unwrap();
        drop(file);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            was,
            "the timestamp check has nothing to go on"
        );
        t.apply();

        assert_eq!(
            fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let cat = 3;\nlet dog = 2;\nlet cat = 1;\n",
            "nothing was written over a line nobody previewed"
        );
        let said = t.app.message().unwrap();
        assert!(said.contains("changed since the search"), "{said}");
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
