//! Edits a language server wants made across the project: a rename, a code
//! action, or an edit a server asks for itself while carrying out a command.
//!
//! All of them arrive as a `WorkspaceEdit`, and all of them go through here,
//! so there is one idea of what such an edit may do, one preview of it, and
//! one way back from it.
//!
//! **Checking.** Either every file in an edit can be edited as it says or
//! none is:
//!
//! - a file operation — create, move, delete — that would overwrite anything,
//!   that lies outside the open folder, that deletes a file that is open, or
//!   whose steps cannot be traced back to the files as they are now, is
//!   refused whole ([`files`] says exactly when);
//! - an edit to a document at a version the editor no longer has is refused
//!   whole, and so is one naming a version for a file that is not open, since
//!   it describes text nobody can see;
//! - an edit to an open file whose unsaved changes the server has not seen is
//!   refused whole, since its positions describe the file on disk;
//! - a file named twice, edits that overlap, a file that cannot be read as
//!   text, or one whose lines end in a bare carriage return are refused whole
//!   too.
//!
//! **Files created, moved and deleted.** A server may ask for these in among
//! its text edits — rust-analyzer moves a module's file when the module is
//! renamed. They are sorted out before anything is done ([`files`]): edits to
//! files already there are made where those files are now, edits to a file
//! the edit creates become what it is created holding, and then the
//! operations run on the workspace worker, in the order the server listed
//! them, through the same `FsHistory` the file tree uses — so a delete goes
//! to the trash and nothing is ever overwritten. They are kept off the tree's
//! own undo, since taking back the move alone would leave the files that name
//! it pointing at nothing; each comes back with its reverse, kept with the
//! rest of the edit. An open file that is moved follows its file, as it does
//! for a move in the tree, and its server hears it closed under the old name
//! and opened under the new. The server is not asked `willRenameFiles` and is
//! not told `didRenameFiles`, since it asked for the move itself. It is told
//! `didChangeWatchedFiles` of every file created, moved, deleted or written on
//! disk that it registered to watch, both ways, at once rather than when the
//! watcher gets round to it: a server that leaves watching to its client
//! would otherwise go on believing, for a moment or for good, that a module's
//! file is where it was.
//!
//! **Straight in, or previewed.** A rename is always previewed: it writes
//! files nobody has open, the second most destructive thing the editor does
//! after a project-wide replace. A code action usually edits the one file it
//! was asked about — an import added, a variable renamed where it stands — and
//! is expected to happen the moment it is chosen. So an edit from a code
//! action that touches exactly one file, and that file is open, goes straight
//! into its buffer as one undo step; anything wider, or anything that creates,
//! moves or deletes a file, is previewed exactly as a rename is. So is an edit a server sends of its own accord, or one that
//! arrives after the text it was worked out for has changed: nobody chose
//! that edit with the text in front of them. The rule is the one line in
//! [`App::edit_workspace`].
//!
//! **Previewing.** Files that are open are previewed from their buffers. Files
//! that are not are read on the workspace worker, never here, and previewed
//! from what was read. What the panel shows for a file is computed by the same
//! splice whose result is written or compared against, so the preview and the
//! write cannot disagree. Each file carries a tick: clicking it leaves the file
//! out. Leaving a file out usually breaks the build, but it is the person's
//! project and the choice is theirs to make with their eyes open. A file
//! operation has a row of its own, above the files, and no tick: the steps
//! after it were worked out on the files as it leaves them, so it cannot be
//! left out alone. Nor can the text of a file the edit creates.
//!
//! **Applying.** Open files are edited in their buffers, through
//! `Buffer::apply_batch`, so each gets exactly one undo step and none of them
//! is saved; each buffer is checked afterwards to hold exactly what was
//! previewed, and if one does not, every buffer is taken back and nothing is
//! written. Files that are not open are written on the worker with
//! `Job::Rewrite`, which writes a file only while it still holds the text that
//! was previewed, and checks all of them before writing any. Once every one
//! is written, the file operations run, stopping at the first that fails.
//!
//! **What undo means.** An editor undo is per buffer, and an edit across the
//! project touches files that have no buffer. So a previewed edit keeps its
//! own record and its own way back — offered on the status line as soon as it
//! lands, and as "Undo rename" in the palette after that:
//!
//! - an open file is taken back by undoing its buffer's step, provided its
//!   text is still exactly what the edit left and that step is still the one
//!   on top (an undo that turns out to reverse something else is redone at
//!   once and reported);
//! - a file that was written is taken back by writing its old text over it —
//!   the same guarded rewrite the other way round, so a file that has changed
//!   since is not overwritten and the undo is refused, naming it;
//! - a file operation is taken back by its reverse, last first, before any
//!   file is rewritten: a move is moved back, a created file goes to the trash
//!   provided it still holds what it was created with, and a deleted one comes
//!   back out of the trash.
//!
//! Everything is checked before anything is taken back. Ctrl+Z in one open
//! file still undoes that file's part alone, as it would any other edit.
//!
//! **When it goes wrong halfway.** A write can fail, or a file can change in
//! the moment between its check and its write. Then the run stops there, and
//! the status line names what was written and which operations happened,
//! what stopped it and why, and what was never tried. What was written stays written and the undo takes back
//! exactly that. A failure before anything reached the disk takes the open
//! buffers back too, so nothing is left half done.
//!
//! **A server waiting.** An edit a server asked for is answered once it is
//! done with — applied, refused, or put away unapplied from the preview — and
//! never before, so the server hears what actually happened. It waits on its
//! own thread meanwhile; nothing here waits on it.

mod files;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use nun_core::Edit;
use nun_lsp::types::{
    DocumentChangeOperation, DocumentChanges, FileChangeType, OneOf, TextEdit, WorkspaceEdit,
};
use nun_lsp::{EditRequest, Encoding};
use nun_ui::{Glyph, HitState, SearchRow, SearchView};
use nun_workspace::{Carried, FileOp, Job, Present, Rewrite, Written};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ropey::Rope;

use self::files::{Decided, Sorted, Step, Wanted};
use super::panes::DocId;
use super::{App, Focus, Outcome, SidebarView, Target};

/// What the pointer can land on in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// The panel's title row.
    Header,
    /// The header's button: put the preview away.
    Back,
    /// One of the two rows at the top, which only show what the edit is.
    Field,
    /// The cell at the end of the second row that applies the edit.
    Apply,
    /// A text button on the row below them: apply, or cancel.
    Action(usize),
    /// The tick on a file row, which leaves the file out or puts it back.
    Mark(usize),
    /// A row of the list.
    Row(usize),
    /// Below the rows.
    Empty,
}

/// What an edit is, for the panel and for everything said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Subject {
    /// A symbol renamed.
    Rename { old: String, new: String },
    /// A code action, or an edit a server asked for, by its title.
    Action { title: String },
}

impl Subject {
    /// The panel's title.
    const fn title(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "RENAME",
            Self::Action { .. } => "CODE ACTION",
        }
    }

    /// The text buttons where the search's toggles go: do it, or don't.
    const fn actions(&self) -> &'static [&'static str] {
        match self {
            Self::Rename { .. } => &["Rename", "Cancel"],
            Self::Action { .. } => &["Apply", "Cancel"],
        }
    }

    /// The two rows at the top of the panel.
    fn fields(&self) -> (&str, &str) {
        match self {
            Self::Rename { old, new } => (old, new),
            Self::Action { title } => (title, ""),
        }
    }

    /// Doing it, as "Did not …" goes on: "rename cat".
    fn verb(&self) -> String {
        match self {
            Self::Rename { old, .. } => format!("rename {old}"),
            Self::Action { title } => format!("apply “{title}”"),
        }
    }

    /// Having done it: "Renamed cat to dog".
    fn done(&self) -> String {
        match self {
            Self::Rename { old, new } => format!("Renamed {old} to {new}"),
            Self::Action { title } => format!("Applied “{title}”"),
        }
    }

    /// Doing it, while it is done: "Renaming cat to dog…".
    fn doing(&self) -> String {
        match self {
            Self::Rename { old, new } => format!("Renaming {old} to {new}…"),
            Self::Action { title } => format!("Applying “{title}”…"),
        }
    }

    /// What it is, as a thing: "the rename".
    const fn what(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "the rename",
            Self::Action { .. } => "the code action",
        }
    }

    /// Which one it is: "the rename of cat to dog".
    fn named(&self) -> String {
        match self {
            Self::Rename { old, new } => format!("the rename of {old} to {new}"),
            Self::Action { title } => format!("“{title}”"),
        }
    }

    /// The bare verb: nothing was "renamed", nothing to "rename".
    const fn change(&self) -> (&'static str, &'static str) {
        match self {
            Self::Rename { .. } => ("rename", "renamed"),
            Self::Action { .. } => ("change", "changed"),
        }
    }

    /// What an open file still has after an undo that could not take it.
    const fn kept(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "the new name",
            Self::Action { .. } => "the change",
        }
    }

    /// The name of the way back.
    const fn undo(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "Undo rename",
            Self::Action { .. } => "Undo",
        }
    }

    /// Having done some of it: "Renamed only partly".
    fn partly(&self) -> String {
        match self {
            Self::Rename { .. } => "Renamed only partly".into(),
            Self::Action { .. } => format!("{} only partly", self.done()),
        }
    }

    /// What is said while the files it touches are read.
    fn reading(&self) -> String {
        match self {
            Self::Rename { old, .. } => format!("Reading the files {old} is renamed in…"),
            Self::Action { title } => format!("Reading the files “{title}” edits…"),
        }
    }

    /// What is said when it turns out to change nothing.
    fn nothing(&self) -> String {
        match self {
            Self::Rename { old, new } => format!("Renaming {old} to {new} changes nothing."),
            Self::Action { title } => format!("“{title}” changes nothing."),
        }
    }

    /// How to see what it would do now.
    const fn again(&self) -> &'static str {
        match self {
            Self::Rename { .. } => "Rename again",
            Self::Action { .. } => "Ask for the code action again",
        }
    }
}

/// What follows an edit, once it is done with.
#[derive(Debug, Default)]
pub(super) struct After {
    /// A server waiting to hear whether its edit was made.
    pub(super) reply: Option<EditRequest>,
    /// A server command to carry out once the edit is in — a code action may
    /// have both — and the document whose server carries it out.
    pub(super) then: Option<(DocId, nun_lsp::types::Command)>,
}

/// Where an edit has got to.
#[derive(Debug, Default)]
enum Stage {
    /// Nothing is happening, or a preview is waiting for the person.
    #[default]
    Idle,
    /// Files that are not open are being read, to preview them.
    Reading {
        tag: u64,
        subject: Subject,
        /// What is ready already: the open files.
        files: Vec<FilePlan>,
        /// What the worker is reading, and the server's edits to each.
        disk: Vec<(PathBuf, Vec<TextEdit>)>,
        /// The files it creates, moves and deletes, whose places the worker
        /// is looking at.
        sorted: Sorted,
        encoding: Encoding,
    },
    /// The edit has been applied to the open files, and the rest are being
    /// written. The file operations follow.
    Applying { tag: u64, applied: Applied, planned: Vec<Rewrite>, ops: Vec<FileOp> },
    /// Every file is written, and the file operations are being carried out.
    Moving { tag: u64, applied: Applied },
    /// An edit is being taken back, and its file operations reversed. The
    /// files follow.
    Unmoving { tag: u64, applied: Applied },
    /// An edit is being taken back, and its files are being written. `did`
    /// is what taking back its file operations did, in words.
    Undoing { tag: u64, applied: Applied, did: Vec<String> },
}

/// Edits across the project, and everything the preview and the undo need of
/// them.
#[derive(Debug, Default)]
pub(super) struct Edits {
    stage: Stage,
    /// What is on screen, while it is.
    preview: Option<Preview>,
    /// What follows the edit in hand, from the moment it arrives until it is
    /// done with.
    after: After,
    /// The last edit applied that can be taken back, until it is or another
    /// replaces it.
    last: Option<Applied>,
    /// The message that offered to take the last edit back. The status
    /// line's Undo button means "undo that" only while it is still the
    /// message showing; anything else that has happened since has its own.
    offer: Option<String>,
    /// Numbers the worker's jobs, so an answer finds its question.
    tags: u64,
}

impl Edits {
    /// Whether an edit is between its checking and its end: being read,
    /// written, or taken back.
    pub(super) const fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    fn tag(&mut self) -> u64 {
        self.tags += 1;
        self.tags
    }
}

/// One file in the preview.
#[derive(Debug)]
struct FilePlan {
    /// The file, in full.
    path: PathBuf,
    /// How it reads in the panel.
    label: String,
    /// Whether it is to be edited.
    included: bool,
    /// How it changes.
    target: Where,
    /// The lines that change, as the panel shows them.
    changes: Vec<Change>,
}

/// Where a file's text lives, and what it becomes.
#[derive(Debug)]
enum Where {
    /// In an open buffer: the text the edits were worked out against, the
    /// edits, and the text they give.
    Open { doc: DocId, before: Rope, edits: Vec<Edit>, after: Rope },
    /// On disk only: what was read, and what is to be written.
    Disk { before: String, after: String },
    /// Nowhere yet: the edit creates it, holding this.
    Created,
}

/// One changed line, before and after.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    /// Which line, counting from one.
    line: u32,
    /// Which line it is afterwards, which an earlier edit adding or taking
    /// away a line break moves.
    after_line: u32,
    /// The line as it is, without its ending.
    before: String,
    /// The char ranges of it that the edits replace.
    matched: Vec<Range<u32>>,
    /// The line as it will be.
    after: String,
}

/// A row of the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    /// A file, by index.
    File(usize),
    /// A changed line as it is: which file, which change.
    Before(usize, usize),
    /// The same line as it will be.
    After(usize, usize),
    /// A file operation, by index.
    Op(usize),
}

/// The panel.
#[derive(Debug)]
struct Preview {
    subject: Subject,
    files: Vec<FilePlan>,
    /// What the edit creates, moves and deletes, in order, each with how it
    /// reads.
    ops: Vec<(FileOp, String)>,
    collapsed: BTreeSet<usize>,
    rows: Vec<Line>,
    widest: u32,
    scroll: usize,
    selected: Option<usize>,
    summary: String,
}

impl Preview {
    fn new(subject: Subject, mut files: Vec<FilePlan>, ops: Vec<(FileOp, String)>) -> Self {
        files.sort_by(|a, b| a.label.cmp(&b.label));
        let widest = files
            .iter()
            .flat_map(|file| file.changes.iter().map(|change| change.line.max(change.after_line)))
            .max()
            .unwrap_or(1);
        let mut preview = Self {
            subject,
            files,
            ops,
            collapsed: BTreeSet::new(),
            rows: Vec::new(),
            widest,
            scroll: 0,
            selected: None,
            summary: String::new(),
        };
        preview.relist();
        preview
    }

    fn relist(&mut self) {
        let mut rows: Vec<Line> = (0..self.ops.len()).map(Line::Op).collect();
        for (at, file) in self.files.iter().enumerate() {
            rows.push(Line::File(at));
            if self.collapsed.contains(&at) {
                continue;
            }
            for index in 0..file.changes.len() {
                rows.push(Line::Before(at, index));
                if file.included {
                    rows.push(Line::After(at, index));
                }
            }
        }
        self.rows = rows;
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.map(|at| at.min(last));

        let files = self.files.iter().filter(|file| file.included).count();
        let places: usize = self
            .files
            .iter()
            .filter(|file| file.included)
            .map(|file| file.changes.iter().map(|change| change.matched.len()).sum::<usize>())
            .sum();
        let left_out = self.files.len() - files;
        self.summary =
            format!("{places} {} in {files} {}", plural(places, "change"), plural(files, "file"));
        if left_out > 0 {
            let _ = write!(self.summary, ", {left_out} left out");
        }
        let ops = self.ops.len();
        if ops > 0 {
            let _ = write!(self.summary, ", {ops} file {}", plural(ops, "operation"));
        }
    }

    fn view_rows(&self, window: Range<usize>) -> Vec<SearchRow<'_>> {
        let end = window.end.min(self.rows.len());
        let first = window.start.min(end);
        self.rows[first..end]
            .iter()
            .map(|line| match *line {
                Line::File(at) => {
                    let file = &self.files[at];
                    SearchRow::File {
                        path: &file.label,
                        hits: file.changes.len(),
                        collapsed: self.collapsed.contains(&at),
                        state: if file.included { HitState::Included } else { HitState::Excluded },
                    }
                }
                Line::Before(at, index) => {
                    let file = &self.files[at];
                    let change = &file.changes[index];
                    SearchRow::Hit {
                        line: change.line,
                        text: &change.before,
                        matched: &change.matched,
                        state: if file.included { HitState::Included } else { HitState::Excluded },
                    }
                }
                Line::After(at, index) => {
                    let change = &self.files[at].changes[index];
                    SearchRow::After { line: change.after_line, text: &change.after }
                }
                Line::Op(at) => SearchRow::Operation { text: &self.ops[at].1 },
            })
            .collect()
    }

    fn file_of(&self, row: usize) -> Option<usize> {
        match *self.rows.get(row)? {
            Line::File(at) | Line::Before(at, _) | Line::After(at, _) => Some(at),
            Line::Op(_) => None,
        }
    }
}

/// An edit as it was applied: enough to take it back.
#[derive(Debug)]
struct Applied {
    subject: Subject,
    /// The open files it edited.
    buffers: Vec<Undoable>,
    /// The files it wrote, each as the rewrite that would take it back.
    disk: Vec<Rewrite>,
    /// The file operations that take back the ones it carried out, in the
    /// order to carry them out: the last one done, first.
    files: Vec<FileOp>,
}

/// One open file an edit changed.
#[derive(Debug)]
struct Undoable {
    doc: DocId,
    label: String,
    before: Rope,
    after: Rope,
}

/// File operations sorted by what became of them, while that is said.
#[derive(Debug, Default)]
struct Carrying {
    /// What happened, in words.
    did: Vec<String>,
    /// What takes back each that happened, in the order they happened.
    reverses: Vec<FileOp>,
    /// What stopped the run, in words.
    failed: Option<String>,
    /// What never happened, in words.
    untried: Vec<String>,
    /// The operations that did not happen, the one that stopped the run
    /// first.
    left: Vec<FileOp>,
}

/// The edits a server wants made to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileEdits {
    path: PathBuf,
    /// The version of the document they were worked out against, when the
    /// server said.
    version: Option<i32>,
    edits: Vec<TextEdit>,
}

impl App {
    /// Make a server's `edit`, about `subject`, whose positions count in
    /// `encoding`: check it, then preview it, or — when it may go `straight`
    /// in and touches one open file and nothing else — make it at once.
    /// `after` is what follows once it is done with.
    pub(super) fn edit_workspace(
        &mut self,
        subject: Subject,
        encoding: Encoding,
        edit: &WorkspaceEdit,
        straight: bool,
        after: After,
    ) -> Outcome {
        if self.edits.busy() {
            self.message = Some("Still working on the last edit across files…".into());
            self.edits_answer(after, Err("nun is still making another edit".into()));
            return Outcome::Redraw;
        }
        // A preview nobody has decided on yet is put away for the new one:
        // it was asked for after, so it is the one wanted.
        if self.edits.preview.take().is_some() {
            self.close_edit_preview();
            self.edits_done(Err("another edit came first".into()));
        }
        self.edits.after = after;
        match self.plan(encoding, edit) {
            Ok((files, disk, sorted)) if disk.is_empty() && sorted.ops.is_empty() => {
                let mut files: Vec<FilePlan> =
                    files.into_iter().filter(|file| !file.changes.is_empty()).collect();
                // The rule: an edit chosen with the text in front of the
                // person, to one open file, goes straight in as one undo step.
                if straight && files.len() == 1 {
                    return self.apply_straight(&subject, files.remove(0));
                }
                self.preview_edit(subject, files, Vec::new())
            }
            Ok(_) if self.sidebar.is_none() => {
                // The worker that would read the other files, and create,
                // move and delete, belongs to the folder, and there is none.
                self.message = Some(format!(
                    "Did not {}: it edits files that are not open, which needs a folder open.",
                    subject.verb()
                ));
                self.edits_done(Err("it edits files that are not open, and no folder is".into()));
                Outcome::Redraw
            }
            Ok((files, disk, sorted)) => {
                let tag = self.edits.tag();
                let paths = disk.iter().map(|(path, _)| path.clone()).collect();
                let probe = sorted.probe.clone();
                self.send_job(Job::Read { tag, paths, probe });
                self.message = Some(subject.reading());
                self.edits.stage = Stage::Reading { tag, subject, files, disk, sorted, encoding };
                Outcome::Redraw
            }
            Err(why) => {
                self.message = Some(format!("Did not {}: {why}.", subject.verb()));
                self.edits_done(Err(why));
                Outcome::Redraw
            }
        }
    }

    /// Check a server's edit, and work out what it does to every open file.
    ///
    /// Returns those; the files that are not open with the edits to each; and
    /// the files it creates, moves and deletes, still to be checked against
    /// what is on disk.
    #[allow(clippy::type_complexity)] // Three lists, named in the comment.
    fn plan(
        &self,
        encoding: Encoding,
        edit: &WorkspaceEdit,
    ) -> Result<(Vec<FilePlan>, Vec<(PathBuf, Vec<TextEdit>)>, Sorted), String> {
        let lsp = self.lsp.as_ref().ok_or("the language server stopped")?;
        let steps = flatten(edit, &|path| self.local(path))?;
        let mut sorted = files::sort(steps, &|path| self.label(path))?;
        self.may_touch(&sorted.ops)?;
        let mut open = Vec::new();
        let mut disk = Vec::new();
        for file in std::mem::take(&mut sorted.edits) {
            let label = self.label(&file.path);
            let Some(document) = self.docs.iter().find(|document| {
                document.buffer.path().is_some_and(|open| super::same_file(open, &file.path))
                    || lsp
                        .identifier(document.id)
                        .and_then(|id| nun_lsp::uri::to_path(&id.uri))
                        .is_some_and(|path| path == file.path)
            }) else {
                if file.version.is_some() {
                    return Err(format!(
                        "the server edits {label} as an open file, and it is not open"
                    ));
                }
                disk.push((file.path, file.edits));
                continue;
            };
            let followed = lsp.version(document.id);
            if let Some(version) = file.version
                && followed != Some(version)
            {
                return Err(format!("{label} has changed since the server worked it out"));
            }
            if followed.is_none() && document.buffer.is_modified() {
                return Err(format!(
                    "{label} has unsaved changes the language server has not seen"
                ));
            }
            let before = document.buffer.rope().clone();
            // A buffer holds a lone carriage return only when a file had one,
            // and the server counts it as a line break where nothing here
            // does, so its line numbers would land on the wrong lines.
            if before.chars().any(|ch| ch == '\r') {
                return Err(format!(
                    "{label} has a bare carriage return in it, which the server counts \
                     differently"
                ));
            }
            let edits = join(encoding.edits(&before, &file.edits), before.len_chars())
                .map_err(|why| format!("{label}: {why}"))?;
            let after = Rope::from_str(&splice(&before, &edits));
            let changes =
                changes(&before, &edits, &after, self.palette.glyph(Glyph::ReplaceLineBreak));
            open.push(FilePlan {
                path: file.path,
                label,
                included: true,
                target: Where::Open { doc: document.id, before, edits, after },
                changes,
            });
        }
        Ok((open, disk, sorted))
    }

    /// Refuse file operations outside the folder, and deletes of files that
    /// are open: a buffer whose file has gone to the trash would put it
    /// back at the next save, and nobody asked for that.
    fn may_touch(&self, ops: &[Wanted]) -> Result<(), String> {
        let root = self.workspace_root();
        for op in ops {
            for path in op.paths() {
                if root.as_deref().is_none_or(|root| !path.starts_with(root) || path == root) {
                    return Err(format!(
                        "the server would create, move or delete {}, which is outside the folder",
                        path.display()
                    ));
                }
            }
            if let Wanted::Delete { path, .. } = op
                && let Some(open) = self
                    .docs
                    .iter()
                    .filter_map(|document| document.buffer.path())
                    .find(|open| open.starts_with(path))
            {
                return Err(format!(
                    "the server would delete {}, which is open; close it first",
                    self.label(open)
                ));
            }
        }
        Ok(())
    }

    /// A path as the folder was opened, when it is inside it by either of
    /// the names [`App::label`] knows it by; as it is otherwise.
    fn local(&self, path: PathBuf) -> PathBuf {
        match (self.workspace_root(), self.label(&path)) {
            (Some(root), label) if Path::new(&label).is_relative() => root.join(label),
            _ => path,
        }
    }

    /// Make an edit to one open file there and then, as one undo step.
    fn apply_straight(&mut self, subject: &Subject, file: FilePlan) -> Outcome {
        let Where::Open { doc, edits, after, .. } = file.target else {
            return self.preview_edit(subject.clone(), vec![file], Vec::new());
        };
        let Some(document) = self.docs.iter_mut().find(|d| d.id == doc) else {
            self.edits_done(Err(format!("{} was closed", file.label)));
            return Outcome::Continue;
        };
        let applied = document.buffer.apply_batch(edits);
        if applied.is_err() || document.buffer.rope() != &after {
            if applied.is_ok_and(|changed| changed) {
                document.buffer.undo();
            }
            let why = format!("the edits to {} did not come out as worked out", file.label);
            self.message =
                Some(format!("Did not {}: {why}, so nothing was changed.", subject.verb()));
            self.edits_done(Err(why));
            return Outcome::Redraw;
        }
        self.follow_caret();
        self.message = Some(format!("{}.", subject.done()));
        self.edits_done(Ok(()));
        Outcome::Redraw
    }

    /// The edit in hand is done with: tell the server that asked for it how
    /// it went, and carry out what follows it if it went in.
    fn edits_done(&mut self, result: Result<(), String>) {
        let after = std::mem::take(&mut self.edits.after);
        self.edits_answer(after, result);
    }

    fn edits_answer(&mut self, after: After, result: Result<(), String>) {
        let went_in = result.is_ok();
        if let Some(reply) = after.reply
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.answer_edit(reply, result);
        }
        if went_in && let Some((doc, command)) = after.then {
            self.execute_command(doc, command);
        }
    }

    /// The files that are not open have been read, and the places the edit
    /// creates, moves and deletes looked at: preview everything.
    pub(super) fn edits_read(
        &mut self,
        tag: u64,
        read: Vec<(PathBuf, Result<String, String>)>,
        probed: &[(PathBuf, Present)],
    ) -> Outcome {
        let Stage::Reading { tag: asked, subject, mut files, disk, sorted, encoding } =
            std::mem::take(&mut self.edits.stage)
        else {
            return Outcome::Continue;
        };
        if asked != tag {
            self.edits.stage =
                Stage::Reading { tag: asked, subject, files, disk, sorted, encoding };
            return Outcome::Continue;
        }
        for ((path, text), (_, edits)) in read.into_iter().zip(disk) {
            let label = self.label(&path);
            let line_break = self.palette.glyph(Glyph::ReplaceLineBreak);
            let planned = text.and_then(|text| {
                let (after, changes) = disk_plan(&text, encoding, &edits, line_break)?;
                Ok((text, after, changes))
            });
            match planned {
                Ok((before, after, changes)) => files.push(FilePlan {
                    path,
                    label,
                    included: true,
                    target: Where::Disk { before, after },
                    changes,
                }),
                Err(why) => {
                    self.message = Some(format!("Did not {}: {label}: {why}.", subject.verb()));
                    self.edits_done(Err(format!("{label}: {why}")));
                    return Outcome::Redraw;
                }
            }
        }
        let ops = match self.plan_ops(&sorted, probed, encoding, &mut files) {
            Ok(ops) => ops,
            Err(why) => {
                self.message = Some(format!("Did not {}: {why}.", subject.verb()));
                self.edits_done(Err(why));
                return Outcome::Redraw;
            }
        };
        let files = files.into_iter().filter(|file| !file.changes.is_empty()).collect();
        self.preview_edit(subject, files, ops)
    }

    /// Decide what the edit creates, moves and deletes, now that what is
    /// there is known: each operation as the worker is to carry it out, with
    /// how it reads. A file it creates with text in it is added to `files`.
    fn plan_ops(
        &self,
        sorted: &Sorted,
        probed: &[(PathBuf, Present)],
        encoding: Encoding,
        files: &mut Vec<FilePlan>,
    ) -> Result<Vec<(FileOp, String)>, String> {
        let decided = sorted.decide(probed, &|path| self.label(path))?;
        let line_break = self.palette.glyph(Glyph::ReplaceLineBreak);
        let mut ops = Vec::new();
        for (at, decided) in decided.into_iter().enumerate() {
            let op = match decided {
                Decided::Skip => continue,
                Decided::Create(path) => {
                    let edits = sorted.created.iter().find(|(made, _)| *made == at);
                    let label = self.label(&path);
                    let (text, changes) = match edits {
                        Some((_, edits)) => disk_plan("", encoding, edits, line_break)
                            .map_err(|why| format!("{label}: {why}"))?,
                        None => (String::new(), Vec::new()),
                    };
                    files.push(FilePlan {
                        path: path.clone(),
                        label,
                        included: true,
                        target: Where::Created,
                        changes,
                    });
                    FileOp::Create { path, text }
                }
                Decided::Move(from, to) => FileOp::Move { from, to },
                Decided::Delete(path) => FileOp::Delete { path },
            };
            let mut said = self.said(&op);
            said[..1].make_ascii_uppercase();
            ops.push((op, said));
        }
        Ok(ops)
    }

    /// A file operation as a clause: "move src/foo.rs to src/bar.rs".
    fn said(&self, op: &FileOp) -> String {
        match op {
            FileOp::Create { path, .. } => format!("create {}", self.label(path)),
            FileOp::Move { from, to } => {
                format!("move {} to {}", self.label(from), self.label(to))
            }
            FileOp::Delete { path } => format!("delete {}", self.label(path)),
            FileOp::Restore { path, .. } => format!("restore {}", self.label(path)),
            FileOp::Discard { path, .. } => format!("remove {}", self.label(path)),
        }
    }

    /// The same, done: "moved src/foo.rs to src/bar.rs".
    fn did(&self, op: &FileOp) -> String {
        match op {
            FileOp::Create { path, .. } => format!("created {}", self.label(path)),
            FileOp::Move { from, to } => {
                format!("moved {} to {}", self.label(from), self.label(to))
            }
            FileOp::Delete { path } => format!("deleted {}", self.label(path)),
            FileOp::Restore { path, .. } => format!("restored {}", self.label(path)),
            FileOp::Discard { path, .. } => format!("removed {}", self.label(path)),
        }
    }

    /// Show what the edit would do.
    fn preview_edit(
        &mut self,
        subject: Subject,
        files: Vec<FilePlan>,
        ops: Vec<(FileOp, String)>,
    ) -> Outcome {
        if files.is_empty() && ops.is_empty() {
            self.message = Some(subject.nothing());
            self.edits_done(Ok(()));
            return Outcome::Redraw;
        }
        let Some(sidebar) = self.sidebar.as_mut() else {
            self.message = Some(format!(
                "Did not {}: it edits more than one file, which needs a folder open to preview.",
                subject.verb()
            ));
            self.edits_done(Err("nun previews such an edit, and has no folder open".into()));
            return Outcome::Redraw;
        };
        sidebar.visible = true;
        self.edits.preview = Some(Preview::new(subject, files, ops));
        self.sidebar_view = SidebarView::EditPreview;
        self.focus = Focus::EditPreview;
        self.message = None;
        self.relayout();
        Outcome::Redraw
    }

    /// Put the preview away without changing anything.
    pub(super) fn cancel_edit_preview(&mut self) -> Outcome {
        let Some(preview) = self.edits.preview.take() else { return Outcome::Continue };
        self.close_edit_preview();
        self.message = Some(format!("Nothing was {}.", preview.subject.change().1));
        self.edits_done(Err("put away without being applied".into()));
        Outcome::Redraw
    }

    /// The sidebar is about to show something else. A preview nobody can
    /// see is one nobody can apply, so it is put away — and a server waiting
    /// on it hears so, rather than waiting for ever.
    pub(super) fn leave_edit_preview(&mut self) {
        if self.sidebar_view == SidebarView::EditPreview && self.edits.preview.is_some() {
            self.cancel_edit_preview();
        }
    }

    fn close_edit_preview(&mut self) {
        if self.sidebar_view == SidebarView::EditPreview {
            self.sidebar_view = SidebarView::Files;
        }
        if self.focus == Focus::EditPreview {
            self.focus = Focus::Editor;
        }
        self.relayout();
    }

    /// Where the preview is, while the sidebar is showing it.
    pub(super) fn edit_preview_area(&self) -> Option<Rect> {
        (self.sidebar_view == SidebarView::EditPreview && self.edits.preview.is_some())
            .then(|| self.tree_area())
            .flatten()
    }

    // ── applying ─────────────────────────────────────────────────────────────

    /// Make the edit in every file that is ticked.
    pub(super) fn apply_edit_preview(&mut self) -> Outcome {
        let Some(preview) = self.edits.preview.take() else { return Outcome::Continue };
        if preview.ops.is_empty() && !preview.files.iter().any(|file| file.included) {
            let change = preview.subject.change().0;
            self.edits.preview = Some(preview);
            self.message =
                Some(format!("Every file is left out, so there is nothing to {change}."));
            return Outcome::Redraw;
        }
        // Nothing is touched unless every open file still holds what was
        // previewed.
        for file in preview.files.iter().filter(|file| file.included) {
            if let Where::Open { doc, before, .. } = &file.target
                && self.doc_by(*doc).is_none_or(|document| document.buffer.rope() != before)
            {
                let label = file.label.clone();
                let again = preview.subject.again();
                self.edits.preview = Some(preview);
                self.message = Some(format!(
                    "{label} has changed since the preview. {again} to see what it would do now."
                ));
                return Outcome::Redraw;
            }
        }

        let Preview { subject, files, ops, .. } = preview;
        let ops: Vec<FileOp> = ops.into_iter().map(|(op, _)| op).collect();
        self.close_edit_preview();
        let mut buffers: Vec<Undoable> = Vec::new();
        let mut planned: Vec<Rewrite> = Vec::new();
        for file in files.into_iter().filter(|file| file.included) {
            match file.target {
                Where::Open { doc, before, edits, after } => {
                    let Some(document) = self.docs.iter_mut().find(|d| d.id == doc) else {
                        continue;
                    };
                    let applied = document.buffer.apply_batch(edits);
                    let exact = document.buffer.rope() == &after;
                    if applied.is_err() || !exact {
                        if applied.is_ok_and(|changed| changed) {
                            document.buffer.undo();
                        }
                        let failed = self.take_back_buffers(&subject, &buffers);
                        let why =
                            format!("the edits to {} did not come out as previewed", file.label);
                        self.message = Some(format!(
                            "Did not {}: {why}, so nothing was changed.{failed}",
                            subject.verb(),
                        ));
                        self.edits_done(Err(why));
                        return Outcome::Redraw;
                    }
                    buffers.push(Undoable { doc, label: file.label, before, after });
                }
                Where::Disk { before, after } => {
                    planned.push(Rewrite { path: file.path, expect: before, text: after });
                }
                // Its text goes in with the operation that creates it.
                Where::Created => {}
            }
        }
        self.follow_caret();

        let applied = Applied { subject, buffers, disk: Vec::new(), files: Vec::new() };
        if planned.is_empty() {
            return self.run_file_ops(applied, ops);
        }
        let tag = self.edits.tag();
        self.send_job(Job::Rewrite { tag, files: planned.clone() });
        self.message = Some(applied.subject.doing());
        self.edits.stage = Stage::Applying { tag, applied, planned, ops };
        Outcome::Redraw
    }

    /// Every file is written: create, move and delete what the edit says,
    /// or, when it says nothing of that, it is done.
    fn run_file_ops(&mut self, applied: Applied, ops: Vec<FileOp>) -> Outcome {
        if ops.is_empty() {
            let written: Vec<PathBuf> =
                applied.disk.iter().map(|rewrite| rewrite.path.clone()).collect();
            return self.applied_everywhere(applied, &written, &[]);
        }
        let tag = self.edits.tag();
        self.send_job(Job::FileOps { tag, ops });
        self.message = Some(applied.subject.doing());
        self.edits.stage = Stage::Moving { tag, applied };
        Outcome::Redraw
    }

    /// The worker wrote, or took back, what it was asked to.
    pub(super) fn edits_rewritten(&mut self, tag: u64, files: &[(PathBuf, Written)]) -> Outcome {
        match std::mem::take(&mut self.edits.stage) {
            Stage::Applying { tag: asked, applied, planned, ops } if asked == tag => {
                self.written(applied, &planned, files, ops)
            }
            Stage::Undoing { tag: asked, applied, did } if asked == tag => {
                self.written_back(applied, files, &did)
            }
            other => {
                self.edits.stage = other;
                Outcome::Continue
            }
        }
    }

    /// The files that are not open have been written, as far as that got.
    fn written(
        &mut self,
        mut applied: Applied,
        planned: &[Rewrite],
        files: &[(PathBuf, Written)],
        ops: Vec<FileOp>,
    ) -> Outcome {
        let done: Vec<&Rewrite> = planned
            .iter()
            .zip(files)
            .filter(|(_, (_, written))| *written == Written::Written)
            .map(|(rewrite, _)| rewrite)
            .collect();
        if done.is_empty() {
            // Nothing reached the disk, so nothing is left half done: the
            // open files go back as well.
            let failed = self.take_back_buffers(&applied.subject, &applied.buffers);
            let why = self.why_not(files);
            self.message = Some(format!(
                "Did not {}: {why}. Nothing was changed.{failed}",
                applied.subject.verb()
            ));
            self.edits_done(Err(why));
            return Outcome::Redraw;
        }
        let changed = done.iter().map(|rewrite| (rewrite.path.clone(), FileChangeType::CHANGED));
        self.tell_servers(changed.collect());

        applied.disk = done
            .iter()
            .map(|rewrite| Rewrite {
                path: rewrite.path.clone(),
                expect: rewrite.text.clone(),
                text: rewrite.expect.clone(),
            })
            .collect();
        let paths: Vec<PathBuf> = done.iter().map(|rewrite| rewrite.path.clone()).collect();
        if done.len() == planned.len() {
            return self.run_file_ops(applied, ops);
        }
        let written: Vec<String> = paths.iter().map(|path| self.label(path)).collect();
        let mut why = self.why_not(files);
        if !ops.is_empty() {
            let untried: Vec<String> = ops.iter().map(|op| self.said(op)).collect();
            let _ = write!(why, "; not done: {}", untried.join(", "));
        }
        let message = format!(
            "{}: wrote {}, then stopped: {why}. {} takes back what was written.",
            applied.subject.partly(),
            written.join(", "),
            applied.subject.undo(),
        );
        self.reload_written(&paths);
        self.edits.last = Some(applied);
        self.edits.offer = Some(message.clone());
        self.message = Some(message);
        self.undo_offer = true;
        self.last_undone = false;
        self.edits_done(Err(format!("only partly: {why}")));
        Outcome::Redraw
    }

    /// The edit is in: say so, and offer to take it back. `did` is what it
    /// created, moved and deleted, in words.
    fn applied_everywhere(
        &mut self,
        applied: Applied,
        written: &[PathBuf],
        did: &[String],
    ) -> Outcome {
        let untaken = self.reload_written(written);
        let files = applied.buffers.len() + applied.disk.len();
        let mut message = applied.subject.done();
        if files > 0 {
            let _ = write!(message, " in {files} {}", plural(files, "file"));
        }
        match (files, did) {
            (_, []) => {}
            (0, did) => {
                let _ = write!(message, ": {}", did.join(", "));
            }
            (_, did) => {
                let _ = write!(message, ", and {}", did.join(", "));
            }
        }
        message.push('.');
        let open = applied.buffers.len();
        if open > 0 {
            let _ = write!(
                message,
                " {open} open {} not saved yet.",
                if open == 1 { "file is" } else { "files are" }
            );
        }
        if !untaken.is_empty() {
            let _ = write!(
                message,
                " {} has unsaved changes and still shows the old text.",
                untaken.join(", ")
            );
        }
        self.edits.last = Some(applied);
        self.edits.offer = Some(message.clone());
        self.message = Some(message);
        self.undo_offer = true;
        self.last_undone = false;
        self.edits_done(Ok(()));
        Outcome::Redraw
    }

    /// The worker created, moved and deleted what it was asked to, or took
    /// that back, as far as it got.
    pub(super) fn edits_carried(&mut self, tag: u64, ops: &[(FileOp, Carried)]) -> Outcome {
        match std::mem::take(&mut self.edits.stage) {
            Stage::Moving { tag: asked, applied } if asked == tag => self.moved(applied, ops),
            Stage::Unmoving { tag: asked, applied } if asked == tag => {
                self.moved_back(applied, ops)
            }
            other => {
                self.edits.stage = other;
                Outcome::Continue
            }
        }
    }

    /// Bring the tree and the open files up to date with the operations that
    /// happened, and sort them into what happened, what stopped the run and
    /// why, and what was never tried — each in words.
    fn followed(&mut self, ops: &[(FileOp, Carried)]) -> Carrying {
        let mut carrying = Carrying::default();
        let mut events = Vec::new();
        for (op, carried) in ops {
            match carried {
                Carried::Done(reverse) => {
                    self.follow_file_op(op);
                    events.extend(watched(op));
                    carrying.did.push(self.did(op));
                    carrying.reverses.push(reverse.clone());
                }
                Carried::Failed(why) => {
                    carrying.failed = Some(format!("could not {}: {why}", self.said(op)));
                    carrying.left.push(op.clone());
                }
                Carried::NotReached => {
                    carrying.untried.push(self.said(op));
                    carrying.left.push(op.clone());
                }
            }
        }
        self.tell_servers(events);
        carrying
    }

    /// Tell the servers that files changed on disk, each server of those
    /// it registered to watch. The watcher would say so too, a moment later,
    /// but it may not be running, and a server that heard of a move late
    /// could answer the next question about the old file.
    fn tell_servers(&mut self, events: Vec<(PathBuf, FileChangeType)>) {
        if let Some(lsp) = self.lsp.as_mut() {
            lsp.files_changed(events);
        }
    }

    /// One file operation happened: the tree shows it, and an open file it
    /// moved follows it, so the next save goes to the new place — and its
    /// server knows it by the new name.
    fn follow_file_op(&mut self, op: &FileOp) {
        let (from, to) = match op {
            FileOp::Move { from, to } => (Some(from), to),
            FileOp::Create { path, .. }
            | FileOp::Discard { path, .. }
            | FileOp::Delete { path }
            | FileOp::Restore { path, .. } => (None, path),
        };
        if let Some(sidebar) = self.sidebar.as_mut() {
            for path in from.into_iter().chain([to]) {
                if let Some(dir) = path.parent() {
                    sidebar.tree.refresh_dir(dir);
                }
            }
            sidebar.request_listings();
            sidebar.sync_watches();
        }
        let Some(from) = from else { return };
        let moved: Vec<(DocId, PathBuf)> = self
            .docs
            .iter()
            .filter_map(|document| {
                let rest = document.buffer.path()?.strip_prefix(from).ok()?;
                let path = if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
                Some((document.id, path))
            })
            .collect();
        for (id, path) in moved {
            if let Some(document) = self.docs.iter_mut().find(|document| document.id == id) {
                document.buffer.set_path(path);
            }
            // What the edit changed in it is in the text it is opened with.
            self.lsp_open(id);
        }
    }

    /// The file operations ran, as far as they got.
    fn moved(&mut self, mut applied: Applied, ops: &[(FileOp, Carried)]) -> Outcome {
        let mut carrying = self.followed(ops);
        carrying.reverses.reverse();
        applied.files = std::mem::take(&mut carrying.reverses);
        let written: Vec<PathBuf> =
            applied.disk.iter().map(|rewrite| rewrite.path.clone()).collect();
        let Some(failed) = carrying.failed else {
            return self.applied_everywhere(applied, &written, &carrying.did);
        };
        let mut why = failed;
        if !carrying.untried.is_empty() {
            let _ = write!(why, "; not done: {}", carrying.untried.join(", "));
        }
        if carrying.did.is_empty() && written.is_empty() {
            // Nothing reached the disk, so nothing is left half done.
            let back = self.take_back_buffers(&applied.subject, &applied.buffers);
            self.message = Some(format!(
                "Did not {}: {why}. Nothing was changed.{back}",
                applied.subject.verb()
            ));
            self.edits_done(Err(why));
            return Outcome::Redraw;
        }
        let mut did: Vec<String> = Vec::new();
        if !written.is_empty() {
            let names: Vec<String> = written.iter().map(|path| self.label(path)).collect();
            did.push(format!("wrote {}", names.join(", ")));
        }
        did.extend(carrying.did);
        let message = format!(
            "{}: {}, then stopped: {why}. {} takes back what was done.",
            applied.subject.partly(),
            did.join("; "),
            applied.subject.undo(),
        );
        self.reload_written(&written);
        self.edits.last = Some(applied);
        self.edits.offer = Some(message.clone());
        self.message = Some(message);
        self.undo_offer = true;
        self.last_undone = false;
        self.edits_done(Err(format!("only partly: {why}")));
        Outcome::Redraw
    }

    /// The file operations were taken back, as far as that got. The files
    /// follow once all of them are.
    fn moved_back(&mut self, mut applied: Applied, ops: &[(FileOp, Carried)]) -> Outcome {
        let carrying = self.followed(ops);
        applied.files = carrying.left;
        let Some(failed) = carrying.failed else {
            return self.rewrite_back(applied, carrying.did);
        };
        let what = applied.subject.what();
        self.message = Some(if carrying.did.is_empty() {
            format!("Did not take {what} back: {failed}. Nothing was changed.")
        } else {
            format!(
                "Took {what} back only partly: {}, then stopped: {failed}. {} again tries the \
                 rest.",
                carrying.did.join(", "),
                applied.subject.undo(),
            )
        });
        self.edits.last = Some(applied);
        Outcome::Redraw
    }

    /// Why a rewrite stopped, in a sentence: each file that stopped it, and
    /// how many were never tried.
    fn why_not(&self, files: &[(PathBuf, Written)]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (path, written) in files {
            match written {
                Written::Changed => {
                    parts.push(format!("{} has changed on disk since", self.label(path)));
                }
                Written::Failed(why) => parts.push(why.clone()),
                Written::Written | Written::NotReached => {}
            }
        }
        let untried: Vec<String> = files
            .iter()
            .filter(|(_, written)| *written == Written::NotReached)
            .map(|(path, _)| self.label(path))
            .collect();
        let mut said = parts.join("; ");
        if !untried.is_empty() && files.iter().any(|(_, written)| *written == Written::Written) {
            let _ = write!(said, "; not written: {}", untried.join(", "));
        }
        said
    }

    /// Undo the edit's step of each of these buffers, checking each undo
    /// took back exactly the edit. A sentence naming any that did not, to
    /// add to whatever is being said, or nothing.
    fn take_back_buffers(&mut self, subject: &Subject, buffers: &[Undoable]) -> String {
        let mut kept: Vec<String> = Vec::new();
        for undoable in buffers {
            let Some(document) = self.docs.iter_mut().find(|d| d.id == undoable.doc) else {
                kept.push(undoable.label.clone());
                continue;
            };
            if document.buffer.rope() != &undoable.after {
                kept.push(undoable.label.clone());
                continue;
            }
            document.buffer.undo();
            if document.buffer.rope() != &undoable.before {
                // What was on top was not the edit after all. Put it back.
                document.buffer.redo();
                kept.push(undoable.label.clone());
            }
        }
        self.follow_caret();
        if kept.is_empty() {
            String::new()
        } else {
            format!(" {} still has {}; undo it there.", kept.join(", "), subject.kept())
        }
    }

    // ── undoing ──────────────────────────────────────────────────────────────

    /// Whether the status line's Undo is offering to take an edit back.
    pub(super) fn edit_undo_offered(&self) -> bool {
        self.undo_offer
            && self.edits.offer.is_some()
            && self.message.as_deref() == self.edits.offer.as_deref()
    }

    /// Take the last edit back, in every file it touched.
    pub(super) fn undo_edit(&mut self) -> Outcome {
        self.undo_offer = false;
        if self.edits.busy() {
            self.message = Some("Still working on the last edit across files…".into());
            return Outcome::Redraw;
        }
        let Some(applied) = self.edits.last.take() else {
            self.message = Some("There is no rename or code action to take back.".into());
            return Outcome::Redraw;
        };
        let what = applied.subject.what();
        // Everything is checked before anything is taken back.
        for undoable in &applied.buffers {
            if self.doc_by(undoable.doc).is_none_or(|d| d.buffer.rope() != &undoable.after) {
                self.message = Some(format!(
                    "Did not take {what} back: {} has been edited since. Undo that first, or \
                     undo {what} there.",
                    undoable.label
                ));
                self.edits.last = Some(applied);
                return Outcome::Redraw;
            }
        }
        let tag = self.edits.tag();
        if !applied.files.is_empty() {
            self.send_job(Job::FileOps { tag, ops: applied.files.clone() });
            self.message = Some(format!("Taking back {}…", applied.subject.named()));
            self.edits.stage = Stage::Unmoving { tag, applied };
            return Outcome::Redraw;
        }
        self.rewrite_back(applied, Vec::new())
    }

    /// Every file operation is taken back, which did `did`: put the files'
    /// old text back.
    fn rewrite_back(&mut self, applied: Applied, did: Vec<String>) -> Outcome {
        if applied.disk.is_empty() {
            return self.taken_back(&applied, &[], &did);
        }
        let tag = self.edits.tag();
        self.send_job(Job::Rewrite { tag, files: applied.disk.clone() });
        self.message = Some(format!("Taking back {}…", applied.subject.named()));
        self.edits.stage = Stage::Undoing { tag, applied, did };
        Outcome::Redraw
    }

    /// The written files have been put back, as far as that got.
    fn written_back(
        &mut self,
        mut applied: Applied,
        files: &[(PathBuf, Written)],
        did: &[String],
    ) -> Outcome {
        let restored: Vec<PathBuf> = files
            .iter()
            .filter(|(_, written)| *written == Written::Written)
            .map(|(path, _)| path.clone())
            .collect();
        let changed = restored.iter().map(|path| (path.clone(), FileChangeType::CHANGED));
        self.tell_servers(changed.collect());
        if restored.len() == applied.disk.len() {
            return self.taken_back(&applied, &restored, did);
        }
        let _ = self.reload_written(&restored);
        let why = self.why_not(files);
        let what = applied.subject.what();
        let mut done = did.to_vec();
        if !restored.is_empty() {
            let names: Vec<String> = restored.iter().map(|path| self.label(path)).collect();
            done.push(format!("restored {}", names.join(", ")));
        }
        if done.is_empty() {
            self.message = Some(format!("Did not take {what} back: {why}. Nothing was changed."));
        } else {
            self.message = Some(format!(
                "Took {what} back only partly: {}, then stopped: {why}. {} again tries the rest.",
                done.join("; "),
                applied.subject.undo(),
            ));
        }
        // What was put back is done with; the rest can be tried again.
        applied.disk.retain(|rewrite| !restored.contains(&rewrite.path));
        self.edits.last = Some(applied);
        Outcome::Redraw
    }

    /// Every file has its old text back on disk, and taking back the file
    /// operations did `did`: undo the buffers too.
    fn taken_back(&mut self, applied: &Applied, restored: &[PathBuf], did: &[String]) -> Outcome {
        let kept = self.take_back_buffers(&applied.subject, &applied.buffers);
        let _ = self.reload_written(restored);
        let files = applied.buffers.len() + restored.len();
        let mut message = format!("Took back {}", applied.subject.named());
        if files > 0 {
            let _ = write!(message, " in {files} {}", plural(files, "file"));
        }
        if !did.is_empty() {
            let _ = write!(message, "{} {}", if files > 0 { ", and" } else { ":" }, did.join(", "));
        }
        self.message = Some(format!("{message}.{kept}"));
        self.edits.offer = None;
        Outcome::Redraw
    }

    // ── the panel ────────────────────────────────────────────────────────────

    /// Lay the panel's hit regions out, from the geometry it draws with.
    pub(super) fn layout_edit_preview(&self, hits: &mut nun_input::HitMap<Target>) {
        let (Some(area), Some(preview)) = (self.edit_preview_area(), self.edits.preview.as_ref())
        else {
            return;
        };
        let spot = |spot| Target::EditPreview(spot);
        hits.push(super::cells(SearchView::header_area(area)), spot(Spot::Header), false);
        if let Some(cell) = SearchView::back_area(area) {
            hits.push(super::cells(cell), spot(Spot::Back), true);
        }
        hits.push(super::cells(SearchView::query_area(area)), spot(Spot::Field), false);
        hits.push(super::cells(SearchView::replace_area(area)), spot(Spot::Field), false);
        if let Some(cell) = SearchView::apply_area(area) {
            hits.push(super::cells(cell), spot(Spot::Apply), true);
        }
        let actions = preview.subject.actions();
        for index in 0..actions.len() {
            if let Some(cell) = SearchView::action_area(area, actions, index) {
                hits.push(super::cells(cell), spot(Spot::Action(index)), true);
            }
        }
        let rows = SearchView::rows_area(area);
        hits.push(super::cells(rows), spot(Spot::Empty), false);
        let shown = SearchView::visible_rows(area);
        let view = preview.view_rows(preview.scroll..preview.scroll + shown);
        for (within, index) in (preview.scroll..preview.rows.len()).take(shown).enumerate() {
            let Ok(offset) = u16::try_from(within) else { break };
            let line = Rect { y: rows.y + offset, height: 1, ..rows };
            hits.push(super::cells(line), spot(Spot::Row(index)), true);
            if let Some(cell) = SearchView::marker_area(area, &view, within, 0)
                && matches!(preview.rows[index], Line::File(_))
            {
                hits.push(super::cells(cell), spot(Spot::Mark(index)), true);
            }
        }
    }

    /// Draw it.
    pub(super) fn render_edit_preview(&self, cells: &mut Cells) {
        let (Some(area), Some(preview)) = (self.edit_preview_area(), self.edits.preview.as_ref())
        else {
            return;
        };
        let hovered = match self.hover.current() {
            Some(Target::EditPreview(spot)) => Some(spot),
            _ => None,
        };
        let first = preview.scroll;
        let last = first.saturating_add(SearchView::visible_rows(area));
        let within = |row: Option<usize>| {
            row.filter(|row| (first..last).contains(row)).map(|row| row - first)
        };
        let row = match hovered {
            Some(Spot::Row(row) | Spot::Mark(row)) => Some(row),
            _ => None,
        };
        let action = match hovered {
            Some(Spot::Action(index)) => Some(index),
            _ => None,
        };
        let subject = &preview.subject;
        let (change, _) = subject.change();
        let button = subject.actions()[0];
        let summary = match hovered {
            Some(Spot::Back) => format!("Put the preview away and {change} nothing"),
            Some(Spot::Apply | Spot::Action(0)) => format!("{button} in every ticked file"),
            Some(Spot::Action(_)) => {
                let mut nothing = format!("{change} nothing");
                nothing[..1].make_ascii_uppercase();
                nothing
            }
            Some(Spot::Mark(_)) => {
                format!("Leave this file out of {}, or put it back", subject.what())
            }
            _ => preview.summary.clone(),
        };
        let rows = preview.view_rows(first..last);
        let (query, replacement) = subject.fields();
        SearchView::new(query, &rows, &self.palette)
            .title(subject.title())
            .actions(subject.actions())
            .hovered_action(action)
            .replacement(replacement)
            .scrolled_to(0)
            .widest_line(preview.widest)
            .selected(within(preview.selected))
            .hovered(within(row))
            .hovered_back(hovered == Some(Spot::Back))
            .hovered_apply(hovered == Some(Spot::Apply))
            .focused(self.focus == Focus::EditPreview)
            .summary(Some(&summary))
            .render(area, cells);
    }

    /// A click in the panel.
    pub(super) fn edit_preview_press(&mut self, spot: Spot) -> Outcome {
        self.focus = Focus::EditPreview;
        match spot {
            Spot::Back | Spot::Action(1..) => self.cancel_edit_preview(),
            Spot::Apply | Spot::Action(0) => self.apply_edit_preview(),
            Spot::Mark(row) => self.toggle_edit_file(row),
            Spot::Row(row) => {
                if let Some(preview) = self.edits.preview.as_mut() {
                    preview.selected = Some(row);
                }
                self.pick_edit_row(row)
            }
            Spot::Header | Spot::Field | Spot::Empty => Outcome::Redraw,
        }
    }

    /// A key while the panel has the keyboard: the arrows move, Space leaves
    /// the selected file out or puts it back, Enter applies, Esc does not.
    pub(super) fn edit_preview_key(&mut self, key: &KeyEvent) -> Outcome {
        let page = self.edit_preview_area().map_or(1, SearchView::visible_rows).max(1);
        match key.code {
            KeyCode::Esc => self.cancel_edit_preview(),
            KeyCode::Enter => self.apply_edit_preview(),
            KeyCode::Char(' ') => match self.edits.preview.as_ref().and_then(|p| p.selected) {
                Some(row) => self.toggle_edit_file(row),
                None => Outcome::Redraw,
            },
            KeyCode::Up => self.move_edit_selection(-1),
            KeyCode::Down => self.move_edit_selection(1),
            KeyCode::PageUp => self.move_edit_selection(-isize::try_from(page).unwrap_or(1)),
            KeyCode::PageDown => self.move_edit_selection(isize::try_from(page).unwrap_or(1)),
            _ => Outcome::Continue,
        }
    }

    fn move_edit_selection(&mut self, delta: isize) -> Outcome {
        let visible = self.edit_preview_area().map_or(0, SearchView::visible_rows);
        let Some(preview) = self.edits.preview.as_mut() else { return Outcome::Continue };
        let rows = preview.rows.len();
        if rows == 0 {
            return Outcome::Redraw;
        }
        let last = isize::try_from(rows).unwrap_or(1) - 1;
        let at = match preview.selected {
            Some(at) => isize::try_from(at).unwrap_or(0).saturating_add(delta),
            None if delta < 0 => last,
            None => 0,
        };
        let at = usize::try_from(at.clamp(0, last)).unwrap_or(0);
        preview.selected = Some(at);
        if at < preview.scroll {
            preview.scroll = at;
        } else if visible > 0 && at >= preview.scroll + visible {
            preview.scroll = at + 1 - visible;
        }
        Outcome::Redraw
    }

    /// The wheel over the panel.
    pub(super) fn edit_preview_scroll(&mut self, down: bool) -> Outcome {
        let visible = self.edit_preview_area().map_or(0, SearchView::visible_rows);
        let Some(preview) = self.edits.preview.as_mut() else { return Outcome::Continue };
        let last = preview.rows.len().saturating_sub(visible);
        preview.scroll =
            if down { (preview.scroll + 3).min(last) } else { preview.scroll.saturating_sub(3) };
        Outcome::Redraw
    }

    /// Leave the file a row belongs to out of the edit, or put it back.
    fn toggle_edit_file(&mut self, row: usize) -> Outcome {
        let Some(preview) = self.edits.preview.as_mut() else { return Outcome::Continue };
        let Some(at) = preview.file_of(row) else { return Outcome::Continue };
        if matches!(preview.files[at].target, Where::Created) {
            self.message = Some(format!(
                "{} is new, and its text comes with creating it; it cannot be left out on its \
                 own.",
                preview.files[at].label
            ));
            return Outcome::Redraw;
        }
        preview.files[at].included = !preview.files[at].included;
        preview.relist();
        Outcome::Redraw
    }

    /// A file row folds; a line opens its file there.
    fn pick_edit_row(&mut self, row: usize) -> Outcome {
        let Some(preview) = self.edits.preview.as_mut() else { return Outcome::Continue };
        let Some(line) = preview.rows.get(row).copied() else { return Outcome::Continue };
        match line {
            Line::File(at) => {
                if !preview.collapsed.remove(&at) {
                    preview.collapsed.insert(at);
                }
                preview.relist();
                Outcome::Redraw
            }
            // Nothing to open: the file is not there yet, or moves.
            Line::Op(_) => Outcome::Redraw,
            Line::Before(at, _) | Line::After(at, _)
                if matches!(preview.files[at].target, Where::Created) =>
            {
                Outcome::Redraw
            }
            Line::Before(at, index) | Line::After(at, index) => {
                let path = preview.files[at].path.clone();
                let line = preview.files[at].changes[index].line;
                self.open_file(&path);
                if self.doc().buffer.path().is_some_and(|open| super::same_file(open, &path)) {
                    let buffer = &self.doc().buffer;
                    let line = usize::try_from(line).unwrap_or(1).saturating_sub(1);
                    let at = buffer.line_start(line.min(buffer.len_lines().saturating_sub(1)));
                    self.doc_mut()
                        .buffer
                        .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
                    self.follow_caret();
                }
                Outcome::Redraw
            }
        }
    }

    /// How a file reads in the panel and in messages: relative to the folder
    /// when it is inside it, whole otherwise.
    ///
    /// A server names files by their resolved paths, and the folder may have
    /// been opened through a symbolic link — on macOS every temporary folder
    /// is. So a path that is not under the folder as it was opened is tried
    /// under the folder as the server knows it, which an open file shows by
    /// having a name of each kind. Working it out here costs no filesystem
    /// call, which on this thread is the point.
    fn label(&self, path: &Path) -> String {
        let Some(root) = self.workspace_root() else { return path.display().to_string() };
        if let Ok(relative) = path.strip_prefix(&root) {
            return relative.display().to_string();
        }
        let lsp = self.lsp.as_ref();
        for document in &self.docs {
            let Some(relative) =
                document.buffer.path().and_then(|mine| mine.strip_prefix(&root).ok())
            else {
                continue;
            };
            let Some(theirs) = lsp
                .and_then(|lsp| lsp.identifier(document.id))
                .and_then(|id| nun_lsp::uri::to_path(&id.uri))
            else {
                continue;
            };
            if !theirs.ends_with(relative) {
                continue;
            }
            if let Some(resolved) = theirs.ancestors().nth(relative.components().count())
                && let Ok(relative) = path.strip_prefix(resolved)
            {
                return relative.display().to_string();
            }
        }
        path.display().to_string()
    }

    /// Whether an edit is waiting on the worker, or being checked. Only the
    /// tests ask.
    #[cfg(test)]
    pub(super) const fn edits_reading(&self) -> bool {
        matches!(self.edits.stage, Stage::Reading { .. })
    }

    /// Whether the preview is up. Only the tests ask.
    #[cfg(test)]
    pub(super) const fn edit_previewing(&self) -> bool {
        self.edits.preview.is_some()
    }

    /// The preview as it reads, a row a line: `[file]`, `3- before` and
    /// `3+ after`. Only the tests ask.
    #[cfg(test)]
    pub(super) fn edit_preview_rows(&self) -> Vec<String> {
        let preview = self.edits.preview.as_ref().expect("a preview is up");
        preview
            .rows
            .iter()
            .map(|line| match *line {
                Line::File(at) => {
                    let file = &preview.files[at];
                    format!("[{}{}]", if file.included { "" } else { "out: " }, file.label)
                }
                Line::Before(at, index) => {
                    let change = &preview.files[at].changes[index];
                    format!("{}- {}", change.line, change.before)
                }
                Line::After(at, index) => {
                    let change = &preview.files[at].changes[index];
                    format!("{}+ {}", change.line, change.after)
                }
                Line::Op(at) => format!("{{{}}}", preview.ops[at].1),
            })
            .collect()
    }
}

/// What a server watching files would hear of a file operation.
fn watched(op: &FileOp) -> Vec<(PathBuf, FileChangeType)> {
    match op {
        FileOp::Create { path, .. } | FileOp::Restore { path, .. } => {
            vec![(path.clone(), FileChangeType::CREATED)]
        }
        FileOp::Delete { path } | FileOp::Discard { path, .. } => {
            vec![(path.clone(), FileChangeType::DELETED)]
        }
        FileOp::Move { from, to } => {
            vec![(from.clone(), FileChangeType::DELETED), (to.clone(), FileChangeType::CREATED)]
        }
    }
}

/// `word`, or `words`.
fn plural(count: usize, word: &str) -> String {
    if count == 1 { word.to_string() } else { format!("{word}s") }
}

/// A server's edit as its steps in order, in either of the two forms it can
/// come in, with every path as `local` makes it, or the reason it cannot be
/// done as a whole.
fn flatten(edit: &WorkspaceEdit, local: &impl Fn(PathBuf) -> PathBuf) -> Result<Vec<Step>, String> {
    let path = |uri: &nun_lsp::types::Uri| {
        nun_lsp::uri::to_path(uri)
            .map(local)
            .ok_or_else(|| format!("the server edits {}, which is not a file", uri.as_str()))
    };
    let text = |edit: &nun_lsp::types::TextDocumentEdit| -> Result<Step, String> {
        let edits = edit
            .edits
            .iter()
            .map(|edit| match edit {
                OneOf::Left(edit) => edit.clone(),
                OneOf::Right(annotated) => annotated.text_edit.clone(),
            })
            .collect();
        let path = path(&edit.text_document.uri)?;
        Ok(Step::Edit(FileEdits { path, version: edit.text_document.version, edits }))
    };
    let mut steps = Vec::new();
    // Where both are given the protocol prefers the versioned form.
    match (&edit.document_changes, &edit.changes) {
        (Some(DocumentChanges::Edits(edits)), _) => {
            for edit in edits {
                steps.push(text(edit)?);
            }
        }
        (Some(DocumentChanges::Operations(operations)), _) => {
            for operation in operations {
                steps.push(match operation {
                    DocumentChangeOperation::Edit(edit) => text(edit)?,
                    DocumentChangeOperation::Op(op) => {
                        Step::Op(Wanted::of(op, local).ok_or_else(|| {
                            "the server creates, moves or deletes something that is not a file"
                                .to_string()
                        })?)
                    }
                });
            }
        }
        (None, Some(changes)) => {
            for (uri, edits) in changes {
                steps.push(Step::Edit(FileEdits {
                    path: path(uri)?,
                    version: None,
                    edits: edits.clone(),
                }));
            }
        }
        (None, None) => {}
    }
    // A file with nothing to change in it is not one to preview or check.
    steps.retain(|step| !matches!(step, Step::Edit(file) if file.edits.is_empty()));
    Ok(steps)
}

/// `text` with every line ending as `\n`: what a buffer holds.
fn line_feeds(text: &str) -> String {
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.into() }
}

/// A server's edits in the order they are spliced, joined where they share a
/// place, with their new text's line endings made `\n` — or why they cannot
/// be made at all.
///
/// Exactly what `Buffer::apply_batch` does before it applies a batch, rule for
/// rule, so the text this leads to is the text a buffer given the same edits
/// ends up holding (a property test below holds the two together): sorted by
/// where each starts and otherwise kept in order; inserts at one place joined
/// in that order, with only the last of them allowed to replace anything; an
/// insert where a replacement starts put after its text; anything else that
/// starts inside an earlier edit refused. Line endings are made `\n` only
/// after joining, so a `\r\n` split across two inserts is still one break.
fn join(mut edits: Vec<Edit>, len: usize) -> Result<Vec<Edit>, String> {
    if edits.iter().any(|edit| edit.start > edit.end || edit.end > len) {
        return Err("the server's edits fall outside the file".into());
    }
    edits.sort_by_key(|edit| edit.start);
    let mut joined: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match joined.last_mut() {
            Some(last) if last.start == edit.start && last.start == last.end => {
                last.text.push_str(&edit.text);
                last.end = edit.end;
            }
            Some(last) if last.start == edit.start && edit.start == edit.end => {
                last.text.push_str(&edit.text);
            }
            Some(last) if last.end > edit.start => {
                return Err("the server's edits overlap".into());
            }
            _ => joined.push(edit),
        }
    }
    for edit in &mut joined {
        edit.text = line_feeds(&edit.text);
    }
    Ok(joined)
}

/// The text of `before` with `edits`, as [`join`] leaves them, made to it.
///
/// This is the one engine behind a file's preview, and behind what is written
/// or checked for it: an open buffer is edited through `apply_batch` and then
/// compared with this, character for character.
fn splice(before: &Rope, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(before.len_bytes());
    let mut at = 0;
    for edit in edits {
        out.extend(before.slice(at..edit.start).chunks());
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.extend(before.slice(at..).chunks());
    out
}

/// A run of lines some edits change, while the rows are being worked out.
struct Run {
    /// The first and last lines of the text before.
    from: usize,
    to: usize,
    /// The first and last lines of the text after.
    after_from: usize,
    after_to: usize,
    /// What the edits replace, as char ranges of the text before.
    ranges: Vec<Range<usize>>,
}

/// The lines `edits` change, as they read before and after.
///
/// Edits whose lines touch share a row, so a row is a run of whole lines of
/// `before` and the run of whole lines of `after` it became, each joined with
/// `line_break` when there is more than one — which only happens when an edit
/// takes a line break away or puts one in. Where an edit lands in `after` is
/// its start moved by what every edit before it added or took away, which is
/// exact because `after` is [`splice`] of the same list.
fn changes(before: &Rope, edits: &[Edit], after: &Rope, line_break: &str) -> Vec<Change> {
    let mut runs: Vec<Run> = Vec::new();
    let mut shift: isize = 0;
    for edit in edits {
        let from = before.char_to_line(edit.start);
        let to = before.char_to_line(edit.end);
        let moved = edit.start.saturating_add_signed(shift);
        let added = edit.text.chars().count();
        let removed = isize::try_from(edit.end - edit.start).unwrap_or(isize::MAX);
        shift = shift.saturating_add(isize::try_from(added).unwrap_or(isize::MAX) - removed);
        let after_from = after.char_to_line(moved.min(after.len_chars()));
        let after_to = after.char_to_line((moved + added).min(after.len_chars()));
        match runs.last_mut() {
            Some(run) if run.to >= from => {
                run.to = run.to.max(to);
                run.after_to = run.after_to.max(after_to);
                run.ranges.push(edit.start..edit.end);
            }
            _ => runs.push(Run {
                from,
                to,
                after_from,
                after_to,
                ranges: std::iter::once(edit.start..edit.end).collect(),
            }),
        }
    }
    runs.into_iter()
        .map(|run| {
            let (text, offsets) = shown(before, run.from, run.to, line_break);
            let matched = run
                .ranges
                .iter()
                .map(|range| {
                    to_u32(place(&offsets, range.start))..to_u32(place(&offsets, range.end))
                })
                .collect();
            Change {
                line: to_u32(run.from + 1),
                after_line: to_u32(run.after_from + 1),
                before: text,
                matched,
                after: shown(after, run.after_from, run.after_to, line_break).0,
            }
        })
        .collect()
}

/// Lines `from` to `to` of `text` as one row, and for each line its first
/// char in `text`, where it starts in the row, and how many chars it has
/// there — for [`place`]. Lines are joined with `line_break`, which may be
/// more than one char.
fn shown(
    text: &Rope,
    from: usize,
    to: usize,
    line_break: &str,
) -> (String, Vec<(usize, usize, usize)>) {
    let mut row = String::new();
    let mut lines = Vec::new();
    let mut at = 0;
    for line in from..=to {
        if line > from {
            row.push_str(line_break);
            at += line_break.chars().count();
        }
        let content = line_text(text, line);
        let chars = content.chars().count();
        lines.push((text.line_to_char(line), at, chars));
        row.push_str(&content);
        at += chars;
    }
    (row, lines)
}

/// Where char `char` of the text falls in a row [`shown`] built. A place in a
/// line's ending, past its last char, is the end of the line.
fn place(lines: &[(usize, usize, usize)], char: usize) -> usize {
    let Some(&(first, start, chars)) =
        lines.iter().rev().find(|(first, ..)| *first <= char).or(lines.first())
    else {
        return 0;
    };
    start + char.saturating_sub(first).min(chars)
}

/// One line of `text`, without whatever ends it.
fn line_text(text: &Rope, line: usize) -> String {
    let line = text.line(line).to_string();
    let line = line.strip_suffix('\n').unwrap_or(&line);
    line.strip_suffix('\r').unwrap_or(line).to_string()
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// What a file that is not open becomes: its new text, and the rows that
/// preview it.
///
/// Everything about the file that the edits do not touch is kept byte for
/// byte — its byte-order mark, its line endings, mixed or not, and whether it
/// ends in one. Positions are counted without the byte-order mark, as they are
/// for an open file, whose buffer does not hold one. New text a server writes
/// with line breaks gets the file's own.
///
/// A file whose lines end in a bare carriage return is refused: the protocol
/// counts that as a line break and nothing else here does, so the server's
/// line numbers would land on the wrong lines.
fn disk_plan(
    text: &str,
    encoding: Encoding,
    edits: &[TextEdit],
    line_break: &str,
) -> Result<(String, Vec<Change>), String> {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", text),
    };
    if body.match_indices('\r').any(|(at, _)| !body[at + 1..].starts_with('\n')) {
        return Err("it ends lines with a bare carriage return, which the server counts \
                    differently"
            .into());
    }
    let crlf = nun_core::LineEnding::detect(body) == nun_core::LineEnding::Crlf;
    let before = Rope::from_str(body);
    let mut edits = join(encoding.edits(&before, edits), before.len_chars())?;
    if crlf {
        for edit in &mut edits {
            edit.text = edit.text.replace('\n', "\r\n");
        }
    }
    let after = splice(&before, &edits);
    let changes = changes(&before, &edits, &Rope::from_str(&after), line_break);
    Ok((format!("{bom}{after}"), changes))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use nun_core::Buffer;
    use nun_lsp::types::{Position, Range as Span, TextDocumentEdit, Uri};
    use proptest::prelude::*;

    use super::*;

    /// The default preset's line break, which the rows below are written with.
    const BREAK: &str = "↵";

    fn uri(path: &str) -> Uri {
        Uri::from_str(&format!("file://{path}")).unwrap()
    }

    fn text_edit(line: u32, from: u32, to: u32, text: &str) -> TextEdit {
        TextEdit {
            range: Span {
                start: Position { line, character: from },
                end: Position { line, character: to },
            },
            new_text: text.into(),
        }
    }

    #[test]
    fn both_forms_of_an_edit_come_out_the_same() {
        let changes = WorkspaceEdit {
            changes: Some(HashMap::from([(uri("/p/a.rs"), vec![text_edit(0, 0, 1, "x")])])),
            ..WorkspaceEdit::default()
        };
        let versioned = WorkspaceEdit {
            document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                text_document: nun_lsp::types::OptionalVersionedTextDocumentIdentifier {
                    uri: uri("/p/a.rs"),
                    version: Some(3),
                },
                edits: vec![OneOf::Left(text_edit(0, 0, 1, "x"))],
            }])),
            ..WorkspaceEdit::default()
        };
        let file = |edit| match flatten(edit, &|path| path).unwrap().remove(0) {
            Step::Edit(file) => file,
            Step::Op(op) => panic!("{op:?}"),
        };
        let (plain, with) = (file(&changes), file(&versioned));
        assert_eq!(plain.path, PathBuf::from("/p/a.rs"));
        assert_eq!(plain.edits, with.edits);
        assert_eq!((plain.version, with.version), (None, Some(3)));
    }

    #[test]
    fn a_file_named_twice_is_refused() {
        let twice =
            WorkspaceEdit {
                document_changes: Some(DocumentChanges::Operations(vec![
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: nun_lsp::types::OptionalVersionedTextDocumentIdentifier {
                        uri: uri("/p/a.rs"),
                        version: None,
                    },
                    edits: vec![OneOf::Left(text_edit(0, 0, 1, "x"))],
                });
                2
            ])),
                ..WorkspaceEdit::default()
            };
        let steps = flatten(&twice, &|path| path).unwrap();
        let label = |path: &Path| path.display().to_string();
        assert!(files::sort(steps, &label).unwrap_err().contains("twice"));
    }

    #[test]
    fn a_file_on_disk_keeps_its_mark_its_endings_and_its_last_line() {
        let text = "\u{feff}let cat = 1;\r\nlet cat = 2;\nno newline cat";
        let edits =
            [text_edit(0, 4, 7, "dog"), text_edit(1, 4, 7, "dog"), text_edit(2, 11, 14, "dog")];
        let (after, changes) = disk_plan(text, Encoding::Utf16, &edits, BREAK).unwrap();
        assert_eq!(after, "\u{feff}let dog = 1;\r\nlet dog = 2;\nno newline dog");
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].before, "let cat = 1;", "no mark, no ending");
        assert_eq!(changes[0].matched, vec![4..7]);
        assert_eq!(changes[2].after, "no newline dog");
    }

    #[test]
    fn positions_are_counted_in_the_servers_units_not_bytes() {
        // Two UTF-16 units for the crab, four bytes, one char.
        let text = "🦀 cat é cat\n";
        let edits = [text_edit(0, 3, 6, "dog"), text_edit(0, 9, 12, "dog")];
        let (after, changes) = disk_plan(text, Encoding::Utf16, &edits, BREAK).unwrap();
        assert_eq!(after, "🦀 dog é dog\n");
        assert_eq!(changes.len(), 1, "one line, one row");
        assert_eq!(changes[0].matched, [2..5, 8..11], "char offsets");

        let edits = [text_edit(0, 5, 8, "dog")];
        let (after, _) = disk_plan(text, Encoding::Utf8, &edits, BREAK).unwrap();
        assert_eq!(after, "🦀 dog é cat\n");
    }

    #[test]
    fn new_text_with_line_breaks_takes_the_files_own() {
        let (after, _) =
            disk_plan("a\r\nb\r\n", Encoding::Utf16, &[text_edit(0, 1, 1, "\nx")], BREAK).unwrap();
        assert_eq!(after, "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn a_bare_carriage_return_is_refused_rather_than_miscounted() {
        let refused = disk_plan("a\rb cat\n", Encoding::Utf16, &[text_edit(1, 2, 5, "dog")], BREAK);
        assert!(refused.unwrap_err().contains("bare carriage return"));
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let refused = disk_plan(
            "cat\n",
            Encoding::Utf16,
            &[text_edit(0, 0, 2, "x"), text_edit(0, 1, 3, "y")],
            BREAK,
        );
        assert!(refused.unwrap_err().contains("overlap"));
    }

    #[test]
    fn a_change_that_moves_a_line_down_still_shows_the_right_after_line() {
        let before = Rope::from_str("a\nb cat\n");
        let edits = join(vec![Edit::replace(0, 1, "x\ny"), Edit::replace(4, 7, "dog")], 8).unwrap();
        let after = Rope::from_str(&splice(&before, &edits));
        assert_eq!(after.to_string(), "x\ny\nb dog\n");
        let rows = changes(&before, &edits, &after, BREAK);
        assert_eq!((rows[0].line, rows[0].after_line), (1, 1));
        assert_eq!(rows[0].after, "x↵y", "both lines it became");
        assert_eq!((rows[1].line, rows[1].after_line), (2, 3), "numbered where it now is");
        assert_eq!(rows[1].after, "b dog");
    }

    #[test]
    fn edits_that_join_lines_share_a_row_and_mark_where_they_are() {
        let before = Rope::from_str("let cat\n= cat;\nnext\n");
        let edits =
            join(vec![Edit::replace(4, 8, "dog "), Edit::replace(10, 13, "dog")], 20).unwrap();
        let after = Rope::from_str(&splice(&before, &edits));
        assert_eq!(after.to_string(), "let dog = dog;\nnext\n");
        let rows = changes(&before, &edits, &after, BREAK);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].before, "let cat↵= cat;");
        assert_eq!(rows[0].matched, vec![4..8, 10..13], "the break counts as one");
        assert_eq!(rows[0].after, "let dog = dog;");

        // A break drawn with two chars moves what follows it by two.
        let rows = changes(&before, &edits, &after, "$\u{301}");
        assert_eq!(rows[0].before, "let cat$\u{301}= cat;");
        assert_eq!(rows[0].matched, vec![4..9, 11..14], "the first edit takes the break with it");
    }

    #[test]
    fn an_insert_where_a_replacement_starts_goes_after_it_as_a_buffer_puts_it() {
        let text = "fn cat";
        let edits = vec![Edit::replace(3, 6, "dog"), Edit::insert(3, "x")];
        let spliced = splice(&Rope::from_str(text), &join(edits.clone(), 6).unwrap());
        let mut buffer = Buffer::from_text(text);
        buffer.apply_batch(edits).unwrap();
        assert_eq!(spliced, "fn dogx");
        assert_eq!(buffer.text().to_string(), spliced);
    }

    #[test]
    fn past_the_end_of_a_crlf_line_on_disk_keeps_its_carriage_return() {
        let (after, _) =
            disk_plan("cat\r\nx", Encoding::Utf16, &[text_edit(0, 0, 99, "dog")], BREAK).unwrap();
        assert_eq!(after, "dog\r\nx");
    }

    fn arbitrary_edit() -> impl Strategy<Value = (usize, usize, String)> {
        (0usize..40, 0usize..4, "[x🦀\n]{0,3}")
    }

    proptest! {
        /// Whatever a server sends, the preview is taken from exactly the
        /// text that is written: every untouched line is there as it was, in
        /// any of the three encodings, with or without a byte-order mark or
        /// a last line ending; and each row's before and after are the lines
        /// of the two texts it names.
        #[test]
        fn the_preview_is_of_the_text_that_is_written(
            lines in proptest::collection::vec("[a-c🦀é中 ]{0,6}", 1..6),
            crlf in any::<bool>(),
            bom in any::<bool>(),
            last in any::<bool>(),
            encoding in prop_oneof![Just(Encoding::Utf8), Just(Encoding::Utf16), Just(Encoding::Utf32)],
            picks in proptest::collection::vec(arbitrary_edit(), 0..5),
        ) {
            let ending = if crlf { "\r\n" } else { "\n" };
            let mut body = lines.join(ending);
            if last {
                body.push_str(ending);
            }
            let text = if bom { format!("\u{feff}{body}") } else { body.clone() };
            let rope = Rope::from_str(&body);
            let len = rope.len_chars();
            let edits: Vec<TextEdit> = picks
                .into_iter()
                .map(|(from, span, new)| {
                    let from = from.min(len);
                    let to = (from + span).min(len);
                    TextEdit { range: encoding.range(&rope, from..to), new_text: new }
                })
                .collect();
            let Ok((after, rows)) = disk_plan(&text, encoding, &edits, BREAK) else {
                // Overlapping, or split a CRLF: refused, which is allowed.
                return Ok(());
            };
            prop_assert_eq!(after.starts_with('\u{feff}'), bom);
            let split = |text: &str| -> Vec<String> {
                let text = text.strip_prefix('\u{feff}').unwrap_or(text);
                text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect()
            };
            let (old, new) = (split(&text), split(&after));
            for row in &rows {
                let first = usize::try_from(row.line).unwrap() - 1;
                let shown: Vec<&str> = row.before.split(BREAK).collect();
                prop_assert_eq!(&old[first..first + shown.len()], shown.as_slice(), "{:?}", row);
                let first = usize::try_from(row.after_line).unwrap() - 1;
                let shown: Vec<&str> = row.after.split(BREAK).collect();
                prop_assert_eq!(&new[first..first + shown.len()], shown.as_slice(), "{:?}", row);
            }
        }

        /// What an open buffer ends up holding after `apply_batch` is exactly
        /// what the preview was spliced from, inserts sharing a place and
        /// replacements starting where an insert does included — and the two
        /// refuse the same batches.
        #[test]
        fn a_buffer_ends_up_holding_what_was_previewed(
            text in "[ab🦀é中\n]{0,12}",
            picks in proptest::collection::vec(arbitrary_edit(), 0..6),
        ) {
            let rope = Rope::from_str(&text);
            let len = rope.len_chars();
            let edits: Vec<Edit> = picks
                .into_iter()
                .map(|(from, span, new)| {
                    let from = from % (len + 1);
                    Edit::replace(from, (from + span).min(len), new)
                })
                .collect();
            let mut buffer = Buffer::from_text(&text);
            let applied = buffer.apply_batch(edits.clone());
            match join(edits, len) {
                Ok(joined) => {
                    prop_assert!(applied.is_ok());
                    prop_assert_eq!(buffer.text().to_string(), splice(&rope, &joined));
                }
                Err(_) => prop_assert!(applied.is_err()),
            }
        }
    }
}
