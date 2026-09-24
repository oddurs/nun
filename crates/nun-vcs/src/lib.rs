//! Git, for an editor: what changed in each open file, and in the tree.
//!
//! Three layers, each usable without the one above it:
//!
//! - [`Diff`] is pure: the line [`Hunk`]s between an old text and the one on
//!   screen, what each line is ([`LineMark`]), the old text of a hunk, the
//!   edit that reverts it, and the text staging it would write. Intra-line
//!   changes for a diff view come from [`inline_changes`].
//! - [`Repo`] is one repository: finding it from a path (a linked worktree or
//!   a submodule is found the way git finds it), reading the staged or
//!   committed version of a file, writing one hunk into the index, and the
//!   working tree's [`Status`].
//! - [`Vcs`] runs all of that on threads of its own and answers with
//!   [`Reply`] messages, so the editor never waits on git. The hunks it sends
//!   are always between the index and the buffer as it stands — unsaved edits
//!   included — so the marks follow what is on the screen.
//!
//! A file in no repository is not an error anywhere here: there is simply
//! no diff, and no status.
//!
//! No terminal dependency; tested against real temporary repositories, made
//! with the `git` command when one is installed.

mod diff;
mod repo;
mod status;
mod worker;

pub use diff::{Base, Diff, Hunk, HunkKind, Inline, LineMark, inline_changes, line_hunks};
pub use repo::{Against, Blob, Error, MOST_BYTES, Repo, is_binary};
pub use status::{FileStatus, Status};
pub use worker::{DocId, Reply, Request, Vcs};
