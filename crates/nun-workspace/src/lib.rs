//! The project on disk: the file tree, the operations that change it, and the
//! watcher that notices when something else does.
//!
//! Three pieces, deliberately independent of each other so the binary can wire
//! them together on its own terms:
//!
//! - [`FileTree`] is a lazily loaded, virtualised model of a directory. It reads
//!   one directory level at a time, only when that directory is expanded, so a
//!   repository with a hundred thousand files in `target/` opens as fast as one
//!   with ten. It keeps a flat list of visible rows so drawing a window of the
//!   sidebar costs the window, not the tree.
//! - [`FsHistory`] creates, renames, moves and deletes, and can undo and redo
//!   every one of them. Delete moves the entry into a nun-owned trash rather
//!   than removing it, because a delete that cannot be undone is not one a
//!   person should be able to reach with a single click.
//! - [`Jobs`] does the actual filesystem work — listing directories and
//!   running operations — on a worker thread, and reports each result as a
//!   message, so nothing the editor draws ever waits on a disk.
//! - [`Grep`] searches the project's text on a thread of its own, streaming
//!   hits as it finds them and stopping the moment the query changes. It is
//!   separate from [`Jobs`] on purpose: a search of a large repository would
//!   otherwise sit in front of the listings the tree is waiting on.
//! - [`Replacer`] rewrites what [`Grep`] found, with one engine answering both
//!   the panel's preview and the bytes that are written, so the two cannot
//!   disagree. The writing itself is a [`Job`], because it is filesystem work
//!   and it records into the same undo history everything else does.
//! - [`Rewrite`] writes whole files whose new text was worked out elsewhere —
//!   a language server's rename — and only while each still holds what it was
//!   read as. It is a [`Job`] too, and its undo is the same job the other way.
//! - [`Watcher`] watches the expanded directories and reports, coalesced per
//!   directory, that something in one of them changed. It never touches the
//!   tree itself: the editor state has one owner on the main thread, so the
//!   watcher posts a message and the owner calls [`FileTree::refresh_dir`].
//!
//! No terminal dependency; all of this is unit tested directly against temporary
//! directories.

pub mod grep;
pub mod jobs;
pub mod labels;
pub mod ops;
mod order;
pub mod replace;
pub mod rewrite;
pub mod search;
pub mod tree;
pub mod watch;

pub use grep::{Case, Found, Grep, Hit, MOST_CHARS, Options};
pub use jobs::{Done, Job, Jobs, trash_or_temp};
pub use labels::{UNNAMED, tab_labels};
pub use ops::{Change, FsHistory, OpError, Operation, default_trash_dir};
pub use order::compare_names;
pub use replace::{Outcome, Recorded, Replacer, Report, Skipped, preview};
pub use rewrite::{Rewrite, Written};
pub use search::{MOST_FILES, Match, list_files, search};
pub use tree::{Entry, FileTree, Kind, Row, list_dir};
pub use watch::{FsChange, WatchError, Watcher};
