//! Renaming a symbol across the project, through its language server.
//!
//! **Asking.** Where the server says it can, it is asked first what would be
//! renamed (`textDocument/prepareRename`). It answers with the range of the
//! name and perhaps a placeholder, or says to use the editor's own idea of the
//! word under the caret, or says nothing here can be renamed. A server that
//! cannot be asked is treated as though it had said the second. The new name
//! is typed into a prompt drawn at the symbol itself, filled in with the old
//! one; Enter or the prompt's button asks for the rename.
//!
//! **Everything after.** The server's answer is a `WorkspaceEdit`, and a
//! rename writes files nobody has open, which makes it the second most
//! destructive thing the editor does after a project-wide replace. So it goes
//! the way a replace goes: checked whole, previewed file by file in the panel
//! the replace uses, applied from there, and taken back across every file by
//! "Undo rename". All of that is `workspace_edit`, which code actions share.

use std::time::Duration;

use nun_lsp::RequestId;
use nun_lsp::Response;
use nun_lsp::types::request::{PrepareRenameRequest, Rename};
use nun_lsp::types::{OneOf, PrepareRenameResponse, RenameParams, WorkDoneProgressParams};

use super::panes::DocId;
use super::prompt::{Prompt, Purpose};
use super::workspace_edit::{After, Subject};
use super::{App, Outcome};

/// How long a server has to work out a rename. Longer than the default: a
/// server finds every reference in the project first, and on a large one that
/// takes a while.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Where a rename has got to, until the server's edit is in hand.
#[derive(Debug, Default)]
enum Stage {
    /// Nothing is happening.
    #[default]
    Idle,
    /// The server has been asked what would be renamed.
    Preparing {
        id: RequestId,
        doc: DocId,
        /// The caret, which is what is being renamed.
        at: usize,
    },
    /// The prompt is up.
    Naming { doc: DocId, version: i32, at: usize, old: String },
    /// The server has been asked for the rename.
    Asking { id: RequestId, doc: DocId, old: String, new: String },
}

/// A rename, while the server and the person are asked about it.
#[derive(Debug, Default)]
pub(super) struct Renaming {
    stage: Stage,
}

impl Renaming {
    /// Whether `id` is a request of the rename's.
    pub(super) fn asked(&self, id: RequestId) -> bool {
        match self.stage {
            Stage::Preparing { id: asked, .. } | Stage::Asking { id: asked, .. } => asked == id,
            _ => false,
        }
    }
}

impl App {
    /// Rename the symbol at the caret: ask the server what it is, then ask
    /// for its new name.
    pub(super) fn start_rename(&mut self) -> Outcome {
        if self.edits.busy() {
            self.message = Some("Still working on the last rename…".into());
            return Outcome::Redraw;
        }
        if self.sidebar.is_none() {
            self.message = Some("Open a folder to rename across it.".into());
            return Outcome::Redraw;
        }
        // A question still waiting is forgotten: this is a new one.
        if let Stage::Preparing { id, .. } | Stage::Asking { id, .. } =
            std::mem::take(&mut self.rename.stage)
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
        let id = self.doc().id;
        let at = self.doc().buffer.selections().primary().head;
        let Some(lsp) = self.lsp.as_mut() else {
            self.message = Some("Cannot rename: no language server is running.".into());
            return Outcome::Redraw;
        };
        let Some(capabilities) = lsp.capabilities(id) else {
            self.message = Some("Cannot rename: this file's language server is not ready.".into());
            return Outcome::Redraw;
        };
        let prepare = match &capabilities.rename_provider {
            None | Some(OneOf::Left(false)) => {
                self.message = Some("This file's language server does not rename.".into());
                return Outcome::Redraw;
            }
            Some(OneOf::Left(true)) => false,
            Some(OneOf::Right(options)) => options.prepare_provider == Some(true),
        };
        if !prepare {
            return self.default_name(id, at);
        }
        let rope = self.doc().buffer.rope();
        let Some(params) = self.lsp.as_ref().and_then(|lsp| lsp.position_params(id, rope, at))
        else {
            self.message = Some("Cannot rename: this file's language server is not ready.".into());
            return Outcome::Redraw;
        };
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        match lsp.request::<PrepareRenameRequest>(id, params) {
            Ok(request) => {
                self.rename.stage = Stage::Preparing { id: request, doc: id, at };
                self.message = Some("Asking the language server what to rename…".into());
            }
            Err(error) => self.message = Some(format!("Cannot rename: {error}.")),
        }
        Outcome::Redraw
    }

    /// A server answered one of the rename's questions.
    pub(super) fn rename_answer(&mut self, response: &Response) -> Outcome {
        match std::mem::take(&mut self.rename.stage) {
            Stage::Preparing { id, doc, at } if id == response.id => {
                self.prepared(doc, at, response)
            }
            Stage::Asking { id, doc, old, new } if id == response.id => {
                self.renamed(doc, old, new, response)
            }
            other => {
                self.rename.stage = other;
                Outcome::Continue
            }
        }
    }

    /// What the server said about renaming at the caret.
    fn prepared(&mut self, doc: DocId, at: usize, response: &Response) -> Outcome {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if current != Some(response.version) {
            self.message = Some("The file changed while the server was asked. Try again.".into());
            return Outcome::Redraw;
        }
        let answer = match response.parse::<PrepareRenameRequest>() {
            Ok(answer) => answer,
            Err(error) => {
                self.message = Some(format!("Cannot rename this: {error}."));
                return Outcome::Redraw;
            }
        };
        let (range, placeholder) = match answer {
            None | Some(PrepareRenameResponse::DefaultBehavior { default_behavior: false }) => {
                self.message = Some("Nothing here can be renamed.".into());
                return Outcome::Redraw;
            }
            Some(PrepareRenameResponse::DefaultBehavior { default_behavior: true }) => {
                return self.default_name(doc, at);
            }
            Some(PrepareRenameResponse::Range(range)) => (range, None),
            Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder }) => {
                (range, Some(placeholder))
            }
        };
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        let Some(encoding) = self.lsp.as_ref().and_then(|lsp| lsp.encoding(doc)) else {
            return Outcome::Continue;
        };
        let range = encoding.char_range(document.buffer.rope(), range);
        let old = document.buffer.rope().slice(range.clone()).to_string();
        let placeholder = placeholder.unwrap_or_else(|| old.clone());
        self.ask_name(doc, at, range.start, placeholder)
    }

    /// The server left it to the editor: the word under the caret.
    fn default_name(&mut self, doc: DocId, at: usize) -> Outcome {
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        let (from, to) = document.buffer.word_range(at.min(document.buffer.len_chars()));
        let word = document.buffer.rope().slice(from..to).to_string();
        if word.trim().is_empty() {
            self.message = Some("Put the caret on a name to rename it.".into());
            return Outcome::Redraw;
        }
        self.ask_name(doc, at, from, word)
    }

    /// Put the prompt up at the symbol.
    fn ask_name(&mut self, doc: DocId, at: usize, start: usize, old: String) -> Outcome {
        let Some(version) = self.lsp.as_ref().and_then(|lsp| lsp.version(doc)) else {
            return Outcome::Continue;
        };
        // Only the file being edited has a place on screen to put it; had the
        // person switched tabs meanwhile, the status line is where it goes.
        let anchor = (self.doc().id == doc).then_some(start);
        self.prompt =
            Some(Prompt::name(Purpose::RenameSymbol, "Rename to".into(), &old).at(anchor));
        self.message = None;
        self.rename.stage = Stage::Naming { doc, version, at, old };
        Outcome::Redraw
    }

    /// The prompt was answered: ask for the rename.
    pub(super) fn rename_named(&mut self, confirmed: bool, name: &str) -> Outcome {
        let Stage::Naming { doc, version, at, old } = std::mem::take(&mut self.rename.stage) else {
            return Outcome::Redraw;
        };
        if !confirmed {
            return Outcome::Redraw;
        }
        if name.is_empty() || name == old {
            self.message = Some("The name is the same, so there is nothing to rename.".into());
            return Outcome::Redraw;
        }
        let Some(document) = self.doc_by(doc) else {
            self.message = Some("That file is no longer open.".into());
            return Outcome::Redraw;
        };
        let rope = document.buffer.rope();
        if self.lsp.as_ref().and_then(|lsp| lsp.version(doc)) != Some(version) {
            self.message = Some("The file changed while the name was typed. Try again.".into());
            return Outcome::Redraw;
        }
        let Some(position) = self.lsp.as_ref().and_then(|lsp| lsp.position_params(doc, rope, at))
        else {
            self.message = Some("Cannot rename: the language server is not ready.".into());
            return Outcome::Redraw;
        };
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        let params = RenameParams {
            text_document_position: position,
            new_name: name.to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        match lsp.request_within::<Rename>(doc, params, TIMEOUT) {
            Ok(id) => {
                self.message = Some(format!("Finding every {old} to rename…"));
                self.rename.stage = Stage::Asking { id, doc, old, new: name.to_string() };
            }
            Err(error) => self.message = Some(format!("Cannot rename: {error}.")),
        }
        Outcome::Redraw
    }

    /// The server's rename came back: check it, and preview it.
    fn renamed(&mut self, doc: DocId, old: String, new: String, response: &Response) -> Outcome {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if current != Some(response.version) {
            self.message =
                Some("The file changed while the rename was worked out. Try again.".into());
            return Outcome::Redraw;
        }
        let edit = match response.parse::<Rename>() {
            Ok(Some(edit)) => edit,
            Ok(None) => {
                self.message = Some(format!("The language server found no {old} to rename."));
                return Outcome::Redraw;
            }
            Err(error) => {
                self.message = Some(format!("Cannot rename {old}: {error}."));
                return Outcome::Redraw;
            }
        };
        // Every position the server sent is in its own encoding: the one it
        // agreed for the file it was asked about, whichever file it edits.
        let Some(encoding) = self.lsp.as_ref().and_then(|lsp| lsp.encoding(doc)) else {
            self.message = Some(format!("Did not rename {old}: the language server stopped."));
            return Outcome::Redraw;
        };
        self.edit_workspace(Subject::Rename { old, new }, encoding, &edit, false, After::default())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_core::{Buffer, Range as Caret, Selections};
    use nun_lsp::{Lsp, ServerSpec};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use nun_workspace::Done;
    use ratatui::buffer::Buffer as Cells;
    use ratatui::layout::Rect;

    use super::*;
    use crate::app::workspace_edit::Spot;
    use crate::app::{SidebarView, Target};
    use crate::commands::Command;

    /// A language server in `sh`, answering `initialize` with the
    /// capabilities in `$1`, `textDocument/prepareRename` with `$2` and
    /// `textDocument/rename` with `$3`, as they are given.
    const SERVER: &str = r#"
caps="$1"; prepare="$2"; rename="$3"
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
    *'"method":"initialize"'*) result="{\"capabilities\":$caps}" ;;
    *'"method":"textDocument/prepareRename"'*) result="$prepare" ;;
    *'"method":"textDocument/rename"'*) result="$rename" ;;
    *'"method":"shutdown"'*) result=null ;;
    *'"method":"exit"'*) exit 0 ;;
    *) continue ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"
done
"#;

    /// Renames with a prepare step, and edits by URI.
    const PREPARES: &str = r#"{"textDocumentSync":1,"renameProvider":{"prepareProvider":true}}"#;

    /// Renames, and leaves finding the name to the editor.
    const PLAIN: &str = r#"{"textDocumentSync":1,"renameProvider":true}"#;

    /// An editor on a project, with the server above answering with the
    /// given JSON, the file tree's worker attached, and both pumped the way
    /// the event loop pumps them.
    struct Tester {
        app: App,
        lsp: Receiver<nun_lsp::Event>,
        done: Receiver<Done>,
        dir: tempfile::TempDir,
    }

    impl Tester {
        /// `rename` may name files as `{a.rs}`, which becomes that file's
        /// URI — the resolved one, as a server would say it — whether or not
        /// it is there.
        fn new(files: &[(&str, &str)], caps: &str, prepare: &str, rename: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, text) in files {
                fs::write(dir.path().join(name), text).unwrap();
            }
            let resolved = fs::canonicalize(dir.path()).unwrap();
            let rename = uris(rename, &resolved);

            let (buffer, _) = Buffer::load(dir.path().join(files[0].0)).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                crate::commands::defaults(crate::commands::KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 120, 30));
            let (sender, done) = channel();
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                false,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            let spec = ServerSpec {
                command: "sh".into(),
                args: vec![
                    "-c".into(),
                    SERVER.into(),
                    "server".into(),
                    caps.into(),
                    prepare.into(),
                    rename,
                ],
                optional: false,
            };
            let (sender, lsp) = channel();
            let handle = Lsp::start(
                BTreeMap::from([("rust".to_string(), spec)]),
                None,
                Box::new(move |event| {
                    let _ = sender.send(event);
                }),
            )
            .unwrap();
            app.attach_lsp(handle);
            let mut tester = Self { app, lsp, done, dir };
            tester.until(|app| app.lsp.as_ref().and_then(|lsp| lsp.capabilities(0)).is_some());
            tester
        }

        /// Hand the editor whatever the server and the worker say, and tick
        /// it, until `done`.
        fn until(&mut self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done(&self.app) {
                assert!(Instant::now() < deadline, "gave up; it says {:?}", self.app.message());
                if let Ok(event) = self.lsp.recv_timeout(Duration::from_millis(5)) {
                    self.app.handle(Event::Lsp(event));
                }
                while let Ok(message) = self.done.try_recv() {
                    self.app.handle(Event::Workspace(message));
                }
                self.app.tick(Instant::now());
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.path(name)).unwrap()
        }

        fn caret(&mut self, at: usize) {
            self.app.doc_mut().buffer.set_selections(Selections::single(Caret::caret(at)));
        }

        fn key(&mut self, code: KeyCode) {
            self.app.handle(Event::Key(KeyEvent::from(code)));
        }

        fn click(&mut self, column: u16, row: u16) {
            for kind in
                [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)]
            {
                self.app.handle(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }));
            }
        }

        /// Click wherever `target` is laid out.
        fn click_on(&mut self, target: Target) {
            let area = self.app.viewport;
            let at = (area.top()..area.bottom())
                .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                .find(|&(x, y)| self.app.hits.at(x, y).map(|hit| hit.target) == Some(target))
                .unwrap_or_else(|| panic!("{target:?} is not on screen"));
            self.click(at.0, at.1);
        }

        /// Ask to rename what is at char `at`, and wait for the prompt.
        fn ask(&mut self, at: usize) {
            self.caret(at);
            self.app.run(Command::RenameSymbol);
            self.until(|app| app.prompt.is_some() || !asked_anything(app));
        }

        /// Type `name` over what the prompt holds, and send it.
        fn name(&mut self, name: &str) {
            let typed = self.app.prompt.as_ref().and_then(|p| p.field.clone()).unwrap();
            for _ in typed.chars() {
                self.key(KeyCode::Backspace);
            }
            for ch in name.chars() {
                self.key(KeyCode::Char(ch));
            }
            self.key(KeyCode::Enter);
            self.until(|app| !asked_anything(app));
        }

        /// The preview as it reads.
        fn rows(&self) -> Vec<String> {
            self.app.edit_preview_rows()
        }

        fn settled(&mut self) {
            self.until(|app| !app.edits.busy());
        }
    }

    /// `text` with every `{name}` in it — a name of letters, digits, dots
    /// and slashes — made the URI of that name under `root`.
    fn uris(text: &str, root: &std::path::Path) -> String {
        let named = |inside: &str| {
            inside.contains('.')
                && inside.chars().all(|ch| ch.is_ascii_alphanumeric() || "._/".contains(ch))
        };
        let mut out = String::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) if named(&after[..close]) => {
                    let uri = nun_lsp::uri::from_path(&root.join(&after[..close])).unwrap();
                    out.push_str(uri.as_str());
                    rest = &after[close + 1..];
                }
                _ => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Whether a question to the server or the worker is still out.
    fn asked_anything(app: &App) -> bool {
        matches!(app.rename.stage, Stage::Preparing { .. } | Stage::Asking { .. })
            || app.edits_reading()
    }

    const MAIN: &str = "fn cat() {}\nfn main() { cat(); }\n";
    const OTHER: &str = "use crate::cat;\r\nfn f() { cat() }\r\n";

    fn edit(line: u32, from: u32, to: u32, text: &str) -> String {
        format!(
            r#"{{"range":{{"start":{{"line":{line},"character":{from}}},"end":{{"line":{line},"character":{to}}}}},"newText":"{text}"}}"#
        )
    }

    /// `cat` to `dog` in both files, as `changes`.
    fn by_uri(name: &str) -> String {
        format!(
            r#"{{"changes":{{"{{main.rs}}":[{},{}],"{{other.rs}}":[{},{}]}}}}"#,
            edit(0, 3, 6, name),
            edit(1, 12, 15, name),
            edit(0, 11, 14, name),
            edit(1, 9, 12, name),
        )
    }

    const PLACEHOLDER: &str = r#"{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":6}},"placeholder":"cat"}"#;

    #[test]
    fn a_rename_previews_every_file_applies_as_one_and_undoes_as_one() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        let prompt = t.app.prompt.as_ref().expect("the prompt is up");
        assert_eq!(prompt.field.as_deref(), Some("cat"), "filled with the placeholder");
        let status = t.app.areas().1;
        let beside = t.app.prompt_area(status);
        assert_ne!(beside, status, "drawn at the symbol, not in the status line");
        assert_eq!(beside.y, t.app.areas().0.y + 1, "on the row under it");
        let mut cells = Cells::empty(t.app.viewport);
        t.app.render(t.app.viewport, &mut cells);
        let drawn: String =
            (beside.x..beside.right()).map(|x| cells[(x, beside.y)].symbol()).collect();
        assert!(drawn.starts_with(" Rename to cat"), "{drawn:?}");
        assert!(drawn.trim_end().ends_with(" Rename   Cancel"), "{drawn:?}");
        // Its buttons are where they are drawn.
        t.app.relayout();
        let buttons = t.app.prompt.as_ref().unwrap().button_areas(beside);
        assert_eq!(
            t.app.hits.at(buttons[1].x + 1, buttons[1].y).map(|hit| hit.target),
            Some(Target::PromptButton(1))
        );

        t.name("dog");
        assert_eq!(
            t.rows(),
            [
                "[main.rs]",
                "1- fn cat() {}",
                "1+ fn dog() {}",
                "2- fn main() { cat(); }",
                "2+ fn main() { dog(); }",
                "[other.rs]",
                "1- use crate::cat;",
                "1+ use crate::dog;",
                "2- fn f() { cat() }",
                "2+ fn f() { dog() }",
            ],
        );
        assert_eq!(t.app.sidebar_view, SidebarView::EditPreview);
        assert_eq!(t.read("other.rs"), OTHER, "nothing is written before it is applied");

        // The panel's own button.
        t.click_on(Target::EditPreview(Spot::Action(0)));
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n", "CRLF kept");
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\nfn main() { dog(); }\n");
        assert_eq!(t.read("main.rs"), MAIN, "an open file is edited, not saved");
        let message = t.app.message().unwrap().to_string();
        assert!(message.starts_with("Renamed cat to dog in 2 files."), "{message}");
        assert_eq!(t.app.sidebar_view, SidebarView::Files, "the preview is put away");

        // The status line offers the way back, and it takes back everything.
        assert!(t.app.edit_undo_offered());
        t.click_on(Target::StatusUndo);
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
        assert!(!t.app.buffer().is_modified(), "back where it was");
        assert!(t.app.message().unwrap().starts_with("Took back the rename of cat to dog"));
    }

    #[test]
    fn a_server_without_a_prepare_step_renames_the_word_at_the_caret() {
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PLAIN, "null", &by_uri("dog"));
        t.ask(5);
        assert_eq!(t.app.prompt.as_ref().and_then(|p| p.field.as_deref()), Some("cat"));
        t.name("dog");
        assert_eq!(t.rows().len(), 10);
    }

    #[test]
    fn a_server_that_says_use_the_default_gets_the_word_at_the_caret() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            r#"{"defaultBehavior":true}"#,
            &by_uri("dog"),
        );
        t.ask(4);
        assert_eq!(t.app.prompt.as_ref().and_then(|p| p.field.as_deref()), Some("cat"));
    }

    #[test]
    fn a_server_that_says_nothing_is_there_puts_no_prompt_up() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            "null",
            &by_uri("dog"),
        );
        t.ask(9);
        assert!(t.app.prompt.is_none());
        assert_eq!(t.app.message(), Some("Nothing here can be renamed."));
    }

    #[test]
    fn a_versioned_edit_to_text_that_has_moved_on_is_refused_whole() {
        let rename = format!(
            r#"{{"documentChanges":[{{"textDocument":{{"uri":"{{other.rs}}","version":null}},"edits":[{}]}},{{"textDocument":{{"uri":"{{main.rs}}","version":41}},"edits":[{}]}}]}}"#,
            edit(0, 11, 14, "dog"),
            edit(0, 3, 6, "dog"),
        );
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        assert!(!t.app.edit_previewing(), "nothing to preview");
        let message = t.app.message().unwrap();
        assert!(
            message.contains("main.rs has changed since the server worked it out"),
            "{message}"
        );
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    /// `documentChanges` of the steps given, each already JSON.
    fn steps(steps: &[String]) -> String {
        format!(r#"{{"documentChanges":[{}]}}"#, steps.join(","))
    }

    /// Edits to a file, as a step.
    fn edits_to(name: &str, edits: &[String]) -> String {
        format!(
            r#"{{"textDocument":{{"uri":"{{{name}}}","version":null}},"edits":[{}]}}"#,
            edits.join(",")
        )
    }

    fn move_file(from: &str, to: &str) -> String {
        format!(r#"{{"kind":"rename","oldUri":"{{{from}}}","newUri":"{{{to}}}"}}"#)
    }

    const LIB: &str = "mod foo;\nfn main() { foo::f(); }\n";
    const FOO: &str = "pub fn f() {}\n";

    /// A module renamed as rust-analyzer renames one: the files that name it
    /// edited, its file moved — and here an edit to it under its new name,
    /// which some servers send.
    fn module_rename() -> String {
        steps(&[
            edits_to("lib.rs", &[edit(0, 4, 7, "bar"), edit(1, 12, 15, "bar")]),
            move_file("foo.rs", "bar.rs"),
            edits_to("bar.rs", &[edit(0, 7, 8, "g")]),
        ])
    }

    #[test]
    fn a_rename_that_moves_a_file_previews_the_move_carries_it_out_and_undoes_it() {
        let lib_placeholder = r#"{"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":7}},"placeholder":"foo"}"#;
        let mut t = Tester::new(
            &[("lib.rs", LIB), ("foo.rs", FOO)],
            PREPARES,
            lib_placeholder,
            &module_rename(),
        );
        t.ask(4);
        t.name("bar");
        assert_eq!(
            t.rows(),
            [
                "{Move foo.rs to bar.rs}",
                "[foo.rs]",
                "1- pub fn f() {}",
                "1+ pub fn g() {}",
                "[lib.rs]",
                "1- mod foo;",
                "1+ mod bar;",
                "2- fn main() { foo::f(); }",
                "2+ fn main() { bar::f(); }",
            ],
        );
        assert!(t.path("foo.rs").exists(), "nothing is moved before it is applied");

        t.key(KeyCode::Enter);
        t.settled();
        assert!(!t.path("foo.rs").exists());
        assert_eq!(t.read("bar.rs"), "pub fn g() {}\n", "edited, then moved");
        assert_eq!(t.app.buffer().text().to_string(), "mod bar;\nfn main() { bar::f(); }\n");
        let message = t.app.message().unwrap().to_string();
        assert!(
            message.starts_with("Renamed foo to bar in 2 files, and moved foo.rs to bar.rs."),
            "{message}"
        );

        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("foo.rs"), FOO);
        assert!(!t.path("bar.rs").exists());
        assert_eq!(t.app.buffer().text().to_string(), LIB);
        let message = t.app.message().unwrap().to_string();
        assert!(message.contains("and moved bar.rs to foo.rs"), "{message}");
    }

    #[test]
    fn an_open_file_that_is_moved_follows_its_file_and_comes_back_with_the_undo() {
        let mut t = Tester::new(
            &[("foo.rs", FOO), ("lib.rs", LIB)],
            PREPARES,
            PLACEHOLDER,
            &module_rename(),
        );
        t.ask(7);
        t.name("bar");
        t.key(KeyCode::Enter);
        t.settled();
        let path = t.app.buffer().path().unwrap().to_path_buf();
        assert_eq!(path, t.path("bar.rs"), "the buffer follows");
        assert_eq!(t.app.buffer().text().to_string(), "pub fn g() {}\n");
        assert!(t.app.buffer().is_modified(), "edited, and not saved");
        assert_eq!(t.read("bar.rs"), FOO, "moved as it was on disk");
        assert_eq!(t.read("lib.rs"), "mod bar;\nfn main() { bar::f(); }\n");
        t.until(|app| {
            app.lsp
                .as_ref()
                .and_then(|lsp| lsp.identifier(app.doc().id))
                .is_some_and(|id| id.uri.as_str().ends_with("/bar.rs"))
        });

        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.app.buffer().path().unwrap(), t.path("foo.rs"));
        assert_eq!(t.app.buffer().text().to_string(), FOO);
        assert_eq!(t.read("lib.rs"), LIB);
    }

    #[test]
    fn a_create_and_a_delete_are_previewed_carried_out_and_taken_back() {
        let rename = steps(&[
            edits_to("main.rs", &[edit(0, 3, 6, "dog")]),
            r#"{"kind":"create","uri":"{new.rs}"}"#.to_string(),
            edits_to("new.rs", &[edit(0, 0, 0, "fn dog() {}\\n")]),
            r#"{"kind":"delete","uri":"{other.rs}"}"#.to_string(),
        ]);
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        let rows = t.rows();
        assert_eq!(rows[..2], ["{Create new.rs}", "{Delete other.rs}"], "{rows:?}");
        assert!(rows.contains(&"[new.rs]".to_string()), "its text shown: {rows:?}");
        assert!(rows.contains(&"1+ fn dog() {}↵".to_string()), "{rows:?}");

        // Its text cannot be left out apart from the file.
        let row = rows.iter().position(|row| row == "[new.rs]").unwrap();
        t.click_on(Target::EditPreview(Spot::Mark(row)));
        assert!(t.rows().contains(&"[new.rs]".to_string()));

        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("new.rs"), "fn dog() {}\n");
        assert!(!t.path("other.rs").exists());
        let trashed = fs::read_dir(t.path(".trash")).unwrap().count();
        assert_eq!(trashed, 1, "deleted into the trash");

        t.app.run(Command::UndoRename);
        t.settled();
        assert!(!t.path("new.rs").exists());
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn nothing_is_overwritten_whatever_the_server_says() {
        for options in ["", r#","options":{"overwrite":true}"#] {
            let rename = steps(&[
                edits_to("main.rs", &[edit(0, 3, 6, "dog")]),
                format!(r#"{{"kind":"create","uri":"{{other.rs}}"{options}}}"#),
            ]);
            let mut t = Tester::new(
                &[("main.rs", MAIN), ("other.rs", OTHER)],
                PREPARES,
                PLACEHOLDER,
                &rename,
            );
            t.ask(4);
            t.name("dog");
            let message = t.app.message().unwrap().to_string();
            assert!(message.starts_with("Did not rename cat: the server would"), "{message}");
            assert!(!t.app.edit_previewing());
            assert_eq!(t.read("other.rs"), OTHER);
            assert_eq!(t.app.buffer().text().to_string(), MAIN);
        }
    }

    #[test]
    fn a_create_told_to_leave_what_is_there_alone_leaves_it_alone() {
        let rename = steps(&[
            edits_to("main.rs", &[edit(0, 3, 6, "dog")]),
            r#"{"kind":"create","uri":"{other.rs}","options":{"ignoreIfExists":true}}"#.to_string(),
        ]);
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        assert!(!t.rows().iter().any(|row| row.starts_with('{')), "{:?}", t.rows());
        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
    }

    #[test]
    fn deleting_a_file_that_is_open_is_refused() {
        let rename = steps(&[r#"{"kind":"delete","uri":"{main.rs}"}"#.to_string()]);
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        assert!(t.app.message().unwrap().contains("which is open"), "{:?}", t.app.message());
        assert!(t.path("main.rs").exists());
    }

    #[test]
    fn a_file_operation_outside_the_folder_is_refused() {
        let rename =
            steps(&[r#"{"kind":"create","uri":"file:///tmp/nun-0078-outside.rs"}"#.to_string()]);
        let mut t =
            Tester::new(&[("main.rs", MAIN), ("other.rs", OTHER)], PREPARES, PLACEHOLDER, &rename);
        t.ask(4);
        t.name("dog");
        assert!(t.app.message().unwrap().contains("outside the folder"), "{:?}", t.app.message());
    }

    #[test]
    fn a_failure_partway_says_exactly_which_operations_happened_and_undo_takes_them_back() {
        let rename = steps(&[
            edits_to("other.rs", &[edit(0, 11, 14, "dog")]),
            move_file("a.rs", "b.rs"),
            r#"{"kind":"create","uri":"{c.rs}"}"#.to_string(),
            r#"{"kind":"delete","uri":"{d.rs}"}"#.to_string(),
        ]);
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER), ("a.rs", "a\n"), ("d.rs", "d\n")],
            PREPARES,
            PLACEHOLDER,
            &rename,
        );
        t.ask(4);
        t.name("dog");
        assert!(t.rows().contains(&"{Create c.rs}".to_string()));
        // Something takes the name between the preview and the apply.
        fs::write(t.path("c.rs"), "mine\n").unwrap();
        t.key(KeyCode::Enter);
        t.settled();
        let message = t.app.message().unwrap().to_string();
        assert!(
            message.starts_with(
                "Renamed only partly: wrote other.rs; moved a.rs to b.rs, then stopped: could not \
                 create c.rs: "
            ),
            "{message}"
        );
        assert!(message.contains("already exists; not done: delete d.rs."), "{message}");
        assert_eq!(t.read("c.rs"), "mine\n", "not overwritten");
        assert!(t.path("d.rs").exists(), "not tried");
        assert!(t.app.edit_undo_offered());

        // A message this long leaves no room for the status line's button;
        // the palette's Undo rename is the same thing.
        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("a.rs"), "a\n");
        assert!(!t.path("b.rs").exists());
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.read("c.rs"), "mine\n", "never ours to take back");
    }

    #[test]
    fn a_file_changed_on_disk_after_the_preview_stops_everything() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        fs::write(t.path("other.rs"), "use crate::cat; // edited\r\n").unwrap();
        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::cat; // edited\r\n");
        assert_eq!(t.app.buffer().text().to_string(), MAIN, "the open file went back too");
        let message = t.app.message().unwrap();
        assert!(message.contains("other.rs has changed on disk since"), "{message}");
        assert!(message.contains("Nothing was changed"), "{message}");
    }

    #[test]
    fn a_file_left_out_is_left_alone() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        // The tick on other.rs's row.
        let row = t.rows().iter().position(|row| row == "[other.rs]").unwrap();
        t.click_on(Target::EditPreview(Spot::Mark(row)));
        assert!(t.rows().contains(&"[out: other.rs]".to_string()), "{:?}", t.rows());
        assert!(!t.rows().iter().any(|row| row.contains("crate::dog")), "no after rows for it");
        t.click_on(Target::EditPreview(Spot::Apply));
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\nfn main() { dog(); }\n");
    }

    #[test]
    fn cancelling_renames_nothing() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.click_on(Target::EditPreview(Spot::Action(1)));
        assert!(!t.app.edit_previewing());
        assert_eq!(t.app.sidebar_view, SidebarView::Files);
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn undo_is_refused_when_a_written_file_has_changed_since() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        fs::write(t.path("other.rs"), "use crate::dog; // mine\r\n").unwrap();
        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("other.rs"), "use crate::dog; // mine\r\n", "not overwritten");
        let renamed = "fn dog() {}\nfn main() { dog(); }\n";
        assert_eq!(t.app.buffer().text().to_string(), renamed, "nor is the open file undone");
        let message = t.app.message().unwrap();
        assert!(message.contains("other.rs has changed on disk since"), "{message}");

        // Put it back as the rename left it, and the undo goes through.
        fs::write(t.path("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n").unwrap();
        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("other.rs"), OTHER);
        assert_eq!(t.app.buffer().text().to_string(), MAIN);
    }

    #[test]
    fn undo_is_refused_when_an_open_file_has_been_edited_since() {
        let mut t = Tester::new(
            &[("main.rs", MAIN), ("other.rs", OTHER)],
            PREPARES,
            PLACEHOLDER,
            &by_uri("dog"),
        );
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        t.app.handle(Event::Paste("// more\n".into()));
        t.app.run(Command::UndoRename);
        t.settled();
        assert!(t.app.message().unwrap().contains("main.rs has been edited since"));
        assert_eq!(t.read("other.rs"), "use crate::dog;\r\nfn f() { dog() }\r\n", "checked first");
    }

    #[cfg(unix)]
    #[test]
    fn a_write_that_fails_halfway_is_reported_exactly_and_undo_takes_back_what_was_written() {
        use std::os::unix::fs::PermissionsExt;
        let rename = format!(
            r#"{{"changes":{{"{{a.rs}}":[{}],"{{b.rs}}":[{}],"{{c.rs}}":[{}]}}}}"#,
            edit(0, 3, 6, "dog"),
            edit(0, 0, 3, "dog"),
            edit(0, 0, 3, "dog"),
        );
        let mut t = Tester::new(
            &[("a.rs", "fn cat() {}\n"), ("b.rs", "cat\n"), ("c.rs", "cat\n")],
            PREPARES,
            PLACEHOLDER,
            &rename,
        );
        fs::set_permissions(t.path("c.rs"), fs::Permissions::from_mode(0o444)).unwrap();
        if fs::OpenOptions::new().write(true).open(t.path("c.rs")).is_ok() {
            return; // Root writes anyway; there is no failure to see.
        }
        t.ask(4);
        t.name("dog");
        t.key(KeyCode::Enter);
        t.settled();
        assert_eq!(t.read("b.rs"), "dog\n", "written");
        assert_eq!(t.read("c.rs"), "cat\n", "refused");
        let message = t.app.message().unwrap().to_string();
        assert!(
            message.starts_with("Renamed only partly: wrote b.rs, then stopped: "),
            "{message}"
        );
        assert!(message.contains("c.rs"), "{message}");
        assert_eq!(t.app.buffer().text().to_string(), "fn dog() {}\n", "the open file kept it");

        t.app.run(Command::UndoRename);
        t.settled();
        assert_eq!(t.read("b.rs"), "cat\n");
        assert_eq!(t.app.buffer().text().to_string(), "fn cat() {}\n");
    }

    #[test]
    fn an_open_file_holding_a_bare_carriage_return_is_refused() {
        let mut t = Tester::new(
            &[("main.rs", "a\rcat\ncat\n"), ("other.rs", OTHER)],
            PREPARES,
            r#"{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},"placeholder":"cat"}"#,
            &format!(
                r#"{{"changes":{{"{{main.rs}}":[{},{}]}}}}"#,
                edit(1, 0, 3, "dog"),
                edit(2, 0, 3, "dog")
            ),
        );
        t.ask(2);
        t.name("dog");
        let message = t.app.message().unwrap();
        assert!(message.contains("bare carriage return"), "{message}");
        assert_eq!(t.app.buffer().text().to_string(), "a\rcat\ncat\n");
    }

    #[test]
    fn the_prompt_sits_under_the_symbol_where_it_is_drawn_not_where_its_chars_are() {
        let mut app = App::new(
            Buffer::from_text("\t中文 cat\n"),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 10));
        // The tab, two wide characters and a space: past four chars, but
        // drawn past the tab stop and four more columns.
        let tab = app.doc().buffer.tab_width();
        let at = 4;
        app.prompt =
            Some(Prompt::name(Purpose::RenameSymbol, "Rename to".into(), "cat").at(Some(at)));
        let status = app.areas().1;
        let area = app.prompt_area(status);
        let text = app.areas().0;
        assert_eq!(usize::from(area.x - text.x - app.gutter_width()), tab + 5);
        assert_eq!(area.y, text.y + 1);
    }
}
