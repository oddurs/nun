//! Completion: suggestions from the language server, offered as you type and
//! never waited for.
//!
//! **Asking.** A character the server declares a trigger — `.`, `::` — asks
//! it, and so does the explicit command (`Ctrl+Space`, the palette, or the
//! right-click menu). The question goes out after the keystroke's own edit,
//! so the server is always asked about text it has been told about.
//!
//! **Typing on.** The popup is not a mode. Every key but the few it answers —
//! Up, Down, Page Up and Down, Enter, Shift+Enter, Tab and Escape — edits the
//! text exactly as it would without it. The answer is filtered and re-sorted
//! locally on every keystroke, so the popup keeps up however slow the server
//! is; only a list the server marked incomplete is asked for again, and the
//! old list stays up until the new one arrives. Anything but typing or
//! deleting at the caret — moving it, clicking elsewhere, switching tabs —
//! closes the popup and cancels what it asked, so an answer about a word
//! nobody is completing any more never lands. An answer is matched to its
//! question by request id, and superseded questions are cancelled, not just
//! ignored.
//!
//! **Accepting.** Enter, Tab or a click takes the item. The item's insert
//! range is used — the word up to the caret — and Shift+Enter uses its
//! replace range instead, which takes the rest of the word too. Edits
//! elsewhere (an auto-import) go in with it, as one undo step, with the
//! server's `\r\n` made `\n` like everything else the buffer holds.
//!
//! **Several carets.** The item goes in at every caret whose preceding
//! characters are the same as the primary caret's — which after typing a
//! word at several carets is all of them. A caret in front of different text
//! is left alone, and so is a caret with a selection. Edits elsewhere go in
//! once.
//!
//! **Snippets.** A snippet's placeholders become tab-stops. Tab moves every
//! caret's copy of the snippet to its next stop at once, and Shift+Tab back;
//! a stop's placeholders are selected, so typing replaces them, and a stop
//! used twice is edited in both places. Every stop is washed while the
//! snippet is live — the current one, with its mirrors, more strongly — so a
//! Tab that means something else is never a surprise. Reaching the final stop,
//! pressing Escape, or moving the caret out of the stop ends it, takes the
//! marks off, and Tab goes back to typing a tab.
//!
//! **Mouse.** Hovering a row previews its documentation beside the popup;
//! clicking it accepts it, and Shift-clicking accepts it over the whole word
//! as Shift+Enter does; the wheel over the popup scrolls it. Clicking into
//! another of a snippet's stops goes to that stop, as Tab would. Documentation
//! is asked for only when an item is looked at, by selection or by hover.

use std::ops::Range as Span;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_core::{Buffer, Edit, Range, Selections};
use nun_lsp::completion::{self as model, Asked, Mode, Shown};
use nun_lsp::snippet::{self, Snippet};
use nun_lsp::types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionItemTag, CompletionParams,
    CompletionTriggerKind, Documentation, request,
};
use nun_lsp::{RequestId, Response};
use nun_theme::Role;
use nun_ui::{CompletionView, DocsView, EditorView, MOST_SUGGESTIONS, Suggestion};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget as _;

use super::panes::DocId;
use super::{App, Document, Focus, Outcome, Target};

/// Completion's state: the popup, the tab-stops of an inserted snippet, and
/// what the current keystroke has asked for.
#[derive(Debug, Default)]
pub(super) struct Completion {
    popup: Option<Popup>,
    stops: Option<Tabstops>,
    /// The character the key being handled types, until the event is over.
    typed: Option<char>,
    /// A question to ask once the event's edits have gone to the server.
    wanted: Option<Trigger>,
    /// Whether the tab-stops ended during the event, and their marks have to
    /// come off the screen.
    ended: bool,
}

/// Why a question is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// Somebody asked.
    Invoked,
    /// A trigger character was typed.
    Character(char),
    /// The list was incomplete and the word has changed.
    Incomplete,
}

/// A question on its way.
#[derive(Debug)]
struct Pending {
    id: RequestId,
    asked: Asked,
    version: i32,
}

/// The popup: the word it is completing, and what the server offered.
#[derive(Debug)]
struct Popup {
    doc: DocId,
    /// Where the word being completed starts.
    start: usize,
    /// The line it is on.
    line: usize,
    /// What the line holds before `start`, and after the caret: while both
    /// stand, the only change since has been typing at the caret.
    before: String,
    after: String,
    /// How many selections there were.
    carets: usize,
    /// Whether it was asked for, rather than triggered — and so worth saying
    /// when there is nothing to offer.
    explicit: bool,
    pending: Option<Pending>,
    /// The question the items answer.
    asked: Option<Asked>,
    items: Vec<CompletionItem>,
    incomplete: bool,
    /// Whether each item has had its documentation asked for.
    resolved: Vec<bool>,
    resolving: Option<(RequestId, usize)>,
    /// What has been typed of the word.
    query: String,
    shown: Vec<Shown>,
    selected: usize,
    scroll: usize,
}

impl Popup {
    fn refilter(&mut self, query: String) {
        self.shown = model::filter(&self.items, &query);
        self.query = query;
        self.selected = 0;
        self.scroll = 0;
    }

    fn item(&self, row: usize) -> Option<&CompletionItem> {
        self.shown.get(row).map(|found| &self.items[found.index])
    }
}

/// The tab-stops of an inserted snippet, in every copy of it.
#[derive(Debug)]
struct Tabstops {
    doc: DocId,
    /// For each stop in the order Tab visits them, where it is in each copy:
    /// which copy, and the chars it covers.
    stops: Vec<Vec<(usize, Span<usize>)>>,
    /// The copy the primary caret is in.
    primary: usize,
    /// Carets the completion did not go in at, which stay where they are.
    others: Vec<Span<usize>>,
    current: usize,
    /// How long the text should be, once every edit since has been followed.
    /// When it is not, an edit went unseen and the stops cannot be trusted.
    len: usize,
}

impl Completion {
    /// Follow the edits just made to `doc`, oldest first, each in the text as
    /// it stood before it.
    pub(super) fn follow_edits(&mut self, doc: DocId, edits: &[Edit]) {
        let Some(stops) = self.stops.as_mut().filter(|stops| stops.doc == doc) else { return };
        for edit in edits {
            for (index, stop) in stops.stops.iter_mut().enumerate() {
                let current = index == stops.current;
                for (_, span) in stop.iter_mut() {
                    *span = map_span(span, edit, current);
                }
            }
            for span in &mut stops.others {
                *span = map_span(span, edit, false);
            }
            stops.len = (stops.len + edit.inserted()).saturating_sub(edit.removed());
        }
    }
}

/// Where a stop's `span` is once `edit` has been made.
///
/// The stop being typed in grows with what is typed at its edges. Any other
/// stop keeps out of it: one that ends where the edit starts stays before it,
/// and one that starts where it ends moves after it.
fn map_span(span: &Span<usize>, edit: &Edit, current: bool) -> Span<usize> {
    use nun_core::Assoc::{After, Before};
    if current {
        return edit.map_pos(span.start, Before)..edit.map_pos(span.end, After);
    }
    // An empty stop at a pure insert goes after it — `$1$2`, typing in the
    // first — but one where a replacement starts stays before it.
    let before = span.end < edit.start
        || (span.end == edit.start && (span.start < span.end || edit.start < edit.end));
    if before {
        return span.clone();
    }
    if span.start >= edit.end {
        let moved = |pos: usize| (pos + edit.inserted()).saturating_sub(edit.removed());
        return moved(span.start)..moved(span.end);
    }
    edit.map_pos(span.start, Before)..edit.map_pos(span.end, After)
}

/// Whether `ch` is part of a word being completed.
fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The start of the word that ends at `head`, on its line.
fn word_start(buffer: &Buffer, head: usize) -> usize {
    let line_start = buffer.line_start(buffer.line_of(head));
    let rope = buffer.rope();
    // By cluster, so a combining mark or a virama stays with its letter.
    let mut start = head;
    while start > line_start {
        let cluster = buffer.prev_grapheme(start).max(line_start);
        if !is_word(rope.char(cluster)) {
            break;
        }
        start = cluster;
    }
    start
}

/// A few letters for what an item is, and the colour they are drawn in.
fn kind_of(kind: Option<CompletionItemKind>) -> (&'static str, Role) {
    let Some(kind) = kind else { return ("", Role::Dim) };
    match kind {
        CompletionItemKind::METHOD => ("meth", Role::Function),
        CompletionItemKind::FUNCTION => ("fn", Role::Function),
        CompletionItemKind::CONSTRUCTOR => ("new", Role::Function),
        CompletionItemKind::FIELD => ("fld", Role::Text),
        CompletionItemKind::VARIABLE => ("var", Role::Text),
        CompletionItemKind::CLASS => ("cls", Role::Type),
        CompletionItemKind::INTERFACE => ("ifc", Role::Type),
        CompletionItemKind::MODULE => ("mod", Role::Type),
        CompletionItemKind::PROPERTY => ("prop", Role::Text),
        CompletionItemKind::UNIT => ("unit", Role::Number),
        CompletionItemKind::VALUE => ("val", Role::Number),
        CompletionItemKind::ENUM => ("enum", Role::Type),
        CompletionItemKind::KEYWORD => ("kw", Role::Keyword),
        CompletionItemKind::SNIPPET => ("snip", Role::Dim),
        CompletionItemKind::COLOR => ("col", Role::Number),
        CompletionItemKind::FILE => ("file", Role::Dim),
        CompletionItemKind::REFERENCE => ("ref", Role::Dim),
        CompletionItemKind::FOLDER => ("dir", Role::Dim),
        CompletionItemKind::ENUM_MEMBER => ("var", Role::Number),
        CompletionItemKind::CONSTANT => ("const", Role::Number),
        CompletionItemKind::STRUCT => ("st", Role::Type),
        CompletionItemKind::EVENT => ("evt", Role::Text),
        CompletionItemKind::OPERATOR => ("op", Role::Punctuation),
        CompletionItemKind::TYPE_PARAMETER => ("T", Role::Type),
        _ => ("txt", Role::Dim),
    }
}

/// The documentation of an item, as plain text: its detail, then its
/// documentation.
fn docs_of(item: &CompletionItem) -> String {
    let documentation = match &item.documentation {
        Some(Documentation::String(text)) => text.as_str(),
        Some(Documentation::MarkupContent(markup)) => markup.value.as_str(),
        None => "",
    };
    match item.detail.as_deref() {
        Some(detail) if !documentation.is_empty() => format!("{detail}\n\n{documentation}"),
        Some(detail) => detail.to_string(),
        None => documentation.to_string(),
    }
}

/// The snippet variables nun can answer, for the caret at `head` in `buffer`
/// with `word` typed of the word being completed.
fn variables(buffer: &Buffer, head: usize, word: &str) -> Vec<(&'static str, String)> {
    let line = buffer.line_of(head);
    let mut known = vec![
        ("TM_LINE_INDEX", line.to_string()),
        ("TM_LINE_NUMBER", (line + 1).to_string()),
        ("TM_CURRENT_LINE", buffer.line_text(line).trim_end_matches('\n').to_string()),
        ("TM_CURRENT_WORD", word.to_string()),
        ("TM_SELECTED_TEXT", String::new()),
    ];
    if let Some(path) = buffer.path() {
        let text = |value: Option<&std::ffi::OsStr>| {
            value.map(|value| value.to_string_lossy().into_owned()).unwrap_or_default()
        };
        known.push(("TM_FILEPATH", path.display().to_string()));
        known.push(("TM_FILENAME", text(path.file_name())));
        known.push(("TM_FILENAME_BASE", text(path.file_stem())));
        known.push((
            "TM_DIRECTORY",
            path.parent().map(|dir| dir.display().to_string()).unwrap_or_default(),
        ));
    }
    known
}

impl App {
    // ── keys ────────────────────────────────────────────────────────────────

    /// A key, before the keymap has it: the few the popup and the tab-stops
    /// answer. Everything else goes on to edit the text as usual.
    pub(super) fn completion_key(&mut self, event: &KeyEvent) -> Option<Outcome> {
        if !self.chords.pending().is_empty() || self.focus != Focus::Editor {
            return None;
        }
        let mods = event.modifiers;
        let bare = mods.is_empty();
        if let KeyCode::Char(ch) = event.code
            && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.completion.typed = Some(ch);
        }

        if self.completion_showing() {
            let page = usize::from(MOST_SUGGESTIONS);
            let outcome = match event.code {
                KeyCode::Up if bare => self.completion_step(-1, true),
                KeyCode::Down if bare => self.completion_step(1, true),
                KeyCode::PageUp if bare => self.completion_step(-page.cast_signed(), false),
                KeyCode::PageDown if bare => self.completion_step(page.cast_signed(), false),
                KeyCode::Enter | KeyCode::Tab if bare => {
                    self.completion_accept_row(None, Mode::Insert)
                }
                KeyCode::Enter if mods == KeyModifiers::SHIFT => {
                    self.completion_accept_row(None, Mode::Replace)
                }
                KeyCode::Esc => {
                    self.close_completion();
                    Outcome::Redraw
                }
                _ => return None,
            };
            return Some(outcome);
        }
        // Nothing on screen yet, but asked: Escape takes the question back,
        // and then does what it always does.
        if event.code == KeyCode::Esc {
            self.close_completion();
        }

        if self.completion.stops.is_some() {
            match event.code {
                KeyCode::Tab if bare => return Some(self.next_stop(1)),
                KeyCode::BackTab => return Some(self.next_stop(-1)),
                KeyCode::Tab if mods == KeyModifiers::SHIFT => return Some(self.next_stop(-1)),
                KeyCode::Esc => self.end_snippet(),
                _ => {}
            }
        }
        None
    }

    /// Ask for completions here: the explicit command.
    pub(super) fn complete_here(&mut self) -> Outcome {
        self.focus = Focus::Editor;
        self.completion.wanted = Some(Trigger::Invoked);
        Outcome::Redraw
    }

    /// Whether the focused file's server can complete, for the right-click
    /// menu to offer it.
    pub(super) fn can_complete(&self) -> bool {
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.capabilities(self.doc().id))
            .is_some_and(|capabilities| capabilities.completion_provider.is_some())
    }

    /// Whether the popup has something on screen.
    fn completion_showing(&self) -> bool {
        self.completion.popup.as_ref().is_some_and(|popup| !popup.shown.is_empty())
    }

    /// Move the selection by `by` rows, wrapping round the ends when `wrap`.
    fn completion_step(&mut self, by: isize, wrap: bool) -> Outcome {
        let area = self.completion_area().map(|(area, _)| area);
        let Some(popup) = self.completion.popup.as_mut() else { return Outcome::Continue };
        let last = popup.shown.len().saturating_sub(1);
        let target = popup.selected.cast_signed() + by;
        popup.selected = if wrap && target < 0 {
            last
        } else if wrap && target.cast_unsigned() > last {
            0
        } else {
            target.clamp(0, last.cast_signed()).cast_unsigned()
        };
        let area = area.unwrap_or(Rect::new(0, 0, 1, MOST_SUGGESTIONS));
        popup.scroll = CompletionView::scroll_to(area, popup.selected, popup.scroll);
        Outcome::Redraw
    }

    // ── following along ─────────────────────────────────────────────────────

    /// After every event, once its edits have gone to the server: close what
    /// no longer applies, re-filter what does, and ask what needs asking.
    pub(super) fn completion_follow(&mut self) -> Outcome {
        let typed = self.completion.typed.take();
        let mut outcome = Outcome::Continue;

        if self.completion.stops.is_some() && !self.stops_hold() {
            // A click into another stop goes to it, as Tab would have; into
            // the final one, it is done.
            let count = self.completion.stops.as_ref().map_or(0, |stops| stops.stops.len());
            match self.stop_under_caret().filter(|index| index + 1 < count) {
                Some(index) => {
                    if let Some(stops) = self.completion.stops.as_mut() {
                        stops.current = index;
                    }
                    // The current stop's mark moves with it.
                    outcome = Outcome::Redraw;
                }
                None => self.end_snippet(),
            }
        }
        if std::mem::take(&mut self.completion.ended) {
            outcome = Outcome::Redraw;
        }

        if self.completion.popup.is_some() {
            match self.popup_query() {
                None => {
                    self.close_completion();
                    outcome = Outcome::Redraw;
                }
                Some(query) => {
                    let popup = self.completion.popup.as_mut().expect("checked just above");
                    if query != popup.query {
                        if popup.incomplete {
                            self.completion.wanted.get_or_insert(Trigger::Incomplete);
                        }
                        popup.refilter(query);
                        outcome = Outcome::Redraw;
                    }
                }
            }
        }

        if let Some(ch) = typed.filter(|ch| self.typed_here(*ch)) {
            if self.is_trigger(ch) {
                self.completion.wanted = Some(Trigger::Character(ch));
            } else if !is_word(ch) && self.completion.popup.is_some() {
                self.close_completion();
                outcome = Outcome::Redraw;
            }
        }

        if let Some(trigger) = self.completion.wanted.take() {
            outcome = outcome.and(self.ask_completion(trigger));
        }
        self.resolve_preview();
        outcome
    }

    /// Whether `ch` is what the key just typed, just before the caret.
    fn typed_here(&self, ch: char) -> bool {
        if self.focus != Focus::Editor || self.finder.is_some() || self.prompt.is_some() {
            return false;
        }
        let buffer = &self.doc().buffer;
        let head = buffer.selections().primary().head;
        head > 0 && buffer.rope().char(head - 1) == ch
    }

    /// Whether the focused file's server asks to be asked when `ch` is typed.
    fn is_trigger(&self, ch: char) -> bool {
        let Some(capabilities) = self.lsp.as_ref().and_then(|lsp| lsp.capabilities(self.doc().id))
        else {
            return false;
        };
        let mut text = [0u8; 4];
        let ch = &*ch.encode_utf8(&mut text);
        capabilities
            .completion_provider
            .as_ref()
            .and_then(|provider| provider.trigger_characters.as_ref())
            .is_some_and(|triggers| triggers.iter().any(|trigger| trigger.ends_with(ch)))
    }

    /// What has been typed of the word, while the only change since the popup
    /// opened has been typing or deleting at the caret; otherwise `None`.
    fn popup_query(&self) -> Option<String> {
        let popup = self.completion.popup.as_ref()?;
        if self.focus != Focus::Editor || self.finder.is_some() || self.prompt.is_some() {
            return None;
        }
        let document = self.doc();
        let buffer = &document.buffer;
        let selections = buffer.selections();
        let primary = selections.primary();
        if document.id != popup.doc
            || selections.len() != popup.carets
            || !primary.is_empty()
            || primary.head < popup.start
            || buffer.line_of(primary.head) != popup.line
        {
            return None;
        }
        let line_start = buffer.line_start(popup.line);
        let line_end = buffer.line_end(popup.line);
        let rope = buffer.rope();
        if popup.start < line_start
            || rope.slice(line_start..popup.start) != popup.before.as_str()
            || rope.slice(primary.head..line_end) != popup.after.as_str()
        {
            return None;
        }
        let query = rope.slice(popup.start..primary.head).to_string();
        // A space typed or pasted into it ends the word.
        (!query.contains(char::is_whitespace)).then_some(query)
    }

    /// Put the popup away, and take back what it asked.
    pub(super) fn close_completion(&mut self) {
        let Some(popup) = self.completion.popup.take() else { return };
        if let Some(lsp) = self.lsp.as_mut() {
            if let Some(pending) = popup.pending {
                lsp.cancel(pending.id);
            }
            if let Some((id, _)) = popup.resolving {
                lsp.cancel(id);
            }
        }
    }

    // ── asking ──────────────────────────────────────────────────────────────

    fn ask_completion(&mut self, trigger: Trigger) -> Outcome {
        let explicit = trigger == Trigger::Invoked;
        let say = |app: &mut Self, what: &str| {
            if explicit {
                app.message = Some(what.to_string());
            }
            Outcome::Redraw
        };
        if self.focus != Focus::Editor {
            return Outcome::Continue;
        }
        let document = self.doc();
        let doc = document.id;
        let rope = document.buffer.rope().clone();
        let head = document.buffer.selections().primary().head;
        let Some(lsp) = self.lsp.as_mut() else {
            return say(self, "This file has no language server to complete from.");
        };
        if lsp
            .capabilities(doc)
            .is_none_or(|capabilities| capabilities.completion_provider.is_none())
        {
            return say(self, "The language server offers no completions here.");
        }
        let (Some(position), Some(encoding)) =
            (lsp.position_params(doc, &rope, head), lsp.encoding(doc))
        else {
            return say(self, "The language server offers no completions here.");
        };
        let context = CompletionContext {
            trigger_kind: match trigger {
                Trigger::Invoked => CompletionTriggerKind::INVOKED,
                Trigger::Character(_) => CompletionTriggerKind::TRIGGER_CHARACTER,
                Trigger::Incomplete => CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
            },
            trigger_character: match trigger {
                Trigger::Character(ch) => Some(ch.to_string()),
                _ => None,
            },
        };
        let params = CompletionParams {
            text_document_position: position,
            work_done_progress_params: nun_lsp::types::WorkDoneProgressParams::default(),
            partial_result_params: nun_lsp::types::PartialResultParams::default(),
            context: Some(context),
        };
        let Ok(id) = lsp.request::<request::Completion>(doc, params) else {
            return say(self, "The language server is not ready yet.");
        };
        let pending = Pending {
            id,
            asked: Asked::new(rope, head, encoding),
            version: lsp.version(doc).unwrap_or(0),
        };

        // Asking again for the same word keeps what is on screen until the
        // answer comes; anything else starts a new popup.
        if trigger == Trigger::Incomplete
            && let Some(popup) = self.completion.popup.as_mut()
        {
            if let Some(old) = popup.pending.replace(pending) {
                lsp.cancel(old.id);
            }
            return Outcome::Continue;
        }
        self.close_completion();
        let start = match trigger {
            Trigger::Character(_) => head,
            _ => word_start(&self.doc().buffer, head),
        };
        self.open_popup(start, explicit, Some(pending));
        Outcome::Continue
    }

    /// Open the popup, empty until its answer comes, on the word the primary
    /// caret is typing that starts at `start`.
    fn open_popup(&mut self, start: usize, explicit: bool, pending: Option<Pending>) {
        let document = self.doc();
        let buffer = &document.buffer;
        let head = buffer.selections().primary().head;
        let line = buffer.line_of(head);
        let rope = buffer.rope();
        self.completion.popup = Some(Popup {
            doc: document.id,
            start,
            line,
            before: rope.slice(buffer.line_start(line)..start).to_string(),
            after: rope.slice(head..buffer.line_end(line)).to_string(),
            carets: buffer.selections().len(),
            explicit,
            pending,
            asked: None,
            items: Vec::new(),
            incomplete: false,
            resolved: Vec::new(),
            resolving: None,
            query: rope.slice(start..head).to_string(),
            shown: Vec::new(),
            selected: 0,
            scroll: 0,
        });
    }

    /// Whether `response` answers something completion asked.
    pub(super) fn completion_owns(&self, response: &Response) -> bool {
        self.completion.popup.as_ref().is_some_and(|popup| {
            popup.pending.as_ref().is_some_and(|pending| pending.id == response.id)
                || popup.resolving.is_some_and(|(id, _)| id == response.id)
        })
    }

    /// The server answered.
    pub(super) fn completion_answer(&mut self, response: &Response) -> Outcome {
        let focused = self.doc().id;
        let Some(popup) = self.completion.popup.as_mut() else { return Outcome::Continue };

        if let Some((_, index)) = popup.resolving.filter(|(id, _)| *id == response.id) {
            popup.resolving = None;
            if let Ok(resolved) = response.parse::<request::ResolveCompletionItem>()
                && let Some(item) = popup.items.get_mut(index)
            {
                merge_resolved(item, resolved);
                return Outcome::Redraw;
            }
            return Outcome::Continue;
        }

        let Some(pending) = popup.pending.take_if(|pending| pending.id == response.id) else {
            return Outcome::Continue;
        };
        // About another document, or another version of it than was asked
        // about: an answer to a question nobody is asking any more.
        if response.doc != popup.doc || focused != popup.doc || response.version != pending.version
        {
            return Outcome::Continue;
        }
        self.completion_items(response.parse::<request::Completion>(), pending.asked)
    }

    /// Take in the items of an answer to the question `asked`.
    fn completion_items(
        &mut self,
        answer: Result<Option<nun_lsp::types::CompletionResponse>, nun_lsp::Error>,
        asked: Asked,
    ) -> Outcome {
        let document = self.doc();
        let (rope, head) =
            (document.buffer.rope().clone(), document.buffer.selections().primary().head);
        let line_start = document.buffer.line_start(document.buffer.line_of(head));
        let Some(popup) = self.completion.popup.as_mut() else { return Outcome::Continue };
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                if popup.items.is_empty() {
                    let explicit = popup.explicit;
                    self.close_completion();
                    if explicit {
                        self.message = Some(format!("No completions: {error}."));
                    }
                    return Outcome::Redraw;
                }
                return Outcome::Continue;
            }
        };
        let (items, incomplete) = model::items_of(answer);
        if let Some((id, _)) = popup.resolving.take()
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
        popup.resolved = vec![false; items.len()];
        popup.items = items;
        popup.incomplete = incomplete;

        // Where the server says the word starts, when it says: a word with a
        // `$` or a `-` in it is still one word to a server that knows the
        // language.
        let range_start = popup
            .items
            .iter()
            .find_map(|item| model::insertion(item, Mode::Insert).range.map(|range| range.start));
        if let Some(position) = range_start {
            let start = asked.char_index(&rope, head, position);
            if (line_start..=head).contains(&start) && start != popup.start {
                popup.start = start;
                popup.before = rope.slice(line_start..start).to_string();
            }
        }
        popup.asked = Some(asked);
        let query = rope.slice(popup.start.min(head)..head).to_string();
        popup.refilter(query);
        popup.selected = popup
            .shown
            .iter()
            .position(|found| popup.items[found.index].preselect == Some(true))
            .unwrap_or(0);

        if popup.items.is_empty() && !popup.incomplete {
            let explicit = popup.explicit;
            self.close_completion();
            if explicit {
                self.message = Some("No completions here.".into());
            }
        }
        self.resolve_preview();
        Outcome::Redraw
    }

    /// Ask for the documentation of the item being looked at — the one under
    /// the pointer, or else the one selected — if it has not been asked for.
    fn resolve_preview(&mut self) {
        let hovered = match self.hover.current() {
            Some(Target::CompletionRow(row)) => Some(row),
            _ => None,
        };
        let Some(popup) = self.completion.popup.as_mut() else { return };
        let Some(index) =
            popup.shown.get(hovered.unwrap_or(popup.selected)).map(|found| found.index)
        else {
            return;
        };
        if popup.resolved.get(index).is_none_or(|resolved| *resolved) {
            return;
        }
        popup.resolved[index] = true;
        let Some(lsp) = self.lsp.as_mut() else { return };
        let resolves = lsp
            .capabilities(popup.doc)
            .and_then(|capabilities| capabilities.completion_provider.as_ref())
            .and_then(|provider| provider.resolve_provider)
            .unwrap_or(false);
        let item = &popup.items[index];
        if !resolves || (item.documentation.is_some() && item.detail.is_some()) {
            return;
        }
        if let Ok(id) = lsp.request::<request::ResolveCompletionItem>(popup.doc, item.clone())
            && let Some((old, _)) = popup.resolving.replace((id, index))
        {
            lsp.cancel(old);
        }
    }

    // ── accepting ───────────────────────────────────────────────────────────

    /// Accept the item on `row`, or the selected one.
    fn completion_accept_row(&mut self, row: Option<usize>, mode: Mode) -> Outcome {
        let Some(mut popup) = self.completion.popup.take() else { return Outcome::Continue };
        if let Some(lsp) = self.lsp.as_mut() {
            if let Some(pending) = popup.pending.take() {
                lsp.cancel(pending.id);
            }
            if let Some((id, _)) = popup.resolving.take() {
                lsp.cancel(id);
            }
        }
        let row = row.unwrap_or(popup.selected);
        let (Some(item), Some(asked)) = (popup.item(row), popup.asked.as_ref()) else {
            return Outcome::Redraw;
        };
        let insertion = model::insertion(item, mode);
        self.insert_completion(&popup, asked, &insertion);
        self.follow_caret();
        Outcome::Redraw
    }

    /// Put `insertion` into the text at every caret it belongs at.
    fn insert_completion(&mut self, popup: &Popup, asked: &Asked, insertion: &model::Insertion) {
        let document = self.doc();
        let doc = document.id;
        let buffer = &document.buffer;
        let rope = buffer.rope().clone();
        let head = buffer.selections().primary().head;
        let at = |position| asked.char_index(&rope, head, position);

        let (from, to) = insertion
            .range
            .map(|range| (at(range.start), at(range.end)))
            .filter(|(from, to)| from <= &head && &head <= to)
            .unwrap_or((popup.start.min(head), head));
        let (kept, primary) = copies(buffer, (from, head, to));
        // Each copy its own, indented like its own line.
        let snippets: Vec<Snippet> = kept
            .iter()
            .map(|(start, _)| snippet_for(buffer, start + (head - from), *start, insertion))
            .collect();
        let additional: Vec<Edit> = insertion
            .additional
            .iter()
            .map(|edit| {
                let (start, end) = (at(edit.range.start), at(edit.range.end));
                Edit::replace(start.min(end), end.max(start), edit.new_text.clone())
            })
            .collect();
        let texts: Vec<&str> = snippets.iter().map(|snippet| snippet.text.as_str()).collect();
        let (edits, bases) = plan(&kept, &texts, additional);
        let others: Vec<Span<usize>> = buffer
            .selections()
            .ranges()
            .iter()
            .filter(|range| !kept.iter().any(|copy| touches(*copy, (range.from(), range.to()))))
            .map(|range| shifted(&edits, range.anchor)..shifted(&edits, range.head))
            .collect();

        // One undo step of its own, never run together with the typing
        // before it or after.
        if let Err(error) = self.doc_mut().buffer.apply_batch(edits) {
            self.message = Some(format!("Could not complete: {error}."));
            return;
        }
        // Sent now, so the tab-stops only ever follow edits made after them.
        self.lsp_flush();

        let primary_snippet = &snippets[primary];
        let stops: Vec<Vec<(usize, Span<usize>)>> = (0..primary_snippet.stops.len())
            .map(|index| {
                bases
                    .iter()
                    .zip(&snippets)
                    .enumerate()
                    .flat_map(|(copy, (base, snippet))| {
                        let ranges = snippet.stops.get(index).map_or(&[][..], |stop| &stop.ranges);
                        ranges.iter().map(move |span| (copy, base + span.start..base + span.end))
                    })
                    .collect()
            })
            .collect();
        let len = self.doc().buffer.len_chars();
        let last = stops.len() - 1;
        self.completion.stops = Some(Tabstops { doc, stops, primary, others, current: 0, len });
        self.select_stop(if primary_snippet.has_stops() { 0 } else { last });
    }

    // ── tab-stops ───────────────────────────────────────────────────────────

    /// Select every copy of stop `index`. The last stop is where the snippet
    /// ends, so reaching it ends the tab-stops.
    fn select_stop(&mut self, index: usize) {
        let Some(stops) = self.completion.stops.as_mut() else { return };
        let Some(stop) = stops.stops.get(index) else { return };
        stops.current = index;
        let primary = stop.iter().position(|(copy, _)| *copy == stops.primary).unwrap_or(0);
        let ranges: Vec<Range> = stop
            .iter()
            .map(|(_, span)| span)
            .chain(&stops.others)
            .map(|span| Range::new(span.start, span.end))
            .collect();
        if index + 1 >= stops.stops.len() {
            self.end_snippet();
        }
        if !ranges.is_empty() {
            self.doc_mut().buffer.set_selections(Selections::new(ranges, primary));
        }
    }

    /// End the tab-stops: Tab types a tab again, and their marks come off.
    fn end_snippet(&mut self) {
        self.completion.ended |= self.completion.stops.take().is_some();
    }

    /// Where the live snippet's stops are in `doc`, for drawing, and which
    /// of them is being edited. None when there is no snippet in it, or when
    /// an edit went unseen and the stops no longer describe the text.
    pub(super) fn snippet_stops(&self, doc: &Document) -> Vec<nun_ui::Stop> {
        let Some(stops) = self.completion.stops.as_ref() else { return Vec::new() };
        if stops.doc != doc.id || stops.len != doc.buffer.len_chars() {
            return Vec::new();
        }
        stops
            .stops
            .iter()
            .enumerate()
            .flat_map(|(index, stop)| {
                stop.iter().map(move |(_, span)| nun_ui::Stop {
                    start: span.start,
                    end: span.end,
                    current: index == stops.current,
                })
            })
            .collect()
    }

    /// Tab or Shift+Tab: the next stop, or the one before.
    fn next_stop(&mut self, by: isize) -> Outcome {
        let Some(stops) = self.completion.stops.as_ref() else { return Outcome::Continue };
        let last = stops.stops.len() - 1;
        let index = stops.current.saturating_add_signed(by).min(last);
        self.select_stop(index);
        self.follow_caret();
        Outcome::Redraw
    }

    /// Whether the tab-stops still describe the text, and the caret is still
    /// in the current stop.
    fn stops_hold(&self) -> bool {
        let current = self.completion.stops.as_ref().map(|stops| stops.current);
        current.is_some() && self.stops_trusted() && self.stop_contains(current, self.caret())
    }

    /// Whether the tab-stops can be trusted at all: the same document, with
    /// every edit since followed.
    fn stops_trusted(&self) -> bool {
        let Some(stops) = self.completion.stops.as_ref() else { return false };
        let document = self.doc();
        self.focus == Focus::Editor
            && document.id == stops.doc
            && document.buffer.len_chars() == stops.len
    }

    /// The primary caret.
    fn caret(&self) -> usize {
        self.doc().buffer.selections().primary().head
    }

    /// Whether stop `index` covers `at` in any of its places.
    fn stop_contains(&self, index: Option<usize>, at: usize) -> bool {
        let stop =
            self.completion.stops.as_ref().zip(index).and_then(|(stops, i)| stops.stops.get(i));
        stop.is_some_and(|stop| stop.iter().any(|(_, span)| span.start <= at && at <= span.end))
    }

    /// The stop the primary caret is in, when the stops can be trusted and
    /// it is in one.
    fn stop_under_caret(&self) -> Option<usize> {
        if !self.stops_trusted() {
            return None;
        }
        let count = self.completion.stops.as_ref()?.stops.len();
        let caret = self.caret();
        (0..count).find(|index| self.stop_contains(Some(*index), caret))
    }

    // ── the popup on screen ─────────────────────────────────────────────────

    /// Where the popup goes, and whether it opened above the word.
    fn completion_area(&self) -> Option<(Rect, bool)> {
        let popup = self.completion.popup.as_ref().filter(|popup| !popup.shown.is_empty())?;
        let document = self.doc();
        if document.id != popup.doc {
            return None;
        }
        let (text, _) = self.areas();
        let (x, y) = EditorView::new(&document.buffer, &self.palette)
            .scrolled_to(document.scroll)
            .cell_of(text, popup.start)?;
        let bounds = self.completion_bounds();
        let width = CompletionView::width(&Self::suggestions(popup, 0, 200));
        let area = CompletionView::area(bounds, x, y, popup.shown.len(), width);
        (area.height > 0).then_some((area, area.y < y))
    }

    /// The room the popup and its documentation have: everything above the
    /// status line.
    fn completion_bounds(&self) -> Rect {
        Rect { height: self.viewport.height.saturating_sub(1), ..self.viewport }
    }

    /// The rows from `from`, at most `count` of them, as the widget draws them.
    fn suggestions(popup: &Popup, from: usize, count: usize) -> Vec<Suggestion> {
        popup
            .shown
            .iter()
            .skip(from)
            .take(count)
            .map(|found| {
                let item = &popup.items[found.index];
                let (kind, kind_role) = kind_of(item.kind);
                let detail = item
                    .label_details
                    .as_ref()
                    .and_then(|details| {
                        details.description.clone().or_else(|| details.detail.clone())
                    })
                    .or_else(|| item.detail.clone())
                    .unwrap_or_default();
                let deprecated = item.deprecated == Some(true)
                    || item
                        .tags
                        .as_ref()
                        .is_some_and(|tags| tags.contains(&CompletionItemTag::DEPRECATED));
                Suggestion {
                    kind,
                    kind_role,
                    label: item.label.clone(),
                    matched: found.matched.clone(),
                    detail: detail.lines().next().unwrap_or_default().to_string(),
                    deprecated,
                }
            })
            .collect()
    }

    /// The documentation shown: for the row under the pointer, or else the
    /// selected one.
    fn completion_docs(&self) -> Option<String> {
        let popup = self.completion.popup.as_ref()?;
        let row = match self.hover.current() {
            Some(Target::CompletionRow(row)) => row,
            _ => popup.selected,
        };
        Some(docs_of(popup.item(row)?)).filter(|docs| !docs.trim().is_empty())
    }

    fn docs_area(&self, popup: Rect, above: bool) -> Option<(Rect, String)> {
        let docs = self.completion_docs()?;
        Some((DocsView::area(self.completion_bounds(), popup, above, &docs)?, docs))
    }

    /// Lay out the popup's rows, and the documentation beside it.
    pub(super) fn layout_completion(&self, hits: &mut nun_input::HitMap<Target>) {
        let Some((area, above)) = self.completion_area() else { return };
        let Some(popup) = self.completion.popup.as_ref() else { return };
        if let Some((docs, _)) = self.docs_area(area, above) {
            hits.push(super::cells(docs), Target::Completion, false);
        }
        hits.push(super::cells(area), Target::Completion, false);
        let rows = (popup.scroll..popup.shown.len()).take(usize::from(area.height));
        for (offset, row) in rows.enumerate() {
            let Ok(offset) = u16::try_from(offset) else { break };
            let cells = Rect { y: area.y + offset, height: 1, ..area };
            hits.push(super::cells(cells), Target::CompletionRow(row), true);
        }
    }

    /// Draw the popup and the documentation beside it.
    pub(super) fn render_completion(&self, cells: &mut Cells) {
        let Some((area, above)) = self.completion_area() else { return };
        let Some(popup) = self.completion.popup.as_ref() else { return };
        let rows = Self::suggestions(popup, popup.scroll, usize::from(area.height));
        let hovered = match self.hover.current() {
            Some(Target::CompletionRow(row)) => row.checked_sub(popup.scroll),
            _ => None,
        };
        CompletionView::new(&self.palette, &rows)
            .selected(popup.selected.wrapping_sub(popup.scroll))
            .hovered(hovered)
            .render(area, cells);
        if let Some((docs, text)) = self.docs_area(area, above) {
            DocsView::new(&self.palette, &text).render(docs, cells);
        }
    }

    /// A click on the popup: a row is accepted, over the rest of the word
    /// too when Shift is held, as with Shift+Enter.
    pub(super) fn completion_click(&mut self, target: Target, replace: bool) -> Outcome {
        let mode = if replace { Mode::Replace } else { Mode::Insert };
        match target {
            Target::CompletionRow(row) => self.completion_accept_row(Some(row), mode),
            _ => Outcome::Continue,
        }
    }

    /// The wheel over the popup scrolls it, leaving the selection alone.
    pub(super) fn completion_scroll(&mut self, down: bool) -> Outcome {
        let height = self.completion_area().map_or(0, |(area, _)| usize::from(area.height));
        let Some(popup) = self.completion.popup.as_mut() else { return Outcome::Continue };
        let most = popup.shown.len().saturating_sub(height);
        popup.scroll =
            if down { (popup.scroll + 3).min(most) } else { popup.scroll.saturating_sub(3) };
        Outcome::Redraw
    }
}

/// What `insertion` puts in at a caret at `head` in `buffer`, whose word
/// starts at `from`: its snippet, or its text as one.
fn snippet_for(buffer: &Buffer, head: usize, from: usize, insertion: &model::Insertion) -> Snippet {
    // The rest of the lines take the indentation of this one.
    let source = if insertion.indent && insertion.text.contains('\n') {
        let line = buffer.line_text(buffer.line_of(head));
        let indent: String = line.chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect();
        insertion.text.replace('\n', &format!("\n{indent}"))
    } else {
        insertion.text.clone()
    };
    if !insertion.snippet {
        return Snippet::plain(&source);
    }
    let known = variables(buffer, head, &buffer.rope().slice(from..head).to_string());
    let lookup =
        |name: &str| known.iter().find(|(known, _)| *known == name).map(|(_, value)| value.clone());
    snippet::parse(&source, &lookup)
}

/// Where the completion goes: `from..to` around the primary caret at `head`,
/// and the same span at every other caret with the same characters before
/// it. The spans in order, disjoint, and which of them is the primary's.
fn copies(
    buffer: &Buffer,
    (from, head, to): (usize, usize, usize),
) -> (Vec<(usize, usize)>, usize) {
    let rope = buffer.rope();
    let selections = buffer.selections();
    let (before, after) = (head - from, to - head);
    let typed = rope.slice(from..head);
    let tail = rope.slice(head..to);
    let mut copies: Vec<(usize, usize)> = Vec::new();
    let mut primary = 0;
    for (index, range) in selections.ranges().iter().enumerate() {
        if index == selections.primary_index() {
            primary = copies.len();
            copies.push((from, to));
            continue;
        }
        let caret = range.head;
        if !range.is_empty() || caret < before || rope.slice(caret - before..caret) != typed {
            continue;
        }
        let end = caret + after;
        let end =
            if end <= rope.len_chars() && rope.slice(caret..end) == tail { end } else { caret };
        copies.push((caret - before, end));
    }
    // Two carets close enough that their words overlap: the primary's wins,
    // and a neighbour that would collide is left alone.
    let mut kept: Vec<(usize, usize)> = Vec::with_capacity(copies.len());
    let mut kept_primary = 0;
    for (index, copy) in copies.iter().copied().enumerate() {
        let collides = copies.iter().enumerate().any(|(other, span)| {
            other != index && overlaps(*span, copy) && (other == primary || other < index)
        });
        if index == primary {
            kept_primary = kept.len();
            kept.push(copy);
        } else if !collides {
            kept.push(copy);
        }
    }
    (kept, kept_primary)
}

/// The edits that put each of `texts` over its span in `copies`, with `additional`
/// where it touches none of them or each other; and where each copy starts
/// once they have all gone in.
fn plan(
    copies: &[(usize, usize)],
    texts: &[&str],
    additional: Vec<Edit>,
) -> (Vec<Edit>, Vec<usize>) {
    let mut elsewhere: Vec<Edit> = Vec::new();
    for edit in additional {
        let span = (edit.start, edit.end);
        if copies.iter().any(|copy| touches(*copy, span)) {
            continue;
        }
        // Two inserts at one place: one after the other, as the server
        // listed them.
        if let Some(same) = elsewhere.iter_mut().find(|other| {
            other.start == edit.start && other.end == edit.end && edit.start == edit.end
        }) {
            same.text.push_str(&edit.text);
            continue;
        }
        if elsewhere.iter().any(|other| touches((other.start, other.end), span)) {
            continue;
        }
        elsewhere.push(edit);
    }

    let mut edits: Vec<(Option<usize>, Edit)> = copies
        .iter()
        .enumerate()
        .map(|(copy, (start, end))| (Some(copy), Edit::replace(*start, *end, texts[copy])))
        .chain(elsewhere.into_iter().map(|edit| (None, edit)))
        .collect();
    edits.sort_by_key(|(_, edit)| edit.start);
    let mut bases = vec![0; copies.len()];
    let mut shift = 0isize;
    for (copy, edit) in &edits {
        if let Some(copy) = copy {
            bases[*copy] = edit.start.saturating_add_signed(shift);
        }
        shift += edit.inserted().cast_signed() - edit.removed().cast_signed();
    }
    (edits.into_iter().map(|(_, edit)| edit).collect(), bases)
}

/// Where `pos` is once `edits` — sorted, disjoint, in the text before any of
/// them — have all gone in. A position inside one ends up after what it put
/// there.
fn shifted(edits: &[Edit], pos: usize) -> usize {
    let mut shift = 0isize;
    for edit in edits {
        if edit.start > pos {
            break;
        }
        if pos <= edit.end && edit.start < edit.end {
            return edit.end_after().saturating_add_signed(shift);
        }
        shift += edit.inserted().cast_signed() - edit.removed().cast_signed();
    }
    pos.saturating_add_signed(shift)
}

/// Fold what a resolve answered into the item: the documentation and detail
/// it was asked for, and any edits elsewhere the item did not have yet.
fn merge_resolved(item: &mut CompletionItem, resolved: CompletionItem) {
    if resolved.documentation.is_some() {
        item.documentation = resolved.documentation;
    }
    if resolved.detail.is_some() {
        item.detail = resolved.detail;
    }
    if item.additional_text_edits.is_none() {
        item.additional_text_edits = resolved.additional_text_edits;
    }
}

/// Whether two char spans share any char.
const fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Whether two char spans share a char or an edge — too close for two edits
/// in one revision, where which went first would change the result.
const fn touches(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

#[cfg(test)]
mod tests {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use nun_lsp::Encoding;
    use nun_lsp::types::{
        CompletionList, CompletionResponse, CompletionTextEdit, InsertReplaceEdit,
        InsertTextFormat, Position, TextEdit,
    };
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};

    use super::*;

    /// An editor over `text` with the caret at `caret`.
    fn editor(text: &str, caret: usize) -> App {
        let mut buffer = Buffer::from_text(text);
        buffer.set_selections(Selections::single(Range::caret(caret)));
        // As for a document a server follows, which is the only kind that
        // gets completions.
        buffer.keep_edits(true);
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 12));
        app
    }

    fn item(label: &str) -> CompletionItem {
        CompletionItem { label: label.to_string(), ..CompletionItem::default() }
    }

    const fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn edit(from: Position, to: Position, text: &str) -> CompletionTextEdit {
        CompletionTextEdit::Edit(TextEdit {
            range: nun_lsp::types::Range::new(from, to),
            new_text: text.to_string(),
        })
    }

    /// Ask as the explicit command would, as of the text now, and have the
    /// server answer `items`.
    fn ask(app: &mut App, items: Vec<CompletionItem>) {
        ask_incomplete(app, items, false);
    }

    fn ask_incomplete(app: &mut App, items: Vec<CompletionItem>, incomplete: bool) {
        let asked = question(app);
        let start = word_start(&app.doc().buffer, asked.head());
        app.open_popup(start, true, None);
        answer(app, asked, items, incomplete);
    }

    /// The question as of the text now.
    fn question(app: &App) -> Asked {
        let buffer = &app.doc().buffer;
        Asked::new(buffer.rope().clone(), buffer.selections().primary().head, Encoding::Utf16)
    }

    fn answer(app: &mut App, asked: Asked, items: Vec<CompletionItem>, incomplete: bool) {
        let list = CompletionResponse::List(CompletionList { is_incomplete: incomplete, items });
        app.completion_items(Ok(Some(list)), asked);
        app.relayout();
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    fn text(app: &App) -> String {
        app.doc().buffer.text().to_string()
    }

    fn shown(app: &App) -> Vec<String> {
        let Some(popup) = app.completion.popup.as_ref() else { return Vec::new() };
        popup.shown.iter().map(|found| popup.items[found.index].label.clone()).collect()
    }

    fn selections(app: &App) -> Vec<(usize, usize)> {
        let ranges = app.doc().buffer.selections().ranges().to_vec();
        ranges.iter().map(|range| (range.anchor, range.head)).collect()
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    }

    #[test]
    fn typing_on_filters_locally_and_edits_the_text() {
        let mut app = editor("let x = pr", 10);
        ask(&mut app, vec![item("print"), item("println"), item("process"), item("eprintln")]);
        assert_eq!(shown(&app), ["print", "println", "process", "eprintln"]);

        type_text(&mut app, "ln");
        assert_eq!(text(&app), "let x = prln", "every key still types");
        assert_eq!(shown(&app), ["println", "eprintln"]);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(shown(&app), ["print", "println", "process", "eprintln"], "and back");
    }

    #[test]
    fn nothing_on_screen_takes_no_keys() {
        // Asked, and the server has not answered: the keys are the text's.
        let mut app = editor("ab", 2);
        app.open_popup(0, true, None);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "ab\n", "Enter typed a newline");
        assert!(app.completion.popup.is_none(), "and a new line ended the word");
    }

    #[test]
    fn moving_the_caret_closes_it() {
        let mut app = editor("foo bar", 3);
        ask(&mut app, vec![item("food")]);
        assert!(app.completion_showing());
        press(&mut app, KeyCode::Left);
        assert!(app.completion.popup.is_none());
    }

    #[test]
    fn a_character_that_ends_the_word_closes_it() {
        let mut app = editor("fo", 2);
        ask(&mut app, vec![item("foo")]);
        type_text(&mut app, "(");
        assert!(app.completion.popup.is_none());
        assert_eq!(text(&app), "fo(");
    }

    #[test]
    fn escape_puts_it_away_and_leaves_the_text() {
        let mut app = editor("fo", 2);
        ask(&mut app, vec![item("foo")]);
        press(&mut app, KeyCode::Esc);
        assert!(app.completion.popup.is_none());
        assert_eq!(text(&app), "fo");
    }

    #[test]
    fn enter_accepts_the_selected_item_over_the_word() {
        let mut app = editor("x = pri;", 7);
        ask(&mut app, vec![item("print"), item("println")]);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "x = println;");
        assert_eq!(selections(&app), [(11, 11)]);
        assert!(app.completion.popup.is_none());
    }

    #[test]
    fn up_from_the_top_wraps_to_the_bottom() {
        let mut app = editor("p", 1);
        ask(&mut app, vec![item("pa"), item("pb"), item("pc")]);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), "pc");
    }

    #[test]
    fn the_insert_range_keeps_the_rest_of_the_word_and_the_replace_range_takes_it() {
        let both = |item: &mut CompletionItem| {
            item.text_edit = Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                new_text: "println".into(),
                insert: nun_lsp::types::Range::new(at(0, 0), at(0, 3)),
                replace: nun_lsp::types::Range::new(at(0, 0), at(0, 5)),
            }));
        };
        let mut println = item("println");
        both(&mut println);

        let mut app = editor("prixx", 3);
        ask(&mut app, vec![println.clone()]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "printlnxx");

        let mut app = editor("prixx", 3);
        ask(&mut app, vec![println]);
        app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert_eq!(text(&app), "println");
    }

    #[test]
    fn an_answer_to_an_earlier_prefix_lands_where_the_word_is_now() {
        // Asked at `pr|`; two more letters typed before the answer came.
        let mut app = editor("x pr", 4);
        let asked = question(&app);
        app.open_popup(2, false, None);
        type_text(&mut app, "in");
        let mut print = item("println");
        print.text_edit = Some(edit(at(0, 2), at(0, 4), "println"));
        answer(&mut app, asked, vec![print, item("process")], false);
        assert_eq!(shown(&app), ["println"], "filtered by what is typed now");
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "x println");
    }

    #[test]
    fn an_incomplete_list_is_asked_for_again_as_the_word_changes() {
        let mut app = editor("pr", 2);
        ask_incomplete(&mut app, vec![item("print")], true);
        // No server to ask: the question is noted, not sent, and the list
        // stays up in the meantime.
        type_text(&mut app, "i");
        assert_eq!(shown(&app), ["print"]);
        assert!(app.completion.popup.as_ref().is_some_and(|popup| popup.incomplete));
    }

    #[test]
    fn edits_elsewhere_go_in_with_it_as_one_undo_step() {
        let mut app = editor("fn main() {\n    Has\n}\n", 19);
        let mut map = item("HashMap");
        map.additional_text_edits = Some(vec![TextEdit {
            range: nun_lsp::types::Range::new(at(0, 0), at(0, 0)),
            new_text: "use std::collections::HashMap;\r\n\r\n".into(),
        }]);
        ask(&mut app, vec![map]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            text(&app),
            "use std::collections::HashMap;\n\nfn main() {\n    HashMap\n}\n",
            "the server's CRLF is the buffer's LF"
        );
        let head = app.doc().buffer.selections().primary().head;
        assert_eq!(app.doc().buffer.line_of(head), 3, "the caret after the word, not the import");
        app.handle(Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)));
        assert_eq!(text(&app), "fn main() {\n    Has\n}\n", "one undo takes both back");
    }

    #[test]
    fn a_snippet_puts_the_caret_on_its_first_stop_and_tab_walks_them() {
        let mut app = editor("fo", 2);
        let mut snippet = item("for");
        snippet.insert_text = Some("for ${1:item} in ${2:iter} {\n\t$0\n}".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "for item in iter {\n\t\n}");
        assert_eq!(selections(&app), [(4, 8)], "the first placeholder, selected");

        type_text(&mut app, "x");
        assert_eq!(text(&app), "for x in iter {\n\t\n}");
        press(&mut app, KeyCode::Tab);
        assert_eq!(selections(&app), [(9, 13)], "the second stop moved with the typing");
        app.handle(Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert_eq!(selections(&app), [(4, 5)], "back to the first, as typed");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selections(&app), [(17, 17)], "the end: $0");
        assert!(app.completion.stops.is_none(), "and that is the end of it");
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), "for x in iter {\n\t\t\n}", "Tab types a tab again");
    }

    #[test]
    fn a_snippet_is_indented_like_its_line() {
        let mut app = editor("    fo", 6);
        let mut snippet = item("for");
        snippet.insert_text = Some("for {\n\t$0\n}".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "    for {\n    \t\n    }");
        assert_eq!(selections(&app), [(15, 15)]);
    }

    #[test]
    fn a_mirrored_stop_is_edited_in_both_places() {
        let mut app = editor("le", 2);
        let mut snippet = item("let");
        snippet.insert_text = Some("let ${1:x} = $1;".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "yz");
        assert_eq!(text(&app), "let yz = yz;");
    }

    #[test]
    fn moving_out_of_a_stop_ends_the_snippet() {
        let mut app = editor("f", 1);
        let mut snippet = item("fn");
        snippet.insert_text = Some("fn ${1:name}() $0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Home);
        assert!(app.completion.stops.is_none());
        press(&mut app, KeyCode::Tab);
        assert!(text(&app).starts_with('\t'), "{:?}", text(&app));
    }

    #[test]
    fn every_caret_typing_the_same_word_gets_the_completion_and_its_stops() {
        let mut buffer = Buffer::from_text("pr\npr\nzz");
        buffer.set_selections(Selections::new(
            vec![Range::caret(2), Range::caret(5), Range::caret(8)],
            0,
        ));
        buffer.keep_edits(true);
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 12));
        let mut snippet = item("println!");
        snippet.insert_text = Some("println!(\"${1}\")$0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "println!(\"\")\nprintln!(\"\")\nzz", "not after `zz`");
        assert_eq!(selections(&app), [(10, 10), (23, 23), (28, 28)]);
        type_text(&mut app, "hi");
        assert_eq!(text(&app), "println!(\"hi\")\nprintln!(\"hi\")\nzzhi");
        press(&mut app, KeyCode::Tab);
        assert_eq!(selections(&app)[..2], [(14, 14), (29, 29)]);
    }

    #[test]
    fn a_click_accepts_a_row_and_the_wheel_scrolls() {
        let mut app = editor("p", 1);
        let items: Vec<CompletionItem> = (0..30).map(|n| item(&format!("p{n:02}"))).collect();
        ask(&mut app, items);
        let (area, above) = app.completion_area().unwrap();
        assert!(!above);
        assert_eq!(area.y, 1, "under the word");

        app.handle(mouse(MouseEventKind::ScrollDown, area.x + 2, area.y));
        assert_eq!(app.completion.popup.as_ref().unwrap().scroll, 3);
        assert_eq!(app.completion.popup.as_ref().unwrap().selected, 0, "the wheel is not a pick");
        assert_eq!(text(&app), "p", "and it did not scroll the text");

        app.handle(mouse(MouseEventKind::Down(MouseButton::Left), area.x + 8, area.y + 1));
        assert_eq!(text(&app), "p04");
    }

    #[test]
    fn hovering_a_row_previews_its_documentation() {
        let mut app = editor("p", 1);
        let mut first = item("panic");
        first.documentation = Some(Documentation::String("Panics.".into()));
        let mut second = item("print");
        second.documentation = Some(Documentation::String("Prints.".into()));
        ask(&mut app, vec![first, second]);
        assert_eq!(app.completion_docs().as_deref(), Some("Panics."), "the selected one");

        let (area, _) = app.completion_area().unwrap();
        app.handle(mouse(MouseEventKind::Moved, area.x + 8, area.y + 1));
        assert_eq!(app.completion_docs().as_deref(), Some("Prints."), "the hovered one");
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        let drawn: String = (0..app.viewport.width).map(|x| cells[(x, area.y)].symbol()).collect();
        assert!(drawn.contains("Prints."), "{drawn}");
        assert_eq!(text(&app), "p", "hovering changes nothing");
    }

    #[test]
    fn near_the_bottom_it_opens_above_the_word() {
        let mut app = editor(&"\n".repeat(10), 10);
        type_text(&mut app, "p");
        let items: Vec<CompletionItem> = (0..8).map(|n| item(&format!("p{n}"))).collect();
        ask(&mut app, items);
        let (area, above) = app.completion_area().unwrap();
        assert!(above);
        assert_eq!(area.bottom(), 10, "ending on the row above the word");
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        let row: String = (0..20).map(|x| cells[(x, area.y)].symbol()).collect();
        assert!(row.contains("p0"), "{row}");
    }

    #[test]
    fn with_no_language_server_the_command_says_so() {
        let mut app = editor("x", 1);
        app.handle(Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
        assert_eq!(app.message(), Some("This file has no language server to complete from."));
        assert_eq!(text(&app), "x", "and typed nothing");
    }

    #[test]
    fn a_stop_that_is_not_being_typed_in_keeps_out_of_the_way() {
        // `$1$2`: typing in the first pushes the second along.
        let typed = Edit::insert(4, "x");
        assert_eq!(map_span(&(4..4), &typed, true), 4..5);
        assert_eq!(map_span(&(4..4), &typed, false), 5..5);
        // A stop that ends where the typing starts stays before it.
        assert_eq!(map_span(&(2..4), &typed, false), 2..4);
        // One further on moves.
        assert_eq!(map_span(&(6..9), &Edit::replace(1, 3, "abc"), false), 7..10);
    }

    #[test]
    fn a_word_is_its_letters_digits_and_underscores_whatever_the_script() {
        let buffer = Buffer::from_text("x.größe_1");
        assert_eq!(word_start(&buffer, 9), 2);
        let buffer = Buffer::from_text("a 日本");
        assert_eq!(word_start(&buffer, 4), 2);
    }

    #[test]
    fn a_decomposed_accent_is_part_of_the_word() {
        let mut app = editor("cafe\u{301}", 5);
        ask(&mut app, vec![item("cafe\u{301}_au_lait")]);
        assert_eq!(app.completion.popup.as_ref().unwrap().start, 0);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "cafe\u{301}_au_lait", "the word replaced, not doubled");
        assert_eq!(word_start(&Buffer::from_text("x नमस्ते"), 8), 2, "a virama joins, too");
    }

    #[test]
    fn each_caret_s_copy_is_indented_like_its_own_line() {
        let mut buffer = Buffer::from_text("    pr\npr");
        buffer.set_selections(Selections::new(vec![Range::caret(6), Range::caret(9)], 0));
        buffer.keep_edits(true);
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        let mut snippet = item("proc");
        snippet.insert_text = Some("{\n\t$1\n}$0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "    {\n    \t\n    }\n{\n\t\n}");
        assert_eq!(selections(&app), [(11, 11), (21, 21)]);
    }

    #[test]
    fn a_click_into_another_stop_goes_to_it() {
        let mut app = editor("f", 1);
        let mut snippet = item("fn");
        snippet.insert_text = Some("fn ${1:name}(${2:args}) $0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "fn name(args) ");
        // Into `args`, with the mouse: gutter of three, then column 9.
        app.handle(mouse(MouseEventKind::Down(MouseButton::Left), 3 + 9, 0));
        app.handle(mouse(MouseEventKind::Up(MouseButton::Left), 3 + 9, 0));
        assert_eq!(app.completion.stops.as_ref().map(|stops| stops.current), Some(1));
        app.handle(Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert_eq!(selections(&app), [(3, 7)], "and Shift+Tab goes back from there");
    }

    /// The marks drawn for the live snippet: where, and whether current.
    fn marks(app: &App) -> Vec<(usize, usize, bool)> {
        let stops = app.snippet_stops(app.doc());
        stops.iter().map(|stop| (stop.start, stop.end, stop.current)).collect()
    }

    fn fn_snippet(app: &mut App) {
        let mut snippet = item("fn");
        snippet.insert_text = Some("fn ${1:name}(${2:args}) $0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(app, vec![snippet]);
        press(app, KeyCode::Enter);
    }

    #[test]
    fn every_stop_is_marked_and_the_marks_follow_the_typing() {
        let mut app = editor("f", 1);
        fn_snippet(&mut app);
        assert_eq!(marks(&app), [(3, 7, true), (8, 12, false), (14, 14, false)]);
        type_text(&mut app, "x");
        assert_eq!(marks(&app), [(3, 4, true), (5, 9, false), (11, 11, false)], "{}", text(&app));
        press(&mut app, KeyCode::Tab);
        assert_eq!(marks(&app), [(3, 4, false), (5, 9, true), (11, 11, false)]);
        let outcome = app.handle(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert_eq!(outcome, Outcome::Redraw);
        assert_eq!(marks(&app), [], "the end of the snippet takes its marks with it");
    }

    #[test]
    fn a_click_into_another_stop_moves_the_strong_mark_to_it() {
        let mut app = editor("f", 1);
        fn_snippet(&mut app);
        app.handle(mouse(MouseEventKind::Down(MouseButton::Left), 3 + 9, 0));
        app.handle(mouse(MouseEventKind::Up(MouseButton::Left), 3 + 9, 0));
        assert_eq!(marks(&app), [(3, 7, false), (8, 12, true), (14, 14, false)]);
    }

    #[test]
    fn escape_or_moving_out_takes_the_marks_off() {
        let mut app = editor("f", 1);
        fn_snippet(&mut app);
        let outcome = app.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(outcome, Outcome::Redraw, "the marks have to come off the screen");
        assert_eq!(marks(&app), []);

        let mut app = editor("f", 1);
        fn_snippet(&mut app);
        press(&mut app, KeyCode::Home);
        assert_eq!(marks(&app), []);
    }

    #[test]
    fn a_mirror_shares_the_current_mark() {
        let mut app = editor("le", 2);
        let mut snippet = item("let");
        snippet.insert_text = Some("let ${1:x} = $1;$0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "yz");
        assert_eq!(marks(&app), [(4, 6, true), (9, 11, true), (12, 12, false)]);
    }

    #[test]
    fn every_carets_copy_is_marked() {
        let mut buffer = Buffer::from_text("pr\npr");
        buffer.set_selections(Selections::new(vec![Range::caret(2), Range::caret(5)], 0));
        buffer.keep_edits(true);
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 12));
        let mut snippet = item("println!");
        snippet.insert_text = Some("println!(\"${1}\")$0".into());
        snippet.insert_text_format = Some(InsertTextFormat::SNIPPET);
        ask(&mut app, vec![snippet]);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "hi");
        assert_eq!(text(&app), "println!(\"hi\")\nprintln!(\"hi\")");
        assert_eq!(marks(&app), [(10, 12, true), (25, 27, true), (14, 14, false), (29, 29, false)]);
    }

    #[test]
    fn the_marks_are_on_screen_while_the_snippet_is_live_and_gone_after() {
        let mut app = editor("f", 1);
        fn_snippet(&mut app);
        type_text(&mut app, "x");
        let area = Rect::new(0, 0, 80, 12);
        let palette = Palette::new(derive(&Probe::builtin_dark()));
        let draw = |app: &App| {
            let mut cells = Cells::empty(area);
            app.render(area, &mut cells);
            // `a` of `args`: a gutter of three, then column 5.
            cells[(3 + 5, 0)].bg
        };
        assert_eq!(Some(draw(&app)), palette.tabstop(false).bg, "the next stop, marked");
        press(&mut app, KeyCode::Esc);
        assert_ne!(Some(draw(&app)), palette.tabstop(false).bg, "and not once it has ended");
    }

    #[test]
    fn shift_click_takes_the_whole_word() {
        let mut println = item("println");
        println.text_edit = Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
            new_text: "println".into(),
            insert: nun_lsp::types::Range::new(at(0, 0), at(0, 3)),
            replace: nun_lsp::types::Range::new(at(0, 0), at(0, 5)),
        }));
        let mut app = editor("prixx", 3);
        ask(&mut app, vec![println]);
        let (area, _) = app.completion_area().unwrap();
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 8,
            row: area.y,
            modifiers: KeyModifiers::SHIFT,
        }));
        assert_eq!(text(&app), "println");
    }

    #[test]
    fn an_empty_stop_where_a_placeholder_is_typed_over_stays_before_it() {
        // `$2${1:name}`: typing `x` over `name` leaves stop 2 in front.
        assert_eq!(map_span(&(0..0), &Edit::replace(0, 4, "x"), false), 0..0);
    }
}
