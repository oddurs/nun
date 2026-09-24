//! Noticing which files change on disk, anywhere under a few folders, for
//! whoever needs to hear about each file — a language server, which asked
//! for them.
//!
//! [`crate::Watcher`] answers a narrower question, *which directory* changed,
//! for the directories the tree has expanded. This answers *which files* were
//! created, changed or deleted anywhere under a folder, including ones nobody
//! has looked at: a `git checkout`, another editor's save, a folder moved in a
//! file manager.
//!
//! It works the same way underneath. Native events say only where to look;
//! what changed is worked out by listing that directory again and comparing
//! the listing with the last one, name by name, size and modification time.
//! Events drop, merge and reorder under load, and name a move as two paths
//! with no way to tell which is which, but a listing is always right — and it
//! is also the only way to hear about the files inside a folder that was moved
//! in whole, for which the platform reports the folder alone.
//!
//! What the ignore rules leave out is not watched, the same rule the tree and
//! the project search follow: a `target/` or a `node_modules/` holds more files
//! than the rest of a project put together, is rewritten by every build, and
//! no server wants to hear about it. How much is spent on the rest depends on
//! the platform:
//!
//! - where a whole tree can be watched for the price of one watch — `FSEvents`
//!   on macOS, `ReadDirectoryChangesW` on Windows — it is, recursively, and
//!   events in ignored directories are dropped on arrival;
//! - elsewhere — inotify on Linux, where a recursive watch is really one per
//!   directory — each directory that is not ignored is watched on its own, so
//!   an ignored one costs nothing at all.
//!
//! Everything happens on one background thread, and changes are reported in
//! batches: after [`QUIET`] passes without another event, or [`MAX_DELAY`]
//! after the first, whichever comes sooner.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ignore::WalkBuilder;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _, WatcherKind};

use crate::watch::WatchError;

/// How long the folders must go without events before a batch is reported.
pub const QUIET: Duration = Duration::from_millis(100);

/// The longest a change waits to be reported, however busy the folders are.
pub const MAX_DELAY: Duration = Duration::from_millis(500);

/// What happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// It is there now and was not before: new, or moved here.
    Created,
    /// Its contents changed.
    Changed,
    /// It was there and is not now: deleted, or moved away.
    Deleted,
}

/// One file, or folder, that changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskChange {
    /// Where, spelled from the folder's resolved path: through no symbolic
    /// link.
    pub path: PathBuf,
    /// What happened to it.
    pub kind: ChangeKind,
}

/// What a [`DiskWatcher`] has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskNews {
    /// These changed, in one batch.
    Changed(Vec<DiskChange>),
    /// Part of a folder could not be watched, such as when Linux's inotify
    /// watch limit is reached; changes there will not be noticed.
    Failed(String),
}

/// Watches whole folders and reports every file that changes in them.
///
/// Dropping it stops the background thread.
pub struct DiskWatcher {
    messages: Sender<Message>,
}

impl fmt::Debug for DiskWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiskWatcher").finish_non_exhaustive()
    }
}

enum Message {
    Folders(Vec<PathBuf>),
    Event(notify::Result<Event>),
    Stop,
}

/// How the native watcher is asked to cover a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One recursive watch on the folder.
    Recursive,
    /// One watch on each directory in it that is not ignored.
    EachDir,
}

impl DiskWatcher {
    /// Start watching nothing yet, reporting news to `report`.
    ///
    /// `report` runs on the watcher's own thread. It should post the news to
    /// the editor's channel and return.
    ///
    /// # Errors
    ///
    /// [`WatchError`] if the platform watcher cannot be created.
    pub fn new(report: Box<dyn Fn(DiskNews) + Send + 'static>) -> Result<Self, WatchError> {
        let mode = match RecommendedWatcher::kind() {
            WatcherKind::Fsevent | WatcherKind::ReadDirectoryChangesWatcher => Mode::Recursive,
            _ => Mode::EachDir,
        };
        Self::with_mode(mode, report)
    }

    fn with_mode(mode: Mode, report: Box<dyn Fn(DiskNews) + Send>) -> Result<Self, WatchError> {
        let (messages, inbox) = mpsc::channel();
        let events = messages.clone();
        let native = RecommendedWatcher::new(
            move |event| {
                // Fails only once the worker has stopped.
                let _ = events.send(Message::Event(event));
            },
            notify::Config::default(),
        )?;
        thread::Builder::new()
            .name("nun-disk".into())
            .spawn(move || run(native, mode, &inbox, &*report))
            .map_err(|error| WatchError::from(notify::Error::io(error)))?;
        Ok(Self { messages })
    }

    /// Watch exactly these folders, and everything under them, from now on.
    /// A folder inside another is covered by the outer one. Returns at once;
    /// the folders are read on the watcher's thread, and what is in them when
    /// they are is where changes are counted from.
    pub fn watch(&self, folders: Vec<PathBuf>) {
        let _ = self.messages.send(Message::Folders(folders));
    }
}

impl Drop for DiskWatcher {
    fn drop(&mut self) {
        // The native watcher holds a sender of its own, so the worker has to
        // be told.
        let _ = self.messages.send(Message::Stop);
    }
}

fn run(
    mut native: RecommendedWatcher,
    mode: Mode,
    inbox: &Receiver<Message>,
    report: &dyn Fn(DiskNews),
) {
    let mut tracker = Tracker::new(mode);
    let mut pending = BTreeSet::new();
    let mut since: Option<Instant> = None;
    loop {
        let message = match since {
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(first) => inbox.recv_timeout(QUIET.min(MAX_DELAY.saturating_sub(first.elapsed()))),
        };
        let quiet = matches!(message, Err(RecvTimeoutError::Timeout));
        match message {
            Ok(Message::Folders(folders)) => tracker.set_folders(&folders),
            Ok(Message::Event(event)) => {
                let dirs = match &event {
                    Ok(event) => tracker.affected_by(event),
                    // Events were probably lost; anything could have changed.
                    Err(_) => tracker.all(),
                };
                if !dirs.is_empty() {
                    pending.extend(dirs);
                    since.get_or_insert_with(Instant::now);
                }
            }
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if quiet || since.is_some_and(|first| first.elapsed() >= MAX_DELAY) {
            let mut changes = Vec::new();
            for dir in std::mem::take(&mut pending) {
                changes.extend(tracker.relist(&dir));
            }
            since = None;
            if !changes.is_empty() {
                report(DiskNews::Changed(changes));
            }
        }
        // Directories that turned up need watching, and ones that went need
        // it no more.
        for order in std::mem::take(&mut tracker.orders) {
            let failed = match order {
                Order::Watch(dir, recursive) => native.watch(&dir, recursive).err(),
                Order::Unwatch(dir) => {
                    let _ = native.unwatch(&dir);
                    None
                }
            };
            if let Some(error) = failed {
                report(DiskNews::Failed(error.to_string()));
            }
        }
    }
}

/// What is known about one entry of a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stat {
    Dir,
    File { len: u64, modified: Option<SystemTime> },
}

/// A directory's entries, by name, as last read.
type Listing = BTreeMap<OsString, Stat>;

/// Something for the native watcher to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Order {
    Watch(PathBuf, RecursiveMode),
    Unwatch(PathBuf),
}

/// The folders watched and every directory in them, as last read. Touches
/// the disk, but never the native watcher: what it wants done to that goes
/// in `orders`, so it can be tested without one.
#[derive(Debug)]
struct Tracker {
    mode: Mode,
    /// Resolved, and none inside another.
    folders: BTreeSet<PathBuf>,
    dirs: BTreeMap<PathBuf, Listing>,
    orders: Vec<Order>,
}

impl Tracker {
    fn new(mode: Mode) -> Self {
        Self { mode, folders: BTreeSet::new(), dirs: BTreeMap::new(), orders: Vec::new() }
    }

    fn set_folders(&mut self, folders: &[PathBuf]) {
        let mut wanted: Vec<PathBuf> =
            folders.iter().filter_map(|folder| fs::canonicalize(folder).ok()).collect();
        wanted.sort();
        let mut outermost = BTreeSet::new();
        for folder in wanted {
            if !outermost.iter().any(|outer: &PathBuf| folder.starts_with(outer)) {
                outermost.insert(folder);
            }
        }
        for gone in self.folders.difference(&outermost).cloned().collect::<Vec<_>>() {
            let _ = self.forget(&gone);
            if self.mode == Mode::Recursive {
                self.orders.push(Order::Unwatch(gone));
            }
        }
        for new in outermost.difference(&self.folders).cloned().collect::<Vec<_>>() {
            if self.mode == Mode::Recursive {
                self.orders.push(Order::Watch(new.clone(), RecursiveMode::Recursive));
            }
            // What is there to begin with is where changes count from.
            let _ = self.walk(&new);
        }
        self.folders = outermost;
    }

    fn all(&self) -> BTreeSet<PathBuf> {
        self.dirs.keys().cloned().collect()
    }

    /// The directories whose listing an event may have changed: the one each
    /// path is in, and the path itself when it is a directory.
    fn affected_by(&self, event: &Event) -> BTreeSet<PathBuf> {
        if event.kind.is_access() {
            return BTreeSet::new();
        }
        if event.need_rescan() || event.paths.is_empty() {
            return self.all();
        }
        let mut dirs = BTreeSet::new();
        for path in &event.paths {
            for candidate in [path.parent(), Some(path.as_path())].into_iter().flatten() {
                if self.dirs.contains_key(candidate) {
                    dirs.insert(candidate.to_path_buf());
                }
            }
        }
        dirs
    }

    /// Read everything under `dir`, which is new, and say it was all created.
    fn walk(&mut self, dir: &Path) -> Vec<DiskChange> {
        let mut created = Vec::new();
        self.dirs.insert(dir.to_path_buf(), Listing::new());
        self.watch_one(dir);
        for entry in walker(dir, None).filter_map(Result::ok).filter(|entry| entry.depth() > 0) {
            let path = entry.path().to_path_buf();
            let Some(stat) = stat_of(&entry) else { continue };
            if let Some(listing) = path.parent().and_then(|parent| self.dirs.get_mut(parent)) {
                listing.insert(entry.file_name().to_os_string(), stat);
            }
            if stat == Stat::Dir {
                self.dirs.insert(path.clone(), Listing::new());
                self.watch_one(&path);
            }
            created.push(DiskChange { path, kind: ChangeKind::Created });
        }
        created
    }

    fn watch_one(&mut self, dir: &Path) {
        if self.mode == Mode::EachDir {
            self.orders.push(Order::Watch(dir.to_path_buf(), RecursiveMode::NonRecursive));
        }
    }

    /// Stop tracking `dir` and everything under it, and say what was there.
    fn forget(&mut self, dir: &Path) -> Vec<DiskChange> {
        let under: Vec<PathBuf> = self
            .dirs
            .range(dir.to_path_buf()..)
            .map(|(path, _)| path)
            .take_while(|path| path.starts_with(dir))
            .cloned()
            .collect();
        let mut gone = Vec::new();
        for path in under {
            let Some(listing) = self.dirs.remove(&path) else { continue };
            if self.mode == Mode::EachDir {
                self.orders.push(Order::Unwatch(path.clone()));
            }
            for (name, stat) in listing {
                if stat != Stat::Dir {
                    gone.push(DiskChange { path: path.join(name), kind: ChangeKind::Deleted });
                }
            }
            if path != dir {
                gone.push(DiskChange { path, kind: ChangeKind::Deleted });
            }
        }
        gone
    }

    /// Read `dir` again and say how it differs from the last reading.
    fn relist(&mut self, dir: &Path) -> Vec<DiskChange> {
        let Some(old) = self.dirs.get(dir) else { return Vec::new() };
        let Some(new) = list(dir) else {
            // Gone, and whatever was under it with it.
            let mut gone = self.forget(dir);
            let name = dir.file_name().map(OsString::from);
            let parent = dir.parent().and_then(|parent| self.dirs.get_mut(parent));
            if let (Some(listing), Some(name)) = (parent, name) {
                listing.remove(&name);
            }
            gone.push(DiskChange { path: dir.to_path_buf(), kind: ChangeKind::Deleted });
            return gone;
        };
        let old = old.clone();
        let mut changes = Vec::new();
        for (name, before) in &old {
            let path = dir.join(name);
            match new.get(name) {
                Some(now) if now == before => {}
                Some(Stat::File { .. }) if *before != Stat::Dir => {
                    changes.push(DiskChange { path, kind: ChangeKind::Changed });
                }
                // A file where a folder was, or the other way round.
                Some(_) => changes.extend(self.remove(&path, *before)),
                None => {
                    let deleted = self.remove(&path, *before);
                    // Still there but no longer listed: an ignore rule covers
                    // it now. Not deleted; only no longer watched.
                    if fs::symlink_metadata(&path).is_err() {
                        changes.extend(deleted);
                    }
                }
            }
        }
        for (name, now) in &new {
            let path = dir.join(name);
            let fresh =
                old.get(name).is_none_or(|before| (*before == Stat::Dir) != (*now == Stat::Dir));
            if !fresh {
                continue;
            }
            changes.push(DiskChange { path: path.clone(), kind: ChangeKind::Created });
            if *now == Stat::Dir {
                changes.extend(self.walk(&path));
            }
        }
        self.dirs.insert(dir.to_path_buf(), new);
        changes
    }

    /// Stop tracking an entry that was `stat`, and say it was deleted.
    fn remove(&mut self, path: &Path, stat: Stat) -> Vec<DiskChange> {
        let mut gone = if stat == Stat::Dir { self.forget(path) } else { Vec::new() };
        gone.push(DiskChange { path: path.to_path_buf(), kind: ChangeKind::Deleted });
        gone
    }
}

/// The ignore rules' walk: hidden files included, `.git` never.
fn walker(dir: &Path, depth: Option<usize>) -> ignore::Walk {
    WalkBuilder::new(dir)
        .max_depth(depth)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
}

fn stat_of(entry: &ignore::DirEntry) -> Option<Stat> {
    if entry.file_type()?.is_dir() {
        return Some(Stat::Dir);
    }
    let metadata = entry.metadata().ok()?;
    Some(Stat::File { len: metadata.len(), modified: metadata.modified().ok() })
}

/// The entries of `dir` that no ignore rule leaves out. `None` when it is not
/// a directory any more.
fn list(dir: &Path) -> Option<Listing> {
    if !fs::metadata(dir).is_ok_and(|metadata| metadata.is_dir()) {
        return None;
    }
    Some(
        walker(dir, Some(1))
            .filter_map(Result::ok)
            .filter(|entry| entry.depth() == 1)
            .filter_map(|entry| Some((entry.file_name().to_os_string(), stat_of(&entry)?)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, EventKind};

    fn kinds(changes: &[DiskChange], root: &Path) -> Vec<(String, ChangeKind)> {
        let mut named: Vec<(String, ChangeKind)> = changes
            .iter()
            .map(|change| {
                let path = change.path.strip_prefix(root).unwrap_or(&change.path);
                (path.display().to_string(), change.kind)
            })
            .collect();
        named.sort_by(|a, b| a.0.cmp(&b.0));
        named
    }

    /// A folder with `src/main.rs`, and a tracker watching it, resolved.
    fn project(mode: Mode) -> (tempfile::TempDir, PathBuf, Tracker) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let mut tracker = Tracker::new(mode);
        tracker.set_folders(&[temp.path().to_path_buf()]);
        (temp, root, tracker)
    }

    #[test]
    fn a_new_changed_and_deleted_file_are_each_told_apart() {
        let (_temp, root, mut tracker) = project(Mode::Recursive);
        let src = root.join("src");
        fs::write(src.join("lib.rs"), "").unwrap();
        fs::write(src.join("main.rs"), "fn main() { changed(); }").unwrap();
        assert_eq!(
            kinds(&tracker.relist(&src), &root),
            [
                ("src/lib.rs".into(), ChangeKind::Created),
                ("src/main.rs".into(), ChangeKind::Changed)
            ]
        );
        assert!(tracker.relist(&src).is_empty(), "nothing new the second time");
        fs::remove_file(src.join("lib.rs")).unwrap();
        assert_eq!(
            kinds(&tracker.relist(&src), &root),
            [("src/lib.rs".into(), ChangeKind::Deleted)]
        );
    }

    #[test]
    fn a_file_moved_is_deleted_where_it_was_and_created_where_it_is() {
        let (_temp, root, mut tracker) = project(Mode::Recursive);
        fs::create_dir(root.join("src/net")).unwrap();
        let _ = tracker.relist(&root.join("src"));
        fs::rename(root.join("src/main.rs"), root.join("src/net/mod.rs")).unwrap();
        let mut changes = tracker.relist(&root.join("src"));
        changes.extend(tracker.relist(&root.join("src/net")));
        assert_eq!(
            kinds(&changes, &root),
            [
                ("src/main.rs".into(), ChangeKind::Deleted),
                ("src/net/mod.rs".into(), ChangeKind::Created)
            ]
        );
    }

    #[test]
    fn a_folder_moved_in_whole_reports_every_file_in_it() {
        let (temp, root, mut tracker) = project(Mode::EachDir);
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("net");
        fs::create_dir_all(outside.join("tcp")).unwrap();
        fs::write(outside.join("mod.rs"), "").unwrap();
        fs::write(outside.join("tcp/mod.rs"), "").unwrap();
        fs::rename(&outside, root.join("src/net")).unwrap();
        assert_eq!(
            kinds(&tracker.relist(&root.join("src")), &root),
            [
                ("src/net".into(), ChangeKind::Created),
                ("src/net/mod.rs".into(), ChangeKind::Created),
                ("src/net/tcp".into(), ChangeKind::Created),
                ("src/net/tcp/mod.rs".into(), ChangeKind::Created),
            ]
        );
        assert!(
            tracker
                .orders
                .contains(&Order::Watch(root.join("src/net/tcp"), RecursiveMode::NonRecursive)),
            "each new directory is watched: {:?}",
            tracker.orders
        );

        fs::rename(root.join("src"), elsewhere.path().join("src")).unwrap();
        tracker.orders.clear();
        assert_eq!(
            kinds(&tracker.relist(&root), &root),
            [
                ("src".into(), ChangeKind::Deleted),
                ("src/main.rs".into(), ChangeKind::Deleted),
                ("src/net".into(), ChangeKind::Deleted),
                ("src/net/mod.rs".into(), ChangeKind::Deleted),
                ("src/net/tcp".into(), ChangeKind::Deleted),
                ("src/net/tcp/mod.rs".into(), ChangeKind::Deleted),
            ]
        );
        assert!(tracker.orders.contains(&Order::Unwatch(root.join("src/net"))));
        assert_eq!(tracker.all(), BTreeSet::from([root.clone()]));
        drop(temp);
    }

    #[test]
    fn what_the_ignore_rules_leave_out_is_never_read_or_watched() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::write(root.join(".ignore"), "target/\n").unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".eslintrc"), "").unwrap();
        let mut tracker = Tracker::new(Mode::EachDir);
        tracker.set_folders(std::slice::from_ref(&root));
        assert_eq!(tracker.all(), BTreeSet::from([root.clone()]));
        assert_eq!(tracker.orders, [Order::Watch(root.clone(), RecursiveMode::NonRecursive)]);
        let built =
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("target/debug/nun"));
        assert!(
            tracker.affected_by(&built).is_empty(),
            "an ignored directory's events are dropped"
        );

        fs::create_dir(root.join("target/release")).unwrap();
        fs::write(root.join("target/release/nun"), "").unwrap();
        assert!(tracker.relist(&root).is_empty());
        // A hidden file is watched; a newly ignored one is let go quietly,
        // for it was not deleted.
        fs::write(root.join(".eslintrc"), "{}").unwrap();
        assert_eq!(
            kinds(&tracker.relist(&root), &root),
            [(".eslintrc".into(), ChangeKind::Changed)]
        );
        fs::write(root.join(".ignore"), "target/\n.eslintrc\n").unwrap();
        assert_eq!(kinds(&tracker.relist(&root), &root), [(".ignore".into(), ChangeKind::Changed)]);
    }

    #[test]
    fn folders_are_resolved_and_one_inside_another_is_covered_by_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real/a.rs"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let mut tracker = Tracker::new(Mode::Recursive);
        tracker.set_folders(&[root.join("real"), root.clone()]);
        assert_eq!(tracker.folders, BTreeSet::from([root.clone()]));
        #[cfg(unix)]
        {
            tracker.set_folders(&[root.join("link")]);
            assert_eq!(tracker.folders, BTreeSet::from([root.join("real")]));
            assert!(tracker.orders.contains(&Order::Unwatch(root.clone())));
            assert_eq!(tracker.all(), BTreeSet::from([root.join("real")]));
            fs::write(root.join("real/b.rs"), "").unwrap();
            assert_eq!(
                tracker.relist(&root.join("real")),
                [DiskChange { path: root.join("real/b.rs"), kind: ChangeKind::Created }],
                "spelled through no link"
            );
        }
    }

    /// Against the real platform watcher, both ways it can be used. Files are
    /// written until news arrives, because registration is asynchronous.
    #[test]
    fn a_file_created_outside_is_reported_by_the_real_watcher() {
        for mode in [Mode::Recursive, Mode::EachDir] {
            let temp = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(temp.path()).unwrap();
            fs::create_dir(root.join("src")).unwrap();
            let (tx, rx) = mpsc::channel();
            let watcher = DiskWatcher::with_mode(
                mode,
                Box::new(move |news| {
                    let _ = tx.send(news);
                }),
            )
            .unwrap();
            watcher.watch(vec![temp.path().to_path_buf()]);
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut attempt = 0;
            let changes = loop {
                assert!(Instant::now() < deadline, "{mode:?}: nothing reported within 20 seconds");
                attempt += 1;
                fs::write(root.join(format!("src/new-{attempt}.rs")), "").unwrap();
                if let Ok(DiskNews::Changed(changes)) = rx.recv_timeout(Duration::from_secs(1)) {
                    break changes;
                }
            };
            assert!(
                changes.contains(&DiskChange {
                    path: root.join(format!("src/new-{attempt}.rs")),
                    kind: ChangeKind::Created
                }),
                "{mode:?}: {changes:?}"
            );
        }
    }
}
