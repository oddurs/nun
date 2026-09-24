//! Creating, moving and deleting files for a caller that keeps its own way
//! back: the file operations a language server asks for as part of an edit.
//!
//! They go through [`FsHistory`], so they follow its rules — nothing is ever
//! overwritten, and a delete goes to the trash rather than away — but they
//! are not pushed onto its undo stack. A server's rename that moves a file
//! also edits the files that name it, and the tree's undo taking back the move
//! alone would leave those pointing at nothing. So each operation that is
//! carried out comes back with the operation that reverses it, and the caller
//! keeps those beside the rest of the edit and takes all of it back together.
//!
//! **Operations run in order and stop at the first that fails.** Each one
//! comes back beside what became of it, so the caller can say exactly which
//! happened. The reverse of each is itself a [`FileOp`], carried out the same
//! way: taking a create back puts the file in the trash only while it still
//! holds what was created, and taking a delete back brings the entry out of
//! the trash only while its old place is still free.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::ops::{self, FsHistory, OpError};

/// One file operation, or the reverse of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Create a file that is not there yet, holding `text`.
    Create {
        /// The new file.
        path: PathBuf,
        /// What it starts out holding.
        text: String,
    },
    /// Move an entry, file or directory, to a place nothing is at.
    Move {
        /// Where it is.
        from: PathBuf,
        /// Where it goes.
        to: PathBuf,
    },
    /// Move an entry, file or directory, into the trash.
    Delete {
        /// The entry.
        path: PathBuf,
    },
    /// Bring an entry a delete put in the trash back to where it was: the
    /// reverse of [`FileOp::Delete`].
    Restore {
        /// Where it is in the trash.
        trashed: PathBuf,
        /// Where it goes back to.
        path: PathBuf,
    },
    /// Put a file that was created into the trash, provided it still holds
    /// what it was created with: the reverse of [`FileOp::Create`].
    Discard {
        /// The file.
        path: PathBuf,
        /// What it must still hold.
        text: String,
    },
}

/// What became of one [`FileOp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carried {
    /// It happened, and this is the operation that takes it back.
    Done(FileOp),
    /// It did not happen, and this is why, as a sentence.
    Failed(String),
    /// It was not tried: something before it stopped the run.
    NotReached,
}

/// Whether something is at a path, and what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Present {
    /// Nothing is.
    Missing,
    /// A file, or a link, which is moved and trashed as itself.
    File,
    /// A directory.
    Dir,
}

/// What is at `path`, without following a link there.
#[must_use]
pub fn probe(path: &Path) -> Present {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Present::Dir,
        Ok(_) => Present::File,
        Err(_) => Present::Missing,
    }
}

impl FsHistory {
    /// Carry out `ops` in order, stopping at the first that fails, without
    /// recording them in the undo history.
    ///
    /// Each comes back beside what became of it, in the order given.
    pub fn carry_out(&mut self, ops: &[FileOp]) -> Vec<(FileOp, Carried)> {
        let mut outcomes = Vec::with_capacity(ops.len());
        let mut stopped = false;
        for op in ops {
            let carried = if stopped {
                Carried::NotReached
            } else {
                match self.carry(op) {
                    Ok(reverse) => Carried::Done(reverse),
                    Err(why) => {
                        stopped = true;
                        Carried::Failed(why)
                    }
                }
            };
            outcomes.push((op.clone(), carried));
        }
        outcomes
    }

    fn carry(&mut self, op: &FileOp) -> Result<FileOp, String> {
        let failed = |error: OpError| error.to_string();
        match op {
            FileOp::Create { path, text } => {
                create(path, text).map_err(failed)?;
                Ok(FileOp::Discard { path: path.clone(), text: text.clone() })
            }
            FileOp::Move { from, to } => {
                ops::relocate(from, to).map_err(failed)?;
                Ok(FileOp::Move { from: to.clone(), to: from.clone() })
            }
            FileOp::Delete { path } => {
                let trashed = self.trash_into(path).map_err(failed)?;
                Ok(FileOp::Restore { trashed, path: path.clone() })
            }
            FileOp::Restore { trashed, path } => {
                ops::restore(Some(trashed), path).map_err(failed)?;
                Ok(FileOp::Delete { path: path.clone() })
            }
            FileOp::Discard { path, text } => {
                match fs::read(path) {
                    Ok(bytes) if bytes == text.as_bytes() => {}
                    Ok(_) => return Err(format!("{} has changed since", path.display())),
                    Err(error) => return Err(ops::io_error(path, error).to_string()),
                }
                let trashed = self.trash_into(path).map_err(failed)?;
                Ok(FileOp::Restore { trashed, path: path.clone() })
            }
        }
    }
}

/// Make a new file holding `text`, never over one that is there. A file that
/// was made but could not be written is taken away again, so a failure leaves
/// nothing behind.
fn create(path: &Path, text: &str) -> Result<(), OpError> {
    let mut file = fs::File::create_new(path).map_err(|error| ops::io_error(path, error))?;
    if let Err(error) = file.write_all(text.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(ops::io_error(path, error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(dir: &Path) -> FsHistory {
        FsHistory::new(dir.join(".trash"))
    }

    #[test]
    fn a_move_a_create_and_a_delete_come_back_with_their_reverses() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        fs::write(at("foo.rs"), "foo").unwrap();
        fs::write(at("old.rs"), "old").unwrap();
        let mut history = history(dir.path());

        let done = history.carry_out(&[
            FileOp::Move { from: at("foo.rs"), to: at("bar.rs") },
            FileOp::Create { path: at("new.rs"), text: "new".into() },
            FileOp::Delete { path: at("old.rs") },
        ]);
        assert_eq!(fs::read_to_string(at("bar.rs")).unwrap(), "foo");
        assert_eq!(fs::read_to_string(at("new.rs")).unwrap(), "new");
        assert!(!at("foo.rs").exists() && !at("old.rs").exists());
        assert!(!history.can_undo(), "kept off the tree's undo");

        let mut reverses: Vec<FileOp> = done
            .into_iter()
            .map(|(_, carried)| match carried {
                Carried::Done(reverse) => reverse,
                other => panic!("{other:?}"),
            })
            .collect();
        reverses.reverse();
        let undone = history.carry_out(&reverses);
        assert!(undone.iter().all(|(_, carried)| matches!(carried, Carried::Done(_))));
        assert_eq!(fs::read_to_string(at("foo.rs")).unwrap(), "foo");
        assert_eq!(fs::read_to_string(at("old.rs")).unwrap(), "old");
        assert!(!at("bar.rs").exists() && !at("new.rs").exists());
    }

    #[test]
    fn nothing_is_overwritten_and_the_run_stops_where_it_failed() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        fs::write(at("a.rs"), "a").unwrap();
        fs::write(at("taken.rs"), "mine").unwrap();
        let mut history = history(dir.path());

        let done = history.carry_out(&[
            FileOp::Move { from: at("a.rs"), to: at("b.rs") },
            FileOp::Create { path: at("taken.rs"), text: "theirs".into() },
            FileOp::Delete { path: at("b.rs") },
        ]);
        assert!(matches!(done[0].1, Carried::Done(_)));
        assert!(matches!(&done[1].1, Carried::Failed(why) if why.contains("already exists")));
        assert_eq!(done[2].1, Carried::NotReached);
        assert_eq!(fs::read_to_string(at("taken.rs")).unwrap(), "mine");
        assert!(at("b.rs").exists(), "not deleted");
    }

    #[test]
    fn a_created_file_written_to_since_is_not_taken_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.rs");
        let mut history = history(dir.path());
        let done = history.carry_out(&[FileOp::Create { path: path.clone(), text: "x".into() }]);
        let Carried::Done(reverse) = done[0].1.clone() else { panic!() };
        fs::write(&path, "x and more").unwrap();
        let undone = history.carry_out(&[reverse]);
        assert!(matches!(&undone[0].1, Carried::Failed(why) if why.contains("changed since")));
        assert_eq!(fs::read_to_string(&path).unwrap(), "x and more");
    }

    #[test]
    fn a_probe_tells_files_from_directories_from_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "").unwrap();
        assert_eq!(probe(&dir.path().join("a")), Present::File);
        assert_eq!(probe(dir.path()), Present::Dir);
        assert_eq!(probe(&dir.path().join("b")), Present::Missing);
    }
}
