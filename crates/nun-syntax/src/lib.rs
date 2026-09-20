//! Syntax: what the code in a buffer actually is.
//!
//! Tree-sitter parses; this crate wraps it in the two things the editor needs
//! and nothing else. [`Document`] holds a parse and keeps it up to date
//! incrementally, handing back highlight runs for whatever part of the file is
//! on screen. [`Worker`] runs those documents on a thread of their own, because
//! parsing is exactly the kind of work a frame must never wait for.
//!
//! Capture names come out as they are — `function.method`, `string`,
//! `keyword` — and stay strings. What colour they take is a question about
//! themes, and this crate has no business knowing the answer.

mod highlight;
mod language;
mod worker;

pub use highlight::{Document, PARSE_BUDGET, Span, TextEdit, Trouble};
pub use language::{Language, all, of_name, of_path};
pub use worker::{DocId, Reply, Request, Worker};
