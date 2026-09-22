//! What nun remembers between sessions.
//!
//! For now that is only which regions of each file were folded. It is kept
//! in the state directory, not the config one: nobody edits it, losing it
//! costs nothing but a few folds, and it changes every time nun quits.
//!
//! Files are remembered by their absolute, resolved path. The format is one
//! line per file — the folded header lines, then a tab, then the path —
//! because it is read once at startup and written once at the end, and a
//! line-per-file text file needs no parser to be read by a person wondering
//! what is in it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How many files' folds are kept. Past this the ones not touched for
/// longest are forgotten, so the file cannot grow for ever.
const MOST_FILES: usize = 500;

/// Folds remembered from earlier sessions, and the file they live in.
#[derive(Debug, Default)]
pub struct Session {
    /// Where to write them. `None` keeps everything in memory only, which is
    /// what the tests and a system with no home directory get.
    path: Option<PathBuf>,
    /// The folded header lines of each file, by its path.
    files: BTreeMap<PathBuf, Vec<usize>>,
    /// Files in the order they were last remembered, oldest first.
    order: Vec<PathBuf>,
    /// The files this process has remembered anything about. Only these are
    /// written back over what is on disk, so a second nun quitting later
    /// does not undo what the first one saved.
    touched: BTreeSet<PathBuf>,
}

/// The one name a file is remembered by, however it was opened: absolute,
/// with symbolic links and `..` resolved. `src/main.rs` in two projects is two
/// files, and the same file from the command line and from the tree is one.
fn key(file: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(file) {
        return resolved;
    }
    // Not there (yet, or any more): resolve the folder it would be in, so the
    // name agrees with the one it will have once it is.
    let beside = file.parent().zip(file.file_name()).and_then(|(dir, name)| {
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        fs::canonicalize(dir).ok().map(|dir| dir.join(name))
    });
    beside.or_else(|| std::path::absolute(file).ok()).unwrap_or_else(|| file.to_path_buf())
}

impl Session {
    /// The session kept at `path`, or an empty one if there is none yet.
    ///
    /// A line that cannot be read is skipped rather than failing the rest:
    /// the worst a damaged file can cost is the folds on that line.
    #[must_use]
    pub fn load(path: PathBuf) -> Self {
        let (files, order) = read(&path);
        Self { path: Some(path), files, order, touched: BTreeSet::new() }
    }

    /// Where the session lives: `$XDG_STATE_HOME/nun/session`, or
    /// `~/.local/state/nun/session`. `None` with neither set.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })?;
        Some(base.join("nun").join("session"))
    }

    /// The folded header lines remembered for `file`.
    #[must_use]
    pub fn folds_of(&self, file: &Path) -> Vec<usize> {
        self.files.get(&key(file)).cloned().unwrap_or_default()
    }

    /// Remember that `file` has these header lines folded. None forgets it.
    pub fn remember(&mut self, file: &Path, headers: Vec<usize>) {
        let file = key(file);
        self.touched.insert(file.clone());
        put(&mut self.files, &mut self.order, file, headers);
    }

    /// Write the session out, if it has somewhere to go.
    ///
    /// Written to a temporary file and moved into place, so a crash half way
    /// through leaves the last session rather than half of this one.
    ///
    /// # Errors
    ///
    /// Whatever creating the directory or writing the file ran into.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = self.path.as_ref() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // What is on disk now, which another nun may have written since this
        // one started, with only this one's own files laid over it.
        let (mut files, mut order) = read(path);
        for file in &self.touched {
            let headers = self.files.get(file).cloned().unwrap_or_default();
            put(&mut files, &mut order, file.clone(), headers);
        }
        let mut text = String::new();
        for file in &order {
            let Some(headers) = files.get(file) else { continue };
            // A path that would break the one-line-per-file format is simply
            // not remembered.
            let Some(name) = file.to_str().filter(|name| !name.contains(['\n', '\t'])) else {
                continue;
            };
            let headers: Vec<String> = headers.iter().map(ToString::to_string).collect();
            text.push_str(&headers.join(","));
            text.push('\t');
            text.push_str(name);
            text.push('\n');
        }
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&temporary, text)?;
        fs::rename(&temporary, path)
    }
}

/// The files a session file holds, and the order they were remembered in.
///
/// A line that cannot be read is skipped rather than failing the rest: the
/// worst a damaged file can cost is the folds on that line. A missing file is
/// an empty session.
fn read(path: &Path) -> (BTreeMap<PathBuf, Vec<usize>>, Vec<PathBuf>) {
    let text = fs::read_to_string(path).unwrap_or_default();
    let (mut files, mut order) = (BTreeMap::new(), Vec::new());
    for line in text.lines() {
        let Some((headers, file)) = line.split_once('\t') else { continue };
        let headers: Option<Vec<usize>> =
            headers.split(',').filter(|h| !h.is_empty()).map(|h| h.parse().ok()).collect();
        if let Some(headers) = headers.filter(|headers| !headers.is_empty()) {
            put(&mut files, &mut order, PathBuf::from(file), headers);
        }
    }
    (files, order)
}

/// Record `file`'s folds as the most recent, forgetting it when there are
/// none and the oldest files when there are too many.
fn put(
    files: &mut BTreeMap<PathBuf, Vec<usize>>,
    order: &mut Vec<PathBuf>,
    file: PathBuf,
    headers: Vec<usize>,
) {
    order.retain(|known| *known != file);
    if headers.is_empty() {
        files.remove(&file);
        return;
    }
    files.insert(file.clone(), headers);
    order.push(file);
    while order.len() > MOST_FILES {
        let oldest = order.remove(0);
        files.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_survive_a_save_and_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("session");
        let mut session = Session::load(path.clone());
        session.remember(Path::new("/src/main.rs"), vec![3, 17]);
        session.remember(Path::new("/src/lib.rs"), vec![0]);
        session.save().unwrap();

        let again = Session::load(path);
        assert_eq!(again.folds_of(Path::new("/src/main.rs")), [3, 17]);
        assert_eq!(again.folds_of(Path::new("/src/lib.rs")), [0]);
        assert!(again.folds_of(Path::new("/src/other.rs")).is_empty());
    }

    #[test]
    fn a_second_nun_quitting_later_keeps_what_the_first_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session");
        let mut first = Session::load(path.clone());
        let mut second = Session::load(path.clone());
        first.remember(Path::new("/a.rs"), vec![1]);
        first.save().unwrap();
        second.remember(Path::new("/b.rs"), vec![2]);
        second.save().unwrap();

        let after = Session::load(path);
        assert_eq!(after.folds_of(Path::new("/a.rs")), [1], "not lost to the later save");
        assert_eq!(after.folds_of(Path::new("/b.rs")), [2]);
    }

    #[test]
    fn a_file_is_one_file_however_it_was_named() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("x.rs"), "").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let mut session = Session::default();
        session.remember(&dir.path().join("sub/../x.rs"), vec![4]);
        assert_eq!(session.folds_of(&dir.path().join("x.rs")), [4]);
    }

    #[test]
    fn a_file_with_nothing_folded_is_forgotten() {
        let mut session = Session::default();
        session.remember(Path::new("/a"), vec![1]);
        session.remember(Path::new("/a"), Vec::new());
        assert!(session.files.is_empty() && session.order.is_empty());
        assert!(session.touched.contains(Path::new("/a")), "forgetting is written back too");
    }

    #[test]
    fn a_damaged_line_costs_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session");
        fs::write(&path, "1,x\t/bad\nnot a line\n4,5\t/good\n").unwrap();
        let session = Session::load(path);
        assert!(session.folds_of(Path::new("/bad")).is_empty());
        assert_eq!(session.folds_of(Path::new("/good")), [4, 5]);
    }

    #[test]
    fn the_oldest_files_are_forgotten_first() {
        let mut session = Session::default();
        for n in 0..=MOST_FILES {
            session.remember(&PathBuf::from(format!("/{n}")), vec![1]);
        }
        assert!(session.folds_of(Path::new("/0")).is_empty(), "the oldest went");
        assert_eq!(session.folds_of(Path::new("/1")), [1]);
    }
}
