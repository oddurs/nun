//! The editor's side: a handle that never waits, and the runtime behind it.
//!
//! [`Lsp`] lives on the main thread with the rest of the editor's state. Every
//! call on it sends a message and returns; nothing on it blocks, awaits or
//! locks, apart from [`Lsp::shutdown`] at exit. What the servers say comes
//! back as [`Event`]s through the editor's own channel, and the editor hands
//! each to [`Lsp::handle`], which is the only place the handle's picture of
//! the servers changes — the same shape as every other worker in nun.
//!
//! Behind it, one thread runs a single-threaded tokio runtime: one task routes
//! messages, and one task per server does everything else. A single thread is
//! plenty — all of it is waiting on pipes — and it keeps an idle editor's
//! language servers costing one parked thread.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lsp_types::{MessageType, Position, TextDocumentIdentifier, TextDocumentPositionParams};
use nun_core::Edit;
use ropey::Rope;
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::event::{
    Capabilities, DocId, EditRequest, Error, Event, Published, RequestId, Response, ServerId,
    Status,
};
use crate::language::{self, Language};
use crate::log::Log;
use crate::position::Encoding;
use crate::server::{Launcher, Report, Server, Shared, Spec, Timing, ToServer};

/// How to start the server for one language: `[lsp.<language>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// The program, found on `PATH` unless it is a path.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Whether it is only a default, which a person may well not have
    /// installed. A default that is missing is skipped without a word; one
    /// somebody configured themselves is worth saying is missing.
    pub optional: bool,
}

/// Something for the runtime to do.
#[derive(Debug)]
enum Command {
    Open(Opening),
    Change {
        doc: DocId,
        version: i32,
        edits: Vec<Edit>,
        text: Rope,
    },
    Save {
        doc: DocId,
    },
    Close {
        doc: DocId,
    },
    Request {
        id: RequestId,
        doc: DocId,
        version: i32,
        method: String,
        params: Value,
        timeout: Option<Duration>,
    },
    Notify {
        doc: DocId,
        method: String,
        params: Value,
    },
    Cancel {
        id: RequestId,
        doc: DocId,
    },
    AnswerEdit {
        server: ServerId,
        run: u64,
        id: Value,
        result: Result<(), String>,
    },
    Restart {
        server: ServerId,
    },
    Shutdown {
        deadline: std::time::Instant,
    },
}

/// A document to start following, and how to serve it.
#[derive(Debug)]
struct Opening {
    doc: DocId,
    path: PathBuf,
    language: Language,
    spec: Spec,
    optional: bool,
    version: i32,
    text: Rope,
}

/// What the editor should do about an event, once [`Lsp::handle`] has taken
/// in what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    /// Nothing on screen changed.
    Nothing,
    /// Something the status line or the text shows has changed.
    Redraw,
    /// Something worth telling the person, once.
    Notice(String),
    /// The answer to a request that is still wanted. Answers to requests that
    /// were cancelled never get this far.
    Response(Response),
    /// A server wants an edit made, and waits to hear whether it was: answer
    /// it with [`Lsp::answer_edit`], always.
    ApplyEdit(EditRequest),
}

/// A document, as the editor's side knows it.
#[derive(Debug)]
struct Doc {
    version: i32,
    config: &'static str,
    server: Option<ServerId>,
    /// Its full, resolved path, once the runtime has worked it out.
    path: Option<PathBuf>,
}

/// A server, as the editor's side knows it.
#[derive(Debug)]
struct Known {
    name: String,
    config: &'static str,
    status: Status,
    capabilities: Option<Capabilities>,
    encoding: Encoding,
    progress: Option<String>,
}

/// What the status line shows for a document's server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indicator<'a> {
    /// The server's name: its command.
    pub name: &'a str,
    /// Where it is in its life.
    pub status: &'a Status,
    /// What it says it is busy with.
    pub progress: Option<&'a str>,
}

impl Indicator<'_> {
    /// The words for the status line.
    #[must_use]
    pub fn label(&self) -> String {
        let name = self.name;
        match (self.status, self.progress) {
            (Status::Ready, Some(progress)) => format!("{name} · {progress}"),
            (Status::Ready, None) => name.to_string(),
            (Status::Starting, _) => format!("{name} starting…"),
            (Status::Crashed { .. }, _) => format!("{name} restarting…"),
            (Status::GaveUp { .. }, _) => format!("{name} crashed"),
            (Status::Missing, _) => format!("{name} not installed"),
            (Status::Stopped, _) => format!("{name} stopped"),
        }
    }

    /// Whether it is in trouble, and should look it.
    #[must_use]
    pub const fn is_trouble(&self) -> bool {
        matches!(self.status, Status::Crashed { .. } | Status::GaveUp { .. })
    }
}

/// The editor's handle on its language servers.
///
/// [`Lsp::shutdown`] shuts the servers down with a deadline. Dropping the
/// handle without it does the same with a short one, so a way out of the
/// editor that skips the shutdown — an error on the way — still does not
/// leave servers running behind it.
#[derive(Debug)]
pub struct Lsp {
    specs: BTreeMap<String, ServerSpec>,
    commands: UnboundedSender<Command>,
    /// Signalled when the runtime has finished, for [`Lsp::shutdown`].
    finished: Option<std::sync::mpsc::Receiver<()>>,
    next_request: u64,
    docs: HashMap<DocId, Doc>,
    servers: HashMap<ServerId, Known>,
    pending: HashMap<RequestId, DocId>,
    diagnostics: HashMap<PathBuf, Published>,
}

impl Lsp {
    /// Start the runtime, with `servers` by language, the conversation logged
    /// to `log` if given, and every event handed to `report`.
    ///
    /// No server starts until a document in its language is opened.
    /// `report` runs on the runtime's thread, so it should do nothing but put
    /// the event on the editor's channel.
    ///
    /// # Errors
    ///
    /// If the log cannot be created, or the runtime or its thread cannot be.
    pub fn start(
        servers: BTreeMap<String, ServerSpec>,
        log: Option<&Path>,
        report: Box<dyn Fn(Event) + Send + Sync>,
    ) -> io::Result<Self> {
        let log = match log {
            Some(path) => Log::to_file(path)?,
            None => Log::none(),
        };
        Self::with(servers, log, Arc::from(report), crate::server::processes(), Timing::default())
    }

    pub(crate) fn with(
        specs: BTreeMap<String, ServerSpec>,
        log: Log,
        report: Report,
        launcher: Launcher,
        timing: Timing,
    ) -> io::Result<Self> {
        let (commands, receiver) = unbounded_channel();
        let (done, finished) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let timing = Arc::new(timing);
        let router = Router {
            shared: Shared { report, launcher, timing, log },
            servers: Vec::new(),
            docs: HashMap::new(),
        };
        std::thread::Builder::new().name("nun-lsp".into()).spawn(move || {
            runtime.block_on(router.run(receiver));
            // Dropping the runtime drops every process still running, and
            // each is killed as it goes.
            drop(runtime);
            let _ = done.send(());
        })?;
        Ok(Self {
            specs,
            commands,
            finished: Some(finished),
            next_request: 0,
            docs: HashMap::new(),
            servers: HashMap::new(),
            pending: HashMap::new(),
            diagnostics: HashMap::new(),
        })
    }

    fn send(&self, command: Command) {
        // The runtime only goes away at shutdown, when nothing is listening
        // for an answer anyway.
        let _ = self.commands.send(command);
    }

    // ── documents ───────────────────────────────────────────────────────────

    /// Start following a document at `path` holding `text`, starting its
    /// server if this is the first document it serves.
    ///
    /// Whether there is a server for it. When there is, the caller keeps the
    /// buffer's edits (`Buffer::keep_edits`) and passes them to
    /// [`Lsp::change`]. Opening a document that is already open — a reload, a
    /// rename — starts it afresh.
    pub fn open(&mut self, doc: DocId, path: &Path, text: &Rope) -> bool {
        let previous = self.docs.get(&doc).map(|document| document.version);
        if previous.is_some() {
            self.close(doc);
        }
        let Some(language) = language::of_path(path) else { return false };
        let Some(spec) = self.specs.get(language.config) else { return false };
        let optional = spec.optional;
        let spec = Spec { command: spec.command.clone(), args: spec.args.clone() };
        // Carrying on from the old version keeps every version of a document
        // id distinct, so an answer about the old text is never taken for one
        // about the new.
        let version = previous.map_or(0, |version| version + 1);
        self.docs.insert(doc, Doc { version, config: language.config, server: None, path: None });
        let path = path.to_path_buf();
        let text = text.clone();
        self.send(Command::Open(Opening { doc, path, language, spec, optional, version, text }));
        true
    }

    /// The document changed, by `edits` — everything `Buffer::take_edits`
    /// returned since the last call — and now holds `text`.
    pub fn change(&mut self, doc: DocId, edits: Vec<Edit>, text: &Rope) {
        if edits.is_empty() {
            return;
        }
        let Some(document) = self.docs.get_mut(&doc) else { return };
        document.version += 1;
        let version = document.version;
        self.send(Command::Change { doc, version, edits, text: text.clone() });
    }

    /// The document was saved.
    pub fn save(&mut self, doc: DocId) {
        if self.docs.contains_key(&doc) {
            self.send(Command::Save { doc });
        }
    }

    /// Stop following a document. Anything still asked about it is cancelled.
    pub fn close(&mut self, doc: DocId) {
        if self.docs.remove(&doc).is_none() {
            return;
        }
        self.cancel_all(doc);
        self.send(Command::Close { doc });
    }

    /// Whether a document is followed.
    #[must_use]
    pub fn is_open(&self, doc: DocId) -> bool {
        self.docs.contains_key(&doc)
    }

    /// The version the server has of a document — the one requests are asked
    /// about, and the one a [`Response`] is compared against.
    #[must_use]
    pub fn version(&self, doc: DocId) -> Option<i32> {
        self.docs.get(&doc).map(|document| document.version)
    }

    // ── requests ────────────────────────────────────────────────────────────

    /// Ask the document's server `R`, answered within the default timeout.
    ///
    /// The answer comes back later as [`Handled::Response`] with the id
    /// returned here. Asking again before it has is fine; cancel the first
    /// with [`Lsp::cancel`] if its answer is no longer wanted.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] at once when the document has no server, or its
    /// server is not initialized; [`Error::Malformed`] if the parameters do
    /// not serialize.
    pub fn request<R: lsp_types::request::Request>(
        &mut self,
        doc: DocId,
        params: R::Params,
    ) -> Result<RequestId, Error> {
        let params =
            serde_json::to_value(params).map_err(|error| Error::Malformed(error.to_string()))?;
        self.request_raw(doc, R::METHOD, params, None)
    }

    /// [`Lsp::request`] with a timeout of its own — shorter for something
    /// asked on every keystroke, longer for a search of the whole project.
    ///
    /// # Errors
    ///
    /// As [`Lsp::request`].
    pub fn request_within<R: lsp_types::request::Request>(
        &mut self,
        doc: DocId,
        params: R::Params,
        timeout: Duration,
    ) -> Result<RequestId, Error> {
        let params =
            serde_json::to_value(params).map_err(|error| Error::Malformed(error.to_string()))?;
        self.request_raw(doc, R::METHOD, params, Some(timeout))
    }

    /// Ask by method name, for anything `lsp_types` has no type for.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] as for [`Lsp::request`].
    pub fn request_raw(
        &mut self,
        doc: DocId,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<RequestId, Error> {
        let document = self.docs.get(&doc).ok_or(Error::NotReady)?;
        let server = document.server.ok_or(Error::NotReady)?;
        if self.servers.get(&server).is_none_or(|known| known.status != Status::Ready) {
            return Err(Error::NotReady);
        }
        let id = RequestId(self.next_request);
        self.next_request += 1;
        let version = document.version;
        self.pending.insert(id, doc);
        self.send(Command::Request {
            id,
            doc,
            version,
            method: method.to_string(),
            params,
            timeout,
        });
        Ok(id)
    }

    /// Tell the document's server something, wanting no answer. Dropped if it
    /// is not ready.
    pub fn notify<N: lsp_types::notification::Notification>(
        &mut self,
        doc: DocId,
        params: N::Params,
    ) {
        if let Ok(params) = serde_json::to_value(params)
            && self.docs.contains_key(&doc)
        {
            self.send(Command::Notify { doc, method: N::METHOD.to_string(), params });
        }
    }

    /// The answer to `id` is no longer wanted: the server is told to stop
    /// working on it, and the answer, if it comes, is dropped.
    pub fn cancel(&mut self, id: RequestId) {
        if let Some(doc) = self.pending.remove(&id) {
            self.send(Command::Cancel { id, doc });
        }
    }

    /// Cancel everything asked about a document.
    pub fn cancel_all(&mut self, doc: DocId) {
        let ids: Vec<RequestId> =
            self.pending.iter().filter(|(_, about)| **about == doc).map(|(id, _)| *id).collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Whether `id` has been asked and neither answered nor cancelled.
    #[must_use]
    pub fn is_pending(&self, id: RequestId) -> bool {
        self.pending.contains_key(&id)
    }

    // ── what the servers said ───────────────────────────────────────────────

    /// Take in an event from the runtime, and say what the editor should do.
    pub fn handle(&mut self, event: Event) -> Handled {
        match event {
            Event::Attached { doc, server, name, path } => {
                let Some(document) = self.docs.get_mut(&doc) else { return Handled::Nothing };
                document.server = Some(server);
                document.path = Some(path);
                let config = document.config;
                self.servers.entry(server).or_insert_with(|| Known {
                    name,
                    config,
                    status: Status::Starting,
                    capabilities: None,
                    encoding: Encoding::default(),
                    progress: None,
                });
                Handled::Redraw
            }
            Event::Initialized { server, capabilities, encoding } => {
                if let Some(known) = self.servers.get_mut(&server) {
                    known.capabilities = Some(capabilities);
                    known.encoding = encoding;
                }
                Handled::Nothing
            }
            Event::Status { server, status } => self.status(server, status),
            Event::Progress { server, text } => {
                let Some(known) = self.servers.get_mut(&server) else { return Handled::Nothing };
                known.progress = text;
                Handled::Redraw
            }
            Event::Response(response) => {
                if self.pending.remove(&response.id).is_some() {
                    Handled::Response(response)
                } else {
                    Handled::Nothing
                }
            }
            Event::Diagnostics { path, published } => {
                let shown =
                    self.docs.values().any(|document| document.path.as_ref() == Some(&path));
                if published.diagnostics.is_empty() {
                    self.diagnostics.remove(&path);
                } else {
                    self.diagnostics.insert(path, published);
                }
                if shown { Handled::Redraw } else { Handled::Nothing }
            }
            Event::ApplyEdit(request) => Handled::ApplyEdit(request),
            Event::Message { server, kind, text } => {
                // Errors and warnings are for the person; the rest is chatter,
                // and is in the log for anyone who wants it.
                if kind != MessageType::ERROR && kind != MessageType::WARNING {
                    return Handled::Nothing;
                }
                let name = self
                    .servers
                    .get(&server)
                    .map_or("The language server", |known| known.name.as_str());
                Handled::Notice(format!("{name}: {text}"))
            }
        }
    }

    fn status(&mut self, server: ServerId, status: Status) -> Handled {
        let Some(known) = self.servers.get_mut(&server) else { return Handled::Nothing };
        known.status = status.clone();
        if status != Status::Ready {
            known.progress = None;
        }
        let name = known.name.clone();
        let optional = self.specs.get(known.config).is_none_or(|spec| spec.optional);
        // What a server that has gone for good said is no longer being kept
        // up to date. One restarting keeps its marks until it says again.
        if matches!(status, Status::GaveUp { .. } | Status::Missing | Status::Stopped) {
            self.diagnostics.retain(|_, published| published.server != server);
        }
        match status {
            Status::GaveUp { attempts, why } => Handled::Notice(format!(
                "{name} crashed {attempts} times in a row, so nun has stopped restarting it ({why}). \
                 Click its name in the status line to try again."
            )),
            Status::Missing if !optional => Handled::Notice(format!(
                "{name} is not installed, or not on PATH. `nun config` shows the [lsp] settings."
            )),
            _ => Handled::Redraw,
        }
    }

    /// Tell a server whether the edit it asked for was made: `Ok` when it
    /// was, in full, or why it was not. An answer for a server that has
    /// restarted since it asked goes nowhere, as nothing is waiting for it.
    pub fn answer_edit(&mut self, request: EditRequest, result: Result<(), String>) {
        let EditRequest { server, run, id, .. } = request;
        self.send(Command::AnswerEdit { server, run, id, result });
    }

    /// Stop and start again the server of a document — after a crash, after
    /// giving up, or to pick up a newly installed one. Its name, if it has a
    /// server.
    pub fn restart(&mut self, doc: DocId) -> Option<&str> {
        let server = self.docs.get(&doc)?.server?;
        self.send(Command::Restart { server });
        self.servers.get(&server).map(|known| known.name.as_str())
    }

    fn known(&self, doc: DocId) -> Option<&Known> {
        self.servers.get(&self.docs.get(&doc)?.server?)
    }

    /// What the status line shows for a document's server. `None` when it has
    /// none, or its server is an optional one that is not installed.
    #[must_use]
    pub fn indicator(&self, doc: DocId) -> Option<Indicator<'_>> {
        let known = self.known(doc)?;
        let optional = self.specs.get(known.config).is_none_or(|spec| spec.optional);
        if known.status == Status::Missing && optional {
            return None;
        }
        Some(Indicator {
            name: &known.name,
            status: &known.status,
            progress: known.progress.as_deref(),
        })
    }

    /// What a document's server can do, once it is initialized and running.
    #[must_use]
    pub fn capabilities(&self, doc: DocId) -> Option<&Capabilities> {
        let known = self.known(doc).filter(|known| known.status == Status::Ready)?;
        known.capabilities.as_ref()
    }

    /// The unit a document's server counts positions in.
    #[must_use]
    pub fn encoding(&self, doc: DocId) -> Option<Encoding> {
        self.known(doc).map(|known| known.encoding)
    }

    /// The document as the protocol names it.
    #[must_use]
    pub fn identifier(&self, doc: DocId) -> Option<TextDocumentIdentifier> {
        let path = self.docs.get(&doc)?.path.as_ref()?;
        Some(TextDocumentIdentifier { uri: crate::uri::from_path(path)? })
    }

    /// A char index in the document's `text` as its server's position.
    #[must_use]
    pub fn position(&self, doc: DocId, text: &Rope, char: usize) -> Option<Position> {
        Some(self.encoding(doc)?.position(text, char))
    }

    /// The server's position as a char index into the document's `text`.
    #[must_use]
    pub fn char_index(&self, doc: DocId, text: &Rope, position: Position) -> Option<usize> {
        Some(self.encoding(doc)?.char_index(text, position))
    }

    /// A server's edits to a document's `text` as buffer edits, ready for
    /// `Buffer::apply_batch`. See [`Encoding::edits`].
    #[must_use]
    pub fn edits(
        &self,
        doc: DocId,
        text: &Rope,
        edits: &[lsp_types::TextEdit],
    ) -> Option<Vec<Edit>> {
        Some(self.encoding(doc)?.edits(text, edits))
    }

    /// The document and a place in it, the parameters most requests start
    /// from: hover, completion, definition, references, rename.
    #[must_use]
    pub fn position_params(
        &self,
        doc: DocId,
        text: &Rope,
        char: usize,
    ) -> Option<TextDocumentPositionParams> {
        Some(TextDocumentPositionParams {
            text_document: self.identifier(doc)?,
            position: self.position(doc, text, char)?,
        })
    }

    /// The diagnostics last published for a document.
    #[must_use]
    pub fn diagnostics(&self, doc: DocId) -> Option<&Published> {
        self.diagnostics.get(self.docs.get(&doc)?.path.as_ref()?)
    }

    /// Every file with diagnostics, open or not, and what was published.
    pub fn all_diagnostics(&self) -> impl Iterator<Item = (&Path, &Published)> {
        self.diagnostics.iter().map(|(path, published)| (path.as_path(), published))
    }

    /// Shut every server down: ask each to, give them until `within` has
    /// passed, then kill whatever is left. Blocks for at most `within` and a
    /// little over — the one call on this handle that waits, meant for after
    /// the terminal has been put back.
    pub fn shutdown(&mut self, within: Duration) {
        let Some(finished) = self.finished.take() else { return };
        let deadline = std::time::Instant::now() + within;
        self.send(Command::Shutdown { deadline });
        let _ = finished.recv_timeout(within + Duration::from_millis(250));
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        self.shutdown(Duration::from_millis(500));
    }
}

/// A server running for one command in one root.
struct Running {
    spec: Spec,
    root: PathBuf,
    id: ServerId,
    inbox: UnboundedSender<ToServer>,
    task: tokio::task::JoinHandle<()>,
}

/// The runtime's main task: starts servers as documents need them, and
/// passes each message to the server it is for.
struct Router {
    shared: Shared,
    servers: Vec<Running>,
    /// Which server each document is on, by its place in `servers`.
    docs: HashMap<DocId, usize>,
}

/// What `name` calls itself in the status line: its command, without a path.
fn name_of(spec: &Spec) -> String {
    Path::new(&spec.command)
        .file_name()
        .map_or_else(|| spec.command.clone(), |name| name.to_string_lossy().into_owned())
}

impl Router {
    fn deliver(&self, doc: DocId, message: ToServer) {
        if let Some(running) = self.docs.get(&doc).and_then(|&index| self.servers.get(index)) {
            let _ = running.inbox.send(message);
        }
    }

    /// The server for `spec` in `root`, started if it is not running.
    fn server(&mut self, spec: Spec, root: PathBuf, optional: bool) -> usize {
        if let Some(index) =
            self.servers.iter().position(|running| running.spec == spec && running.root == root)
        {
            return index;
        }
        let id = ServerId(u32::try_from(self.servers.len()).unwrap_or(u32::MAX));
        let (inbox, receiver) = unbounded_channel();
        let server = Server::new(
            id,
            name_of(&spec),
            spec.clone(),
            root.clone(),
            optional,
            receiver,
            self.shared.clone(),
        );
        let task = tokio::spawn(server.run());
        self.servers.push(Running { spec, root, id, inbox, task });
        self.servers.len() - 1
    }

    fn open(&mut self, opening: Opening) {
        let Opening { doc, path, language, spec, optional, version, text } = opening;
        // The path a server is told, and the folder it runs in, both come
        // from the filesystem, which is why they are worked out here and not
        // where the document was opened.
        let path =
            std::fs::canonicalize(&path).or_else(|_| std::path::absolute(&path)).unwrap_or(path);
        let Some(uri) = crate::uri::from_path(&path) else { return };
        let index = self.server(spec, language.root(&path), optional);
        self.docs.insert(doc, index);
        let running = &self.servers[index];
        (self.shared.report)(Event::Attached {
            doc,
            server: running.id,
            name: name_of(&running.spec),
            path,
        });
        let _ =
            running.inbox.send(ToServer::Open { doc, uri, language: language.id, version, text });
    }

    /// Route until shut down, then shut every server down by the deadline.
    async fn run(mut self, mut commands: UnboundedReceiver<Command>) {
        let mut deadline = None;
        while let Some(command) = commands.recv().await {
            match command {
                Command::Open(opening) => self.open(opening),
                Command::Change { doc, version, edits, text } => {
                    self.deliver(doc, ToServer::Change { doc, version, edits, text });
                }
                Command::Save { doc } => self.deliver(doc, ToServer::Save { doc }),
                Command::Close { doc } => {
                    self.deliver(doc, ToServer::Close { doc });
                    self.docs.remove(&doc);
                }
                Command::Request { id, doc, version, method, params, timeout } => {
                    if self.docs.contains_key(&doc) {
                        self.deliver(
                            doc,
                            ToServer::Request { id, doc, version, method, params, timeout },
                        );
                    } else {
                        let result = Err(Error::NotReady);
                        (self.shared.report)(Event::Response(Response {
                            id,
                            doc,
                            version,
                            result,
                        }));
                    }
                }
                Command::Notify { doc, method, params } => {
                    self.deliver(doc, ToServer::Notify { method, params });
                }
                Command::Cancel { id, doc } => self.deliver(doc, ToServer::Cancel { id }),
                Command::AnswerEdit { server, run, id, result } => {
                    if let Some(running) = self.servers.iter().find(|running| running.id == server)
                    {
                        let _ = running.inbox.send(ToServer::AnswerEdit { run, id, result });
                    }
                }
                Command::Restart { server } => {
                    if let Some(running) = self.servers.iter().find(|running| running.id == server)
                    {
                        let _ = running.inbox.send(ToServer::Restart);
                    }
                }
                Command::Shutdown { deadline: when } => {
                    deadline = Some(tokio::time::Instant::from_std(when));
                    break;
                }
            }
        }
        // Asked to shut down, or the handle is gone: either way, every server
        // is asked to stop by the deadline, and anything left after it is
        // killed as the runtime is dropped.
        let reap = self.shared.timing.reap;
        let deadline = deadline.unwrap_or_else(|| tokio::time::Instant::now() + reap);
        for running in &self.servers {
            let _ = running.inbox.send(ToServer::Shutdown { deadline });
        }
        for running in self.servers {
            let _ = tokio::time::timeout_at(deadline + reap, running.task).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::fake::{self, Script, Seen};

    /// A handle, and the channel its events arrive on, as the editor has them.
    struct Editor {
        lsp: Lsp,
        events: Receiver<Event>,
        dir: tempfile::TempDir,
    }

    fn specs(command: &str, args: &[&str], optional: bool) -> BTreeMap<String, ServerSpec> {
        let spec = ServerSpec {
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            optional,
        };
        BTreeMap::from([("rust".to_string(), spec)])
    }

    fn editor(
        specs: BTreeMap<String, ServerSpec>,
        launcher: Launcher,
        timing: Timing,
        log: Log,
    ) -> Editor {
        let (sender, events) = std::sync::mpsc::channel();
        let report: Report = Arc::new(move |event| {
            let _ = sender.send(event);
        });
        let lsp = Lsp::with(specs, log, report, launcher, timing).unwrap();
        Editor { lsp, events, dir: tempfile::tempdir().unwrap() }
    }

    fn with_fake(script: Script) -> (Editor, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let launcher = fake::launcher(script, seen.clone());
        (editor(specs("fake", &[], true), launcher, Timing::default(), Log::none()), seen)
    }

    impl Editor {
        fn file(&self, name: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            std::fs::write(&path, "").unwrap();
            path
        }

        /// Pump events into the handle, as the editor's loop does, until one
        /// is handled into something `wanted` accepts.
        fn until(&mut self, wanted: impl Fn(&Handled) -> bool) -> Handled {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let event = self.events.recv_timeout(left).expect("the event came in time");
                let handled = self.lsp.handle(event);
                if wanted(&handled) {
                    return handled;
                }
            }
        }

        fn until_ready(&mut self, doc: DocId) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while self.lsp.capabilities(doc).is_none() {
                let left = deadline.saturating_duration_since(Instant::now());
                let event = self.events.recv_timeout(left).expect("ready in time");
                self.lsp.handle(event);
            }
        }
    }

    #[test]
    fn a_document_with_no_server_is_not_followed() {
        let (mut editor, seen) = with_fake(Script::default());
        let notes = editor.file("notes.txt");
        assert!(!editor.lsp.open(1, &notes, &Rope::new()));
        assert!(!editor.lsp.is_open(1));
        assert_eq!(editor.lsp.request_raw(1, "test/echo", json!(1), None), Err(Error::NotReady));
        let python = editor.file("x.py");
        assert!(!editor.lsp.open(2, &python, &Rope::new()), "python has no server configured here");
        assert_eq!(seen.launches.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing started");
    }

    #[test]
    fn an_answer_arrives_as_a_response_and_a_cancelled_one_never_does() {
        let (mut editor, _) = with_fake(Script::default());
        let path = editor.file("main.rs");
        assert!(editor.lsp.open(1, &path, &Rope::from_str("fn main() {}")));
        assert_eq!(
            editor.lsp.request_raw(1, "test/echo", json!(1), None),
            Err(Error::NotReady),
            "not yet"
        );
        editor.until_ready(1);
        assert_eq!(editor.lsp.indicator(1).map(|indicator| indicator.label()), Some("fake".into()));
        assert!(editor.lsp.capabilities(1).is_some_and(|caps| caps.hover_provider.is_some()));

        let cancelled = editor.lsp.request_raw(1, "test/slow", json!({ "ms": 50 }), None).unwrap();
        editor.lsp.cancel(cancelled);
        assert!(!editor.lsp.is_pending(cancelled));
        // Asked after, answered after: by the time this one's answer is
        // handled, the slow one's would have been too.
        let slow = editor.lsp.request_raw(1, "test/slow", json!({ "ms": 200 }), None).unwrap();
        let Handled::Response(response) =
            editor.until(|handled| matches!(handled, Handled::Response(_)))
        else {
            unreachable!()
        };
        assert_eq!(response.id, slow, "the cancelled request's answer never surfaced");
        assert_eq!(response.version, 0);
        assert_eq!(response.result, Ok(json!({ "ms": 200 })));
    }

    #[test]
    fn edits_bump_the_version_and_diagnostics_are_kept_by_document() {
        let (mut editor, seen) = with_fake(Script::default());
        let path = editor.file("main.rs");
        let mut buffer = nun_core::Buffer::from_text("a\nb");
        buffer.keep_edits(true);
        editor.lsp.open(1, &path, buffer.rope());
        editor.until_ready(1);
        buffer.insert("\n\n");
        editor.lsp.change(1, buffer.take_edits(), buffer.rope());
        assert_eq!(editor.lsp.version(1), Some(1));
        let deadline = Instant::now() + Duration::from_secs(20);
        while editor.lsp.diagnostics(1).is_none_or(|published| published.version != Some(1)) {
            assert!(Instant::now() < deadline, "diagnostics for version 1 arrived");
            if let Ok(event) = editor.events.recv_timeout(Duration::from_millis(50)) {
                editor.lsp.handle(event);
            }
        }
        let published = editor.lsp.diagnostics(1).unwrap();
        assert_eq!(published.diagnostics[0].message, "4 lines");
        assert_eq!(editor.lsp.all_diagnostics().count(), 1);
        let uri = editor.lsp.identifier(1).unwrap().uri;
        assert_eq!(
            seen.document(uri.as_str()).map(|(text, _)| text),
            Some(buffer.rope().to_string())
        );
    }

    #[test]
    fn positions_use_the_encoding_the_server_chose() {
        let (mut editor, _) = with_fake(Script { encoding: Some("utf-8"), ..Script::default() });
        let path = editor.file("main.rs");
        let text = Rope::from_str("😀x");
        editor.lsp.open(1, &path, &text);
        editor.until_ready(1);
        assert_eq!(editor.lsp.encoding(1), Some(Encoding::Utf8));
        let params = editor.lsp.position_params(1, &text, 1).unwrap();
        assert_eq!(params.position, Position { line: 0, character: 4 });
        assert_eq!(editor.lsp.char_index(1, &text, params.position), Some(1));
        assert!(params.text_document.uri.as_str().ends_with("/main.rs"));
    }

    #[test]
    fn closing_a_document_cancels_what_was_asked_about_it() {
        let (mut editor, seen) = with_fake(Script::default());
        let path = editor.file("main.rs");
        editor.lsp.open(1, &path, &Rope::new());
        editor.until_ready(1);
        let asked = editor.lsp.request_raw(1, "test/hang", Value::Null, None).unwrap();
        editor.lsp.close(1);
        assert!(!editor.lsp.is_pending(asked));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !seen.methods().contains(&"textDocument/didClose".to_string()) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let methods = seen.methods();
        let cancel = methods.iter().position(|method| method == "$/cancelRequest");
        let close = methods.iter().position(|method| method == "textDocument/didClose");
        assert!(cancel.is_some() && cancel < close, "{methods:?}");
    }

    #[test]
    fn restarting_by_hand_starts_a_fresh_server() {
        let (mut editor, seen) = with_fake(Script::default());
        let path = editor.file("main.rs");
        editor.lsp.open(1, &path, &Rope::new());
        editor.until_ready(1);
        assert_eq!(editor.lsp.restart(1), Some("fake"));
        editor.until(|_| seen.launches.load(std::sync::atomic::Ordering::SeqCst) == 2);
        editor.until_ready(1);
    }

    #[test]
    fn a_missing_default_server_is_quiet_and_a_configured_one_is_not() {
        for optional in [true, false] {
            let mut editor = editor(
                specs("nun-test-no-such-server", &[], optional),
                crate::server::processes(),
                Timing::default(),
                Log::none(),
            );
            let path = editor.file("main.rs");
            editor.lsp.open(1, &path, &Rope::new());
            let handled = editor.until(|handled| {
                matches!(handled, Handled::Notice(_)) || (optional && *handled == Handled::Redraw)
            });
            if optional {
                // Redraws until it is known to be missing, and then nothing.
                while editor.lsp.indicator(1).is_some() {
                    let event = editor.events.recv_timeout(Duration::from_secs(20)).unwrap();
                    assert!(!matches!(editor.lsp.handle(event), Handled::Notice(_)));
                }
            } else {
                assert!(
                    matches!(&handled, Handled::Notice(text) if text.contains("not installed")),
                    "{handled:?}"
                );
                assert_eq!(
                    editor.lsp.indicator(1).map(|i| i.label()),
                    Some("nun-test-no-such-server not installed".into())
                );
            }
        }
    }

    #[test]
    fn a_process_that_keeps_dying_is_given_up_on_with_its_exit_status() {
        let timing = Timing {
            backoff: Duration::from_millis(10),
            most_backoff: Duration::from_millis(20),
            attempts: 3,
            ..Timing::default()
        };
        let mut editor = editor(
            specs("sh", &["-c", "exit 3"], false),
            crate::server::processes(),
            timing,
            Log::none(),
        );
        let path = editor.file("main.rs");
        editor.lsp.open(1, &path, &Rope::new());
        let Handled::Notice(notice) = editor.until(|handled| matches!(handled, Handled::Notice(_)))
        else {
            unreachable!()
        };
        assert!(notice.contains("sh crashed 3 times"), "{notice}");
        assert!(notice.contains("status 3"), "{notice}");
        assert!(notice.contains("Click"), "the notice says how to try again: {notice}");
        let indicator = editor.lsp.indicator(1).unwrap();
        assert!(indicator.is_trouble());
        assert_eq!(indicator.label(), "sh crashed");
    }

    #[test]
    fn a_default_that_exits_before_saying_anything_is_taken_as_not_installed() {
        // What a rustup proxy does for a component that was never installed:
        // prints to stderr and exits. For a default server that is not a
        // crash worth five restarts and a notice; it is simply not there.
        let quick = Timing { backoff: Duration::from_millis(10), ..Timing::default() };
        let mut quiet = editor(
            specs("sh", &["-c", "exit 1"], true),
            crate::server::processes(),
            quick.clone(),
            Log::none(),
        );
        let path = quiet.file("main.rs");
        quiet.lsp.open(1, &path, &Rope::new());
        let deadline = Instant::now() + Duration::from_secs(20);
        while quiet.lsp.known(1).is_none_or(|known| known.status != Status::Missing) {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = quiet.events.recv_timeout(left).expect("missing in time");
            assert!(!matches!(quiet.lsp.handle(event), Handled::Notice(_)), "not a word");
        }
        assert_eq!(quiet.lsp.indicator(1), None);

        // Configured by hand, the same exit is a crash, and said so.
        let mut loud = editor(
            specs("sh", &["-c", "exit 1"], false),
            crate::server::processes(),
            quick,
            Log::none(),
        );
        let path = loud.file("main.rs");
        loud.lsp.open(1, &path, &Rope::new());
        let handled = loud.until(|handled| matches!(handled, Handled::Notice(_)));
        assert!(
            matches!(&handled, Handled::Notice(text) if text.contains("crashed")),
            "{handled:?}"
        );
    }

    #[test]
    fn a_process_that_never_answers_is_killed_at_shutdown_and_the_log_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("lsp.log");
        let log = Log::to_file(&log_path).unwrap();
        // `sleep` reads nothing and answers nothing: a hung server.
        let mut editor = editor(
            specs("sleep", &["30"], false),
            crate::server::processes(),
            Timing::default(),
            log,
        );
        let path = editor.file("main.rs");
        editor.lsp.open(1, &path, &Rope::new());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !std::fs::read_to_string(&log_path).is_ok_and(|text| text.contains("as process")) {
            assert!(Instant::now() < deadline, "the server was started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        editor.lsp.shutdown(Duration::from_millis(300));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown kept its deadline: {:?}",
            started.elapsed()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(&log_path).unwrap();
            if text.contains("killing it") {
                assert!(
                    text.contains(r#""method":"initialize""#),
                    "the conversation is logged: {text}"
                );
                break;
            }
            assert!(Instant::now() < deadline, "the log says the server was killed:\n{text}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Against the real thing, when it is installed: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs rust-analyzer on PATH"]
    fn rust_analyzer_starts_answers_and_shuts_down() {
        // `cargo test` sets RUSTUP_TOOLCHAIN, which would make a rustup proxy
        // look in this repository's toolchain rather than wherever
        // rust-analyzer is actually installed.
        let found = std::process::Command::new("rustup")
            .args(["which", "--toolchain", "stable", "rust-analyzer"])
            .output();
        let command = found.ok().filter(|output| output.status.success()).map_or_else(
            || "rust-analyzer".to_string(),
            |output| String::from_utf8_lossy(&output.stdout).trim().to_string(),
        );
        let mut editor = editor(
            specs(&command, &[], false),
            crate::server::processes(),
            Timing::default(),
            Log::none(),
        );
        std::fs::write(
            editor.dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(editor.dir.path().join("src")).unwrap();
        let path = editor.dir.path().join("src/main.rs");
        let text = "fn main() {\n    let 😀x = 1;\n}\n";
        std::fs::write(&path, text).unwrap();
        let rope = Rope::from_str(text);
        editor.lsp.open(1, &path, &rope);
        editor.until_ready(1);
        let encoding = editor.lsp.encoding(1);
        assert!(
            matches!(encoding, Some(Encoding::Utf8 | Encoding::Utf32)),
            "it took one nun prefers: {encoding:?}"
        );
        let params = lsp_types::HoverParams {
            text_document_position_params: editor.lsp.position_params(1, &rope, 21).unwrap(),
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        };
        let asked = editor.lsp.request::<lsp_types::request::HoverRequest>(1, params).unwrap();
        let Handled::Response(response) =
            editor.until(|handled| matches!(handled, Handled::Response(_)))
        else {
            unreachable!()
        };
        assert_eq!(response.id, asked);
        assert!(response.parse::<lsp_types::request::HoverRequest>().is_ok(), "{response:?}");
        let started = Instant::now();
        editor.lsp.shutdown(Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
