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

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        self.done.push(Record { operation: Operation::Delete(path), trashed: Some(slot) });
        self.undone.clear();
        Ok(change(&self.done[self.done.len() - 1].operation, false))
    }

    /// Reverse the most recent operation.
    ///
    /// Returns `Ok(None)` when there is nothing to undo.
    ///
    /// # Errors
    ///
    /// Whatever stopped the reversal, most often [`OpError::Exists`] because
    /// something has taken the original name since. The operation stays on
    /// the undo stack so it can be tried again.
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
    /// Whatever stopped it, most often [`OpError::Exists`]. The operation stays
    /// on the redo stack.
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
        self.done.push(Record { operation, trashed: None });
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
        }
        Ok(())
    }

    /// Move `path` into a fresh slot in the trash and return where it went.
    ///
    /// Each slot is its own directory, so two deleted files with the same name
    /// never meet, and the entry keeps its name inside it for anyone looking
    /// through the trash by hand.
    fn trash_into(&mut self, path: &Path) -> Result<PathBuf, OpError> {
        if fs::symlink_metadata(path).is_err() {
            return Err(OpError::Missing(path.to_path_buf()));
        }
        let Some(name) = path.file_name() else {
            return Err(OpError::InvalidName(path.display().to_string()));
        };
        self.slots += 1;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let slot = self.trash.join(format!("{stamp}-{}-{}", std::process::id(), self.slots));
        fs::create_dir_all(&slot).map_err(|error| io_error(&slot, error))?;
        let target = slot.join(name);
        if let Err(error) = relocate(path, &target) {
            let _ = fs::remove_dir(&slot);
            return Err(error);
        }
        Ok(target)
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[derive(Debug, Clone)]
    enum Step {
        CreateFile(usize),
        CreateDir(usize),
        Rename(usize, usize),
        Move(usize, usize),
        Delete(usize),
    }

    const NAMES: [&str; 5] = ["a", "b", "dir", "日本", "🦀"];

    fn step() -> impl Strategy<Value = Step> {
        let n = 0..NAMES.len();
        prop_oneof![
            n.clone().prop_map(Step::CreateFile),
            n.clone().prop_map(Step::CreateDir),
            (n.clone(), n.clone()).prop_map(|(a, b)| Step::Rename(a, b)),
            (n.clone(), n.clone()).prop_map(|(a, b)| Step::Move(a, b)),
            n.prop_map(Step::Delete),
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
                let _ = match step {
                    Step::CreateFile(i) => fx.history.create_file(path(i)),
                    Step::CreateDir(i) => fx.history.create_dir(path(i)),
                    Step::Rename(i, j) => fx.history.rename(path(i), NAMES[j]),
                    Step::Move(i, j) => fx.history.move_into(path(i), path(j)),
                    Step::Delete(i) => fx.history.delete(path(i)),
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
