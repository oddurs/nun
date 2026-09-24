//! One git repository, as far as an editor needs it: where a file is in it,
//! what the index and `HEAD` hold for that file, and writing a new version of
//! it into the index.
//!
//! Everything here reads or writes files, so none of it belongs on the thread
//! that draws — [`crate::Vcs`] calls it from its own threads.
//!
//! Finding the repository is gix's discovery, which is git's: the nearest
//! `.git` at or above a path, a directory or a `gitdir:` file, which is what
//! makes a linked worktree and a submodule work without anything special
//! here. A path in no repository is not an error — [`Repo::discover`] answers
//! `None`, and the marks are simply absent.

use std::fmt;
use std::path::{Path, PathBuf};

use gix::bstr::{BStr, BString};
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// Files larger than this are not diffed: a gutter over a generated file of
/// that size is noise, and the diff would hold the thread for longer than a
/// keystroke is worth.
pub const MOST_BYTES: usize = 16 * 1024 * 1024;

/// Something git would not do, as a sentence for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub(crate) fn context(what: &str, error: impl fmt::Display) -> Self {
        Self(format!("{what}: {error}"))
    }
}

/// Which old version of a file to compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Against {
    /// What is staged: the version `git diff` compares with.
    Index,
    /// The last commit: the version `git diff HEAD` compares with.
    Head,
}

/// A repository with a working tree.
#[derive(Debug)]
pub struct Repo {
    git: gix::Repository,
    workdir: PathBuf,
    /// The working tree with symlinks resolved, for a path spelled through a
    /// different route to the same place — `/tmp` and `/private/tmp`.
    canonical: Option<PathBuf>,
}

/// An old version of a file, as git stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    /// Its object id, which changes exactly when its content does.
    pub id: gix::ObjectId,
    /// Its bytes.
    pub data: Vec<u8>,
}

impl Repo {
    /// The repository `path` is in, if any. `path` may be a file or a folder,
    /// and need not exist yet.
    ///
    /// A bare repository has no files to compare, and counts as none.
    #[must_use]
    pub fn discover(path: &Path) -> Option<Self> {
        let start = path.ancestors().find(|dir| dir.is_dir())?;
        let repo: gix::Repository = gix::ThreadSafeRepository::discover_opts(
            start,
            gix::discover::upwards::Options::default(),
            options(),
        )
        .ok()?
        .into();
        let workdir = repo.workdir()?.to_path_buf();
        let canonical = workdir.canonicalize().ok();
        Some(Self { git: repo, workdir, canonical })
    }

    /// The top of the working tree.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The directory holding the repository's shared state: `.git` for most,
    /// and the main checkout's `.git` for a linked worktree. Two `Repo`s with
    /// the same common dir and working tree are the same checkout.
    #[must_use]
    pub fn common_dir(&self) -> &Path {
        self.git.common_dir()
    }

    /// Where `path` is inside the working tree, as git spells it: relative,
    /// with `/` between components. `None` for a path outside it.
    #[must_use]
    pub fn relative(&self, path: &Path) -> Option<BString> {
        let rel = relative_to(path, &self.workdir, self.canonical.as_deref())?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        Some(gix::path::to_unix_separators_on_windows(gix::path::into_bstr(rel)).into_owned())
    }

    /// The staged version of the file at `rel`, when it is a regular file
    /// with nothing unresolved about it. `None` for an untracked file, a
    /// conflicted one, a symlink or a submodule.
    ///
    /// # Errors
    ///
    /// If the index or the object cannot be read.
    pub fn index_blob(&self, rel: &BStr) -> Result<Option<Blob>, Error> {
        let index =
            self.git.index_or_empty().map_err(|e| Error::context("Could not read the index", e))?;
        let Some(entry) = index.entry_by_path_and_stage(rel, Stage::Unconflicted) else {
            return Ok(None);
        };
        if !matches!(entry.mode, Mode::FILE | Mode::FILE_EXECUTABLE) {
            return Ok(None);
        }
        self.blob(entry.id).map(Some)
    }

    /// The staged version's id alone, which is cheap: whether the base has
    /// changed can be told without reading it.
    ///
    /// # Errors
    ///
    /// If the index cannot be read.
    pub fn index_id(&self, rel: &BStr) -> Result<Option<gix::ObjectId>, Error> {
        let index =
            self.git.index_or_empty().map_err(|e| Error::context("Could not read the index", e))?;
        Ok(index
            .entry_by_path_and_stage(rel, Stage::Unconflicted)
            .filter(|entry| matches!(entry.mode, Mode::FILE | Mode::FILE_EXECUTABLE))
            .map(|entry| entry.id))
    }

    /// The committed version of the file at `rel`. `None` when `HEAD` has no
    /// such file, or no commit at all yet. A detached `HEAD` is a commit like
    /// any other.
    ///
    /// # Errors
    ///
    /// If `HEAD` or the object cannot be read.
    pub fn head_blob(&self, rel: &BStr) -> Result<Option<Blob>, Error> {
        let Ok(tree) = self.git.head_tree() else { return Ok(None) };
        let path = gix::path::from_bstr(rel);
        let entry = tree
            .lookup_entry_by_path(&*path)
            .map_err(|e| Error::context("Could not read HEAD", e))?;
        match entry {
            Some(entry) if entry.mode().is_blob() => self.blob(entry.object_id()).map(Some),
            _ => Ok(None),
        }
    }

    /// The old version of `rel` to compare against.
    ///
    /// # Errors
    ///
    /// If it cannot be read.
    pub fn blob_against(&self, rel: &BStr, against: Against) -> Result<Option<Blob>, Error> {
        match against {
            Against::Index => self.index_blob(rel),
            Against::Head => self.head_blob(rel),
        }
    }

    fn blob(&self, id: gix::ObjectId) -> Result<Blob, Error> {
        let blob =
            self.git.find_blob(id).map_err(|e| Error::context("Could not read a blob", e))?;
        Ok(Blob { id, data: blob.detach().data })
    }

    /// Write `bytes` into the index as the staged version of `rel`, leaving
    /// the working tree alone — what `git add -p` does for one hunk.
    ///
    /// Only a file already in the index can be staged this way, since a new
    /// entry needs a mode and a decision git makes on `add`. An entry added
    /// with `git add -N` is in the index, and stops being only an intent.
    ///
    /// The bytes are written as they are: no clean filter runs, so a file
    /// under one (Git LFS, say) should not be staged through here. Line
    /// endings are the caller's to get right — [`crate::Base::encode`] does.
    ///
    /// # Errors
    ///
    /// If `rel` is not a plain file in the index, the index is split or
    /// sparse (both of which this does not write), or it cannot be locked,
    /// read or written.
    pub fn stage(&self, rel: &BStr, bytes: &[u8]) -> Result<(), Error> {
        let mut index =
            self.git.open_index().map_err(|e| Error::context("Could not read the index", e))?;
        if index.link().is_some() || index.is_sparse() {
            return Err(Error(
                "This repository's index is split or sparse, which nun cannot write yet.".into(),
            ));
        }
        let Some(at) = index.entry_index_by_path_and_stage(rel, Stage::Unconflicted) else {
            return Err(Error(format!(
                "{rel} is not in the index, so there is nothing to stage into."
            )));
        };
        if !matches!(index.entries()[at].mode, Mode::FILE | Mode::FILE_EXECUTABLE) {
            return Err(Error(format!("{rel} is not a plain file in the index.")));
        }
        let id = self
            .git
            .write_blob(bytes)
            .map_err(|e| Error::context("Could not write the staged text", e))?
            .detach();
        let entry = &mut index.entries_mut()[at];
        entry.id = id;
        // The recorded stat described the old blob. Left in place, git would
        // take a file whose stat still matches as unchanged since staging,
        // when it now differs from the index.
        entry.stat = Stat::default();
        entry.flags.remove(Flags::UPTODATE | Flags::FSMONITOR_VALID | Flags::INTENT_TO_ADD);
        // The tree cache is written back as it was read, and still says the
        // folders above this entry hold what they held. A commit would trust
        // it and record the old content.
        index.remove_tree();
        index
            .write(gix::index::write::Options::default())
            .map_err(|e| Error::context("Could not write the index", e))
    }

    /// The underlying repository, for status.
    pub(crate) const fn gix(&self) -> &gix::Repository {
        &self.git
    }
}

/// How to open a repository: as git would, except that `GIT_*` variables are
/// not heeded. The repository is the one the file is in, whatever the
/// environment says — and with nun as git's editor, or started from a hook,
/// `GIT_INDEX_FILE` and `GIT_WORK_TREE` describe some other operation.
fn options() -> gix::sec::trust::Mapping<gix::open::Options> {
    use gix::sec::trust::DefaultForLevel;
    let mut trust = gix::sec::trust::Mapping::<gix::open::Options>::default();
    for (options, level) in
        [(&mut trust.full, gix::sec::Trust::Full), (&mut trust.reduced, gix::sec::Trust::Reduced)]
    {
        let mut permissions = gix::open::Permissions::default_for_level(level);
        permissions.env.git_prefix = gix::sec::Permission::Deny;
        options.modify(|options| options.permissions(permissions));
    }
    trust
}

/// `path` relative to `dir`, trying the spelling it was given first and the
/// one with symlinks resolved second.
pub(crate) fn relative_to(path: &Path, dir: &Path, canonical: Option<&Path>) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(dir) {
        return Some(rel.to_path_buf());
    }
    let canonical = canonical?;
    // The file itself may not exist yet; its folder has to.
    let (folder, name) = match path.canonicalize() {
        Ok(real) => (real, None),
        Err(_) => (path.parent()?.canonicalize().ok()?, path.file_name()),
    };
    let rel = folder.strip_prefix(canonical).ok()?;
    Some(name.map_or_else(|| rel.to_path_buf(), |name| rel.join(name)))
}

/// Whether `bytes` look like something other than text, the way git decides:
/// a NUL in the first few thousand bytes.
#[must_use]
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}
