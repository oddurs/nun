//! Language servers, kept out of the way of everything else.
//!
//! The editor holds an [`Lsp`] and calls it from the main thread; nothing on
//! it waits. Opening a document starts its server if it is the first of its
//! language and project; edits go over as the buffer journals them; requests
//! return an id at once and are answered later as an [`Event`] on the editor's
//! channel, which [`Lsp::handle`] turns into what the editor should do.
//!
//! Behind the handle each server is one tokio task that owns the process, the
//! server's copy of every document, and every request not yet answered. A
//! server that crashes is started again after a backoff, and given up on —
//! loudly — after five crashes in a row. A server that hangs stops being
//! waited for: requests time out and are cancelled, a server that lets three
//! in a row time out is restarted, and shutdown gives it a deadline and then
//! kills it. At worst the editor loses its language features; it is never the
//! editor that stops answering.
//!
//! Positions cross the boundary through [`Encoding`], which converts exactly
//! between nun's char indices and whichever of UTF-8, UTF-16 or UTF-32 the
//! server agreed to.

mod client;
pub mod completion;
mod event;
mod language;
mod log;
mod position;
mod rpc;
mod server;
pub mod snippet;
mod sync;
pub mod uri;

#[cfg(test)]
mod fake;

pub use client::{Handled, Indicator, Lsp, ServerSpec};
pub use event::{
    Capabilities, DocId, Error, Event, Published, RequestId, Response, ServerId, Status,
};
pub use language::{Language, names as languages, of_path as language_of};
pub use lsp_types as types;
pub use position::Encoding;
