//! What comes back from the servers, as messages for the editor's channel.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lsp_types::{Diagnostic, MessageType, ServerCapabilities, WorkspaceEdit};
use serde_json::Value;

use crate::position::Encoding;

/// Which document a message is about: the editor's own id for it.
pub type DocId = u32;

/// One request, from the moment it is asked until its answer is handled or it
/// is cancelled. Unique for the life of the [`crate::Lsp`], so an answer can
/// always be matched to the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub(crate) u64);

/// One server: one command, run for one project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerId(pub(crate) u32);

/// What a server said it can do, shared rather than copied: every message and
/// every document that mentions it points at the one answer.
#[derive(Debug, Clone)]
pub struct Capabilities(pub(crate) Arc<ServerCapabilities>);

impl Deref for Capabilities {
    type Target = ServerCapabilities;

    fn deref(&self) -> &ServerCapabilities {
        &self.0
    }
}

impl PartialEq for Capabilities {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

// Capabilities hold no floating point, so equality is an equivalence.
impl Eq for Capabilities {}

/// Where a server is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Its process is starting, or it has not answered `initialize` yet.
    Starting,
    /// Initialized: requests go to it.
    Ready,
    /// It exited, broke the protocol, or stopped answering, and will be
    /// started again after `retry_in`.
    Crashed {
        /// Which attempt the next start will be, counting from one.
        attempt: u32,
        /// How long until then.
        retry_in: Duration,
        /// What happened.
        why: String,
    },
    /// It crashed too many times in a row; nun has stopped starting it until
    /// asked to again.
    GaveUp {
        /// How many times it was started.
        attempts: u32,
        /// What happened the last time.
        why: String,
    },
    /// Its command is not installed. Not an error: most people have only a
    /// few of the servers nun knows about.
    Missing,
    /// Shut down, as asked.
    Stopped,
}

/// Why a request has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No server is running for the document, or it is not initialized yet.
    NotReady,
    /// The server did not answer in time. `$/cancelRequest` has been sent.
    TimedOut,
    /// The server stopped — crashed, restarted, or shut down — before it
    /// answered.
    Stopped,
    /// The server answered with an error.
    Server {
        /// The protocol's error code.
        code: i64,
        /// What the server said.
        message: String,
    },
    /// The answer was not the shape the request's type says it should be.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => write!(f, "the language server is not ready"),
            Self::TimedOut => write!(f, "the language server did not answer in time"),
            Self::Stopped => write!(f, "the language server stopped before it answered"),
            Self::Server { message, .. } => write!(f, "{message}"),
            Self::Malformed(why) => write!(f, "the language server's answer made no sense: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Which request.
    pub id: RequestId,
    /// The document it was about.
    pub doc: DocId,
    /// The version of the document it was asked about. An answer about an
    /// older version than the document is at now describes text that has
    /// since changed.
    pub version: i32,
    /// The result, as JSON; [`Response::parse`] reads it as the request's
    /// result type.
    pub result: Result<Value, Error>,
}

impl Response {
    /// The result as request `R` defines it.
    ///
    /// # Errors
    ///
    /// The request's own error, or [`Error::Malformed`] when the result is
    /// not what `R` says it should be.
    pub fn parse<R: lsp_types::request::Request>(&self) -> Result<R::Result, Error> {
        let value = self.result.clone()?;
        serde_json::from_value(value).map_err(|error| Error::Malformed(error.to_string()))
    }
}

/// Diagnostics a server published for one file, as it last published them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// Which server said so.
    pub server: ServerId,
    /// The version of the document they describe, when the server said.
    pub version: Option<i32>,
    /// The diagnostics, positions in the server's encoding.
    pub diagnostics: Vec<Diagnostic>,
}

/// A server asking for an edit to be made — `workspace/applyEdit`, usually
/// while it carries out a command a code action named.
///
/// The server waits for the answer, so every one of these must be answered,
/// with [`crate::Lsp::answer_edit`], whatever becomes of the edit. It is
/// answered when the editor is done with it, which may be after the person
/// has looked it over: nothing on the server's side blocks meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// Which server asked.
    pub server: ServerId,
    /// What the server calls the edit, for the person.
    pub label: Option<String>,
    /// The edit.
    pub edit: WorkspaceEdit,
    /// The unit every position in it counts in: its server's.
    pub encoding: Encoding,
    /// Which run of the server asked, so an answer never reaches a later
    /// run that asked nothing.
    pub(crate) run: u64,
    /// The request's id on the wire, echoed in the answer.
    pub(crate) id: Value,
}

/// Something a server did, for the editor's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A document is served by `server`, as `path` — its full, resolved path,
    /// which is what diagnostics for it will be filed under.
    Attached {
        /// The document.
        doc: DocId,
        /// Its server.
        server: ServerId,
        /// What the server is called, for the status line.
        name: String,
        /// The document's path, resolved.
        path: PathBuf,
    },
    /// A server answered `initialize`.
    Initialized {
        /// Which.
        server: ServerId,
        /// What it can do.
        capabilities: Capabilities,
        /// The unit its positions count in.
        encoding: Encoding,
    },
    /// A server moved on in its life.
    Status {
        /// Which.
        server: ServerId,
        /// Where it is now.
        status: Status,
    },
    /// A server is working on something long, or has finished.
    Progress {
        /// Which.
        server: ServerId,
        /// What it is doing, as it said it: "Indexing 40%". `None` when it
        /// has finished everything it announced.
        text: Option<String>,
    },
    /// An answer to a request.
    Response(Response),
    /// A server published diagnostics for a file.
    Diagnostics {
        /// The file, resolved.
        path: PathBuf,
        /// What it published.
        published: Published,
    },
    /// A server asked for an edit to be made, and is waiting for the answer.
    ApplyEdit(EditRequest),
    /// A server asked for something to be shown: `window/showMessage`.
    Message {
        /// Which.
        server: ServerId,
        /// How serious it is.
        kind: MessageType,
        /// What it said.
        text: String,
    },
}
