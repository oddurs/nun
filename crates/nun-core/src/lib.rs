//! Text storage and editing primitives for nun.
//!
//! This crate has no terminal dependency and is unit-testable directly. It owns
//! the rope-backed [`Buffer`], the [`Edit`] type every mutation goes through,
//! the undo [`History`], and the plural [`Selections`] model.
//!
//! Indices are **char indices** throughout, matching [`ropey`]. Byte indices
//! appear only inside a line-local helper and never cross a public boundary.

mod buffer;
mod edit;
mod grapheme;
mod history;
mod selection;
mod text;

pub use buffer::{Buffer, SaveError};
pub use edit::{Assoc, Edit};
pub use history::History;
pub use selection::{Range, Selections};
pub use text::{LineEnding, LoadReport};
