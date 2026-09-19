//! A lazily loaded, virtualised tree over a directory.
//!
//! Two decisions carry the whole design.
//!
//! **Read a directory only when it is expanded, and only one level of it.**
//! Opening a tree reads the root's entries and nothing else, so the cost of
//! opening a repository is the cost of listing its top level, however much is
//! under `target/` or `node_modules/`. Each read goes through
//! [`ignore::WalkBuilder`] with a depth of one rather than through
//! [`std::fs::read_dir`] alone, so `.gitignore` files in every parent, `.ignore`
//! files, `.git/info/exclude` and the global excludes file all still apply to a
//! directory five levels down.
//!
//! **Keep the visible rows flat.** Every expand, collapse and refresh rebuilds a
//! `Vec<Row>` of exactly what is on screen when scrolled all the way through,
//! so the sidebar draws a window by slicing it. Rendering is then proportional
//! to the height of the sidebar, and the rebuild — proportional to the number
//! of visible rows — only happens when the shape of the tree changes.
//!
//! `.gitignore` is honoured only inside a git repository, the same as git and
//! ripgrep. Without that boundary a stray `.gitignore` in a home directory would
//! hide files in every project beneath it.
//!
//! Paths in and out of the tree are spelled the way the root was given: a row's
//! path is the root joined with names read from disk. Nothing is canonicalised,
//! so a tree opened on a symlinked directory stays inside the name it was given.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;

use crate::order::compare_names;

/// What sort of entry a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A directory, which can be expanded.
    Dir,
    /// A regular file, or anything else that is not a directory or a link.
    File,
    /// A symbolic link. Links are not followed, so a link to a directory is
    /// shown as a leaf: following them invites cycles and trees that are
    /// bigger than the repository they are part of.
    Symlink,
}

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// How many directories deep, with the root's own entries at zero.
    pub depth: usize,
    /// The file name for display. A name that is not valid UTF-8 is shown with
    /// replacement characters; `path` keeps the real one.
    pub name: String,
    /// The full path, the tree's root joined with each name down to this one.
    pub path: PathBuf,
    /// What sort of entry it is.
    pub kind: Kind,
    /// Whether this is a directory that is currently expanded.
    pub expanded: bool,
    /// Whether ignore rules or the hidden-file rule would have left it out.
    /// Only ever true while ignored entries are being shown, so the UI can dim
    /// them.
    pub ignored: bool,
    /// Why this directory's contents could not be read the last time they were
    /// asked for, such as a permission error.
    pub error: Option<String>,
}

/// A lazily loaded tree of a directory, flattened into rows for drawing.
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    top: Dir,
    show_ignored: bool,
    rows: Vec<Row>,
}

#[derive(Debug)]
struct Node {
    name: OsString,
    kind: Kind,
    ignored: bool,
    /// Present exactly when `kind` is [`Kind::Dir`].
    dir: Option<Dir>,
}

#[derive(Debug, Default)]
struct Dir {
    expanded: bool,
    /// `None` until the directory is first read. Kept when it is collapsed, so
    /// expanding it again brings back whatever was expanded inside it.
    children: Option<Vec<Node>>,
    /// Set when this directory's listing has been asked for and has not
    /// arrived yet. Reading a directory can be slow — a network mount, a
    /// folder with a hundred thousand entries — so the tree never reads one
    /// itself; it says which it wants and takes the answer when it comes.
    wanted: bool,
    error: Option<String>,
}

/// One entry of a directory listing, as the tree wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its name, exactly as the filesystem spells it.
    pub name: OsString,
    /// What it is.
    pub kind: Kind,
    /// Whether an ignore rule or the hidden-file rule excludes it.
    pub ignored: bool,
}

impl FileTree {
    /// Open a tree on `root`, reading its top level and nothing below it.
    ///
    /// Never fails: if the root cannot be read the tree is empty and
    /// [`FileTree::error`] on the root says why.
    #[must_use]
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            top: Dir { expanded: true, wanted: true, ..Dir::default() },
            show_ignored: false,
            rows: Vec::new(),
        }
    }

    /// The directories whose listings the tree is waiting for.
    ///
    /// Ask for each one — off this thread — and hand the answer back to
    /// [`FileTree::apply_listing`]. Asking twice is harmless: a directory
    /// stays in this list until its listing arrives.
    #[must_use]
    pub fn wanted(&self) -> Vec<PathBuf> {
        let mut wanted = Vec::new();
        collect_wanted(&self.top, &self.root, &mut wanted);
        wanted
    }

    /// Read everything the tree is waiting for, here and now.
    ///
    /// **Blocking.** For tests and one-shot tools. The editor must not call
    /// this: reading a directory can take as long as the filesystem likes, and
    /// nothing that draws may wait on it. There, [`crate::Jobs`] does the
    /// reading and [`FileTree::apply_listing`] takes the answers.
    pub fn load_blocking(&mut self) {
        // A listing can expose directories that were already expanded, so this
        // goes round until nothing is left waiting.
        while let Some(dir) = self.wanted().into_iter().next() {
            let listing = list_dir(&dir, self.show_ignored);
            self.apply_listing(&dir, listing);
        }
    }

    /// Whether anything is still waiting to be listed.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        !self.wanted().is_empty()
    }

    /// Take the listing of `dir`, or why it could not be listed.
    ///
    /// Entries that survive keep their state, so an expanded subdirectory
    /// stays expanded with its contents intact. Returns whether the tree
    /// changed; a listing for a directory nobody is waiting for is ignored,
    /// which is what makes a late answer harmless.
    pub fn apply_listing(&mut self, dir: &Path, listing: Result<Vec<Entry>, String>) -> bool {
        let Some(node) = self.dir_mut(dir) else { return false };
        node.wanted = false;
        match listing {
            Ok(entries) => node.take(entries),
            Err(error) => {
                node.children.get_or_insert_default();
                node.error = Some(error);
            }
        }
        self.rebuild();
        true
    }

    /// The directory the tree was opened on.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every visible row, top to bottom. Slice it to draw a window.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether ignored and hidden entries are listed.
    #[must_use]
    pub const fn show_ignored(&self) -> bool {
        self.show_ignored
    }

    /// List ignored and hidden entries too, or stop listing them.
    ///
    /// Re-reads every directory that has been loaded so far, because which
    /// entries exist in the model depends on it. `.git` is never listed.
    pub fn set_show_ignored(&mut self, show: bool) {
        if show == self.show_ignored {
            return;
        }
        self.show_ignored = show;
        self.top.want_all();
    }

    /// The row showing `path`, if it is visible.
    ///
    /// A linear scan of the visible rows: this answers clicks and reveals, not
    /// something done per frame.
    #[must_use]
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    /// Why the directory at `path` could not be read, if it could not.
    #[must_use]
    pub fn error(&self, path: &Path) -> Option<&str> {
        self.dir(path)?.error.as_deref()
    }

    /// Expand the directory row at `index` if it is collapsed, or collapse it if
    /// it is expanded.
    ///
    /// Returns whether anything changed; toggling a file or an index past the
    /// end does nothing.
    pub fn toggle(&mut self, index: usize) -> bool {
        let Some(row) = self.rows.get(index) else { return false };
        if row.kind != Kind::Dir {
            return false;
        }
        let path = row.path.clone();
        if row.expanded { self.collapse(&path) } else { self.expand(&path) }
    }

    /// Expand the directory at `path`, reading its entries fresh from disk.
    ///
    /// Children that were expanded before it was collapsed stay expanded.
    /// Returns whether `path` names a directory the tree knows about.
    pub fn expand(&mut self, path: &Path) -> bool {
        let Some(dir) = self.dir_mut(path) else { return false };
        if !dir.expanded || dir.children.is_none() {
            dir.wanted = dir.wanted || dir.children.is_none();
            dir.expanded = true;
            self.rebuild();
        }
        true
    }

    /// Collapse the directory at `path`. The root cannot be collapsed.
    ///
    /// Returns whether anything changed.
    pub fn collapse(&mut self, path: &Path) -> bool {
        if path == self.root {
            return false;
        }
        let Some(dir) = self.dir_mut(path) else { return false };
        if !dir.expanded {
            return false;
        }
        dir.expanded = false;
        self.rebuild();
        true
    }

    /// Expand every ancestor of `path` so that it is visible, and return its
    /// row.
    ///
    /// Returns the row when the tree already knows the path. When a directory
    /// on the way has not been listed yet, it asks for that listing and
    /// returns `None`; call it again as listings arrive. A path that the
    /// listings do not contain — created since, or never there — stays `None`,
    /// so revealing something new means refreshing its directory first.
    /// `None` for a path outside the root.
    pub fn reveal(&mut self, path: &Path) -> Option<usize> {
        let relative = path.strip_prefix(&self.root).ok()?.to_path_buf();
        let mut ancestor = self.root.clone();
        let mut names = normal_components(&relative)?;
        names.pop();
        for name in names {
            ancestor.push(name);
            let dir = self.dir_mut(&ancestor)?;
            if !dir.expanded || dir.children.is_none() {
                dir.wanted = dir.wanted || dir.children.is_none();
                dir.expanded = true;
            }
        }
        self.rebuild();
        self.row_of(path)
    }

    /// Read the directory at `path` again, as after a watcher reports a change
    /// in it.
    ///
    /// Entries that survive keep their state, so an expanded subdirectory stays
    /// expanded with its contents intact. Returns whether anything was read: a
    /// directory the tree has never loaded is left alone, because it will be
    /// read fresh when it is expanded.
    pub fn refresh_dir(&mut self, path: &Path) -> bool {
        let Some(dir) = self.dir_mut(path) else { return false };
        if dir.children.is_none() {
            return false;
        }
        dir.wanted = true;
        true
    }

    /// The directories whose contents are on screen: the root, and every
    /// expanded directory whose ancestors are all expanded too.
    ///
    /// These are the directories worth watching. A loaded directory hidden
    /// under a collapsed one is not in the list; it is re-read when it is next
    /// expanded, so missing its changes costs nothing.
    #[must_use]
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.clone()];
        dirs.extend(self.rows.iter().filter(|row| row.expanded).map(|row| row.path.clone()));
        dirs
    }

    fn dir(&self, path: &Path) -> Option<&Dir> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut dir = &self.top;
        for name in normal_components(relative)? {
            let node = dir.children.as_ref()?.iter().find(|node| node.name == name)?;
            dir = node.dir.as_ref()?;
        }
        Some(dir)
    }

    fn dir_mut(&mut self, path: &Path) -> Option<&mut Dir> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut dir = &mut self.top;
        for name in normal_components(relative)? {
            let node = dir.children.as_mut()?.iter_mut().find(|node| node.name == name)?;
            dir = node.dir.as_mut()?;
        }
        Some(dir)
    }

    fn rebuild(&mut self) {
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        push_rows(&mut rows, &self.top, &self.root, 0);
        self.rows = rows;
    }
}

impl Dir {
    /// Take a fresh listing, carrying over the state of any subdirectory that
    /// is still there.
    fn take(&mut self, entries: Vec<Entry>) {
        let mut previous: HashMap<OsString, Dir> = self
            .children
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|node| Some((node.name, node.dir?)))
            .collect();

        let fresh = entries
            .into_iter()
            .map(|entry| {
                let dir = (entry.kind == Kind::Dir)
                    .then(|| previous.remove(&entry.name).unwrap_or_default());
                Node { name: entry.name, kind: entry.kind, ignored: entry.ignored, dir }
            })
            .collect();
        self.children = Some(fresh);
        self.error = None;
    }

    /// Ask for this directory and every loaded directory beneath it again.
    fn want_all(&mut self) {
        if self.children.is_some() {
            self.wanted = true;
        }
        for node in self.children.iter_mut().flatten() {
            if let Some(dir) = &mut node.dir {
                dir.want_all();
            }
        }
    }
}

fn push_rows(rows: &mut Vec<Row>, dir: &Dir, path: &Path, depth: usize) {
    for node in dir.children.iter().flatten() {
        let child = path.join(&node.name);
        let expanded = node.dir.as_ref().is_some_and(|dir| dir.expanded);
        rows.push(Row {
            depth,
            name: node.name.to_string_lossy().into_owned(),
            path: child.clone(),
            kind: node.kind,
            expanded,
            ignored: node.ignored,
            error: node.dir.as_ref().and_then(|dir| dir.error.clone()),
        });
        if expanded && let Some(dir) = &node.dir {
            push_rows(rows, dir, &child, depth + 1);
        }
    }
}

/// The names in a relative path, or `None` if it has anything other than
/// plain names in it (`..`, a root, a prefix).
fn normal_components(relative: &Path) -> Option<Vec<&std::ffi::OsStr>> {
    relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// Every directory waiting for a listing, deepest last.
fn collect_wanted(dir: &Dir, path: &Path, wanted: &mut Vec<PathBuf>) {
    if dir.wanted {
        wanted.push(path.to_path_buf());
    }
    for node in dir.children.iter().flatten() {
        if let Some(child) = &node.dir
            && (child.expanded || child.wanted)
        {
            collect_wanted(child, &path.join(&node.name), wanted);
        }
    }
}

/// List one directory, sorted directories first and then by name.
///
/// Two passes: `read_dir` for everything that is there, and a one-level walk
/// with the ignore rules applied for what survives them. The difference is
/// what gets flagged as ignored. Listing a directory twice is cheap next to
/// the alternative of reimplementing the ignore crate's parent-directory rule
/// stacking here.
///
/// This touches the disk, so it belongs on a worker thread; [`crate::Jobs`]
/// is what calls it.
///
/// # Errors
///
/// Whatever the filesystem said, as a sentence for the row to show.
pub fn list_dir(dir: &Path, show_ignored: bool) -> Result<Vec<Entry>, String> {
    read_level(dir, show_ignored).map_err(|error| error.to_string())
}

fn read_level(dir: &Path, show_ignored: bool) -> io::Result<Vec<Entry>> {
    let entries = fs::read_dir(dir)?;
    let kept = unignored_names(dir);
    let mut nodes: Vec<Entry> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if name == ".git" {
                return None;
            }
            let ignored = !kept.contains(&name);
            if ignored && !show_ignored {
                return None;
            }
            let kind = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => Kind::Symlink,
                Ok(kind) if kind.is_dir() => Kind::Dir,
                _ => Kind::File,
            };
            Some(Entry { name, kind, ignored })
        })
        .collect();
    nodes.sort_by(|a, b| {
        (a.kind != Kind::Dir)
            .cmp(&(b.kind != Kind::Dir))
            .then_with(|| compare_names(&a.name.to_string_lossy(), &b.name.to_string_lossy()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(nodes)
}

/// The names directly inside `dir` that no ignore rule and no hidden-file rule
/// excludes.
fn unignored_names(dir: &Path) -> HashSet<OsString> {
    WalkBuilder::new(dir)
        .max_depth(Some(1))
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| entry.file_name().to_os_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reveal a path, waiting for whatever listings that needs.
    ///
    /// Each listing can expose the next directory down, so this goes round
    /// until the path turns up or nothing is left to wait for — which is what
    /// the editor does as listings arrive.
    fn revealed(tree: &mut FileTree, path: &Path) -> Option<usize> {
        loop {
            if let Some(row) = tree.reveal(path) {
                return Some(row);
            }
            if !tree.is_loading() {
                return None;
            }
            tree.load_blocking();
        }
    }

    /// A tree with its first listings already in, as tests want it.
    fn opened(root: impl Into<PathBuf>) -> FileTree {
        let mut tree = FileTree::open(root);
        tree.load_blocking();
        tree
    }
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// A temporary git repository: an empty `.git` directory is enough for
    /// the ignore crate to treat `.gitignore` as meaningful.
    fn repo(files: &[&str]) -> TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        for file in files {
            let path = temp.path().join(file);
            if let Some(stripped) = file.strip_suffix('/') {
                fs::create_dir_all(temp.path().join(stripped)).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, file).unwrap();
            }
        }
        temp
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.rows().iter().map(|row| format!("{}{}", "  ".repeat(row.depth), row.name)).collect()
    }

    #[test]
    fn opening_reads_only_the_top_level() {
        let temp = repo(&["src/main.rs", "src/lib.rs", "Cargo.toml"]);
        let tree = opened(temp.path());
        assert_eq!(names(&tree), ["src", "Cargo.toml"]);
        assert!(!tree.rows()[0].expanded);
    }

    #[test]
    fn directories_come_first_then_names_in_natural_order() {
        let temp = repo(&["b.rs", "A.rs", "zeta/", "Alpha/", "file10", "file2"]);
        let tree = opened(temp.path());
        assert_eq!(names(&tree), ["Alpha", "zeta", "A.rs", "b.rs", "file2", "file10"]);
    }

    #[test]
    fn expanding_and_collapsing_changes_the_visible_rows() {
        let temp = repo(&["src/main.rs", "src/nested/deep.rs", "README.md"]);
        let mut tree = opened(temp.path());
        assert!(tree.toggle(0));
        tree.load_blocking();
        assert_eq!(names(&tree), ["src", "  nested", "  main.rs", "README.md"]);
        assert!(tree.rows()[0].expanded);
        assert!(tree.toggle(1));
        tree.load_blocking();
        assert_eq!(names(&tree), ["src", "  nested", "    deep.rs", "  main.rs", "README.md"]);
        assert!(tree.toggle(0));
        tree.load_blocking();
        assert_eq!(names(&tree), ["src", "README.md"]);
        assert!(!tree.toggle(1), "toggling a file does nothing");
        tree.load_blocking();
        assert!(!tree.toggle(99), "toggling past the end does nothing");
        tree.load_blocking();
    }

    #[test]
    fn collapsing_and_expanding_again_keeps_inner_expansion() {
        let temp = repo(&["src/nested/deep.rs"]);
        let mut tree = opened(temp.path());
        let src = temp.path().join("src");
        tree.expand(&src);
        tree.load_blocking();
        tree.expand(&src.join("nested"));
        tree.load_blocking();
        tree.collapse(&src);
        tree.load_blocking();
        tree.expand(&src);
        tree.load_blocking();
        assert_eq!(names(&tree), ["src", "  nested", "    deep.rs"]);
    }

    #[test]
    fn gitignore_is_respected_in_every_directory() {
        let temp = repo(&[
            ".gitignore",
            "target/debug/app",
            "src/main.rs",
            "src/generated.rs",
            "src/sub/also.log",
            "src/sub/kept.rs",
        ]);
        fs::write(temp.path().join(".gitignore"), "/target\ngenerated.rs\n*.log\n").unwrap();
        let mut tree = opened(temp.path());
        assert_eq!(names(&tree), ["src"], "target and the dotfile are left out");
        revealed(&mut tree, &temp.path().join("src/sub/kept.rs")).unwrap();
        assert_eq!(names(&tree), ["src", "  sub", "    kept.rs", "  main.rs"]);
    }

    #[test]
    fn dot_ignore_files_apply_too() {
        let temp = repo(&[".ignore", "notes.txt", "code.rs"]);
        fs::write(temp.path().join(".ignore"), "notes.txt\n").unwrap();
        let tree = opened(temp.path());
        assert_eq!(names(&tree), ["code.rs"]);
    }

    #[test]
    fn showing_ignored_lists_and_flags_them_but_never_git() {
        let temp = repo(&[".gitignore", "target/app", "src/generated.rs", "src/main.rs"]);
        fs::write(temp.path().join(".gitignore"), "/target\ngenerated.rs\n").unwrap();
        let mut tree = opened(temp.path());
        tree.expand(&temp.path().join("src"));
        tree.load_blocking();
        tree.set_show_ignored(true);
        tree.load_blocking();
        assert!(tree.show_ignored());
        let flagged: Vec<(String, bool)> =
            tree.rows().iter().map(|row| (row.name.clone(), row.ignored)).collect();
        let expected = [
            ("src", false),
            ("generated.rs", true),
            ("main.rs", false),
            ("target", true),
            (".gitignore", true),
        ]
        .map(|(name, ignored)| (name.to_string(), ignored));
        assert_eq!(flagged, expected);
        assert!(tree.rows()[0].expanded, "expansion survives the toggle");
        assert!(tree.row_of(&temp.path().join(".git")).is_none());

        tree.set_show_ignored(false);
        tree.load_blocking();

        tree.load_blocking();
        assert_eq!(names(&tree), ["src", "  main.rs"]);
    }

    #[test]
    fn refresh_keeps_surviving_expansion_and_drops_the_rest() {
        let temp = repo(&["a/inner/x.rs", "b/y.rs"]);
        let mut tree = opened(temp.path());
        tree.expand(&temp.path().join("a"));
        tree.load_blocking();
        tree.expand(&temp.path().join("a/inner"));
        tree.load_blocking();
        tree.expand(&temp.path().join("b"));
        tree.load_blocking();

        fs::remove_dir_all(temp.path().join("b")).unwrap();
        fs::write(temp.path().join("new.rs"), "").unwrap();
        assert!(tree.refresh_dir(temp.path()));
        tree.load_blocking();
        assert_eq!(names(&tree), ["a", "  inner", "    x.rs", "new.rs"]);

        fs::create_dir(temp.path().join("b")).unwrap();
        tree.refresh_dir(temp.path());
        tree.load_blocking();
        let b = tree.row_of(&temp.path().join("b")).unwrap();
        assert!(!tree.rows()[b].expanded, "a directory that went away comes back collapsed");
    }

    #[test]
    fn refreshing_an_unloaded_directory_does_nothing() {
        let temp = repo(&["a/x.rs"]);
        let mut tree = opened(temp.path());
        assert!(!tree.refresh_dir(&temp.path().join("a")));
        tree.load_blocking();
        assert!(!tree.refresh_dir(&temp.path().join("missing")));
        tree.load_blocking();
        assert!(!tree.refresh_dir(Path::new("/somewhere/else")));
        tree.load_blocking();
    }

    #[test]
    fn reveal_expands_ancestors_and_returns_the_row() {
        let temp = repo(&["a/b/c/target.rs", "a/b/other.rs", "z.rs"]);
        let mut tree = opened(temp.path());
        let path = temp.path().join("a/b/c/target.rs");
        let row = revealed(&mut tree, &path).unwrap();
        assert_eq!(tree.rows()[row].path, path);
        assert_eq!(tree.rows()[row].depth, 3);
        assert_eq!(tree.row_of(&path), Some(row));
        assert!(revealed(&mut tree, Path::new("/not/under/root")).is_none());
        assert!(revealed(&mut tree, &temp.path().join("a/missing.rs")).is_none());
    }

    #[test]
    fn reveal_finds_a_file_created_since_its_directory_was_read() {
        let temp = repo(&["a/old.rs"]);
        let mut tree = opened(temp.path());
        tree.expand(&temp.path().join("a"));
        tree.load_blocking();
        fs::write(temp.path().join("a/new.rs"), "").unwrap();
        assert!(revealed(&mut tree, &temp.path().join("a/new.rs")).is_none(), "not listed yet");

        // Its directory has to be read again first, which is what the editor
        // does when an operation or the watcher says something changed.
        tree.refresh_dir(&temp.path().join("a"));
        tree.load_blocking();
        assert!(revealed(&mut tree, &temp.path().join("a/new.rs")).is_some());
    }

    #[test]
    fn expanded_dirs_lists_only_what_is_on_screen() {
        let temp = repo(&["a/inner/x.rs", "b/y.rs"]);
        let mut tree = opened(temp.path());
        tree.expand(&temp.path().join("a"));
        tree.load_blocking();
        tree.expand(&temp.path().join("a/inner"));
        tree.load_blocking();
        tree.collapse(&temp.path().join("a"));
        tree.load_blocking();
        tree.expand(&temp.path().join("b"));
        tree.load_blocking();
        assert_eq!(tree.expanded_dirs(), [temp.path().to_path_buf(), temp.path().join("b")]);
    }

    #[test]
    fn non_ascii_names_round_trip_and_sort() {
        let temp = repo(&["日本語.txt", "émoji-🦀.rs", "Zebra.md", "données/ß.rs", "👨‍👩‍👧.txt"]);
        let mut tree = opened(temp.path());
        assert_eq!(names(&tree), ["données", "Zebra.md", "émoji-🦀.rs", "日本語.txt", "👨‍👩‍👧.txt"]);
        for row in tree.rows() {
            assert!(row.path.exists(), "{} round-trips to a real path", row.name);
        }
        let file = temp.path().join("données/ß.rs");
        let row = revealed(&mut tree, &file).unwrap();
        assert_eq!(tree.rows()[row].name, "ß.rs");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_records_an_error_instead_of_panicking() {
        use std::os::unix::fs::PermissionsExt;
        let temp = repo(&["locked/secret.rs"]);
        let locked = temp.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut tree = opened(temp.path());
        tree.expand(&locked);
        tree.load_blocking();
        let readable = fs::read_dir(&locked).is_ok();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return; // running as root: permissions do not stop anything
        }
        let row = &tree.rows()[tree.row_of(&locked).unwrap()];
        assert!(row.expanded);
        assert!(row.error.is_some());
        assert!(tree.error(&locked).is_some());
        assert_eq!(tree.rows().len(), 1, "no children, no panic");

        assert!(tree.refresh_dir(&locked));
        tree.load_blocking();
        assert!(tree.error(&locked).is_none(), "a successful read clears the error");
    }

    #[test]
    fn an_unreadable_root_is_an_empty_tree_with_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let tree = opened(&missing);
        assert!(tree.rows().is_empty());
        assert!(tree.error(&missing).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_leaves() {
        let temp = repo(&["real/x.rs"]);
        std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("link")).unwrap();
        let mut tree = opened(temp.path());
        let link = tree.row_of(&temp.path().join("link")).unwrap();
        assert_eq!(tree.rows()[link].kind, Kind::Symlink);
        assert!(!tree.toggle(link));
        tree.load_blocking();
    }

    /// Only the root level is read, so what lies under an ignored `target/`
    /// costs nothing to open.
    #[test]
    fn a_large_ignored_target_opens_instantly() {
        let temp = repo(&[".gitignore", "Cargo.toml", "README.md", "src/main.rs"]);
        fs::write(temp.path().join(".gitignore"), "/target\n").unwrap();
        for bucket in 0..20 {
            let dir = temp.path().join(format!("target/debug/deps/{bucket}"));
            fs::create_dir_all(&dir).unwrap();
            for file in 0..1000 {
                fs::File::create(dir.join(format!("lib{file}.rlib"))).unwrap();
            }
        }

        let started = Instant::now();
        let mut tree = opened(temp.path());
        let opened = started.elapsed();
        assert_eq!(names(&tree), ["src", "Cargo.toml", "README.md"]);
        assert!(opened < Duration::from_millis(100), "opening took {opened:?}");

        let started = Instant::now();
        tree.set_show_ignored(true);
        tree.load_blocking();
        let shown = started.elapsed();
        assert!(tree.row_of(&temp.path().join("target")).is_some());
        assert!(shown < Duration::from_millis(100), "showing ignored took {shown:?}");
    }
}
