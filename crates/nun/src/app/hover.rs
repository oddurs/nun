//! What a symbol is, in a card beside it: resting the pointer on it, or
//! asking from the caret.
//!
//! The text is not a hover target — every cell of it reporting enter and
//! leave would be noise — so resting on a symbol is watched for here, the
//! way Ctrl-hover watches for it: each movement of the pointer onto a new
//! symbol starts a wait, moving on before it is over abandons it, and
//! moving on after the server has been asked cancels the question. Only
//! once the pointer has stayed [`DELAY`] on one symbol is the server asked.
//!
//! The card is a pointer card that stays while the pointer is on the symbol
//! or in the card, never covers the symbol, and scrolls with the wheel. It
//! never takes the keyboard: the next key goes to the text, and puts the
//! card away as it does.
//!
//! Where the symbol is also under a diagnostic, one card says both — what
//! is wrong first, then what the symbol is — rather than two cards taking
//! turns. The diagnostic's own card, which opens on its own dwell, gives
//! way to it when the answer arrives.
//!
//! Links in the card are drawn as links and followed with a click whatever
//! the terminal: a web link is handed to the system's opener off the main
//! thread, and a link to a file opens it here.

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEventKind, MouseEvent, MouseEventKind};
use nun_lsp::types::request::HoverRequest;
use nun_lsp::types::{
    self as lsp, HoverContents, HoverParams, HoverProviderCapability, MarkedString, MarkupKind,
};
use nun_lsp::{Encoding, RequestId, Response};
use nun_ui::{CodeBudget, Event, Glyphs, Markdown};

use super::card::{Anchor, Card};
use super::navigation::{Open, Place};
use super::panes::DocId;
use super::{App, Outcome, Target};

/// How long the pointer rests on a symbol before its server is asked what
/// it is, unless the configuration says otherwise. Long enough that moving
/// the pointer across the text on the way somewhere does not scatter cards
/// over it.
pub const DELAY: Duration = Duration::from_millis(400);

/// What opens a web link on this system.
#[cfg(not(test))]
const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// The hover card: how it behaves, and what it is waiting on.
#[derive(Debug)]
pub(super) struct Hovering {
    /// How long the pointer rests before the server is asked.
    delay: Duration,
    /// Whether a card's links are marked with OSC 8 as well.
    hyperlinks: bool,
    /// The symbol being hovered, or asked about from the caret.
    spot: Option<Spot>,
    /// Links handed to the system's opener, which the tests look at instead
    /// of opening a browser.
    #[cfg(test)]
    launched: Vec<String>,
}

impl Default for Hovering {
    fn default() -> Self {
        Self {
            delay: DELAY,
            hyperlinks: true,
            spot: None,
            #[cfg(test)]
            launched: Vec::new(),
        }
    }
}

impl Hovering {
    /// Whether `id` is the question being waited on.
    pub(super) fn asked(&self, id: RequestId) -> bool {
        matches!(self.spot, Some(Spot { state: State::Asking(asked, _), .. }) if asked == id)
    }
}

/// One symbol, and where the question about it has got to.
#[derive(Debug)]
struct Spot {
    pane: usize,
    doc: DocId,
    /// The char asked about.
    at: usize,
    /// The chars the pointer can move over and still be on the same symbol:
    /// the word, or what the server said the answer is about.
    word: Range<usize>,
    /// Whether the pointer asked, rather than the keyboard.
    pointer: bool,
    state: State,
}

#[derive(Debug, Clone, Copy)]
enum State {
    /// Waiting until this for the pointer to settle.
    Settling(Instant),
    /// Asked, and the version of the document it was asked about.
    Asking(RequestId, Option<i32>),
    /// Answered, card or no card. Not asked again while the pointer stays.
    Answered,
}

impl App {
    /// How long the pointer rests on a symbol before its card is asked for,
    /// and whether links in cards are marked with OSC 8 too.
    pub fn set_hover(&mut self, delay: Duration, hyperlinks: bool) {
        self.hovering.delay = delay;
        self.hovering.hyperlinks = hyperlinks;
    }

    /// Whether the focused document's server can say what a symbol is.
    fn can_hover(&self) -> bool {
        let id = self.doc().id;
        self.lsp.as_ref().and_then(|lsp| lsp.capabilities(id)).is_some_and(|caps| {
            !matches!(caps.hover_provider, None | Some(HoverProviderCapability::Simple(false)))
        })
    }

    /// Whether the pointer needs following with no button held: whenever
    /// resting on a symbol could open a card.
    pub(super) fn wants_hover_motion(&self) -> bool {
        self.can_hover()
    }

    /// The symbol at `at` in the focused document: a word, or a single
    /// character of punctuation, which servers describe too (`?`, `.`).
    /// `None` on whitespace, where there is nothing to describe.
    fn hover_word(&self, at: usize) -> Option<Range<usize>> {
        let buffer = &self.doc().buffer;
        let ch = buffer.rope().get_char(at)?;
        if ch.is_whitespace() {
            return None;
        }
        if ch.is_alphanumeric() || ch == '_' {
            let (from, to) = buffer.word_range(at);
            return (from < to).then_some(from..to);
        }
        Some(at..at + 1)
    }

    // ── the pointer ─────────────────────────────────────────────────────────

    /// Any mouse report: start waiting when the pointer comes to a new
    /// symbol, and stop when it leaves one.
    pub(super) fn hover_pointer(&mut self, mouse: MouseEvent, now: Instant) -> Outcome {
        let target = self.hits.at(mouse.column, mouse.row).map(|hit| hit.target);
        if target.is_some_and(Target::in_card) {
            return Outcome::Continue;
        }
        if mouse.kind == MouseEventKind::Moved {
            return self.hover_motion(mouse.column, mouse.row, target, now);
        }
        // A press, a drag or the wheel is doing something else.
        self.leave_hover();
        Outcome::Continue
    }

    fn hover_motion(
        &mut self,
        column: u16,
        row: u16,
        target: Option<Target>,
        now: Instant,
    ) -> Outcome {
        // A question asked from the keyboard is not the pointer's to cancel.
        if matches!(self.hovering.spot, Some(Spot { pointer: false, state: State::Asking(..), .. }))
        {
            return Outcome::Continue;
        }
        let over_text = target.is_some_and(|target| target.pressed() == Target::Text);
        let at = (over_text && self.finder.is_none() && self.menu.is_none())
            .then(|| self.position_at(column, row))
            .flatten();
        let doc = self.doc().id;
        if let (Some(at), Some(spot)) = (at, self.hovering.spot.as_ref())
            && spot.pointer
            && spot.doc == doc
            && spot.word.contains(&at)
        {
            return Outcome::Continue;
        }

        // Somewhere else: whatever was asked about here is not wanted now,
        // and a card about the last symbol is not about this one.
        self.leave_hover();
        let mut outcome = Outcome::Continue;
        if self.card.as_ref().is_some_and(Card::is_held) {
            self.close_card();
            outcome = Outcome::Redraw;
            // Still on an underline: what it says comes back at once, as it
            // would have had the symbol's card not taken its place.
            if let Some(target @ Target::Diagnostic(..)) = target {
                outcome = outcome.and(self.dwelt(target));
            }
        }
        if let Some(word) = at.and_then(|at| self.hover_word(at)).filter(|_| self.can_hover()) {
            let at = at.unwrap_or(word.start);
            self.hovering.spot = Some(Spot {
                pane: self.panes.focus(),
                doc,
                at,
                word,
                pointer: true,
                state: State::Settling(now + self.hovering.delay),
            });
        }
        outcome
    }

    /// Stop waiting on the symbol, and cancel the question if it has been
    /// asked. Any card stays: it goes by its own rules.
    fn leave_hover(&mut self) {
        let Some(spot) = self.hovering.spot.take() else { return };
        if let State::Asking(id, _) = spot.state
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
    }

    /// Before any event: typing, a paste, or the terminal losing focus
    /// abandons a card still on its way. One that has arrived goes by its
    /// own rules.
    pub(super) fn hover_before(&mut self, event: &Event) {
        let abandons = match event {
            Event::Key(key) => {
                key.kind != KeyEventKind::Release && !matches!(key.code, KeyCode::Modifier(_))
            }
            Event::Paste(_) | Event::Focus(false) => true,
            _ => false,
        };
        if abandons {
            self.leave_hover();
        }
    }

    /// When the pointer will have settled, if it is settling.
    pub(super) fn hover_deadline(&self) -> Option<Instant> {
        match self.hovering.spot.as_ref()?.state {
            State::Settling(due) => Some(due),
            _ => None,
        }
    }

    /// The pointer has settled: ask about the symbol under it.
    pub(super) fn hover_tick(&mut self, now: Instant) -> Outcome {
        let Some(spot) = self.hovering.spot.as_ref() else { return Outcome::Continue };
        let State::Settling(due) = spot.state else { return Outcome::Continue };
        if now < due {
            return Outcome::Continue;
        }
        if spot.doc != self.doc().id {
            self.hovering.spot = None;
            return Outcome::Continue;
        }
        let state = self.ask_hover(spot.doc, spot.at);
        match (state, self.hovering.spot.as_mut()) {
            (Ok(state), Some(spot)) => spot.state = state,
            _ => self.hovering.spot = None,
        }
        Outcome::Continue
    }

    /// Send the question about `at` in `doc`.
    fn ask_hover(&mut self, doc: DocId, at: usize) -> Result<State, String> {
        let lsp = self.lsp.as_mut().ok_or_else(|| "no language server".to_string())?;
        let rope = self.docs.iter().find(|document| document.id == doc).map(|d| d.buffer.rope());
        let position = rope
            .and_then(|rope| lsp.position_params(doc, rope, at))
            .ok_or_else(|| "the file is not followed".to_string())?;
        let version = lsp.version(doc);
        let params = HoverParams {
            text_document_position_params: position,
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
        };
        let id = lsp.request::<HoverRequest>(doc, params).map_err(|error| error.to_string())?;
        Ok(State::Asking(id, version))
    }

    // ── the keyboard ────────────────────────────────────────────────────────

    /// Show what the symbol at the caret is, in a card beside it.
    pub(super) fn show_hover(&mut self) -> Outcome {
        if !self.can_hover() {
            self.message = Some(self.no_server("describe symbols"));
            return Outcome::Redraw;
        }
        self.leave_hover();
        let doc = self.doc().id;
        let at = self.doc().buffer.selections().primary().head;
        let word = self.hover_word(at).unwrap_or(at..at + 1);
        match self.ask_hover(doc, at) {
            Ok(state) => {
                let pane = self.panes.focus();
                self.hovering.spot = Some(Spot { pane, doc, at, word, pointer: false, state });
                Outcome::Continue
            }
            Err(error) => {
                self.message = Some(format!("Cannot describe the symbol: {error}."));
                Outcome::Redraw
            }
        }
    }

    // ── the answer ──────────────────────────────────────────────────────────

    /// The server answered the question being waited on.
    pub(super) fn hover_answered(&mut self, response: &Response) -> Outcome {
        let Some(State::Asking(_, asked)) = self.hovering.spot.as_ref().map(|spot| spot.state)
        else {
            return Outcome::Continue;
        };
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(response.doc));
        let encoding =
            self.lsp.as_ref().and_then(|lsp| lsp.encoding(response.doc)).unwrap_or_default();
        let pointer = self.hovering.spot.as_ref().is_some_and(|spot| spot.pointer);
        match response.parse::<HoverRequest>() {
            // An answer about text that has changed since may be about
            // another symbol altogether.
            Ok(_) if current != asked => {
                self.hovering.spot = None;
                Outcome::Continue
            }
            Ok(found) => self.hover_found(found, encoding),
            Err(error) if !pointer => {
                self.hovering.spot = None;
                self.message = Some(format!("Could not describe the symbol: {error}."));
                Outcome::Redraw
            }
            Err(_) => {
                self.hovering.spot = None;
                Outcome::Continue
            }
        }
    }

    /// What the server said the symbol is, if anything: shown in a card
    /// beside it, with what any diagnostic on it says.
    pub(super) fn hover_found(&mut self, found: Option<lsp::Hover>, encoding: Encoding) -> Outcome {
        let Some(spot) = self.hovering.spot.as_mut() else { return Outcome::Continue };
        spot.state = State::Answered;
        let (pane, doc, at, pointer) = (spot.pane, spot.doc, spot.at, spot.pointer);
        if !pointer {
            self.hovering.spot = None;
        }
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        if self.panes.get(pane).and_then(super::panes::Pane::current) != Some(doc) {
            return Outcome::Continue;
        }

        // The server knows better than a word boundary what the answer is
        // about, as long as that covers what was asked about.
        let rope = document.buffer.rope();
        let span =
            found.as_ref().and_then(|hover| hover.range).map(|r| encoding.char_range(rope, r));
        let language = Self::language_of(document);
        let word = match (span, self.hovering.spot.as_ref()) {
            (Some(span), _) if !span.is_empty() && span.contains(&at) => span,
            (_, Some(spot)) => spot.word.clone(),
            _ => self.hover_word(at).unwrap_or(at..at + 1),
        };
        if let Some(spot) = self.hovering.spot.as_mut() {
            spot.word = word.clone();
        }

        let markdown = found.map_or_else(Markdown::default, |hover| {
            contents(hover.contents, language, self.palette.glyphs())
        });
        let problems = self.problems_on(doc, &word);
        let fixable = !problems.is_empty();
        if markdown.is_empty() && problems.is_empty() {
            if pointer {
                return Outcome::Continue;
            }
            self.message = Some(format!("Nothing to say about `{}`.", self.hover_text(&word)));
            return Outcome::Redraw;
        }
        let mut body = problems;
        if !body.is_empty() && !markdown.is_empty() {
            body.push(Vec::new());
        }
        body.extend(markdown.body);

        let anchor = Anchor::Text { pane, doc, from: word.start, to: word.end };
        let card = Card::new(anchor, body, Vec::new(), None)
            .with_links(markdown.links, self.hovering.hyperlinks);
        self.show_card(if pointer { card.held_over_anchor() } else { card });
        if fixable {
            self.ask_fixes();
        }
        Outcome::Redraw
    }

    /// What the diagnostics on `word` say, as a card says it; nothing when
    /// there are none.
    fn problems_on(&self, doc: DocId, word: &Range<usize>) -> Vec<nun_ui::Paragraph> {
        let marks = self.diagnostics.marks(doc);
        let on = marks.iter().position(|mark| {
            mark.start < word.end.max(word.start + 1) && word.start < mark.end.max(mark.start + 1)
        });
        on.map_or_else(Vec::new, |index| self.card_body(doc, index))
    }

    /// The text of `range` in the focused document.
    fn hover_text(&self, range: &Range<usize>) -> String {
        let rope = self.doc().buffer.rope();
        let end = range.end.min(rope.len_chars());
        rope.slice(range.start.min(end)..end).to_string()
    }

    // ── links ───────────────────────────────────────────────────────────────

    /// A link in a card was clicked. A file opens here, at the line its
    /// fragment names; a web page or an address opens in whatever the
    /// system opens them with. Anything else — a server's own command
    /// scheme, say — is refused rather than guessed at.
    pub(super) fn follow_link(&mut self, link: &str) -> Outcome {
        let scheme = link.split_once(':').map(|(scheme, _)| scheme.to_ascii_lowercase());
        match scheme.as_deref() {
            Some("file") => self.follow_file_link(link),
            Some("http" | "https" | "mailto") => {
                // Handed over as one argument, never through a shell, and
                // only as printable ASCII: the scheme was checked, so it
                // cannot be read as an option either.
                if !link.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
                    self.message =
                        Some("That link has characters in it nun will not pass on.".into());
                    return Outcome::Redraw;
                }
                self.launch(link.to_string());
                self.message = Some(format!("Opening {link}"));
                Outcome::Redraw
            }
            Some(scheme) => {
                self.message = Some(format!("nun does not open {scheme}: links."));
                Outcome::Redraw
            }
            None => {
                self.message = Some(format!("{link} is not a link nun can open."));
                Outcome::Redraw
            }
        }
    }

    /// Open a `file:` link here, at the line in its fragment if it has one:
    /// `#L12`, or `#12`, as servers write them.
    fn follow_file_link(&mut self, link: &str) -> Outcome {
        let (base, fragment) = link.split_once('#').unwrap_or((link, ""));
        let path = base.parse::<lsp::Uri>().ok().and_then(|uri| nun_lsp::uri::to_path(&uri));
        let Some(path) = path else {
            self.message = Some(format!("{link} is not a file nun can open."));
            return Outcome::Redraw;
        };
        let digits: String = fragment
            .trim_start_matches(['L', 'l'])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let row = digits.parse::<u32>().map_or(0, |number| number.saturating_sub(1));
        let at = lsp::Position { line: row, character: 0 };
        let place =
            Place { path, range: lsp::Range { start: at, end: at }, encoding: Encoding::Utf16 };
        self.pick_place(&place, Open::Here)
    }

    /// Hand `link` to the system's opener, on a thread of its own: the
    /// opener may take a moment, and nothing waits on it.
    #[cfg(not(test))]
    #[allow(clippy::unused_self, reason = "the tests' version records what it was given")]
    fn launch(&mut self, link: String) {
        use std::process::{Command, Stdio};

        std::thread::spawn(move || {
            let _ = Command::new(OPENER)
                .arg(link)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    #[cfg(test)]
    fn launch(&mut self, link: String) {
        self.hovering.launched.push(link);
    }
}

/// What a hover says, as a card shows it: every form the protocol allows.
/// All of its code shares one card's allowance of highlighting.
fn contents(contents: HoverContents, language: Option<&str>, glyphs: &Glyphs) -> Markdown {
    let mut budget = CodeBudget::new();
    let mut marked = |marked: MarkedString| match marked {
        MarkedString::String(text) => Markdown::parse(&text, language, &mut budget, glyphs),
        MarkedString::LanguageString(code) => {
            Markdown::code(&code.language, &code.value, &mut budget)
        }
    };
    match contents {
        HoverContents::Scalar(one) => marked(one),
        HoverContents::Array(many) => {
            let mut all = Markdown::default();
            for one in many {
                all.append(marked(one));
            }
            all
        }
        HoverContents::Markup(markup) => match markup.kind {
            MarkupKind::Markdown => Markdown::parse(&markup.value, language, &mut budget, glyphs),
            MarkupKind::PlainText => Markdown::plain(&markup.value),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{Receiver, channel};

    use crossterm::event::{KeyEvent, KeyModifiers, MouseButton};
    use nun_core::{Buffer, Range as Span, Selections};
    use nun_lsp::types::{
        Diagnostic, DiagnosticSeverity, Hover, LanguageString, MarkupContent, Position,
    };
    use nun_lsp::{Lsp, ServerSpec};
    use nun_theme::{Probe, Role, derive};
    use nun_ui::Palette;
    use ratatui::buffer::Buffer as Cells;
    use ratatui::layout::Rect;

    use super::*;
    use crate::commands::Command;

    /// A language server in a few lines of shell. It says it can hover, and
    /// answers a hover with a signature, a rule, and a line of docs with a
    /// link in it — unless started with `slow`, when it never answers one.
    /// It writes the method of everything it is sent to the file it is
    /// given, so a test can see what reached it.
    const SERVER: &str = r#"
mode="$1"
log="$2"
while :; do
  len=
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
  done
  [ -z "$len" ] && exit 0
  body=$(dd bs=1 count="$len" 2>/dev/null)
  printf '%s\n' "$body" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p' >> "$log"
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$body" in
    *'"method":"initialize"'*)
      result='{"capabilities":{"textDocumentSync":1,"hoverProvider":true}}' ;;
    *'"method":"textDocument/hover"'*)
      [ "$mode" = slow ] && continue
      result='{"contents":{"kind":"markdown","value":"```rust\nfn helper()\n```\n---\nDoes [things](https://example.com/x)."}}' ;;
    *'"method":"shutdown"'*) result=null ;;
    *'"method":"exit"'*) exit 0 ;;
    *) continue ;;
  esac
  reply="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  printf 'Content-Length: %s\r\n\r\n%s' "${#reply}" "$reply"
done
"#;

    const TEXT: &str = "fn main() {\n    helper();\n}\n";

    struct Tester {
        app: App,
        events: Receiver<nun_lsp::Event>,
        dir: tempfile::TempDir,
    }

    impl Tester {
        /// An editor over `main.rs` holding [`TEXT`], with [`SERVER`] as its
        /// Rust server, ready to be asked.
        fn new(mode: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("main.rs");
            std::fs::write(&path, TEXT).unwrap();
            let (buffer, _) = Buffer::load(&path).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                crate::commands::defaults(crate::commands::KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 80, 16));
            let log = dir.path().join("log").to_string_lossy().into_owned();
            let spec = ServerSpec {
                command: "sh".into(),
                args: vec!["-c".into(), SERVER.into(), "server".into(), mode.into(), log],
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
            let mut tester = Self { app, events, dir };
            tester.until(App::can_hover);
            tester.app.relayout();
            tester
        }

        /// Hand the editor what the server says, and tick it, until `done`.
        fn until(&mut self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done(&self.app) {
                assert!(Instant::now() < deadline, "gave up waiting");
                if let Ok(event) = self.events.recv_timeout(Duration::from_millis(20)) {
                    self.app.handle(Event::Lsp(event));
                }
                self.app.tick(Instant::now());
            }
        }

        /// What the server has been sent, by method.
        fn received(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
        }

        /// Where `helper` is on screen.
        fn helper(&self) -> (u16, u16) {
            let (text, _) = self.app.areas();
            (text.x + self.app.gutter_width() + 4, text.y + 1)
        }

        fn moved(&mut self, column: u16, row: u16, now: Instant) -> Outcome {
            self.app.handle_at(mouse(MouseEventKind::Moved, column, row), now)
        }

        /// Rest on `helper` until its card is up.
        fn hover_helper(&mut self) {
            let (x, y) = self.helper();
            let now = Instant::now();
            self.moved(x + 2, y, now);
            let due = self.app.deadline().expect("waiting for the pointer to settle");
            self.app.tick(due);
            self.until(|app| app.card.is_some());
        }

        fn card_text(&self) -> String {
            let area = self.app.card_area().expect("a card on screen");
            let mut cells = Cells::empty(self.app.viewport);
            self.app.render(self.app.viewport, &mut cells);
            (area.y..area.bottom())
                .map(|y| {
                    (area.x..area.right())
                        .map(|x| strip_osc8(cells[(x, y)].symbol()))
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// The first cell, reading order, the hit map says is `target`.
    fn find(app: &App, target: Target) -> Option<(u16, u16)> {
        let area = app.viewport;
        (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
            .find(|&(x, y)| app.hits.at(x, y).is_some_and(|hit| hit.target == target))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    }

    /// A cell's text without any OSC 8 wrapped round it.
    fn strip_osc8(symbol: &str) -> String {
        let mut out = String::new();
        let mut rest = symbol;
        while let Some(start) = rest.find("\x1b]8;") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            let end = after.find("\x1b\\").map_or(after.len(), |end| end + 2);
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn resting_on_a_symbol_opens_its_card_beside_it_with_the_markdown_rendered() {
        let mut t = Tester::new("answer");
        assert!(t.app.wants_motion(), "the pointer is followed over plain text");
        t.hover_helper();

        let card = t.app.card_area().unwrap();
        let (x, y) = t.helper();
        let symbol = Rect::new(x, y, 6, 1);
        assert!(!card.intersects(symbol), "{card:?} covers the symbol at {symbol:?}");
        assert_eq!(card.y, y + 1, "just below it");

        let text = t.card_text();
        assert!(text.contains("fn helper()"), "{text}");
        assert!(text.contains("Does things."), "{text}");
        assert!(!text.contains("```") && !text.contains("---") && !text.contains("]("), "{text}");
        assert!(t.received().contains("textDocument/hover"));
    }

    #[test]
    fn the_signature_is_highlighted_and_the_link_is_a_link() {
        let mut t = Tester::new("answer");
        t.hover_helper();
        let card = t.app.card_area().unwrap();
        let mut cells = Cells::empty(t.app.viewport);
        t.app.render(t.app.viewport, &mut cells);
        let keyword = t.app.palette.on(Role::Overlay, Role::Keyword).fg;
        assert_eq!(cells[(card.x + 1, card.y)].symbol(), "f");
        assert_eq!(cells[(card.x + 1, card.y)].fg, keyword.unwrap(), "`fn` is a keyword");

        let (x, y) = find(&t.app, Target::CardLink(0)).expect("the link is in the hit map");
        let cell = &cells[(x, y)];
        assert!(cell.symbol().starts_with("\x1b]8;id=nun-0;https://example.com/x\x1b\\"));
        assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));

        // Clicked, it goes to whatever opens web pages.
        t.app.handle(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
        assert_eq!(t.app.hovering.launched, ["https://example.com/x"]);
        assert!(t.app.card.is_none());
    }

    #[test]
    fn osc8_can_be_turned_off_and_the_link_still_clicks() {
        let mut t = Tester::new("answer");
        t.app.set_hover(DELAY, false);
        t.hover_helper();
        let mut cells = Cells::empty(t.app.viewport);
        t.app.render(t.app.viewport, &mut cells);
        assert!(!cells.content().iter().any(|cell| cell.symbol().contains('\x1b')));
        assert!(find(&t.app, Target::CardLink(0)).is_some());
    }

    #[test]
    fn the_card_stays_over_the_symbol_and_the_card_and_goes_elsewhere() {
        let mut t = Tester::new("answer");
        t.hover_helper();
        let (x, y) = t.helper();
        let now = Instant::now();
        t.moved(x, y, now);
        assert!(t.app.card.is_some(), "along the same symbol");
        let card = t.app.card_area().unwrap();
        t.moved(card.x + 2, card.y, now);
        assert!(t.app.card.is_some(), "into the card");
        t.app.handle(mouse(MouseEventKind::ScrollDown, card.x + 2, card.y));
        assert!(t.app.card.is_some(), "the wheel scrolls it rather than closing it");
        t.moved(x - 2, y, now);
        assert!(t.app.card.is_none(), "whitespace is not the symbol");
    }

    #[test]
    fn the_card_does_not_take_the_keyboard() {
        let mut t = Tester::new("answer");
        t.hover_helper();
        t.app.handle(Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
        assert!(t.app.card.is_none());
        assert!(t.app.buffer().text().to_string().starts_with('x'), "the key was typed");
    }

    #[test]
    fn moving_away_before_the_answer_cancels_the_question() {
        let mut t = Tester::new("slow");
        let (x, y) = t.helper();
        let now = Instant::now();
        t.moved(x, y, now);
        let due = t.app.deadline().unwrap();
        t.app.tick(due);
        let id = match t.app.hovering.spot.as_ref().map(|spot| spot.state) {
            Some(State::Asking(id, _)) => id,
            other => panic!("asked: {other:?}"),
        };
        assert!(t.app.lsp.as_ref().unwrap().is_pending(id));

        t.moved(x, y + 1, now);
        assert!(t.app.hovering.spot.is_none());
        assert!(!t.app.lsp.as_ref().unwrap().is_pending(id), "cancelled");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !t.received().contains("$/cancelRequest") {
            assert!(Instant::now() < deadline, "the server was told: {}", t.received());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn moving_away_before_the_delay_asks_nothing() {
        let mut t = Tester::new("answer");
        let (x, y) = t.helper();
        let now = Instant::now();
        t.moved(x, y, now);
        assert_eq!(t.app.deadline(), Some(now + DELAY));
        t.moved(x - 3, y, now + DELAY / 2);
        assert_eq!(t.app.deadline(), None);
        t.app.tick(now + DELAY * 2);
        assert!(t.app.hovering.spot.is_none());
    }

    #[test]
    fn the_delay_is_as_configured() {
        let mut t = Tester::new("answer");
        t.app.set_hover(Duration::from_millis(1200), true);
        let (x, y) = t.helper();
        let now = Instant::now();
        t.moved(x, y, now);
        assert_eq!(t.app.deadline(), Some(now + Duration::from_millis(1200)));
        t.app.tick(now + DELAY);
        assert!(matches!(t.app.hovering.spot.as_ref().unwrap().state, State::Settling(_)));
    }

    #[test]
    fn show_hover_opens_a_keyboard_card_at_the_caret() {
        let mut t = Tester::new("answer");
        let at = t.app.doc().buffer.line_start(1) + 6;
        t.app.doc_mut().buffer.set_selections(Selections::single(Span::caret(at)));
        t.app.run(Command::ShowHover);
        t.until(|app| app.card.is_some());
        assert!(!t.app.card.as_ref().unwrap().is_held(), "a keyboard card");
        let card = t.app.card_area().unwrap();
        let (x, y) = t.helper();
        assert!(!card.intersects(Rect::new(x, y, 6, 1)));

        // The pointer passing over the text leaves it be.
        t.moved(0, 0, Instant::now());
        assert!(t.app.card.is_some());
        t.app.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(t.app.card.is_none());
    }

    #[test]
    fn a_diagnostic_on_the_symbol_shares_its_card() {
        let mut t = Tester::new("answer");
        let id = t.app.doc().id;
        let text = t.app.doc().buffer.rope().clone();
        let diagnostic = Diagnostic {
            range: lsp::Range {
                start: Position { line: 1, character: 4 },
                end: Position { line: 1, character: 10 },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            message: "cannot find function `helper`".into(),
            ..Diagnostic::default()
        };
        t.app.diagnostics.opened(id, 0, &text);
        t.app.diagnostics.publish(id, Some((Some(0), &[diagnostic])), Encoding::Utf16);
        t.app.relayout();

        t.hover_helper();
        let text = t.card_text();
        let problem = text.find("cannot find function").expect(&text);
        let signature = text.find("fn helper()").expect(&text);
        assert!(problem < signature, "what is wrong first: {text}");
        assert!(t.app.card.as_ref().unwrap().is_held());

        // The underline's own dwell does not replace it.
        let (x, y) = t.helper();
        let target = t.app.hits.at(x, y).map(|hit| hit.target).unwrap();
        assert!(matches!(target, Target::Diagnostic(..)));
        t.app.dwelt(target);
        assert!(t.card_text().contains("fn helper()"));
    }

    #[test]
    fn a_file_link_opens_the_file_at_its_line_and_others_are_refused() {
        let mut t = Tester::new("answer");
        let other = t.dir.path().join("lib.rs");
        std::fs::write(&other, "one\ntwo\nthree\n").unwrap();
        let uri = nun_lsp::uri::from_path(&other).unwrap();
        t.app.follow_link(&format!("{}#L3", uri.as_str()));
        assert_eq!(t.app.doc().buffer.path(), Some(other.as_path()));
        let head = t.app.doc().buffer.selections().primary().head;
        assert_eq!(t.app.doc().buffer.line_of(head), 2);

        t.app.follow_link("command:rust-analyzer.run");
        assert_eq!(t.app.message(), Some("nun does not open command: links."));
        t.app.follow_link("https://example.com/\x1b]evil");
        assert!(t.app.hovering.launched.is_empty(), "a control byte is not passed on");
    }

    #[test]
    fn every_form_a_hover_can_take_is_read() {
        let plain = |markdown: &Markdown| -> String {
            markdown
                .body
                .iter()
                .map(|paragraph| paragraph.iter().map(|run| run.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("|")
        };
        let scalar = HoverContents::Scalar(MarkedString::String("*a*".into()));
        assert_eq!(plain(&contents(scalar, None, &Glyphs::default())), "a");
        let code = HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
            language: "rust".into(),
            value: "fn f()".into(),
        }));
        let code = contents(code, None, &Glyphs::default());
        assert_eq!(plain(&code), "fn f()");
        assert!(code.body[0].iter().any(|run| run.role == Role::Keyword));
        let array = HoverContents::Array(vec![
            MarkedString::String("one".into()),
            MarkedString::String("two".into()),
        ]);
        assert_eq!(plain(&contents(array, None, &Glyphs::default())), "one||two");
        let text = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: "*not* markdown".into(),
        });
        assert_eq!(plain(&contents(text, None, &Glyphs::default())), "*not* markdown");
        let markdown = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: "```\nlet x\n```".into(),
        });
        let markdown = contents(markdown, Some("rust"), &Glyphs::default());
        assert!(
            markdown.body[0].iter().any(|run| run.role == Role::Keyword),
            "the file's language"
        );
    }

    #[test]
    fn an_empty_answer_shows_nothing_and_says_so_to_the_keyboard() {
        let mut t = Tester::new("answer");
        t.app.hovering.spot = Some(Spot {
            pane: 0,
            doc: t.app.doc().id,
            at: 0,
            word: 0..2,
            pointer: false,
            state: State::Answered,
        });
        t.app.hover_found(
            Some(Hover { contents: HoverContents::Array(vec![]), range: None }),
            Encoding::Utf16,
        );
        assert!(t.app.card.is_none());
        assert_eq!(t.app.message(), Some("Nothing to say about `fn`."));
    }
}
