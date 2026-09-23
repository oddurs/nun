//! One language server, from start to shutdown, as one task.
//!
//! The task owns everything about its server: the process, the documents it
//! has open and the server's copy of each, the requests it has not answered.
//! The editor talks to it through a channel and hears back through another,
//! and never waits for it — so whatever the server does, from answering slowly
//! to not answering at all, the worst the editor sees is no answer.
//!
//! Reading and writing each have a task of their own, so this one only ever
//! waits on channels and timers. A server that has stopped reading its input
//! fills the pipe and blocks the writer, not this; the timeouts still fire,
//! and killing the process unblocks the writer.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lsp_types::{
    InitializeResult, ProgressParams, ProgressParamsValue, PublishDiagnosticsParams,
    ServerCapabilities, ShowMessageParams, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncSaveOptions, Uri, WorkDoneProgress,
};
use nun_core::Edit;
use ropey::Rope;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep_until, timeout_at};

use crate::event::{
    Capabilities, DocId, Error, Event, Published, RequestId, Response, ServerId, Status,
};
use crate::log::{Direction, Log};
use crate::position::Encoding;
use crate::rpc::{self, Failure, Message, ReadError};
use crate::sync::{Shadow, SyncKind};

/// How to start a server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Spec {
    pub command: String,
    pub args: Vec<String>,
}

/// A running server's streams, and its process when it is one.
pub(crate) struct Connection {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
    pub stderr: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub child: Option<tokio::process::Child>,
}

/// Starts a server: a process, normally, and something in-process in tests.
/// Told whether to keep stderr, which is wanted only for the log.
pub(crate) type Launcher = Arc<dyn Fn(&Spec, &Path, bool) -> io::Result<Connection> + Send + Sync>;

/// Where the task's news goes.
pub(crate) type Report = Arc<dyn Fn(Event) + Send + Sync>;

/// What every server's task shares with the rest.
#[derive(Clone)]
pub(crate) struct Shared {
    pub report: Report,
    pub launcher: Launcher,
    pub timing: Arc<Timing>,
    pub log: Log,
}

/// Start servers as child processes.
pub(crate) fn processes() -> Launcher {
    Arc::new(|spec: &Spec, root: &Path, keep_stderr: bool| {
        let mut command = tokio::process::Command::new(&spec.command);
        command
            .args(&spec.args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(if keep_stderr {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            // However nun goes, the server goes with it.
            .kill_on_drop(true);
        // A group of its own, so the terminal's signals are nun's to handle:
        // a server does not need to hear about a window resize or a suspend.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let missing = || io::Error::other("the server's pipes were not set up");
        let reader = child.stdout.take().ok_or_else(missing)?;
        let writer = child.stdin.take().ok_or_else(missing)?;
        let stderr = child.stderr.take();
        Ok(Connection {
            reader: Box::new(reader),
            writer: Box::new(writer),
            stderr: stderr.map(|stderr| Box::new(stderr) as Box<dyn AsyncRead + Send + Unpin>),
            child: Some(child),
        })
    })
}

/// How long everything is given.
#[derive(Debug, Clone)]
pub(crate) struct Timing {
    /// For `initialize` to be answered.
    pub initialize: Duration,
    /// For a request to be answered, unless it says otherwise.
    pub request: Duration,
    /// Before the first restart; each after it waits twice as long.
    pub backoff: Duration,
    /// The longest wait before a restart.
    pub most_backoff: Duration,
    /// Starts in a row that crash before nun gives up.
    pub attempts: u32,
    /// How long a server has to stay up for its crashes to be forgiven.
    pub stable: Duration,
    /// Requests in a row that time out before a server is taken as hung.
    pub hung_after: u32,
    /// The least time between two progress reports passed on.
    pub progress_every: Duration,
    /// For an exited process to be reaped, before it is killed.
    pub reap: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(20),
            request: Duration::from_secs(10),
            backoff: Duration::from_millis(500),
            most_backoff: Duration::from_secs(8),
            attempts: 5,
            stable: Duration::from_secs(60),
            hung_after: 3,
            progress_every: Duration::from_millis(100),
            reap: Duration::from_secs(1),
        }
    }
}

impl Timing {
    /// The wait before start number `attempt + 1`, after `attempt` crashes.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.backoff.saturating_mul(1 << doublings).min(self.most_backoff)
    }
}

/// Something for a server to do.
#[derive(Debug)]
pub(crate) enum ToServer {
    Open {
        doc: DocId,
        uri: Uri,
        language: &'static str,
        version: i32,
        text: Rope,
    },
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
        method: String,
        params: Value,
    },
    Cancel {
        id: RequestId,
    },
    Restart,
    Shutdown {
        deadline: Instant,
    },
}

/// A document the server has, or will have once it is running.
#[derive(Debug)]
struct Doc {
    uri: Uri,
    language: &'static str,
    version: i32,
    shadow: Shadow,
    /// Whether this run of the server has been sent `didOpen` for it.
    opened: bool,
}

/// How a run of the server ended.
#[derive(Debug)]
enum End {
    Shutdown,
    Restart,
    Missing,
    Crashed(String),
    /// Exited before saying a word. On a first run that is usually not a
    /// crash at all but a server that is not really there: a rustup proxy
    /// for a component that was never installed exits at once, like this.
    Silent(String),
}

/// Why waiting between runs stopped.
enum Wake {
    Retry,
    Restart,
    Shutdown,
}

/// A request on its way.
#[derive(Debug)]
struct InFlight {
    id: RequestId,
    doc: DocId,
    version: i32,
    deadline: Instant,
}

/// One run of the server: from its start to its exit.
struct Session {
    /// Frames for the writer. `None` once the server's input is closed.
    outgoing: Option<UnboundedSender<Vec<u8>>>,
    /// The id the next request of ours takes on the wire.
    next: i64,
    in_flight: HashMap<i64, InFlight>,
    /// When `initialize` must be answered by, while it has not been.
    initializing: Option<Instant>,
    encoding: Encoding,
    sync: SyncKind,
    open_close: bool,
    /// Whether to send `didSave`, and whether with the text.
    save: Option<bool>,
    timeouts_in_a_row: u32,
    /// Whether the server has said anything at all.
    heard: bool,
    /// Work the server has announced and not finished: token, then text.
    progress: Vec<(String, String)>,
    progress_said: Option<String>,
    progress_at: Option<Instant>,
}

/// The wire id of `initialize`, which is always the first request of a run.
const INITIALIZE: i64 = 0;

pub(crate) struct Server {
    id: ServerId,
    name: String,
    spec: Spec,
    root: PathBuf,
    /// Whether this server is only a default, which may well not be
    /// installed, and should be quiet about it if it is not.
    optional: bool,
    inbox: UnboundedReceiver<ToServer>,
    shared: Shared,
    /// Whether any run of it has got as far as initializing.
    been_ready: bool,
    docs: HashMap<DocId, Doc>,
}

impl Server {
    pub(crate) fn new(
        id: ServerId,
        name: String,
        spec: Spec,
        root: PathBuf,
        optional: bool,
        inbox: UnboundedReceiver<ToServer>,
        shared: Shared,
    ) -> Self {
        Self {
            id,
            name,
            spec,
            root,
            optional,
            inbox,
            shared,
            been_ready: false,
            docs: HashMap::new(),
        }
    }

    fn status(&self, status: Status) {
        self.note(&format!("status: {status:?}"));
        (self.shared.report)(Event::Status { server: self.id, status });
    }

    fn note(&self, text: &str) {
        self.shared.log.write(&self.name, Direction::Note, text);
    }

    /// Run the server until it is shut down, starting it again whenever it
    /// crashes, until it has crashed too often.
    pub(crate) async fn run(mut self) {
        let mut attempts = 0u32;
        loop {
            self.status(Status::Starting);
            let started = Instant::now();
            let end = match (self.shared.launcher)(&self.spec, &self.root, self.shared.log.is_on())
            {
                Ok(connection) => self.session(connection).await,
                Err(error) if error.kind() == io::ErrorKind::NotFound => End::Missing,
                Err(error) => End::Crashed(format!("could not be started: {error}")),
            };
            let end = match end {
                End::Silent(why) if self.optional && !self.been_ready => {
                    self.note(&format!("{why} before saying anything; taking it as not installed"));
                    End::Missing
                }
                End::Silent(why) => End::Crashed(why),
                end => end,
            };
            let wake = match end {
                End::Shutdown => Wake::Shutdown,
                End::Restart => Wake::Restart,
                End::Missing => {
                    self.note(&format!("`{}` is not installed", self.spec.command));
                    self.status(Status::Missing);
                    self.idle(None).await
                }
                End::Silent(why) | End::Crashed(why) => {
                    if started.elapsed() >= self.shared.timing.stable {
                        attempts = 0;
                    }
                    attempts += 1;
                    self.note(&format!("crashed: {why}"));
                    if attempts >= self.shared.timing.attempts {
                        self.status(Status::GaveUp { attempts, why });
                        self.idle(None).await
                    } else {
                        let retry_in = self.shared.timing.backoff(attempts);
                        self.status(Status::Crashed { attempt: attempts + 1, retry_in, why });
                        self.idle(Some(Instant::now() + retry_in)).await
                    }
                }
            };
            match wake {
                Wake::Shutdown => {
                    self.status(Status::Stopped);
                    return;
                }
                Wake::Restart => attempts = 0,
                Wake::Retry => {}
            }
        }
    }

    /// Wait for `until`, or for a restart or a shutdown, keeping the documents
    /// up to date meanwhile so the next run opens them as they are.
    async fn idle(&mut self, until: Option<Instant>) -> Wake {
        loop {
            tokio::select! {
                message = self.inbox.recv() => match message {
                    None | Some(ToServer::Shutdown { .. }) => return Wake::Shutdown,
                    Some(ToServer::Restart) => return Wake::Restart,
                    Some(message) => self.offline(message),
                },
                () = sleep_until_maybe(until) => return Wake::Retry,
            }
        }
    }

    /// Keep track of a message while there is no server to send it to.
    fn offline(&mut self, message: ToServer) {
        match message {
            ToServer::Open { doc, uri, language, version, text, .. } => {
                let shadow = Shadow::new(text);
                self.docs.insert(doc, Doc { uri, language, version, shadow, opened: false });
            }
            ToServer::Change { doc, version, text, .. } => {
                if let Some(document) = self.docs.get_mut(&doc) {
                    document.version = version;
                    document.shadow.reset(&text);
                }
            }
            ToServer::Close { doc } => {
                self.docs.remove(&doc);
            }
            ToServer::Request { id, doc, version, .. } => {
                self.answer(id, doc, version, Err(Error::NotReady));
            }
            ToServer::Save { .. }
            | ToServer::Notify { .. }
            | ToServer::Cancel { .. }
            | ToServer::Restart
            | ToServer::Shutdown { .. } => {}
        }
    }

    fn answer(&self, id: RequestId, doc: DocId, version: i32, result: Result<Value, Error>) {
        (self.shared.report)(Event::Response(Response { id, doc, version, result }));
    }

    /// One run of the server, from `initialize` to its exit.
    async fn session(&mut self, connection: Connection) -> End {
        let Connection { reader, writer, stderr, mut child } = connection;
        let pid = child.as_ref().and_then(tokio::process::Child::id);
        self.note(&format!(
            "started `{} {}` in {}{}",
            self.spec.command,
            self.spec.args.join(" "),
            self.root.display(),
            pid.map_or_else(String::new, |pid| format!(" as process {pid}"))
        ));

        let (incoming_sender, mut incoming) = unbounded_channel();
        let reading = tokio::spawn(read_all(reader, incoming_sender));
        let (outgoing, frames) = unbounded_channel();
        let mut writing = tokio::spawn(write_all(writer, frames));
        if let Some(stderr) = stderr {
            tokio::spawn(read_stderr(stderr, self.shared.log.clone(), self.name.clone()));
        }

        let mut session = Session::new(outgoing, Instant::now() + self.shared.timing.initialize);
        self.send(
            &mut session,
            &Message::Request {
                id: json!(INITIALIZE),
                method: "initialize".into(),
                params: initialize_params(&self.root),
            },
        );

        let end = loop {
            let next = session.next_deadline();
            tokio::select! {
                message = self.inbox.recv() => match message {
                    None | Some(ToServer::Shutdown { .. }) => {
                        let deadline = match message {
                            Some(ToServer::Shutdown { deadline }) => deadline,
                            _ => Instant::now() + self.shared.timing.reap,
                        };
                        self.shut_down(&mut session, &mut incoming, &mut child, deadline).await;
                        break End::Shutdown;
                    }
                    Some(ToServer::Restart) => {
                        self.note("restarting, as asked");
                        let deadline = Instant::now() + self.shared.timing.reap;
                        self.shut_down(&mut session, &mut incoming, &mut child, deadline).await;
                        break End::Restart;
                    }
                    Some(message) => self.online(&mut session, message),
                },
                read = incoming.recv() => match read {
                    Some(Ok(value)) => {
                        session.heard = true;
                        if let Some(end) = self.received(&mut session, value) {
                            break end;
                        }
                    }
                    Some(Err(ReadError::Broken(why))) => {
                        break End::Crashed(format!("broke the protocol: {why}"));
                    }
                    Some(Err(ReadError::Closed)) | None => {
                        let why = exited(&mut child, self.shared.timing.reap).await;
                        break if session.heard { End::Crashed(why) } else { End::Silent(why) };
                    }
                },
                () = sleep_until_maybe(next) => {
                    if let Some(end) = self.expire(&mut session) {
                        break end;
                    }
                }
            }
        };

        self.wind_down(&mut session, child.as_mut()).await;
        reading.abort();
        // Its input is closed, so the writer finishes what is queued — an
        // `exit`, say — and stops; unless the server stopped reading, in which
        // case it is stuck, and is stopped.
        let _ = timeout_at(Instant::now() + self.shared.timing.reap, &mut writing).await;
        writing.abort();
        end
    }

    /// Settle everything a run leaves behind: its unanswered requests, its
    /// progress, its documents, and its process if it is still going.
    async fn wind_down(
        &mut self,
        session: &mut Session,
        child: Option<&mut tokio::process::Child>,
    ) {
        // Whatever was asked of this run will not be answered by it.
        for (_, request) in session.in_flight.drain() {
            self.answer(request.id, request.doc, request.version, Err(Error::Stopped));
        }
        if session.progress_said.is_some() {
            (self.shared.report)(Event::Progress { server: self.id, text: None });
        }
        for document in self.docs.values_mut() {
            document.opened = false;
        }
        session.outgoing = None;
        if let Some(child) = child
            && matches!(child.try_wait(), Ok(None))
        {
            self.note("killing it");
            let _ = child.start_kill();
            let _ = timeout_at(Instant::now() + self.shared.timing.reap, child.wait()).await;
        }
    }

    /// Ask the server to shut down and exit, and make sure it has by `deadline`.
    async fn shut_down(
        &self,
        session: &mut Session,
        incoming: &mut UnboundedReceiver<Result<Value, ReadError>>,
        child: &mut Option<tokio::process::Child>,
        deadline: Instant,
    ) {
        if session.initializing.is_none() {
            let wire = session.next;
            session.next += 1;
            self.send(
                session,
                &Message::Request {
                    id: json!(wire),
                    method: "shutdown".into(),
                    params: Value::Null,
                },
            );
            // Only the answer matters now; everything else it says is late.
            let answered = timeout_at(deadline, async {
                while let Some(Ok(value)) = incoming.recv().await {
                    self.log_received(&value);
                    if value.get("id").and_then(Value::as_i64) == Some(wire)
                        && value.get("method").is_none()
                    {
                        return true;
                    }
                }
                false
            })
            .await;
            if answered != Ok(true) {
                self.note("did not answer `shutdown` in time");
            }
            self.send(
                session,
                &Message::Notification { method: "exit".into(), params: Value::Null },
            );
        }
        // Closing its input is the last thing it hears.
        session.outgoing = None;
        if let Some(child) = child.as_mut() {
            if let Ok(Ok(status)) = timeout_at(deadline, child.wait()).await {
                self.note(&format!("exited: {status}"));
            } else {
                self.note("still running at the deadline; killing it");
                let _ = child.start_kill();
            }
        }
    }

    fn send(&self, session: &mut Session, message: &Message) {
        let body = message.to_value().to_string();
        self.shared.log.write(&self.name, Direction::Sent, &body);
        if let Some(outgoing) = &session.outgoing {
            let _ = outgoing.send(rpc::frame(&body));
        }
    }

    fn notify(&self, session: &mut Session, method: &str, params: Value) {
        self.send(session, &Message::Notification { method: method.into(), params });
    }

    fn log_received(&self, value: &Value) {
        if self.shared.log.is_on() {
            self.shared.log.write(&self.name, Direction::Received, &value.to_string());
        }
    }

    /// Act on a message from the editor while the server is running.
    fn online(&mut self, session: &mut Session, message: ToServer) {
        let ready = session.initializing.is_none();
        match message {
            ToServer::Open { doc, uri, language, version, text, .. } => {
                let shadow = Shadow::new(text);
                self.docs.insert(doc, Doc { uri, language, version, shadow, opened: false });
                if ready {
                    self.did_open(session, doc);
                }
            }
            ToServer::Change { doc, version, edits, text } => {
                let Some(document) = self.docs.get_mut(&doc) else { return };
                document.version = version;
                if !document.opened {
                    document.shadow.reset(&text);
                    return;
                }
                let changes =
                    document.shadow.changes(&edits, &text, session.encoding, session.sync);
                if changes.is_empty() {
                    return;
                }
                let params = json!({
                    "textDocument": { "uri": document.uri, "version": version },
                    "contentChanges": changes,
                });
                self.notify(session, "textDocument/didChange", params);
            }
            ToServer::Save { doc } => {
                let Some(document) = self.docs.get(&doc) else { return };
                let Some(include_text) = session.save.filter(|_| document.opened) else { return };
                let mut params = json!({ "textDocument": { "uri": document.uri } });
                if include_text {
                    params["text"] = Value::String(document.shadow.text().to_string());
                }
                self.notify(session, "textDocument/didSave", params);
            }
            ToServer::Close { doc } => {
                let Some(document) = self.docs.remove(&doc) else { return };
                if document.opened && session.open_close {
                    let params = json!({ "textDocument": { "uri": document.uri } });
                    self.notify(session, "textDocument/didClose", params);
                }
            }
            ToServer::Request { id, doc, version, method, params, timeout } => {
                if !ready || !self.docs.get(&doc).is_some_and(|document| document.opened) {
                    self.answer(id, doc, version, Err(Error::NotReady));
                    return;
                }
                let wire = session.next;
                session.next += 1;
                let deadline = Instant::now() + timeout.unwrap_or(self.shared.timing.request);
                session.in_flight.insert(wire, InFlight { id, doc, version, deadline });
                self.send(session, &Message::Request { id: json!(wire), method, params });
            }
            ToServer::Notify { method, params, .. } => {
                if ready {
                    self.notify(session, &method, params);
                }
            }
            ToServer::Cancel { id } => {
                let wire = session
                    .in_flight
                    .iter()
                    .find(|(_, request)| request.id == id)
                    .map(|(wire, _)| *wire);
                if let Some(wire) = wire {
                    session.in_flight.remove(&wire);
                    self.notify(session, "$/cancelRequest", json!({ "id": wire }));
                }
            }
            // Handled by the session loop before they get here.
            ToServer::Restart | ToServer::Shutdown { .. } => {}
        }
    }

    fn did_open(&mut self, session: &mut Session, doc: DocId) {
        let Some(document) = self.docs.get_mut(&doc) else { return };
        document.opened = true;
        if !session.open_close {
            return;
        }
        let params = json!({
            "textDocument": {
                "uri": document.uri,
                "languageId": document.language,
                "version": document.version,
                "text": document.shadow.text().to_string(),
            }
        });
        self.notify(session, "textDocument/didOpen", params);
    }

    /// Act on something the server sent. `Some` when it ends the run.
    fn received(&mut self, session: &mut Session, value: Value) -> Option<End> {
        self.log_received(&value);
        let Some(message) = Message::from_value(value) else {
            self.note(
                "ignoring a message that is neither a request, a notification nor a response",
            );
            return None;
        };
        match message {
            Message::Response { id, result } => {
                let wire = id.as_i64()?;
                if wire == INITIALIZE && session.initializing.is_some() {
                    return self.initialized(session, result);
                }
                // An answer to a request since cancelled or timed out is
                // nobody's business now.
                let request = session.in_flight.remove(&wire)?;
                session.timeouts_in_a_row = 0;
                let result =
                    result.map_err(|Failure { code, message }| Error::Server { code, message });
                self.answer(request.id, request.doc, request.version, result);
            }
            Message::Request { id, method, params } => {
                let result = self.server_request(&method, &params);
                self.send(session, &Message::Response { id, result });
            }
            Message::Notification { method, params } => {
                self.server_notification(session, &method, params);
            }
        }
        None
    }

    /// The answer to `initialize`: what the server can do, and how to talk to
    /// it. Then every document is opened on it.
    fn initialized(
        &mut self,
        session: &mut Session,
        result: Result<Value, Failure>,
    ) -> Option<End> {
        let result = match result {
            Ok(result) => result,
            Err(failure) => {
                return Some(End::Crashed(format!("refused to initialize: {}", failure.message)));
            }
        };
        let result: InitializeResult = match serde_json::from_value(result) {
            Ok(result) => result,
            Err(error) => {
                return Some(End::Crashed(format!("answered `initialize` with nonsense: {error}")));
            }
        };
        let capabilities = result.capabilities;
        session.encoding = Encoding::chosen(capabilities.position_encoding.as_ref());
        (session.sync, session.open_close, session.save) = synchronisation(&capabilities);
        session.initializing = None;
        self.notify(session, "initialized", json!({}));
        let capabilities = Capabilities(Arc::new(capabilities));
        (self.shared.report)(Event::Initialized {
            server: self.id,
            capabilities,
            encoding: session.encoding,
        });
        self.been_ready = true;
        self.status(Status::Ready);
        let mut docs: Vec<DocId> = self.docs.keys().copied().collect();
        docs.sort_unstable();
        for doc in docs {
            self.did_open(session, doc);
        }
        None
    }

    /// Answer something the server asked. Everything a server can ask gets an
    /// answer, even if it is "no": a server waiting on one may wait for ever.
    fn server_request(&self, method: &str, params: &Value) -> Result<Value, Failure> {
        match method {
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability" => Ok(Value::Null),
            // No settings to give: every server falls back to its defaults.
            "workspace/configuration" => {
                let items = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
                Ok(Value::Array(vec![Value::Null; items]))
            }
            "workspace/workspaceFolders" => Ok(json!([workspace_folder(&self.root)])),
            "window/showMessageRequest" => {
                if let Ok(shown) = serde_json::from_value::<ShowMessageParams>(params.clone()) {
                    self.show(shown);
                }
                Ok(Value::Null)
            }
            "workspace/applyEdit" => Ok(json!({
                "applied": false,
                "failureReason": "nun does not apply edits a server asks for",
            })),
            _ => Err(Failure {
                code: rpc::METHOD_NOT_FOUND,
                message: format!("nun does not handle `{method}`"),
            }),
        }
    }

    fn show(&self, shown: ShowMessageParams) {
        (self.shared.report)(Event::Message {
            server: self.id,
            kind: shown.typ,
            text: shown.message,
        });
    }

    fn server_notification(&self, session: &mut Session, method: &str, params: Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Ok(published) = serde_json::from_value::<PublishDiagnosticsParams>(params)
                else {
                    return;
                };
                let Some(path) = crate::uri::to_path(&published.uri) else { return };
                (self.shared.report)(Event::Diagnostics {
                    path,
                    published: Published {
                        server: self.id,
                        version: published.version,
                        diagnostics: published.diagnostics,
                    },
                });
            }
            "window/showMessage" => {
                if let Ok(shown) = serde_json::from_value::<ShowMessageParams>(params) {
                    self.show(shown);
                }
            }
            "$/progress" => {
                if let Ok(progress) = serde_json::from_value::<ProgressParams>(params) {
                    self.progress(session, progress);
                }
            }
            // Already in the log, which is the only place it belongs.
            _ => {}
        }
    }

    /// Keep track of long work the server announces, and pass on what it is
    /// doing now — often enough to watch, not so often that a server
    /// reporting every file it indexes redraws the editor for each one.
    fn progress(&self, session: &mut Session, progress: ProgressParams) {
        let token = match progress.token {
            lsp_types::NumberOrString::Number(number) => number.to_string(),
            lsp_types::NumberOrString::String(text) => text,
        };
        let ProgressParamsValue::WorkDone(work) = progress.value;
        let urgent = match work {
            WorkDoneProgress::Begin(begin) => {
                let text = describe(&begin.title, begin.message.as_deref(), begin.percentage);
                session.progress.push((token, text));
                true
            }
            WorkDoneProgress::Report(report) => {
                if let Some((_, text)) =
                    session.progress.iter_mut().find(|(known, _)| *known == token)
                {
                    let title = text.split(" · ").next().unwrap_or_default().to_string();
                    *text = describe(&title, report.message.as_deref(), report.percentage);
                }
                false
            }
            WorkDoneProgress::End(_) => {
                session.progress.retain(|(known, _)| *known != token);
                true
            }
        };
        let now = Instant::now();
        let due =
            session.progress_at.is_none_or(|at| now >= at + self.shared.timing.progress_every);
        let text = session.progress.last().map(|(_, text)| text.clone());
        if text != session.progress_said && (urgent || due) {
            session.progress_said.clone_from(&text);
            session.progress_at = Some(now);
            (self.shared.report)(Event::Progress { server: self.id, text });
        }
    }

    /// Deadlines that have passed. `Some` when the server is taken as hung.
    fn expire(&mut self, session: &mut Session) -> Option<End> {
        let now = Instant::now();
        if session.initializing.is_some_and(|deadline| now >= deadline) {
            return Some(End::Crashed(format!(
                "did not answer `initialize` within {}s",
                self.shared.timing.initialize.as_secs()
            )));
        }
        let late: Vec<i64> = session
            .in_flight
            .iter()
            .filter(|(_, request)| now >= request.deadline)
            .map(|(wire, _)| *wire)
            .collect();
        for wire in late {
            let Some(request) = session.in_flight.remove(&wire) else { continue };
            self.note(&format!("request {wire} timed out"));
            self.notify(session, "$/cancelRequest", json!({ "id": wire }));
            self.answer(request.id, request.doc, request.version, Err(Error::TimedOut));
            session.timeouts_in_a_row += 1;
        }
        (session.timeouts_in_a_row >= self.shared.timing.hung_after).then(|| {
            End::Crashed(format!(
                "stopped answering: {} requests in a row timed out",
                session.timeouts_in_a_row
            ))
        })
    }
}

impl Session {
    fn new(outgoing: UnboundedSender<Vec<u8>>, initialize_by: Instant) -> Self {
        Self {
            outgoing: Some(outgoing),
            next: INITIALIZE + 1,
            in_flight: HashMap::new(),
            initializing: Some(initialize_by),
            encoding: Encoding::default(),
            sync: SyncKind::default(),
            open_close: true,
            save: None,
            timeouts_in_a_row: 0,
            heard: false,
            progress: Vec::new(),
            progress_said: None,
            progress_at: None,
        }
    }

    /// The next moment something is due: `initialize`, or the soonest request.
    fn next_deadline(&self) -> Option<Instant> {
        self.in_flight.values().map(|request| request.deadline).chain(self.initializing).min()
    }
}

/// "Indexing · 12/340 · 3%", from what a server said.
fn describe(title: &str, message: Option<&str>, percentage: Option<u32>) -> String {
    let mut text = title.to_string();
    if let Some(message) = message.filter(|message| !message.is_empty()) {
        text.push_str(" · ");
        text.push_str(message);
    }
    if let Some(percentage) = percentage {
        let _ = write!(text, " · {percentage}%");
    }
    text
}

/// How to keep a server's documents in step, from what it said it wants: how
/// changes are sent, whether opens and closes are, and whether saves are and
/// with the text.
fn synchronisation(capabilities: &ServerCapabilities) -> (SyncKind, bool, Option<bool>) {
    let kind = |kind: TextDocumentSyncKind| match kind {
        TextDocumentSyncKind::FULL => SyncKind::Full,
        TextDocumentSyncKind::INCREMENTAL => SyncKind::Incremental,
        _ => SyncKind::None,
    };
    match &capabilities.text_document_sync {
        // Said nothing: the protocol's default is to send nothing.
        None => (SyncKind::None, false, None),
        // The older, shorter form: opens, closes and saves go whenever changes do.
        Some(TextDocumentSyncCapability::Kind(change)) => {
            let sync = kind(*change);
            let on = sync != SyncKind::None;
            (sync, on, on.then_some(false))
        }
        Some(TextDocumentSyncCapability::Options(options)) => {
            let save = match &options.save {
                None | Some(TextDocumentSyncSaveOptions::Supported(false)) => None,
                Some(TextDocumentSyncSaveOptions::Supported(true)) => Some(false),
                Some(TextDocumentSyncSaveOptions::SaveOptions(save)) => {
                    Some(save.include_text.unwrap_or(false))
                }
            };
            (options.change.map_or(SyncKind::None, kind), options.open_close.unwrap_or(false), save)
        }
    }
}

fn workspace_folder(root: &Path) -> Value {
    let name = root
        .file_name()
        .map_or_else(|| root.display().to_string(), |name| name.to_string_lossy().into_owned());
    json!({ "uri": crate::uri::from_path(root), "name": name })
}

/// What nun tells a server about itself.
///
/// This is where the client capabilities live. Everything the features built
/// on this client use is declared here once — hover, completion, definition,
/// references, rename, formatting — so that adding a feature means using the
/// capability, not negotiating it again.
fn initialize_params(root: &Path) -> Value {
    let encodings: Vec<String> =
        Encoding::PREFERRED.iter().map(|encoding| encoding.kind().as_str().to_string()).collect();
    let markup = json!(["markdown", "plaintext"]);
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "nun", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": crate::uri::from_path(root),
        "rootPath": root,
        "workspaceFolders": [workspace_folder(root)],
        "capabilities": {
            "general": { "positionEncodings": encodings },
            "window": { "workDoneProgress": true, "showMessage": {} },
            "workspace": { "configuration": true, "workspaceFolders": true, "applyEdit": false },
            "textDocument": {
                "synchronization": { "didSave": true, "willSave": false, "willSaveWaitUntil": false },
                "publishDiagnostics": {
                    "relatedInformation": true,
                    "versionSupport": true,
                    "codeDescriptionSupport": true,
                    "tagSupport": { "valueSet": [1, 2] },
                },
                "hover": { "contentFormat": markup },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": true,
                        "documentationFormat": markup,
                        "labelDetailsSupport": true,
                        "deprecatedSupport": true,
                        "tagSupport": { "valueSet": [1] },
                        "preselectSupport": true,
                        "insertReplaceSupport": true,
                        "insertTextModeSupport": { "valueSet": [1, 2] },
                        // Documentation and detail are asked for when an item
                        // is selected, so a long list costs the server less to
                        // send. The edits are not: an import added late would
                        // arrive after the item had been accepted.
                        "resolveSupport": { "properties": ["documentation", "detail"] },
                    },
                },
                "definition": { "linkSupport": false },
                "references": {},
                "rename": { "prepareSupport": true },
                "formatting": {},
            },
        },
    })
}

async fn sleep_until_maybe(until: Option<Instant>) {
    match until {
        Some(until) => sleep_until(until).await,
        None => std::future::pending().await,
    }
}

/// How a server that closed its output went, for the status line.
async fn exited(child: &mut Option<tokio::process::Child>, reap: Duration) -> String {
    let Some(child) = child.as_mut() else { return "closed its connection".into() };
    match timeout_at(Instant::now() + reap, child.wait()).await {
        Ok(Ok(status)) => match status.code() {
            Some(code) => format!("exited with status {code}"),
            None => format!("exited: {status}"),
        },
        _ => "closed its output".into(),
    }
}

async fn read_all(
    reader: Box<dyn AsyncRead + Send + Unpin>,
    incoming: UnboundedSender<Result<Value, ReadError>>,
) {
    let mut reader = BufReader::new(reader);
    loop {
        let message = rpc::read(&mut reader).await;
        let done = message.is_err();
        if incoming.send(message).is_err() || done {
            return;
        }
    }
}

async fn write_all(
    mut writer: Box<dyn AsyncWrite + Send + Unpin>,
    mut frames: UnboundedReceiver<Vec<u8>>,
) {
    while let Some(frame) = frames.recv().await {
        if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

async fn read_stderr(stderr: Box<dyn AsyncRead + Send + Unpin>, log: Log, name: String) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        log.write(&name, Direction::Stderr, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_a_ceiling() {
        let timing = Timing::default();
        let waits: Vec<u128> = (1..=6).map(|attempt| timing.backoff(attempt).as_millis()).collect();
        assert_eq!(waits, [500, 1000, 2000, 4000, 8000, 8000]);
        assert_eq!(timing.backoff(u32::MAX), timing.most_backoff, "no overflow");
    }

    #[test]
    fn the_progress_line_reads_like_a_sentence() {
        assert_eq!(describe("Indexing", Some("12/340"), Some(3)), "Indexing · 12/340 · 3%");
        assert_eq!(describe("Loading", None, None), "Loading");
        assert_eq!(describe("Loading", Some(""), Some(0)), "Loading · 0%");
    }

    #[test]
    fn synchronisation_follows_what_the_server_said() {
        let caps = |sync: Value| -> ServerCapabilities {
            serde_json::from_value(json!({ "textDocumentSync": sync })).unwrap()
        };
        assert_eq!(synchronisation(&caps(json!(2))), (SyncKind::Incremental, true, Some(false)));
        assert_eq!(synchronisation(&caps(json!(1))), (SyncKind::Full, true, Some(false)));
        assert_eq!(synchronisation(&caps(json!(0))), (SyncKind::None, false, None));
        assert_eq!(
            synchronisation(&caps(
                json!({ "openClose": true, "change": 2, "save": { "includeText": true } })
            )),
            (SyncKind::Incremental, true, Some(true))
        );
        assert_eq!(synchronisation(&caps(json!({ "change": 1 }))), (SyncKind::Full, false, None));
        assert_eq!(synchronisation(&ServerCapabilities::default()), (SyncKind::None, false, None));
    }

    #[test]
    fn nun_offers_every_encoding_it_can_convert_exactly() {
        let params = initialize_params(Path::new("/tmp/project"));
        assert_eq!(
            params["capabilities"]["general"]["positionEncodings"],
            json!(["utf-32", "utf-8", "utf-16"])
        );
        assert_eq!(params["rootUri"], json!("file:///tmp/project"));
        assert_eq!(params["workspaceFolders"][0]["name"], json!("project"));
    }

    #[test]
    fn completion_asks_for_snippets_and_both_ranges() {
        let params = initialize_params(Path::new("/tmp/project"));
        let item = &params["capabilities"]["textDocument"]["completion"]["completionItem"];
        assert_eq!(item["snippetSupport"], json!(true));
        assert_eq!(item["insertReplaceSupport"], json!(true));
        assert_eq!(item["resolveSupport"]["properties"], json!(["documentation", "detail"]));
    }

    #[test]
    fn every_server_request_gets_an_answer() {
        let (_, inbox) = unbounded_channel();
        let server = Server::new(
            ServerId(0),
            "x".into(),
            Spec { command: "x".into(), args: Vec::new() },
            PathBuf::from("/tmp"),
            false,
            inbox,
            Shared {
                report: Arc::new(|_| {}),
                launcher: processes(),
                timing: Arc::new(Timing::default()),
                log: Log::none(),
            },
        );
        assert_eq!(
            server.server_request("workspace/configuration", &json!({ "items": [{}, {}] })),
            Ok(json!([null, null]))
        );
        assert_eq!(
            server.server_request("window/workDoneProgress/create", &json!({})),
            Ok(Value::Null)
        );
        assert_eq!(
            server.server_request("workspace/applyEdit", &json!({})).unwrap()["applied"],
            json!(false)
        );
        assert_eq!(
            server.server_request("something/new", &Value::Null).unwrap_err().code,
            rpc::METHOD_NOT_FOUND
        );
    }
}
