//! The file tree: where it goes, what clicking it does, and the file
//! operations it offers.
//!
//! Every operation has a mouse path. Rows open on click, folders expand on
//! click, a row dragged onto a folder moves there, the header's buttons create
//! files and folders and show ignored files, a right-click offers the rest,
//! and each change is undoable from the toast it leaves in the status line.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use nun_ui::{MenuItem, TreeButton, TreeView};
use nun_workspace::{Change, Done, FileTree, Job, Jobs, Kind, Watcher};
use ratatui::layout::Rect;

use super::prompt::{Prompt, Purpose};
use super::{App, Focus, Outcome, Target};
use crate::commands::Command;

/// Narrowest the sidebar can be dragged to.
const MIN_WIDTH: u16 = 12;

/// The file tree and everything that goes with it.
#[derive(Debug)]
pub(super) struct Sidebar {
    pub(super) tree: FileTree,
    /// The worker that does the reading and the writing. Nothing here touches
    /// the filesystem itself: a listing or a move can take as long as the disk
    /// likes, and the editor has frames to draw.
    jobs: Jobs,
    /// Listings already asked for, so a redraw does not ask again.
    requested: BTreeSet<PathBuf>,
    /// A path to select once the listings that would show it arrive.
    reveal_after: Option<PathBuf>,
    watcher: Option<Watcher>,
    watched: BTreeSet<PathBuf>,
    pub(super) visible: bool,
    /// Columns, including the divider on its right edge.
    pub(super) width: u16,
    pub(super) scroll: usize,
    pub(super) selected: Option<usize>,
    pub(super) drag: Option<TreeDrag>,
    pub(super) resizing: bool,
}

/// A row being carried somewhere.
#[derive(Debug, Clone, Copy)]
pub(super) struct TreeDrag {
    row: usize,
    press: (u16, u16),
    /// The folder row it would be dropped into, once it has moved.
    pub(super) target: Option<usize>,
    moved: bool,
}

/// A menu on screen, and what each of its items does.
#[derive(Debug, Clone)]
pub(super) struct OpenMenu {
    pub(super) area: Rect,
    pub(super) items: Vec<MenuItem>,
    pub(super) commands: Vec<Command>,
}

impl Sidebar {
    /// A sidebar over `root`, deleting into `trash`, reporting finished jobs
    /// through `report`.
    pub(super) fn new(
        root: PathBuf,
        trash: PathBuf,
        visible: bool,
        report: Box<dyn Fn(Done) + Send + 'static>,
    ) -> Self {
        Self {
            tree: FileTree::open(root),
            jobs: Jobs::new(trash, report),
            requested: BTreeSet::new(),
            reveal_after: None,
            watcher: None,
            watched: BTreeSet::new(),
            visible,
            width: 30,
            scroll: 0,
            selected: None,
            drag: None,
            resizing: false,
        }
    }

    /// Watch the tree with `watcher` from now on.
    pub(super) fn attach(&mut self, watcher: Watcher) {
        self.watcher = Some(watcher);
        self.sync_watches();
    }

    /// Ask for every listing the tree is waiting for and has not been asked
    /// for yet.
    fn request_listings(&mut self) {
        let show = self.tree.show_ignored();
        for dir in self.tree.wanted() {
            if self.requested.insert(dir.clone()) {
                self.jobs.list(dir, show);
            }
        }
    }

    /// Ask for a job to be done.
    fn send(&self, job: Job) {
        self.jobs.send(job);
    }

    /// Watch exactly the directories that are expanded — the ones whose
    /// changes would show — and nothing else.
    fn sync_watches(&mut self) {
        let Some(watcher) = &self.watcher else { return };
        let wanted: BTreeSet<PathBuf> = self.tree.expanded_dirs().into_iter().collect();
        for gone in self.watched.difference(&wanted) {
            watcher.unwatch(gone.clone());
        }
        for new in wanted.difference(&self.watched) {
            watcher.watch(new.clone());
        }
        self.watched = wanted;
    }

    /// The folder new entries go into: the selected folder, the folder of the
    /// selected file, or the root.
    fn target_dir(&self) -> PathBuf {
        let root = self.tree.root().to_path_buf();
        let Some(row) = self.selected.and_then(|index| self.tree.rows().get(index)) else {
            return root;
        };
        match row.kind {
            Kind::Dir => row.path.clone(),
            _ => row.path.parent().map_or(root, Path::to_path_buf),
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected.and_then(|index| self.tree.rows().get(index)).map(|row| row.path.clone())
    }

    /// Keep the selected row inside the scrolled window.
    fn follow_selection(&mut self, visible: usize) {
        let Some(selected) = self.selected else { return };
        if selected < self.scroll {
            self.scroll = selected;
        } else if visible > 0 && selected >= self.scroll + visible {
            self.scroll = selected + 1 - visible;
        }
    }

    /// Whether `row` is a folder `from` may be dropped into: not itself, and
    /// not anywhere inside itself.
    fn accepts(&self, from: usize, row: usize) -> bool {
        let rows = self.tree.rows();
        let (Some(source), Some(target)) = (rows.get(from), rows.get(row)) else { return false };
        target.kind == Kind::Dir
            && !target.path.starts_with(&source.path)
            && source.path.parent() != Some(target.path.as_path())
    }
}

impl App {
    /// The sidebar's rectangle, when it is shown.
    pub(super) fn sidebar_area(&self) -> Option<Rect> {
        let sidebar = self.sidebar.as_ref().filter(|sidebar| sidebar.visible)?;
        let height = self.viewport.height.saturating_sub(1);
        // Never so wide the editor has nothing left.
        let width = sidebar.width.min(self.viewport.width / 2);
        Some(Rect::new(self.viewport.x, self.viewport.y, width, height))
    }

    /// The tree part of the sidebar, without its divider column.
    pub(super) fn tree_area(&self) -> Option<Rect> {
        self.sidebar_area().map(|area| Rect { width: area.width.saturating_sub(1), ..area })
    }

    /// Lay out the sidebar's hit regions.
    pub(super) fn layout_sidebar(&self, hits: &mut nun_input::HitMap<Target>) {
        let (Some(sidebar), Some(area), Some(tree)) =
            (self.sidebar.as_ref(), self.sidebar_area(), self.tree_area())
        else {
            return;
        };

        hits.push(super::cells(tree), Target::TreeEmpty, false);
        hits.push(super::cells(TreeView::header_area(tree)), Target::TreeHeader, false);
        for button in TreeButton::ALL {
            if let Some(cell) = TreeView::button_area(tree, button) {
                hits.push(super::cells(cell), Target::TreeButton(button), true);
            }
        }
        let rows = TreeView::rows_area(tree);
        let count = sidebar.tree.rows().len();
        for (offset, index) in (sidebar.scroll..count).take(usize::from(rows.height)).enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let line = Rect { y: rows.y + offset, height: 1, ..rows };
            hits.push(super::cells(line), Target::TreeRow(index), true);
        }
        let edge = Rect { x: area.right().saturating_sub(1), width: 1, ..area };
        hits.push(super::cells(edge), Target::SidebarEdge, false);
    }

    /// Show the sidebar and give it the keyboard; if it has the keyboard
    /// already, hide it.
    pub(super) fn toggle_sidebar(&mut self) -> Outcome {
        let Some(sidebar) = self.sidebar.as_mut() else {
            self.message = Some("No folder is open. Start nun on a folder: `nun .`".into());
            return Outcome::Redraw;
        };
        if sidebar.visible && self.focus == Focus::Sidebar {
            sidebar.visible = false;
            self.focus = Focus::Editor;
        } else {
            sidebar.visible = true;
            self.focus = Focus::Sidebar;
        }
        Outcome::Redraw
    }

    /// The left button went down somewhere in the sidebar.
    pub(super) fn sidebar_press(&mut self, mouse: MouseEvent, target: Target) -> Outcome {
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        self.focus = Focus::Sidebar;
        match target {
            Target::TreeRow(row) => {
                sidebar.selected = Some(row);
                sidebar.drag = Some(TreeDrag {
                    row,
                    press: (mouse.column, mouse.row),
                    target: None,
                    moved: false,
                });
            }
            Target::TreeButton(button) => return self.tree_button(button),
            Target::SidebarEdge => sidebar.resizing = true,
            _ => sidebar.selected = None,
        }
        Outcome::Redraw
    }

    /// The pointer moved with the button held, after a press in the sidebar.
    /// Returns `None` when the sidebar has no drag of its own going.
    pub(super) fn sidebar_drag(&mut self, column: u16, row: u16) -> Option<Outcome> {
        let hit = self.hits.at(column, row).map(|hit| hit.target);
        let sidebar = self.sidebar.as_mut()?;

        if sidebar.resizing {
            sidebar.width = (column + 1).clamp(MIN_WIDTH, self.viewport.width / 2);
            return Some(Outcome::Redraw);
        }

        let mut drag = sidebar.drag?;
        if (column, row) != drag.press {
            drag.moved = true;
        }
        drag.target = match hit {
            Some(Target::TreeRow(over)) if drag.moved => {
                // Over a file, the drop goes into the folder the file is in.
                let folder = match sidebar.tree.rows()[over].kind {
                    Kind::Dir => Some(over),
                    _ => sidebar.tree.rows()[over]
                        .path
                        .parent()
                        .and_then(|parent| sidebar.tree.row_of(parent)),
                };
                folder.filter(|&folder| sidebar.accepts(drag.row, folder))
            }
            _ => None,
        };
        sidebar.drag = Some(drag);
        Some(Outcome::Redraw)
    }

    /// The button came up after a press in the sidebar. `None` when the
    /// sidebar had nothing going.
    pub(super) fn sidebar_release(&mut self) -> Option<Outcome> {
        let sidebar = self.sidebar.as_mut()?;
        if std::mem::take(&mut sidebar.resizing) {
            return Some(Outcome::Redraw);
        }
        let drag = sidebar.drag.take()?;

        if drag.moved {
            // Dropped somewhere that takes it: move it there. Anywhere else, a
            // drag that went nowhere does nothing.
            if let Some(target) = drag.target {
                let rows = sidebar.tree.rows();
                let (from, into) = (rows[drag.row].path.clone(), rows[target].path.clone());
                sidebar.send(Job::MoveInto { from, dir: into });
            }
            return Some(Outcome::Redraw);
        }

        // A click: folders open and close, files open in the editor.
        let row = &sidebar.tree.rows()[drag.row];
        let (path, kind) = (row.path.clone(), row.kind);
        if kind == Kind::Dir {
            sidebar.tree.toggle(drag.row);
            sidebar.request_listings();
            sidebar.sync_watches();
        } else {
            // Opening a file moves on to it: the next thing typed belongs to
            // the text, not to the tree.
            self.open_file(&path);
            self.focus = Focus::Editor;
        }
        Some(Outcome::Redraw)
    }

    /// A right-click in the sidebar: select what was clicked and offer what
    /// can be done to it.
    pub(super) fn sidebar_menu(&mut self, mouse: MouseEvent, target: Target) -> Outcome {
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        self.focus = Focus::Sidebar;
        sidebar.selected = match target {
            Target::TreeRow(row) => Some(row),
            _ => None,
        };
        let mut commands = vec![Command::NewFile, Command::NewFolder];
        if sidebar.selected.is_some() {
            commands.extend([Command::Rename, Command::Delete]);
        }
        commands.push(Command::ToggleIgnored);

        let items: Vec<MenuItem> = commands
            .iter()
            .map(|&command| MenuItem {
                label: self.menu_label(command),
                hint: self.binding_for(command),
            })
            .collect();
        let area = nun_ui::Menu::area(&items, mouse.column, mouse.row, self.viewport);
        self.menu = Some(OpenMenu { area, items, commands });
        Outcome::Redraw
    }

    fn menu_label(&self, command: Command) -> String {
        match command {
            Command::ToggleIgnored if self.showing_ignored() => "Hide ignored files".into(),
            Command::ToggleIgnored => "Show ignored files".into(),
            _ => command.title().to_string(),
        }
    }

    pub(super) fn showing_ignored(&self) -> bool {
        self.sidebar.as_ref().is_some_and(|sidebar| sidebar.tree.show_ignored())
    }

    /// The wheel turned over the sidebar.
    pub(super) fn sidebar_scroll(&mut self, down: bool) -> Outcome {
        let visible = self.tree_area().map_or(0, TreeView::visible_rows);
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        let last = sidebar.tree.rows().len().saturating_sub(visible);
        sidebar.scroll =
            if down { (sidebar.scroll + 3).min(last) } else { sidebar.scroll.saturating_sub(3) };
        Outcome::Redraw
    }

    fn tree_button(&mut self, button: TreeButton) -> Outcome {
        match button {
            TreeButton::NewFile => self.run(Command::NewFile),
            TreeButton::NewDir => self.run(Command::NewFolder),
            TreeButton::ToggleIgnored => self.run(Command::ToggleIgnored),
        }
    }

    /// A key while the sidebar has the keyboard, that no binding took.
    pub(super) fn sidebar_key(&mut self, key: &KeyEvent) -> Outcome {
        let visible = self.tree_area().map_or(0, TreeView::visible_rows);
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        // The first key to arrive with nothing selected starts at the top.
        let navigating = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Enter
        );
        if sidebar.selected.is_none() && navigating && !sidebar.tree.rows().is_empty() {
            sidebar.selected = Some(0);
            if matches!(key.code, KeyCode::Down) {
                return Outcome::Redraw;
            }
        }
        let count = sidebar.tree.rows().len();
        let current = sidebar.selected;
        let last = count.saturating_sub(1);

        let selected = match key.code {
            KeyCode::Up => current.map_or(0, |row| row.saturating_sub(1)),
            KeyCode::Down => current.map_or(0, |row| (row + 1).min(last)),
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::PageUp => current.unwrap_or(0).saturating_sub(visible),
            KeyCode::PageDown => (current.unwrap_or(0) + visible).min(last),
            KeyCode::Right | KeyCode::Left | KeyCode::Enter => {
                return self.sidebar_open_or_fold(key.code);
            }
            KeyCode::Delete | KeyCode::Backspace
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return self.run(Command::Delete);
            }
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                return Outcome::Redraw;
            }
            _ => return Outcome::Continue,
        };
        if count > 0 {
            sidebar.selected = Some(selected);
            sidebar.follow_selection(visible);
        }
        Outcome::Redraw
    }

    fn sidebar_open_or_fold(&mut self, code: KeyCode) -> Outcome {
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        let Some(index) = sidebar.selected else { return Outcome::Continue };
        let Some(row) = sidebar.tree.rows().get(index).cloned() else { return Outcome::Continue };

        match (code, row.kind, row.expanded) {
            (KeyCode::Enter | KeyCode::Right, Kind::Dir, false) => {
                sidebar.tree.expand(&row.path);
                sidebar.request_listings();
            }
            (KeyCode::Enter | KeyCode::Left, Kind::Dir, true) => {
                sidebar.tree.collapse(&row.path);
            }
            (KeyCode::Right, Kind::Dir, true) => {
                // Already open: step into its first entry.
                if index + 1 < sidebar.tree.rows().len() {
                    sidebar.selected = Some(index + 1);
                }
            }
            (KeyCode::Left, _, _) => {
                // Step out to the folder this is in.
                if let Some(parent) =
                    row.path.parent().and_then(|parent| sidebar.tree.row_of(parent))
                {
                    sidebar.selected = Some(parent);
                }
            }
            (KeyCode::Enter | KeyCode::Right, _, _) => {
                self.open_file(&row.path);
                self.focus = Focus::Editor;
                return Outcome::Redraw;
            }
            _ => {}
        }
        sidebar.sync_watches();
        Outcome::Redraw
    }

    // ── file operations ─────────────────────────────────────────────────────

    /// Ask for the name of a new file or folder in the selected folder.
    pub(super) fn ask_new(&mut self, folder: bool) -> Outcome {
        let Some(sidebar) = self.sidebar.as_ref() else { return self.toggle_sidebar() };
        let dir = sidebar.target_dir();
        let shown = super::display_path(Some(&dir));
        let (purpose, what) = if folder {
            (Purpose::NewFolder(dir), "New folder in")
        } else {
            (Purpose::NewFile(dir), "New file in")
        };
        self.prompt = Some(Prompt::name(purpose, format!("{what} {shown}/:"), ""));
        Outcome::Redraw
    }

    /// Ask for a new name for the selected entry, or the open file.
    pub(super) fn ask_rename(&mut self) -> Outcome {
        let Some(path) = self.op_target() else {
            self.message = Some("Select a file or folder to rename.".into());
            return Outcome::Redraw;
        };
        let name =
            path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        self.prompt =
            Some(Prompt::name(Purpose::Rename(path), format!("Rename {name} to:"), &name));
        Outcome::Redraw
    }

    /// Move the selected entry to the trash. No confirmation: it is undoable,
    /// and the toast says so.
    pub(super) fn delete_selected(&mut self) -> Outcome {
        let Some(path) = self.op_target() else {
            self.message = Some("Select a file or folder to delete.".into());
            return Outcome::Redraw;
        };
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        sidebar.send(Job::Delete(path));
        Outcome::Redraw
    }

    pub(super) fn toggle_ignored(&mut self) -> Outcome {
        let Some(sidebar) = self.sidebar.as_mut() else { return self.toggle_sidebar() };
        sidebar.reveal_after = sidebar.selected_path();
        sidebar.tree.set_show_ignored(!sidebar.tree.show_ignored());
        sidebar.request_listings();
        Outcome::Redraw
    }

    /// Undo the last file operation. Offered by the toast after each one.
    pub(super) fn undo_file_op(&mut self) -> Outcome {
        let Some(sidebar) = self.sidebar.as_ref() else { return Outcome::Continue };
        sidebar.send(Job::Undo);
        Outcome::Redraw
    }

    /// Redo the last undone file operation.
    pub(super) fn redo_file_op(&mut self) -> Outcome {
        let Some(sidebar) = self.sidebar.as_ref() else { return Outcome::Continue };
        sidebar.send(Job::Redo);
        Outcome::Redraw
    }

    /// A filesystem job finished.
    pub(super) fn job_done(&mut self, done: Done) -> Outcome {
        match done {
            Done::Listed { dir, entries } => self.listing_arrived(&dir, entries),
            // Only ever asked for by a caller waiting for the worker to catch
            // up; there is nothing to do when it comes back.
            Done::Echo(_) => Outcome::Continue,
            Done::Changed(change) => {
                self.after_op(&change);
                Outcome::Redraw
            }
            Done::Failed(error) => {
                self.message = Some(error);
                self.undo_offer = false;
                self.open_when_created = None;
                Outcome::Redraw
            }
            Done::Nothing { redo } => {
                let what = if redo { "redo" } else { "undo" };
                self.message = Some(format!("Nothing to {what} in the file tree."));
                self.undo_offer = false;
                Outcome::Redraw
            }
        }
    }

    fn listing_arrived(
        &mut self,
        dir: &Path,
        entries: Result<Vec<nun_workspace::Entry>, String>,
    ) -> Outcome {
        let visible = self.tree_area().map_or(0, TreeView::visible_rows);
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        sidebar.requested.remove(dir);

        let selected = sidebar.selected_path();
        sidebar.tree.apply_listing(dir, entries);
        sidebar.selected = selected.and_then(|path| sidebar.tree.row_of(&path));

        // A listing can expose the next directory down, which is how revealing
        // something deep walks its way in as the answers come back.
        if let Some(path) = sidebar.reveal_after.clone()
            && let Some(row) = sidebar.tree.reveal(&path)
        {
            sidebar.selected = Some(row);
            sidebar.reveal_after = None;
        }
        sidebar.request_listings();
        sidebar.sync_watches();
        sidebar.follow_selection(visible);
        Outcome::Redraw
    }

    /// What rename and delete act on: the selected row when the tree has the
    /// keyboard or a selection, otherwise the open file.
    fn op_target(&self) -> Option<PathBuf> {
        let sidebar = self.sidebar.as_ref()?;
        sidebar.selected_path().or_else(|| {
            self.buffer
                .path()
                .filter(|path| path.starts_with(sidebar.tree.root()))
                .map(Path::to_path_buf)
        })
    }

    pub(super) fn create(&mut self, dir: &Path, name: &str, folder: bool) {
        let Some(sidebar) = self.sidebar.as_mut() else { return };
        if name.is_empty() || name.contains(std::path::is_separator) || name == "." || name == ".."
        {
            self.message = Some(format!("`{name}` is not a valid name."));
            return;
        }
        let path = dir.join(name);
        sidebar.send(if folder {
            Job::CreateDir(path.clone())
        } else {
            Job::CreateFile(path.clone())
        });
        // A new file is for writing in, so it opens once it exists.
        self.open_when_created = (!folder).then_some(path);
    }

    pub(super) fn rename(&mut self, path: &Path, name: &str) {
        let Some(sidebar) = self.sidebar.as_ref() else { return };
        sidebar.send(Job::Rename { from: path.to_path_buf(), name: name.to_string() });
    }

    /// Bring the tree, the open buffer and the status line up to date after
    /// a file operation, its undo, or its redo.
    fn after_op(&mut self, change: &Change) {
        let Some(sidebar) = self.sidebar.as_mut() else { return };
        for dir in &change.dirs {
            sidebar.tree.refresh_dir(dir);
        }
        // The row shows up once the refreshed listings arrive.
        sidebar.reveal_after.clone_from(&change.path);
        sidebar.request_listings();
        sidebar.sync_watches();

        // The open file moved: follow it, so the next save goes to the new
        // place instead of recreating the old one.
        if let nun_workspace::Operation::Rename { from, to }
        | nun_workspace::Operation::Move { from, to } = &change.operation
        {
            let (from, to) = if change.undone { (to, from) } else { (from, to) };
            if let Some(open) = self.buffer.path().map(Path::to_path_buf)
                && let Ok(rest) = open.strip_prefix(from)
            {
                let moved = if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
                self.buffer.set_path(moved);
            }
        }

        self.message = Some(change.to_string());
        self.undo_offer = true;
        self.last_undone = change.undone;

        if let Some(path) = self.open_when_created.take()
            && change.path.as_deref() == Some(path.as_path())
        {
            self.open_file(&path);
            self.focus = Focus::Editor;
        }
    }

    /// Something changed on disk in a watched folder.
    pub(super) fn files_changed(&mut self, dir: &Path, error: Option<String>) -> Outcome {
        if let Some(error) = error {
            self.warn(format!("The file tree is no longer live for {}: {error}", dir.display()));
            return Outcome::Redraw;
        }
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        if !sidebar.tree.refresh_dir(dir) {
            return Outcome::Continue;
        }
        sidebar.request_listings();
        Outcome::Redraw
    }

    // ── opening files ───────────────────────────────────────────────────────

    /// Open `path` in the editor, asking first if the open buffer has
    /// unsaved changes.
    pub(super) fn open_file(&mut self, path: &Path) {
        if self.buffer.path() == Some(path) {
            return;
        }
        if self.buffer.is_modified() {
            let name = super::display_path(self.buffer.path());
            self.prompt =
                Some(Prompt::unsaved(Purpose::UnsavedThenOpen(path.to_path_buf()), &name));
            return;
        }
        self.load(path);
    }

    /// Replace the buffer with the file at `path`.
    pub(super) fn load(&mut self, path: &Path) {
        match crate::open(path) {
            Ok((mut buffer, report)) => {
                buffer.set_tab_width(self.buffer.tab_width());
                self.buffer = buffer;
                self.scroll = 0;
                self.end_drag();
                if report.lossy {
                    self.message = Some(
                        "This file is not valid UTF-8. Saving it would destroy the original bytes."
                            .into(),
                    );
                }
                let visible = self.tree_area().map_or(0, TreeView::visible_rows);
                if let Some(sidebar) = self.sidebar.as_mut() {
                    sidebar.selected = sidebar.tree.reveal(path);
                    sidebar.reveal_after = sidebar.selected.is_none().then(|| path.to_path_buf());
                    sidebar.request_listings();
                    sidebar.sync_watches();
                    sidebar.follow_selection(visible);
                }
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    /// Attach a folder, for `nun <folder>` and for opening a file inside one.
    pub fn open_folder(
        &mut self,
        root: PathBuf,
        trash: PathBuf,
        visible: bool,
        report: Box<dyn Fn(Done) + Send + 'static>,
    ) {
        let mut sidebar = Sidebar::new(root, trash, visible, report);
        sidebar.reveal_after = self.buffer.path().map(Path::to_path_buf);
        sidebar.request_listings();
        if visible {
            // Nothing is selected to begin with, so a new file goes to the
            // root rather than into whichever folder happens to sort first.
            self.focus = Focus::Sidebar;
        }
        self.sidebar = Some(sidebar);
        self.relayout();
    }

    /// Keep the tree live with `watcher`.
    pub fn attach_watcher(&mut self, watcher: Watcher) {
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar.attach(watcher);
        }
    }

    /// Ask the worker for a marker, for tests that wait for it to catch up.
    #[cfg(test)]
    pub(super) fn echo(&self, marker: u64) {
        if let Some(sidebar) = self.sidebar.as_ref() {
            sidebar.send(Job::Echo(marker));
        }
    }

    /// Whether the tree is still waiting for a listing.
    #[cfg(test)]
    pub(super) fn is_loading(&self) -> bool {
        self.sidebar.as_ref().is_some_and(|sidebar| sidebar.tree.is_loading())
    }

    /// The names of the rows in the tree, for tests.
    #[cfg(test)]
    pub(super) fn sidebar_rows(&self) -> Vec<String> {
        self.sidebar
            .as_ref()
            .map(|sidebar| sidebar.tree.rows().iter().map(|row| row.name.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{KeySet, defaults};
    use crossterm::event::{KeyEvent, MouseButton, MouseEventKind};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use std::fs;
    use tempfile::TempDir;

    /// A project with a folder, a file in it, one at the root, and an ignored
    /// build directory.
    fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "target\n").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("README.md"), "# hi\n").unwrap();
        fs::write(dir.path().join("TODO.md"), "- one\n").unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/out.o"), "").unwrap();
        dir
    }

    /// The app with its folder open, and the worker's answers pumped in, so a
    /// test sees the tree as a person would a moment after opening it.
    struct Tester {
        app: App,
        done: std::sync::mpsc::Receiver<Done>,
    }

    impl Tester {
        fn new(dir: &TempDir) -> Self {
            let (sender, done) = std::sync::mpsc::channel();
            let mut app = App::new(
                Buffer::new(),
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 70, 12));
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                true,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            let mut tester = Self { app, done };
            tester.settle();
            tester
        }

        /// Take everything the worker has finished, up to a marker asked for
        /// now. Jobs are done in order, so when the marker comes back
        /// everything asked for before it has been done — and a listing that
        /// asks for another listing is followed round until it settles.
        fn settle(&mut self) {
            for marker in 0..64 {
                self.app.echo(marker);
                loop {
                    let message = self
                        .done
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("the worker answered");
                    if message == Done::Echo(marker) {
                        break;
                    }
                    self.app.handle(Event::Workspace(message));
                }
                if !self.app.is_loading() {
                    return;
                }
            }
            panic!("the tree never stopped asking for listings");
        }

        fn handle(&mut self, event: Event) {
            self.app.handle(event);
            self.settle();
        }

        fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
            self.handle(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }));
        }

        fn click(&mut self, column: u16, row: u16) {
            self.mouse(MouseEventKind::Down(MouseButton::Left), column, row);
            self.mouse(MouseEventKind::Up(MouseButton::Left), column, row);
        }

        fn right_click(&mut self, column: u16, row: u16) {
            self.mouse(MouseEventKind::Down(MouseButton::Right), column, row);
        }

        fn key(&mut self, code: KeyCode) {
            self.handle(Event::Key(KeyEvent::from(code)));
        }

        fn type_name(&mut self, text: &str) {
            for ch in text.chars() {
                self.key(KeyCode::Char(ch));
            }
        }

        fn run(&mut self, command: Command) {
            self.app.run(command);
            self.settle();
        }

        fn rows(&self) -> Vec<String> {
            self.app.sidebar_rows()
        }

        /// The screen row of the tree row named `name`.
        fn row_of(&self, name: &str) -> u16 {
            let index = self.rows().iter().position(|row| row == name).expect("row is there");
            // The header takes the first row of the sidebar.
            u16::try_from(index).unwrap() + 1
        }
    }

    // ── showing the tree ────────────────────────────────────────────────────

    #[test]
    fn a_folder_opens_with_its_entries_and_hides_what_is_ignored() {
        let dir = project();
        let t = Tester::new(&dir);
        assert_eq!(
            t.rows(),
            vec!["src", "README.md", "TODO.md"],
            "target is ignored, .git is never shown"
        );
    }

    #[test]
    fn showing_ignored_files_lists_them_and_hiding_them_again_does_not() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.run(Command::ToggleIgnored);
        assert!(t.rows().contains(&"target".to_string()));
        assert!(t.rows().contains(&".gitignore".to_string()));
        t.run(Command::ToggleIgnored);
        assert_eq!(t.rows(), vec!["src", "README.md", "TODO.md"]);
    }

    #[test]
    fn clicking_a_folder_expands_it_and_clicking_again_folds_it() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let src = t.row_of("src");

        t.click(3, src);
        assert_eq!(t.rows(), vec!["src", "main.rs", "README.md", "TODO.md"]);
        t.click(3, src);
        assert_eq!(t.rows(), vec!["src", "README.md", "TODO.md"]);
    }

    #[test]
    fn clicking_a_file_opens_it() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("README.md").as_path()));
        assert_eq!(t.app.buffer().text().to_string(), "# hi\n");
    }

    #[test]
    fn the_tree_is_a_hover_target_so_motion_is_worth_reporting() {
        let dir = project();
        let t = Tester::new(&dir);
        assert!(t.app.wants_motion(), "rows and buttons react to hover");

        let mut plain = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        plain.set_viewport(Rect::new(0, 0, 70, 12));
        assert!(!plain.wants_motion(), "with no folder open, nothing reacts to hover");
    }

    #[test]
    fn the_sidebar_can_be_hidden_and_brought_back() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.run(Command::ToggleSidebar);
        assert!(t.app.sidebar_area().is_none());
        // The status line's file button brings it back.
        t.click(1, 11);
        assert!(t.app.sidebar_area().is_some());
    }

    #[test]
    fn dragging_the_edge_resizes_the_sidebar() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let edge = t.app.sidebar_area().unwrap().right() - 1;
        t.mouse(MouseEventKind::Down(MouseButton::Left), edge, 4);
        t.mouse(MouseEventKind::Drag(MouseButton::Left), 18, 4);
        assert_eq!(t.app.sidebar_area().unwrap().width, 19);
    }

    // ── creating, renaming, deleting ────────────────────────────────────────

    #[test]
    fn the_new_file_button_asks_for_a_name_and_opens_what_it_creates() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let button =
            nun_ui::TreeView::button_area(t.app.tree_area().unwrap(), TreeButton::NewFile).unwrap();
        t.click(button.x, button.y);

        t.type_name("notes.md");
        t.key(KeyCode::Enter);

        assert!(dir.path().join("notes.md").exists());
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("notes.md").as_path()));
        assert!(t.rows().contains(&"notes.md".to_string()));
    }

    #[test]
    fn a_new_file_goes_into_the_selected_folder() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("src");
        t.click(3, row);
        t.run(Command::NewFile);
        t.type_name("lib.rs");
        t.key(KeyCode::Enter);
        assert!(dir.path().join("src/lib.rs").exists());
    }

    #[test]
    fn a_name_with_a_separator_in_it_is_refused() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.run(Command::NewFile);
        t.type_name("a/b.rs");
        t.key(KeyCode::Enter);
        assert!(t.app.message().unwrap().contains("not a valid name"), "{:?}", t.app.message());
        assert!(!dir.path().join("a").exists());
    }

    #[test]
    fn cancelling_the_name_prompt_creates_nothing() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.run(Command::NewFolder);
        t.type_name("docs");
        t.key(KeyCode::Esc);
        assert!(!dir.path().join("docs").exists());
    }

    #[test]
    fn renaming_follows_the_file_that_is_open() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        t.run(Command::Rename);
        // The field starts with the current name, ready to be edited.
        for _ in 0.."README.md".len() {
            t.key(KeyCode::Backspace);
        }
        t.type_name("GUIDE.md");
        t.key(KeyCode::Enter);

        assert!(dir.path().join("GUIDE.md").exists());
        assert!(!dir.path().join("README.md").exists());
        assert_eq!(
            t.app.buffer().path(),
            Some(dir.path().join("GUIDE.md").as_path()),
            "the open buffer follows its file"
        );
    }

    #[test]
    fn deleting_offers_an_undo_that_brings_the_file_back() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        t.run(Command::Delete);

        assert!(!dir.path().join("README.md").exists());
        assert!(t.app.message().unwrap().contains("README.md"), "{:?}", t.app.message());
        assert!(t.app.undo_offer, "the toast offers to undo it");

        // The Undo in the status line is a click target.
        let undo = t.app.status_parts(t.app.areas().1).undo.expect("Undo is shown");
        assert_eq!(
            t.app.hits.at(undo.x + 1, undo.y).map(|hit| hit.target),
            Some(Target::StatusUndo)
        );
        t.click(undo.x + 1, undo.y);
        assert!(t.app.message().is_some(), "{:?}", t.app.message());
        assert!(dir.path().join("README.md").exists());
        assert_eq!(fs::read_to_string(dir.path().join("README.md")).unwrap(), "# hi\n");
    }

    #[test]
    fn a_right_click_offers_what_can_be_done_to_a_row() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.right_click(3, row);

        let menu = t.app.menu.clone().expect("a menu is open");
        let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"Rename"), "{labels:?}");
        assert!(labels.contains(&"Delete"), "{labels:?}");

        // Clicking Delete does it.
        let delete = menu.commands.iter().position(|command| *command == Command::Delete).unwrap();
        let row = menu.area.y + u16::try_from(delete).unwrap();
        t.click(menu.area.x + 1, row);
        assert!(!dir.path().join("README.md").exists());
    }

    #[test]
    fn clicking_outside_a_menu_closes_it_without_acting() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.right_click(3, row);
        t.click(60, 8);
        assert!(t.app.menu.is_none());
        assert!(dir.path().join("README.md").exists());
    }

    #[test]
    fn dragging_a_file_onto_a_folder_moves_it() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let readme = t.row_of("README.md");
        let src = t.row_of("src");

        t.mouse(MouseEventKind::Down(MouseButton::Left), 4, readme);
        t.mouse(MouseEventKind::Drag(MouseButton::Left), 4, src);
        t.mouse(MouseEventKind::Up(MouseButton::Left), 4, src);

        assert!(dir.path().join("src/README.md").exists());
        assert!(!dir.path().join("README.md").exists());
        assert!(t.app.message().unwrap().contains("Moved"), "{:?}", t.app.message());
    }

    #[test]
    fn a_folder_cannot_be_dropped_into_itself() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let src = t.row_of("src");
        t.mouse(MouseEventKind::Down(MouseButton::Left), 4, src);
        t.mouse(MouseEventKind::Drag(MouseButton::Left), 5, src);
        t.mouse(MouseEventKind::Up(MouseButton::Left), 5, src);
        assert!(dir.path().join("src").exists());
        assert!(!dir.path().join("src/src").exists());
    }

    // ── unsaved changes ─────────────────────────────────────────────────────

    #[test]
    fn opening_another_file_with_unsaved_changes_asks_first() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        t.key(KeyCode::Char('x'));
        assert!(t.app.buffer().is_modified());

        let row = t.row_of("TODO.md");
        t.click(3, row);
        assert!(t.app.prompt.is_some(), "it asks rather than throwing the changes away");
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("README.md").as_path()));
    }

    #[test]
    fn saving_from_the_prompt_then_opens_the_other_file() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        t.key(KeyCode::Char('x'));

        let row = t.row_of("TODO.md");
        t.click(3, row);
        // Answer with the keyboard: Enter is the first button, Save.
        t.key(KeyCode::Enter);
        assert_eq!(fs::read_to_string(dir.path().join("README.md")).unwrap(), "x# hi\n");
    }

    #[test]
    fn discarding_the_changes_opens_the_other_file() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let todo_row = t.row_of("TODO.md");
        let row = t.row_of("README.md");
        t.click(3, row);
        t.key(KeyCode::Char('x'));
        t.click(3, todo_row);

        let prompt = t.app.prompt.as_ref().unwrap().clone();
        let discard = prompt.button_areas(t.app.areas().1)[1];
        t.click(discard.x + 1, discard.y);
        assert_eq!(
            fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "# hi\n",
            "not saved"
        );
    }

    // ── the keyboard ────────────────────────────────────────────────────────

    #[test]
    fn the_tree_can_be_walked_and_opened_from_the_keyboard() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.key(KeyCode::Down);
        t.key(KeyCode::Down);
        t.key(KeyCode::Enter);
        assert_eq!(t.app.buffer().path(), Some(dir.path().join("README.md").as_path()));
        assert_eq!(t.app.focus, Focus::Editor, "opening a file moves on to it");
    }

    #[test]
    fn right_and_left_expand_and_fold_a_folder() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.key(KeyCode::Right);
        assert_eq!(t.rows(), vec!["src", "main.rs", "README.md", "TODO.md"]);
        t.key(KeyCode::Left);
        assert_eq!(t.rows(), vec!["src", "README.md", "TODO.md"]);
    }

    #[test]
    fn typing_with_the_tree_focused_types_into_the_text() {
        // The tree has no use for a letter, so rather than swallowing it the
        // keyboard goes back to the text and types it.
        let dir = project();
        let mut t = Tester::new(&dir);
        t.key(KeyCode::Char('x'));
        assert_eq!(t.app.buffer().text().to_string(), "x");
        assert_eq!(t.app.focus, Focus::Editor);
    }

    #[test]
    fn keys_the_tree_uses_stay_with_the_tree() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.key(KeyCode::Down);
        assert_eq!(t.app.focus, Focus::Sidebar);
        assert_eq!(t.app.buffer().text().to_string(), "");
    }

    #[test]
    fn after_an_undo_the_button_offers_the_redo() {
        let dir = project();
        let mut t = Tester::new(&dir);
        let row = t.row_of("README.md");
        t.click(3, row);
        t.run(Command::Delete);
        assert_eq!(t.app.undo_label(), " Undo ");

        let undo = t.app.status_parts(t.app.areas().1).undo.unwrap();
        t.click(undo.x + 1, undo.y);
        assert!(dir.path().join("README.md").exists());
        assert_eq!(t.app.undo_label(), " Redo ", "the same button puts it back");

        let redo = t.app.status_parts(t.app.areas().1).undo.unwrap();
        t.click(redo.x + 1, redo.y);
        assert!(!dir.path().join("README.md").exists());
    }

    #[test]
    fn escape_hands_the_keyboard_back_to_the_text() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.key(KeyCode::Esc);
        assert_eq!(t.app.focus, Focus::Editor);
        t.key(KeyCode::Char('x'));
        assert_eq!(t.app.buffer().text().to_string(), "x");
    }

    // ── the outside world ───────────────────────────────────────────────────

    #[test]
    fn a_file_created_by_something_else_appears_when_the_watcher_says_so() {
        let dir = project();
        let mut t = Tester::new(&dir);
        fs::write(dir.path().join("new.txt"), "").unwrap();
        assert!(!t.rows().contains(&"new.txt".to_string()), "not until it is told");

        t.handle(Event::Files { dir: dir.path().to_path_buf(), error: None });
        assert!(t.rows().contains(&"new.txt".to_string()));
    }

    #[test]
    fn a_watch_that_fails_says_so_rather_than_going_quietly_stale() {
        let dir = project();
        let mut t = Tester::new(&dir);
        t.handle(Event::Files {
            dir: dir.path().to_path_buf(),
            error: Some("too many open watches".into()),
        });
        let message = t.app.message().unwrap();
        assert!(message.contains("no longer live"), "{message}");
    }
}
