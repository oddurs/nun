//! A language server small enough to trust, for testing the client against.
//!
//! It runs in-process over a duplex pipe, on the same runtime as the client,
//! so a test can use tokio's paused clock: timeouts, backoff and deadlines all
//! happen in virtual time, deterministically, and a test of a thirty-second
//! backoff takes no time at all.
//!
//! It keeps a copy of every document the way the protocol says a server must —
//! the `Mirror` from the sync tests, which shares no code with the sync layer —
//! so a test can compare it with the buffer after any sequence of edits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::position::Encoding;
use crate::rpc::{self, Message};
use crate::server::{Connection, Launcher};
use crate::sync::tests::Mirror;

/// How the fake answers `initialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Initialize {
    Answer,
    Never,
    Refuse,
}

/// How the fake behaves.
#[derive(Debug, Clone)]
pub(crate) struct Script {
    /// The position encoding it picks: `utf-8`, `utf-16`, `utf-32`, or
    /// nothing, which means UTF-16.
    pub encoding: Option<&'static str>,
    /// The `TextDocumentSyncKind` it asks for.
    pub sync: u8,
    pub initialize: Initialize,
    /// Whether it answers `shutdown`.
    pub answers_shutdown: bool,
    /// This many launches die the moment they start.
    pub crash_first: usize,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            encoding: None,
            sync: 2,
            initialize: Initialize::Answer,
            answers_shutdown: true,
            crash_first: 0,
        }
    }
}

/// What the fake has seen, for the test to look at.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    pub launches: AtomicUsize,
    /// Every request and notification, by method, in order.
    pub received: Mutex<Vec<(String, Value)>>,
    /// Its copy of each open document, by URI, with the version.
    pub documents: Mutex<HashMap<String, (Mirror, i64)>>,
    /// What the client answered the fake's own requests with.
    pub answers: Mutex<Vec<Value>>,
}

impl Seen {
    pub(crate) fn methods(&self) -> Vec<String> {
        self.received.lock().unwrap().iter().map(|(method, _)| method.clone()).collect()
    }

    pub(crate) fn document(&self, uri: &str) -> Option<(String, i64)> {
        self.documents
            .lock()
            .unwrap()
            .get(uri)
            .map(|(mirror, version)| (mirror.text.clone(), *version))
    }
}

/// Launch fakes following `script`, reporting to `seen`.
pub(crate) fn launcher(script: Script, seen: Arc<Seen>) -> Launcher {
    Arc::new(move |_, _, _| {
        let launch = seen.launches.fetch_add(1, Ordering::SeqCst);
        let (client, server) = tokio::io::duplex(1 << 16);
        let (reader, writer) = tokio::io::split(client);
        if launch >= script.crash_first {
            tokio::spawn(serve(server, script.clone(), seen.clone()));
        }
        Ok(Connection {
            reader: Box::new(reader),
            writer: Box::new(writer),
            stderr: None,
            child: None,
        })
    })
}

/// One running fake.
struct Fake {
    script: Script,
    seen: Arc<Seen>,
    encoding: Encoding,
    /// Frames for the writer.
    out: UnboundedSender<Value>,
    /// Requests it is sitting on, by id.
    hanging: Vec<Value>,
}

async fn serve(stream: tokio::io::DuplexStream, script: Script, seen: Arc<Seen>) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let (out, mut frames) = unbounded_channel::<Value>();
    let writing = tokio::spawn(async move {
        while let Some(value) = frames.recv().await {
            if writer.write_all(&rpc::frame(&value.to_string())).await.is_err() {
                return;
            }
        }
    });
    let encoding = match script.encoding {
        Some("utf-8") => Encoding::Utf8,
        Some("utf-32") => Encoding::Utf32,
        _ => Encoding::Utf16,
    };
    let mut fake = Fake { script, seen, encoding, out, hanging: Vec::new() };
    while let Ok(value) = rpc::read(&mut reader).await {
        let going = match Message::from_value(value) {
            Some(Message::Request { id, method, params }) => fake.request(id, &method, params),
            Some(Message::Notification { method, params }) => fake.notification(&method, &params),
            Some(Message::Response { result, .. }) => {
                fake.seen.answers.lock().unwrap().push(result.unwrap_or(Value::Null));
                true
            }
            None => true,
        };
        if !going {
            break;
        }
    }
    // Gone, as a process that exits is: whatever it had not sent is lost.
    writing.abort();
}

impl Fake {
    fn send(&self, value: Value) {
        let _ = self.out.send(value);
    }

    fn reply(&self, id: &Value, result: &Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn fail(&self, id: &Value, code: i64, message: &str) {
        self.send(
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        );
    }

    fn notify(&self, method: &str, params: &Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn publish(&self, uri: &str, mirror: &Mirror, version: i64) {
        let lines = mirror.text.split('\n').count();
        self.notify(
            "textDocument/publishDiagnostics",
            &json!({
                "uri": uri,
                "version": version,
                "diagnostics": [{
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                    "message": format!("{lines} lines"),
                }],
            }),
        );
    }

    /// Answer a request, or not. Whether to carry on.
    fn request(&mut self, id: Value, method: &str, params: Value) -> bool {
        self.seen.received.lock().unwrap().push((method.to_string(), params.clone()));
        match method {
            "initialize" => match self.script.initialize {
                Initialize::Answer => self.reply(
                    &id,
                    &json!({
                        "capabilities": {
                            "positionEncoding": self.script.encoding,
                            "textDocumentSync": {
                                "openClose": true,
                                "change": self.script.sync,
                                "save": { "includeText": true },
                            },
                            "hoverProvider": true,
                        },
                        "serverInfo": { "name": "fake" },
                    }),
                ),
                Initialize::Never => self.hanging.push(id),
                Initialize::Refuse => self.fail(&id, -32603, "no thank you"),
            },
            "shutdown" if self.script.answers_shutdown => self.reply(&id, &Value::Null),
            "test/echo" => self.reply(&id, &params),
            "test/crash" => return false,
            "test/slow" => {
                let ms = params["ms"].as_u64().unwrap_or(1000);
                let out = self.out.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    let _ = out.send(json!({ "jsonrpc": "2.0", "id": id, "result": params }));
                });
            }
            "test/progress" => {
                for value in [
                    json!({ "kind": "begin", "title": "Indexing", "percentage": 0 }),
                    json!({ "kind": "report", "message": "1/2", "percentage": 50 }),
                    json!({ "kind": "end" }),
                ] {
                    self.notify("$/progress", &json!({ "token": "t", "value": value }));
                }
                self.reply(&id, &Value::Null);
            }
            "test/ask" => {
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": "fake-1",
                    "method": "workspace/configuration",
                    "params": { "items": [{ "section": "fake" }] },
                }));
                self.reply(&id, &Value::Null);
            }
            // Asks for `params` to be applied as an edit, as a server carrying
            // out a command does, and answers once it has its answer.
            "test/edit" => {
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "method": "workspace/applyEdit",
                    "params": params,
                }));
                self.hanging.push(id);
            }
            // Everything else hangs, as a busy server would.
            _ => self.hanging.push(id),
        }
        true
    }

    /// Take in a notification. Whether to carry on.
    fn notification(&mut self, method: &str, params: &Value) -> bool {
        self.seen.received.lock().unwrap().push((method.to_string(), params.clone()));
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_string();
        let version = params["textDocument"]["version"].as_i64().unwrap_or(-1);
        match method {
            "exit" => return false,
            "textDocument/didOpen" => {
                let text = params["textDocument"]["text"].as_str().unwrap_or_default();
                let mirror = Mirror::new(text, self.encoding);
                self.publish(&uri, &mirror, version);
                self.seen.documents.lock().unwrap().insert(uri, (mirror, version));
            }
            "textDocument/didChange" => {
                let changes: Vec<lsp_types::TextDocumentContentChangeEvent> =
                    serde_json::from_value(params["contentChanges"].clone()).unwrap();
                let seen = self.seen.clone();
                let mut documents = seen.documents.lock().unwrap();
                let (mirror, known) =
                    documents.get_mut(&uri).expect("changed before it was opened");
                assert!(version > *known, "versions go up: {version} after {known}");
                for change in &changes {
                    mirror.apply(change);
                }
                *known = version;
                self.publish(&uri, mirror, version);
            }
            "textDocument/didClose" => {
                self.seen.documents.lock().unwrap().remove(&uri);
            }
            "$/cancelRequest" => {
                let id = &params["id"];
                if let Some(at) = self.hanging.iter().position(|hung| hung == id) {
                    self.hanging.remove(at);
                    self.fail(id, rpc::REQUEST_CANCELLED, "cancelled");
                }
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use nun_core::{Buffer, Range, Selections};
    use ropey::Rope;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
    use tokio::time::Instant;

    use super::*;
    use crate::event::{DocId, Error, Event, RequestId, ServerId, Status};
    use crate::log::Log;
    use crate::server::{Report, Server, Shared, Spec, Timing, ToServer};

    const URI: &str = "file:///tmp/project/src/main.rs";

    struct Harness {
        inbox: UnboundedSender<ToServer>,
        events: UnboundedReceiver<Event>,
        seen: Arc<Seen>,
        task: tokio::task::JoinHandle<()>,
        next: u64,
    }

    fn timing() -> Timing {
        Timing { request: Duration::from_secs(5), ..Timing::default() }
    }

    fn start(script: Script, timing: Timing) -> Harness {
        let seen = Arc::new(Seen::default());
        let (sender, events) = unbounded_channel();
        let report: Report = Arc::new(move |event| {
            let _ = sender.send(event);
        });
        let (inbox, receiver) = unbounded_channel();
        let server = Server::new(
            ServerId(0),
            "fake".into(),
            Spec { command: "fake".into(), args: Vec::new() },
            PathBuf::from("/tmp/project"),
            false,
            receiver,
            Shared {
                report,
                launcher: launcher(script, seen.clone()),
                timing: Arc::new(timing),
                log: Log::none(),
            },
        );
        let task = tokio::spawn(server.run());
        Harness { inbox, events, seen, task, next: 0 }
    }

    impl Harness {
        /// The next event matching `wanted`, skipping the rest. Waits in
        /// virtual time, so an hour is free; a test that gets here without its
        /// event has found a bug rather than a slow machine.
        async fn until(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
            let deadline = Instant::now() + Duration::from_secs(3600);
            loop {
                let event = tokio::time::timeout_at(deadline, self.events.recv())
                    .await
                    .expect("the event came")
                    .expect("the server is still reporting");
                if wanted(&event) {
                    return event;
                }
            }
        }

        async fn status(&mut self, wanted: impl Fn(&Status) -> bool) -> Status {
            match self
                .until(|event| matches!(event, Event::Status { status, .. } if wanted(status)))
                .await
            {
                Event::Status { status, .. } => status,
                _ => unreachable!(),
            }
        }

        async fn ready(&mut self) {
            self.status(|status| *status == Status::Ready).await;
        }

        fn open(&self, doc: DocId, text: &str, version: i32) {
            self.send(ToServer::Open {
                doc,
                uri: URI.parse().unwrap(),
                language: "rust",
                version,
                text: Rope::from_str(text),
            });
        }

        fn send(&self, message: ToServer) {
            self.inbox.send(message).unwrap();
        }

        fn ask(&mut self, method: &str, params: Value, timeout: Option<Duration>) -> RequestId {
            let id = RequestId(self.next);
            self.next += 1;
            self.send(ToServer::Request {
                id,
                doc: 1,
                version: 0,
                method: method.into(),
                params,
                timeout,
            });
            id
        }

        async fn answer(&mut self, id: RequestId) -> Result<Value, Error> {
            match self
                .until(|event| matches!(event, Event::Response(response) if response.id == id))
                .await
            {
                Event::Response(response) => response.result,
                _ => unreachable!(),
            }
        }

        /// Everything sent before this has been read by the fake.
        async fn settle(&mut self) {
            let id = self.ask("test/echo", json!("settle"), None);
            assert_eq!(self.answer(id).await, Ok(json!("settle")));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_is_answered_with_its_id_and_the_version_it_asked_about() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "fn main() {}", 0);
        fake.ready().await;
        fake.send(ToServer::Request {
            id: RequestId(7),
            doc: 1,
            version: 4,
            method: "test/echo".into(),
            params: json!({ "hello": "😀" }),
            timeout: None,
        });
        let Event::Response(response) =
            fake.until(|event| matches!(event, Event::Response(_))).await
        else {
            unreachable!()
        };
        assert_eq!((response.id, response.doc, response.version), (RequestId(7), 1, 4));
        assert_eq!(response.result, Ok(json!({ "hello": "😀" })));
    }

    #[tokio::test(start_paused = true)]
    async fn opening_sends_the_text_and_diagnostics_come_back_with_a_path() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "a\nb\nc", 3);
        fake.ready().await;
        let Event::Diagnostics { path, published } =
            fake.until(|event| matches!(event, Event::Diagnostics { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(path, PathBuf::from("/tmp/project/src/main.rs"));
        assert_eq!(published.version, Some(3));
        assert_eq!(published.diagnostics[0].message, "3 lines");
        assert_eq!(fake.seen.document(URI), Some(("a\nb\nc".into(), 3)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_before_the_server_is_ready_is_refused_at_once() {
        let mut fake =
            start(Script { initialize: Initialize::Never, ..Script::default() }, timing());
        fake.open(1, "", 0);
        let started = Instant::now();
        let id = fake.ask("test/echo", json!(1), None);
        assert_eq!(fake.answer(id).await, Err(Error::NotReady));
        assert!(started.elapsed() < Duration::from_millis(1), "not after a timeout");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_request_times_out_is_cancelled_and_its_late_answer_dropped() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let started = Instant::now();
        let id = fake.ask("test/hang", Value::Null, Some(Duration::from_secs(2)));
        assert_eq!(fake.answer(id).await, Err(Error::TimedOut));
        assert!(started.elapsed() >= Duration::from_secs(2));
        fake.settle().await;
        assert!(fake.seen.methods().contains(&"$/cancelRequest".to_string()));
        // The fake answered the cancel with RequestCancelled; nothing more
        // came of it, and the server is still up.
        while let Ok(event) = fake.events.try_recv() {
            assert!(!matches!(&event, Event::Response(response) if response.id == id), "{event:?}");
        }
        assert_eq!(fake.seen.launches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_tells_the_server_and_no_answer_comes_back() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let id = fake.ask("test/slow", json!({ "ms": 500 }), None);
        fake.send(ToServer::Cancel { id });
        fake.settle().await;
        let cancel = fake
            .seen
            .received
            .lock()
            .unwrap()
            .iter()
            .find(|(method, _)| method == "$/cancelRequest")
            .map(|(_, params)| params.clone());
        assert!(cancel.is_some_and(|params| params["id"].is_i64()), "cancelled by its wire id");
        // Well past when the slow answer arrives.
        tokio::time::sleep(Duration::from_secs(1)).await;
        fake.settle().await;
        while let Ok(event) = fake.events.try_recv() {
            assert!(!matches!(&event, Event::Response(response) if response.id == id), "{event:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_stops_answering_is_restarted() {
        let mut fake = start(Script::default(), Timing { hung_after: 3, ..timing() });
        fake.open(1, "text", 0);
        fake.ready().await;
        for _ in 0..3 {
            let id = fake.ask("test/hang", Value::Null, Some(Duration::from_secs(1)));
            assert_eq!(fake.answer(id).await, Err(Error::TimedOut));
        }
        let crashed = fake.status(|status| matches!(status, Status::Crashed { .. })).await;
        assert!(
            matches!(&crashed, Status::Crashed { why, .. } if why.contains("stopped answering")),
            "{crashed:?}"
        );
        fake.ready().await;
        assert_eq!(fake.seen.launches.load(Ordering::SeqCst), 2);
        assert_eq!(fake.seen.document(URI).map(|(text, _)| text), Some("text".into()), "reopened");
    }

    #[tokio::test(start_paused = true)]
    async fn crashes_back_off_doubling_then_give_up_loudly() {
        let mut fake = start(Script { crash_first: usize::MAX, ..Script::default() }, timing());
        let started = Instant::now();
        let mut waits = Vec::new();
        let gave_up = loop {
            match fake
                .status(|status| matches!(status, Status::Crashed { .. } | Status::GaveUp { .. }))
                .await
            {
                Status::Crashed { retry_in, attempt, .. } => {
                    assert_eq!(attempt as usize, waits.len() + 2);
                    waits.push(retry_in.as_millis());
                }
                other => break other,
            }
        };
        assert_eq!(waits, [500, 1000, 2000, 4000]);
        assert!(
            matches!(&gave_up, Status::GaveUp { attempts: 5, why } if why.contains("closed")),
            "{gave_up:?}"
        );
        assert!(started.elapsed() >= Duration::from_millis(7500), "the waits were waited");
        assert_eq!(fake.seen.launches.load(Ordering::SeqCst), 5);

        // Given up on means left alone: nothing more happens by itself.
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(fake.seen.launches.load(Ordering::SeqCst), 5);

        // Until someone asks.
        fake.send(ToServer::Restart);
        fake.status(|status| *status == Status::Starting).await;
        assert_eq!(fake.seen.launches.load(Ordering::SeqCst), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_is_recovered_from_with_the_text_as_it_is_now() {
        let mut fake = start(Script { crash_first: 2, ..Script::default() }, timing());
        fake.open(1, "before", 0);
        fake.status(|status| matches!(status, Status::Crashed { .. })).await;
        // Edited while the server is down: the restarted server is opened on
        // the new text, not the old.
        fake.send(ToServer::Change {
            doc: 1,
            version: 1,
            edits: vec![nun_core::Edit::replace(0, 6, "after")],
            text: Rope::from_str("after"),
        });
        fake.ready().await;
        fake.settle().await;
        assert_eq!(fake.seen.document(URI), Some(("after".into(), 1)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_initializes_is_retried_and_given_up_on() {
        let mut fake = start(
            Script { initialize: Initialize::Never, ..Script::default() },
            Timing { initialize: Duration::from_secs(5), ..timing() },
        );
        let crashed = fake.status(|status| matches!(status, Status::Crashed { .. })).await;
        assert!(
            matches!(&crashed, Status::Crashed { why, .. } if why.contains("initialize")),
            "{crashed:?}"
        );
        let gave_up = fake.status(|status| matches!(status, Status::GaveUp { .. })).await;
        assert!(matches!(gave_up, Status::GaveUp { attempts: 5, .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_refusing_to_initialize_says_why() {
        let mut fake =
            start(Script { initialize: Initialize::Refuse, ..Script::default() }, timing());
        let crashed = fake.status(|status| matches!(status, Status::Crashed { .. })).await;
        assert!(
            matches!(&crashed, Status::Crashed { why, .. } if why.contains("no thank you")),
            "{crashed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_answers_what_was_in_flight() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let slow = fake.ask("test/slow", json!({ "ms": 60_000 }), Some(Duration::from_secs(120)));
        let _ = fake.ask("test/crash", Value::Null, None);
        assert_eq!(fake.answer(slow).await, Err(Error::Stopped));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_asks_then_says_exit() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        fake.send(ToServer::Shutdown { deadline: Instant::now() + Duration::from_secs(2) });
        fake.status(|status| *status == Status::Stopped).await;
        let methods = fake.seen.methods();
        let tail: Vec<&str> = methods.iter().rev().take(2).rev().map(String::as_str).collect();
        assert_eq!(tail, ["shutdown", "exit"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_ignoring_shutdown_is_cut_off_at_the_deadline() {
        let mut fake = start(Script { answers_shutdown: false, ..Script::default() }, timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let started = Instant::now();
        fake.send(ToServer::Shutdown { deadline: started + Duration::from_secs(2) });
        fake.status(|status| *status == Status::Stopped).await;
        let took = started.elapsed();
        assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(4), "{took:?}");
        let harness = fake;
        tokio::time::timeout(Duration::from_secs(5), harness.task)
            .await
            .expect("the task ended")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_servers_own_requests_are_answered() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let id = fake.ask("test/ask", Value::Null, None);
        fake.answer(id).await.unwrap();
        fake.settle().await;
        assert_eq!(*fake.seen.answers.lock().unwrap(), [json!([null])]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_edit_the_server_asks_for_is_answered_when_the_editor_says() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let edit = json!({ "label": "Fix it", "edit": { "changes": {} } });
        fake.ask("test/edit", edit, None);
        let Event::ApplyEdit(request) =
            fake.until(|event| matches!(event, Event::ApplyEdit(_))).await
        else {
            unreachable!()
        };
        assert_eq!(request.label.as_deref(), Some("Fix it"));
        assert_eq!(request.encoding, Encoding::Utf16);
        // Nothing is said until the editor answers; meanwhile the server is
        // served as ever.
        fake.settle().await;
        assert!(fake.seen.answers.lock().unwrap().is_empty());

        let (run, id) = (request.run, request.id.clone());
        fake.send(ToServer::AnswerEdit { run, id: id.clone(), result: Err("no".into()) });
        // Answered once only, however often the editor says.
        fake.send(ToServer::AnswerEdit { run, id: id.clone(), result: Ok(()) });
        // And never to a run that did not ask.
        fake.send(ToServer::AnswerEdit { run: run + 1, id, result: Ok(()) });
        fake.settle().await;
        assert_eq!(
            *fake.seen.answers.lock().unwrap(),
            [json!({ "applied": false, "failureReason": "no" })]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_edit_that_is_not_one_is_refused_at_once() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        fake.ask("test/edit", json!({ "edit": 7 }), None);
        fake.settle().await;
        assert_eq!(*fake.seen.answers.lock().unwrap(), [Value::Null], "an error, not a result");
    }

    #[tokio::test(start_paused = true)]
    async fn progress_is_passed_on_and_cleared_when_done() {
        let mut fake = start(Script::default(), timing());
        fake.open(1, "", 0);
        fake.ready().await;
        let id = fake.ask("test/progress", Value::Null, None);
        let Event::Progress { text, .. } =
            fake.until(|event| matches!(event, Event::Progress { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(text.as_deref(), Some("Indexing · 0%"));
        let Event::Progress { text, .. } =
            fake.until(|event| matches!(event, Event::Progress { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(text, None, "the report came too soon to pass on; the end always does");
        fake.answer(id).await.unwrap();
    }

    /// One thing done to a buffer.
    type Step = Box<dyn Fn(&mut Buffer)>;

    /// Edits made through a real buffer and sent over the wire, checked
    /// against the fake's own copy.
    async fn follow_over_the_wire(encoding: Option<&'static str>, sync: u8, start_bytes: &[u8]) {
        let mut fake = start(Script { encoding, sync, ..Script::default() }, timing());
        let (mut buffer, _) = Buffer::from_bytes(start_bytes);
        buffer.keep_edits(true);
        fake.send(ToServer::Open {
            doc: 1,
            uri: URI.parse().unwrap(),
            language: "rust",
            version: 0,
            text: buffer.rope().clone(),
        });
        fake.ready().await;
        let steps: Vec<Step> = vec![
            Box::new(|b| b.insert("😀")),
            Box::new(|b| {
                let carets =
                    (0..b.len_lines()).map(|line| Range::caret(b.line_start(line))).collect();
                b.set_selections(Selections::new(carets, 0));
            }),
            Box::new(|b| b.insert("e\u{301}中 ")),
            Box::new(nun_core::Buffer::delete_backward),
            Box::new(|b| {
                b.undo();
            }),
            Box::new(|b| {
                b.undo();
            }),
            Box::new(|b| {
                b.redo();
            }),
            Box::new(|b| b.insert("\n")),
            Box::new(|b| b.set_selections(Selections::single(Range::new(1, b.len_chars() - 1)))),
            Box::new(|b| b.insert("\u{1F469}\u{200D}\u{1F4BB}")),
            Box::new(nun_core::Buffer::delete_backward),
            Box::new(nun_core::Buffer::delete_backward),
        ];
        let mut version = 0;
        for step in &steps {
            step(&mut buffer);
            let edits = buffer.take_edits();
            if edits.is_empty() {
                continue;
            }
            version += 1;
            fake.send(ToServer::Change { doc: 1, version, edits, text: buffer.rope().clone() });
            fake.settle().await;
            assert_eq!(
                fake.seen.document(URI),
                Some((buffer.rope().to_string(), i64::from(version))),
                "{encoding:?}, sync {sync}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_server_copy_matches_the_buffer_in_every_encoding() {
        for encoding in [Some("utf-8"), Some("utf-16"), Some("utf-32"), None] {
            follow_over_the_wire(encoding, 2, "fn main() {\n    ö😀\n}\n".as_bytes()).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_server_copy_matches_the_buffer_for_a_crlf_file_and_a_full_sync_server() {
        follow_over_the_wire(Some("utf-16"), 2, b"one\r\ntwo\r\n").await;
        follow_over_the_wire(Some("utf-16"), 1, b"one\r\ntwo\r\n").await;
    }
}
