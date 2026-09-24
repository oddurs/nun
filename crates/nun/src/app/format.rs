//! Formatting a document through its language server: on demand, and on save.
//!
//! Nothing here waits, so a save that formats first is two steps. Saving asks
//! the server for its edits and returns at once; the save itself happens when
//! the answer comes back, or when the time allowed for one runs out. The
//! editor goes on drawing and taking keys in between, and the status line says
//! what it is waiting for.
//!
//! Whatever happens to the formatting, a save that was asked for happens. A
//! server that fails, refuses, times out or is not there leaves the file
//! unformatted and saved, and the status line says why. The same goes for text
//! that changed while the server was working: its edits describe text that is
//! no longer there, so they are dropped and what is there now is saved as it
//! is. Asking again instead could chase someone typing for as long as they
//! kept going, and a save that never lands is worse than one unformatted.
//! Quitting saves anything still waiting, unformatted, before it asks about
//! unsaved changes.
//!
//! The edits go in through `Buffer::apply_batch`: one undo step, applied as
//! the server meant whatever order it sent them in, cut down to the
//! whitespace that actually changed, with every caret and selection carried
//! across by the text around it rather than left at its old offset.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use nun_lsp::types::request::Formatting as Format;
use nun_lsp::types::{DocumentFormattingParams, FormattingOptions, OneOf, TextEdit};
use nun_lsp::{Error, RequestId, Response};

use super::panes::DocId;
use super::{App, Outcome};

/// How long a server has to format a file before a save goes ahead without
/// it. Long enough for rustfmt on a large file; short enough that a server
/// that has hung costs a moment, not the save.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// How long past the server's own timeout to wait for its answer before
/// giving up on the handle's word, in case it never comes: the save must not
/// hang on that either.
const GRACE: Duration = Duration::from_secs(1);

/// What is being formatted, and for which languages it happens on save.
#[derive(Debug)]
pub struct Formatting {
    /// The languages, by their `[lsp.<name>]` section, whose files are
    /// formatted before they are saved.
    on_save: BTreeSet<String>,
    /// Requests not answered yet: one per document at most.
    pending: Vec<Pending>,
    /// How long a server is given; shorter in the tests.
    timeout: Duration,
}

impl Default for Formatting {
    fn default() -> Self {
        Self { on_save: BTreeSet::new(), pending: Vec::new(), timeout: TIMEOUT }
    }
}

impl Formatting {
    /// Whether `id` is a formatting request of ours.
    pub fn asked(&self, id: RequestId) -> bool {
        self.pending.iter().any(|pending| pending.id == id)
    }

    /// When the first request still waiting stops being waited for.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.iter().map(|pending| pending.deadline).min()
    }
}

/// One request in flight.
#[derive(Debug, Clone, Copy)]
struct Pending {
    id: RequestId,
    doc: DocId,
    /// Save once the edits are in, or once it is clear none are coming.
    save: bool,
    /// Then close the document's tab, as the prompt about closing it asked.
    close: bool,
    /// When to stop waiting for the answer.
    deadline: Instant,
}

impl App {
    /// Format files in these languages, by their `[lsp.<name>]` section,
    /// before saving them.
    pub fn set_format_on_save(&mut self, languages: impl IntoIterator<Item = String>) {
        self.formatting.on_save = languages.into_iter().collect();
    }

    /// Format the document being edited, without saving it.
    pub(super) fn format_document(&mut self) -> Outcome {
        let id = self.doc().id;
        if self.formatting.pending.iter().any(|pending| pending.doc == id) {
            self.message = Some("Already formatting this file…".into());
            return Outcome::Redraw;
        }
        self.message = Some(match self.ask_format(id) {
            Ok(request) => {
                self.wait_for(request, id, false, false);
                "Formatting…".into()
            }
            Err(why) => format!("Cannot format this file: {why}."),
        });
        Outcome::Redraw
    }

    /// Save a document, formatting it first when its language is formatted
    /// on save, and then close its tab if `close`.
    ///
    /// Whether the save was put off until the server answers. When it was
    /// not, nothing has been done and the caller saves now.
    pub(super) fn format_then_save(&mut self, id: DocId, close: bool) -> bool {
        // Already being formatted, on demand or by a save a moment ago: that
        // answer will do, and the save goes with it.
        if let Some(pending) = self.formatting.pending.iter_mut().find(|pending| pending.doc == id)
        {
            pending.save = true;
            pending.close |= close;
            self.message = Some("Formatting before saving…".into());
            return true;
        }
        if !self.formats_on_save(id) {
            return false;
        }
        // No server able to answer is no reason to hold the save up.
        let Ok(request) = self.ask_format(id) else { return false };
        self.wait_for(request, id, true, close);
        self.message = Some("Formatting before saving…".into());
        true
    }

    /// Whether a document's language is formatted on save.
    pub(super) fn formats_on_save(&self, id: DocId) -> bool {
        self.doc_by(id)
            .filter(|document| !document.buffer.is_lossy())
            .and_then(|document| document.buffer.path())
            .and_then(nun_lsp::language_of)
            .is_some_and(|language| self.formatting.on_save.contains(language.config))
    }

    /// Ask a document's server to format it.
    fn ask_format(&mut self, id: DocId) -> Result<RequestId, String> {
        let document = self.docs.iter().find(|document| document.id == id);
        let buffer = &document.ok_or("it is no longer open")?.buffer;
        let options = FormattingOptions {
            tab_size: u32::try_from(buffer.tab_width()).unwrap_or(4),
            insert_spaces: !indents_with_tabs(buffer),
            ..FormattingOptions::default()
        };
        let lsp = self.lsp.as_mut().ok_or("no language server is running")?;
        let capabilities = lsp.capabilities(id).ok_or("its language server is not ready")?;
        if matches!(capabilities.document_formatting_provider, None | Some(OneOf::Left(false))) {
            return Err("its language server does not format files".into());
        }
        let text_document = lsp.identifier(id).ok_or("its language server is not ready")?;
        let params = DocumentFormattingParams {
            text_document,
            options,
            work_done_progress_params: nun_lsp::types::WorkDoneProgressParams::default(),
        };
        lsp.request_within::<Format>(id, params, self.formatting.timeout)
            .map_err(|error| error.to_string())
    }

    fn wait_for(&mut self, id: RequestId, doc: DocId, save: bool, close: bool) {
        let deadline = Instant::now() + self.formatting.timeout + GRACE;
        self.formatting.pending.push(Pending { id, doc, save, close, deadline });
    }

    /// A server answered a formatting request.
    pub(super) fn format_answer(&mut self, response: &Response) -> Outcome {
        let Some(at) = self.formatting.pending.iter().position(|pending| pending.id == response.id)
        else {
            return Outcome::Continue;
        };
        let pending = self.formatting.pending.remove(at);
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(pending.doc));
        let formatted = match response.parse::<Format>() {
            Err(error) => Err(error.to_string()),
            Ok(_) if current != Some(response.version) => {
                Err("the file changed while it was being formatted".into())
            }
            Ok(edits) => self.apply_format(pending.doc, &edits.unwrap_or_default()),
        };
        self.format_done(pending, formatted)
    }

    /// Put a server's formatting into a document. Whether it changed
    /// anything.
    fn apply_format(&mut self, id: DocId, edits: &[TextEdit]) -> Result<bool, String> {
        let lsp = self.lsp.as_ref().ok_or("the language server stopped")?;
        let document = self
            .docs
            .iter_mut()
            .find(|document| document.id == id)
            .ok_or("the file is no longer open")?;
        let edits =
            lsp.edits(id, document.buffer.rope(), edits).ok_or("the language server stopped")?;
        let changed = document
            .buffer
            .apply_batch(edits)
            .map_err(|error| format!("the language server's edits made no sense ({error})"))?;
        if changed && self.doc().id == id {
            self.follow_caret();
        }
        Ok(changed)
    }

    /// Formatting is over, one way or the other: save if that was asked,
    /// and say how it went.
    fn format_done(&mut self, pending: Pending, formatted: Result<bool, String>) -> Outcome {
        if !pending.save {
            self.message = Some(match formatted {
                Ok(true) => "Formatted.".into(),
                Ok(false) => "Already formatted.".into(),
                Err(why) => format!("Not formatted: {why}."),
            });
            return Outcome::Redraw;
        }
        self.message = Some(match (self.write(pending.doc), formatted) {
            (Ok(saved), Ok(_)) => saved,
            (Ok(saved), Err(why)) => format!("{saved}, unformatted: {why}."),
            (Err(failed), _) => failed,
        });
        let saved = self.doc_by(pending.doc).is_some_and(|document| !document.buffer.is_modified());
        if pending.close
            && saved
            && let Some((pane, index)) = self.panes.find(pending.doc)
        {
            return self.drop_tab(pane, index);
        }
        Outcome::Redraw
    }

    /// Stop waiting for every answer overdue at `now`.
    pub(super) fn format_tick(&mut self, now: Instant) -> Outcome {
        let mut outcome = Outcome::Continue;
        while let Some(at) = self.formatting.pending.iter().position(|p| p.deadline <= now) {
            let pending = self.formatting.pending.remove(at);
            if let Some(lsp) = self.lsp.as_mut() {
                lsp.cancel(pending.id);
            }
            outcome = outcome.and(self.format_done(pending, Err(Error::TimedOut.to_string())));
        }
        outcome
    }

    /// Save, unformatted, every document whose save is waiting on a server,
    /// and stop waiting for it: nun is about to quit, and a save that was
    /// asked for must not be lost with it.
    pub fn save_before_quitting(&mut self) {
        let waiting = std::mem::take(&mut self.formatting.pending);
        for pending in waiting {
            if let Some(lsp) = self.lsp.as_mut() {
                lsp.cancel(pending.id);
            }
            if pending.save {
                let _ = self.write(pending.doc);
            }
        }
    }
}

/// Whether a buffer's indentation is tabs, judged by the first indented line
/// among the first thousand.
fn indents_with_tabs(buffer: &nun_core::Buffer) -> bool {
    buffer
        .text()
        .lines()
        .take(1000)
        .find_map(|line| match line.chars().next() {
            Some('\t') => Some(true),
            Some(' ') => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{Receiver, channel};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nun_core::{Buffer, Range, Selections};
    use nun_lsp::{Lsp, ServerSpec};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use ratatui::layout::Rect;

    use super::*;
    use crate::commands::Command;

    /// A language server small enough to read, in `sh`: it answers
    /// `initialize` saying it formats, and `textDocument/formatting` with one
    /// edit putting a space before the first `{` of `fn main(){}` — unless
    /// started with `silent`, when it never answers that at all.
    const SERVER: &str = r#"
mode="$1"
while :; do
  len=
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
  done
  [ -z "$len" ] && exit 0
  body=$(dd bs=1 count="$len" 2>/dev/null)
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$body" in
    *'"method":"initialize"'*)
      result='{"capabilities":{"textDocumentSync":1,"documentFormattingProvider":true}}' ;;
    *'"method":"textDocument/formatting"'*)
      [ "$mode" = silent ] && continue
      result='[{"range":{"start":{"line":0,"character":9},"end":{"line":0,"character":9}},"newText":" "}]' ;;
    *'"method":"shutdown"'*) result=null ;;
    *'"method":"exit"'*) exit 0 ;;
    *) continue ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"
done
"#;

    /// An editor over `main.rs` holding `fn main(){}`, with [`SERVER`] as
    /// its Rust server, formatted on save.
    fn editor(mode: &str) -> (App, Receiver<nun_lsp::Event>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "fn main(){}\n").unwrap();
        let (buffer, _) = Buffer::load(&path).unwrap();
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 100, 8));
        app.set_format_on_save(["rust".to_string()]);
        app.formatting.timeout = Duration::from_millis(300);
        let spec = ServerSpec {
            command: "sh".into(),
            args: vec!["-c".into(), SERVER.into(), "server".into(), mode.into()],
            optional: false,
        };
        let (sender, events) = channel();
        let lsp = Lsp::start(
            BTreeMap::from([("rust".to_string(), spec)]),
            None,
            Box::new(move |event| {
                let _ = sender.send(event);
            }),
        )
        .unwrap();
        app.attach_lsp(lsp);
        // Ready once it can be asked something.
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.lsp.as_ref().and_then(|lsp| lsp.capabilities(0)).is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = events.recv_timeout(left).expect("the server started");
            app.handle(Event::Lsp(event));
        }
        (app, events, dir)
    }

    /// Hand the editor what the server says, and tick it, until `done`.
    fn until(app: &mut App, events: &Receiver<nun_lsp::Event>, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(app) {
            assert!(Instant::now() < deadline, "gave up waiting; it says {:?}", app.message());
            if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
                app.handle(Event::Lsp(event));
            }
            app.tick(Instant::now());
        }
    }

    fn on_disk(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("main.rs")).unwrap()
    }

    fn saved(app: &App) -> bool {
        !app.buffer().is_modified()
    }

    /// Type an `x` between the braces, so there is something to save.
    fn touch(app: &mut App) {
        app.doc_mut().buffer.set_selections(Selections::single(Range::caret(10)));
        app.handle(Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
        assert_eq!(app.buffer().text().to_string(), "fn main(){x}\n");
    }

    #[test]
    fn saving_formats_first_and_the_caret_stays_on_its_token() {
        let (mut app, events, dir) = editor("format");
        touch(&mut app);

        assert_eq!(app.run(Command::Save), Outcome::Redraw);
        assert!(!saved(&app), "not until the server has answered");
        assert_eq!(app.message(), Some("Formatting before saving…"));

        until(&mut app, &events, saved);
        assert_eq!(on_disk(&dir), "fn main() {x}\n");
        assert_eq!(
            app.message().map(|m| m.starts_with("Saved ")),
            Some(true),
            "{:?}",
            app.message()
        );
        assert_eq!(app.buffer().selections().primary(), Range::caret(12), "still after the x");

        // One undo takes the formatting back, and only that.
        app.run(Command::Undo);
        assert_eq!(app.buffer().text().to_string(), "fn main(){x}\n");
    }

    #[test]
    fn a_server_that_never_answers_does_not_stop_the_save() {
        let (mut app, events, dir) = editor("silent");
        app.handle(Event::Paste("// note\n".into()));
        app.run(Command::Save);
        assert!(app.deadline().is_some(), "something will wake the editor to save");

        until(&mut app, &events, saved);
        assert_eq!(on_disk(&dir), "// note\nfn main(){}\n", "saved, unformatted");
        let message = app.message().unwrap_or_default().to_string();
        assert!(message.contains("unformatted: the language server did not answer"), "{message}");
    }

    #[test]
    fn text_changed_while_formatting_is_saved_as_it_is() {
        let (mut app, events, dir) = editor("format");
        app.handle(Event::Paste("// a\n".into()));
        app.run(Command::Save);
        // Typed before the answer is handled: the answer is about older text.
        app.handle(Event::Paste("// b\n".into()));

        until(&mut app, &events, saved);
        assert_eq!(on_disk(&dir), "// a\n// b\nfn main(){}\n");
        let message = app.message().unwrap_or_default().to_string();
        assert!(message.contains("unformatted: the file changed"), "{message}");
    }

    #[test]
    fn quitting_saves_what_is_waiting_on_the_server() {
        let (mut app, _events, dir) = editor("silent");
        app.handle(Event::Paste("// last words\n".into()));
        app.run(Command::Save);
        assert_eq!(app.run(Command::Quit), Outcome::Quit, "nothing left unsaved to ask about");
        assert_eq!(on_disk(&dir), "// last words\nfn main(){}\n");
    }

    #[test]
    fn format_document_formats_without_saving() {
        let (mut app, events, dir) = editor("format");
        app.run(Command::FormatDocument);
        until(&mut app, &events, |app| app.message() == Some("Formatted."));
        assert_eq!(app.buffer().text().to_string(), "fn main() {}\n");
        assert!(!saved(&app));
        assert_eq!(on_disk(&dir), "fn main(){}\n");
    }

    #[test]
    fn a_language_not_formatted_on_save_saves_at_once() {
        let (mut app, _events, dir) = editor("format");
        app.set_format_on_save(Vec::<String>::new());
        app.handle(Event::Paste("// now\n".into()));
        app.run(Command::Save);
        assert!(saved(&app));
        assert_eq!(on_disk(&dir), "// now\nfn main(){}\n");
    }

    #[test]
    fn closing_an_unsaved_tab_formats_saves_and_then_closes_it() {
        let (mut app, events, dir) = editor("format");
        touch(&mut app);
        app.run(Command::CloseTab);
        // The prompt's Save.
        app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        until(&mut app, &events, |app| app.doc_by(0).is_none());
        assert_eq!(on_disk(&dir), "fn main() {x}\n");
    }

    #[test]
    fn a_file_indented_with_tabs_asks_for_tabs() {
        assert!(indents_with_tabs(&Buffer::from_text("fn f() {\n\tx\n}\n")));
        assert!(!indents_with_tabs(&Buffer::from_text("fn f() {\n    x\n}\n")));
        assert!(
            !indents_with_tabs(&Buffer::from_text("")),
            "spaces when there is nothing to go by"
        );
    }
}
