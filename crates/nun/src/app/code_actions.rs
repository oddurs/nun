//! Code actions: the quick fixes on a diagnostic's card, and everything a
//! server offers at the caret, in a chooser.
//!
//! **On the card.** A card about a diagnostic asks the server, as it opens,
//! what would fix what it is about (`textDocument/codeAction`, with those
//! diagnostics as the context). The card does not wait: it shows the message
//! at once, and the fixes join it as buttons when the answer comes, if the
//! card is still up and still about the same text. A card that closes first
//! cancels the question. The answer is kept, so resting on the same underline
//! again shows its fixes straight away, as long as the text has not changed.
//! Two fixes fit beside the card's own buttons; when there are more, a third
//! button opens them all in the chooser.
//!
//! **In the chooser.** "Code actions" (Ctrl+., or Ctrl+K . where a terminal
//! cannot report Ctrl+.; the right-click menu; the command palette) asks for
//! everything at the caret or over the selection, fixes for any diagnostic
//! there first, and lists them in the palette — the same fixes the card
//! offers, reached from the keyboard.
//!
//! **Choosing one.** A server may send an action without its edit and fill
//! it in only when it is chosen (`codeAction/resolve`), and nun asks for it
//! then. The edit goes through `workspace_edit`, which decides whether it goes
//! straight in — one open file, one undo step — or is previewed first. An
//! action may also name a command, which the server carries out once the
//! edit is in (`workspace/executeCommand`); a server doing so usually sends
//! its own edit back (`workspace/applyEdit`), which goes the same way, and the
//! server is told how that went once it has gone. An action chosen after the
//! text has changed is not made: its positions describe text that is no
//! longer there.
//!
//! **In the gutter.** Once the caret has settled on a line, the server is
//! asked what it offers anywhere on that line, and if it offers anything a
//! mark goes beside it; clicking the mark opens those offers in the chooser.
//! The question waits for the caret to rest, is cancelled when it moves on,
//! and is not asked again while the line, its text and its diagnostics stay
//! as they were. An edit never asks it: typing clears the mark and leaves it
//! off until the caret goes to another line or the line's diagnostics
//! change, so a burst of typing costs the server nothing.

use std::time::{Duration, Instant};

use nun_lsp::types::request::{CodeActionRequest, CodeActionResolveRequest, ExecuteCommand};
use nun_lsp::types::{
    CodeAction, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionTriggerKind, ExecuteCommandParams, PartialResultParams,
    WorkDoneProgressParams,
};
use nun_lsp::{EditRequest, RequestId, Response};
use nun_ui::{Glyph, PaletteEntry};
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::card::Anchor;
use super::palette::{Pick, Row};
use super::panes::DocId;
use super::workspace_edit::{After, Subject};
use super::{App, Outcome};

/// How many fixes the card shows as buttons before it offers the rest in the
/// chooser.
const MOST_ON_CARD: usize = 2;

/// How wide a fix's button may be before its title is cut short.
const MOST_BUTTON_WIDTH: usize = 30;

/// How long a server has to carry out a command. Long, because it may be
/// waiting on its own edit, which may be waiting on the person in the
/// preview.
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the caret rests on a line before the gutter's question is asked.
/// Long enough that holding an arrow key down asks nothing on the way.
const BULB_SETTLE: Duration = Duration::from_millis(300);

/// What the servers have offered, and what has been asked of them.
#[derive(Debug, Default)]
pub(super) struct CodeActions {
    /// The question asked for the card, and what the card is about.
    card_asked: Option<(RequestId, Anchor)>,
    /// The fixes for the last card: what it was about, and them.
    card: Option<(Anchor, Vec<Offered>)>,
    /// The question asked for the chooser.
    chooser_asked: Option<RequestId>,
    /// What the chooser offers, while it is open.
    chosen: Vec<Offered>,
    /// An action whose edit has been asked for, as it was before.
    resolving: Option<(RequestId, Offered)>,
    /// Commands a server is carrying out.
    executing: Vec<(RequestId, Executing)>,
    /// Whether to mark the caret's line when it has actions.
    lightbulb_off: bool,
    /// The caret's line, and what has been asked or said about it.
    bulb: Option<Bulb>,
}

/// The line the gutter's question is about, as it stood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    doc: DocId,
    line: usize,
    version: i32,
    /// How many diagnostics touch it: a server's fixes are for those, so
    /// when they change the answer may too.
    diagnostics: usize,
}

#[derive(Debug)]
struct Bulb {
    place: Place,
    state: BulbState,
}

impl Bulb {
    /// Whether the server offered something on the line that can be chosen.
    fn lit(&self) -> bool {
        matches!(&self.state, BulbState::Answered(offered)
            if offered.iter().any(|offer| offer.disabled().is_none()))
    }
}

#[derive(Debug)]
enum BulbState {
    /// Changed by an edit: nothing asked until the caret goes to another
    /// line or the line's diagnostics change.
    Quiet,
    /// Waiting until this for the caret to rest.
    Settling(Instant),
    Asking(RequestId),
    /// What the server offers on the line.
    Answered(Vec<Offered>),
}

/// A command a server is carrying out.
#[derive(Debug)]
struct Executing {
    /// What it is called.
    title: String,
    /// The document it was chosen in, and the version it was chosen at.
    doc: DocId,
    version: Option<i32>,
}

impl CodeActions {
    /// Whether `id` is a question of code actions'.
    pub(super) fn owns(&self, id: RequestId) -> bool {
        self.card_asked.is_some_and(|(asked, _)| asked == id)
            || self.chooser_asked == Some(id)
            || self.resolving.as_ref().is_some_and(|(asked, _)| *asked == id)
            || self.executing.iter().any(|(asked, _)| *asked == id)
            || self
                .bulb
                .as_ref()
                .is_some_and(|bulb| matches!(bulb.state, BulbState::Asking(asked) if asked == id))
    }
}

/// One action on offer, and the text it was offered for.
#[derive(Debug, Clone, PartialEq)]
struct Offered {
    doc: DocId,
    /// The version of the document it was worked out against.
    version: i32,
    action: CodeActionOrCommand,
}

impl Offered {
    fn title(&self) -> &str {
        match &self.action {
            CodeActionOrCommand::Command(command) => &command.title,
            CodeActionOrCommand::CodeAction(action) => &action.title,
        }
    }

    fn kind(&self) -> Option<&CodeActionKind> {
        match &self.action {
            CodeActionOrCommand::Command(_) => None,
            CodeActionOrCommand::CodeAction(action) => action.kind.as_ref(),
        }
    }

    /// Why it cannot be chosen, when the server says it cannot.
    fn disabled(&self) -> Option<&str> {
        match &self.action {
            CodeActionOrCommand::CodeAction(CodeAction { disabled: Some(disabled), .. }) => {
                Some(&disabled.reason)
            }
            _ => None,
        }
    }

    /// Whether it fixes something: a quick fix, or an action with no kind
    /// that names the diagnostics it is for.
    fn is_fix(&self) -> bool {
        match &self.action {
            CodeActionOrCommand::Command(_) => false,
            CodeActionOrCommand::CodeAction(action) => match &action.kind {
                Some(kind) => is_quickfix(kind),
                None => action.diagnostics.as_ref().is_some_and(|d| !d.is_empty()),
            },
        }
    }

    /// Where it goes in a list: preferred fixes, fixes, then the rest, each
    /// in the server's order.
    fn rank(&self) -> u8 {
        let preferred = matches!(
            &self.action,
            CodeActionOrCommand::CodeAction(CodeAction { is_preferred: Some(true), .. })
        );
        match (self.is_fix(), preferred) {
            (true, true) => 0,
            (true, false) => 1,
            (false, _) => 2,
        }
    }
}

fn is_quickfix(kind: &CodeActionKind) -> bool {
    let kind = kind.as_str();
    kind == "quickfix" || kind.starts_with("quickfix.")
}

/// How a kind reads beside an action in the chooser.
fn kind_hint(kind: Option<&CodeActionKind>) -> &'static str {
    let Some(kind) = kind.map(CodeActionKind::as_str) else { return "" };
    let family = kind.split('.').next().unwrap_or(kind);
    match family {
        "quickfix" => "fix",
        "refactor" => "refactor",
        "source" => "source",
        _ => "",
    }
}

/// A title as it can be drawn: a server's line break or tab would move the
/// terminal's cursor rather than take a cell, so each is a space.
fn printable(title: &str) -> String {
    title.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect()
}

/// How many columns `text` takes as it is drawn, a grapheme at a time.
fn drawn_width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// `title`, cut short between graphemes to fit on a button, ending in
/// `ellipsis` when it was.
fn button_label(title: &str, ellipsis: &str) -> String {
    let title = printable(title);
    if drawn_width(&title) <= MOST_BUTTON_WIDTH {
        return title;
    }
    let mut label = String::new();
    let mut used = 0;
    for grapheme in title.graphemes(true) {
        let wide = grapheme.width();
        if used + wide > MOST_BUTTON_WIDTH.saturating_sub(ellipsis.width()) {
            break;
        }
        label.push_str(grapheme);
        used += wide;
    }
    label.push_str(ellipsis);
    label
}

impl App {
    /// Whether the file being edited has a server that offers code actions.
    pub(super) fn can_code_action(&self) -> bool {
        self.resolves(self.doc().id).is_some()
    }

    /// Whether `doc`'s server offers code actions, and if it does, whether
    /// it fills their edits in when asked.
    fn resolves(&self, doc: DocId) -> Option<bool> {
        let capabilities = self.lsp.as_ref()?.capabilities(doc)?;
        match capabilities.code_action_provider.as_ref()? {
            CodeActionProviderCapability::Simple(false) => None,
            CodeActionProviderCapability::Simple(true) => Some(false),
            CodeActionProviderCapability::Options(options) => {
                Some(options.resolve_provider == Some(true))
            }
        }
    }

    /// The question for chars `from..to` of `doc`: that range, and every
    /// diagnostic touching it as it stands now, as the server said it.
    fn code_action_params(
        &self,
        doc: DocId,
        from: usize,
        to: usize,
        only_fixes: bool,
        trigger: CodeActionTriggerKind,
    ) -> Option<CodeActionParams> {
        let lsp = self.lsp.as_ref()?;
        let rope = self.doc_by(doc)?.buffer.rope();
        // A lone carriage return is a line break to the server and not here,
        // so every position after one would name the wrong place — and no
        // edit to such a file is made anyway.
        if rope.chars().any(|ch| ch == '\r') {
            return None;
        }
        let encoding = lsp.encoding(doc)?;
        let marks = self.diagnostics.marks(doc);
        let notes = self.diagnostics.notes(doc);
        let diagnostics = marks
            .iter()
            .zip(notes)
            .filter(|(mark, _)| mark.start <= to && from <= mark.end)
            .map(|(mark, note)| {
                // Where it is now: the text has usually moved on since the
                // server said it, and the question is about the text as it is.
                let mut diagnostic = note.diagnostic.clone();
                let end = mark.end.min(rope.len_chars());
                diagnostic.range = encoding.range(rope, mark.start.min(end)..end);
                diagnostic
            })
            .collect();
        Some(CodeActionParams {
            text_document: lsp.identifier(doc)?,
            range: encoding.range(rope, from..to),
            context: CodeActionContext {
                diagnostics,
                only: only_fixes.then(|| vec![CodeActionKind::QUICKFIX]),
                trigger_kind: Some(trigger),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
    }

    // ── on the card ─────────────────────────────────────────────────────────

    /// A diagnostic's card has just opened: ask what would fix it, or show
    /// what was offered for it already.
    pub(super) fn ask_fixes(&mut self) {
        let Some(anchor) = self.card.as_ref().map(super::card::Card::anchor) else { return };
        let Anchor::Text { doc, from, to, .. } = anchor else { return };
        if self.code_actions.card_asked.is_some_and(|(_, asked)| asked == anchor) {
            return;
        }
        self.forget_card_fixes();
        let version = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if let Some((about, offered)) = &self.code_actions.card
            && *about == anchor
            && offered.first().is_some_and(|first| Some(first.version) == version)
        {
            self.show_fixes(anchor);
            return;
        }
        if self.resolves(doc).is_none() {
            return;
        }
        let Some(params) =
            self.code_action_params(doc, from, to, false, CodeActionTriggerKind::INVOKED)
        else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else { return };
        if let Ok(id) = lsp.request::<CodeActionRequest>(doc, params) {
            self.code_actions.card_asked = Some((id, anchor));
        }
    }

    /// The card has gone: its question is no longer worth an answer.
    pub(super) fn forget_card_fixes(&mut self) {
        if let Some((id, _)) = self.code_actions.card_asked.take()
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
    }

    /// Put the fixes offered for `anchor` on the card, if it is still about
    /// that.
    fn show_fixes(&mut self, anchor: Anchor) {
        let Some((about, offered)) = &self.code_actions.card else { return };
        if *about != anchor || offered.is_empty() {
            return;
        }
        let ellipsis = self.palette.glyph(Glyph::Ellipsis);
        let mut labels: Vec<String> = offered
            .iter()
            .take(MOST_ON_CARD)
            .map(|offer| button_label(offer.title(), ellipsis))
            .collect();
        if offered.len() > MOST_ON_CARD {
            labels.push(format!("{} more…", offered.len() - MOST_ON_CARD));
        }
        if let Some(card) = self.card.as_mut().filter(|card| card.anchor() == anchor) {
            card.offer_fixes(labels);
        }
    }

    /// A fix button on the card was pressed: the fix, or, past the ones
    /// shown, all of them in the chooser.
    pub(super) fn card_fix(&mut self, index: usize) -> Outcome {
        let Some((_, offered)) = &self.code_actions.card else { return Outcome::Redraw };
        if index < MOST_ON_CARD.min(offered.len()) {
            let offer = offered[index].clone();
            return self.choose(offer);
        }
        let offered = offered.clone();
        self.offer_in_chooser(offered)
    }

    // ── in the chooser ──────────────────────────────────────────────────────

    /// Ask for every code action at the caret, or over the selection, to
    /// choose from.
    pub(super) fn code_actions_here(&mut self) -> Outcome {
        let doc = self.doc().id;
        let range = self.doc().buffer.selections().primary();
        let (from, to) = (range.from(), range.to());
        if self.resolves(doc).is_none() {
            self.message = Some(
                if self.lsp.as_ref().and_then(|lsp| lsp.capabilities(doc)).is_none() {
                    "No code actions: this file's language server is not ready."
                } else {
                    "This file's language server offers no code actions."
                }
                .into(),
            );
            return Outcome::Redraw;
        }
        let Some(params) =
            self.code_action_params(doc, from, to, false, CodeActionTriggerKind::INVOKED)
        else {
            return Outcome::Continue;
        };
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        if let Some(id) = self.code_actions.chooser_asked.take() {
            lsp.cancel(id);
        }
        match lsp.request::<CodeActionRequest>(doc, params) {
            Ok(id) => {
                self.code_actions.chooser_asked = Some(id);
                self.message = Some("Asking for code actions…".into());
            }
            Err(error) => self.message = Some(format!("No code actions: {error}.")),
        }
        Outcome::Redraw
    }

    /// Open the chooser on `offered`.
    fn offer_in_chooser(&mut self, offered: Vec<Offered>) -> Outcome {
        let rows = offered
            .iter()
            .enumerate()
            .map(|(index, offer)| {
                let (hint, pick) = match offer.disabled() {
                    Some(why) => (why.to_string(), Pick::Nothing),
                    None => (kind_hint(offer.kind()).to_string(), Pick::Action(index)),
                };
                Row {
                    entry: PaletteEntry {
                        label: printable(offer.title()),
                        matched: Vec::new(),
                        hint,
                    },
                    pick,
                }
            })
            .collect();
        let count = offered.len();
        self.code_actions.chosen = offered;
        let what =
            if count == 1 { "1 code action".to_string() } else { format!("{count} code actions") };
        self.message = None;
        self.open_choices(format!("{what} — pick one"), rows)
    }

    /// Row `index` of the chooser was picked.
    pub(super) fn pick_code_action(&mut self, index: usize) -> Outcome {
        let chosen = std::mem::take(&mut self.code_actions.chosen);
        match chosen.into_iter().nth(index) {
            Some(offer) => self.choose(offer),
            None => Outcome::Redraw,
        }
    }

    // ── the answers ─────────────────────────────────────────────────────────

    /// A server answered one of the questions asked here.
    pub(super) fn code_action_answer(&mut self, response: &Response) -> Outcome {
        let id = response.id;
        if let Some((_, anchor)) = self.code_actions.card_asked.filter(|(asked, _)| *asked == id) {
            self.code_actions.card_asked = None;
            return self.card_answered(anchor, response);
        }
        if self.code_actions.chooser_asked == Some(id) {
            self.code_actions.chooser_asked = None;
            return self.chooser_answered(response);
        }
        if self.code_actions.resolving.as_ref().is_some_and(|(asked, _)| *asked == id)
            && let Some((_, offer)) = self.code_actions.resolving.take()
        {
            return self.action_resolved(offer, response);
        }
        if self
            .code_actions
            .bulb
            .as_ref()
            .is_some_and(|bulb| matches!(bulb.state, BulbState::Asking(asked) if asked == id))
        {
            return self.bulb_answered(response);
        }
        if let Some(at) = self.code_actions.executing.iter().position(|(asked, _)| *asked == id) {
            let (_, executing) = self.code_actions.executing.remove(at);
            if let Err(error) = &response.result {
                self.message = Some(format!("“{}” did not work: {error}.", executing.title));
                return Outcome::Redraw;
            }
        }
        Outcome::Continue
    }

    /// The actions a server offered, in the order they are shown, or why
    /// there are none.
    fn offered(&self, response: &Response) -> Result<Vec<Offered>, String> {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(response.doc));
        if current != Some(response.version) {
            return Err("the file changed while the server was asked".into());
        }
        let answer = response.parse::<CodeActionRequest>().map_err(|error| error.to_string())?;
        let mut offered: Vec<Offered> = answer
            .unwrap_or_default()
            .into_iter()
            .map(|action| Offered { doc: response.doc, version: response.version, action })
            .collect();
        offered.sort_by_key(Offered::rank);
        Ok(offered)
    }

    fn card_answered(&mut self, anchor: Anchor, response: &Response) -> Outcome {
        let Ok(offered) = self.offered(response) else { return Outcome::Continue };
        let fixes: Vec<Offered> = offered
            .into_iter()
            .filter(|offer| offer.is_fix() && offer.disabled().is_none())
            .collect();
        self.code_actions.card = Some((anchor, fixes));
        self.show_fixes(anchor);
        Outcome::Redraw
    }

    fn chooser_answered(&mut self, response: &Response) -> Outcome {
        match self.offered(response) {
            Ok(offered) if offered.is_empty() => {
                self.message = Some("No code actions here.".into());
                Outcome::Redraw
            }
            Ok(offered) => self.offer_in_chooser(offered),
            Err(why) => {
                self.message = Some(format!("No code actions: {why}. Try again."));
                Outcome::Redraw
            }
        }
    }

    // ── in the gutter ───────────────────────────────────────────────────────

    /// Mark the caret's line when it has code actions, or not.
    pub fn set_lightbulb(&mut self, on: bool) {
        self.code_actions.lightbulb_off = !on;
        if !on {
            self.forget_bulb();
        }
    }

    /// The line the caret is on in the focused document, as it stands, if
    /// its server can be asked about it.
    fn caret_line(&self) -> Option<Place> {
        if self.code_actions.lightbulb_off {
            return None;
        }
        let doc = self.doc();
        self.resolves(doc.id)?;
        let version = self.lsp.as_ref()?.version(doc.id)?;
        let line = doc.buffer.line_of(doc.buffer.selections().primary().head);
        let (from, to) = (doc.buffer.line_start(line), doc.buffer.line_end(line));
        let diagnostics = self
            .diagnostics
            .marks(doc.id)
            .iter()
            .filter(|mark| mark.start <= to && from <= mark.end)
            .count();
        Some(Place { doc: doc.id, line, version, diagnostics })
    }

    /// After anything that happened: if the caret is on another line, or
    /// its line changed, what was said about the old one no longer holds.
    /// Moved or newly diagnosed, the line is asked about once the caret has
    /// rested; edited, it is not asked about at all.
    pub(super) fn bulb_follow(&mut self, now: Instant) -> Outcome {
        let place = self.caret_line();
        let was = self.code_actions.bulb.as_ref().map(|bulb| bulb.place);
        if place == was {
            return Outcome::Continue;
        }
        let shown = self.code_actions.bulb.as_ref().is_some_and(Bulb::lit);
        self.forget_bulb();
        if let Some(place) = place {
            let edited =
                was.is_some_and(|was| was.doc == place.doc && was.version != place.version);
            let state =
                if edited { BulbState::Quiet } else { BulbState::Settling(now + BULB_SETTLE) };
            self.code_actions.bulb = Some(Bulb { place, state });
        }
        if shown { Outcome::Redraw } else { Outcome::Continue }
    }

    /// Stop waiting, and cancel the question if it has been asked.
    fn forget_bulb(&mut self) {
        let Some(bulb) = self.code_actions.bulb.take() else { return };
        if let BulbState::Asking(id) = bulb.state
            && let Some(lsp) = self.lsp.as_mut()
        {
            lsp.cancel(id);
        }
    }

    /// When the caret will have rested, if it is resting.
    pub(super) fn bulb_deadline(&self) -> Option<Instant> {
        match self.code_actions.bulb.as_ref()?.state {
            BulbState::Settling(due) => Some(due),
            _ => None,
        }
    }

    /// The caret has rested: ask what the server offers on its line.
    pub(super) fn bulb_tick(&mut self, now: Instant) -> Outcome {
        let Some(bulb) = self.code_actions.bulb.as_ref() else { return Outcome::Continue };
        let BulbState::Settling(due) = bulb.state else { return Outcome::Continue };
        if now < due {
            return Outcome::Continue;
        }
        let Place { doc, line, .. } = bulb.place;
        let buffer = &self.doc().buffer;
        if doc != self.doc().id || line >= buffer.len_lines() {
            self.forget_bulb();
            return Outcome::Continue;
        }
        let (from, to) = (buffer.line_start(line), buffer.line_end(line));
        let params =
            self.code_action_params(doc, from, to, false, CodeActionTriggerKind::AUTOMATIC);
        let asked = params.and_then(|params| {
            self.lsp.as_mut().and_then(|lsp| lsp.request::<CodeActionRequest>(doc, params).ok())
        });
        if let Some(bulb) = self.code_actions.bulb.as_mut() {
            bulb.state = asked.map_or(BulbState::Answered(Vec::new()), BulbState::Asking);
        }
        Outcome::Continue
    }

    fn bulb_answered(&mut self, response: &Response) -> Outcome {
        let offered = self.offered(response).unwrap_or_default();
        let Some(bulb) = self.code_actions.bulb.as_mut() else { return Outcome::Continue };
        bulb.state = BulbState::Answered(offered);
        if self.lightbulb_line().is_some() { Outcome::Redraw } else { Outcome::Continue }
    }

    /// The caret's line and what is on offer there, while that is still
    /// the line asked about, as it was, and anything there can be chosen.
    fn bulb_offers(&self) -> Option<(usize, &[Offered])> {
        let bulb = self.code_actions.bulb.as_ref().filter(|bulb| bulb.lit())?;
        let BulbState::Answered(offered) = &bulb.state else { return None };
        let doc = self.doc();
        let still = bulb.place.doc == doc.id
            && bulb.place.line == doc.buffer.line_of(doc.buffer.selections().primary().head)
            && self.lsp.as_ref().and_then(|lsp| lsp.version(doc.id)) == Some(bulb.place.version);
        still.then_some((bulb.place.line, offered))
    }

    /// The focused document's line to mark as having code actions.
    pub(super) fn lightbulb_line(&self) -> Option<usize> {
        self.bulb_offers().map(|(line, _)| line)
    }

    /// The row of `text`, the focused pane's text area, that the mark is
    /// on, when it is on one.
    pub(super) fn lightbulb_row(&self, text: Rect) -> Option<u16> {
        let line = self.lightbulb_line()?;
        let doc = self.doc();
        let row = doc
            .buffer
            .hidden()
            .from(doc.scroll)
            .take(usize::from(text.height))
            .position(|l| l == line)?;
        Some(text.y + u16::try_from(row).ok()?)
    }

    /// The mark was clicked: what it stands for, in the chooser — or, if it
    /// no longer stands for anything, the question the chooser asks.
    pub(super) fn lightbulb_click(&mut self) -> Outcome {
        match self.bulb_offers() {
            Some((_, offered)) => {
                let offered = offered.to_vec();
                self.offer_in_chooser(offered)
            }
            None => self.code_actions_here(),
        }
    }

    // ── carrying one out ────────────────────────────────────────────────────

    /// Carry out an action the person chose.
    fn choose(&mut self, offer: Offered) -> Outcome {
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(offer.doc));
        if current != Some(offer.version) {
            self.message = Some(format!(
                "Did not apply “{}”: the file has changed since it was offered. Ask again.",
                offer.title()
            ));
            return Outcome::Redraw;
        }
        if let Some(why) = offer.disabled() {
            self.message = Some(format!("“{}” cannot be applied: {why}.", offer.title()));
            return Outcome::Redraw;
        }
        let doc = offer.doc;
        let action = match offer.action {
            CodeActionOrCommand::Command(command) => {
                self.execute_command(doc, command);
                return Outcome::Redraw;
            }
            CodeActionOrCommand::CodeAction(action) => action,
        };
        // Left for later is how a server that resolves says so: it has
        // nothing to resolve the action from but what it put in `data`.
        if action.edit.is_none() && action.data.is_some() && self.resolves(doc) == Some(true) {
            return self
                .resolve(Offered { action: CodeActionOrCommand::CodeAction(action), ..offer });
        }
        self.carry_out(doc, action)
    }

    /// Ask the server to fill in the action's edit.
    fn resolve(&mut self, offer: Offered) -> Outcome {
        let CodeActionOrCommand::CodeAction(action) = &offer.action else {
            return Outcome::Continue;
        };
        let title = action.title.clone();
        let action = action.clone();
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        if let Some((id, _)) = self.code_actions.resolving.take() {
            lsp.cancel(id);
        }
        match lsp.request::<CodeActionResolveRequest>(offer.doc, action) {
            Ok(id) => {
                self.code_actions.resolving = Some((id, offer));
                self.message = Some(format!("Working out “{title}”…"));
            }
            Err(error) => self.message = Some(format!("Did not apply “{title}”: {error}.")),
        }
        Outcome::Redraw
    }

    /// The server filled an action in — or could not, in which case what it
    /// offered in the first place is all there is to go on.
    fn action_resolved(&mut self, offer: Offered, response: &Response) -> Outcome {
        let Offered { doc, version, action } = offer;
        let CodeActionOrCommand::CodeAction(offered) = action else { return Outcome::Continue };
        let current = self.lsp.as_ref().and_then(|lsp| lsp.version(doc));
        if current != Some(version) || response.version != version {
            self.message = Some(format!(
                "Did not apply “{}”: the file changed while it was worked out. Ask again.",
                offered.title
            ));
            return Outcome::Redraw;
        }
        match response.parse::<CodeActionResolveRequest>() {
            Ok(action) => self.carry_out(doc, action),
            Err(_) if offered.command.is_some() => self.carry_out(doc, offered),
            Err(error) => {
                self.message = Some(format!("Did not apply “{}”: {error}.", offered.title));
                Outcome::Redraw
            }
        }
    }

    /// Make an action's edit, then run its command: the order the protocol
    /// gives them in.
    fn carry_out(&mut self, doc: DocId, action: CodeAction) -> Outcome {
        let CodeAction { title, edit, command, .. } = action;
        let then = command.map(|command| (doc, command));
        let Some(edit) = edit else {
            match then {
                Some((doc, command)) => self.execute_command(doc, command),
                None => self.message = Some(format!("“{title}” has nothing to do.")),
            }
            return Outcome::Redraw;
        };
        let Some(encoding) = self.lsp.as_ref().and_then(|lsp| lsp.encoding(doc)) else {
            self.message = Some(format!("Did not apply “{title}”: the language server stopped."));
            return Outcome::Redraw;
        };
        let after = After { reply: None, then };
        self.edit_workspace(Subject::Action { title }, encoding, &edit, true, after)
    }

    /// Have `doc`'s server carry out `command`. What it does usually comes
    /// back as an edit it asks for, which [`App::server_edit`] takes.
    pub(super) fn execute_command(&mut self, doc: DocId, command: nun_lsp::types::Command) {
        let nun_lsp::types::Command { title, command, arguments } = command;
        let params = ExecuteCommandParams {
            command,
            arguments: arguments.unwrap_or_default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let Some(lsp) = self.lsp.as_mut() else { return };
        let version = lsp.version(doc);
        match lsp.request_within::<ExecuteCommand>(doc, params, EXECUTE_TIMEOUT) {
            Ok(id) => {
                self.code_actions.executing.push((id, Executing { title, doc, version }));
            }
            Err(error) => self.message = Some(format!("Could not run “{title}”: {error}.")),
        }
    }

    /// A server asked for an edit to be made. It is made the way a code
    /// action's is, and the server is answered once it has been, or has not.
    ///
    /// It goes straight in only while it is plainly the edit of a command
    /// the person just chose, in text that has not changed since: an edit a
    /// server sends of its own accord, or one whose text has moved on while
    /// the command ran, is previewed. An edit without versions cannot say
    /// which text it was worked out for, and typing meanwhile would put it in
    /// the wrong place.
    pub(super) fn server_edit(&mut self, request: EditRequest) -> Outcome {
        // Most often it is the command just run that asks, and its title
        // says what the edit is better than nothing does.
        let running = self.code_actions.executing.last().map(|(_, running)| running);
        let straight = running.is_some_and(|running| {
            let now = self.lsp.as_ref().and_then(|lsp| lsp.version(running.doc));
            running.version.is_some() && now == running.version
        });
        let title = request
            .label
            .clone()
            .or_else(|| running.map(|running| running.title.clone()))
            .unwrap_or_else(|| "the language server's edit".into());
        let encoding = request.encoding;
        let edit = request.edit.clone();
        let after = After { reply: Some(request), then: None };
        self.edit_workspace(Subject::Action { title }, encoding, &edit, straight, after)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
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
    use crate::app::Target;
    use crate::commands::Command;

    /// A language server in `sh`. It answers `initialize` with the
    /// capabilities in `$1`, `textDocument/codeAction` with `$2` after `$5`
    /// seconds, and `codeAction/resolve` with `$3`; it publishes `$4` as the
    /// diagnostics of every file opened; and asked to execute a command, it
    /// asks for `$6` to be applied, as a server carrying one out does. Every
    /// message it reads, the answers to its own requests included, goes on a
    /// line of `$7`.
    const SERVER: &str = r#"
caps="$1"; actions="$2"; resolved="$3"; diag="$4"; delay="$5"; apply="$6"; log="$7"
send() { printf 'Content-Length: %s\r\n\r\n%s' "${#1}" "$1"; }
while :; do
  len=
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
  done
  [ -z "$len" ] && exit 0
  body=$(dd bs=1 count="$len" 2>/dev/null)
  printf '%s\n' "$body" >> "$log"
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$body" in
    *'"method":"initialize"'*) result="{\"capabilities\":$caps}" ;;
    *'"method":"textDocument/didOpen"'*)
      uri=$(printf '%s' "$body" | sed -n 's/.*"uri":"\([^"]*\)".*/\1/p')
      send "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{\"uri\":\"$uri\",\"diagnostics\":[$diag]}}"
      continue ;;
    *'"method":"textDocument/codeAction"'*) sleep "$delay"; result="$actions" ;;
    *'"method":"codeAction/resolve"'*) result="$resolved" ;;
    *'"method":"workspace/executeCommand"'*)
      send "{\"jsonrpc\":\"2.0\",\"id\":\"e1\",\"method\":\"workspace/applyEdit\",\"params\":$apply}"
      result=null ;;
    *'"method":"shutdown"'*) result=null ;;
    *'"method":"exit"'*) exit 0 ;;
    *) continue ;;
  esac
  send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
done
"#;

    const CAPS: &str = r#"{"textDocumentSync":1,"codeActionProvider":{"resolveProvider":true},"executeCommandProvider":{"commands":["fix.it"]}}"#;

    /// `x` is unused, in `main.rs`.
    const MAIN: &str = "fn main() { let x = 1; }\n";
    const DIAG: &str = r#"{"range":{"start":{"line":0,"character":16},"end":{"line":0,"character":17}},"severity":2,"message":"unused variable","code":"unused_variables"}"#;

    fn edit(line: u32, from: u32, to: u32, text: &str) -> String {
        format!(
            r#"{{"range":{{"start":{{"line":{line},"character":{from}}},"end":{{"line":{line},"character":{to}}}}},"newText":"{text}"}}"#
        )
    }

    /// What the server says a code action is, with `rest` added.
    fn action(title: &str, kind: &str, rest: &str) -> String {
        format!(r#"{{"title":"{title}","kind":"{kind}"{rest}}}"#)
    }

    /// `x` to `_x` in `main.rs`.
    fn underscore() -> String {
        format!(r#","edit":{{"changes":{{"{{main.rs}}":[{}]}}}}"#, edit(0, 16, 17, "_x"))
    }

    /// What the server is told to say, and what `main.rs` holds; an empty
    /// field means the usual.
    #[derive(Default)]
    struct Says<'a> {
        main: &'a str,
        diag: &'a str,
        actions: &'a str,
        resolved: &'a str,
        delay: &'a str,
        apply: &'a str,
        /// Whether the caret's line is marked; off unless a test is about
        /// it, so its questions are not in the others' way.
        lightbulb: bool,
    }

    struct Tester {
        app: App,
        lsp: Receiver<nun_lsp::Event>,
        done: Receiver<Done>,
        dir: tempfile::TempDir,
        log: tempfile::NamedTempFile,
    }

    impl Tester {
        /// An editor on `main.rs` and `other.rs` in a folder, with the server
        /// above saying what `says` says — where `{main.rs}` stands for that
        /// file's URI — and its diagnostics in.
        fn new(says: &Says) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let main = if says.main.is_empty() { MAIN } else { says.main };
            fs::write(dir.path().join("main.rs"), main).unwrap();
            fs::write(dir.path().join("other.rs"), "fn other() {}\n").unwrap();
            let resolved = fs::canonicalize(dir.path()).unwrap();
            let fill = |text: &str| {
                let mut text = text.to_string();
                for name in ["main.rs", "other.rs", "new.rs"] {
                    let uri = nun_lsp::uri::from_path(&resolved.join(name)).unwrap();
                    text = text.replace(&format!("{{{name}}}"), uri.as_str());
                }
                text
            };
            let log = tempfile::NamedTempFile::new().unwrap();

            let (buffer, _) = Buffer::load(dir.path().join("main.rs")).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                crate::commands::defaults(crate::commands::KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 120, 30));
            app.set_lightbulb(says.lightbulb);
            let (sender, done) = channel();
            app.open_folder(
                dir.path().to_path_buf(),
                dir.path().join(".trash"),
                false,
                Box::new(move |message| {
                    let _ = sender.send(message);
                }),
            );
            let args = [
                CAPS.to_string(),
                fill(if says.actions.is_empty() { "[]" } else { says.actions }),
                fill(if says.resolved.is_empty() { "null" } else { says.resolved }),
                if says.diag.is_empty() { DIAG } else { says.diag }.to_string(),
                if says.delay.is_empty() { "0" } else { says.delay }.to_string(),
                fill(if says.apply.is_empty() { "{}" } else { says.apply }),
                log.path().display().to_string(),
            ];
            let spec = ServerSpec {
                command: "sh".into(),
                args: ["-c".to_string(), SERVER.to_string(), "server".to_string()]
                    .into_iter()
                    .chain(args)
                    .collect(),
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
            let mut tester = Self { app, lsp, done, dir, log };
            tester.until(|app| {
                app.can_code_action() && !app.diagnostics.marks(app.doc().id).is_empty()
            });
            tester
        }

        /// Hand the editor whatever the server and the worker say, and tick
        /// it, until `done`.
        fn until(&mut self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + std::time::Duration::from_secs(20);
            while !done(&self.app) {
                assert!(Instant::now() < deadline, "gave up; it says {:?}", self.app.message());
                if let Ok(event) = self.lsp.recv_timeout(std::time::Duration::from_millis(5)) {
                    self.app.handle(Event::Lsp(event));
                }
                while let Ok(message) = self.done.try_recv() {
                    self.app.handle(Event::Workspace(message));
                }
                self.app.tick(Instant::now());
            }
        }

        /// What the server has read, one message a line.
        fn log(&self) -> String {
            fs::read_to_string(self.log.path()).unwrap()
        }

        fn text(&self) -> String {
            self.app.buffer().text().to_string()
        }

        fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.app.handle(Event::Key(KeyEvent::new(code, modifiers)));
        }

        fn click(&mut self, column: u16, row: u16, button: MouseButton) {
            for kind in [MouseEventKind::Down(button), MouseEventKind::Up(button)] {
                let modifiers = KeyModifiers::NONE;
                self.app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers }));
            }
        }

        /// Click wherever `target` is laid out.
        fn click_on(&mut self, target: Target) {
            self.app.relayout();
            let area = self.app.viewport;
            let at = (area.top()..area.bottom())
                .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                .find(|&(x, y)| self.app.hits.at(x, y).map(|hit| hit.target) == Some(target))
                .unwrap_or_else(|| panic!("{target:?} is not on screen"));
            self.click(at.0, at.1, MouseButton::Left);
        }

        /// The card's buttons, as drawn.
        fn buttons(&self) -> Vec<String> {
            let mut cells = Cells::empty(self.app.viewport);
            self.app.render(self.app.viewport, &mut cells);
            self.app
                .card_buttons()
                .iter()
                .map(|area| {
                    let text: String =
                        (area.x..area.right()).map(|x| cells[(x, area.y)].symbol()).collect();
                    text.trim().to_string()
                })
                .collect()
        }

        /// Open the diagnostic's card from the keyboard.
        fn card(&mut self) {
            self.app.run(Command::NextDiagnostic);
            assert!(self.app.card.is_some());
        }

        fn has_fixes(app: &App) -> bool {
            app.card.as_ref().is_some_and(|card| card.fixes() > 0)
        }
    }

    fn two_fixes_and_a_refactor() -> String {
        format!(
            "[{},{},{}]",
            action("Extract into a function", "refactor.extract", ""),
            action("Remove the variable", "quickfix", ""),
            action(
                "Prefix with an underscore",
                "quickfix",
                &format!(r#","isPreferred":true{}"#, underscore())
            ),
        )
    }

    #[test]
    fn a_fix_on_the_card_applies_as_one_undo_step() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        assert_eq!(
            t.buttons(),
            ["Prefix with an underscore", "Remove the variable"],
            "the fixes, the preferred first; the refactor is not a fix"
        );
        // The server was asked about the diagnostic, as it said it.
        let log = t.log();
        let asked = log.lines().find(|line| line.contains("textDocument/codeAction")).unwrap();
        assert!(asked.contains("unused variable") && asked.contains("unused_variables"), "{asked}");

        t.click_on(Target::CardButton(0));
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
        assert_eq!(t.app.message(), Some("Applied “Prefix with an underscore”."));
        assert!(t.app.card.is_none());
        t.key(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(t.text(), MAIN, "one undo takes it all back");
    }

    #[test]
    fn a_slow_server_never_delays_the_card() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, delay: "1", ..Says::default() });
        let asked = Instant::now();
        t.card();
        assert!(t.buttons().is_empty(), "the card is up at once, without them");
        t.until(Tester::has_fixes);
        assert!(asked.elapsed() >= std::time::Duration::from_millis(900));
        assert_eq!(t.buttons().len(), 2);
    }

    #[test]
    fn a_card_that_closes_first_cancels_the_question() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, delay: "1", ..Says::default() });
        t.card();
        t.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(t.app.code_actions.card_asked.is_none());
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !t.log().contains("$/cancelRequest") {
            assert!(Instant::now() < deadline, "never cancelled: {}", t.log());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn more_fixes_than_fit_are_one_click_away_in_the_chooser() {
        let fixes: Vec<String> = ["One", "Two", "Three"]
            .iter()
            .map(|title| action(title, "quickfix", &underscore()))
            .collect();
        let actions = format!("[{}]", fixes.join(","));
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        assert_eq!(t.buttons(), ["One", "Two", "1 more…"]);
        t.click_on(Target::CardButton(2));
        let rows: Vec<&str> = t
            .app
            .finder
            .as_ref()
            .expect("the chooser")
            .rows
            .iter()
            .map(|row| row.entry.label.as_str())
            .collect();
        assert_eq!(rows, ["One", "Two", "Three"]);
    }

    #[test]
    fn the_same_fixes_are_in_the_chooser_from_the_keyboard() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        t.app.doc_mut().buffer.set_selections(Selections::single(Caret::caret(16)));
        t.key(KeyCode::Char('.'), KeyModifiers::CONTROL);
        t.until(|app| app.finder.is_some());
        let palette = t.app.finder.as_ref().unwrap();
        let rows: Vec<(&str, &str)> = palette
            .rows
            .iter()
            .map(|row| (row.entry.label.as_str(), row.entry.hint.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Prefix with an underscore", "fix"),
                ("Remove the variable", "fix"),
                ("Extract into a function", "refactor"),
            ]
        );
        t.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
    }

    #[test]
    fn an_edit_left_for_later_is_asked_for_when_chosen() {
        let actions = format!("[{}]", action("Prefix it", "quickfix", r#","data":{"n":7}"#));
        let resolved = action("Prefix it", "quickfix", &underscore());
        let mut t =
            Tester::new(&Says { actions: &actions, resolved: &resolved, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        t.click_on(Target::CardButton(0));
        t.until(|app| app.buffer().text() != MAIN);
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
        let log = t.log();
        let resolving = log.lines().find(|line| line.contains("codeAction/resolve")).unwrap();
        assert!(resolving.contains(r#""data":{"n":7}"#), "handed back as it came: {resolving}");
    }

    #[test]
    fn a_command_s_edit_comes_back_from_the_server_and_is_answered() {
        let actions = format!(
            "[{}]",
            action("Fix it", "quickfix", r#","command":{"title":"Fix it","command":"fix.it"}"#)
        );
        let apply =
            format!(r#"{{"edit":{{"changes":{{"{{main.rs}}":[{}]}}}}}}"#, edit(0, 16, 17, "_x"));
        let mut t = Tester::new(&Says { actions: &actions, apply: &apply, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        t.click_on(Target::CardButton(0));
        t.until(|app| app.buffer().text() != MAIN);
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
        assert_eq!(t.app.message(), Some("Applied “Fix it”."), "named after the command");
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !t.log().contains(r#""applied":true"#) {
            assert!(Instant::now() < deadline, "never answered: {}", t.log());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(t.log().contains("workspace/executeCommand"));
    }

    #[test]
    fn an_edit_to_several_files_is_previewed_and_a_server_waits_for_the_verdict() {
        let actions = format!(
            "[{}]",
            action(
                "Fix it everywhere",
                "quickfix",
                r#","command":{"title":"Fix it everywhere","command":"fix.it"}"#
            )
        );
        let apply = format!(
            r#"{{"label":"Fix it everywhere","edit":{{"changes":{{"{{main.rs}}":[{}],"{{other.rs}}":[{}]}}}}}}"#,
            edit(0, 16, 17, "_x"),
            edit(0, 3, 8, "renamed"),
        );
        let mut t = Tester::new(&Says { actions: &actions, apply: &apply, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        t.click_on(Target::CardButton(0));
        t.until(App::edit_previewing);
        assert_eq!(t.text(), MAIN, "nothing yet");
        assert!(!t.log().contains("applied"), "the server is still waiting");

        t.click_on(Target::EditPreview(crate::app::workspace_edit::Spot::Action(0)));
        t.until(|app| !app.edits.busy());
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
        assert_eq!(fs::read_to_string(t.dir.path().join("other.rs")).unwrap(), "fn renamed() {}\n");
        let message = t.app.message().unwrap();
        assert!(message.starts_with("Applied “Fix it everywhere” in 2 files."), "{message}");
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !t.log().contains(r#""applied":true"#) {
            assert!(Instant::now() < deadline, "never answered: {}", t.log());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // And the way back takes back both.
        t.app.run(Command::UndoRename);
        t.until(|app| !app.edits.busy());
        assert_eq!(t.text(), MAIN);
        assert_eq!(fs::read_to_string(t.dir.path().join("other.rs")).unwrap(), "fn other() {}\n");
    }

    #[test]
    fn a_fix_that_creates_a_file_is_previewed_rather_than_made_at_once_and_undoes_whole() {
        let edit = format!(
            r#","edit":{{"documentChanges":[{{"kind":"create","uri":"{{new.rs}}"}},{{"textDocument":{{"uri":"{{new.rs}}","version":null}},"edits":[{}]}},{{"textDocument":{{"uri":"{{main.rs}}","version":null}},"edits":[{}]}}]}}"#,
            edit(0, 0, 0, "pub fn x() {}"),
            edit(0, 16, 17, "_x"),
        );
        let actions = format!("[{}]", action("Move it out", "quickfix", &edit));
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        t.click_on(Target::CardButton(0));
        t.until(App::edit_previewing);
        assert_eq!(t.app.edit_preview_rows()[0], "{Create new.rs}");
        assert_eq!(t.text(), MAIN, "one open file, but it makes another: previewed");

        t.click_on(Target::EditPreview(crate::app::workspace_edit::Spot::Action(0)));
        t.until(|app| !app.edits.busy());
        let new = t.dir.path().join("new.rs");
        assert_eq!(fs::read_to_string(&new).unwrap(), "pub fn x() {}");
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
        let message = t.app.message().unwrap();
        assert!(
            message.starts_with("Applied “Move it out” in 1 file, and created new.rs."),
            "{message}"
        );

        t.app.run(Command::UndoRename);
        t.until(|app| !app.edits.busy());
        assert!(!new.exists(), "into the trash");
        assert_eq!(t.text(), MAIN);
    }

    #[test]
    fn a_fix_offered_before_the_text_changed_is_not_made() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        let offered = t.app.code_actions.card.as_ref().unwrap().1[0].clone();
        t.app.handle(Event::Paste("// edited\n".into()));
        t.app.choose(offered);
        assert!(t.app.message().unwrap().contains("has changed since it was offered"));
        assert!(!t.text().contains("_x"));
    }

    #[test]
    fn the_right_click_menu_offers_code_actions() {
        let mut t = Tester::new(&Says::default());
        let text = t.app.areas().0;
        t.click(text.x + 10, text.y, MouseButton::Right);
        let menu = t.app.menu.as_ref().expect("a menu");
        assert!(menu.commands.contains(&Command::CodeActions));
    }

    #[test]
    fn positions_are_asked_in_the_servers_units_where_the_text_is_now() {
        // The crab is two UTF-16 units, so `x` is at 18..19.
        let main = "fn main() { let 🦀x = 1; }\n";
        let diag = DIAG.replace(":16}", ":18}").replace(":17}", ":19}");
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { main, diag: &diag, actions: &actions, ..Says::default() });
        // Typed before the card opens: the mark moves along, one unit.
        t.app.doc_mut().buffer.set_selections(Selections::single(Caret::caret(0)));
        t.app.handle(Event::Paste("é".into()));
        t.card();
        t.until(Tester::has_fixes);
        let log = t.log();
        let asked = log.lines().rfind(|line| line.contains("textDocument/codeAction"));
        let asked = asked.unwrap();
        let range =
            r#""range":{"end":{"character":20,"line":0},"start":{"character":19,"line":0}}"#;
        assert_eq!(asked.matches(range).count(), 2, "the question, and the diagnostic: {asked}");
    }

    #[test]
    fn an_edit_whose_text_moved_on_while_its_command_ran_is_previewed() {
        let actions = format!(
            "[{}]",
            action("Fix it", "quickfix", r#","command":{"title":"Fix it","command":"fix.it"}"#)
        );
        let apply =
            format!(r#"{{"edit":{{"changes":{{"{{main.rs}}":[{}]}}}}}}"#, edit(0, 16, 17, "_x"));
        let mut t = Tester::new(&Says { actions: &actions, apply: &apply, ..Says::default() });
        t.card();
        t.until(Tester::has_fixes);
        t.click_on(Target::CardButton(0));
        // Typed while the server works on it, before its edit arrives.
        t.app.doc_mut().buffer.set_selections(Selections::single(Caret::caret(0)));
        t.app.handle(Event::Paste("é".into()));
        t.until(App::edit_previewing);
        assert_eq!(t.text(), format!("é{MAIN}"), "not made where it no longer belongs");

        // Put away unseen, and the server hears it was not made.
        t.app.show_file_tree();
        assert!(!t.app.edit_previewing());
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !t.log().contains(r#""applied":false"#) {
            assert!(Instant::now() < deadline, "never answered: {}", t.log());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_long_title_is_cut_short_on_its_button() {
        let label = button_label("Change the type of this binding to something much longer", "…");
        assert!(label.ends_with('…'));
        assert!(label.width() <= MOST_BUTTON_WIDTH, "{label}");
        assert_eq!(button_label("Short", "…"), "Short");
        assert_eq!(button_label("Import\nfrom\tthere", "…"), "Import from there");

        let label =
            button_label("Change the type of this binding to something much longer", "~\u{301}");
        assert!(label.ends_with("~\u{301}"), "{label}");
        assert_eq!(
            label.width(),
            MOST_BUTTON_WIDTH,
            "the ellipsis is one cell, however many chars"
        );
    }

    #[test]
    fn a_title_is_never_cut_inside_a_grapheme() {
        for (title, whole) in [
            (format!("{}👨\u{200d}👩\u{200d}👧 family", "a".repeat(27)), "👨\u{200d}👩\u{200d}👧"),
            (format!("{}🇮🇸 flag", "a".repeat(28)), "🇮🇸"),
            (format!("{}👍🏽 ok", "a".repeat(27)), "👍🏽"),
        ] {
            let label = button_label(&title, "…");
            let kept = label.strip_suffix('…').unwrap();
            assert!(title.starts_with(kept), "{label:?}");
            let last = kept.graphemes(true).next_back().unwrap();
            assert!(last == "a" || last == whole, "{label:?}");
        }
        // Measured as it is drawn: a grapheme at a time.
        let label = button_label(&"لا".repeat(15), "…");
        assert!(drawn_width(&label) <= MOST_BUTTON_WIDTH, "{label}");
    }

    // ── in the gutter ───────────────────────────────────────────────────────

    impl Tester {
        /// How many times the server has been asked for code actions.
        fn asked(&self) -> usize {
            self.log().lines().filter(|line| line.contains("textDocument/codeAction")).count()
        }

        /// Whether the gutter's question is on its way.
        fn bulb_asking(app: &App) -> bool {
            app.code_actions.bulb.as_ref().is_some_and(|b| matches!(b.state, BulbState::Asking(_)))
        }

        /// Whether the gutter's question has been answered.
        fn bulb_answered(app: &App) -> bool {
            app.code_actions
                .bulb
                .as_ref()
                .is_some_and(|b| matches!(b.state, BulbState::Answered(_)))
        }

        /// Keep handing the editor what comes in, and ticking it, for `long`.
        fn wait(&mut self, long: Duration) {
            let until = Instant::now() + long;
            self.until(|_| Instant::now() >= until);
        }

        /// The symbol drawn in the gutter's last column on `row`.
        fn gutter_mark(&self, row: u16) -> String {
            let mut cells = Cells::empty(self.app.viewport);
            self.app.render(self.app.viewport, &mut cells);
            let (text, _) = self.app.areas();
            let x = text.x + self.app.gutter_width() - 1;
            cells[(x, text.y + row)].symbol().to_string()
        }
    }

    #[test]
    fn a_line_with_actions_is_marked_once_the_caret_rests_and_the_mark_opens_them() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, lightbulb: true, ..Says::default() });
        t.until(|app| app.lightbulb_line() == Some(0));
        assert_eq!(t.gutter_mark(0), t.app.palette.glyph(nun_ui::Glyph::Lightbulb));
        assert_eq!(t.gutter_mark(1), " ", "only the caret's line");

        // Asked about the whole line, as something the person did not ask
        // for, with the diagnostic on it.
        let log = t.log();
        let asked = log.lines().find(|line| line.contains("textDocument/codeAction")).unwrap();
        assert!(asked.contains(r#""triggerKind":2"#), "{asked}");
        assert!(asked.contains("unused_variables"), "{asked}");
        let line = r#""range":{"end":{"character":24,"line":0},"start":{"character":0,"line":0}}"#;
        assert!(asked.contains(line), "{asked}");

        // Once, and not again while the caret stays on the line.
        t.key(KeyCode::Right, KeyModifiers::NONE);
        t.wait(BULB_SETTLE * 2);
        assert_eq!(t.asked(), 1);
        assert_eq!(t.app.bulb_deadline(), None, "nothing left to wake up for");

        t.click_on(Target::Lightbulb);
        let rows: Vec<&str> = t
            .app
            .finder
            .as_ref()
            .expect("the chooser")
            .rows
            .iter()
            .map(|row| row.entry.label.as_str())
            .collect();
        assert_eq!(
            rows,
            ["Prefix with an underscore", "Remove the variable", "Extract into a function"]
        );
        assert_eq!(t.asked(), 1, "what the mark stands for, without asking again");
        t.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(t.text(), "fn main() { let _x = 1; }\n");
    }

    #[test]
    fn typing_neither_asks_nor_waits_and_moving_on_asks_again() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, lightbulb: true, ..Says::default() });
        t.until(|app| app.lightbulb_line() == Some(0));
        let before = t.asked();

        for ch in "abc".chars() {
            t.key(KeyCode::Char(ch), KeyModifiers::NONE);
            assert_eq!(t.app.lightbulb_line(), None, "the mark is about text no longer there");
            assert_eq!(t.app.bulb_deadline(), None, "and typing arms nothing");
        }
        t.wait(BULB_SETTLE * 2);
        assert_eq!(t.asked(), before, "typing asked nothing: {}", t.log());

        // Going to another line is resting somewhere new.
        t.key(KeyCode::Down, KeyModifiers::NONE);
        assert!(t.app.bulb_deadline().is_some());
        t.until(|app| app.lightbulb_line() == Some(1));
        assert_eq!(t.asked(), before + 1);
    }

    #[test]
    fn moving_on_cancels_the_question() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says {
            actions: &actions,
            delay: "1",
            lightbulb: true,
            ..Says::default()
        });
        t.until(Tester::bulb_asking);
        t.key(KeyCode::Down, KeyModifiers::NONE);
        assert!(!Tester::bulb_asking(&t.app));
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !t.log().contains("$/cancelRequest") {
            assert!(Instant::now() < deadline, "never cancelled: {}", t.log());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_line_with_nothing_on_offer_is_not_marked() {
        let mut t = Tester::new(&Says { lightbulb: true, ..Says::default() });
        t.until(Tester::bulb_answered);
        assert_eq!(t.app.lightbulb_line(), None);
        assert_eq!(t.gutter_mark(0), " ");
        // A click where it would be is the gutter's: it selects the line.
        let (text, _) = t.app.areas();
        t.click(text.x + t.app.gutter_width() - 1, text.y, MouseButton::Left);
        assert!(t.app.finder.is_none());
    }

    #[test]
    fn turned_off_it_asks_nothing() {
        let actions = two_fixes_and_a_refactor();
        let mut t = Tester::new(&Says { actions: &actions, ..Says::default() });
        assert_eq!(t.app.bulb_deadline(), None);
        t.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(t.app.bulb_deadline(), None);
        t.wait(BULB_SETTLE * 2);
        assert_eq!(t.asked(), 0);
        assert_eq!(t.app.lightbulb_line(), None);
    }

    proptest::proptest! {
        /// Whatever a server calls a fix, its button is a prefix of the title
        /// that ends between graphemes and fits.
        #[test]
        fn a_button_label_is_a_prefix_that_fits(title in "[a 😀é中\u{301}\u{200d}لا🇮🇸]{0,40}") {
            let label = button_label(&title, "…");
            let kept = label.strip_suffix('…').unwrap_or(&label);
            proptest::prop_assert!(title.starts_with(kept));
            proptest::prop_assert!(title.graphemes(true).count() >= kept.graphemes(true).count());
            let boundary = title
                .grapheme_indices(true)
                .map(|(at, _)| at)
                .chain(std::iter::once(title.len()))
                .any(|at| at == kept.len());
            proptest::prop_assert!(boundary, "{:?} from {:?}", label, title);
            proptest::prop_assert!(drawn_width(&label) <= MOST_BUTTON_WIDTH);
        }
    }
}
