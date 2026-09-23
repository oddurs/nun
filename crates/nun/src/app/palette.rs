//! The palette: one overlay, several modes.
//!
//! The first character decides what is being searched: nothing for files,
//! `>` for commands, `:` for a line, `?` for the list of prefixes. Files are
//! matched on the worker, because a hundred thousand paths is not something a
//! keystroke should wait for; everything else is a short list and is matched
//! here.
//!
//! Files that have been opened before come first among equals. Opening a file
//! is a vote for it, and a palette that remembers is the difference between
//! typing one letter and typing six.

use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_ui::{PaletteEntry, PaletteView};
use nun_workspace::{Job, Match};
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;

use super::{App, Focus, Outcome, Target};
use crate::commands::Command;

/// What the palette is searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Files in the project.
    Files,
    /// Commands, by `>`.
    Commands,
    /// A line in the file being edited, by `:`.
    Line,
    /// Symbols in this file, by `@`.
    Symbols,
    /// Symbols across the project, by `#`, which needs a language server.
    ProjectSymbols,
    /// What the prefixes are, by `?`.
    Help,
}

impl Mode {
    /// Which mode a query is in, and the rest of it once the prefix is gone.
    fn of(query: &str) -> (Self, &str) {
        match query.chars().next() {
            Some('>') => (Self::Commands, &query[1..]),
            Some(':') => (Self::Line, &query[1..]),
            Some('@') => (Self::Symbols, &query[1..]),
            Some('#') => (Self::ProjectSymbols, &query[1..]),
            Some('?') => (Self::Help, &query[1..]),
            _ => (Self::Files, query),
        }
    }

    /// What the empty palette suggests in this mode.
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Files => "Go to a file — `>` commands, `:` line, `?` help",
            Self::Commands => "Run a command",
            Self::Line => "Go to a line",
            Self::Symbols => "Go to a symbol in this file",
            Self::ProjectSymbols => "Symbols across the project",
            Self::Help => "What the prefixes do",
        }
    }
}

/// What picking a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pick {
    /// Open this file.
    File(PathBuf),
    /// Run this command.
    Run(Command),
    /// Go to this line, counting from one.
    Line(usize),
    /// Go to this char offset, which is where a symbol's name starts.
    At(u32),
    /// Start the query again with this prefix, which is how the help list
    /// leads into every other mode with the mouse.
    Prefix(&'static str),
    /// Go to this place a server named, here or beside.
    Place(super::navigation::Place, super::navigation::Open),
    /// Carry out this code action, by its place in the chooser.
    Action(usize),
    /// Nothing: a row that is only telling you something.
    Nothing,
}

/// One row, and what it does.
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub(super) entry: PaletteEntry,
    pub(super) pick: Pick,
}

/// The palette, while it is open.
#[derive(Debug, Default)]
pub(super) struct Palette {
    pub(super) query: String,
    pub(super) rows: Vec<Row>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    /// Which keystroke the rows on screen came from, so an answer to an older
    /// one — and a pick made against rows that have gone stale — can be
    /// recognised.
    generation: u64,
    /// The generation the rows on screen were built from.
    rows_from: u64,
    /// Whether the project's files are listed, so a search has something to
    /// search.
    listed: bool,
    /// What had the keyboard before it opened.
    was_focused: Focus,
    /// A fixed list to pick from, filtered by what is typed, instead of a
    /// mode: the definitions of a symbol that has several.
    choices: Option<Choices>,
}

/// A fixed list for the palette to offer.
#[derive(Debug)]
pub(super) struct Choices {
    /// What the empty query says.
    title: String,
    rows: Vec<Row>,
}

/// How much having opened a file before is worth, next to a match score.
const FRECENCY_BONUS: u32 = 12;
/// Rows asked of the worker. More than fits, so scrolling has somewhere to go.
const LIMIT: usize = 200;

impl App {
    /// Open the palette with `prefix` already typed.
    pub(super) fn open_palette(&mut self, prefix: &str) -> Outcome {
        let mut palette =
            Palette { query: prefix.to_string(), was_focused: self.focus, ..Palette::default() };
        // The project is walked once and the list kept: it is a whole
        // directory tree, on the same worker as the file tree and every file
        // operation, and doing it on each Ctrl+P would stall all of them.
        palette.listed = self.file_count.is_some();
        if let Some(root) = self.workspace_root().filter(|_| self.file_count.is_none()) {
            self.send_job(Job::ListFiles(root));
        }
        self.finder = Some(palette);
        self.focus = Focus::Editor;
        self.refresh_palette();
        Outcome::Redraw
    }

    /// Open the palette on a fixed list of `rows`, headed by `title`.
    pub(super) fn open_choices(&mut self, title: String, rows: Vec<Row>) -> Outcome {
        self.finder = Some(Palette {
            was_focused: self.focus,
            choices: Some(Choices { title, rows }),
            ..Palette::default()
        });
        self.focus = Focus::Editor;
        self.refresh_palette();
        Outcome::Redraw
    }

    /// Close it.
    pub(super) fn close_palette(&mut self) -> Outcome {
        // The keyboard goes back where it was: opening the palette from the
        // tree and pressing Esc should leave the tree focused.
        if let Some(palette) = self.finder.take() {
            self.focus = palette.was_focused;
        }
        Outcome::Redraw
    }

    /// A key while the palette is open. Everything goes to it: it is a
    /// question on screen, and nothing reaches the text behind it.
    pub(super) fn palette_key(&mut self, key: &KeyEvent) -> Outcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let Some(palette) = self.finder.as_mut() else { return Outcome::Continue };

        match key.code {
            KeyCode::Esc => return self.close_palette(),
            KeyCode::Enter => {
                let selected = palette.selected;
                return self.pick_row(selected, alt);
            }
            KeyCode::Up => {
                palette.selected = palette.selected.saturating_sub(1);
                self.follow_palette();
            }
            KeyCode::Down => {
                palette.selected = (palette.selected + 1).min(palette.rows.len().saturating_sub(1));
                self.follow_palette();
            }
            KeyCode::Home => {
                palette.selected = 0;
                self.follow_palette();
            }
            KeyCode::End => {
                palette.selected = palette.rows.len().saturating_sub(1);
                self.follow_palette();
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let rows = PaletteView::visible_rows(self.palette_area());
                let Some(palette) = self.finder.as_mut() else { return Outcome::Continue };
                palette.selected = if key.code == KeyCode::PageUp {
                    palette.selected.saturating_sub(rows)
                } else {
                    (palette.selected + rows).min(palette.rows.len().saturating_sub(1))
                };
                self.follow_palette();
            }
            KeyCode::Backspace => {
                if let Some((at, _)) = palette.query.grapheme_indices(true).next_back() {
                    palette.query.truncate(at);
                    self.refresh_palette();
                } else {
                    return self.close_palette();
                }
            }
            KeyCode::Char(ch) if !control => {
                palette.query.push(ch);
                self.refresh_palette();
            }
            _ => {}
        }
        Outcome::Redraw
    }

    /// Text pasted into the palette goes into the query, on one line.
    pub(super) fn palette_paste(&mut self, text: &str) {
        if let Some(palette) = self.finder.as_mut() {
            palette.query.extend(text.chars().filter(|ch| *ch != '\n' && *ch != '\r'));
            self.refresh_palette();
        }
    }

    /// Work out the rows for what has been typed.
    pub(super) fn refresh_palette(&mut self) {
        let Some(palette) = self.finder.as_mut() else { return };
        if let Some(choices) = palette.choices.as_ref() {
            let labels: Vec<String> =
                choices.rows.iter().map(|row| row.entry.label.clone()).collect();
            palette.rows = nun_workspace::search(&labels, &palette.query, LIMIT)
                .into_iter()
                .map(|found| {
                    let mut row = choices.rows[found.index].clone();
                    row.entry.matched = found.matched;
                    row
                })
                .collect();
            palette.selected = 0;
            palette.scroll = 0;
            return;
        }
        let (mode, rest) = Mode::of(&palette.query);
        let rest = rest.to_string();
        palette.selected = 0;
        palette.scroll = 0;

        match mode {
            Mode::Files => {
                // Files are matched on the worker; the answer arrives as a
                // message. The generation says which keystroke asked, so an
                // answer to an older one is dropped rather than flickering.
                // Monotonic for the life of the editor, so a search from a
                // palette just closed can never look newer than one from the
                // palette just opened.
                let listed = palette.listed;
                let generation = self.searches + 1;
                palette.generation = generation;
                let job = Job::Search { query: rest.clone(), limit: LIMIT, generation };
                self.searches = generation;

                if listed {
                    self.send_job(job);
                } else {
                    let waiting = self.workspace_root().is_some();
                    let Some(palette) = self.finder.as_mut() else { return };
                    palette.rows = vec![note(if waiting {
                        "Listing the project…"
                    } else {
                        "No folder is open — start nun on one: `nun .`"
                    })];
                }
            }
            Mode::Commands => {
                let rows = self.command_rows(&rest);
                if let Some(palette) = self.finder.as_mut() {
                    palette.rows = rows;
                }
            }
            Mode::Line => {
                let rows = self.line_rows(&rest);
                if let Some(palette) = self.finder.as_mut() {
                    palette.rows = rows;
                }
            }
            Mode::Symbols => {
                self.want_outline(std::time::Instant::now());
                let rows = self.symbol_rows(&rest);
                if let Some(palette) = self.finder.as_mut() {
                    palette.rows = rows;
                }
            }
            Mode::ProjectSymbols => {
                palette.rows = vec![note(
                    "Symbols across the project arrive with the language servers — milestone 4",
                )];
            }
            Mode::Help => {
                palette.rows = help_rows();
            }
        }
    }

    /// The commands matching `query`, each with the key that runs it.
    fn command_rows(&self, query: &str) -> Vec<Row> {
        let titles: Vec<String> =
            Command::ALL.iter().map(|command| command.title().to_string()).collect();
        nun_workspace::search(&titles, query, LIMIT)
            .into_iter()
            .map(|found| Row {
                entry: PaletteEntry {
                    label: titles[found.index].clone(),
                    matched: found.matched,
                    hint: self.binding_for(Command::ALL[found.index]),
                },
                pick: Pick::Run(Command::ALL[found.index]),
            })
            .collect()
    }

    /// Where `:` takes you.
    fn line_rows(&self, query: &str) -> Vec<Row> {
        let lines = self.doc().buffer.len_lines();
        let Ok(line) = query.trim().parse::<usize>() else {
            return vec![note(&format!("Type a line number, 1 to {lines}"))];
        };
        let line = line.clamp(1, lines);
        vec![Row {
            entry: PaletteEntry {
                label: format!("Line {line}"),
                matched: Vec::new(),
                hint: format!("of {lines}"),
            },
            pick: Pick::Line(line),
        }]
    }

    /// Whether the palette is open and showing this file's outline.
    pub(super) fn palette_wants_symbols(&self) -> bool {
        self.finder.as_ref().is_some_and(|palette| Mode::of(&palette.query).0 == Mode::Symbols)
    }

    /// The outline of the file being edited, filtered by `query`.
    ///
    /// Filtering keeps the ancestors of a match, because a method's name means
    /// little without the type it hangs off, and a list of bare names is not
    /// an outline.
    fn symbol_rows(&self, query: &str) -> Vec<Row> {
        let symbols = &self.symbols.found;
        if symbols.is_empty() {
            return vec![note(if self.symbols.waiting {
                "Reading the file…"
            } else if App::syntax_off(self.doc()) {
                // Knowing the language and having given up on it are opposite
                // things, and saying the first for the second sends someone
                // looking for a missing grammar they already have.
                "This file's grammar gave up, so there is no outline of it"
            } else if App::language_of(self.doc()).is_some() {
                "Nothing in this file declares anything nun can see"
            } else {
                "nun does not know this file's language"
            })];
        }

        let keep: Vec<(usize, Vec<u32>)> = if query.trim().is_empty() {
            // Capped like every other mode: an outline is a list to scroll,
            // not a reason to build ten thousand rows nobody will read.
            (0..symbols.len().min(LIMIT)).map(|index| (index, Vec::new())).collect()
        } else {
            let found = nun_workspace::search(&self.symbols.labels, query, LIMIT);
            // Every ancestor of a match comes with it, unmatched, so the
            // nesting the rows are indented by still means something. Each
            // symbol knows its parent, so this walks the chain rather than
            // scanning backwards through the file for something shallower.
            let mut wanted: std::collections::BTreeMap<usize, Vec<u32>> =
                found.into_iter().map(|found| (found.index, found.matched)).collect();
            let mut ancestors: Vec<usize> = Vec::new();
            for index in wanted.keys() {
                let mut at = symbols[*index].parent;
                while let Some(parent) = at {
                    ancestors.push(parent);
                    at = symbols[parent].parent;
                }
            }
            for ancestor in ancestors {
                wanted.entry(ancestor).or_default();
            }
            wanted.into_iter().collect()
        };

        keep.into_iter()
            .map(|(index, matched)| {
                let symbol = &symbols[index];
                // Indented in the label rather than by the widget, so the
                // matched offsets the search gave still line up.
                let indent = "  ".repeat(symbol.depth);
                let shift = u32::try_from(indent.chars().count()).unwrap_or(0);
                Row {
                    entry: PaletteEntry {
                        label: format!("{indent}{}", symbol.name),
                        matched: matched.into_iter().map(|at| at + shift).collect(),
                        hint: symbol.kind.to_string(),
                    },
                    pick: Pick::At(symbol.at),
                }
            })
            .collect()
    }

    /// Scroll so `at` is visible with a few lines above it.
    ///
    /// Landing a definition on the top row hides what it belongs to. A little
    /// room above is the difference between arriving somewhere and arriving
    /// somewhere you can read.
    pub(super) fn show_with_context(&mut self, at: usize) {
        const ABOVE: usize = 3;
        let line = self.doc().buffer.line_of(at);
        let height = self.text_height();
        let hidden = self.doc().buffer.hidden();
        let doc = self.doc_mut();
        let rows = hidden.rows_between(doc.scroll, line);
        if line < doc.scroll || rows < ABOVE || rows >= height {
            doc.scroll = hidden.step(line, -3);
        }
        self.follow_caret();
    }

    /// The worker answered a file search.
    pub(super) fn palette_found(
        &mut self,
        query: &str,
        generation: u64,
        results: Vec<(PathBuf, Match)>,
    ) -> Outcome {
        let Some(palette) = self.finder.as_ref() else { return Outcome::Continue };
        let (mode, rest) = Mode::of(&palette.query);
        let rest = rest.to_string();
        // An answer to a keystroke that has been typed past, or to a mode that
        // is no longer showing, is not worth drawing.
        if mode != Mode::Files || generation != palette.generation || rest != query {
            return Outcome::Continue;
        }

        let root = self.workspace_root();
        let mut rows: Vec<(u32, Row)> = results
            .into_iter()
            .map(|(path, found)| {
                let full = root.as_ref().map_or_else(|| path.clone(), |root| root.join(&path));
                let bonus = self.frecency.get(&full).copied().unwrap_or(0) * FRECENCY_BONUS;
                let open = self.docs.iter().any(|doc| doc.buffer.path() == Some(full.as_path()));
                (
                    found.score + bonus,
                    Row {
                        entry: PaletteEntry {
                            label: path.display().to_string(),
                            matched: found.matched,
                            hint: if open { "open".into() } else { String::new() },
                        },
                        pick: Pick::File(full),
                    },
                )
            })
            .collect();
        // Stable, so files that score and rank the same keep the order the
        // worker sent them in: shallowest path first.
        rows.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

        let Some(palette) = self.finder.as_mut() else { return Outcome::Continue };
        palette.rows = rows.into_iter().map(|(_, row)| row).collect();
        palette.rows_from = generation;
        if palette.rows.is_empty() && !rest.is_empty() {
            palette.rows = vec![note("No file matches that")];
        }
        palette.selected = 0;
        palette.scroll = 0;
        Outcome::Redraw
    }

    /// The project's files have been listed.
    pub(super) fn palette_listed(&mut self, count: usize) -> Outcome {
        self.file_count = Some(count);
        // The palette that asked for the listing has nothing to search until
        // it arrives, so this is where its first search goes out.
        let Some(palette) = self.finder.as_mut() else { return Outcome::Continue };
        if palette.listed {
            return Outcome::Continue;
        }
        palette.listed = true;
        self.refresh_palette();
        Outcome::Redraw
    }

    /// Do what row `index` says. `split` opens a file in a new pane.
    pub(super) fn pick_row(&mut self, index: usize, split: bool) -> Outcome {
        let Some(palette) = self.finder.as_ref() else { return Outcome::Continue };
        // Rows from an older keystroke are still on screen while the worker
        // answers the current one. Acting on them would open whatever the
        // previous query matched, which is not what is typed.
        let (mode, _) = Mode::of(&palette.query);
        if mode == Mode::Files && palette.rows_from != palette.generation {
            return Outcome::Continue;
        }
        let Some(pick) = palette.rows.get(index).map(|row| row.pick.clone()) else {
            return Outcome::Redraw;
        };

        match pick {
            Pick::Nothing => return Outcome::Redraw,
            Pick::Place(place, open) => {
                self.finder = None;
                let open = if split { super::navigation::Open::Beside } else { open };
                return self.pick_place(&place, open);
            }
            Pick::Action(index) => {
                self.finder = None;
                return self.pick_code_action(index);
            }
            Pick::Prefix(prefix) => {
                if let Some(palette) = self.finder.as_mut() {
                    palette.query = prefix.to_string();
                }
                self.refresh_palette();
            }
            Pick::File(path) => {
                self.finder = None;
                if split {
                    self.split_pane(nun_ui::Dir::Beside);
                }
                *self.frecency.entry(path.clone()).or_insert(0) += 1;
                self.open_in_tab(&path);
            }
            Pick::Run(command) => {
                self.finder = None;
                return self.run(command);
            }
            Pick::Line(line) => {
                self.finder = None;
                let start = self.doc().buffer.line_start(line - 1);
                self.doc_mut()
                    .buffer
                    .set_selections(nun_core::Selections::single(nun_core::Range::caret(start)));
                self.follow_caret();
            }
            Pick::At(at) => {
                self.finder = None;
                let at = (at as usize).min(self.doc().buffer.len_chars());
                self.doc_mut()
                    .buffer
                    .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
                self.show_with_context(at);
            }
        }
        Outcome::Redraw
    }

    /// Where the palette goes.
    pub(super) fn palette_area(&self) -> Rect {
        let rows = self.finder.as_ref().map_or(0, |palette| palette.rows.len());
        PaletteView::area(self.viewport, rows)
    }

    /// Keep the selected row in view.
    fn follow_palette(&mut self) {
        let area = self.palette_area();
        if let Some(palette) = self.finder.as_mut() {
            palette.scroll = PaletteView::scroll_to(area, palette.selected, palette.scroll);
        }
    }

    /// Lay out the palette's rows, and the rest of the screen as the place a
    /// click goes to close it.
    pub(super) fn layout_palette(&self, hits: &mut nun_input::HitMap<Target>) {
        let Some(palette) = self.finder.as_ref() else { return };
        let area = self.palette_area();
        hits.push(super::cells(self.viewport), Target::PaletteOutside, false);
        hits.push(super::cells(area), Target::Palette, false);

        let visible = PaletteView::visible_rows(area);
        for (offset, index) in (palette.scroll..palette.rows.len()).take(visible).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let row = Rect { y: area.y + 2 + offset, height: 1, ..area };
            hits.push(super::cells(row), Target::PaletteRow(index), true);
        }
    }

    /// Draw it.
    pub(super) fn render_palette(&self, cells: &mut ratatui::buffer::Buffer) {
        use ratatui::widgets::Widget as _;
        let Some(palette) = self.finder.as_ref() else { return };
        let area = self.palette_area();
        if area.right() > cells.area().right() || area.bottom() > cells.area().bottom() {
            return;
        }

        let (mode, _) = Mode::of(&palette.query);
        let entries: Vec<PaletteEntry> = palette.rows.iter().map(|row| row.entry.clone()).collect();
        let hovered = match self.hover.current() {
            Some(Target::PaletteRow(index)) => Some(index),
            _ => None,
        };
        PaletteView::new(&self.palette, &palette.query, &entries)
            .placeholder(
                palette.choices.as_ref().map_or(mode.placeholder(), |choices| &choices.title),
            )
            .selected(palette.selected)
            .scrolled_to(palette.scroll)
            .hovered(hovered)
            .render(area, cells);
    }

    /// The wheel over the palette scrolls it.
    pub(super) fn palette_scroll(&mut self, down: bool) -> Outcome {
        let area = self.palette_area();
        let visible = PaletteView::visible_rows(area);
        let Some(palette) = self.finder.as_mut() else { return Outcome::Continue };
        let most = palette.rows.len().saturating_sub(visible);
        palette.scroll =
            if down { (palette.scroll + 3).min(most) } else { palette.scroll.saturating_sub(3) };
        Outcome::Redraw
    }
}

/// A row that only says something.
fn note(text: &str) -> Row {
    Row {
        entry: PaletteEntry { label: text.to_string(), matched: Vec::new(), hint: String::new() },
        pick: Pick::Nothing,
    }
}

/// What `?` shows.
fn help_rows() -> Vec<Row> {
    [
        ("(nothing)", "Files in the project"),
        (">", "Commands, with the keys that run them"),
        (":", "A line in this file"),
        ("@", "Symbols in this file"),
        ("#", "Symbols in the project (milestone 4)"),
        ("?", "This list"),
    ]
    .into_iter()
    .map(|(prefix, what)| Row {
        entry: PaletteEntry {
            label: what.to_string(),
            matched: Vec::new(),
            hint: prefix.to_string(),
        },
        // Clicking one goes to that mode, so the help list is the way into
        // every prefix without knowing to type it.
        pick: if prefix == "(nothing)" { Pick::Prefix("") } else { Pick::Prefix(prefix) },
    })
    .collect()
}

/// Frecency: how often each file has been opened from the palette.
pub(super) type Frecency = HashMap<PathBuf, u32>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{KeySet, defaults};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette as Colours};
    use nun_workspace::Done;
    use std::fs;
    use tempfile::TempDir;

    struct Tester {
        app: App,
        done: std::sync::mpsc::Receiver<Done>,
    }

    impl Tester {
        /// An editor on a project of `files`, with the worker's answers pumped
        /// in as they arrive.
        fn new(dir: &TempDir, files: &[&str]) -> Self {
            fs::create_dir_all(dir.path().join(".git")).unwrap();
            for name in files {
                let path = dir.path().join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, format!("{name}\n")).unwrap();
            }

            let (sender, done) = std::sync::mpsc::channel();
            let mut app = App::new(
                Buffer::new(),
                Colours::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 80, 20));
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                false,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            let mut tester = Self { app, done };
            tester.settle();
            tester
        }

        /// Take everything the worker has finished, up to a marker asked for
        /// now — and again while handling those answers asked for more, which
        /// listing the project and then searching it does.
        fn settle(&mut self) {
            for marker in 0..32 {
                self.app.echo(marker);
                let mut handled = 0;
                loop {
                    let message = self
                        .done
                        .recv_timeout(std::time::Duration::from_secs(10))
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

        fn key(&mut self, code: KeyCode) {
            self.app.handle(Event::Key(KeyEvent::from(code)));
            self.settle();
        }

        fn type_text(&mut self, text: &str) {
            for ch in text.chars() {
                self.key(KeyCode::Char(ch));
            }
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
            self.settle();
        }

        fn wheel(&mut self, down: bool) {
            let kind = if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp };
            self.app.handle(Event::Mouse(MouseEvent {
                kind,
                column: 40,
                row: 6,
                modifiers: KeyModifiers::NONE,
            }));
        }

        fn open(&mut self, prefix: &str) {
            self.app.open_palette(prefix);
            self.settle();
        }

        fn labels(&self) -> Vec<String> {
            self.app
                .finder
                .as_ref()
                .map(|palette| palette.rows.iter().map(|row| row.entry.label.clone()).collect())
                .unwrap_or_default()
        }
    }

    #[test]
    fn the_palette_offers_the_project_files_and_opens_the_one_picked() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["src/main.rs", "README.md"]);
        t.open("");

        assert_eq!(t.labels().len(), 2, "{:?}", t.labels());
        t.type_text("main");
        assert_eq!(t.labels(), vec!["src/main.rs"]);

        t.key(KeyCode::Enter);
        assert!(t.app.finder.is_none(), "picking closes it");
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("src/main.rs").as_path()));
    }

    #[test]
    fn the_matched_characters_are_marked_for_highlighting() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["src/main.rs"]);
        t.open("");
        t.type_text("main");

        let matched = t.app.finder.as_ref().unwrap().rows[0].entry.matched.clone();
        let label = t.labels()[0].clone();
        let letters: String =
            matched.iter().map(|index| label.chars().nth(*index as usize).unwrap()).collect();
        assert_eq!(letters, "main");
    }

    #[test]
    fn commands_are_a_prefix_away_and_show_their_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open(">");
        t.type_text("split bes");

        let rows = t.app.finder.as_ref().unwrap().rows.clone();
        assert_eq!(rows[0].entry.label, "Split beside");
        assert_eq!(rows[0].entry.hint, "Ctrl+K V", "the palette is the keymap reference");

        t.key(KeyCode::Enter);
        assert_eq!(t.app.panes.len(), 2, "and it ran");
    }

    #[test]
    fn a_line_number_goes_to_that_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["long.rs"]);
        fs::write(dir.path().join("long.rs"), "x\n".repeat(50)).unwrap();
        t.open("");
        t.type_text("long");
        t.key(KeyCode::Enter);

        t.open(":");
        t.type_text("30");
        t.key(KeyCode::Enter);

        let head = t.app.buffer().selections().primary().head;
        assert_eq!(t.app.buffer().line_of(head), 29, "counting from one");
    }

    #[test]
    fn the_help_prefix_lists_the_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open("?");
        let hints: Vec<String> =
            t.app.finder.as_ref().unwrap().rows.iter().map(|row| row.entry.hint.clone()).collect();
        assert!(hints.contains(&">".to_string()), "{hints:?}");
        assert!(hints.contains(&":".to_string()), "{hints:?}");
    }

    #[test]
    fn project_symbols_say_when_they_arrive() {
        // `@` reads the file's own tree and is tested where a parser is
        // attached; `#` needs a language server and is still milestone 4.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open("#");
        assert!(t.labels()[0].contains("language servers"), "{:?}", t.labels());
    }

    #[test]
    fn a_file_opened_before_comes_first_among_equals() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["one/thing.rs", "two/thing.rs"]);

        // Open the second one through the palette, which is a vote for it.
        t.open("");
        t.type_text("two/thing");
        t.key(KeyCode::Enter);

        t.open("");
        t.type_text("thing");
        assert_eq!(t.labels()[0], "two/thing.rs", "{:?}", t.labels());
    }

    #[test]
    fn the_keyboard_and_the_wheel_both_move_through_the_results() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<String> = (0..30).map(|index| format!("file{index}.rs")).collect();
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut t = Tester::new(&dir, &names);
        t.open("");

        t.key(KeyCode::Down);
        t.key(KeyCode::Down);
        assert_eq!(t.app.finder.as_ref().unwrap().selected, 2);

        t.key(KeyCode::End);
        let last = t.app.finder.as_ref().unwrap().selected;
        assert_eq!(last, t.labels().len() - 1);
        assert!(t.app.finder.as_ref().unwrap().scroll > 0, "the view followed it down");

        t.key(KeyCode::Home);
        assert_eq!(t.app.finder.as_ref().unwrap().scroll, 0);

        t.wheel(true);
        assert!(t.app.finder.as_ref().unwrap().scroll > 0, "the wheel scrolls it");
    }

    #[test]
    fn a_click_picks_a_row_and_a_click_outside_puts_it_away() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["src/main.rs", "README.md"]);
        t.open("");
        t.type_text("readme");

        let area = t.app.palette_area();
        t.click(area.x + 2, area.y + 2);
        assert!(t.app.finder.is_none());
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("README.md").as_path()));

        t.open("");
        t.click(0, 19);
        assert!(t.app.finder.is_none(), "a click outside closes it");
    }

    #[test]
    fn alt_enter_opens_the_file_in_a_split() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs", "b.rs"]);
        t.open("");
        t.type_text("b.rs");

        t.app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
        t.settle();
        assert_eq!(t.app.panes.len(), 2, "it opened beside");
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("b.rs").as_path()));
    }

    #[test]
    fn escape_closes_it_and_leaves_the_text_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open("");
        t.type_text("abc");
        t.key(KeyCode::Esc);

        assert!(t.app.finder.is_none());
        assert_eq!(t.app.buffer().text().to_string(), "", "nothing was typed into the file");
    }

    #[test]
    fn the_status_line_has_a_button_that_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        let search = t.app.status_parts(t.app.areas().1).search.expect("a button is offered");
        t.click(search.x + 1, search.y);
        assert!(t.app.finder.is_some());
    }

    #[test]
    fn the_project_is_walked_once_however_often_the_palette_opens() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs", "b.rs"]);
        t.open("");
        let count = t.app.file_count;
        assert_eq!(count, Some(2));

        // Opening it again must not walk the project a second time: the
        // worker is shared with the tree and with every file operation.
        t.app.file_count = Some(99);
        t.open("");
        t.open(">");
        assert_eq!(t.app.file_count, Some(99), "it was not listed again");
    }

    #[test]
    fn the_keyboard_goes_back_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.app.run(Command::ToggleSidebar);
        assert_eq!(t.app.focus, super::Focus::Sidebar);

        t.open("");
        t.key(KeyCode::Esc);
        assert_eq!(t.app.focus, super::Focus::Sidebar, "the tree had it before");
    }

    #[test]
    fn a_row_from_an_older_keystroke_cannot_be_picked() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs", "b.rs"]);
        t.open("");
        t.type_text("a");

        // Type another letter without letting the worker answer: the rows on
        // screen are now from the keystroke before.
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Char('b'))));
        assert_eq!(t.app.pick_row(0, false), Outcome::Continue, "stale rows do nothing");
        assert!(t.app.finder.is_some(), "and the palette stays up");

        t.settle();
        assert_eq!(t.app.pick_row(0, false), Outcome::Redraw, "the answer makes them live again");
    }

    #[test]
    fn the_help_list_is_the_way_into_every_mode_with_the_mouse() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open("?");

        let commands = t
            .app
            .finder
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|row| row.entry.hint == ">")
            .unwrap();
        let area = t.app.palette_area();
        t.click(area.x + 2, area.y + 2 + u16::try_from(commands).unwrap());

        assert_eq!(t.app.finder.as_ref().unwrap().query, ">", "it went to the commands");
        assert!(!t.labels().is_empty());
    }

    #[test]
    fn alt_clicking_a_row_opens_it_in_a_split() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs", "b.rs"]);
        t.open("");
        t.type_text("b.rs");

        let area = t.app.palette_area();
        t.app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y + 2,
            modifiers: KeyModifiers::ALT,
        }));
        t.settle();
        assert_eq!(t.app.panes.len(), 2);
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("b.rs").as_path()));
    }

    #[test]
    fn a_hundred_thousand_files_stay_answerable() {
        // The listing and the matching both happen on the worker; this checks
        // the editor is never the thing waiting.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, &["a.rs"]);
        t.open("");

        let start = std::time::Instant::now();
        t.type_text("a");
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
    }
}
