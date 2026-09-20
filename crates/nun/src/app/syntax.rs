//! Keeping the parser fed, and the colours current.
//!
//! Editing does not wait for a parse. A change marks the document and sets a
//! deadline a few milliseconds out; when it falls due the text and the change
//! that made it go to the worker, and the highlights come back as a message.
//! In between, the runs already on screen stay on screen — stale by a few
//! characters at the very worst, which is invisible, where blanking them would
//! not be.

use std::time::{Duration, Instant};

use nun_syntax::{Reply, Request, Span, TextEdit, Worker};

use super::panes::DocId;
use super::{App, Document, Outcome};

/// How long after a keystroke the parser is asked for a new answer.
///
/// Long enough that a burst of typing is one parse rather than twenty, short
/// enough that the colours are never visibly behind the text.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(12);

/// How far either side of the visible text is asked for, in lines, so that
/// scrolling a screen does not always need a new answer first.
const MARGIN_LINES: usize = 200;

/// What the editor knows about one document's syntax.
#[derive(Debug, Default)]
pub(super) struct Highlighting {
    /// The runs on screen, in order and not overlapping.
    pub(super) spans: Vec<Span>,
    /// Which version of the text they came from.
    version: u64,
    /// The version the text is at now.
    latest: u64,
    /// The change since the parser was last told, when it was exactly one.
    pending: Option<TextEdit>,
    /// Whether anything has happened that the parser has not been told about.
    dirty: bool,
    /// Whether the worker is following this document at all.
    open: bool,
    /// What language it was recognised as, remembered from when it was opened.
    ///
    /// Kept here rather than worked out from the path on demand: the status
    /// line asks on every layout and every frame, and the first such question
    /// is what compiles every grammar's queries — which would happen on the
    /// thread that draws, during startup, before the worker has been asked
    /// for anything at all.
    language: Option<&'static str>,
}

/// The outline of the document the palette is looking at.
#[derive(Debug, Default)]
pub(super) struct Outline {
    /// What it declares, in the order it declares it.
    pub(super) found: Vec<nun_syntax::Symbol>,
    /// Their names, kept beside them so filtering does not clone every one of
    /// them on every keystroke.
    pub(super) labels: Vec<String>,
    /// Whether the request is owed but has not been sent, because the parser
    /// has not been given the text it would be read from yet.
    owed: bool,
    /// Which document it is of, so one file's outline is never shown for
    /// another's.
    of: Option<DocId>,
    /// Which version of that document, so an answer to an older one is not
    /// taken for the current one.
    version: u64,
    /// Whether the parser has been asked and has not answered.
    pub(super) waiting: bool,
}

impl App {
    /// Ask for the outline of the document being edited, unless it is already
    /// in hand.
    ///
    /// The parse is on the worker, so the palette opens on whatever is known
    /// and fills in when the answer arrives — the alternative is a palette
    /// that does not appear until a large file has been read.
    pub(super) fn want_outline(&mut self, now: Instant) {
        let id = self.doc().id;
        let version = self.doc().syntax.latest;
        if self.symbols.of == Some(id) && self.symbols.version == version {
            return;
        }
        // A different document, or one that has been edited since: what is
        // held describes neither.
        self.symbols.found.clear();
        self.symbols.labels.clear();
        self.symbols.of = Some(id);
        self.symbols.version = version;
        self.symbols.waiting = false;
        self.symbols.owed = false;
        if !self.doc().syntax.open || self.syntax.is_none() {
            return;
        }
        self.symbols.waiting = true;
        // The parser answers from the text it has been given, and stamps the
        // answer with the version that was asked for. Asking before the text
        // behind that version has reached it would get an outline of the old
        // text under the new version's name — accepted as current, and never
        // asked for again. So the request waits for the update it belongs to.
        if self.doc().syntax.dirty {
            self.symbols.owed = true;
            self.syntax_deadline = Some(self.syntax_deadline.unwrap_or(now + DEBOUNCE));
        } else {
            self.send_outline(id, version);
        }
    }

    /// Ask the parser for one document's outline.
    fn send_outline(&mut self, id: DocId, version: u64) {
        if let Some(worker) = self.syntax.as_ref() {
            worker.send(Request::Symbols { id, version });
        }
        self.symbols.owed = false;
    }

    /// Start following a document, if nun knows its language.
    pub(super) fn syntax_open(&mut self, id: DocId) {
        let Some(worker) = self.syntax.as_ref() else { return };
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return;
        };
        let Some(language) = document.buffer.path().and_then(nun_syntax::of_path) else {
            document.syntax = Highlighting::default();
            return;
        };
        worker.send(Request::Open { id, language, text: document.buffer.rope().clone() });
        document.syntax = Highlighting {
            open: true,
            dirty: true,
            language: Some(language.name),
            ..Highlighting::default()
        };
    }

    /// Stop following one.
    pub(super) fn syntax_close(&self, id: DocId) {
        if let Some(worker) = self.syntax.as_ref() {
            worker.send(Request::Close(id));
        }
    }

    /// The text of the document being edited changed.
    ///
    /// Called after anything that touches a buffer; the change itself comes
    /// from the buffer, which knows whether it was one edit or something that
    /// cannot be followed.
    pub(super) fn syntax_changed(&mut self, now: Instant) {
        let id = self.doc().id;
        let change = self.doc_mut().buffer.take_change();
        if change == nun_core::Changed::Nothing {
            return;
        }
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return;
        };
        if !document.syntax.open {
            return;
        }

        document.syntax.latest += 1;
        document.syntax.dirty = true;
        // Two changes since the parser last heard cannot be described as one
        // edit, so the next parse starts afresh rather than being told a half
        // truth.
        let one = match change {
            nun_core::Changed::One(change) => Some(TextEdit {
                start: u32::try_from(change.start).unwrap_or(u32::MAX),
                old_end: u32::try_from(change.old_end).unwrap_or(u32::MAX),
                new_end: u32::try_from(change.new_end).unwrap_or(u32::MAX),
            }),
            _ => None,
        };
        document.syntax.pending = match (document.syntax.pending.take(), one) {
            (None, one) => one,
            _ => None,
        };
        self.syntax_deadline = Some(self.syntax_deadline.unwrap_or(now + DEBOUNCE));
    }

    /// The view moved, so a different part of the file matters.
    pub(super) fn syntax_scrolled(&mut self, now: Instant) {
        if self.doc().syntax.open {
            self.syntax_deadline = Some(self.syntax_deadline.unwrap_or(now + DEBOUNCE));
        }
    }

    /// When the parser is next due to be told something.
    pub(super) const fn syntax_deadline(&self) -> Option<Instant> {
        self.syntax_deadline
    }

    /// The debounce fell due: tell the parser what has happened.
    pub(super) fn syntax_tick(&mut self, now: Instant) -> Outcome {
        let Some(deadline) = self.syntax_deadline else { return Outcome::Continue };
        if now < deadline {
            return Outcome::Continue;
        }
        self.syntax_deadline = None;

        let Some(worker) = self.syntax.as_ref() else { return Outcome::Continue };
        for pane in self.panes.all() {
            let Some(id) = pane.current() else { continue };
            let window = self.syntax_window(id);
            let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
                continue;
            };
            if !document.syntax.open {
                continue;
            }
            if document.syntax.dirty {
                document.syntax.dirty = false;
                worker.send(Request::Update {
                    id,
                    version: document.syntax.latest,
                    text: document.buffer.rope().clone(),
                    edit: document.syntax.pending.take(),
                    window,
                });
            } else {
                worker.send(Request::Window { id, version: document.syntax.latest, window });
            }
        }
        // After the updates, so it reads the text they carried rather than
        // whatever the parser held before them.
        if self.symbols.owed
            && let Some(id) = self.symbols.of
        {
            let version = self.symbols.version;
            self.send_outline(id, version);
        }
        Outcome::Continue
    }

    /// The char range worth highlighting: what is on screen, and a few screens
    /// either side.
    fn syntax_window(&self, id: DocId) -> std::ops::Range<u32> {
        let Some(document) = self.docs.iter().find(|document| document.id == id) else {
            return 0..0;
        };
        let lines = document.buffer.len_lines();
        let height = self.text_height().max(1);
        let first = document.scroll.saturating_sub(MARGIN_LINES);
        let last = (document.scroll + height + MARGIN_LINES).min(lines.saturating_sub(1));
        let start = u32::try_from(document.buffer.line_start(first)).unwrap_or(0);
        let end = u32::try_from(document.buffer.line_end(last)).unwrap_or(u32::MAX);
        start..end
    }

    /// The parser has something to say.
    pub(super) fn syntax_reply(&mut self, reply: Reply) -> Outcome {
        match reply {
            Reply::Highlights { id, version, spans, .. } => {
                let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
                    return Outcome::Continue;
                };
                // An answer to a version already superseded is behind what is
                // on screen; the runs already there are closer to the truth.
                if version < document.syntax.version {
                    return Outcome::Continue;
                }
                document.syntax.version = version;
                document.syntax.spans = spans;
                Outcome::Redraw
            }
            Reply::Disabled { id, language, why } => {
                if let Some(document) = self.docs.iter_mut().find(|document| document.id == id) {
                    document.syntax = Highlighting::default();
                }
                // Nothing will ask about this document again, and the worker is
                // still holding it and a clone of its text.
                self.syntax_close(id);
                self.warn(format!(
                    "Highlighting is off for this file: the {language} grammar {why}."
                ));
                Outcome::Redraw
            }
            Reply::Symbols { id, version, symbols } => {
                // The palette may have moved on to another file, or the file
                // may have been edited while the worker was reading it.
                if self.symbols.of != Some(id) || self.symbols.version != version {
                    return Outcome::Continue;
                }
                self.symbols.labels = symbols.iter().map(|symbol| symbol.name.clone()).collect();
                self.symbols.found = symbols;
                self.symbols.waiting = false;
                // The rows on screen were built without it.
                self.refresh_palette();
                Outcome::Redraw
            }
            Reply::Echo(_) => Outcome::Continue,
        }
    }

    /// The runs to draw for a document.
    pub(super) fn spans_of(document: &Document) -> &[Span] {
        &document.syntax.spans
    }

    /// Which language a document is in, for the status line.
    pub(super) const fn language_of(document: &Document) -> Option<&'static str> {
        // A grammar that had to be switched off clears this along with the
        // rest, so the status line stops claiming a language nun is no longer
        // reading the file as.
        document.syntax.language
    }

    /// Ask the parser for a marker, for tests waiting for it to catch up.
    #[cfg(test)]
    pub(super) fn syntax_echo(&self, marker: u64) {
        if let Some(worker) = self.syntax.as_ref() {
            worker.send(Request::Echo(marker));
        }
    }

    /// Start the parser, once the editor has somewhere to post its answers.
    pub fn attach_syntax(&mut self, worker: Worker) {
        self.syntax = Some(worker);
        let ids: Vec<DocId> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.syntax_open(id);
        }
        self.syntax_deadline = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{KeySet, defaults};
    use crossterm::event::{KeyCode, KeyEvent};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use ratatui::layout::Rect;
    use std::fs;
    use tempfile::TempDir;

    /// An editor with a file open and the parser attached, pumped the way the
    /// event loop pumps it.
    struct Tester {
        app: App,
        replies: std::sync::mpsc::Receiver<Reply>,
    }

    impl Tester {
        fn new(dir: &TempDir, name: &str, text: &str) -> Self {
            let path = dir.path().join(name);
            fs::write(&path, text).unwrap();

            let (buffer, _) = nun_core::Buffer::load(&path).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 80, 20));

            let (sender, replies) = std::sync::mpsc::channel();
            app.attach_syntax(Worker::new(Box::new(move |reply| {
                let _ = sender.send(reply);
            })));
            let mut tester = Self { app, replies };
            tester.settle();
            tester
        }

        /// Let the debounce fall due, then take everything the parser has to
        /// say up to a marker asked for now — so a file with nothing to
        /// highlight settles as fast as one with plenty.
        fn settle(&mut self) {
            let later = Instant::now() + DEBOUNCE * 4;
            self.app.tick(later);
            self.app.syntax_echo(1);
            loop {
                let reply = self
                    .replies
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the parser answered");
                if reply == Reply::Echo(1) {
                    return;
                }
                self.app.handle(Event::Syntax(reply));
            }
        }

        fn spans(&self) -> &[Span] {
            App::spans_of(self.app.doc())
        }

        fn type_text(&mut self, text: &str) {
            for ch in text.chars() {
                self.app.handle(Event::Key(KeyEvent::from(KeyCode::Char(ch))));
            }
        }

        /// The capture covering the first occurrence of `needle`.
        fn capture_of(&self, needle: &str) -> Option<&'static str> {
            let text = self.app.doc().buffer.text().to_string();
            let byte = text.find(needle)?;
            let at = u32::try_from(text[..byte].chars().count()).ok()?;
            self.spans()
                .iter()
                .find(|span| span.start <= at && span.end > at)
                .map(|span| span.capture)
        }
    }

    #[test]
    fn opening_a_rust_file_highlights_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = Tester::new(&dir, "main.rs", "fn main() {\n    // go\n}\n");

        assert!(!t.spans().is_empty(), "nothing was highlighted");
        assert!(t.capture_of("fn").unwrap().starts_with("keyword"));
        assert!(t.capture_of("// go").unwrap().starts_with("comment"));
    }

    #[test]
    fn a_file_opened_into_the_scratch_buffer_is_highlighted_too() {
        // `nun` with no argument starts on an empty, unnamed buffer, and the
        // first file opened from the tree or the palette replaces it in place
        // rather than arriving as a second tab. That path has to start the
        // parser as much as the one that makes a new tab does.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {\n    // go\n}\n").unwrap();

        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 20));
        let (sender, replies) = std::sync::mpsc::channel();
        app.attach_syntax(Worker::new(Box::new(move |reply| {
            let _ = sender.send(reply);
        })));

        app.open_in_tab(&path);
        let mut tester = Tester { app, replies };
        tester.settle();

        assert_eq!(tester.app.docs.len(), 1, "it replaced the scratch buffer");
        assert_eq!(App::language_of(tester.app.doc()), Some("rust"));
        assert!(!tester.spans().is_empty(), "nothing was highlighted");
        assert!(tester.capture_of("fn").unwrap().starts_with("keyword"));
    }

    /// The palette's labels, trimmed of the indent that shows nesting, each
    /// prefixed by its depth so nesting is visible in the assertion.
    fn outline_rows(app: &App) -> Vec<String> {
        app.finder
            .as_ref()
            .expect("the palette is open")
            .rows
            .iter()
            .map(|row| {
                let depth = row.entry.label.len() - row.entry.label.trim_start().len();
                format!("{}{}", depth / 2, row.entry.label.trim_start())
            })
            .collect()
    }

    #[test]
    fn the_palette_lists_what_the_file_declares() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(
            &dir,
            "shapes.rs",
            "struct Point;

impl Point {
    fn new() -> Self { Self }
}

fn main() {}
",
        );
        t.app.open_palette("@");
        t.settle();

        let rows = outline_rows(&t.app);
        assert!(rows.iter().any(|row| row == "0Point"), "{rows:?}");
        assert!(rows.iter().any(|row| row == "1new"), "a method nests: {rows:?}");
        assert!(rows.iter().any(|row| row == "0main"), "{rows:?}");
    }

    #[test]
    fn filtering_the_outline_keeps_the_ancestors_of_a_match() {
        // A method's name means little without the type it hangs off, and a
        // list of bare names is not an outline.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(
            &dir,
            "shapes.rs",
            "impl Circle {
    fn radius() {}
}

impl Square {
    fn side() {}
}
",
        );
        t.app.open_palette("@");
        t.settle();
        t.type_text("radius");

        let rows = outline_rows(&t.app);
        assert!(rows.iter().any(|row| row == "1radius"), "the match: {rows:?}");
        assert!(rows.iter().any(|row| row == "0Circle"), "and what it belongs to: {rows:?}");
        assert!(!rows.iter().any(|row| row.ends_with("side")), "but not the rest: {rows:?}");
    }

    #[test]
    fn going_to_a_symbol_leaves_room_above_it() {
        // Landing a definition on the top row hides what it belongs to.
        let dir = tempfile::tempdir().unwrap();
        let mut body = "// filler\n".repeat(60);
        body.push_str("fn needle() {}\n");
        let mut t = Tester::new(&dir, "long.rs", &body);
        t.app.open_palette("@");
        t.settle();
        t.type_text("needle");

        assert_eq!(t.app.pick_row(0, false), Outcome::Redraw);
        let doc = t.app.doc();
        let line = doc.buffer.line_of(doc.buffer.selections().primary().head);
        assert_eq!(line, 60, "the caret is on the definition");
        assert!(doc.scroll < line, "with something above it");
        assert_eq!(line - doc.scroll, 3, "three lines of it");
    }

    #[test]
    fn editing_the_file_makes_the_outline_stale() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "grow.rs", "fn first() {}\n");
        t.app.open_palette("@");
        t.settle();
        assert!(outline_rows(&t.app).iter().any(|row| row == "0first"));

        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Esc)));
        let end = t.app.doc().buffer.len_chars();
        t.app
            .doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(end)));
        t.type_text("fn second() {}\n");
        t.settle();

        t.app.open_palette("@");
        t.settle();
        let rows = outline_rows(&t.app);
        assert!(rows.iter().any(|row| row == "0second"), "the outline was read again: {rows:?}");
    }

    #[test]
    fn the_outline_is_read_from_the_text_the_parser_has_been_given() {
        // The reply is stamped with the version that was asked for, so asking
        // before the text behind that version has reached the worker would get
        // an outline of the old text under the new text's name — accepted as
        // current, and never asked for again.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "grow.rs", "fn first() {}\n");

        // Type without letting the debounce fall due, so the parser has not
        // been told, and then ask for the outline.
        let end = t.app.doc().buffer.len_chars();
        t.app
            .doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(end)));
        t.type_text("fn second() {}\n");
        assert!(t.app.doc().syntax.dirty, "the parser has not been told yet");

        t.app.open_palette("@");
        assert!(t.app.symbols.waiting, "the outline is owed");
        t.settle();

        let rows = outline_rows(&t.app);
        assert!(rows.iter().any(|row| row == "0second"), "it waited for the text: {rows:?}");
    }

    #[test]
    fn an_empty_outline_stops_the_palette_waiting() {
        // A grammar in trouble answers with an empty outline rather than with
        // nothing, because nothing leaves the palette saying "Reading the
        // file…" for the rest of the session.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        t.app.open_palette("@");
        t.settle();

        // The state the palette is in while it waits, and then the answer a
        // grammar in trouble sends: an outline with nothing in it.
        t.app.symbols.found.clear();
        t.app.symbols.labels.clear();
        t.app.symbols.waiting = true;
        let (id, version) = (t.app.doc().id, t.app.symbols.version);
        t.app.handle(Event::Syntax(Reply::Symbols { id, version, symbols: Vec::new() }));

        assert!(!t.app.symbols.waiting, "it is not still waiting");
        let labels: Vec<String> =
            t.app.finder.as_ref().unwrap().rows.iter().map(|row| row.entry.label.clone()).collect();
        assert!(
            labels.iter().all(|label| !label.contains("Reading")),
            "and does not claim to be reading: {labels:?}"
        );
    }

    #[test]
    fn a_file_in_a_language_nun_does_not_have_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let t = Tester::new(&dir, "notes.txt", "hello\n");
        assert!(t.spans().is_empty());
        assert_eq!(App::language_of(t.app.doc()), None);
    }

    #[test]
    fn the_status_line_says_which_language_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        assert!(t.app.status().1.contains("rust"), "{:?}", t.app.status().1);
    }

    #[test]
    fn typing_never_leaves_the_text_unhighlighted() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        assert!(!t.spans().is_empty());

        // Type a burst without letting the parser answer in between: the runs
        // already on screen must stay there.
        for _ in 0..20 {
            t.type_text("x");
            assert!(!t.spans().is_empty(), "the colours blanked while typing");
        }

        t.settle();
        assert!(!t.spans().is_empty());
    }

    #[test]
    fn what_is_typed_is_highlighted_once_the_parser_catches_up() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "\n");
        t.type_text("fn later() {}");
        t.settle();

        assert!(t.capture_of("fn").unwrap().starts_with("keyword"), "{:?}", t.spans());
        assert!(t.capture_of("later").unwrap().starts_with("function"));
    }

    #[test]
    fn an_answer_for_an_older_version_does_not_replace_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        t.type_text("\nfn second() {}");
        t.settle();
        let current = t.spans().to_vec();

        // An answer from before that edit arrives late.
        t.app.handle(Event::Syntax(Reply::Highlights {
            id: t.app.doc().id,
            version: 0,
            window: 0..10,
            spans: Vec::new(),
        }));
        assert_eq!(App::spans_of(t.app.doc()), current.as_slice(), "the stale answer won");
    }

    #[test]
    fn a_grammar_that_misbehaves_turns_itself_off_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        let id = t.app.doc().id;

        t.app.handle(Event::Syntax(Reply::Disabled {
            id,
            language: "rust",
            why: "took too long".into(),
        }));

        assert!(App::spans_of(t.app.doc()).is_empty());
        let message = t.app.message().unwrap();
        assert!(message.contains("rust") && message.contains("too long"), "{message}");

        // And the editor carries on: the file is still perfectly editable.
        t.type_text("x");
        assert!(t.app.doc().buffer.text().to_string().starts_with('x'));
    }

    #[test]
    fn each_document_keeps_its_own_highlighting() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, "main.rs", "fn main() {}\n");
        fs::write(dir.path().join("notes.txt"), "plain\n").unwrap();

        t.app.open_in_tab(&dir.path().join("notes.txt"));
        t.settle();
        assert!(t.spans().is_empty(), "the text file has no highlighting");

        t.app.open_in_tab(&dir.path().join("main.rs"));
        assert!(!t.spans().is_empty(), "and the Rust file still has its own");
    }

    #[test]
    fn a_buffer_with_no_file_yet_is_not_parsed() {
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 20));
        let (sender, _replies) = std::sync::mpsc::channel();
        app.attach_syntax(Worker::new(Box::new(move |reply| {
            let _ = sender.send(reply);
        })));

        app.handle(Event::Key(KeyEvent::from(KeyCode::Char('x'))));
        app.tick(Instant::now() + DEBOUNCE * 4);
        assert!(App::spans_of(app.doc()).is_empty());
    }
}
