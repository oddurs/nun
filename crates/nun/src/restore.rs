//! Where a folder was left: which files were open, in which panes, in which
//! order, with the carets and the scroll where they were.
//!
//! One file per folder, under `$XDG_STATE_HOME/nun/sessions/`, beside the
//! fold memory in [`crate::session`] — never inside the repository. Folds are
//! not repeated here: they are remembered per file, whichever folder it was
//! opened from, and the first parse of each restored file puts them back.
//!
//! The file is TOML, versioned by a `version` key read before anything else,
//! so a later format can be migrated from or discarded cleanly:
//!
//! ```toml
//! version = 1
//! root = "/home/me/project"
//! focus = 1
//! layout = "beside 500 (0, below 400 (1, 2))"
//!
//! [[panes]]
//! active = 0
//!
//! [[panes.tabs]]
//! path = "/home/me/project/src/main.rs"
//! scroll = 40
//! selections = [[52, 8, 52, 8]]
//!
//! [panels.terminal]
//! # whatever the terminal panel keeps; see `State::panels`
//! ```
//!
//! Carets are kept as a line and a column counted in chars from the start of
//! the line, not as char offsets into the file. The file may have changed
//! since: a line and a column still name the same place when something is
//! edited further down, they clamp to something sensible when the file got
//! shorter, and a column can be snapped to a grapheme boundary on its own
//! line. An offset into changed text lands somewhere arbitrary. Columns are
//! chars rather than display columns so that a change of tab width does not
//! move them.
//!
//! Anything wrong with the file — unreadable, not TOML, the wrong shape — is
//! an empty session and a notice, never a failure to start.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

use crate::session::Session;

/// The format written now. A file with a higher one came from a newer nun and
/// is left alone; one with a lower one would be migrated here.
pub const VERSION: u32 = 1;

/// A session file larger than this is not one nun wrote.
const MOST_BYTES: u64 = 1 << 20;

/// How deeply splits may nest before the layout is taken to be damaged.
const DEEPEST: usize = 64;

/// A folder's session, as written to disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The format this was written in: [`VERSION`].
    pub version: u32,
    /// The folder it belongs to. Checked on load, so a file that turns up
    /// under another folder's name is not taken for that folder's.
    pub root: PathBuf,
    /// Which pane had the keyboard, by its place in `panes`.
    #[serde(default)]
    pub focus: usize,
    /// How the panes divide the screen.
    pub layout: Node,
    /// Every pane, in the order the layout draws them.
    pub panes: Vec<PaneState>,
    /// State kept by other kinds of panel, by kind — the integrated terminal
    /// as `[panels.terminal]`, say. Each kind owns its table and its shape.
    /// A kind this build knows nothing about is carried through to the next
    /// write untouched, so adding one is not a format change and an older nun
    /// does not erase it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panels: BTreeMap<String, toml::Table>,
}

/// One pane: its tabs, and which one was showing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    /// Which tab was showing.
    #[serde(default)]
    pub active: usize,
    /// Its files, in tab order.
    #[serde(default)]
    pub tabs: Vec<TabState>,
}

/// One open file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    /// The file, in full.
    pub path: PathBuf,
    /// The first line in view.
    #[serde(default)]
    pub scroll: usize,
    /// Every selection as `[anchor line, anchor column, head line, head
    /// column]`, columns in chars. None is a caret at the top.
    #[serde(default)]
    pub selections: Vec<[usize; 4]>,
    /// Which selection is the primary one.
    #[serde(default)]
    pub primary: usize,
}

/// The pane tree, with panes named by their place in [`State::panes`].
///
/// Written as one line — `beside 500 (0, below 400 (1, 2))` — rather than as
/// nested tables, which in TOML are a header per node and hard to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Node {
    /// A pane.
    Pane(usize),
    /// Two nodes, divided.
    Split {
        /// Side by side (`true`), or one above the other.
        beside: bool,
        /// How much the first takes, in thousandths.
        ratio: u16,
        /// Left, or above.
        first: Box<Node>,
        /// Right, or below.
        second: Box<Node>,
    },
}

impl Node {
    /// Every pane it names, in order.
    #[must_use]
    pub fn panes(&self) -> Vec<usize> {
        match self {
            Self::Pane(pane) => vec![*pane],
            Self::Split { first, second, .. } => [first.panes(), second.panes()].concat(),
        }
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(pane) => write!(f, "{pane}"),
            Self::Split { beside, ratio, first, second } => {
                let dir = if *beside { "beside" } else { "below" };
                write!(f, "{dir} {ratio} ({first}, {second})")
            }
        }
    }
}

impl From<Node> for String {
    fn from(node: Node) -> Self {
        node.to_string()
    }
}

impl TryFrom<String> for Node {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let mut words = Words::new(&text);
        let node = words.node(0)?;
        match words.next() {
            None => Ok(node),
            Some(extra) => Err(format!("layout: `{extra}` after the end")),
        }
    }
}

/// The layout line, split into numbers, words and punctuation.
struct Words<'a> {
    rest: &'a str,
}

impl<'a> Words<'a> {
    const fn new(text: &'a str) -> Self {
        Self { rest: text }
    }

    fn next(&mut self) -> Option<&'a str> {
        self.rest = self.rest.trim_start();
        let first = self.rest.chars().next()?;
        let len = if matches!(first, '(' | ')' | ',') {
            1
        } else {
            self.rest
                .find(|c: char| c.is_whitespace() || "(),".contains(c))
                .unwrap_or(self.rest.len())
        };
        let (word, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(word)
    }

    fn expect(&mut self, wanted: &str) -> Result<(), String> {
        match self.next() {
            Some(word) if word == wanted => Ok(()),
            found => Err(format!("layout: expected `{wanted}`, found {found:?}")),
        }
    }

    fn node(&mut self, depth: usize) -> Result<Node, String> {
        if depth > DEEPEST {
            return Err("layout: nested too deeply".into());
        }
        let word = self.next().ok_or("layout: ended early")?;
        let beside = match word {
            "beside" => true,
            "below" => false,
            number => {
                return number.parse().map(Node::Pane).map_err(|_| format!("layout: `{number}`"));
            }
        };
        let ratio = self.next().ok_or("layout: ended early")?;
        let ratio = ratio.parse().map_err(|_| format!("layout: ratio `{ratio}`"))?;
        self.expect("(")?;
        let first = self.node(depth + 1)?;
        self.expect(",")?;
        let second = self.node(depth + 1)?;
        self.expect(")")?;
        Ok(Node::Split { beside, ratio, first: Box::new(first), second: Box::new(second) })
    }
}

/// What was found where a folder's session is kept.
#[derive(Debug)]
pub enum Loaded {
    /// Nothing, or a session for some other folder.
    Nothing,
    /// A session to restore.
    Found(State),
    /// Something that is not a session, and why.
    Damaged(String),
    /// A session written by a newer nun, in this format version. Left alone:
    /// writing over it would lose what that nun kept.
    Newer(u32),
}

/// Read the session kept at `file` for the folder `root`.
#[must_use]
pub fn load(file: &Path, root: &Path) -> Loaded {
    let text = match fs::metadata(file) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Loaded::Nothing,
        Ok(meta) if meta.len() > MOST_BYTES => {
            return Loaded::Damaged(format!("{} bytes is too many", meta.len()));
        }
        _ => match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) => return Loaded::Damaged(error.to_string()),
        },
    };
    parse(&text, root)
}

/// A session from its text, for the folder `root`.
#[must_use]
pub fn parse(text: &str, root: &Path) -> Loaded {
    // The version first, and on its own: a newer format may not parse as this
    // one, and should be told apart from a damaged file.
    let table: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(error) => return Loaded::Damaged(error.message().to_string()),
    };
    let version = table.get("version").and_then(toml::Value::as_integer);
    match version.map(u32::try_from) {
        Some(Ok(VERSION)) => {}
        Some(Ok(newer)) if newer > VERSION => return Loaded::Newer(newer),
        _ => return Loaded::Damaged("no format version nun knows".into()),
    }
    let state: State = match toml::Value::Table(table).try_into() {
        Ok(state) => state,
        Err(error) => return Loaded::Damaged(error.message().to_string()),
    };
    if state.root != root {
        return Loaded::Nothing;
    }
    Loaded::Found(state)
}

/// The session as text, to be written.
///
/// # Errors
///
/// Only if the state cannot be put into TOML, which a path that is not UTF-8
/// cannot.
pub fn to_text(state: &State) -> Result<String, toml::ser::Error> {
    toml::to_string(state)
}

/// Where the session for `root` is kept: `sessions/` in the state directory,
/// named for the folder so a person looking can tell which is which, and
/// told apart by a hash of its whole path. `None` with no state directory.
#[must_use]
pub fn path_for(root: &Path) -> Option<PathBuf> {
    let dir = Session::default_path()?.with_file_name("sessions");
    let name: String = root
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
        .take(40)
        .collect();
    let hash = fnv1a(root.as_os_str().as_encoded_bytes());
    Some(dir.join(format!("{name}-{hash:016x}.toml")))
}

/// FNV-1a, 64 bits. Written out rather than taken from the standard library,
/// whose hashers are not promised to give the same answer from one release
/// to the next — and a file name has to.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Write `text` to `path` by way of a temporary file beside it, so a crash
/// half way through leaves the last session rather than half of this one.
///
/// # Errors
///
/// Whatever creating the directory, writing or renaming ran into.
pub fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

/// Something to write down: the folder's session, the folds, or both.
///
/// Paths arrive as the editor has them and are resolved here, on the writer's
/// thread: resolving one asks the filesystem, which the editor's thread never
/// waits on.
#[derive(Debug, Default)]
pub struct Job {
    /// The session file, and what to put in it.
    pub state: Option<(PathBuf, State)>,
    /// The fold memory, and what is folded in each open file now, to lay
    /// over it before it is saved.
    pub folds: Option<Folds>,
}

/// The fold memory, and what is folded in each open file now, by path.
pub type Folds = (Session, Vec<(PathBuf, Vec<usize>)>);

/// The thread that writes sessions down, so the disk is never waited on
/// between keystrokes.
///
/// Jobs are written in the order they were sent; any that queue up while one
/// is being written are merged, so only the latest of each kind is.
#[derive(Debug)]
pub struct Writer {
    sender: Option<mpsc::Sender<Job>>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}

impl Writer {
    /// Start the thread.
    ///
    /// # Errors
    ///
    /// If the thread could not be started.
    pub fn start() -> io::Result<Self> {
        let (sender, jobs) = mpsc::channel::<Job>();
        let thread = thread::Builder::new().name("nun-session".into()).spawn(move || {
            let mut result = Ok(());
            while let Ok(mut job) = jobs.recv() {
                for later in jobs.try_iter() {
                    job.state = later.state.or(job.state);
                    job.folds = later.folds.or(job.folds);
                }
                result = write(job);
            }
            // The last write's outcome, which is the one that stands.
            result
        })?;
        Ok(Self { sender: Some(sender), thread: Some(thread) })
    }

    /// Queue `job`.
    pub fn send(&self, job: Job) {
        if let Some(sender) = &self.sender {
            // Gone only if the thread panicked, and then there is no one to
            // write it; the session is worth less than carrying on.
            let _ = sender.send(job);
        }
    }

    /// Write whatever is queued and stop, waiting for the thread.
    ///
    /// # Errors
    ///
    /// What the last write ran into, or that the thread died.
    pub fn finish(mut self) -> io::Result<()> {
        self.sender = None;
        match self.thread.take().map(thread::JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(io::Error::other("the thread writing the session stopped")),
            None => Ok(()),
        }
    }
}

fn write(job: Job) -> io::Result<()> {
    let folds = job.folds.map_or(Ok(()), |(mut session, open)| {
        for (path, headers) in open {
            session.remember(&path, headers);
        }
        session.save()
    });
    let state = job.state.map_or(Ok(()), |(path, mut state)| {
        for tab in state.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            tab.path = resolved(&tab.path);
        }
        let text = to_text(&state).map_err(io::Error::other)?;
        write_atomically(&path, &text)
    });
    folds.and(state)
}

/// `path` in full, however it was opened: nun may be started from anywhere
/// next time.
fn resolved(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            version: VERSION,
            root: PathBuf::from("/p"),
            focus: 1,
            layout: Node::try_from("beside 500 (0, below 400 (1, 2))".to_string()).unwrap(),
            panes: vec![
                PaneState {
                    active: 1,
                    tabs: vec![
                        TabState {
                            path: "/p/a.rs".into(),
                            scroll: 3,
                            selections: vec![[4, 2, 4, 9], [7, 0, 7, 0]],
                            primary: 1,
                        },
                        TabState {
                            path: "/p/ü b.rs".into(),
                            scroll: 0,
                            selections: Vec::new(),
                            primary: 0,
                        },
                    ],
                },
                PaneState::default(),
                PaneState::default(),
            ],
            panels: BTreeMap::new(),
        }
    }

    #[test]
    fn a_session_survives_being_written_and_read() {
        let text = to_text(&state()).unwrap();
        match parse(&text, Path::new("/p")) {
            Loaded::Found(read) => assert_eq!(read, state()),
            other => panic!("{other:?} from\n{text}"),
        }
    }

    #[test]
    fn the_layout_reads_as_it_is_written() {
        for text in ["0", "beside 500 (0, 1)", "below 250 (beside 500 (0, 1), 2)"] {
            let node = Node::try_from(text.to_string()).unwrap();
            assert_eq!(node.to_string(), text);
        }
        assert_eq!(
            Node::try_from("beside 500 (0, below 400 (1, 2))".to_string()).unwrap().panes(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_damaged_layout_is_an_error_not_a_panic() {
        let deep = format!("{}0{}", "beside 1 (".repeat(100), ", 0)".repeat(100));
        for text in ["", "beside", "beside 5 (0 1)", "0 0", "sideways 5 (0, 1)", "-1", &deep] {
            assert!(Node::try_from(text.to_string()).is_err(), "{text:?}");
        }
    }

    #[test]
    fn anything_that_is_not_a_session_is_damaged() {
        for text in [
            "not toml [",
            "root = \"/p\"",
            "version = \"one\"",
            "version = 1\nroot = \"/p\"",
            "version = 1\nroot = \"/p\"\nlayout = \"0\"\npanes = 3",
            "version = 1\nroot = \"/p\"\nlayout = \"beside\"\npanes = []",
            "version = 0",
        ] {
            assert!(matches!(parse(text, Path::new("/p")), Loaded::Damaged(_)), "{text:?}");
        }
    }

    #[test]
    fn a_session_from_a_newer_nun_is_told_apart() {
        let text = "version = 7\nwhatever = \"new\"";
        assert!(matches!(parse(text, Path::new("/p")), Loaded::Newer(7)));
    }

    #[test]
    fn a_session_for_another_folder_is_none_of_this_ones() {
        let text = to_text(&state()).unwrap();
        assert!(matches!(parse(&text, Path::new("/q")), Loaded::Nothing));
    }

    #[test]
    fn panels_of_kinds_this_build_does_not_know_are_carried_through() {
        let text = format!(
            "{}\n[panels.terminal]\nheight = 12\ncwd = [\"/p\", \"/p/sub\"]\n",
            to_text(&state()).unwrap()
        );
        let Loaded::Found(read) = parse(&text, Path::new("/p")) else { panic!("{text}") };
        assert_eq!(read.panels["terminal"]["height"].as_integer(), Some(12));
        let again = to_text(&read).unwrap();
        assert!(again.contains("[panels.terminal]"), "{again}");
    }

    #[test]
    fn a_missing_file_is_no_session() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(load(&dir.path().join("none"), dir.path()), Loaded::Nothing));
    }

    #[test]
    fn a_huge_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.toml");
        fs::write(&file, vec![b' '; 2 << 20]).unwrap();
        assert!(matches!(load(&file, dir.path()), Loaded::Damaged(_)));
    }

    #[test]
    fn each_folder_has_its_own_readable_file_name() {
        let one = path_for(Path::new("/home/me/nun")).unwrap();
        let other = path_for(Path::new("/home/you/nun")).unwrap();
        assert_ne!(one, other);
        let name = one.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("nun-"), "{name}");
        assert_eq!(one.extension().unwrap(), "toml");
        assert_eq!(one.parent().unwrap().file_name().unwrap(), "sessions");
        let odd = path_for(Path::new("/tmp/a b/ü")).unwrap();
        assert!(odd.file_name().unwrap().to_str().unwrap().starts_with("_-"), "{odd:?}");
    }

    #[test]
    fn the_hash_is_the_published_fnv1a() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn the_writer_writes_the_last_of_what_it_was_sent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deep").join("s.toml");
        let writer = Writer::start().unwrap();
        for n in 0..50 {
            let state = State { focus: n, ..state() };
            writer.send(Job { state: Some((file.clone(), state)), folds: None });
        }
        writer.finish().unwrap();
        let Loaded::Found(written) = load(&file, Path::new("/p")) else { panic!() };
        assert_eq!(written.focus, 49);
        let left: Vec<_> = fs::read_dir(file.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file left behind: {left:?}");
    }

    #[test]
    fn the_writer_says_when_it_could_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        let writer = Writer::start().unwrap();
        writer.send(Job { state: Some((blocker.join("s.toml"), state())), folds: None });
        assert!(writer.finish().is_err());
    }
}
