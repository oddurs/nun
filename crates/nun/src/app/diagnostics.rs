//! Diagnostics: underlined in the text, marked on the rail, counted in the
//! status line, and explained in a card.
//!
//! A server describes a version of the text, and by the time its answer
//! arrives the text has usually moved on — a keystroke or two, sometimes a
//! paste. Drawing its ranges against the text as it is now would put the
//! underline on the wrong characters. So every version the server may yet
//! describe is kept, with the edits that led from it to the next: an answer
//! about version 7 is read against the text as it was at 7 and carried
//! forward through every edit since. From then on the marks move with each
//! edit as it is made, the way a selection does, until the server says again.
//!
//! The rail, the status line and the underlines all read the one list, so
//! they cannot disagree about how many there are.
//!
//! Every way in has a mouse path: resting on an underline opens its card,
//! clicking a mark on the rail jumps to it, clicking the count in the status
//! line jumps to the next, and the card has buttons to go on from there.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use nun_core::{Assoc, Edit, Range, Selections};
use nun_lsp::Encoding;
use nun_lsp::types::{Diagnostic, DiagnosticSeverity, NumberOrString};
use nun_theme::Role;
use nun_ui::{EditorView, Mark, Paragraph, Rail, Run, Severity, Tally};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ropey::Rope;

use super::card::{Anchor, Card};
use super::panes::DocId;
use super::{App, Document, Focus, Outcome, Target};
use crate::commands::Command;

/// How many versions of a document are kept for a server that is slow to
/// answer. A server further behind than this is describing text so old that
/// its answer is better waited out than drawn.
const MOST_VERSIONS: usize = 256;

/// How many diagnostics one card lists before it says how many more.
const MOST_IN_CARD: usize = 8;

/// Every followed document's diagnostics, where they are now.
#[derive(Debug, Default)]
pub(super) struct Diagnostics {
    docs: HashMap<DocId, Tracked>,
}

#[derive(Debug, Default)]
struct Tracked {
    /// The text at every version the server may yet describe, oldest first.
    history: VecDeque<Step>,
    /// A digest of the last answer placed, so the same answer is not placed
    /// twice — a second placing would undo the mapping since.
    digest: Option<u64>,
    /// Where each diagnostic is now, ordered by where it starts.
    marks: Vec<Mark>,
    /// What each one says, in the same order.
    notes: Vec<Note>,
}

/// One version of a document.
#[derive(Debug)]
struct Step {
    version: i32,
    /// The text as it was at this version. Ropes share their storage, so
    /// keeping a few hundred of these costs the edits between them, not the
    /// file a few hundred times.
    text: Rope,
    /// The edits that took it to the next version, in order.
    edits: Vec<Edit>,
}

/// What a diagnostic says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Note {
    pub(super) message: String,
    /// Who said it: `rustc`, `clippy`, `pyright`.
    pub(super) source: Option<String>,
    /// Its code, such as `E0308`.
    pub(super) code: Option<String>,
    /// All of it, as the server said it, for handing back to the server
    /// when asking what would fix it.
    pub(super) diagnostic: Diagnostic,
}

impl Diagnostics {
    /// A document started being followed at `version`, holding `text`.
    /// Anything said about it before is about a different text.
    pub(super) fn opened(&mut self, doc: DocId, version: i32, text: &Rope) {
        let step = Step { version, text: text.clone(), edits: Vec::new() };
        self.docs.insert(doc, Tracked { history: VecDeque::from([step]), ..Tracked::default() });
    }

    /// A followed document was edited by `edits` into `version`, which holds
    /// `text`.
    pub(super) fn changed(&mut self, doc: DocId, version: i32, edits: &[Edit], text: &Rope) {
        let Some(tracked) = self.docs.get_mut(&doc) else { return };
        if let Some(last) = tracked.history.back_mut() {
            last.edits.extend_from_slice(edits);
        }
        tracked.history.push_back(Step { version, text: text.clone(), edits: Vec::new() });
        while tracked.history.len() > MOST_VERSIONS {
            tracked.history.pop_front();
        }
        for mark in &mut tracked.marks {
            *mark = carried(*mark, edits);
        }
        // Edits keep marks in order unless they collapse onto each other, and
        // the ties that leaves are sorted out here rather than trusted.
        sort_together(&mut tracked.marks, &mut tracked.notes);
    }

    /// A document is no longer followed.
    pub(super) fn closed(&mut self, doc: DocId) {
        self.docs.remove(&doc);
    }

    /// The server's latest answer about `doc`: `diagnostics` describing
    /// `version`, or the version current when it arrived if the server did
    /// not say. `None` for no answer — the server has gone, or said nothing is
    /// wrong.
    ///
    /// Whether the marks changed.
    pub(super) fn publish(
        &mut self,
        doc: DocId,
        answer: Option<(Option<i32>, &[Diagnostic])>,
        encoding: Encoding,
    ) -> bool {
        let Some(tracked) = self.docs.get_mut(&doc) else { return false };
        let Some((version, diagnostics)) = answer else {
            let had = !tracked.marks.is_empty();
            tracked.marks.clear();
            tracked.notes.clear();
            tracked.digest = None;
            return had;
        };
        let digest = digest(version, diagnostics);
        if tracked.digest == Some(digest) {
            return false;
        }
        tracked.digest = Some(digest);

        let latest = tracked.history.back().map_or(0, |step| step.version);
        let version = version.unwrap_or(latest);
        let Some(first) = tracked.history.iter().position(|step| step.version == version) else {
            // Older than anything kept, or a version never sent. What is on
            // screen stays until an answer that can be placed arrives.
            return false;
        };
        // Nothing older than this will be asked about again.
        tracked.history.drain(..first);

        let text = &tracked.history[0].text;
        let mut placed: Vec<(Mark, Note)> = diagnostics
            .iter()
            .map(|diagnostic| {
                let range = encoding.char_range(text, diagnostic.range);
                let mark =
                    Mark { start: range.start, end: range.end, severity: severity(diagnostic) };
                (mark, note(diagnostic))
            })
            .collect();
        for step in &tracked.history {
            for (mark, _) in &mut placed {
                *mark = carried(*mark, &step.edits);
            }
        }
        (tracked.marks, tracked.notes) = placed.into_iter().unzip();
        sort_together(&mut tracked.marks, &mut tracked.notes);
        true
    }

    /// Where `doc`'s diagnostics are now, ordered by where they start.
    pub(super) fn marks(&self, doc: DocId) -> &[Mark] {
        self.docs.get(&doc).map_or(&[], |tracked| &tracked.marks)
    }

    /// What `doc`'s diagnostics say, in the order of [`Diagnostics::marks`].
    pub(super) fn notes(&self, doc: DocId) -> &[Note] {
        self.docs.get(&doc).map_or(&[], |tracked| &tracked.notes)
    }

    /// How many of each kind `doc` has.
    pub(super) fn tally(&self, doc: DocId) -> Tally {
        Tally::of(self.marks(doc).iter().map(|mark| mark.severity))
    }
}

/// A mark carried through `edits`. Neither end grows to take in text typed
/// against it: new text is not what the server complained about.
fn carried(mark: Mark, edits: &[Edit]) -> Mark {
    let (mut start, mut end) = (mark.start, mark.end);
    for edit in edits {
        start = edit.map_pos(start, Assoc::After);
        end = edit.map_pos(end, Assoc::Before).max(start);
    }
    Mark { start, end, ..mark }
}

/// Order marks by where they start, keeping each note beside its mark.
fn sort_together(marks: &mut Vec<Mark>, notes: &mut Vec<Note>) {
    if marks.windows(2).all(|pair| (pair[0].start, pair[0].end) <= (pair[1].start, pair[1].end)) {
        return;
    }
    let mut both: Vec<(Mark, Note)> = marks.drain(..).zip(notes.drain(..)).collect();
    both.sort_by_key(|(mark, _)| (mark.start, mark.end, mark.severity));
    (*marks, *notes) = both.into_iter().unzip();
}

fn severity(diagnostic: &Diagnostic) -> Severity {
    match diagnostic.severity {
        Some(DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        // The protocol leaves an unmarked one to the client; the one reading
        // that cannot hide a real problem is an error.
        _ => Severity::Error,
    }
}

fn note(diagnostic: &Diagnostic) -> Note {
    Note {
        message: diagnostic.message.clone(),
        source: diagnostic.source.clone(),
        code: diagnostic.code.as_ref().map(|code| match code {
            NumberOrString::Number(number) => number.to_string(),
            NumberOrString::String(text) => text.clone(),
        }),
        diagnostic: diagnostic.clone(),
    }
}

/// Enough of an answer to tell it from a different one.
fn digest(version: Option<i32>, diagnostics: &[Diagnostic]) -> u64 {
    let mut hasher = DefaultHasher::new();
    version.hash(&mut hasher);
    for diagnostic in diagnostics {
        let range = diagnostic.range;
        (range.start.line, range.start.character, range.end.line, range.end.character)
            .hash(&mut hasher);
        diagnostic.message.hash(&mut hasher);
        severity(diagnostic).hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether a server event can change what is marked: new diagnostics, or a
/// server going away and taking its diagnostics with it.
pub(super) const fn moves_marks(event: &nun_lsp::Event) -> bool {
    matches!(event, nun_lsp::Event::Diagnostics { .. } | nun_lsp::Event::Status { .. })
}

impl App {
    /// Read every document's diagnostics afresh from the servers.
    pub(super) fn refresh_diagnostics(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else { return };
        for document in &self.docs {
            let id = document.id;
            let published = lsp
                .diagnostics(id)
                .map(|published| (published.version, &published.diagnostics[..]));
            let encoding = lsp.encoding(id).unwrap_or_default();
            self.diagnostics.publish(id, published, encoding);
        }
    }

    /// Whether `doc` gets a rail: while a server follows it, so the rail does
    /// not come and go as problems do, or while it has marks at all.
    fn has_rail(&self, doc: &Document) -> bool {
        self.lsp.as_ref().is_some_and(|lsp| lsp.is_open(doc.id))
            || !self.diagnostics.marks(doc.id).is_empty()
    }

    /// Split a pane's text area into the part the text is drawn in and the
    /// rail's column, if it has one.
    pub(super) fn split_rail(&self, doc: &Document, text: Rect) -> (Rect, Option<Rect>) {
        let gutter = EditorView::new(&doc.buffer, &self.palette).gutter_width();
        if !self.has_rail(doc) || text.width <= gutter.saturating_add(2) {
            return (text, None);
        }
        let rail = Rect { x: text.right() - 1, width: 1, ..text };
        (Rect { width: text.width - 1, ..text }, Some(rail))
    }

    /// The document in `pane`, the rectangle its text is drawn in, and its
    /// rail.
    fn pane_parts(&self, pane: usize) -> Option<(&Document, Rect, Option<Rect>)> {
        let id = self.panes.get(pane)?.current()?;
        let doc = self.doc_by(id)?;
        let (text, rail) = self.split_rail(doc, self.text_area_of(pane)?);
        Some((doc, text, rail))
    }

    /// The view of `doc` as drawn.
    fn view_of<'a>(&'a self, doc: &'a Document) -> EditorView<'a> {
        EditorView::new(&doc.buffer, &self.palette).scrolled_to(doc.scroll)
    }

    /// The lines of `doc` in view in `text`, for the rail's thumb.
    fn lines_in_view(&self, doc: &Document, text: Rect) -> std::ops::Range<usize> {
        let view = self.view_of(doc);
        let first = view.line_at_row(0).unwrap_or(doc.scroll);
        let last = (0..usize::from(text.height)).rev().find_map(|row| view.line_at_row(row));
        first..last.map_or(first, |last| last + 1)
    }

    /// The line each of `doc`'s marks starts on, and how serious it is.
    fn rail_marks(&self, doc: &Document) -> Vec<(usize, Severity)> {
        let buffer = &doc.buffer;
        self.diagnostics
            .marks(doc.id)
            .iter()
            .map(|mark| (buffer.line_of(mark.start.min(buffer.len_chars())), mark.severity))
            .collect()
    }

    /// Put the underlines and the rail into the hit map: every underline in
    /// view is something to rest the pointer on, and every rail row with
    /// marks is something to rest on or click.
    pub(super) fn layout_diagnostics(&self, hits: &mut nun_input::HitMap<Target>) {
        for (pane, _) in self.panes.rects(self.panes_area()) {
            let Some((doc, text, rail)) = self.pane_parts(pane) else { continue };
            if let Some(rail) = rail {
                hits.push(super::cells(rail), Target::Rail(pane), false);
                let lines = doc.buffer.len_lines();
                for bucket in Rail::buckets(&self.rail_marks(doc), lines, rail.height) {
                    let cell = Rect { y: rail.y + bucket.row, height: 1, ..rail };
                    hits.push(super::cells(cell), Target::RailMark(pane, bucket.row), true);
                }
            }
            let view = self.view_of(doc);
            let shown = self.lines_in_view(doc, text);
            for (index, mark) in self.diagnostics.marks(doc.id).iter().enumerate() {
                let end = mark.end.min(doc.buffer.len_chars());
                if doc.buffer.line_of(end) < shown.start {
                    continue;
                }
                if doc.buffer.line_of(mark.start.min(end)) >= shown.end {
                    // Ordered by start, so everything after is below too.
                    break;
                }
                for rect in view.screen_rects(text, mark.start, mark.end) {
                    hits.push(super::cells(rect), Target::Diagnostic(pane, index), true);
                }
            }
        }
    }

    /// Draw a pane's rail.
    pub(super) fn render_rail(&self, pane: usize, doc: &Document, rail: Rect, cells: &mut Cells) {
        use ratatui::widgets::Widget as _;

        let text = Rect { x: rail.x.saturating_sub(1), ..rail };
        let hovered = match self.hover.current() {
            Some(Target::RailMark(over, row)) if over == pane => Some(row),
            _ => None,
        };
        Rail::new(&self.rail_marks(doc), doc.buffer.len_lines(), &self.palette)
            .viewing(self.lines_in_view(doc, text))
            .hovered(hovered)
            .render(rail, cells);
    }

    // ── going to them ───────────────────────────────────────────────────────

    /// Go to the next diagnostic after the caret, or the one before it,
    /// coming round at the ends.
    pub(super) fn step_diagnostic(&mut self, forward: bool) -> Outcome {
        let id = self.doc().id;
        let marks = self.diagnostics.marks(id);
        if marks.is_empty() {
            self.message = Some("No problems in this file.".into());
            return Outcome::Redraw;
        }
        let caret = self.doc().buffer.selections().primary().head;
        let index = if forward {
            marks.iter().position(|mark| mark.start > caret).unwrap_or(0)
        } else {
            marks.iter().rposition(|mark| mark.start < caret).unwrap_or(marks.len() - 1)
        };
        self.go_to_mark(index)
    }

    /// Put the caret at the start of the focused document's mark `index`,
    /// and say what it is in a card beside it.
    fn go_to_mark(&mut self, index: usize) -> Outcome {
        let id = self.doc().id;
        let Some(mark) = self.diagnostics.marks(id).get(index).copied() else {
            return Outcome::Continue;
        };
        self.focus = Focus::Editor;
        let start = mark.start.min(self.doc().buffer.len_chars());
        self.doc_mut().buffer.set_selections(Selections::single(Range::caret(start)));
        self.follow_caret();
        let anchor =
            Anchor::Text { pane: self.panes.focus(), doc: id, from: mark.start, to: mark.end };
        let body = self.card_body(id, index);
        let buttons = self.stepping_buttons(id);
        self.show_card(Card::new(anchor, body, buttons, None));
        self.ask_fixes();
        Outcome::Redraw
    }

    /// The buttons that go on to the next and previous diagnostics, when
    /// there is anywhere else to go.
    fn stepping_buttons(&self, doc: DocId) -> Vec<(String, Command)> {
        if self.diagnostics.marks(doc).len() < 2 {
            return Vec::new();
        }
        vec![
            ("‹ Previous".to_string(), Command::PreviousDiagnostic),
            ("Next ›".to_string(), Command::NextDiagnostic),
        ]
    }

    // ── the mouse ───────────────────────────────────────────────────────────

    /// A press on the rail, a mark on it, or the count in the status line.
    /// `y` is the screen row pressed.
    pub(super) fn diagnostics_press(&mut self, target: Target, y: u16) -> Outcome {
        match target {
            Target::RailMark(pane, row) => self.rail_mark_press(pane, row),
            Target::Rail(pane) => self.rail_press(pane, y),
            Target::StatusProblems => self.step_diagnostic(true),
            _ => Outcome::Continue,
        }
    }

    /// A click on a rail mark goes to the next of its diagnostics after the
    /// caret, so clicking a crowded row again and again visits each of them.
    fn rail_mark_press(&mut self, pane: usize, row: u16) -> Outcome {
        let Some((doc, _, Some(rail))) = self.pane_parts(pane) else { return Outcome::Continue };
        let lines = Rail::lines_at(row, doc.buffer.len_lines(), rail.height);
        let here: Vec<usize> = self
            .rail_marks(doc)
            .iter()
            .enumerate()
            .filter(|(_, (line, _))| lines.contains(line))
            .map(|(index, _)| index)
            .collect();
        let Some(&first) = here.first() else { return Outcome::Continue };
        self.panes.set_focus(pane);
        let caret = self.doc().buffer.selections().primary().head;
        let marks = self.diagnostics.marks(self.doc().id);
        let next = here.iter().copied().find(|&index| marks[index].start > caret).unwrap_or(first);
        self.go_to_mark(next)
    }

    /// A click on the rail away from any mark scrolls there, as a scrollbar
    /// would.
    fn rail_press(&mut self, pane: usize, y: u16) -> Outcome {
        let Some((doc, text, Some(rail))) = self.pane_parts(pane) else {
            return Outcome::Continue;
        };
        if !(rail.top()..rail.bottom()).contains(&y) {
            return Outcome::Continue;
        }
        let row = y - rail.top();
        let lines = doc.buffer.len_lines();
        let line = Rail::lines_at(row, lines, rail.height).start.min(lines.saturating_sub(1));
        let hidden = doc.buffer.hidden();
        let half = isize::try_from(text.height / 2).unwrap_or(0);
        let scroll = hidden.step(hidden.in_view(line), -half);
        let id = doc.id;
        if let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) {
            doc.scroll = scroll;
        }
        Outcome::Redraw
    }

    /// The pointer rested on `target`.
    pub(super) fn dwelt(&mut self, target: Target) -> Outcome {
        match target {
            // The symbol's card already says what is wrong with it.
            Target::Diagnostic(..) if self.card.as_ref().is_some_and(Card::is_held) => {
                Outcome::Continue
            }
            Target::Diagnostic(pane, index) => self.mark_card(pane, index),
            Target::RailMark(pane, row) => self.rail_card(pane, row),
            _ => Outcome::Redraw,
        }
    }

    /// The card for the diagnostic under the pointer, and any others on the
    /// same text.
    fn mark_card(&mut self, pane: usize, index: usize) -> Outcome {
        let Some((doc, _, _)) = self.pane_parts(pane) else { return Outcome::Continue };
        let id = doc.id;
        let Some(mark) = self.diagnostics.marks(id).get(index).copied() else {
            return Outcome::Continue;
        };
        let anchor = Anchor::Text { pane, doc: id, from: mark.start, to: mark.end };
        let body = self.card_body(id, index);
        let buttons = self.stepping_buttons(id);
        self.show_card(Card::new(anchor, body, buttons, Some(Target::Diagnostic(pane, index))));
        self.ask_fixes();
        Outcome::Redraw
    }

    /// The card for a row of the rail: everything on it, so a crowded row
    /// says how crowded it is.
    fn rail_card(&mut self, pane: usize, row: u16) -> Outcome {
        let Some((doc, _, Some(rail))) = self.pane_parts(pane) else { return Outcome::Continue };
        let lines = Rail::lines_at(row, doc.buffer.len_lines(), rail.height);
        let notes = self.diagnostics.notes(doc.id);
        let here: Vec<(usize, Severity, &Note)> = self
            .rail_marks(doc)
            .into_iter()
            .zip(notes)
            .filter(|((line, _), _)| lines.contains(line))
            .map(|((line, severity), note)| (line, severity, note))
            .collect();
        if here.is_empty() {
            return Outcome::Continue;
        }
        let (first, last) = (lines.start + 1, lines.end);
        let place =
            if first == last { format!("line {first}") } else { format!("lines {first}–{last}") };
        let count = here.len();
        let what = if count == 1 { "1 problem".to_string() } else { format!("{count} problems") };
        let mut body: Vec<Paragraph> =
            vec![vec![Run::new(format!("{what} on {place}"), Role::Dim)]];
        for (line, severity, note) in here.iter().take(MOST_IN_CARD) {
            let message = note.message.lines().next().unwrap_or_default();
            body.push(vec![
                Run::new(format!("Ln {}  ", line + 1), Role::Dim),
                Run::bold(severity.label(), severity.role()),
                Run::new(format!("  {message}"), Role::Text),
            ]);
        }
        if count > MOST_IN_CARD {
            body.push(vec![Run::new(format!("…and {} more", count - MOST_IN_CARD), Role::Dim)]);
        }
        let anchor = Anchor::Screen(Rect { y: rail.y + row, height: 1, ..rail });
        self.show_card(Card::new(anchor, body, Vec::new(), Some(Target::RailMark(pane, row))));
        Outcome::Redraw
    }

    /// What a card about mark `index` of `doc` says: that one, and every
    /// other on text it overlaps, most serious first.
    pub(super) fn card_body(&self, doc: DocId, index: usize) -> Vec<Paragraph> {
        let marks = self.diagnostics.marks(doc);
        let notes = self.diagnostics.notes(doc);
        let Some(target) = marks.get(index) else { return Vec::new() };
        let mut shown: Vec<usize> = (0..marks.len())
            .filter(|&other| {
                let mark = marks[other];
                other == index
                    || (mark.start < target.end.max(target.start + 1)
                        && target.start < mark.end.max(mark.start + 1))
            })
            .collect();
        shown.sort_by_key(|&other| (marks[other].severity, other != index));

        let mut body = Vec::new();
        for &other in shown.iter().take(MOST_IN_CARD) {
            if !body.is_empty() {
                body.push(Vec::new());
            }
            let (mark, note) = (marks[other], &notes[other]);
            let mut header = vec![Run::bold(mark.severity.label(), mark.severity.role())];
            let from: Vec<&str> =
                [note.source.as_deref(), note.code.as_deref()].into_iter().flatten().collect();
            if !from.is_empty() {
                header.push(Run::new(format!("  {}", from.join(" ")), Role::Dim));
            }
            body.push(header);
            for line in note.message.lines() {
                body.push(vec![Run::new(line, Role::Text)]);
            }
        }
        if shown.len() > MOST_IN_CARD {
            body.push(vec![Run::new(
                format!("…and {} more", shown.len() - MOST_IN_CARD),
                Role::Dim,
            )]);
        }
        body
    }

    // ── the status line ─────────────────────────────────────────────────────

    /// The counts in the status line, as runs: nothing when there is nothing
    /// to count.
    fn problems_label(&self) -> Option<Vec<(String, Role)>> {
        let tally = self.diagnostics.tally(self.doc().id);
        if tally.total() == 0 {
            return None;
        }
        let mut runs = vec![(" ".to_string(), Role::Dim)];
        for (count, glyph, role) in [
            (tally.errors, "✕", Role::Error),
            (tally.warnings, "▲", Role::Warn),
            (tally.notes, "●", Role::Info),
        ] {
            if count > 0 {
                runs.push((format!("{glyph} {count} "), role));
            }
        }
        Some(runs)
    }

    /// Where the counts go: just left of whatever is at the right of the
    /// status line, when there is room beside what its left half says.
    pub(super) fn problems_part(
        &self,
        status: Rect,
        text: u16,
        undo: Option<Rect>,
        close: Option<Rect>,
        lsp: Option<Rect>,
    ) -> Option<Rect> {
        let label = self.problems_label()?;
        let width =
            u16::try_from(label.iter().map(|(run, _)| run.chars().count()).sum::<usize>()).ok()?;
        let (left, right) = self.status();
        let edge = match lsp {
            Some(lsp) => lsp.x,
            None => close
                .map_or(status.right(), |close| close.x)
                .checked_sub(u16::try_from(right.chars().count() + 1).ok()?)?,
        };
        let x = edge.checked_sub(width)?;
        let used = undo.map_or_else(
            || text.saturating_add(u16::try_from(left.chars().count() + 1).unwrap_or(u16::MAX)),
            Rect::right,
        );
        (x > used).then(|| Rect::new(x, status.y, width, 1))
    }

    /// Draw the counts.
    pub(super) fn render_problems(&self, area: Rect, cells: &mut Cells) {
        let Some(label) = self.problems_label() else { return };
        let hovered = self.hover.current() == Some(Target::StatusProblems);
        let mut x = area.x;
        for (run, role) in label {
            let style = if hovered {
                self.palette.on(Role::Accent, Role::OnAccent)
            } else {
                self.palette.on(Role::Raised, role)
            };
            super::write_at(cells, area, x, &run, style);
            x = x.saturating_add(u16::try_from(run.chars().count()).unwrap_or(u16::MAX));
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_core::Buffer;
    use nun_lsp::types::{Position, Range as LspRange};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use proptest::prelude::*;

    use super::*;

    fn diagnostic(
        line: u32,
        from: u32,
        to: u32,
        severity: DiagnosticSeverity,
        message: &str,
    ) -> Diagnostic {
        Diagnostic {
            range: LspRange::new(Position::new(line, from), Position::new(line, to)),
            severity: Some(severity),
            source: Some("rustc".into()),
            message: message.into(),
            ..Diagnostic::default()
        }
    }

    fn marked(store: &Diagnostics, text: &Rope) -> Vec<String> {
        store.marks(0).iter().map(|mark| text.slice(mark.start..mark.end).to_string()).collect()
    }

    /// A store following one document, and the buffer it follows.
    fn following(text: &str) -> (Diagnostics, Buffer) {
        let mut buffer = Buffer::from_text(text);
        buffer.keep_edits(true);
        let mut store = Diagnostics::default();
        store.opened(0, 0, buffer.rope());
        (store, buffer)
    }

    /// Make an edit, and tell the store about it as the next version.
    fn edit(store: &mut Diagnostics, buffer: &mut Buffer, version: i32, at: usize, text: &str) {
        buffer.set_selections(Selections::single(Range::caret(at)));
        buffer.insert(text);
        store.changed(0, version, &buffer.take_edits(), buffer.rope());
    }

    #[test]
    fn an_answer_about_an_old_version_is_carried_through_the_edits_since() {
        let (mut store, mut buffer) = following("let x: u8 = \"a\";\nlet y = 😀z;\n");
        // Typed before the answer about version 0 arrives.
        edit(&mut store, &mut buffer, 1, 0, "// 中文\n");
        edit(&mut store, &mut buffer, 2, 4, "é");

        // UTF-16: the emoji is two units, so `z` is at 10..11.
        let answer = [
            diagnostic(0, 12, 15, DiagnosticSeverity::ERROR, "mismatched types"),
            diagnostic(1, 10, 11, DiagnosticSeverity::WARNING, "unused"),
        ];
        assert!(store.publish(0, Some((Some(0), &answer)), Encoding::Utf16));
        assert_eq!(marked(&store, buffer.rope()), ["\"a\"", "z"]);

        // And as editing goes on, the marks go with the text.
        let at = buffer.len_chars() - 3;
        edit(&mut store, &mut buffer, 3, at, "  ");
        assert_eq!(marked(&store, buffer.rope()), ["\"a\"", "z"]);
    }

    #[test]
    fn an_answer_with_no_version_describes_the_text_it_arrived_to() {
        let (mut store, mut buffer) = following("abc def\n");
        edit(&mut store, &mut buffer, 1, 0, "xx ");
        let answer = [diagnostic(0, 3, 6, DiagnosticSeverity::ERROR, "bad")];
        store.publish(0, Some((None, &answer)), Encoding::Utf16);
        assert_eq!(marked(&store, buffer.rope()), ["abc"]);
    }

    #[test]
    fn the_same_answer_twice_is_not_placed_twice() {
        let (mut store, mut buffer) = following("abc def\n");
        let answer = [diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "bad")];
        assert!(store.publish(0, Some((Some(0), &answer)), Encoding::Utf16));
        edit(&mut store, &mut buffer, 1, 0, "!");
        // Placed again against version 0 it would land in the same place, but
        // a server repeating itself about an older text is not news.
        assert!(!store.publish(0, Some((Some(0), &answer)), Encoding::Utf16));
        assert_eq!(marked(&store, buffer.rope()), ["def"]);
    }

    #[test]
    fn deleting_what_a_mark_is_on_leaves_it_empty_where_the_text_was() {
        let (mut store, mut buffer) = following("one two three\n");
        let answer = [diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "bad")];
        store.publish(0, Some((Some(0), &answer)), Encoding::Utf16);
        buffer.set_selections(Selections::single(Range::new(3, 8)));
        buffer.delete_backward();
        store.changed(0, 1, &buffer.take_edits(), buffer.rope());
        let mark = store.marks(0)[0];
        assert_eq!((mark.start, mark.end), (3, 3));
    }

    #[test]
    fn typing_at_either_end_of_a_mark_does_not_grow_it() {
        let (mut store, mut buffer) = following("abc def\n");
        let answer = [diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "bad")];
        store.publish(0, Some((Some(0), &answer)), Encoding::Utf16);
        edit(&mut store, &mut buffer, 1, 7, "g");
        edit(&mut store, &mut buffer, 2, 4, "_");
        assert_eq!(marked(&store, buffer.rope()), ["def"]);
    }

    #[test]
    fn no_answer_clears_the_marks() {
        let (mut store, _) = following("abc\n");
        let answer = [diagnostic(0, 0, 1, DiagnosticSeverity::ERROR, "bad")];
        store.publish(0, Some((Some(0), &answer)), Encoding::Utf16);
        assert!(store.publish(0, None, Encoding::Utf16));
        assert!(store.marks(0).is_empty());
    }

    #[test]
    fn an_answer_about_a_version_nobody_kept_is_waited_out() {
        let (mut store, mut buffer) = following("abc\n");
        for version in 1..=i32::try_from(MOST_VERSIONS).unwrap() + 5 {
            edit(&mut store, &mut buffer, version, 0, "x");
        }
        let answer = [diagnostic(0, 0, 1, DiagnosticSeverity::ERROR, "bad")];
        assert!(!store.publish(0, Some((Some(0), &answer)), Encoding::Utf16));
        assert!(store.marks(0).is_empty(), "rather than drawn in the wrong place");
    }

    proptest! {
        /// However the edits fall, a mark carried through them covers the
        /// same text as the same diagnostic placed against the final text —
        /// which is what a server would say once it caught up.
        #[test]
        fn marks_stay_on_the_text_they_were_placed_on(
            edits in proptest::collection::vec((0usize..60, prop_oneof![
                Just(""), Just("x"), Just("😀"), Just("中"), Just("\n"), Just("e\u{301}")
            ], 0usize..3), 1..20),
        ) {
            let (mut store, mut buffer) = following("fn main() {\n    let 😀 = TARGET;\n}\n");
            let answer = [diagnostic(1, 13, 19, DiagnosticSeverity::ERROR, "here")];
            store.publish(0, Some((Some(0), &answer)), Encoding::Utf16);
            prop_assert_eq!(marked(&store, buffer.rope()), ["TARGET"]);

            for (version, (at, text, delete)) in edits.into_iter().enumerate() {
                let at = at.min(buffer.len_chars());
                let target = buffer.rope().to_string().chars().collect::<Vec<_>>();
                let found = target.windows(6).position(|w| w.iter().collect::<String>() == "TARGET");
                let end = (at + delete).min(buffer.len_chars());
                // Edits that touch the target itself change what it is; the
                // property is about everything around it.
                if found.is_some_and(|start| end > start && at < start + 6) {
                    continue;
                }
                buffer.set_selections(Selections::single(Range::new(at, end)));
                if text.is_empty() && at != end { buffer.delete_backward() } else { buffer.insert(text) }
                let version = i32::try_from(version + 1).unwrap();
                store.changed(0, version, &buffer.take_edits(), buffer.rope());
                prop_assert_eq!(marked(&store, buffer.rope()), ["TARGET"]);
            }
        }
    }

    // ── in the editor ───────────────────────────────────────────────────────

    fn editor(text: &str) -> App {
        let mut app = App::new(
            Buffer::from_text(text),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 60, 12));
        app
    }

    /// Give the focused document `diagnostics`, as a server would.
    fn publish(app: &mut App, diagnostics: &[Diagnostic]) {
        let id = app.doc().id;
        let text = app.doc().buffer.rope().clone();
        app.diagnostics.opened(id, 0, &text);
        app.diagnostics.publish(id, Some((Some(0), diagnostics)), Encoding::Utf16);
        app.relayout();
    }

    fn press(column: u16, row: u16) -> Event {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    }

    fn draw(app: &App) -> Cells {
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        cells
    }

    fn rail_column(app: &App) -> u16 {
        app.viewport.right() - 1
    }

    #[test]
    fn diagnostics_are_underlined_in_their_severity_s_colour() {
        let mut app = editor("let x = 1;\nlet y = 2;\n");
        publish(&mut app, &[diagnostic(1, 4, 5, DiagnosticSeverity::WARNING, "unused")]);
        let cells = draw(&app);
        // The gutter is one digit and two columns of padding.
        let y = &cells[(3 + 4, 1)];
        assert!(y.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        assert_eq!(y.underline_color, app.palette.underline(Role::Warn).underline_color.unwrap());
        assert!(!cells[(3 + 5, 1)].modifier.contains(ratatui::style::Modifier::UNDERLINED));
    }

    #[test]
    fn the_status_line_counts_what_the_rail_shows() {
        let mut app = editor(&"line\n".repeat(200));
        publish(
            &mut app,
            &[
                diagnostic(10, 0, 4, DiagnosticSeverity::ERROR, "a"),
                diagnostic(11, 0, 4, DiagnosticSeverity::ERROR, "b"),
                diagnostic(12, 0, 4, DiagnosticSeverity::WARNING, "c"),
                diagnostic(150, 0, 4, DiagnosticSeverity::HINT, "d"),
            ],
        );
        let (_, text, Some(rail)) = app.pane_parts(app.panes.focus()).unwrap() else {
            panic!("a document with marks has a rail");
        };
        assert_eq!(text.right(), rail.x);
        let buckets = Rail::buckets(&app.rail_marks(app.doc()), 201, rail.height);
        let on_rail: usize = buckets.iter().map(|bucket| bucket.count).sum();
        assert_eq!(on_rail, app.diagnostics.tally(app.doc().id).total());
        assert!(buckets.iter().any(|bucket| bucket.count == 3), "three collide: {buckets:?}");

        let status = app.areas().1;
        let part = app.status_parts(status).problems.expect("the counts are in the status line");
        let cells = draw(&app);
        let drawn: String = (part.x..part.right()).map(|x| cells[(x, part.y)].symbol()).collect();
        assert_eq!(drawn.trim(), "✕ 2 ▲ 1 ● 1");
    }

    #[test]
    fn keys_step_through_the_diagnostics_and_come_round_at_the_end() {
        let mut app = editor("aaa bbb\nccc ddd\n");
        publish(
            &mut app,
            &[
                diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "first"),
                diagnostic(1, 0, 3, DiagnosticSeverity::WARNING, "second"),
            ],
        );
        let f8 = Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        app.handle(f8.clone());
        assert_eq!(app.doc().buffer.selections().primary().head, 4);
        assert!(app.card.is_some(), "the message is shown beside it");
        app.handle(f8.clone());
        assert_eq!(app.doc().buffer.selections().primary().head, 8);
        app.handle(f8);
        assert_eq!(app.doc().buffer.selections().primary().head, 4, "round again");

        app.handle(Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT)));
        assert_eq!(app.doc().buffer.selections().primary().head, 8, "and back");
    }

    #[test]
    fn with_nothing_to_step_to_it_says_so() {
        let mut app = editor("clean\n");
        app.run(Command::NextDiagnostic);
        assert_eq!(app.message(), Some("No problems in this file."));
    }

    #[test]
    fn clicking_a_crowded_rail_row_visits_each_of_its_marks_in_turn() {
        let mut app = editor(&"line\n".repeat(200));
        publish(
            &mut app,
            &[
                diagnostic(10, 0, 4, DiagnosticSeverity::ERROR, "a"),
                diagnostic(11, 0, 4, DiagnosticSeverity::ERROR, "b"),
                diagnostic(150, 0, 4, DiagnosticSeverity::WARNING, "c"),
            ],
        );
        let x = rail_column(&app);
        let row = (0..app.viewport.height)
            .find(|&y| {
                app.hits.at(x, y).is_some_and(|hit| matches!(hit.target, Target::RailMark(..)))
            })
            .expect("a mark on the rail");
        app.handle(press(x, row));
        let line =
            |app: &App| app.doc().buffer.line_of(app.doc().buffer.selections().primary().head);
        assert_eq!(line(&app), 10);
        app.handle(press(x, row));
        assert_eq!(line(&app), 11, "the second mark on the same row");
    }

    #[test]
    fn clicking_the_bare_rail_scrolls_like_a_scrollbar() {
        let mut app = editor(&"line\n".repeat(200));
        publish(&mut app, &[diagnostic(0, 0, 4, DiagnosticSeverity::ERROR, "a")]);
        let x = rail_column(&app);
        let bottom = app.areas().0.bottom() - 1;
        assert_eq!(app.hits.at(x, bottom).map(|hit| hit.target), Some(Target::Rail(0)));
        app.handle(press(x, bottom));
        assert!(app.doc().scroll > 150, "scrolled to near the end: {}", app.doc().scroll);
        assert_eq!(app.doc().buffer.selections().primary().head, 0, "the caret stayed");
    }

    #[test]
    fn resting_on_an_underline_opens_its_card_and_leaving_closes_it() {
        let mut app = editor("let x = 1;\nlet y = 2;\n");
        publish(&mut app, &[diagnostic(0, 4, 5, DiagnosticSeverity::ERROR, "expected `u8`")]);
        let now = std::time::Instant::now();
        let (x, y) = (3 + 4, 0);
        assert!(app.wants_motion(), "an underline on screen reacts to hover");
        app.handle_at(mouse(MouseEventKind::Moved, x, y), now);
        let due = app.deadline().expect("waiting for the pointer to settle");
        app.tick(due);

        let card = app.card_area().expect("a card");
        assert_eq!(card.y, y + 1, "below the underline, not over it");
        let cells = draw(&app);
        let text: String = (card.y..card.bottom())
            .flat_map(|row| (card.x..card.right()).map(move |col| (col, row)))
            .map(|at| cells[at].symbol().to_string())
            .collect();
        assert!(text.contains("expected `u8`"), "{text}");
        assert!(text.contains("error"), "{text}");

        // Into the card keeps it; out to plain text closes it.
        app.handle_at(mouse(MouseEventKind::Moved, card.x + 1, card.y), now);
        assert!(app.card.is_some());
        app.handle_at(mouse(MouseEventKind::Moved, 40, 8), now);
        assert!(app.card.is_none());
    }

    #[test]
    fn a_click_on_an_underline_still_places_the_caret() {
        let mut app = editor("let x = 1;\n");
        publish(&mut app, &[diagnostic(0, 4, 5, DiagnosticSeverity::ERROR, "bad")]);
        app.handle(press(3 + 4, 0));
        assert_eq!(app.doc().buffer.selections().primary().head, 4);
    }

    #[test]
    fn the_card_s_buttons_step_on() {
        let mut app = editor("aaa bbb\nccc ddd\n");
        publish(
            &mut app,
            &[
                diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "first"),
                diagnostic(1, 0, 3, DiagnosticSeverity::WARNING, "second"),
            ],
        );
        app.run(Command::NextDiagnostic);
        let buttons = app.card_buttons();
        assert_eq!(buttons.len(), 2);
        let next = buttons[1];
        app.handle(press(next.x + 1, next.y));
        assert_eq!(app.doc().buffer.selections().primary().head, 8);
        assert!(app.card.is_some(), "and says what is there");
    }

    #[test]
    fn escape_puts_the_card_away_and_leaves_the_selection() {
        let mut app = editor("aaa bbb\n");
        publish(&mut app, &[diagnostic(0, 4, 7, DiagnosticSeverity::ERROR, "first")]);
        app.run(Command::NextDiagnostic);
        app.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.card.is_none());
        assert_eq!(app.doc().buffer.selections().primary().head, 4);
    }

    #[test]
    fn a_file_with_no_server_and_no_marks_has_no_rail() {
        let app = editor("plain\n");
        let (_, _, rail) = app.pane_parts(app.panes.focus()).unwrap();
        assert_eq!(rail, None);
    }
}
