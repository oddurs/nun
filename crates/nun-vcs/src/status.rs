//! What has changed in a working tree, per path — what the file tree colours
//! its rows by.
//!
//! One status is two comparisons folded together, as `git status` shows them:
//! `HEAD` against the index (what is staged) and the index against the files
//! (what is not). A file new in either is added, whichever side it is new on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gix::status::index_worktree::Item as Worktree;
use gix::status::{Item, Submodule, UntrackedFiles};

use crate::repo::{Error, Repo};

/// How one path differs from what git has recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileStatus {
    /// New: untracked, or staged but not in `HEAD`.
    Added,
    /// Changed, staged or not.
    Modified,
    /// Gone from the working tree or the index.
    Deleted,
    /// Mid-merge with conflicts unresolved.
    Conflicted,
}

impl FileStatus {
    /// Which of two statuses for one path to show. A conflict is the most
    /// urgent thing to know; a file both staged as new and edited since is
    /// still new.
    fn strongest(self, other: Self) -> Self {
        let rank = |status| match status {
            Self::Modified => 0,
            Self::Added => 1,
            Self::Deleted => 2,
            Self::Conflicted => 3,
        };
        if rank(other) > rank(self) { other } else { self }
    }

    /// What a folder holding a path with this status shows, combined with
    /// what it already shows. A folder is added when everything changed in it
    /// is new, and modified otherwise.
    fn folder(self, before: Option<Self>) -> Self {
        let this = match self {
            Self::Added => Self::Added,
            Self::Modified | Self::Deleted => Self::Modified,
            Self::Conflicted => Self::Conflicted,
        };
        match (before, this) {
            (None, this) => this,
            (Some(Self::Conflicted), _) | (_, Self::Conflicted) => Self::Conflicted,
            (Some(Self::Added), Self::Added) => Self::Added,
            _ => Self::Modified,
        }
    }
}

/// The status of every changed path in one working tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    workdir: PathBuf,
    /// Other spellings of the working tree that [`Status::of`] accepts: the
    /// one with symlinks resolved, and the one the folder it was asked for
    /// uses. Worked out when it is made, so a lookup never touches the disk.
    spellings: Vec<PathBuf>,
    /// Changed files, by path relative to the working tree.
    files: HashMap<PathBuf, FileStatus>,
    /// Folders with changes somewhere inside, the same way.
    folders: HashMap<PathBuf, FileStatus>,
}

impl Status {
    /// A status over `workdir` with `files` changed, each relative to it.
    #[must_use]
    pub fn from_files(
        workdir: &Path,
        files: impl IntoIterator<Item = (PathBuf, FileStatus)>,
    ) -> Self {
        let mut status = Self {
            workdir: workdir.to_path_buf(),
            spellings: workdir.canonicalize().ok().into_iter().collect(),
            ..Self::default()
        };
        for (path, file) in files {
            status.add(path, file);
        }
        status
    }

    fn add(&mut self, path: PathBuf, file: FileStatus) {
        for folder in path.ancestors().skip(1).filter(|folder| !folder.as_os_str().is_empty()) {
            let before = self.folders.get(folder).copied();
            self.folders.insert(folder.to_path_buf(), file.folder(before));
        }
        let merged = self.files.get(&path).map_or(file, |before| before.strongest(file));
        self.files.insert(path, merged);
    }

    /// The working tree it describes.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// How `path` differs, whether a file or a folder with changes inside.
    /// `None` for anything unchanged, or outside the working tree.
    ///
    /// Only compares paths, so it is cheap enough to ask for every row of a
    /// tree on every frame: `path` is found under the working tree as git
    /// spells it, as it is with symlinks resolved, or as the folder the
    /// status was asked for spells it.
    #[must_use]
    pub fn of(&self, path: &Path) -> Option<FileStatus> {
        let rel = std::iter::once(&self.workdir)
            .chain(&self.spellings)
            .find_map(|dir| path.strip_prefix(dir).ok())?;
        self.files.get(rel).or_else(|| self.folders.get(rel)).copied()
    }

    /// Accept paths spelled the way `root` — a folder in the working tree —
    /// spells it, when that differs from git's spelling: through a symlink,
    /// say, or `/tmp` for `/private/tmp`. Reads the disk, so it is done once,
    /// off the thread that draws.
    pub(crate) fn spelled_from(&mut self, root: &Path) {
        if self.of_root(root) {
            return;
        }
        let (Ok(resolved), Some(canonical)) =
            (root.canonicalize(), self.workdir.canonicalize().ok())
        else {
            return;
        };
        let Ok(inside) = resolved.strip_prefix(&canonical) else { return };
        let mut base = root.to_path_buf();
        for _ in inside.components() {
            if !base.pop() {
                return;
            }
        }
        self.spellings.push(base);
    }

    fn of_root(&self, root: &Path) -> bool {
        std::iter::once(&self.workdir).chain(&self.spellings).any(|dir| root.starts_with(dir))
    }

    /// Every changed file, relative to the working tree.
    pub fn files(&self) -> impl Iterator<Item = (&Path, FileStatus)> {
        self.files.iter().map(|(path, status)| (path.as_path(), *status))
    }

    /// Whether nothing has changed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

impl Repo {
    /// Work out what has changed in the working tree.
    ///
    /// Untracked files are listed one by one rather than collapsed into their
    /// folder, so each shows as added once the folder is expanded. Ignored
    /// ones are not walked at all, which is what keeps a large `target/` from
    /// costing anything. A submodule counts as modified when its checkout is
    /// at a different commit, without looking inside it for edits.
    ///
    /// # Errors
    ///
    /// If git's files cannot be read.
    pub fn status(&self) -> Result<Status, Error> {
        let repo = self.gix();
        let platform = repo
            .status(gix::progress::Discard)
            .map_err(|e| Error::context("Could not start a status", e))?
            .untracked_files(UntrackedFiles::Files)
            .index_worktree_rewrites(None)
            .index_worktree_submodules(Submodule::Given {
                ignore: gix::submodule::config::Ignore::Dirty,
                check_dirty: true,
            });
        // With no commit yet, everything staged is new against nothing.
        let platform = match repo.head_tree_id_or_empty() {
            Ok(tree) => platform.head_tree(tree.detach()),
            Err(_) => platform,
        };
        let items = platform
            .into_iter(None)
            .map_err(|e| Error::context("Could not work out the status", e))?;
        let mut files = Vec::new();
        for item in items {
            let item = item.map_err(|e| Error::context("Could not work out the status", e))?;
            if let Some(found) = summarise(&item) {
                files.push((gix::path::from_bstr(item.location()).into_owned(), found));
            }
        }
        Ok(Status::from_files(self.workdir(), files))
    }
}

/// One item of gix's status as one of ours, or `None` for what only matters
/// to git's own bookkeeping.
fn summarise(item: &Item) -> Option<FileStatus> {
    use gix::status::index_worktree::iter::Summary;
    match item {
        Item::IndexWorktree(change) => match change {
            Worktree::Rewrite { .. } => Some(FileStatus::Added),
            _ => match change.summary()? {
                Summary::Added | Summary::IntentToAdd | Summary::Copied | Summary::Renamed => {
                    Some(FileStatus::Added)
                }
                Summary::Modified | Summary::TypeChange => Some(FileStatus::Modified),
                Summary::Removed => Some(FileStatus::Deleted),
                Summary::Conflict => Some(FileStatus::Conflicted),
            },
        },
        Item::TreeIndex(change) => Some(match change {
            gix::diff::index::ChangeRef::Addition { .. }
            | gix::diff::index::ChangeRef::Rewrite { .. } => FileStatus::Added,
            gix::diff::index::ChangeRef::Deletion { .. } => FileStatus::Deleted,
            gix::diff::index::ChangeRef::Modification { .. } => FileStatus::Modified,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_added_only_when_everything_in_them_is_new() {
        let status = Status::from_files(
            Path::new("/w"),
            [
                (PathBuf::from("new/a.rs"), FileStatus::Added),
                (PathBuf::from("new/deep/b.rs"), FileStatus::Added),
                (PathBuf::from("mixed/a.rs"), FileStatus::Added),
                (PathBuf::from("mixed/b.rs"), FileStatus::Modified),
                (PathBuf::from("gone/c.rs"), FileStatus::Deleted),
            ],
        );
        assert_eq!(status.of(Path::new("/w/new")), Some(FileStatus::Added));
        assert_eq!(status.of(Path::new("/w/new/deep")), Some(FileStatus::Added));
        assert_eq!(status.of(Path::new("/w/mixed")), Some(FileStatus::Modified));
        assert_eq!(status.of(Path::new("/w/mixed/b.rs")), Some(FileStatus::Modified));
        assert_eq!(status.of(Path::new("/w/gone")), Some(FileStatus::Modified));
        assert_eq!(status.of(Path::new("/w/other.rs")), None);
        assert_eq!(status.of(Path::new("/elsewhere/new")), None);
        // The working tree itself is not a row with a status.
        assert_eq!(status.of(Path::new("/w")), None);
    }

    #[test]
    fn a_conflict_outranks_everything_for_a_path() {
        let status = Status::from_files(
            Path::new("/w"),
            [
                (PathBuf::from("a"), FileStatus::Modified),
                (PathBuf::from("a"), FileStatus::Conflicted),
                (PathBuf::from("a"), FileStatus::Added),
            ],
        );
        assert_eq!(status.of(Path::new("/w/a")), Some(FileStatus::Conflicted));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_symlink_finds_its_status() {
        let real = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(real.path().join("src")).unwrap();
        let links = tempfile::TempDir::new().unwrap();
        let link = links.path().join("checkout");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let mut status =
            Status::from_files(real.path(), [(PathBuf::from("src/a.rs"), FileStatus::Added)]);
        assert_eq!(status.of(&link.join("src/a.rs")), None, "not before it is told the spelling");
        status.spelled_from(&link.join("src"));
        assert_eq!(status.of(&link.join("src/a.rs")), Some(FileStatus::Added));
        assert_eq!(status.of(&real.path().join("src/a.rs")), Some(FileStatus::Added));
    }
}
