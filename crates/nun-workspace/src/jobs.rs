//! Filesystem work, off the thread that draws.
//!
//! Listing a directory and moving a subtree are both unbounded: a network
//! mount, a folder with a hundred thousand entries, a recursive copy across
//! filesystems. None of it may happen while a frame is waiting, so all of it
//! happens here and comes back as a message, the same way the watcher does.
//!
//! The worker owns the [`FsHistory`], because undo and redo are themselves
//! filesystem work. The editor never holds it, so it can never be tempted to
//! call it inline.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::ops::{Change, FsHistory};
use crate::tree::{Entry, list_dir};

/// Something to do with the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// List a directory for the tree.
    List {
        /// The directory to list.
        dir: PathBuf,
        /// Whether ignored and hidden entries should be listed too.
        show_ignored: bool,
    },
    /// Create an empty file.
    CreateFile(PathBuf),
    /// Create an empty directory.
    CreateDir(PathBuf),
    /// Rename an entry within its directory.
    Rename {
        /// What to rename.
        from: PathBuf,
        /// Its new name.
        name: String,
    },
    /// Move an entry into a directory.
    MoveInto {
        /// What to move.
        from: PathBuf,
        /// Where to put it.
        dir: PathBuf,
    },
    /// Move an entry to the trash.
    Delete(PathBuf),
    /// Undo the last operation.
    Undo,
    /// Redo the last undone operation.
    Redo,
    /// Nothing: a marker that comes back once everything queued before it is
    /// done. Jobs are done in order, so this is how a caller waits for the
    /// worker to catch up without guessing at a delay.
    Echo(u64),
}

/// What a job came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// A directory was listed, or could not be.
    Listed {
        /// Which directory.
        dir: PathBuf,
        /// Its entries, or why they could not be read.
        entries: Result<Vec<Entry>, String>,
    },
    /// An operation, an undo or a redo happened.
    Changed(Change),
    /// It did not happen, and this is why, as a sentence.
    Failed(String),
    /// The marker from [`Job::Echo`], and with it the news that everything
    /// asked for before it has been done.
    Echo(u64),
    /// There was nothing to undo or redo.
    Nothing {
        /// Whether the empty stack was redo's rather than undo's.
        redo: bool,
    },
}

/// A worker doing filesystem work and reporting back.
///
/// Dropping it stops the worker once the jobs it has are finished.
#[derive(Debug)]
pub struct Jobs {
    sender: Sender<Job>,
}

impl Jobs {
    /// Start a worker that deletes into `trash` and reports through `report`.
    ///
    /// `report` is called on the worker's thread, so it should do nothing but
    /// hand the message on — post it to the editor's event channel and let the
    /// main thread act on it.
    #[must_use]
    pub fn new(trash: impl Into<PathBuf>, report: Box<dyn Fn(Done) + Send + 'static>) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let trash = trash.into();
        thread::spawn(move || {
            let mut history = FsHistory::new(trash);
            for job in receiver {
                report(run(&mut history, job));
            }
        });
        Self { sender }
    }

    /// Ask for `job` to be done.
    ///
    /// A worker that has gone — only at shutdown, when the editor is dropping
    /// everything anyway — silently drops the job rather than failing a
    /// keystroke.
    pub fn send(&self, job: Job) {
        let _ = self.sender.send(job);
    }

    /// Ask for `dir` to be listed.
    pub fn list(&self, dir: impl Into<PathBuf>, show_ignored: bool) {
        self.send(Job::List { dir: dir.into(), show_ignored });
    }
}

fn run(history: &mut FsHistory, job: Job) -> Done {
    let outcome = match job {
        Job::List { dir, show_ignored } => {
            let entries = list_dir(&dir, show_ignored);
            return Done::Listed { dir, entries };
        }
        Job::CreateFile(path) => history.create_file(path),
        Job::CreateDir(path) => history.create_dir(path),
        Job::Rename { from, name } => history.rename(from, &name),
        Job::MoveInto { from, dir } => history.move_into(from, dir),
        Job::Delete(path) => history.delete(path),
        Job::Echo(marker) => return Done::Echo(marker),
        Job::Undo => return step(history.undo(), true),
        Job::Redo => return step(history.redo(), false),
    };
    match outcome {
        Ok(change) => Done::Changed(change),
        Err(error) => Done::Failed(error.to_string()),
    }
}

fn step(outcome: Result<Option<Change>, crate::ops::OpError>, undo: bool) -> Done {
    match outcome {
        Ok(Some(change)) => Done::Changed(change),
        Ok(None) => Done::Nothing { redo: !undo },
        Err(error) => Done::Failed(error.to_string()),
    }
}

/// The trash a worker would use, for callers that want to say where it is.
#[must_use]
pub fn trash_or_temp() -> PathBuf {
    crate::ops::default_trash_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("nun").join("trash"))
}

/// Whether `path` is inside `dir`, for deciding what a change affects.
#[must_use]
pub fn is_inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    fn worker(trash: &Path) -> (Jobs, Receiver<Done>) {
        let (sender, receiver) = mpsc::channel();
        let jobs = Jobs::new(
            trash,
            Box::new(move |done| {
                let _ = sender.send(done);
            }),
        );
        (jobs, receiver)
    }

    fn next(receiver: &Receiver<Done>) -> Done {
        receiver.recv_timeout(Duration::from_secs(5)).expect("the worker answered")
    }

    #[test]
    fn a_listing_comes_back_as_a_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        jobs.list(dir.path(), false);
        match next(&receiver) {
            Done::Listed { dir: listed, entries } => {
                assert_eq!(listed, dir.path());
                let names: Vec<String> = entries
                    .unwrap()
                    .iter()
                    .map(|entry| entry.name.to_string_lossy().into())
                    .collect();
                assert_eq!(names, vec!["a.txt"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn listing_something_unreadable_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.list(dir.path().join("nowhere"), false);
        match next(&receiver) {
            Done::Listed { entries, .. } => assert!(entries.is_err()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn operations_and_their_undo_run_on_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        jobs.send(Job::CreateFile(dir.path().join("new.txt")));
        assert!(matches!(next(&receiver), Done::Changed(_)));
        assert!(dir.path().join("new.txt").exists());

        jobs.send(Job::Delete(dir.path().join("new.txt")));
        assert!(matches!(next(&receiver), Done::Changed(_)));
        assert!(!dir.path().join("new.txt").exists());

        jobs.send(Job::Undo);
        match next(&receiver) {
            Done::Changed(change) => assert!(change.undone),
            other => panic!("{other:?}"),
        }
        assert!(dir.path().join("new.txt").exists());

        jobs.send(Job::Redo);
        assert!(matches!(next(&receiver), Done::Changed(_)));
        assert!(!dir.path().join("new.txt").exists());
    }

    #[test]
    fn an_operation_that_cannot_happen_comes_back_as_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Delete(dir.path().join("missing")));
        match next(&receiver) {
            Done::Failed(message) => assert!(message.contains("does not exist"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn undo_with_nothing_to_undo_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Undo);
        assert_eq!(next(&receiver), Done::Nothing { redo: false });
    }

    #[test]
    fn an_echo_comes_back_after_everything_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::CreateFile(dir.path().join("first")));
        jobs.send(Job::Echo(7));

        assert!(matches!(next(&receiver), Done::Changed(_)));
        assert_eq!(next(&receiver), Done::Echo(7));
        assert!(dir.path().join("first").exists());
    }

    #[test]
    fn jobs_are_done_in_the_order_they_were_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        for name in ["a", "b", "c"] {
            jobs.send(Job::CreateFile(dir.path().join(name)));
        }
        for name in ["a", "b", "c"] {
            match next(&receiver) {
                Done::Changed(change) => {
                    assert!(change.path.unwrap().ends_with(name));
                }
                other => panic!("{other:?}"),
            }
        }
    }
}
