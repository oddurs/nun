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
use crate::project;
use crate::replace::{Recorded, Replacer, Report};
use crate::resource::{self, Carried, FileOp, Present};
use crate::rewrite::{self, Rewrite, Written};
use crate::search::{self, Match};
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
    /// Rewrite the chosen lines of the files a search found.
    ///
    /// Filesystem work, so it belongs here beside create, rename and delete
    /// rather than on the search thread — and the undo history it records is
    /// the same one every other operation records into.
    Replace {
        /// The root that was searched, which `chosen` is relative to.
        root: PathBuf,
        /// The query the hits came from. The replace matches with exactly
        /// what the search matched with.
        options: crate::grep::Options,
        /// What to put in place of each match.
        replacement: String,
        /// Which lines to change, per file, relative to the root. Only
        /// these, and only if each is still the line it was recorded from —
        /// build them with [`Recorded::of`] from the hits the panel is
        /// showing.
        chosen: Vec<(PathBuf, Vec<Recorded>)>,
        /// When the search that found them *started*. A file written to
        /// since is left alone, because what was previewed is not what is
        /// there.
        ///
        /// This is compared against the *filesystem's* idea of when the file
        /// changed, not ours, and the two are not the same ruler: a
        /// modification time has one-second resolution on HFS+ and many
        /// network mounts, two on FAT. So it catches a write comfortably
        /// after the search and misses one in the same clock second, which is
        /// why it is an early-out in front of the line check rather than the
        /// thing being relied on.
        searched_at: std::time::SystemTime,
    },
    /// Read files as text, for a caller that will work out what to write
    /// into them, and say what is at some other paths. `tag` comes back with
    /// the answer.
    Read {
        /// Which request this is, so its answer can be recognised.
        tag: u64,
        /// The files, by full path.
        paths: Vec<PathBuf>,
        /// Paths to say what is at, without reading them: where files are
        /// to be created, moved from or to, or deleted.
        probe: Vec<PathBuf>,
    },
    /// Write whole files, provided each still holds what it was read as —
    /// see [`rewrite::rewrite`]. Not recorded in the undo history: the caller
    /// takes it back by sending the same files the other way round, which
    /// gets the same check.
    Rewrite {
        /// Which request this is, so its answer can be recognised.
        tag: u64,
        /// The files, in the order to write them.
        files: Vec<Rewrite>,
    },
    /// Create, move and delete files, in order, stopping at the first that
    /// fails — see [`FsHistory::carry_out`]. Not recorded in the undo
    /// history: each comes back with its reverse, for the caller to send
    /// when it wants it taken back.
    FileOps {
        /// Which request this is, so its answer can be recognised.
        tag: u64,
        /// The operations, in the order to carry them out.
        ops: Vec<FileOp>,
    },
    /// Undo the last operation.
    Undo,
    /// Redo the last undone operation.
    Redo,
    /// List every file in the project, for the palette to search.
    ListFiles(PathBuf),
    /// Score `query` against the files listed, and answer with the best.
    Search {
        /// What the user has typed.
        query: String,
        /// How many results to send back.
        limit: usize,
        /// Which keystroke this is, so a late answer can be recognised.
        generation: u64,
    },
    /// Read some lines of some files, for showing where they are without
    /// opening them: the references to a symbol, say.
    Lines {
        /// Which request this is, so a late answer can be recognised.
        generation: u64,
        /// Each file, and the lines of it wanted, counting from zero.
        wanted: Vec<(PathBuf, Vec<u32>)>,
    },
    /// Work out what to call the project rooted here — see
    /// [`project::project_name`]. Reads git's files, so it is done here
    /// rather than wherever the name is drawn.
    ProjectName(PathBuf),
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
    /// The project's files were listed.
    Files {
        /// How many there are, so the palette can say what it is searching.
        count: usize,
    },
    /// The best matches for a query.
    Found {
        /// What was searched for.
        query: String,
        /// Which keystroke asked.
        generation: u64,
        /// The matches, best first, with the path each one is.
        results: Vec<(PathBuf, Match)>,
    },
    /// A replace ran.
    Replaced {
        /// What happened to each file, with the paths relative to the root,
        /// as the panel showed them.
        report: Report,
        /// The undoable step it became, to treat exactly like a
        /// [`Done::Changed`]. `None` when no file was written and so there is
        /// nothing to take back.
        change: Option<Change>,
    },
    /// The lines [`Job::Lines`] asked for.
    Lines {
        /// Which request they answer.
        generation: u64,
        /// Each file, and the lines of it that could be read, without their
        /// line endings. A file that could not be read is left out, and so is
        /// a line past its end.
        lines: Vec<(PathBuf, Vec<(u32, String)>)>,
    },
    /// Files were read, or could not be.
    Read {
        /// The tag the job was sent with.
        tag: u64,
        /// Each file beside its text, or why it could not be read as text.
        files: Vec<(PathBuf, Result<String, String>)>,
        /// Each path probed beside what is there.
        probed: Vec<(PathBuf, Present)>,
    },
    /// A rewrite ran, as far as it got.
    Rewritten {
        /// The tag the job was sent with.
        tag: u64,
        /// Each file beside what became of it, in the order they were given.
        files: Vec<(PathBuf, Written)>,
    },
    /// File operations ran, as far as they got.
    FileOps {
        /// The tag the job was sent with.
        tag: u64,
        /// Each operation beside what became of it, in the order given.
        ops: Vec<(FileOp, Carried)>,
    },
    /// What to call the project, from [`Job::ProjectName`].
    ProjectName {
        /// The root it was asked for, so an answer for a folder no longer
        /// open can be recognised.
        root: PathBuf,
        /// `owner/repo`, or the folder's name.
        name: String,
    },
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
            let mut worker =
                Worker { history: FsHistory::new(trash), files: Vec::new(), names: Vec::new() };
            while let Ok(first) = receiver.recv() {
                // Everything waiting is taken at once so that searches the
                // user has already typed past can be skipped: only the latest
                // one is worth the work, and the rest would answer questions
                // nobody is asking any more.
                let mut batch = vec![first];
                batch.extend(receiver.try_iter());
                let latest = batch
                    .iter()
                    .filter_map(|job| match job {
                        Job::Search { generation, .. } => Some(*generation),
                        _ => None,
                    })
                    .max();

                for job in batch {
                    if let Job::Search { generation, .. } = &job
                        && Some(*generation) != latest
                    {
                        continue;
                    }
                    report(worker.run(job));
                }
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

/// The worker's own state: the undo history, and the project's files.
struct Worker {
    history: FsHistory,
    files: Vec<PathBuf>,
    /// The same paths as strings, which is what the matcher wants.
    names: Vec<String>,
}

impl Worker {
    fn run(&mut self, job: Job) -> Done {
        let outcome = match job {
            Job::List { dir, show_ignored } => {
                let entries = list_dir(&dir, show_ignored);
                return Done::Listed { dir, entries };
            }
            Job::ListFiles(root) => {
                self.files = search::list_files(&root);
                self.names = self.files.iter().map(|path| path.display().to_string()).collect();
                return Done::Files { count: self.files.len() };
            }
            Job::Search { query, limit, generation } => {
                let results = search::search(&self.names, &query, limit)
                    .into_iter()
                    .map(|found| (self.files[found.index].clone(), found))
                    .collect();
                return Done::Found { query, generation, results };
            }
            Job::Echo(marker) => return Done::Echo(marker),
            Job::ProjectName(root) => {
                let name = project::project_name(&root);
                return Done::ProjectName { root, name };
            }
            Job::Lines { generation, wanted } => {
                let lines = wanted
                    .into_iter()
                    .filter_map(|(path, wanted)| {
                        let found = read_lines(&path, &wanted)?;
                        Some((path, found))
                    })
                    .collect();
                return Done::Lines { generation, lines };
            }
            Job::Read { tag, paths, probe } => {
                let probed = probe
                    .into_iter()
                    .map(|path| {
                        let present = resource::probe(&path);
                        (path, present)
                    })
                    .collect();
                return Done::Read { tag, files: rewrite::read_texts(&paths), probed };
            }
            Job::FileOps { tag, ops } => {
                return Done::FileOps { tag, ops: self.history.carry_out(&ops) };
            }
            Job::Rewrite { tag, files } => {
                return Done::Rewritten { tag, files: rewrite::rewrite(&files) };
            }
            Job::Replace { root, options, replacement, chosen, searched_at } => {
                let replacer = match Replacer::new(&options, &replacement) {
                    Ok(replacer) => replacer,
                    Err(error) => return Done::Failed(error),
                };
                let (report, change) = self.history.replace(&root, &replacer, &chosen, searched_at);
                return Done::Replaced { report, change };
            }
            Job::CreateFile(path) => self.history.create_file(path),
            Job::CreateDir(path) => self.history.create_dir(path),
            Job::Rename { from, name } => self.history.rename(from, &name),
            Job::MoveInto { from, dir } => self.history.move_into(from, dir),
            Job::Delete(path) => self.history.delete(path),
            Job::Undo => return step(self.history.undo(), true),
            Job::Redo => return step(self.history.redo(), false),
        };
        match outcome {
            Ok(change) => Done::Changed(change),
            Err(error) => Done::Failed(error.to_string()),
        }
    }
}

fn step(outcome: Result<Option<Change>, crate::ops::OpError>, undo: bool) -> Done {
    match outcome {
        Ok(Some(change)) => Done::Changed(change),
        Ok(None) => Done::Nothing { redo: !undo },
        Err(error) => Done::Failed(error.to_string()),
    }
}

/// Lines `wanted` of the file at `path`, each with its number. Read lossily:
/// this is for showing, and one bad byte should not hide the whole line.
fn read_lines(path: &Path, wanted: &[u32]) -> Option<Vec<(u32, String)>> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut wanted = wanted.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    let mut found = Vec::with_capacity(wanted.len());
    let mut next = wanted.iter().peekable();
    for (number, line) in (0u32..).zip(text.split('\n')) {
        let Some(&&want) = next.peek() else { break };
        if number == want {
            found.push((number, line.strip_suffix('\r').unwrap_or(line).to_string()));
            next.next();
        }
    }
    Some(found)
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

/// Shared by the test modules below.
#[cfg(test)]
mod tests_support {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    pub(super) fn worker(trash: &Path) -> (Jobs, Receiver<Done>) {
        let (sender, receiver) = mpsc::channel();
        let jobs = Jobs::new(
            trash,
            Box::new(move |done| {
                let _ = sender.send(done);
            }),
        );
        (jobs, receiver)
    }

    pub(super) fn next(receiver: &Receiver<Done>) -> Done {
        receiver.recv_timeout(Duration::from_secs(5)).expect("the worker answered")
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn lines_are_read_by_number_without_their_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "zero\r\none 😀\ntwo\n").unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        let gone = dir.path().join("gone.rs");
        jobs.send(Job::Lines {
            generation: 7,
            wanted: vec![(path.clone(), vec![1, 0, 9]), (gone, vec![0])],
        });
        match next(&receiver) {
            Done::Lines { generation, lines } => {
                assert_eq!(generation, 7);
                assert_eq!(
                    lines,
                    vec![(path, vec![(0, "zero".to_string()), (1, "one 😀".to_string())])]
                );
            }
            other => panic!("expected lines, got {other:?}"),
        }
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
    fn the_project_name_comes_back_as_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nun");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:oddurs/nun\n",
        )
        .unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        jobs.send(Job::ProjectName(root.clone()));
        assert_eq!(next(&receiver), Done::ProjectName { root, name: "oddurs/nun".into() });
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

#[cfg(test)]
mod replace_tests {
    use super::tests_support::*;
    use super::*;
    use crate::grep::{Case, Found, Grep, Hit, Options};
    use std::collections::BTreeMap;
    use std::time::{Duration, SystemTime};

    fn options(query: &str) -> Options {
        Options { query: query.into(), case: Case::Sensitive, ..Options::default() }
    }

    /// Search `root` and hand back every hit, the way the panel would hold
    /// them — so a replace in these tests is driven by the same line numbers
    /// the person clicked, not by numbers a test made up.
    fn hits(root: &Path, options: Options) -> Vec<Hit> {
        let (sender, receiver) = mpsc::channel();
        let grep = Grep::new(Box::new(move |found| {
            let _ = sender.send(found);
        }));
        grep.search(root, options, 1);
        let mut all = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(30)).expect("the worker answered") {
                Found::Hits { hits, .. } => all.extend(hits),
                Found::Done { .. } => break,
                other @ Found::Failed { .. } => panic!("{other:?}"),
            }
        }
        all.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        all
    }

    /// Every hit, grouped into the shape [`Job::Replace`] wants — which is
    /// all the panel has to do: group by path and call [`Recorded::of`].
    fn chosen(hits: &[Hit]) -> Vec<(PathBuf, Vec<Recorded>)> {
        let mut by_file: BTreeMap<PathBuf, Vec<Recorded>> = BTreeMap::new();
        for hit in hits {
            by_file.entry(hit.path.clone()).or_default().push(Recorded::of(hit));
        }
        by_file.into_iter().collect()
    }

    #[test]
    fn what_the_search_found_is_what_the_replace_writes() {
        let dir = tempfile::tempdir().unwrap();
        // One file per line-ending habit, because a replace that quietly
        // normalised a repository would be worse than the bug it fixed.
        std::fs::write(dir.path().join("unix.rs"), "let cat = 1;\nlet dog = 2;\n").unwrap();
        std::fs::write(dir.path().join("dos.rs"), "let cat = 3;\r\nlet cat = 4;\r\n").unwrap();
        std::fs::write(dir.path().join("bare.rs"), "let cat = 5;").unwrap();
        let searched_at = SystemTime::now();
        let found = hits(dir.path(), options("cat"));
        assert_eq!(found.len(), 4);

        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Replace {
            root: dir.path().to_path_buf(),
            options: options("cat"),
            replacement: "kitten".into(),
            chosen: chosen(&found),
            searched_at,
        });

        match next(&receiver) {
            Done::Replaced { report, change } => {
                assert_eq!(report.lines, 4);
                assert_eq!(report.changed(), 3);
                assert_eq!(report.skipped(), 0);
                assert_eq!(report.failed(), 0);
                assert_eq!(report.to_string(), "Changed 4 lines in 3 files");
                assert_eq!(change.unwrap().to_string(), "Replaced 4 lines in 3 files");
            }
            other => panic!("{other:?}"),
        }

        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("unix.rs"), "let kitten = 1;\nlet dog = 2;\n");
        assert_eq!(read("dos.rs"), "let kitten = 3;\r\nlet kitten = 4;\r\n");
        assert_eq!(read("bare.rs"), "let kitten = 5;");

        jobs.send(Job::Undo);
        assert!(matches!(next(&receiver), Done::Changed(change) if change.undone));
        assert_eq!(read("unix.rs"), "let cat = 1;\nlet dog = 2;\n");
        assert_eq!(read("dos.rs"), "let cat = 3;\r\nlet cat = 4;\r\n");
        assert_eq!(read("bare.rs"), "let cat = 5;");
    }

    #[test]
    fn a_line_the_search_could_only_window_is_replaced_whole() {
        // The hit for a line this long carries a window of it, and the panel
        // sends that window back without knowing it is one. Both halves agree
        // on MOST_CHARS by using the same constant, so this is the test that
        // would fail if either side ever changed its mind about the cap.
        let dir = tempfile::tempdir().unwrap();
        let filler = "abcdefghij".repeat(5_000);
        let line = format!("{filler}cat{filler}cat");
        std::fs::write(dir.path().join("bundle.js"), format!("{line}\n")).unwrap();
        let searched_at = SystemTime::now();

        let found = hits(dir.path(), options("cat"));
        assert_eq!(found.len(), 1, "one line is one hit");
        assert_eq!(found[0].text.chars().count(), crate::MOST_CHARS, "a window, not the line");
        assert_eq!(found[0].matched.len(), 1, "the second match is outside it");

        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Replace {
            root: dir.path().to_path_buf(),
            options: options("cat"),
            replacement: "kitten".into(),
            chosen: chosen(&found),
            searched_at,
        });

        match next(&receiver) {
            Done::Replaced { report, .. } => assert_eq!(report.lines, 1),
            other => panic!("{other:?}"),
        }
        let written = std::fs::read_to_string(dir.path().join("bundle.js")).unwrap();
        assert_eq!(
            written,
            format!("{filler}kitten{filler}kitten\n"),
            "both matches, including the one the window never showed"
        );
    }

    #[test]
    fn a_file_rewritten_so_the_line_still_matches_is_not_replaced() {
        // The acceptance criterion in the shape that actually threatens it,
        // end to end from real hits. The person keeps only the first of the
        // two, a formatter then reorders the file, and the file keeps its
        // modification time — which is what a one-second-granularity
        // filesystem gives for free on any write inside the same second as
        // the search. Line 1 still matches; it is simply not the line that
        // was previewed, and the line it now is was deliberately left out.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        let before = "let cat = 1;\nlet dog = 2;\nlet cat = 3;\n";
        std::fs::write(&path, before).unwrap();
        let searched_at = SystemTime::now();
        let found = hits(dir.path(), options("cat"));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].text, "let cat = 1;");

        let was = std::fs::metadata(&path).unwrap().modified().unwrap();
        let reordered = "let cat = 3;\nlet dog = 2;\nlet cat = 1;\n";
        std::fs::write(&path, reordered).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(was).unwrap();

        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Replace {
            root: dir.path().to_path_buf(),
            options: options("cat"),
            replacement: "kitten".into(),
            chosen: chosen(&found[..1]),
            searched_at,
        });

        match next(&receiver) {
            Done::Replaced { report, change } => {
                assert_eq!(report.lines, 0);
                assert_eq!(report.skipped(), 1);
                assert!(change.is_none(), "nothing written, nothing to undo");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            reordered,
            "line 1 matches the query and is not what was previewed"
        );
    }

    #[test]
    fn a_query_that_does_not_compile_comes_back_as_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::Replace {
            root: dir.path().to_path_buf(),
            options: Options { regex: true, ..options("fn (") },
            replacement: "x".into(),
            chosen: vec![(
                PathBuf::from("a.rs"),
                vec![Recorded::Whole { line: 1, text: "fn (".into() }],
            )],
            searched_at: SystemTime::now(),
        });
        match next(&receiver) {
            Done::Failed(message) => assert!(!message.is_empty()),
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn the_project_is_listed_and_then_searched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        jobs.send(Job::ListFiles(dir.path().to_path_buf()));
        assert_eq!(next(&receiver), Done::Files { count: 2 });

        jobs.send(Job::Search { query: "main".into(), limit: 10, generation: 1 });
        match next(&receiver) {
            Done::Found { query, generation, results } => {
                assert_eq!(query, "main");
                assert_eq!(generation, 1);
                assert_eq!(results.len(), 1);
                assert!(results[0].0.ends_with("main.rs"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_search_the_user_has_typed_past_is_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        jobs.send(Job::ListFiles(dir.path().to_path_buf()));
        next(&receiver);

        // Three keystrokes in a row: only the last one is worth answering.
        for generation in 1..=3 {
            jobs.send(Job::Search {
                query: "a".repeat(generation),
                limit: 10,
                generation: generation as u64,
            });
        }
        jobs.send(Job::Echo(9));

        let mut answered = Vec::new();
        loop {
            match next(&receiver) {
                Done::Found { generation, .. } => answered.push(generation),
                Done::Echo(9) => break,
                other => panic!("{other:?}"),
            }
        }
        assert!(answered.len() <= 3, "{answered:?}");
        assert!(answered.contains(&3), "the last one is always answered: {answered:?}");
    }

    #[test]
    fn work_that_is_not_a_search_is_never_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));
        for name in ["a", "b", "c"] {
            jobs.send(Job::CreateFile(dir.path().join(name)));
        }
        jobs.send(Job::Search { query: "x".into(), limit: 1, generation: 1 });
        jobs.send(Job::Search { query: "y".into(), limit: 1, generation: 2 });
        jobs.send(Job::Echo(1));

        let mut changes = 0;
        loop {
            match next(&receiver) {
                Done::Changed(_) => changes += 1,
                Done::Echo(1) => break,
                Done::Found { .. } => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(changes, 3, "every file was created");
        for name in ["a", "b", "c"] {
            assert!(dir.path().join(name).exists());
        }
    }
}

#[cfg(test)]
mod rewrite_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn files_are_read_and_rewritten_on_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "cat").unwrap();
        let (jobs, receiver) = worker(&dir.path().join(".trash"));

        let gone = dir.path().join("gone.rs");
        jobs.send(Job::Read {
            tag: 7,
            paths: vec![path.clone()],
            probe: vec![dir.path().to_path_buf(), gone.clone()],
        });
        assert_eq!(
            next(&receiver),
            Done::Read {
                tag: 7,
                files: vec![(path.clone(), Ok("cat".to_string()))],
                probed: vec![(dir.path().to_path_buf(), Present::Dir), (gone, Present::Missing)],
            }
        );

        let files = vec![Rewrite { path: path.clone(), expect: "cat".into(), text: "dog".into() }];
        jobs.send(Job::Rewrite { tag: 8, files });
        assert_eq!(
            next(&receiver),
            Done::Rewritten { tag: 8, files: vec![(path.clone(), Written::Written)] }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "dog");

        let to = dir.path().join("b.rs");
        jobs.send(Job::FileOps {
            tag: 9,
            ops: vec![FileOp::Move { from: path.clone(), to: to.clone() }],
        });
        let undo = FileOp::Move { from: to.clone(), to: path.clone() };
        assert_eq!(
            next(&receiver),
            Done::FileOps {
                tag: 9,
                ops: vec![(FileOp::Move { from: path, to: to.clone() }, Carried::Done(undo))],
            }
        );
        assert!(to.exists());
        jobs.send(Job::Undo);
        assert_eq!(next(&receiver), Done::Nothing { redo: false }, "not on the tree's undo");
    }
}
