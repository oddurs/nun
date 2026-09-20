//! Changes to the filesystem that can be taken back.
//!
//! Every operation here is recorded so it can be undone and redone, because the
//! sidebar makes them one gesture away: a drag that lands in the wrong
//! directory, or a delete clicked instead of a rename, has to cost one undo and
//! nothing more.
//!
//! Three rules follow from that.
//!
//! **Delete moves to a trash, never removes.** The trash is a directory nun owns
//! — not the platform trash, which has no portable API and differs on every
//! desktop. Its location is given to [`FsHistory::new`], so tests can use a
//! temporary directory; [`default_trash_dir`] gives the real one. Nothing is
//! ever emptied from it here.
//!
//! **Nothing is overwritten.** Every destination is checked first and an
//! existing entry is an error, not a casualty. The check and the rename are two
//! system calls, so another process can still slip a file in between them; the
//! atomic alternatives (`renameat2` on Linux, `renamex_np` on macOS) need
//! platform calls this crate cannot make without `unsafe`, and the window is a
//! few microseconds wide.
//!
//! **An undo that cannot be done stays on the stack.** If the original name has
//! been taken since, undo reports it and leaves the step where it was, so the
//! person can move the obstruction and try again.
//!
//! Moving across filesystems falls back to copy-then-remove, for directories as
//! well as files, since the trash is often on a different volume from the
//! project. Symbolic links are recreated as links rather than followed.
//!
//! [`FsHistory::replace`] is the one operation that changes what is *inside* a
//! file rather than where it is. It follows the same rules: a copy of every
//! file it writes goes into the trash first, and one undo puts all of them
//! back together, because a project-wide replace the person has to undo forty
//! times is not one they can take back.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::replace::{self, Outcome, Plan, Recorded, Replacer, Report, plural};

/// What an operation did, in terms of the paths a person would recognise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// An empty file was created.
    CreateFile(PathBuf),
    /// An empty directory was created.
    CreateDir(PathBuf),
    /// An entry was renamed within its directory.
    Rename {
        /// Where it was.
        from: PathBuf,
        /// Where it is now.
        to: PathBuf,
    },
    /// An entry was moved into another directory, keeping its name.
    Move {
        /// Where it was.
        from: PathBuf,
        /// Where it is now.
        to: PathBuf,
    },
    /// An entry was moved to the trash.
    Delete(PathBuf),
    /// Lines were rewritten across one or more files, as one step.
    Replace {
        /// The files that were written, in the order they were written.
        files: Vec<PathBuf>,
        /// How many lines changed across all of them.
        lines: usize,
    },
}

/// The outcome of doing, undoing or redoing an operation.
///
/// Carries what the tree needs to refresh and what the UI needs to say. Its
/// [`Display`](fmt::Display) is a sentence for a toast: `Moved a.rs to src/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The operation this was, or was the reverse of.
    pub operation: Operation,
    /// Whether this was the operation being undone rather than done or redone.
    pub undone: bool,
    /// Directories whose listing changed, to pass to `FileTree::refresh_dir`.
    /// Never includes the trash.
    pub dirs: Vec<PathBuf>,
    /// Where the affected entry now is, if it is anywhere visible, so the UI
    /// can reveal and select it.
    pub path: Option<PathBuf>,
}

/// Why an operation, an undo or a redo did not happen.
#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// The destination is taken, and nothing is ever overwritten.
    #[error("{} already exists", .0.display())]
    Exists(PathBuf),
    /// The entry to act on is not there.
    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),
    /// A new name was empty, `.` or `..`, or had a path separator in it.
    #[error("`{0}` is not a valid file name")]
    InvalidName(String),
    /// The entry already has the name or location asked for.
    #[error("{} is already there", .0.display())]
    Unchanged(PathBuf),
    /// A directory cannot be moved inside itself.
    #[error("cannot move {} into itself", .0.display())]
    IntoItself(PathBuf),
    /// A move target was not a directory.
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// The filesystem refused.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path being acted on.
        path: PathBuf,
        /// What the filesystem said.
        #[source]
        source: io::Error,
    },
    /// An undo or a redo of a replace put some of its files back and not the
    /// rest, so the caller is not left believing it all came back.
    #[error("put back {done} of {files} files; {}: {source}", path.display())]
    Partial {
        /// How many files are now on the side that was asked for.
        done: usize,
        /// How many the replace covered in all.
        files: usize,
        /// The first file that could not be written.
        path: PathBuf,
        /// What the filesystem said about it.
        #[source]
        source: io::Error,
    },
}

/// The undo and redo stacks for filesystem operations.
#[derive(Debug)]
pub struct FsHistory {
    trash: PathBuf,
    done: Vec<Record>,
    undone: Vec<Record>,
    slots: u64,
}

/// One step, plus where its entry sits in the trash while it is deleted.
#[derive(Debug)]
struct Record {
    operation: Operation,
    trashed: Option<PathBuf>,
    /// For a replace, one entry per file it wrote. Empty for everything else.
    blobs: Vec<Blob>,
}

/// One file a replace rewrote, beside the copy of its other version.
///
/// Undo and redo are the same move — swap the file with the copy — so one
/// blob serves both directions, and `replaced` says which way round it
/// currently is. That per-file flag is what makes an undo that fails halfway
/// safe to try again: a retry only touches the files that did not move.
#[derive(Debug)]
struct Blob {
    file: PathBuf,
    /// Where the version that is *not* on disk is kept, in the trash.
    kept: PathBuf,
    /// Whether the file currently holds what the replace wrote, rather than
    /// what was there before it.
    replaced: bool,
}

/// Where deleted entries go when nothing else is configured.
///
/// Under the platform's per-user state directory: `~/Library/Application
/// Support/nun/trash` on macOS, `$XDG_STATE_HOME/nun/trash` (falling back to
/// `~/.local/state/nun/trash`) on other Unix systems, and
/// `%LOCALAPPDATA%\nun\trash` on Windows. `None` when the environment does not
/// say where home is.
#[must_use]
pub fn default_trash_dir() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|p| p.is_absolute())?
    } else {
        match std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
            // The XDG spec says a relative value is invalid and must be ignored.
            Some(state) if state.is_absolute() => state,
            _ => home()?.join(".local/state"),
        }
    };
    Some(base.join("nun").join("trash"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())
}

impl FsHistory {
    /// An empty history that trashes deleted entries into `trash`.
    ///
    /// The directory is created the first time something is deleted.
    #[must_use]
    pub fn new(trash: impl Into<PathBuf>) -> Self {
        Self { trash: trash.into(), done: Vec::new(), undone: Vec::new(), slots: 0 }
    }

    /// The trash directory.
    #[must_use]
    pub fn trash(&self) -> &Path {
        &self.trash
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Create an empty file at `path`.
    ///
    /// # Errors
    ///
    /// [`OpError::Exists`] if anything is already at `path`, or
    /// [`OpError::Io`] if the file cannot be created, such as when its parent
    /// directory does not exist.
    pub fn create_file(&mut self, path: impl Into<PathBuf>) -> Result<Change, OpError> {
        let path = path.into();
        fs::File::create_new(&path).map_err(|error| io_error(&path, error))?;
        Ok(self.record(Operation::CreateFile(path)))
    }

    /// Create an empty directory at `path`. Its parent must exist.
    ///
    /// # Errors
    ///
    /// [`OpError::Exists`] if anything is already at `path`, or
    /// [`OpError::Io`] if the directory cannot be created.
    pub fn create_dir(&mut self, path: impl Into<PathBuf>) -> Result<Change, OpError> {
        let path = path.into();
        fs::create_dir(&path).map_err(|error| io_error(&path, error))?;
        Ok(self.record(Operation::CreateDir(path)))
    }

    /// Give the entry at `from` a new name in the same directory.
    ///
    /// A change of case alone is allowed on a case-insensitive filesystem,
    /// where the new name looks taken by the entry itself.
    ///
    /// # Errors
    ///
    /// [`OpError::InvalidName`] if `name` is empty, `.`, `..` or has a path
    /// separator in it; [`OpError::Unchanged`] if it is the current name;
    /// [`OpError::Missing`] if there is nothing at `from`;
    /// [`OpError::Exists`] if the name is taken; [`OpError::Io`] if the rename
    /// itself fails.
    pub fn rename(&mut self, from: impl Into<PathBuf>, name: &str) -> Result<Change, OpError> {
        let from = from.into();
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.chars().any(|c| std::path::is_separator(c) || c == '\0')
        {
            return Err(OpError::InvalidName(name.to_string()));
        }
        let to = from.with_file_name(name);
        if to == from {
            return Err(OpError::Unchanged(from));
        }
        relocate(&from, &to)?;
        Ok(self.record(Operation::Rename { from, to }))
    }

    /// Move the entry at `from` into the directory `dir`, keeping its name.
    ///
    /// # Errors
    ///
    /// [`OpError::Unchanged`] if it is already in `dir`;
    /// [`OpError::IntoItself`] if `dir` is `from` or inside it;
    /// [`OpError::NotADirectory`] if `dir` is not a directory;
    /// [`OpError::Missing`] if there is nothing at `from`;
    /// [`OpError::Exists`] if `dir` already has an entry of that name;
    /// [`OpError::Io`] if the move itself fails.
    pub fn move_into(
        &mut self,
        from: impl Into<PathBuf>,
        dir: impl AsRef<Path>,
    ) -> Result<Change, OpError> {
        let from = from.into();
        let dir = dir.as_ref();
        let Some(name) = from.file_name() else {
            return Err(OpError::InvalidName(from.display().to_string()));
        };
        let to = dir.join(name);
        if to == from {
            return Err(OpError::Unchanged(from));
        }
        if dir.starts_with(&from) {
            return Err(OpError::IntoItself(from));
        }
        if !dir.is_dir() {
            return Err(OpError::NotADirectory(dir.to_path_buf()));
        }
        relocate(&from, &to)?;
        Ok(self.record(Operation::Move { from, to }))
    }

    /// Move the entry at `path` into the trash.
    ///
    /// # Errors
    ///
    /// [`OpError::Missing`] if there is nothing at `path`, or
    /// [`OpError::Io`] if the trash cannot be created or the entry cannot be
    /// moved into it.
    pub fn delete(&mut self, path: impl Into<PathBuf>) -> Result<Change, OpError> {
        let path = path.into();
        let slot = self.trash_into(&path)?;
        self.done.push(Record {
            operation: Operation::Delete(path),
            trashed: Some(slot),
            blobs: Vec::new(),
        });
        self.undone.clear();
        Ok(change(&self.done[self.done.len() - 1].operation, false))
    }

    /// Rewrite the chosen lines of each file in `chosen`, as one step that
    /// undoes and redoes together.
    ///
    /// `chosen` names files relative to `root`, each with the lines the person
    /// kept, each as a [`Recorded`] built from the hit the panel was showing.
    /// Only those lines change; a line that matches but was not chosen is
    /// copied out as it was.
    ///
    /// A line is rewritten only if it is still the line that text came from,
    /// and a file is written only if every line chosen in it passes that.
    /// `searched_at` — when the search that found the hits *started* — is a
    /// cheap early-out in front of the same question.
    ///
    /// Nothing here returns an error: a file that cannot be read or written is
    /// its own entry in the [`Report`], because one unreachable file is not a
    /// reason to abandon the other forty. The [`Change`] is `None` when no
    /// file was written and so there is nothing to take back.
    pub fn replace(
        &mut self,
        root: &Path,
        replacer: &Replacer,
        chosen: &[(PathBuf, Vec<Recorded>)],
        searched_at: SystemTime,
    ) -> (Report, Option<Change>) {
        let mut report = Report::default();
        let mut blobs = Vec::new();
        let mut written = Vec::new();

        for (relative, lines) in replace::tidy(chosen) {
            let path = root.join(&relative);
            let outcome = match replace::plan(&path, replacer, &lines, searched_at) {
                Plan::Leave(outcome) => outcome,
                Plan::Write(text, changed) => match self.rewrite(&path, &text) {
                    Ok(kept) => {
                        blobs.push(Blob { file: path.clone(), kept, replaced: true });
                        written.push(path);
                        report.lines += changed;
                        Outcome::Changed(changed)
                    }
                    Err(error) => Outcome::Failed(error.to_string()),
                },
            };
            report.files.push((relative, outcome));
        }

        if written.is_empty() {
            return (report, None);
        }
        let operation = Operation::Replace { files: written, lines: report.lines };
        let change = change(&operation, false);
        self.done.push(Record { operation, trashed: None, blobs });
        self.undone.clear();
        (report, Some(change))
    }

    /// Reverse the most recent operation.
    ///
    /// Returns `Ok(None)` when there is nothing to undo.
    ///
    /// # Errors
    ///
    /// Whatever stopped the reversal, most often [`OpError::Exists`] because
    /// something has taken the original name since, or [`OpError::Partial`]
    /// when a replace put some of its files back and not the rest. The
    /// operation stays on the undo stack so it can be tried again, and a
    /// retried replace only touches the files that did not come back.
    pub fn undo(&mut self) -> Result<Option<Change>, OpError> {
        let Some(mut record) = self.done.pop() else { return Ok(None) };
        match self.reverse(&mut record) {
            Ok(()) => {
                let change = change(&record.operation, true);
                self.undone.push(record);
                Ok(Some(change))
            }
            Err(error) => {
                self.done.push(record);
                Err(error)
            }
        }
    }

    /// Do again the most recently undone operation.
    ///
    /// Returns `Ok(None)` when there is nothing to redo.
    ///
    /// # Errors
    ///
    /// Whatever stopped it, most often [`OpError::Exists`], or
    /// [`OpError::Partial`] when a replace re-applied to some of its files and
    /// not the rest. The operation stays on the redo stack.
    pub fn redo(&mut self) -> Result<Option<Change>, OpError> {
        let Some(mut record) = self.undone.pop() else { return Ok(None) };
        match self.replay(&mut record) {
            Ok(()) => {
                let change = change(&record.operation, false);
                self.done.push(record);
                Ok(Some(change))
            }
            Err(error) => {
                self.undone.push(record);
                Err(error)
            }
        }
    }

    fn record(&mut self, operation: Operation) -> Change {
        let change = change(&operation, false);
        self.done.push(Record { operation, trashed: None, blobs: Vec::new() });
        self.undone.clear();
        change
    }

    fn reverse(&mut self, record: &mut Record) -> Result<(), OpError> {
        match &record.operation {
            Operation::CreateFile(path) | Operation::CreateDir(path) => {
                // Trashed rather than removed: the file may have been written
                // to since it was created.
                record.trashed = Some(self.trash_into(path)?);
            }
            Operation::Rename { from, to } | Operation::Move { from, to } => relocate(to, from)?,
            Operation::Delete(path) => restore(record.trashed.as_deref(), path)?,
            Operation::Replace { .. } => swap(&mut record.blobs, true)?,
        }
        Ok(())
    }

    fn replay(&mut self, record: &mut Record) -> Result<(), OpError> {
        match &record.operation {
            Operation::CreateFile(path) | Operation::CreateDir(path) => {
                restore(record.trashed.as_deref(), path)?;
            }
            Operation::Rename { from, to } | Operation::Move { from, to } => relocate(from, to)?,
            Operation::Delete(path) => record.trashed = Some(self.trash_into(path)?),
            Operation::Replace { .. } => swap(&mut record.blobs, false)?,
        }
        Ok(())
    }

    /// Copy `path`'s contents into a fresh slot in the trash, leaving the
    /// original where it is.
    ///
    /// The deleted-entry slots are the natural place for this: they are
    /// already nun's, already never emptied here, and already named so that
    /// two files called `mod.rs` cannot meet.
    /// Keep a copy of `path` in the trash, then write `text` over it.
    ///
    /// The copy is taken first, so the previous contents are already safe if
    /// the write is what fails, or if the process dies between the two. The
    /// write is in place rather than to a temporary that is then renamed,
    /// which keeps the file's permissions, its inode and anything linked to
    /// it — and the copy is already the thing that makes a half-written file
    /// recoverable.
    fn rewrite(&mut self, path: &Path, text: &str) -> Result<PathBuf, OpError> {
        let kept = self.keep(path)?;
        match fs::write(path, text) {
            Ok(()) => Ok(kept),
            Err(error) => {
                drop_slot(&kept);
                Err(io_error(path, error))
            }
        }
    }

    /// Copy `path`'s contents into a fresh slot in the trash, leaving the
    /// original where it is.
    ///
    /// The deleted-entry slots are the natural place for this: they are
    /// already nun's, already never emptied here, and already named so that
    /// two files called `mod.rs` cannot meet.
    fn keep(&mut self, path: &Path) -> Result<PathBuf, OpError> {
        let target = self.slot_for(path)?;
        match fs::copy(path, &target) {
            Ok(_) => Ok(target),
            Err(error) => {
                drop_slot(&target);
                Err(io_error(path, error))
            }
        }
    }

    /// Move `path` into a fresh slot in the trash and return where it went.
    fn trash_into(&mut self, path: &Path) -> Result<PathBuf, OpError> {
        if fs::symlink_metadata(path).is_err() {
            return Err(OpError::Missing(path.to_path_buf()));
        }
        let target = self.slot_for(path)?;
        if let Err(error) = relocate(path, &target) {
            drop_slot(&target);
            return Err(error);
        }
        Ok(target)
    }

    /// Make a fresh slot in the trash and return the place inside it that
    /// `path`'s name belongs at.
    ///
    /// Each slot is its own directory, so two deleted files with the same name
    /// never meet, and the entry keeps its name inside it for anyone looking
    /// through the trash by hand.
    fn slot_for(&mut self, path: &Path) -> Result<PathBuf, OpError> {
        let Some(name) = path.file_name() else {
            return Err(OpError::InvalidName(path.display().to_string()));
        };
        self.slots += 1;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let slot = self.trash.join(format!("{stamp}-{}-{}", std::process::id(), self.slots));
        fs::create_dir_all(&slot).map_err(|error| io_error(&slot, error))?;
        Ok(slot.join(name))
    }
}

/// Take away the slot an entry was going to sit in, or sat in.
///
/// Only ever called when the slot is empty or about to be; failing to tidy it
/// is not worth failing over.
fn drop_slot(target: &Path) {
    if let Some(slot) = target.parent() {
        let _ = fs::remove_file(target);
        let _ = fs::remove_dir(slot);
    }
}

/// Exchange each file with the copy kept for it, for the files currently on
/// the `from` side.
///
/// Every file is attempted, so one that cannot be written does not strand the
/// rest, and each one's flag moves when that file itself moved — so pressing
/// undo again after a failure retries exactly the files that did not.
fn swap(blobs: &mut [Blob], from: bool) -> Result<(), OpError> {
    let mut failure = None;
    for blob in blobs.iter_mut().filter(|blob| blob.replaced == from) {
        let outcome = exchange(&blob.file, &blob.kept);
        // The flag says which side the *file* is on, and the write to the
        // file is what settles that. A copy that failed to catch up is worth
        // reporting, but it must not leave the flag saying the file never
        // moved: a retry would then swap it again, and since the copy still
        // holds what the file now holds, both sides would end up the same and
        // the other version would be gone for good.
        if outcome.moved {
            blob.replaced = !from;
        }
        if let Some((path, source)) = outcome.failed
            && failure.is_none()
        {
            failure = Some((path, source));
        }
    }
    let Some((path, source)) = failure else { return Ok(()) };
    let done = blobs.iter().filter(|blob| blob.replaced != from).count();
    Err(OpError::Partial { done, files: blobs.len(), path, source })
}

/// What one exchange did: whether the file itself moved to the other side,
/// and what went wrong if anything did.
struct Exchanged {
    moved: bool,
    failed: Option<(PathBuf, io::Error)>,
}

/// Put what is kept for `file` into it, and what was in it into the copy.
///
/// The version the file currently holds is written somewhere safe *before*
/// the file is overwritten, and only moved into place afterwards. Writing the
/// file first and the copy second would look tidier — the file is the side
/// that refuses, so a refusal there costs nothing — but it puts the only copy
/// of the current version in the one place about to be overwritten. If the
/// copy then could not be written, that version would be gone: not merely
/// unreachable by redo, gone, and the undo would report partial success over
/// the top of it.
///
/// So the order is: save, overwrite, commit. A failure at the first step
/// leaves everything untouched, a failure at the second leaves everything
/// untouched, and the third is a rename within one directory.
fn exchange(file: &Path, kept: &Path) -> Exchanged {
    let failed = |path: &Path, error: io::Error| Exchanged {
        moved: false,
        failed: Some((path.to_path_buf(), error)),
    };
    let now = match fs::read(file) {
        Ok(now) => now,
        Err(error) => return failed(file, error),
    };
    let before = match fs::read(kept) {
        Ok(before) => before,
        Err(error) => return failed(kept, error),
    };

    // Beside the copy, so the move at the end is within one directory.
    let holding = kept.with_extension("swapping");
    if let Err(error) = fs::write(&holding, &now) {
        return failed(&holding, error);
    }
    if let Err(error) = fs::write(file, &before) {
        let _ = fs::remove_file(&holding);
        return failed(file, error);
    }
    // Past here the file has moved, whatever becomes of the copy.
    Exchanged {
        moved: true,
        failed: fs::rename(&holding, kept).err().map(|error| (kept.to_path_buf(), error)),
    }
}

/// Bring an entry back out of its trash slot to `path`, then drop the slot.
fn restore(trashed: Option<&Path>, path: &Path) -> Result<(), OpError> {
    let Some(trashed) = trashed else { return Err(OpError::Missing(path.to_path_buf())) };
    relocate(trashed, path)?;
    if let Some(slot) = trashed.parent() {
        // Only ever empty now; failing to tidy it is not worth failing over.
        let _ = fs::remove_dir(slot);
    }
    Ok(())
}

/// Rename `from` to `to` without overwriting anything, across filesystems if
/// need be.
fn relocate(from: &Path, to: &Path) -> Result<(), OpError> {
    if fs::symlink_metadata(from).is_err() {
        return Err(OpError::Missing(from.to_path_buf()));
    }
    if fs::symlink_metadata(to).is_ok() && !same_entry(from, to) {
        return Err(OpError::Exists(to.to_path_buf()));
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => copy_then_remove(from, to),
        Err(error) => Err(io_error(from, error)),
    }
}

/// Whether two paths name the same entry, as `a.rs` and `A.rs` do on a
/// case-insensitive filesystem.
fn same_entry(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// The cross-filesystem fallback: copy everything, and only once that has
/// fully succeeded remove the original. A failed copy removes what it made, so
/// the original is never the thing that is lost.
fn copy_then_remove(from: &Path, to: &Path) -> Result<(), OpError> {
    if let Err(error) = copy_tree(from, to) {
        let _ = remove_tree(to);
        return Err(error);
    }
    remove_tree(from).map_err(|error| io_error(from, error))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(from).map_err(|error| io_error(from, error))?;
    if metadata.is_symlink() {
        let target = fs::read_link(from).map_err(|error| io_error(from, error))?;
        return symlink(&target, to).map_err(|error| io_error(to, error));
    }
    if metadata.is_dir() {
        fs::create_dir(to).map_err(|error| io_error(to, error))?;
        for entry in fs::read_dir(from).map_err(|error| io_error(from, error))? {
            let entry = entry.map_err(|error| io_error(from, error))?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return fs::set_permissions(to, metadata.permissions())
            .map_err(|error| io_error(to, error));
    }
    fs::copy(from, to).map(drop).map_err(|error| io_error(from, error))
}

fn remove_tree(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("cannot recreate the link {} on another volume", link.display()),
    ))
}

fn io_error(path: &Path, error: io::Error) -> OpError {
    match error.kind() {
        io::ErrorKind::AlreadyExists => OpError::Exists(path.to_path_buf()),
        _ => OpError::Io { path: path.to_path_buf(), source: error },
    }
}

fn parent(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

fn change(operation: &Operation, undone: bool) -> Change {
    let (dirs, path) = match operation {
        Operation::CreateFile(path) | Operation::CreateDir(path) | Operation::Delete(path) => {
            let gone = undone != matches!(operation, Operation::Delete(_));
            (vec![parent(path)], (!gone).then(|| path.clone()))
        }
        Operation::Rename { from, to } | Operation::Move { from, to } => {
            let mut dirs = vec![parent(from)];
            if parent(to) != dirs[0] {
                dirs.push(parent(to));
            }
            (dirs, Some(if undone { from.clone() } else { to.clone() }))
        }
        Operation::Replace { files, .. } => {
            // No entry appears or disappears, but a listing carries what the
            // tree knows about a file, so every directory a rewritten file
            // sits in is worth refreshing. Each one only once, however many of
            // its files were written.
            let mut dirs: Vec<PathBuf> = Vec::new();
            for dir in files.iter().map(|file| parent(file)) {
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            (dirs, files.first().cloned())
        }
    };
    Change { operation: operation.clone(), undone, dirs, path }
}

fn name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into())
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.operation, self.undone) {
            (Operation::CreateFile(path), false) => write!(f, "Created {}", name(path)),
            (Operation::CreateDir(path), false) => write!(f, "Created {}/", name(path)),
            (Operation::CreateFile(path), true) => write!(f, "Removed {}", name(path)),
            (Operation::CreateDir(path), true) => write!(f, "Removed {}/", name(path)),
            (Operation::Rename { from, to }, false) => {
                write!(f, "Renamed {} to {}", name(from), name(to))
            }
            (Operation::Rename { from, to }, true) => {
                write!(f, "Renamed {} back to {}", name(to), name(from))
            }
            (Operation::Move { to, .. }, false) => {
                write!(f, "Moved {} to {}/", name(to), name(&parent(to)))
            }
            (Operation::Move { from, .. }, true) => {
                write!(f, "Moved {} back to {}/", name(from), name(&parent(from)))
            }
            (Operation::Delete(path), false) => write!(f, "Deleted {}", name(path)),
            (Operation::Delete(path), true) => write!(f, "Restored {}", name(path)),
            (Operation::Replace { files, lines }, false) => {
                write!(f, "Replaced {lines} {} in ", plural(*lines, "line"))?;
                match files.as_slice() {
                    [one] => f.write_str(&name(one)),
                    many => write!(f, "{} files", many.len()),
                }
            }
            (Operation::Replace { files, .. }, true) => match files.as_slice() {
                [one] => write!(f, "Restored {}", name(one)),
                many => write!(f, "Restored {} files", many.len()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replace::Skipped;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct Fixture {
        project: TempDir,
        trash: TempDir,
        history: FsHistory,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            let project = tempfile::tempdir().unwrap();
            let trash = tempfile::tempdir().unwrap();
            for (path, content) in files {
                let path = project.path().join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            let history = FsHistory::new(trash.path().join("trash"));
            Self { project, trash, history }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.project.path().join(relative)
        }

        fn read(&self, relative: &str) -> String {
            fs::read_to_string(self.path(relative)).unwrap()
        }

        /// Every entry under the project with its content, `/` for directories.
        fn snapshot(&self) -> BTreeMap<String, String> {
            snapshot(self.project.path())
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, into: &mut BTreeMap<String, String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let key = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if path.is_dir() {
                    into.insert(key, "/".into());
                    walk(root, &path, into);
                } else {
                    into.insert(key, fs::read_to_string(&path).unwrap());
                }
            }
        }
        let mut map = BTreeMap::new();
        walk(root, root, &mut map);
        map
    }

    #[test]
    fn creating_a_file_undoes_to_the_trash_and_redoes_back() {
        let mut fx = Fixture::new(&[]);
        let path = fx.path("new.rs");
        let change = fx.history.create_file(&path).unwrap();
        assert_eq!(change.to_string(), "Created new.rs");
        assert_eq!(change.dirs, [fx.project.path().to_path_buf()]);
        assert_eq!(change.path.as_deref(), Some(path.as_path()));

        fs::write(&path, "typed since").unwrap();
        let undone = fx.history.undo().unwrap().unwrap();
        assert_eq!(undone.to_string(), "Removed new.rs");
        assert_eq!(undone.path, None);
        assert!(!path.exists());

        fx.history.redo().unwrap().unwrap();
        assert_eq!(fx.read("new.rs"), "typed since", "what was written is not lost");
    }

    #[test]
    fn creating_a_directory_undoes_and_redoes() {
        let mut fx = Fixture::new(&[]);
        let path = fx.path("src");
        assert_eq!(fx.history.create_dir(&path).unwrap().to_string(), "Created src/");
        assert!(path.is_dir());
        fx.history.undo().unwrap();
        assert!(!path.exists());
        fx.history.redo().unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn renaming_undoes_and_redoes_keeping_content() {
        let mut fx = Fixture::new(&[("a.rs", "alpha")]);
        let change = fx.history.rename(fx.path("a.rs"), "b.rs").unwrap();
        assert_eq!(change.to_string(), "Renamed a.rs to b.rs");
        assert_eq!(fx.read("b.rs"), "alpha");
        assert!(!fx.path("a.rs").exists());

        assert_eq!(fx.history.undo().unwrap().unwrap().to_string(), "Renamed b.rs back to a.rs");
        assert_eq!(fx.read("a.rs"), "alpha");
        assert!(!fx.path("b.rs").exists());

        fx.history.redo().unwrap();
        assert_eq!(fx.read("b.rs"), "alpha");
    }

    #[test]
    fn a_change_of_case_alone_is_a_valid_rename() {
        let mut fx = Fixture::new(&[("readme.md", "text")]);
        fx.history.rename(fx.path("readme.md"), "README.md").unwrap();
        let names: Vec<_> = fs::read_dir(fx.project.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["README.md"]);
        fx.history.undo().unwrap();
        let names: Vec<_> = fs::read_dir(fx.project.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["readme.md"]);
    }

    #[test]
    fn rename_rejects_names_that_are_not_names() {
        let mut fx = Fixture::new(&[("a.rs", "")]);
        for bad in ["", ".", "..", "sub/b.rs", "nul\0"] {
            assert!(
                matches!(fx.history.rename(fx.path("a.rs"), bad), Err(OpError::InvalidName(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(fx.history.rename(fx.path("a.rs"), "a.rs"), Err(OpError::Unchanged(_))));
        assert!(matches!(fx.history.rename(fx.path("x.rs"), "y.rs"), Err(OpError::Missing(_))));
        assert!(!fx.history.can_undo());
    }

    #[test]
    fn moving_undoes_and_redoes() {
        let mut fx = Fixture::new(&[("a.rs", "alpha"), ("src/lib.rs", "")]);
        let change = fx.history.move_into(fx.path("a.rs"), fx.path("src")).unwrap();
        assert_eq!(change.to_string(), "Moved a.rs to src/");
        assert_eq!(change.dirs, [fx.project.path().to_path_buf(), fx.path("src")]);
        assert_eq!(change.path, Some(fx.path("src/a.rs")));
        assert_eq!(fx.read("src/a.rs"), "alpha");

        let undone = fx.history.undo().unwrap().unwrap();
        assert_eq!(undone.path, Some(fx.path("a.rs")));
        assert_eq!(fx.read("a.rs"), "alpha");
        assert!(!fx.path("src/a.rs").exists());

        fx.history.redo().unwrap();
        assert_eq!(fx.read("src/a.rs"), "alpha");
    }

    #[test]
    fn a_directory_cannot_move_into_itself() {
        let mut fx = Fixture::new(&[("src/inner/x.rs", "")]);
        let into_self = fx.history.move_into(fx.path("src"), fx.path("src/inner"));
        assert!(matches!(into_self, Err(OpError::IntoItself(_))));
        let onto_self = fx.history.move_into(fx.path("src"), fx.path("src"));
        assert!(matches!(onto_self, Err(OpError::IntoItself(_))));
        let already = fx.history.move_into(fx.path("src/inner"), fx.path("src"));
        assert!(matches!(already, Err(OpError::Unchanged(_))));
        let file = fx.history.move_into(fx.path("src"), fx.path("src/inner/x.rs"));
        assert!(matches!(file, Err(OpError::IntoItself(_) | OpError::NotADirectory(_))));
    }

    #[test]
    fn delete_goes_to_the_trash_and_undo_restores_content() {
        let mut fx = Fixture::new(&[("src/a.rs", "alpha"), ("src/deep/b.rs", "beta")]);
        let before = fx.snapshot();
        let change = fx.history.delete(fx.path("src")).unwrap();
        assert_eq!(change.to_string(), "Deleted src");
        assert!(!fx.path("src").exists());
        assert!(fx.trash.path().join("trash").is_dir());

        assert_eq!(fx.history.undo().unwrap().unwrap().to_string(), "Restored src");
        assert_eq!(fx.snapshot(), before);
        assert_eq!(
            fs::read_dir(fx.history.trash()).unwrap().count(),
            0,
            "the empty slot is tidied away"
        );

        fx.history.redo().unwrap();
        assert!(!fx.path("src").exists());
        fx.history.undo().unwrap();
        assert_eq!(fx.snapshot(), before);
    }

    #[test]
    fn two_deleted_files_with_one_name_do_not_collide_in_the_trash() {
        let mut fx = Fixture::new(&[("a/x.rs", "from a"), ("b/x.rs", "from b")]);
        fx.history.delete(fx.path("a/x.rs")).unwrap();
        fx.history.delete(fx.path("b/x.rs")).unwrap();
        fx.history.undo().unwrap();
        fx.history.undo().unwrap();
        assert_eq!(fx.read("a/x.rs"), "from a");
        assert_eq!(fx.read("b/x.rs"), "from b");
    }

    #[test]
    fn nothing_is_ever_overwritten() {
        let mut fx = Fixture::new(&[("a.rs", "alpha"), ("b.rs", "beta"), ("src/a.rs", "inner")]);
        assert!(matches!(fx.history.create_file(fx.path("a.rs")), Err(OpError::Exists(_))));
        assert!(matches!(fx.history.create_dir(fx.path("a.rs")), Err(OpError::Exists(_))));
        assert!(matches!(fx.history.rename(fx.path("a.rs"), "b.rs"), Err(OpError::Exists(_))));
        assert!(matches!(
            fx.history.move_into(fx.path("a.rs"), fx.path("src")),
            Err(OpError::Exists(_))
        ));
        assert_eq!(fx.read("a.rs"), "alpha");
        assert_eq!(fx.read("b.rs"), "beta");
        assert_eq!(fx.read("src/a.rs"), "inner");
        assert!(!fx.history.can_undo(), "refused operations are not recorded");
    }

    #[test]
    fn an_undo_that_would_overwrite_fails_and_stays_undoable() {
        let mut fx = Fixture::new(&[("a.rs", "original")]);
        fx.history.rename(fx.path("a.rs"), "b.rs").unwrap();
        fs::write(fx.path("a.rs"), "newcomer").unwrap();

        assert!(matches!(fx.history.undo(), Err(OpError::Exists(_))));
        assert_eq!(fx.read("a.rs"), "newcomer");
        assert!(fx.history.can_undo());

        fs::remove_file(fx.path("a.rs")).unwrap();
        fx.history.undo().unwrap().unwrap();
        assert_eq!(fx.read("a.rs"), "original");
    }

    #[test]
    fn a_new_operation_discards_the_redo_branch() {
        let mut fx = Fixture::new(&[]);
        fx.history.create_file(fx.path("a.rs")).unwrap();
        fx.history.undo().unwrap();
        assert!(fx.history.can_redo());
        fx.history.create_file(fx.path("b.rs")).unwrap();
        assert!(!fx.history.can_redo());
        assert!(fx.history.redo().unwrap().is_none());
    }

    #[test]
    fn empty_stacks_report_nothing_to_do() {
        let mut fx = Fixture::new(&[]);
        assert!(fx.history.undo().unwrap().is_none());
        assert!(fx.history.redo().unwrap().is_none());
    }

    #[test]
    fn non_ascii_names_round_trip_through_every_operation() {
        let mut fx = Fixture::new(&[("日本語.txt", "こんにちは"), ("données/.keep", "")]);
        let before = fx.snapshot();
        fx.history.rename(fx.path("日本語.txt"), "émoji-🦀.txt").unwrap();
        fx.history.move_into(fx.path("émoji-🦀.txt"), fx.path("données")).unwrap();
        let change = fx.history.delete(fx.path("données/émoji-🦀.txt")).unwrap();
        assert_eq!(change.to_string(), "Deleted émoji-🦀.txt");
        fx.history.create_file(fx.path("👨‍👩‍👧.rs")).unwrap();
        while fx.history.undo().unwrap().is_some() {}
        assert_eq!(fx.snapshot(), before);
    }

    #[cfg(unix)]
    #[test]
    fn the_cross_filesystem_fallback_copies_trees_and_links() {
        let fx = Fixture::new(&[("src/a.rs", "alpha"), ("src/deep/b.rs", "beta")]);
        std::os::unix::fs::symlink("a.rs", fx.path("src/link")).unwrap();
        let before = snapshot(&fx.path("src"));
        copy_then_remove(&fx.path("src"), &fx.path("moved")).unwrap();
        assert!(!fx.path("src").exists());
        assert_eq!(snapshot(&fx.path("moved")), before);
        assert_eq!(fs::read_link(fx.path("moved/link")).unwrap(), Path::new("a.rs"));
    }

    #[test]
    fn a_failed_cross_filesystem_copy_leaves_the_original() {
        let fx = Fixture::new(&[("a.rs", "alpha")]);
        let result = copy_then_remove(&fx.path("a.rs"), &fx.path("missing-dir/a.rs"));
        assert!(result.is_err());
        assert_eq!(fx.read("a.rs"), "alpha");
    }

    #[test]
    fn the_default_trash_is_under_a_nun_directory() {
        if let Some(trash) = default_trash_dir() {
            assert!(trash.is_absolute());
            assert!(trash.ends_with("nun/trash"));
        }
    }

    /// The chosen lines of a file as they read on disk right now — what a
    /// search that had just run would have recorded for them.
    ///
    /// Taken at call time, so a test that then edits the file is recording
    /// what the person previewed rather than what replaced it.
    fn recorded(fx: &Fixture, path: &str, lines: &[u32]) -> (PathBuf, Vec<Recorded>) {
        let text = fs::read_to_string(fx.path(path)).unwrap_or_default();
        let split: Vec<&str> = text.lines().collect();
        let lines = lines
            .iter()
            .map(|&line| {
                let content = split.get(line as usize - 1).copied().unwrap_or_default();
                Recorded::Whole { line, text: content.to_owned() }
            })
            .collect();
        (PathBuf::from(path), lines)
    }

    /// A replace against the fixture's project, recording each chosen line as
    /// it stands now.
    fn replacing(
        fx: &mut Fixture,
        options: &crate::Options,
        replacement: &str,
        chosen: &[(&str, &[u32])],
    ) -> (Report, Option<Change>) {
        let chosen: Vec<_> = chosen.iter().map(|(path, lines)| recorded(fx, path, lines)).collect();
        applying(fx, options, replacement, &chosen)
    }

    /// The same, for a test that has built the recorded text itself.
    fn applying(
        fx: &mut Fixture,
        options: &crate::Options,
        replacement: &str,
        chosen: &[(PathBuf, Vec<Recorded>)],
    ) -> (Report, Option<Change>) {
        let replacer = Replacer::new(options, replacement).unwrap();
        let root = fx.project.path().to_path_buf();
        // Far enough ahead that nothing written by the fixture itself looks
        // newer than the search that is being pretended to have run, so these
        // tests exercise the line check rather than the timestamp.
        let searched_at = SystemTime::now() + std::time::Duration::from_secs(60);
        fx.history.replace(&root, &replacer, chosen, searched_at)
    }

    fn query(text: &str) -> crate::Options {
        crate::Options {
            query: text.into(),
            case: crate::Case::Sensitive,
            ..crate::Options::default()
        }
    }

    #[test]
    fn only_the_chosen_lines_of_a_file_change() {
        let mut fx = Fixture::new(&[("a.rs", "cat\ncat\ncat\ncat\n")]);
        let (report, change) = replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[2, 4])]);

        assert_eq!(fx.read("a.rs"), "cat\ndog\ncat\ndog\n", "lines 1 and 3 were not chosen");
        assert_eq!(report.lines, 2);
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Changed(2))]);
        assert_eq!(change.unwrap().to_string(), "Replaced 2 lines in a.rs");
    }

    #[test]
    fn every_match_on_a_chosen_line_changes_not_just_the_first() {
        let mut fx = Fixture::new(&[("a.rs", "cat cat cat\ncat\n")]);
        replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1])]);
        assert_eq!(fx.read("a.rs"), "dog dog dog\ncat\n");
    }

    #[test]
    fn capture_groups_reach_the_file_they_were_previewed_against() {
        let mut fx = Fixture::new(&[("a.rs", "let one = 1;\nlet two = 2;\n")]);
        let options = crate::Options { regex: true, ..query(r"let (\w+) = (\d+);") };
        let replacer = Replacer::new(&options, "const $1: u8 = $2;").unwrap();

        let previewed = replacer.line("let one = 1;");
        replacing(&mut fx, &options, "const $1: u8 = $2;", &[("a.rs", &[1, 2])]);

        assert_eq!(previewed, "const one: u8 = 1;", "what the panel showed");
        assert_eq!(fx.read("a.rs"), "const one: u8 = 1;\nconst two: u8 = 2;\n");
    }

    #[test]
    fn a_replacement_that_lengthens_or_empties_a_line_writes_it_as_it_is() {
        let mut fx = Fixture::new(&[("a.rs", "x\nremove me\ny\n")]);
        replacing(&mut fx, &query("x"), "xxxx", &[("a.rs", &[1])]);
        replacing(&mut fx, &query("remove me"), "", &[("a.rs", &[2])]);
        assert_eq!(fx.read("a.rs"), "xxxx\n\ny\n");
    }

    #[test]
    fn crlf_endings_and_a_missing_final_newline_survive() {
        let mut fx = Fixture::new(&[
            ("dos.rs", "cat\r\ncat\r\n"),
            ("mixed.rs", "cat\r\ncat\ncat"),
            ("bare.rs", "cat"),
        ]);
        let chosen: &[(&str, &[u32])] =
            &[("dos.rs", &[1, 2]), ("mixed.rs", &[1, 2, 3]), ("bare.rs", &[1])];
        replacing(&mut fx, &query("cat"), "dog", chosen);

        assert_eq!(fx.read("dos.rs"), "dog\r\ndog\r\n", "a CRLF file stays CRLF");
        assert_eq!(fx.read("mixed.rs"), "dog\r\ndog\ndog", "each line keeps its own ending");
        assert_eq!(fx.read("bare.rs"), "dog", "no final newline is grown");
    }

    #[test]
    fn a_file_written_to_since_the_search_is_skipped_and_reported() {
        let mut fx = Fixture::new(&[("stale.rs", "cat\n"), ("fresh.rs", "cat\n")]);
        let replacer = Replacer::new(&query("cat"), "dog").unwrap();
        let root = fx.project.path().to_path_buf();
        // The search is taken to have run a minute ago; both files were
        // written before it, and then one of them is written again.
        let searched_at = SystemTime::now() - std::time::Duration::from_secs(60);
        set_modified(&fx.path("fresh.rs"), searched_at - std::time::Duration::from_secs(60));

        let chosen = vec![recorded(&fx, "stale.rs", &[1]), recorded(&fx, "fresh.rs", &[1])];
        let (report, change) = fx.history.replace(&root, &replacer, &chosen, searched_at);

        assert_eq!(fx.read("stale.rs"), "cat\n", "what was previewed is not what is there");
        assert_eq!(fx.read("fresh.rs"), "dog\n");
        assert_eq!(
            report.files,
            [
                (PathBuf::from("stale.rs"), Outcome::Skipped(Skipped::Written)),
                (PathBuf::from("fresh.rs"), Outcome::Changed(1)),
            ]
        );
        assert_eq!(report.to_string(), "Changed 1 line in 1 file, skipped 1");
        assert_eq!(change.unwrap().to_string(), "Replaced 1 line in fresh.rs");
    }

    #[test]
    fn a_chosen_line_that_no_longer_matches_skips_its_whole_file() {
        // The mtime check is the one that catches an edit; this is the second
        // belt, and it has to hold on its own, so the file is left with the
        // timestamp it had and only its content is different.
        let mut fx = Fixture::new(&[("a.rs", "cat\ncat\n")]);
        let before = fs::metadata(fx.path("a.rs")).unwrap().modified().unwrap();
        fs::write(fx.path("a.rs"), "cat\nmoved away\n").unwrap();
        set_modified(&fx.path("a.rs"), before);

        let (report, change) = replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1, 2])]);
        assert_eq!(fx.read("a.rs"), "cat\nmoved away\n", "not half of a preview");
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Skipped(Skipped::Moved))]);
        assert!(change.is_none(), "nothing was written, so there is nothing to take back");
        assert!(!fx.history.can_undo());
    }

    #[test]
    fn a_line_that_still_matches_but_is_a_different_line_is_not_written() {
        // The case the timestamp is meant to catch, arranged so it cannot:
        // the file is rewritten and then given back its old modification
        // time, which is what a coarse-grained filesystem does for free on
        // any write inside the same clock second as the search.
        //
        // Only line 1 was chosen. A formatter then reorders the file, so line
        // 1 still matches `cat` — it is just a different line, the one that
        // was line 3 and was never chosen. Asking "does it still match" says
        // yes and rewrites it.
        let mut fx = Fixture::new(&[("a.rs", "let cat = 1;\nlet dog = 2;\nlet cat = 3;\n")]);
        let chosen = vec![recorded(&fx, "a.rs", &[1])];
        let was = fs::metadata(fx.path("a.rs")).unwrap().modified().unwrap();
        let reordered = "let cat = 3;\nlet dog = 2;\nlet cat = 1;\n";
        fs::write(fx.path("a.rs"), reordered).unwrap();
        set_modified(&fx.path("a.rs"), was);

        let (report, change) = applying(&mut fx, &query("cat"), "dog", &chosen);

        assert_eq!(
            fx.read("a.rs"),
            reordered,
            "not one byte, though line 1 still matches the query"
        );
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Skipped(Skipped::Moved))]);
        assert!(change.is_none());
    }

    #[test]
    fn a_line_the_preview_is_only_part_of_is_not_accepted_as_that_line() {
        // `cat` is the whole of the recorded line, so it has to be the whole
        // of the current one. Accepting it as a substring would let a line
        // grow around the match and still be rewritten.
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        let chosen = vec![recorded(&fx, "a.rs", &[1])];
        let was = fs::metadata(fx.path("a.rs")).unwrap().modified().unwrap();
        fs::write(fx.path("a.rs"), "if (cat) { return cat; }\n").unwrap();
        set_modified(&fx.path("a.rs"), was);

        let (report, _) = applying(&mut fx, &query("cat"), "dog", &chosen);
        assert_eq!(fx.read("a.rs"), "if (cat) { return cat; }\n");
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Skipped(Skipped::Moved))]);
    }

    #[test]
    fn a_line_too_long_to_have_been_recorded_whole_is_still_checked() {
        // Over the cap the search only ever saw a window, so being identical
        // is not on offer and the window has to still be in the line. A line
        // changed around it is still the line that was previewed; one changed
        // through it is not.
        let body = format!("{}cat{}", replace::filler(2_000), replace::filler(2_000));
        let window: String = body.chars().skip(1_500).take(crate::MOST_CHARS).collect();
        let chosen =
            vec![(PathBuf::from("a.rs"), vec![Recorded::Window { line: 1, text: window }])];

        let mut fx = Fixture::new(&[("a.rs", &format!("{body}\n"))]);
        let was = fs::metadata(fx.path("a.rs")).unwrap().modified().unwrap();
        fs::write(fx.path("a.rs"), format!("{body} // appended\n")).unwrap();
        set_modified(&fx.path("a.rs"), was);
        let (report, _) = applying(&mut fx, &query("cat"), "dog", &chosen);
        assert_eq!(report.changed(), 1, "changed after the window: still that line");
        assert!(fx.read("a.rs").contains("dog"));
        assert!(fx.read("a.rs").ends_with(" // appended\n"), "and the change is kept");

        let changed = replace::tweak(&body, 1_800);
        let mut fx = Fixture::new(&[("a.rs", &format!("{body}\n"))]);
        let was = fs::metadata(fx.path("a.rs")).unwrap().modified().unwrap();
        fs::write(fx.path("a.rs"), format!("{changed}\n")).unwrap();
        set_modified(&fx.path("a.rs"), was);
        let (report, _) = applying(&mut fx, &query("cat"), "dog", &chosen);
        assert_eq!(
            report.files,
            [(PathBuf::from("a.rs"), Outcome::Skipped(Skipped::Moved))],
            "changed through the window: not that line any more"
        );
    }

    #[test]
    fn a_chosen_line_past_the_end_of_the_file_skips_it() {
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        let (report, _) = replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1, 9])]);
        assert_eq!(fx.read("a.rs"), "cat\n");
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Skipped(Skipped::Moved))]);
    }

    #[test]
    fn a_file_that_is_not_text_is_skipped_rather_than_mangled() {
        let fx_files: &[(&str, &str)] = &[];
        let mut fx = Fixture::new(fx_files);
        let mut bytes = b"cat".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
        bytes.extend_from_slice(b"cat\n");
        fs::write(fx.path("a.bin"), &bytes).unwrap();

        let (report, change) = replacing(&mut fx, &query("cat"), "dog", &[("a.bin", &[1])]);
        assert_eq!(fs::read(fx.path("a.bin")).unwrap(), bytes, "not one byte of it moved");
        assert_eq!(report.files, [(PathBuf::from("a.bin"), Outcome::Skipped(Skipped::NotText))]);
        assert!(change.is_none());
    }

    #[test]
    fn a_missing_file_is_a_failure_the_panel_can_show() {
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        let (report, _) =
            replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1]), ("gone.rs", &[1])]);
        assert_eq!(report.changed(), 1);
        assert_eq!(report.failed(), 1);
        match &report.files[1] {
            (path, Outcome::Failed(why)) => {
                assert_eq!(path, Path::new("gone.rs"));
                assert!(why.contains("gone.rs"), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_line_that_comes_out_as_it_went_in_is_not_written() {
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        let (report, change) = replacing(&mut fx, &query("cat"), "cat", &[("a.rs", &[1])]);
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Changed(0))]);
        assert_eq!(report.lines, 0);
        assert!(change.is_none());
        assert!(!fx.history.can_undo());
    }

    #[test]
    fn one_undo_restores_every_file_a_replace_touched_and_redo_reapplies() {
        let mut fx = Fixture::new(&[
            ("a.rs", "cat\ncat\n"),
            ("src/b.rs", "cat here\r\n"),
            ("src/deep/c.rs", "no cat"),
        ]);
        let before = fx.snapshot();
        let chosen: &[(&str, &[u32])] =
            &[("a.rs", &[1]), ("src/b.rs", &[1]), ("src/deep/c.rs", &[1])];
        let (report, change) = replacing(&mut fx, &query("cat"), "dog", chosen);

        assert_eq!(report.lines, 3);
        assert_eq!(report.to_string(), "Changed 3 lines in 3 files");
        let change = change.unwrap();
        assert_eq!(change.to_string(), "Replaced 3 lines in 3 files");
        assert_eq!(
            change.dirs,
            [fx.project.path().to_path_buf(), fx.path("src"), fx.path("src/deep")]
        );
        let after = fx.snapshot();
        assert_ne!(after, before);

        let undone = fx.history.undo().unwrap().unwrap();
        assert_eq!(undone.to_string(), "Restored 3 files");
        assert!(undone.undone);
        assert_eq!(fx.snapshot(), before, "one undo, every file");

        fx.history.redo().unwrap().unwrap();
        assert_eq!(fx.snapshot(), after, "and redo puts it all back");
        fx.history.undo().unwrap().unwrap();
        assert_eq!(fx.snapshot(), before, "as many times as asked");
    }

    #[test]
    fn a_replace_undoes_in_turn_with_the_operations_around_it() {
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        let before = fx.snapshot();
        fx.history.create_file(fx.path("b.rs")).unwrap();
        replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1])]);
        fx.history.rename(fx.path("a.rs"), "renamed.rs").unwrap();

        while fx.history.undo().unwrap().is_some() {}
        assert_eq!(fx.snapshot(), before);
    }

    #[test]
    fn a_path_named_twice_in_one_replace_is_one_pass_over_the_file() {
        let mut fx = Fixture::new(&[("a.rs", "cat\ncat\n")]);
        let (report, _) =
            replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1]), ("a.rs", &[2])]);
        assert_eq!(fx.read("a.rs"), "dog\ndog\n");
        assert_eq!(report.files, [(PathBuf::from("a.rs"), Outcome::Changed(2))]);
    }

    #[test]
    fn a_replace_that_writes_nothing_leaves_the_redo_branch_alone() {
        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        fx.history.create_file(fx.path("b.rs")).unwrap();
        fx.history.undo().unwrap();
        replacing(&mut fx, &query("cat"), "dog", &[("gone.rs", &[1])]);
        assert!(fx.history.can_redo(), "nothing happened, so nothing was discarded");
    }

    #[test]
    fn non_ascii_text_round_trips_through_a_replace() {
        let mut fx = Fixture::new(&[("a.txt", "日本語 🇮🇸 café\ne\u{301}xtra\n")]);
        let before = fx.snapshot();
        replacing(&mut fx, &query("café"), "kaffihús ☕", &[("a.txt", &[1])]);
        assert_eq!(fx.read("a.txt"), "日本語 🇮🇸 kaffihús ☕\ne\u{301}xtra\n");
        fx.history.undo().unwrap();
        assert_eq!(fx.snapshot(), before);
    }

    /// Set a file's modification time, so a test can say when it was written
    /// relative to a search rather than sleeping until the clock agrees.
    fn set_modified(path: &Path, when: SystemTime) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_undo_that_cannot_write_one_file_says_how_far_it_got() {
        use std::os::unix::fs::PermissionsExt as _;

        let mut fx = Fixture::new(&[("a.rs", "cat\n"), ("b.rs", "cat\n")]);
        let before = fx.snapshot();
        replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1]), ("b.rs", &[1])]);

        // The directory has to refuse too, or the write would simply replace
        // the file it cannot open.
        let locked = fx.path("b.rs");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(fx.project.path(), fs::Permissions::from_mode(0o555)).unwrap();

        match fx.history.undo() {
            Err(OpError::Partial { done, files, path, .. }) => {
                assert_eq!((done, files), (1, 2));
                assert_eq!(path, locked);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fx.read("a.rs"), "cat\n", "the one that could came back");
        assert_eq!(fx.read("b.rs"), "dog\n", "and the one that could not did not");
        assert!(fx.history.can_undo(), "so it can be tried again");

        fs::set_permissions(fx.project.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        fx.history.undo().unwrap().unwrap();
        assert_eq!(fx.snapshot(), before, "a retry finishes what is left, and only that");
    }

    #[test]
    #[cfg(unix)]
    fn an_undo_that_cannot_save_the_current_version_does_not_overwrite_it() {
        // The file write is what settles which side a file is on. If the copy
        // fails to catch up and the file is still recorded as unrestored, a
        // retry swaps it a second time — and since the copy still holds what
        // the file now holds, both sides end up the same and the replaced
        // version is gone, with redo left reporting success over nothing.
        use std::os::unix::fs::PermissionsExt as _;

        let mut fx = Fixture::new(&[("a.rs", "cat\n")]);
        replacing(&mut fx, &query("cat"), "dog", &[("a.rs", &[1])]);
        assert_eq!(fx.read("a.rs"), "dog\n");

        // The trash stops accepting writes after the copy was taken, so the
        // file comes back but the copy cannot be brought up to date.
        // The slot stops accepting new entries, so the version the file holds
        // cannot be put anywhere safe.
        let trash = fx.history.trash().to_path_buf();
        let slots: Vec<PathBuf> =
            copies(&trash).iter().filter_map(|file| file.parent().map(Path::to_path_buf)).collect();
        assert_eq!(slots.len(), 1, "one copy was taken");
        for slot in &slots {
            fs::set_permissions(slot, fs::Permissions::from_mode(0o555)).unwrap();
        }

        let failed = fx.history.undo();
        assert!(matches!(failed, Err(OpError::Partial { done: 0, files: 1, .. })), "{failed:?}");
        assert_eq!(
            fx.read("a.rs"),
            "dog\n",
            "an undo that cannot save what is there does not overwrite it"
        );

        for slot in &slots {
            fs::set_permissions(slot, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fx.history.undo().unwrap().unwrap();
        assert_eq!(fx.read("a.rs"), "cat\n", "and a retry finishes it");
        fx.history.redo().unwrap().unwrap();
        assert_eq!(fx.read("a.rs"), "dog\n", "with the replaced version still there to redo to");
    }

    /// Every file kept under `trash`, however deeply it is nested in slots.
    #[cfg(unix)]
    fn copies(trash: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut look = vec![trash.to_path_buf()];
        while let Some(dir) = look.pop() {
            for entry in fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    look.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    #[derive(Debug, Clone)]
    enum Step {
        CreateFile(usize),
        CreateDir(usize),
        Rename(usize, usize),
        Move(usize, usize),
        Delete(usize),
        Replace(usize),
    }

    const NAMES: [&str; 5] = ["a", "b", "dir", "日本", "🦀"];

    fn step() -> impl Strategy<Value = Step> {
        let n = 0..NAMES.len();
        prop_oneof![
            n.clone().prop_map(Step::CreateFile),
            n.clone().prop_map(Step::CreateDir),
            (n.clone(), n.clone()).prop_map(|(a, b)| Step::Rename(a, b)),
            (n.clone(), n.clone()).prop_map(|(a, b)| Step::Move(a, b)),
            n.clone().prop_map(Step::Delete),
            n.prop_map(Step::Replace),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Whatever sequence of operations succeeds, undoing all of them gets
        /// back the starting tree, and redoing all of them gets back the end.
        #[test]
        fn undo_all_restores_and_redo_all_replays(steps in proptest::collection::vec(step(), 1..12)) {
            let mut fx = Fixture::new(&[("a", "first"), ("dir/inner", "second")]);
            let start = fx.snapshot();
            let root = fx.project.path().to_path_buf();
            let path = |i: usize| root.join(NAMES[i]);
            for step in steps {
                // A replace is not addressed by path the way the others are,
                // and it reports rather than erroring, so it stands aside.
                if let Step::Replace(i) = step {
                    let chosen: &[(&str, &[u32])] = &[(NAMES[i], &[1])];
                    replacing(&mut fx, &query("first"), "1st", chosen);
                    continue;
                }
                let _ = match step {
                    Step::CreateFile(i) => fx.history.create_file(path(i)),
                    Step::CreateDir(i) => fx.history.create_dir(path(i)),
                    Step::Rename(i, j) => fx.history.rename(path(i), NAMES[j]),
                    Step::Move(i, j) => fx.history.move_into(path(i), path(j)),
                    Step::Delete(i) => fx.history.delete(path(i)),
                    Step::Replace(_) => unreachable!("handled above"),
                };
            }
            let end = fx.snapshot();
            while fx.history.undo().unwrap().is_some() {}
            prop_assert_eq!(fx.snapshot(), start);
            while fx.history.redo().unwrap().is_some() {}
            prop_assert_eq!(fx.snapshot(), end);
        }
    }
}
