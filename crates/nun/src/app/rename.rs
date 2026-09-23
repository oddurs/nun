//! Renaming a symbol across the project, through its language server.
//!
//! A rename writes files nobody has open, which makes it the second most
//! destructive thing the editor does after a project-wide replace. So it goes
//! the way a replace goes: nothing is written until every edit has been shown,
//! grouped by file, in the panel the replace uses, and applied from there.
//!
//! **Asking.** Where the server says it can, it is asked first what would be
//! renamed (`textDocument/prepareRename`). It answers with the range of the
//! name and perhaps a placeholder, or says to use the editor's own idea of the
//! word under the caret, or says nothing here can be renamed. A server that
//! cannot be asked is treated as though it had said the second. The new name
//! is typed into a prompt drawn at the symbol itself, filled in with the old
//! one; Enter or the prompt's button asks for the rename.
//!
//! **Checking.** The server's answer is a `WorkspaceEdit`, in either of its
//! two forms. Either every file in it can be edited as it says or none is:
//!
//! - an edit that would create, move or delete a file is refused whole, since
//!   nun does not do those as part of a rename (and says so when it starts
//!   the server, so a server that listens will not send one);
//! - an edit to a document at a version the editor no longer has is refused
//!   whole, and so is one naming a version for a file that is not open, since
//!   it describes text nobody can see;
//! - an edit to an open file whose unsaved changes the server has not seen is
//!   refused whole, since its positions describe the file on disk;
//! - a file named twice, edits that overlap, a file that cannot be read as
//!   text, or one whose lines end in a bare carriage return are refused whole
//!   too.
//!
//! **Previewing.** Files that are open are previewed from their buffers. Files
//! that are not are read on the workspace worker, never here, and previewed
//! from what was read. What the panel shows for a file is computed by the same
//! splice whose result is written or compared against, so the preview and the
//! write cannot disagree. Each file carries a tick: clicking it leaves the file
//! out. Leaving a file out of a rename usually breaks the build, but it is the
//! person's project and the choice is theirs to make with their eyes open.
//!
//! **Applying.** Open files are edited in their buffers, through
//! `Buffer::apply_batch`, so each gets exactly one undo step and none of them
//! is saved; each buffer is checked afterwards to hold exactly what was
//! previewed, and if one does not, every buffer is taken back and nothing is
//! written. Files that are not open are written on the worker with
//! `Job::Rewrite`, which writes a file only while it still holds the text that
//! was previewed, and checks all of them before writing any.
//!
//! **What undo means.** An editor undo is per buffer, and a rename touches
//! files that have no buffer. So the rename keeps its own record and its own
//! way back, "Undo rename" — offered on the status line as soon as the rename
//! lands, and in the palette after that:
//!
//! - an open file is taken back by undoing its buffer's rename step, provided
//!   its text is still exactly what the rename left and that step is still the
//!   one on top (an undo that turns out to reverse something else is redone at
//!   once and reported);
//! - a file that was written is taken back by writing its old text over it —
//!   the same guarded rewrite the other way round, so a file that has changed
//!   since the rename is not overwritten and the undo is refused, naming it.
//!
//! Everything is checked before anything is taken back. Ctrl+Z in one open
//! file still undoes that file's part alone, as it would any other edit.
//!
//! **When it goes wrong halfway.** A write can fail, or a file can change in
//! the moment between its check and its write. Then the run stops there, and
//! the status line names what was written, what was not and why, and what was
//! never tried. What was written stays written and "Undo rename" takes back
//! exactly that. A failure before anything reached the disk takes the open
//! buffers back too, so nothing is left half done.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent};
use nun_core::Edit;
use nun_lsp::types::request::{PrepareRenameRequest, Rename};
use nun_lsp::types::{
    DocumentChangeOperation, DocumentChanges, OneOf, PrepareRenameResponse, RenameParams,
    ResourceOp, TextEdit, WorkDoneProgressParams, WorkspaceEdit,
};
use nun_lsp::{Encoding, RequestId, Response};
use nun_ui::{HitState, SearchRow, SearchView};
use nun_workspace::{Job, Rewrite, Written};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ropey::Rope;

use super::panes::DocId;
use super::prompt::{Prompt, Purpose};
use super::{App, Focus, Outcome, SidebarView, Target};

/// How long a server has to work out a rename. Longer than the default: a
/// server finds every reference in the project first, and on a large one that
/// takes a while.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The panel's title.
const TITLE: &str = "RENAME";

/// The text buttons where the search's toggles go: do it, or don't.
const ACTIONS: [&str; 2] = ["Rename", "Cancel"];

/// What the pointer can land on in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// The panel's title row.
    Header,
    /// The header's button: put the preview away.
    Back,
    /// One of the two name rows, which only show the names.
    Field,
    /// The cell at the end of the new name that applies the rename.
    Apply,
    /// A text button on the row below the names, by index into `ACTIONS`.
    Action(usize),
    /// The tick on a file row, which leaves the file out or puts it back.
    Mark(usize),
    /// A row of the list.
    Row(usize),
    /// Below the rows.
    Empty,
}

/// Where a rename has got to.
#[derive(Debug, Default)]
enum Stage {
    /// Nothing is happening.
    #[default]
    Idle,
    /// The server has been asked what would be renamed.
    Preparing {
        id: RequestId,
        doc: DocId,
        /// The caret, which is what is being renamed.
        at: usize,
    },
    /// The prompt is up.
    Naming { doc: DocId, version: i32, at: usize, old: String },
    /// The server has been asked for the rename.
    Asking { id: RequestId, doc: DocId, old: String, new: String },
    /// Files that are not open are being read, to preview them.
    Reading {
        tag: u64,
        old: String,
        new: String,
        /// What is ready already: the open files.
        files: Vec<FilePlan>,
        /// What the worker is reading, and the server's edits to each.
        disk: Vec<(PathBuf, Vec<TextEdit>)>,
        encoding: Encoding,
    },
    /// The rename has been applied to the open files, and the rest are being
    /// written.
    Applying { tag: u64, applied: Applied, planned: Vec<Rewrite> },
    /// A rename is being taken back, and its files are being written.
    Undoing { tag: u64, applied: Applied },
}

/// The rename, and everything the preview and the undo need of it.
#[derive(Debug, Default)]
pub(super) struct Renaming {
    stage: Stage,
    /// What is on screen, while it is.
    preview: Option<Preview>,
    /// The last rename applied, until it is taken back or another replaces
    /// it.
    last: Option<Applied>,
    /// The message that offered to take the last rename back. The status
    /// line's Undo button means "undo the rename" only while it is still the
    /// message showing; anything else that has happened since has its own.
    offer: Option<String>,
    /// Numbers the worker's jobs, so an answer finds its question.
    tags: u64,
}

impl Renaming {
    /// Whether `id` is a request of the rename's.
    pub(super) fn asked(&self, id: RequestId) -> bool {
        match self.stage {
            Stage::Preparing { id: asked, .. } | Stage::Asking { id: asked, .. } => asked == id,
            _ => false,
        }
    }

    const fn busy(&self) -> bool {
        matches!(self.stage, Stage::Reading { .. } | Stage::Applying { .. } | Stage::Undoing { .. })
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
    /// Whether it is to be renamed in.
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
}

/// The panel.
#[derive(Debug)]
struct Preview {
    old: String,
    new: String,
    files: Vec<FilePlan>,
    collapsed: BTreeSet<usize>,
    rows: Vec<Line>,
    widest: u32,
    scroll: usize,
    selected: Option<usize>,
    summary: String,
}

impl Preview {
    fn new(old: String, new: String, mut files: Vec<FilePlan>) -> Self {
        files.sort_by(|a, b| a.label.cmp(&b.label));
        let widest = files
            .iter()
            .flat_map(|file| file.changes.iter().map(|change| change.line.max(change.after_line)))
            .max()
            .unwrap_or(1);
        let mut preview = Self {
            old,
            new,
            files,
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
        let mut rows = Vec::new();
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
            })
            .collect()
    }

    fn file_of(&self, row: usize) -> Option<usize> {
        match *self.rows.get(row)? {
            Line::File(at) | Line::Before(at, _) | Line::After(at, _) => Some(at),
        }
    }
}

/// A rename as it was applied: enough to take it back.
#[derive(Debug, Default)]
struct Applied {
    old: String,
    new: String,
    /// The open files it edited.
    buffers: Vec<Undoable>,
    /// The files it wrote, each as the rewrite that would take it back.
    disk: Vec<Rewrite>,
}

/// One open file a rename edited.
#[derive(Debug)]
struct Undoable {
    doc: DocId,
    label: String,
    before: Rope,
    after: Rope,
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
    /// Rename the symbol at the caret: ask the server what it is, then ask
    /// for its new name.
    pub(super) fn start_rename(&mut self) -> Outcome {
        if self.rename.busy() {
            self.message = Some("Still working on the last rename…".into());
            return Outcome::Redraw;
        }
        if self.sidebar.is_none() {
            self.message = Some("Open a folder to rename across it.".into());
            return Outcome::Redraw;
        }
        // A question still waiting is forgotten: this is a new one.
        if let Stage::Preparing { id, .. } | Stage::Asking { id, .. } =
            std::mem::take(&mut self.rename.stage)
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
        let id = self.doc().id;
        let at = self.doc().buffer.selections().primary().head;
        let Some(lsp) = self.lsp.as_mut() else {
            self.message = Some("Cannot rename: no language server is running.".into());
            return Outcome::Redraw;
        };
        let Some(capabilities) = lsp.capabilities(id) else {
            self.message = Some("Cannot rename: this file's language server is not ready.".into());
            return Outcome::Redraw;
        };
        let prepare = match &capabilities.rename_provider {
            None | Some(OneOf::Left(false)) => {
                self.message = Some("This file's language server does not rename.".into());
                return Outcome::Redraw;
            }
            Some(OneOf::Left(true)) => false,
            Some(OneOf::Right(options)) => options.prepare_provider == Some(true),
        };
        if !prepare {
            return self.default_name(id, at);
        }
        let rope = self.doc().buffer.rope();
        let Some(params) = self.lsp.as_ref().and_then(|lsp| lsp.position_params(id, rope, at))
        else {
            self.message = Some("Cannot rename: this file's language server is not ready.".into());
            return Outcome::Redraw;
        };
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        match lsp.request::<PrepareRenameRequest>(id, params) {
            Ok(request) => {
                self.rename.stage = Stage::Preparing { id: request, doc: id, at };
                self.message = Some("Asking the language server what to rename…".into());
            }
            Err(error) => self.message = Some(format!("Cannot rename: {error}.")),
        }
        Outcome::Redraw
    }

    /// A server answered one of the rename's questions.
    pub(super) fn rename_answer(&mut self, response: &Response) -> Outcome {
        match std::mem::take(&mut self.rename.stage) {
            Stage::Preparing { id, doc, at } if id == response.id => {
                self.prepared(doc, at, response)
            }
            Stage::Asking { id, doc, old, new } if id == response.id => {
                self.renamed(doc, old, new, response)
            }
            other => {
                self.rename.stage = other;
                Outcome::Continue
            }
        }
    }

    /// What the server said about renaming at the caret.
    fn prepared(&mut self, doc: DocId, at: usize, response: &Response) -> Outcome {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if current != Some(response.version) {
            self.message = Some("The file changed while the server was asked. Try again.".into());
            return Outcome::Redraw;
        }
        let answer = match response.parse::<PrepareRenameRequest>() {
            Ok(answer) => answer,
            Err(error) => {
                self.message = Some(format!("Cannot rename this: {error}."));
                return Outcome::Redraw;
            }
        };
        let (range, placeholder) = match answer {
            None | Some(PrepareRenameResponse::DefaultBehavior { default_behavior: false }) => {
                self.message = Some("Nothing here can be renamed.".into());
                return Outcome::Redraw;
            }
            Some(PrepareRenameResponse::DefaultBehavior { default_behavior: true }) => {
                return self.default_name(doc, at);
            }
            Some(PrepareRenameResponse::Range(range)) => (range, None),
            Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder }) => {
                (range, Some(placeholder))
            }
        };
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        let Some(encoding) = self.lsp.as_ref().and_then(|lsp| lsp.encoding(doc)) else {
            return Outcome::Continue;
        };
        let range = encoding.char_range(document.buffer.rope(), range);
        let old = document.buffer.rope().slice(range.clone()).to_string();
        let placeholder = placeholder.unwrap_or_else(|| old.clone());
        self.ask_name(doc, at, range.start, placeholder)
    }

    /// The server left it to the editor: the word under the caret.
    fn default_name(&mut self, doc: DocId, at: usize) -> Outcome {
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        let (from, to) = document.buffer.word_range(at.min(document.buffer.len_chars()));
        let word = document.buffer.rope().slice(from..to).to_string();
        if word.trim().is_empty() {
            self.message = Some("Put the caret on a name to rename it.".into());
            return Outcome::Redraw;
        }
        self.ask_name(doc, at, from, word)
    }

    /// Put the prompt up at the symbol.
    fn ask_name(&mut self, doc: DocId, at: usize, start: usize, old: String) -> Outcome {
        let Some(version) = self.lsp.as_ref().and_then(|lsp| lsp.version(doc)) else {
            return Outcome::Continue;
        };
        // Only the file being edited has a place on screen to put it; had the
        // person switched tabs meanwhile, the status line is where it goes.
        let anchor = (self.doc().id == doc).then_some(start);
        self.prompt =
            Some(Prompt::name(Purpose::RenameSymbol, "Rename to".into(), &old).at(anchor));
        self.message = None;
        self.rename.stage = Stage::Naming { doc, version, at, old };
        Outcome::Redraw
    }

    /// The prompt was answered: ask for the rename.
    pub(super) fn rename_named(&mut self, confirmed: bool, name: &str) -> Outcome {
        let Stage::Naming { doc, version, at, old } = std::mem::take(&mut self.rename.stage) else {
            return Outcome::Redraw;
        };
        if !confirmed {
            return Outcome::Redraw;
        }
        if name.is_empty() || name == old {
            self.message = Some("The name is the same, so there is nothing to rename.".into());
            return Outcome::Redraw;
        }
        let Some(document) = self.doc_by(doc) else {
            self.message = Some("That file is no longer open.".into());
            return Outcome::Redraw;
        };
        let rope = document.buffer.rope();
        if self.lsp.as_ref().and_then(|lsp| lsp.version(doc)) != Some(version) {
            self.message = Some("The file changed while the name was typed. Try again.".into());
            return Outcome::Redraw;
        }
        let Some(position) = self.lsp.as_ref().and_then(|lsp| lsp.position_params(doc, rope, at))
        else {
            self.message = Some("Cannot rename: the language server is not ready.".into());
            return Outcome::Redraw;
        };
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        let params = RenameParams {
            text_document_position: position,
            new_name: name.to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        match lsp.request_within::<Rename>(doc, params, TIMEOUT) {
            Ok(id) => {
                self.message = Some(format!("Finding every {old} to rename…"));
                self.rename.stage = Stage::Asking { id, doc, old, new: name.to_string() };
            }
            Err(error) => self.message = Some(format!("Cannot rename: {error}.")),
        }
        Outcome::Redraw
    }

    /// The server's rename came back: check it, and preview it.
    fn renamed(&mut self, doc: DocId, old: String, new: String, response: &Response) -> Outcome {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if current != Some(response.version) {
            self.message =
                Some("The file changed while the rename was worked out. Try again.".into());
            return Outcome::Redraw;
        }
        let edit = match response.parse::<Rename>() {
            Ok(Some(edit)) => edit,
            Ok(None) => {
                self.message = Some(format!("The language server found no {old} to rename."));
                return Outcome::Redraw;
            }
            Err(error) => {
                self.message = Some(format!("Cannot rename {old}: {error}."));
                return Outcome::Redraw;
            }
        };
        match self.plan(doc, &edit) {
            Ok((files, disk, _)) if disk.is_empty() => self.preview(old, new, files),
            Ok((files, disk, encoding)) => {
                let tag = self.rename.tag();
                let paths = disk.iter().map(|(path, _)| path.clone()).collect();
                self.send_job(Job::Read { tag, paths });
                self.message = Some(format!("Reading the files {old} is renamed in…"));
                self.rename.stage = Stage::Reading { tag, old, new, files, disk, encoding };
                Outcome::Redraw
            }
            Err(why) => {
                self.message = Some(format!("Did not rename {old}: {why}."));
                Outcome::Redraw
            }
        }
    }

    /// Check a server's edit, and work out what it does to every open file.
    ///
    /// Returns those, the files that are not open with the edits to each, and
    /// the encoding every position in it is in.
    #[allow(clippy::type_complexity)] // Three things, named in the comment.
    fn plan(
        &self,
        doc: DocId,
        edit: &WorkspaceEdit,
    ) -> Result<(Vec<FilePlan>, Vec<(PathBuf, Vec<TextEdit>)>, Encoding), String> {
        let lsp = self.lsp.as_ref().ok_or("the language server stopped")?;
        // Every position the server sent is in its own encoding: the one it
        // agreed for the file it was asked about, whichever file it edits.
        let encoding = lsp.encoding(doc).ok_or("the language server stopped")?;
        let mut open = Vec::new();
        let mut disk = Vec::new();
        for file in flatten(edit)? {
            let label = self.label(&file.path);
            let Some(document) = self.docs.iter().find(|document| {
                document.buffer.path() == Some(file.path.as_path())
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
            let changes = changes(&before, &edits, &after);
            open.push(FilePlan {
                path: file.path,
                label,
                included: true,
                target: Where::Open { doc: document.id, before, edits, after },
                changes,
            });
        }
        Ok((open, disk, encoding))
    }

    /// The files that are not open have been read: preview everything.
    pub(super) fn rename_read(
        &mut self,
        tag: u64,
        read: Vec<(PathBuf, Result<String, String>)>,
    ) -> Outcome {
        let Stage::Reading { tag: asked, old, new, mut files, disk, encoding } =
            std::mem::take(&mut self.rename.stage)
        else {
            return Outcome::Continue;
        };
        if asked != tag {
            self.rename.stage = Stage::Reading { tag: asked, old, new, files, disk, encoding };
            return Outcome::Continue;
        }
        for ((path, text), (_, edits)) in read.into_iter().zip(disk) {
            let label = self.label(&path);
            let planned = text.and_then(|text| {
                let (after, changes) = disk_plan(&text, encoding, &edits)?;
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
                    self.message = Some(format!("Did not rename {old}: {label}: {why}."));
                    return Outcome::Redraw;
                }
            }
        }
        self.preview(old, new, files)
    }

    /// Show what the rename would do.
    fn preview(&mut self, old: String, new: String, files: Vec<FilePlan>) -> Outcome {
        let files: Vec<FilePlan> =
            files.into_iter().filter(|file| !file.changes.is_empty()).collect();
        if files.is_empty() {
            self.message = Some(format!("Renaming {old} to {new} changes nothing."));
            return Outcome::Redraw;
        }
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        sidebar.visible = true;
        self.rename.preview = Some(Preview::new(old, new, files));
        self.sidebar_view = SidebarView::Rename;
        self.focus = Focus::Rename;
        self.message = None;
        self.relayout();
        Outcome::Redraw
    }

    /// Put the preview away without renaming anything.
    pub(super) fn cancel_rename(&mut self) -> Outcome {
        self.rename.preview = None;
        self.close_rename_panel();
        self.message = Some("Nothing was renamed.".into());
        Outcome::Redraw
    }

    fn close_rename_panel(&mut self) {
        if self.sidebar_view == SidebarView::Rename {
            self.sidebar_view = SidebarView::Files;
        }
        if self.focus == Focus::Rename {
            self.focus = Focus::Editor;
        }
        self.relayout();
    }

    /// Whether the sidebar is showing the preview.
    pub(super) fn rename_area(&self) -> Option<Rect> {
        (self.sidebar_view == SidebarView::Rename && self.rename.preview.is_some())
            .then(|| self.tree_area())
            .flatten()
    }

    // ── applying ─────────────────────────────────────────────────────────────

    /// Rename in every file that is ticked.
    pub(super) fn apply_rename(&mut self) -> Outcome {
        let Some(preview) = self.rename.preview.take() else { return Outcome::Continue };
        if !preview.files.iter().any(|file| file.included) {
            self.rename.preview = Some(preview);
            self.message = Some("Every file is left out, so there is nothing to rename.".into());
            return Outcome::Redraw;
        }
        // Nothing is touched unless every open file still holds what was
        // previewed.
        for file in preview.files.iter().filter(|file| file.included) {
            if let Where::Open { doc, before, .. } = &file.target
                && self.doc_by(*doc).is_none_or(|document| document.buffer.rope() != before)
            {
                let label = file.label.clone();
                self.rename.preview = Some(preview);
                self.message = Some(format!(
                    "{label} has changed since the preview. Rename again to see what it would do now."
                ));
                return Outcome::Redraw;
            }
        }

        let Preview { old, new, files, .. } = preview;
        self.close_rename_panel();
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
                        let failed = self.take_back_buffers(&buffers);
                        self.message = Some(format!(
                            "Did not rename {old}: the edits to {} did not come out as \
                             previewed, so nothing was changed.{failed}",
                            file.label
                        ));
                        return Outcome::Redraw;
                    }
                    buffers.push(Undoable { doc, label: file.label, before, after });
                }
                Where::Disk { before, after } => {
                    planned.push(Rewrite { path: file.path, expect: before, text: after });
                }
            }
        }
        self.follow_caret();

        let applied = Applied { old, new, buffers, disk: Vec::new() };
        if planned.is_empty() {
            return self.renamed_everywhere(applied, &[]);
        }
        let tag = self.rename.tag();
        self.send_job(Job::Rewrite { tag, files: planned.clone() });
        self.message = Some(format!("Renaming {} to {}…", applied.old, applied.new));
        self.rename.stage = Stage::Applying { tag, applied, planned };
        Outcome::Redraw
    }

    /// The worker wrote, or took back, what it was asked to.
    pub(super) fn rename_rewritten(&mut self, tag: u64, files: &[(PathBuf, Written)]) -> Outcome {
        match std::mem::take(&mut self.rename.stage) {
            Stage::Applying { tag: asked, applied, planned } if asked == tag => {
                self.written(applied, &planned, files)
            }
            Stage::Undoing { tag: asked, applied } if asked == tag => {
                self.written_back(applied, files)
            }
            other => {
                self.rename.stage = other;
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
            let failed = self.take_back_buffers(&applied.buffers);
            self.message = Some(format!(
                "Did not rename {}: {}. Nothing was changed.{failed}",
                applied.old,
                self.why_not(files)
            ));
            return Outcome::Redraw;
        }

        applied.disk = done
            .iter()
            .map(|rewrite| Rewrite {
                path: rewrite.path.clone(),
                expect: rewrite.text.clone(),
                text: rewrite.expect.clone(),
            })
            .collect();
        let paths: Vec<PathBuf> = done.iter().map(|rewrite| rewrite.path.clone()).collect();
        self.renamed_everywhere(applied, &paths);
        if done.len() < planned.len() {
            let written: Vec<String> = paths.iter().map(|path| self.label(path)).collect();
            let message = format!(
                "Renamed only partly: wrote {}, then stopped: {}. Undo rename takes back \
                 what was written.",
                written.join(", "),
                self.why_not(files)
            );
            self.rename.offer = Some(message.clone());
            self.message = Some(message);
        }
        Outcome::Redraw
    }

    /// The rename is in: say so, and offer to take it back.
    fn renamed_everywhere(&mut self, applied: Applied, written: &[PathBuf]) -> Outcome {
        let untaken = self.reload_written(written);
        let files = applied.buffers.len() + applied.disk.len();
        let mut message = format!(
            "Renamed {} to {} in {files} {}.",
            applied.old,
            applied.new,
            plural(files, "file")
        );
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
        self.rename.last = Some(applied);
        self.rename.offer = Some(message.clone());
        self.message = Some(message);
        self.undo_offer = true;
        self.last_undone = false;
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

    /// Undo the rename step of each of these buffers, checking each undo
    /// took back exactly the rename. A sentence naming any that did not, to
    /// add to whatever is being said, or nothing.
    fn take_back_buffers(&mut self, buffers: &[Undoable]) -> String {
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
                // What was on top was not the rename after all. Put it back.
                document.buffer.redo();
                kept.push(undoable.label.clone());
            }
        }
        self.follow_caret();
        if kept.is_empty() {
            String::new()
        } else {
            format!(" {} still has the new name; undo it there.", kept.join(", "))
        }
    }

    // ── undoing ──────────────────────────────────────────────────────────────

    /// Whether the status line's Undo is offering to take a rename back.
    pub(super) fn rename_offered(&self) -> bool {
        self.undo_offer
            && self.rename.offer.is_some()
            && self.message.as_deref() == self.rename.offer.as_deref()
    }

    /// Take the last rename back, in every file it touched.
    pub(super) fn undo_rename(&mut self) -> Outcome {
        self.undo_offer = false;
        if self.rename.busy() {
            self.message = Some("Still working on the last rename…".into());
            return Outcome::Redraw;
        }
        let Some(applied) = self.rename.last.take() else {
            self.message = Some("There is no rename to take back.".into());
            return Outcome::Redraw;
        };
        // Everything is checked before anything is taken back.
        for undoable in &applied.buffers {
            if self.doc_by(undoable.doc).is_none_or(|d| d.buffer.rope() != &undoable.after) {
                self.message = Some(format!(
                    "Did not take the rename back: {} has been edited since. Undo that first, \
                     or undo the rename there.",
                    undoable.label
                ));
                self.rename.last = Some(applied);
                return Outcome::Redraw;
            }
        }
        if applied.disk.is_empty() {
            return self.taken_back(&applied, &[]);
        }
        let tag = self.rename.tag();
        self.send_job(Job::Rewrite { tag, files: applied.disk.clone() });
        self.message = Some(format!("Taking back the rename of {}…", applied.old));
        self.rename.stage = Stage::Undoing { tag, applied };
        Outcome::Redraw
    }

    /// The written files have been put back, as far as that got.
    fn written_back(&mut self, mut applied: Applied, files: &[(PathBuf, Written)]) -> Outcome {
        let restored: Vec<PathBuf> = files
            .iter()
            .filter(|(_, written)| *written == Written::Written)
            .map(|(path, _)| path.clone())
            .collect();
        if restored.len() == applied.disk.len() {
            return self.taken_back(&applied, &restored);
        }
        let _ = self.reload_written(&restored);
        let why = self.why_not(files);
        if restored.is_empty() {
            self.message =
                Some(format!("Did not take the rename back: {why}. Nothing was changed."));
        } else {
            let names: Vec<String> = restored.iter().map(|path| self.label(path)).collect();
            self.message = Some(format!(
                "Took the rename back only partly: restored {}, then stopped: {why}. Undo \
                 rename again tries the rest.",
                names.join(", ")
            ));
        }
        // What was put back is done with; the rest can be tried again.
        applied.disk.retain(|rewrite| !restored.contains(&rewrite.path));
        self.rename.last = Some(applied);
        Outcome::Redraw
    }

    /// Every file has its old text back on disk: undo the buffers too.
    fn taken_back(&mut self, applied: &Applied, restored: &[PathBuf]) -> Outcome {
        let kept = self.take_back_buffers(&applied.buffers);
        let _ = self.reload_written(restored);
        let files = applied.buffers.len() + restored.len();
        self.message = Some(format!(
            "Took back the rename of {} to {} in {files} {}.{kept}",
            applied.old,
            applied.new,
            plural(files, "file")
        ));
        self.rename.offer = None;
        Outcome::Redraw
    }

    // ── the panel ────────────────────────────────────────────────────────────

    /// Lay the panel's hit regions out, from the geometry it draws with.
    pub(super) fn layout_rename(&self, hits: &mut nun_input::HitMap<Target>) {
        let (Some(area), Some(preview)) = (self.rename_area(), self.rename.preview.as_ref()) else {
            return;
        };
        let spot = |spot| Target::Rename(spot);
        hits.push(super::cells(SearchView::header_area(area)), spot(Spot::Header), false);
        if let Some(cell) = SearchView::back_area(area) {
            hits.push(super::cells(cell), spot(Spot::Back), true);
        }
        hits.push(super::cells(SearchView::query_area(area)), spot(Spot::Field), false);
        hits.push(super::cells(SearchView::replace_area(area)), spot(Spot::Field), false);
        if let Some(cell) = SearchView::apply_area(area) {
            hits.push(super::cells(cell), spot(Spot::Apply), true);
        }
        for index in 0..ACTIONS.len() {
            if let Some(cell) = SearchView::action_area(area, &ACTIONS, index) {
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
    pub(super) fn render_rename(&self, cells: &mut Cells) {
        let (Some(area), Some(preview)) = (self.rename_area(), self.rename.preview.as_ref()) else {
            return;
        };
        let hovered = match self.hover.current() {
            Some(Target::Rename(spot)) => Some(spot),
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
        let summary = match hovered {
            Some(Spot::Back) => "Put the preview away and rename nothing",
            Some(Spot::Apply | Spot::Action(0)) => "Rename in every ticked file",
            Some(Spot::Action(_)) => "Rename nothing",
            Some(Spot::Mark(_)) => "Leave this file out of the rename, or put it back",
            _ => preview.summary.as_str(),
        };
        let rows = preview.view_rows(first..last);
        SearchView::new(&preview.old, &rows, &self.palette)
            .title(TITLE)
            .actions(&ACTIONS)
            .hovered_action(action)
            .replacement(&preview.new)
            .scrolled_to(0)
            .widest_line(preview.widest)
            .selected(within(preview.selected))
            .hovered(within(row))
            .hovered_back(hovered == Some(Spot::Back))
            .hovered_apply(hovered == Some(Spot::Apply))
            .focused(self.focus == Focus::Rename)
            .summary(Some(summary))
            .render(area, cells);
    }

    /// A click in the panel.
    pub(super) fn rename_press(&mut self, spot: Spot) -> Outcome {
        self.focus = Focus::Rename;
        match spot {
            Spot::Back | Spot::Action(1..) => self.cancel_rename(),
            Spot::Apply | Spot::Action(0) => self.apply_rename(),
            Spot::Mark(row) => self.toggle_rename_file(row),
            Spot::Row(row) => {
                if let Some(preview) = self.rename.preview.as_mut() {
                    preview.selected = Some(row);
                }
                self.pick_rename_row(row)
            }
            Spot::Header | Spot::Field | Spot::Empty => Outcome::Redraw,
        }
    }

    /// A key while the panel has the keyboard: the arrows move, Space leaves
    /// the selected file out or puts it back, Enter renames, Esc does not.
    pub(super) fn rename_key(&mut self, key: &KeyEvent) -> Outcome {
        let page = self.rename_area().map_or(1, SearchView::visible_rows).max(1);
        match key.code {
            KeyCode::Esc => self.cancel_rename(),
            KeyCode::Enter => self.apply_rename(),
            KeyCode::Char(' ') => match self.rename.preview.as_ref().and_then(|p| p.selected) {
                Some(row) => self.toggle_rename_file(row),
                None => Outcome::Redraw,
            },
            KeyCode::Up => self.move_rename_selection(-1),
            KeyCode::Down => self.move_rename_selection(1),
            KeyCode::PageUp => self.move_rename_selection(-isize::try_from(page).unwrap_or(1)),
            KeyCode::PageDown => self.move_rename_selection(isize::try_from(page).unwrap_or(1)),
            _ => Outcome::Continue,
        }
    }

    fn move_rename_selection(&mut self, delta: isize) -> Outcome {
        let visible = self.rename_area().map_or(0, SearchView::visible_rows);
        let Some(preview) = self.rename.preview.as_mut() else { return Outcome::Continue };
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
    pub(super) fn rename_scroll(&mut self, down: bool) -> Outcome {
        let visible = self.rename_area().map_or(0, SearchView::visible_rows);
        let Some(preview) = self.rename.preview.as_mut() else { return Outcome::Continue };
        let last = preview.rows.len().saturating_sub(visible);
        preview.scroll =
            if down { (preview.scroll + 3).min(last) } else { preview.scroll.saturating_sub(3) };
        Outcome::Redraw
    }

    /// Leave the file a row belongs to out of the rename, or put it back.
    fn toggle_rename_file(&mut self, row: usize) -> Outcome {
        let Some(preview) = self.rename.preview.as_mut() else { return Outcome::Continue };
        let Some(at) = preview.file_of(row) else { return Outcome::Continue };
        preview.files[at].included = !preview.files[at].included;
        preview.relist();
        Outcome::Redraw
    }

    /// A file row folds; a line opens its file there.
    fn pick_rename_row(&mut self, row: usize) -> Outcome {
        let Some(preview) = self.rename.preview.as_mut() else { return Outcome::Continue };
        let Some(line) = preview.rows.get(row).copied() else { return Outcome::Continue };
        match line {
            Line::File(at) => {
                if !preview.collapsed.remove(&at) {
                    preview.collapsed.insert(at);
                }
                preview.relist();
                Outcome::Redraw
            }
            Line::Before(at, index) | Line::After(at, index) => {
                let path = preview.files[at].path.clone();
                let line = preview.files[at].changes[index].line;
                self.open_file(&path);
                if self.doc().buffer.path() == Some(path.as_path()) {
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
}

/// `word`, or `words`.
fn plural(count: usize, word: &str) -> String {
    if count == 1 { word.to_string() } else { format!("{word}s") }
}

/// A server's edit as one list of edits per file, in either of the two forms
/// it can come in, or the reason it cannot be done as a whole.
fn flatten(edit: &WorkspaceEdit) -> Result<Vec<FileEdits>, String> {
    let mut files: Vec<FileEdits> = Vec::new();
    let mut add = |uri: &nun_lsp::types::Uri,
                   version: Option<i32>,
                   edits: Vec<TextEdit>|
     -> Result<(), String> {
        let path = nun_lsp::uri::to_path(uri)
            .ok_or_else(|| format!("the server edits {}, which is not a file", uri.as_str()))?;
        if files.iter().any(|file| file.path == path) {
            // Two lists for one file are applied one after the other, the
            // second in the coordinates the first leaves. Nothing sends that
            // for a rename, and getting it subtly wrong would be worse than
            // refusing it plainly.
            return Err(format!("the server edits {} twice over", path.display()));
        }
        if !edits.is_empty() {
            files.push(FileEdits { path, version, edits });
        }
        Ok(())
    };
    // Where both are given the protocol prefers the versioned form.
    if let Some(changes) = &edit.document_changes {
        let edits: Vec<&nun_lsp::types::TextDocumentEdit> = match changes {
            DocumentChanges::Edits(edits) => edits.iter().collect(),
            DocumentChanges::Operations(operations) => {
                let mut edits = Vec::new();
                for operation in operations {
                    match operation {
                        DocumentChangeOperation::Edit(edit) => edits.push(edit),
                        DocumentChangeOperation::Op(op) => return Err(refused(op)),
                    }
                }
                edits
            }
        };
        for edit in edits {
            let text_edits = edit
                .edits
                .iter()
                .map(|edit| match edit {
                    OneOf::Left(edit) => edit.clone(),
                    OneOf::Right(annotated) => annotated.text_edit.clone(),
                })
                .collect();
            add(&edit.text_document.uri, edit.text_document.version, text_edits)?;
        }
    } else if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            add(uri, None, edits.clone())?;
        }
    }
    Ok(files)
}

/// Why a file operation in a rename is refused, naming it.
fn refused(op: &ResourceOp) -> String {
    let (what, uri) = match op {
        ResourceOp::Create(create) => ("create", &create.uri),
        ResourceOp::Rename(rename) => ("rename", &rename.old_uri),
        ResourceOp::Delete(delete) => ("delete", &delete.uri),
    };
    let name = nun_lsp::uri::to_path(uri)
        .map_or_else(|| uri.as_str().to_string(), |path| path.display().to_string());
    format!(
        "the server would also {what} {name}, and nun does not create, move or delete files as \
         part of a rename"
    )
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

/// What stands for a line break inside a row that shows more than one line.
const BREAK: &str = "↵";

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
/// [`BREAK`] when there is more than one — which only happens when an edit
/// takes a line break away or puts one in. Where an edit lands in `after` is
/// its start moved by what every edit before it added or took away, which is
/// exact because `after` is [`splice`] of the same list.
fn changes(before: &Rope, edits: &[Edit], after: &Rope) -> Vec<Change> {
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
            let (text, offsets) = shown(before, run.from, run.to);
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
                after: shown(after, run.after_from, run.after_to).0,
            }
        })
        .collect()
}

/// Lines `from` to `to` of `text` as one row, and for each line its first
/// char in `text`, where it starts in the row, and how many chars it has
/// there — for [`place`].
fn shown(text: &Rope, from: usize, to: usize) -> (String, Vec<(usize, usize, usize)>) {
    let mut row = String::new();
    let mut lines = Vec::new();
    let mut at = 0;
    for line in from..=to {
        if line > from {
            row.push_str(BREAK);
            at += 1;
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
    let changes = changes(&before, &edits, &Rope::from_str(&after));
    Ok((format!("{bom}{after}"), changes))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::str::FromStr;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use nun_core::{Buffer, Range as Caret, Selections};
    use nun_lsp::types::{Position, Range as Span, TextDocumentEdit, Uri};
    use nun_lsp::{Lsp, ServerSpec};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use nun_workspace::Done;
    use proptest::prelude::*;

    use super::*;
    use crate::commands::Command;

    /// A language server in `sh`, answering `initialize` with the
    /// capabilities in `$1`, `textDocument/prepareRename` with `$2` and
    /// `textDocument/rename` with `$3`, as they are given.
    const SERVER: &str = r#"
caps="$1"; prepare="$2"; rename="$3"
while :; do
  len=
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
  done
  [ -z "$len" ] && exit 0
  body=$(dd bs=1 count="$len" 2>/dev/null)
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$body" in
    *'"method":"initialize"'*) result="{\"capabilities\":$caps}" ;;
    *'"method":"textDocument/prepareRename"'*) result="$prepare" ;;
    *'"method":"textDocument/rename"'*) result="$rename" ;;
    *'"method":"shutdown"'*) result=null ;;
    *'"method":"exit"'*) exit 0 ;;
    *) continue ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"
done
"#;

    /// Renames with a prepare step, and edits by URI.
    const PREPARES: &str = r#"{"textDocumentSync":1,"renameProvider":{"prepareProvider":true}}"#;

    /// Renames, and leaves finding the name to the editor.
    const PLAIN: &str = r#"{"textDocumentSync":1,"renameProvider":true}"#;

    /// An editor on a project, with the server above answering with the
    /// given JSON, the file tree's worker attached, and both pumped the way
    /// the event loop pumps them.
    struct Tester {
        app: App,
        lsp: Receiver<nun_lsp::Event>,
        done: Receiver<Done>,
        dir: tempfile::TempDir,
    }

    impl Tester {
        /// `rename` may name files as `{a.rs}`, which becomes that file's
        /// URI — the resolved one, as a server would say it.
        fn new(files: &[(&str, &str)], caps: &str, prepare: &str, rename: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, text) in files {
                fs::write(dir.path().join(name), text).unwrap();
            }
            let resolved = fs::canonicalize(dir.path()).unwrap();
            let mut rename = rename.to_string();
            for (name, _) in files {
                let uri = nun_lsp::uri::from_path(&resolved.join(name)).unwrap();
                rename = rename.replace(&format!("{{{name}}}"), uri.as_str());
            }

            let (buffer, _) = Buffer::load(dir.path().join(files[0].0)).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                crate::commands::defaults(crate::commands::KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 120, 30));
            let (sender, done) = channel();
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                false,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            let spec = ServerSpec {
                command: "sh".into(),
                args: vec![
                    "-c".into(),
                    SERVER.into(),
                    "server".into(),
                    caps.into(),
                    prepare.into(),
                    rename,
                ],
                optional: false,
            };
            let (sender, lsp) = channel();
            let handle = Lsp::start(
                BTreeMap::from([("rust".to_string(), spec)]),
                None,
                Box::new(move |event| {
                    let _ = sender.send(event);
                }),
            )
            .unwrap();
            app.attach_lsp(handle);
            let mut tester = Self { app, lsp, done, dir };
            tester.until(|app| app.lsp.as_ref().and_then(|lsp| lsp.capabilities(0)).is_some());
            tester
        }

        /// Hand the editor whatever the server and the worker say, and tick
        /// it, until `done`.
        fn until(&mut self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done(&self.app) {
                assert!(Instant::now() < deadline, "gave up; it says {:?}", self.app.message());
                if let Ok(event) = self.lsp.recv_timeout(Duration::from_millis(5)) {
                    self.app.handle(Event::Lsp(event));
                }
                while let Ok(message) = self.done.try_recv() {
                    self.app.handle(Event::Workspace(message));
                }
                self.app.tick(Instant::now());
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.path(name)).unwrap()
        }

        fn caret(&mut self, at: usize) {
            self.app.doc_mut().buffer.set_selections(Selections::single(Caret::caret(at)));
        }

        fn key(&mut self, code: KeyCode) {
            self.app.handle(Event::Key(KeyEvent::from(code)));
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

        /// Click wherever `target` is laid out.
        fn click_on(&mut self, target: Target) {
            let area = self.app.viewport;
            let at = (area.top()..area.bottom())
                .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                .find(|&(x, y)| self.app.hits.at(x, y).map(|hit| hit.target) == Some(target))
                .unwrap_or_else(|| panic!("{target:?} is not on screen"));
            self.click(at.0, at.1);
        }

        /// Ask to rename what is at char `at`, and wait for the prompt.
        fn ask(&mut self, at: usize) {
            self.caret(at);
            self.app.run(Command::RenameSymbol);
            self.until(|app| app.prompt.is_some() || !app.rename.asked_anything());
        }

        /// Type `name` over what the prompt holds, and send it.
        fn name(&mut self, name: &str) {
            let typed = self.app.prompt.as_ref().and_then(|p| p.field.clone()).unwrap();
            for _ in typed.chars() {
                self.key(KeyCode::Backspace);
            }
            for ch in name.chars() {
                self.key(KeyCode::Char(ch));
            }
            self.key(KeyCode::Enter);
            self.until(|app| !app.rename.asked_anything());
        }

        /// The preview as it reads.
        fn rows(&self) -> Vec<String> {
            let preview = self.app.rename.preview.as_ref().expect("a preview is up");
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
                })
                .collect()
        }

        fn settled(&mut self) {
            self.until(|app| !app.rename.busy());
        }
    }

    impl Renaming {
        /// Whether a question to the server or the worker is still out.
        fn asked_anything(&self) -> bool {
            matches!(
                self.stage,
                Stage::Preparing { .. } | Stage::Asking { .. } | Stage::Reading { .. }
            )
        }
    }

    const MAIN: &str = "fn cat() {}\nfn main() { cat(); }\n";
    const OTHER: &str = "use crate::cat;\r\nfn f() { cat() }\r\n";

    fn edit(line: u32, from: u32, to: u32, text: &str) -> String {
        format!(
            r#"{{"range":{{"start":{{"line":{line},"character":{from}}},"end":{{"line":{line},"character":{to}}}}},"newText":"{text}"}}"#
        )
    }

    /// `cat` to `dog` in both files, as `changes`.
    fn by_uri(name: &str) -> String {
        format!(
            r#"{{"changes":{{"{{main.rs}}":[{},{}],"{{other.rs}}":[{},{}]}}}}"#,
            edit(0, 3, 6, name),
            edit(1, 12, 15, name),
            edit(0, 11, 14, name),
            edit(1, 9, 12, name),
        )
    }

    const PLACEHOLDER: &str = r#"{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":6}},"placeholder":"cat"}"#;

    #[test]
    fn a_rename_previews_every_file_applies_as_one_and_undoes_as_one() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        let prompt = t.app.prompt.as_ref().expect("the prompt is up");
        assert_eq!(prompt.field.as_deref(), Some("cat"), "filled with the placeholder");
        let status = t.app.areas().1;
        let beside = t.app.prompt_area(status);
        assert_ne!(beside, status, "drawn at the symbol, not in the status line");
        assert_eq!(beside.y, t.app.areas().0.y + 1, "on the row under it");
        let mut cells = Cells::empty(t.app.viewport);
        t.app.render(t.app.viewport, &mut cells);
        let drawn: String =
            (beside.x..beside.right()).map(|x| cells[(x, beside.y)].symbol()).collect();
        assert!(drawn.starts_with(" Rename to cat"), "{drawn:?}");
        assert!(drawn.trim_end().ends_with(" Rename   Cancel"), "{drawn:?}");
        // Its buttons are where they are drawn.
        t.app.relayout();
        let buttons = t.app.prompt.as_ref().unwrap().button_areas(beside);
        assert_eq!(
            t.app.hits.at(buttons[1].x + 1, buttons[1].y).map(|hit| hit.target),
            Some(Target::PromptButton(1))
        );

        t.name("dog");
        assert_eq!(
            t.rows(),
            [
                "[main.rs]",
                "1- fn cat() {}",
                "1+ fn dog() {}",
                "2- fn main() { cat(); }",
                "2+ fn main() { dog(); }",
                "[other.rs]",
                "1- use crate::cat;",
                "1+ use crate::dog;",
                "2- fn f() { cat() }",
                "2+ fn f() { dog() }",
            ],
        );
        assert_eq!(t.app.sidebar_view, SidebarView::Rename);
        assert_eq!(t.read("other.rs"), OTHER, "nothing is written before it is applied");

        // The panel's own button.
        t.click_on(Target::Rename(Spot::Action(0)));
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n", "CRLF kept");
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\nfn main() { dog(); }\n");
        assert_eq!(t.read("main.rs"), MAIN, "an open file is edited, not saved");
        let message = t.app.message().unwrap().to_string();
        assert!(message.starts_with("Renamed cat to dog in 2 files."), "{message}");
        assert_eq!(t.app.sidebar_view, SidebarView::Files, "the preview is put away");

        // The status line offers the way back, and it takes back everything.
        assert!(t.app.rename_offered());
        t.click_on(Target::StatusUndo);
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
        assert!(!t.app.buffer().is_modified(), "back where it was");
        assert!(t.app.message().unwrap().starts_with("Took back the rename of cat to dog"));
    }

    #[test]
    fn a_server_without_a_prepare_step_renames_the_word_at_the_caret() {
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PLAIN, "null", &by_uri("dog"));
        t.ask(5);
        assert_eq!(t.app.prompt.as_ref().and_then(|p| p.field.as_deref()), Some("cat"));
        t.name("dog");
        assert_eq!(t.rows().len(), 10);
    }

    #[test]
    fn a_server_that_says_use_the_default_gets_the_word_at_the_caret() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            r#"{"defaultBehavior":true}"#,
            &by_uri("dog"),
        );
        t.ask(4);
        assert_eq!(t.app.prompt.as_ref().and_then(|p| p.field.as_deref()), Some("cat"));
    }

    #[test]
    fn a_server_that_says_nothing_is_there_puts_no_prompt_up() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            "null",
            &by_uri("dog"),
        );
        t.ask(9);
        assert!(t.app.prompt.is_none());
        assert_eq!(t.app.message(), Some("Nothing here can be renamed."));
    }

    #[test]
    fn a_versioned_edit_to_text_that_has_moved_on_is_refused_whole() {
        let rename = format!(
            r#"{{"documentChanges":[{{"textDocument":{{"uri":"{{other.rs}}","version":null}},"edits":[{}]}},{{"textDocument":{{"uri":"{{main.rs}}","version":41}},"edits":[{}]}}]}}"#,
            edit(0, 11, 14, "dog"),
            edit(0, 3, 6, "dog"),
        );
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        assert!(t.app.rename.preview.is_none(), "nothing to preview");
        let message = t.app.message().unwrap();
        assert!(
            message.contains("main.rs has changed since the server worked it out"),
            "{message}"
        );
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn an_edit_that_would_create_a_file_is_refused_whole() {
        let rename = format!(
            r#"{{"documentChanges":[{{"textDocument":{{"uri":"{{main.rs}}","version":null}},"edits":[{}]}},{{"kind":"create","uri":"{{main.rs}}.new"}}]}}"#,
            edit(0, 3, 6, "dog"),
        );
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        let message = t.app.message().unwrap();
        assert!(message.contains("would also create"), "{message}");
        assert!(t.app.rename.preview.is_none());
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn a_file_changed_on_disk_after_the_preview_stops_everything() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        fs::write(t.path("other.rs"), "use crate::cat; // edited\r\n").unwrap();
        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::cat; // edited\r\n");
        assert_eq!(t.app.buffer().text().to_string(), MAIN, "the open file went back too");
        let message = t.app.message().unwrap();
        assert!(message.contains("other.rs has changed on disk since"), "{message}");
        assert!(message.contains("Nothing was changed"), "{message}");
    }

    #[test]
    fn a_file_left_out_is_left_alone() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        // The tick on other.rs's row.
        let row = t.rows().iter().position(|row| row == "[other.rs]").unwrap();
        t.click_on(Target::Rename(Spot::Mark(row)));
        assert!(t.rows().contains(&"[out: other.rs]".to_string()), "{:?}", t.rows());
        assert!(!t.rows().iter().any(|row| row.contains("crate::dog")), "no after rows for it");
        t.click_on(Target::Rename(Spot::Apply));
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\nfn main() { dog(); }\n");
    }

    #[test]
    fn cancelling_renames_nothing() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.click_on(Target::Rename(Spot::Action(1)));
        assert!(t.app.rename.preview.is_none());
        assert_eq!(t.app.sidebar_view, SidebarView::Files);
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn undo_is_refused_when_a_written_file_has_changed_since() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        fs::write(t.path("other.rs"), "use crate::dog; // mine\r\n").unwrap();
        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::dog; // mine\r\n", "not overwritten");
        let renamed = "fn dog() {}\nfn main() { dog(); }\n";
        assert_eq!(t.app.buffer().text().to_string(), renamed, "nor is the open file undone");
        let message = t.app.message().unwrap();
        assert!(message.contains("other.rs has changed on disk since"), "{message}");

        // Put it back as the rename left it, and the undo goes through.
        fs::write(t.path("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n").unwrap();
        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn undo_is_refused_when_an_open_file_has_been_edited_since() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        t.app.handle(Event::Paste("// more\n".into()));
        t.app.run(Command::UndoRename);
        t.settled();
        assert!(t.app.message().unwrap().contains("main.rs has been edited since"));
        assert_eq!(t.read("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n", "checked first");
    }

    #[cfg(unix)]
    #[test]
    fn a_write_that_fails_halfway_is_reported_exactly_and_undo_takes_back_what_was_written() {
        use std::os::unix::fs::PermissionsExt;
        let rename = format!(
            r#"{{"changes":{{"{{a.rs}}":[{}],"{{b.rs}}":[{}],"{{c.rs}}":[{}]}}}}"#,
            edit(0, 3, 6, "dog"),
            edit(0, 0, 3, "dog"),
            edit(0, 0, 3, "dog"),
        );
        let mut t = Tester::new(
            &[("a.rs", "fn cat() {}\n"), ("b.rs", "cat\n"), ("c.rs", "cat\n")],
            PREPARES,
            PLACEHOLDER,
            &rename,
        );
        fs::set_permissions(t.path("c.rs"), fs::Permissions::from_mode(0o444)).unwrap();
        if fs::OpenOptions::new().write(true).open(t.path("c.rs")).is_ok() {
            return; // Root writes anyway; there is no failure to see.
        }
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("b.rs"), "dog\n", "written");
        assert_eq!(t.read("c.rs"), "cat\n", "refused");
        let message = t.app.message().unwrap().to_string();
        assert!(
            message.starts_with("Renamed only partly: wrote b.rs, then stopped: "),
            "{message}"
        );
        assert!(message.contains("c.rs"), "{message}");
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\n", "the open file kept it");

        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("b.rs"), "cat\n");
        assert_eq!(t.app.buffer().text().to_string(), "fn cat() {}\n");
    }

    // ── the parts that need no server ──────────────────────────────────────

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
        let plain = flatten(&changes).unwrap();
        let with = flatten(&versioned).unwrap();
        assert_eq!(plain[0].path, PathBuf::from("/p/a.rs"));
        assert_eq!(plain[0].edits, with[0].edits);
        assert_eq!((plain[0].version, with[0].version), (None, Some(3)));
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
        assert!(flatten(&twice).unwrap_err().contains("twice"));
    }

    #[test]
    fn a_file_on_disk_keeps_its_mark_its_endings_and_its_last_line() {
        let text = "\u{feff}let cat = 1;\r\nlet cat = 2;\nno newline cat";
        let edits =
            [text_edit(0, 4, 7, "dog"), text_edit(1, 4, 7, "dog"), text_edit(2, 11, 14, "dog")];
        let (after, changes) = disk_plan(text, Encoding::Utf16, &edits).unwrap();
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
        let (after, changes) = disk_plan(text, Encoding::Utf16, &edits).unwrap();
        assert_eq!(after, "🦀 dog é dog\n");
        assert_eq!(changes.len(), 1, "one line, one row");
        assert_eq!(changes[0].matched, [2..5, 8..11], "char offsets");

        let edits = [text_edit(0, 5, 8, "dog")];
        let (after, _) = disk_plan(text, Encoding::Utf8, &edits).unwrap();
        assert_eq!(after, "🦀 dog é cat\n");
    }

    #[test]
    fn new_text_with_line_breaks_takes_the_files_own() {
        let (after, _) =
            disk_plan("a\r\nb\r\n", Encoding::Utf16, &[text_edit(0, 1, 1, "\nx")]).unwrap();
        assert_eq!(after, "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn a_bare_carriage_return_is_refused_rather_than_miscounted() {
        let refused = disk_plan("a\rb cat\n", Encoding::Utf16, &[text_edit(1, 2, 5, "dog")]);
        assert!(refused.unwrap_err().contains("bare carriage return"));
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let refused = disk_plan(
            "cat\n",
            Encoding::Utf16,
            &[text_edit(0, 0, 2, "x"), text_edit(0, 1, 3, "y")],
        );
        assert!(refused.unwrap_err().contains("overlap"));
    }

    #[test]
    fn a_change_that_moves_a_line_down_still_shows_the_right_after_line() {
        let before = Rope::from_str("a\nb cat\n");
        let edits = join(vec![Edit::replace(0, 1, "x\ny"), Edit::replace(4, 7, "dog")], 8).unwrap();
        let after = Rope::from_str(&splice(&before, &edits));
        assert_eq!(after.to_string(), "x\ny\nb dog\n");
        let rows = changes(&before, &edits, &after);
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
        let rows = changes(&before, &edits, &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].before, "let cat↵= cat;");
        assert_eq!(rows[0].matched, vec![4..8, 10..13], "the break counts as one");
        assert_eq!(rows[0].after, "let dog = dog;");
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
            disk_plan("cat\r\nx", Encoding::Utf16, &[text_edit(0, 0, 99, "dog")]).unwrap();
        assert_eq!(after, "dog\r\nx");
    }

    #[test]
    fn an_open_file_holding_a_bare_carriage_return_is_refused() {
        let mut t = Tester::new(
            &[("main.rs", "a\rcat\ncat\n"), ("other.rs", OTHER)],
            PREPARES,
            r#"{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},"placeholder":"cat"}"#,
            &format!(
                r#"{{"changes":{{"{{main.rs}}":[{},{}]}}}}"#,
                edit(1, 0, 3, "dog"),
                edit(2, 0, 3, "dog")
            ),
        );
        t.ask(2);
        t.name("dog");
        let message = t.app.message().unwrap();
        assert!(message.contains("bare carriage return"), "{message}");
        assert_eq!(t.app.buffer().text().to_string(), "a\rcat\ncat\n");
    }

    #[test]
    fn the_prompt_sits_under_the_symbol_where_it_is_drawn_not_where_its_chars_are() {
        let mut app = App::new(
            Buffer::from_text("\t中文 cat\n"),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 10));
        // The tab, two wide characters and a space: past four chars, but
        // drawn past the tab stop and four more columns.
        let tab = app.doc().buffer.tab_width();
        let at = 4;
        app.prompt =
            Some(Prompt::name(Purpose::RenameSymbol, "Rename to".into(), "cat").at(Some(at)));
        let status = app.areas().1;
        let area = app.prompt_area(status);
        let text = app.areas().0;
        assert_eq!(usize::from(area.x - text.x - app.gutter_width()), tab + 5);
        assert_eq!(area.y, text.y + 1);
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
            let Ok((after, rows)) = disk_plan(&text, encoding, &edits) else {
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
