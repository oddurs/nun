//! Noticing when something other than nun changes the project.
//!
//! The watcher reports *which directory* changed and nothing more. The tree
//! re-reads that one directory, which is the only way to get an answer that is
//! right: native event streams drop, merge and reorder events under load, and
//! reconstructing a listing from them is a bug farm. A listing is cheap.
//!
//! Directories are watched one at a time and non-recursively — the ones that
//! are expanded — so a build writing ten thousand files into a collapsed
//! `target/` costs nothing.
//!
//! All the work happens on one background thread, which owns the native
//! watcher. [`Watcher::watch`] and [`Watcher::unwatch`] post a message to it and
//! return, because registering a path with `FSEvents` restarts its stream and is
//! not something to do on the thread that draws. Events are coalesced: a burst
//! in one directory, such as a `git checkout`, is reported once, after
//! [`QUIET`] passes without another event or [`MAX_DELAY`] after the first.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long a directory must go without events before its change is reported.
pub const QUIET: Duration = Duration::from_millis(50);

/// The longest a change waits to be reported, however busy the directory is.
pub const MAX_DELAY: Duration = Duration::from_millis(250);

/// Something in a watched directory changed, or the directory could not be
/// watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    /// The directory, spelled as it was given to [`Watcher::watch`]. Pass it
    /// to `FileTree::refresh_dir`.
    pub dir: PathBuf,
    /// Set when watching `dir` failed, such as when Linux's inotify watch limit
    /// is reached. The directory is not live; say so rather than let the
    /// sidebar quietly go stale.
    pub watch_error: Option<String>,
}

/// The native watcher could not be started.
#[derive(Debug, thiserror::Error)]
#[error("cannot watch the filesystem: {0}")]
pub struct WatchError(#[from] notify::Error);

/// Watches directories and reports changes through a callback.
///
/// Dropping it stops the background thread.
pub struct Watcher {
    messages: Sender<Message>,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

enum Message {
    Watch(PathBuf),
    Unwatch(PathBuf),
    Event(notify::Result<Event>),
    Stop,
}

impl Watcher {
    /// Start watching nothing yet, reporting changes to `on_change`.
    ///
    /// `on_change` runs on the watcher's own thread. It should hand the change
    /// to whoever owns the tree — post it to the event channel — and return.
    ///
    /// # Errors
    ///
    /// [`WatchError`] if the platform watcher cannot be created, such as when
    /// the process has run out of inotify instances.
    pub fn new(on_change: Box<dyn Fn(FsChange) + Send + 'static>) -> Result<Self, WatchError> {
        let (messages, inbox) = mpsc::channel();
        let events = messages.clone();
        let native = RecommendedWatcher::new(
            move |event| {
                // Fails only once the worker has stopped, when nobody is
                // listening anyway.
                let _ = events.send(Message::Event(event));
            },
            notify::Config::default(),
        )?;
        thread::Builder::new()
            .name("nun-watch".into())
            .spawn(move || run(native, &inbox, &*on_change))
            .map_err(|error| WatchError(notify::Error::io(error)))?;
        Ok(Self { messages })
    }

    /// Start watching the entries directly inside `dir`. Returns immediately;
    /// a failure is reported as an [`FsChange`] with `watch_error` set.
    pub fn watch(&self, dir: impl Into<PathBuf>) {
        let _ = self.messages.send(Message::Watch(dir.into()));
    }

    /// Stop watching `dir`. Returns immediately.
    pub fn unwatch(&self, dir: impl Into<PathBuf>) {
        let _ = self.messages.send(Message::Unwatch(dir.into()));
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The native watcher holds a sender of its own, so the channel never
        // disconnects by itself; the worker has to be told.
        let _ = self.messages.send(Message::Stop);
    }
}

/// The worker loop: register paths, collect events, report them coalesced.
fn run(mut native: RecommendedWatcher, inbox: &Receiver<Message>, on_change: &dyn Fn(FsChange)) {
    let mut watched = Watched::default();
    let mut pending = BTreeSet::new();
    let mut since: Option<Instant> = None;
    loop {
        let message = match since {
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(first) => inbox.recv_timeout(QUIET.min(MAX_DELAY.saturating_sub(first.elapsed()))),
        };
        let quiet = matches!(message, Err(RecvTimeoutError::Timeout));
        match message {
            Ok(Message::Watch(dir)) => match native.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => watched.insert(dir),
                Err(error) => on_change(FsChange { dir, watch_error: Some(error.to_string()) }),
            },
            Ok(Message::Unwatch(dir)) => {
                watched.remove(&dir);
                let _ = native.unwatch(&dir);
            }
            Ok(Message::Event(event)) => {
                let dirs = match &event {
                    Ok(event) => watched.affected_by(event),
                    // An error from the native watcher usually means events
                    // were lost; anything could have changed.
                    Err(_) => watched.all(),
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
            for dir in std::mem::take(&mut pending) {
                on_change(FsChange { dir, watch_error: None });
            }
            since = None;
        }
    }
}

/// The watched directories, findable by either spelling of their path.
///
/// `FSEvents` reports canonical paths — `/private/var/…` for a directory watched
/// as `/var/…` — while inotify reports them as given. Keeping both spellings
/// as keys means an event maps back to the path the caller used, whichever
/// the platform sends.
#[derive(Debug, Default)]
struct Watched {
    by_spelling: HashMap<PathBuf, PathBuf>,
}

impl Watched {
    fn insert(&mut self, dir: PathBuf) {
        if let Ok(canonical) = dir.canonicalize() {
            self.by_spelling.insert(canonical, dir.clone());
        }
        self.by_spelling.insert(dir.clone(), dir);
    }

    fn remove(&mut self, dir: &Path) {
        self.by_spelling.retain(|_, given| given != dir);
    }

    fn all(&self) -> BTreeSet<PathBuf> {
        self.by_spelling.values().cloned().collect()
    }

    /// The watched directories whose listing an event may have changed.
    ///
    /// Access events are dropped: reading a directory to refresh it produces
    /// them on Linux, and reacting to those would refresh forever.
    fn affected_by(&self, event: &Event) -> BTreeSet<PathBuf> {
        if event.kind.is_access() {
            return BTreeSet::new();
        }
        if event.need_rescan() || event.paths.is_empty() {
            return self.all();
        }
        let mut dirs = BTreeSet::new();
        for path in &event.paths {
            // The entry's own directory, and the entry itself when it is a
            // watched directory that was removed or renamed.
            for candidate in [path.parent(), Some(path.as_path())].into_iter().flatten() {
                if let Some(dir) = self.lookup(candidate) {
                    dirs.insert(dir.clone());
                }
            }
        }
        dirs
    }

    fn lookup(&self, path: &Path) -> Option<&PathBuf> {
        self.by_spelling
            .get(path)
            .or_else(|| path.canonicalize().ok().and_then(|path| self.by_spelling.get(&path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, EventKind, Flag, ModifyKind};
    use std::fs;

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        paths.iter().fold(Event::new(kind), |event, path| event.add_path(path.to_path_buf()))
    }

    #[test]
    fn an_event_maps_to_the_directory_it_happened_in() {
        let temp = tempfile::tempdir().unwrap();
        let mut watched = Watched::default();
        watched.insert(temp.path().to_path_buf());
        let created = event(EventKind::Create(CreateKind::File), &[&temp.path().join("new.rs")]);
        assert_eq!(watched.affected_by(&created), BTreeSet::from([temp.path().to_path_buf()]));
    }

    #[test]
    fn a_canonical_path_maps_back_to_the_spelling_that_was_watched() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        let mut watched = Watched::default();
        watched.insert(temp.path().to_path_buf());
        let created = event(EventKind::Create(CreateKind::File), &[&canonical.join("x")]);
        assert_eq!(watched.affected_by(&created), BTreeSet::from([temp.path().to_path_buf()]));
    }

    #[test]
    fn events_outside_watched_directories_and_reads_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let mut watched = Watched::default();
        watched.insert(temp.path().to_path_buf());
        let deeper = event(EventKind::Create(CreateKind::File), &[&temp.path().join("a/b/c")]);
        assert!(watched.affected_by(&deeper).is_empty());
        let read = event(EventKind::Access(AccessKind::Any), &[&temp.path().join("x")]);
        assert!(watched.affected_by(&read).is_empty());
    }

    #[test]
    fn a_removed_watched_directory_reports_itself_and_its_parent() {
        let temp = tempfile::tempdir().unwrap();
        let sub = temp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let mut watched = Watched::default();
        watched.insert(temp.path().to_path_buf());
        watched.insert(sub.clone());
        let renamed = event(EventKind::Modify(ModifyKind::Any), &[&sub]);
        assert_eq!(
            watched.affected_by(&renamed),
            BTreeSet::from([temp.path().to_path_buf(), sub.clone()])
        );
        watched.remove(&sub);
        assert_eq!(watched.all(), BTreeSet::from([temp.path().to_path_buf()]));
    }

    #[test]
    fn a_rescan_reports_every_watched_directory() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut watched = Watched::default();
        watched.insert(a.path().to_path_buf());
        watched.insert(b.path().to_path_buf());
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(watched.affected_by(&rescan).len(), 2);
    }

    /// Against the real platform watcher. Files are created repeatedly until
    /// a change arrives, because registration is asynchronous and `FSEvents` in
    /// particular can take a moment to start delivering.
    #[test]
    fn creating_a_file_in_a_watched_directory_reports_that_directory() {
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(Box::new(move |change| {
            let _ = tx.send(change);
        }))
        .unwrap();
        watcher.watch(temp.path());

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut attempt = 0;
        let change = loop {
            assert!(Instant::now() < deadline, "no change reported within 20 seconds");
            attempt += 1;
            fs::write(temp.path().join(format!("file-{attempt}.rs")), "").unwrap();
            if let Ok(change) = rx.recv_timeout(Duration::from_millis(500)) {
                break change;
            }
        };
        assert_eq!(change, FsChange { dir: temp.path().to_path_buf(), watch_error: None });
    }

    #[test]
    fn watching_a_missing_directory_reports_the_failure() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(Box::new(move |change| {
            let _ = tx.send(change);
        }))
        .unwrap();
        watcher.watch(&missing);
        let change = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(change.dir, missing);
        assert!(change.watch_error.is_some());
    }
}
