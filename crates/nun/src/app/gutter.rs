//! Git in the gutter: a bar beside every line that differs from what is
//! staged, a card saying what it was with buttons to put it back or stage
//! it, and a column of the rail showing where in the file the changes are.
//!
//! The hunks are worked out off this thread (see `nun_vcs`), always against
//! the buffer as it stands, so unsaved edits are marked too. Each edit is
//! sent once the typing pauses, not with every keystroke; until the answer
//! comes the marks already drawn stay where they were.
//!
//! A hunk is acted on from its card or with a key. The card's buttons act
//! on the hunk the card is about, which is not always the one at the caret:
//! a card opened by resting on a mark elsewhere is about that mark. A key
//! acts on the hunk at the caret. Either way the hunk is found again in a
//! diff of the text as it is now, so a revert puts back exactly that hunk
//! and nothing the text has moved on to since.
//!
//! Every way in has a mouse path: resting on a bar opens its card, clicking
//! it goes to it (and again goes on to the next), and the rail's column is
//! rested on and clicked like the diagnostics' beside it.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nun_theme::Role;
use nun_ui::{Change, ChangeRail, CodeBudget, EditorView, Glyph, Markdown, Paragraph, Run};
use nun_vcs::{Diff, Hunk, HunkKind, LineMark, Reply, Request};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;

use super::card::{Anchor, Card};
use super::panes::DocId;
use super::{App, Document, Focus, Outcome, Target};
use crate::commands::Command;

/// How long the typing has to pause before git is sent the text.
const SETTLE: Duration = Duration::from_millis(150);

/// The longest the marks lag behind typing that does not pause.
const MOST_LAG: Duration = Duration::from_secs(1);

/// The most of a hunk's old text a card shows. The card scrolls, but a
/// removal of a whole file is not read in one.
const MOST_OLD_LINES: usize = 200;

/// Every document git is following, and what it last said about each.
#[derive(Debug, Default)]
pub(super) struct Changes {
    docs: HashMap<DocId, Followed>,
    /// The hunk a card's button was pressed on, from the press until the
    /// command the button runs takes it.
    aimed: Option<Aim>,
    /// Whether a terminal in the panel had the keyboard, as of the last
    /// event: coming back from one is when a `git add` may have happened.
    in_terminal: bool,
}

/// One document git is following.
#[derive(Debug, Default)]
struct Followed {
    /// The version of its text last sent.
    sent: u64,
    /// The buffer's revision when it was.
    revision: u64,
    /// The revision last seen, which may not have been sent yet.
    seen: u64,
    /// When the text first changed without being sent, and when it last did.
    unsent: Option<(Instant, Instant)>,
    /// The latest hunks, and which version of the text they describe.
    /// `None` when git has nothing to compare it with.
    diff: Option<Arc<Diff>>,
    version: u64,
}

impl Followed {
    /// When the text should be sent, if it has changed.
    fn due(&self) -> Option<Instant> {
        self.unsent.map(|(first, last)| (last + SETTLE).min(first + MOST_LAG))
    }
}

/// A hunk a card is about: which document, and the hunk as it was when
/// the card was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Aim {
    doc: DocId,
    hunk: Hunk,
}

impl Changes {
    /// Take the hunk a card's button was pressed on.
    pub(super) fn aim(&mut self, aim: Option<Aim>) {
        self.aimed = aim;
    }
}

/// The changes' mark for a hunk of a text `lines` lines long.
fn change_of(hunk: &Hunk, lines: u32) -> Change {
    match hunk.kind() {
        HunkKind::Added => Change::Added,
        HunkKind::Modified => Change::Modified,
        HunkKind::Removed if hunk.after.start >= lines && lines > 0 => Change::RemovedBelow,
        HunkKind::Removed => Change::RemovedAbove,
    }
}

const fn change_of_mark(mark: LineMark) -> Change {
    match mark {
        LineMark::Added => Change::Added,
        LineMark::Modified => Change::Modified,
        LineMark::RemovedAbove => Change::RemovedAbove,
        LineMark::RemovedBelow => Change::RemovedBelow,
    }
}

/// How many lines `document` has, counted as git counts them: a final
/// newline ends a line rather than starting one.
fn git_lines(document: &Document) -> u32 {
    let rope = document.buffer.rope();
    let lines = rope.len_lines()
        - usize::from(rope.len_chars() == 0 || rope.char(rope.len_chars() - 1) == '\n');
    u32::try_from(lines).unwrap_or(u32::MAX)
}

/// The lines a hunk is marked on: its own, or for a removal the one line
/// it is shown on.
fn marked_lines(hunk: &Hunk, lines: u32) -> Range<usize> {
    let lines = if hunk.after.is_empty() {
        let at = hunk.anchor(lines);
        at..at + 1
    } else {
        hunk.after.clone()
    };
    lines.start as usize..lines.end as usize
}

/// Lines `range` as a person reads them.
fn place(range: &Range<usize>) -> String {
    if range.len() <= 1 {
        format!("line {}", range.start + 1)
    } else {
        format!("lines {}–{}", range.start + 1, range.end)
    }
}

/// `count` lines, in words.
fn lines_word(count: usize) -> String {
    if count == 1 { "1 line".into() } else { format!("{count} lines") }
}

impl App {
    // ── following documents ─────────────────────────────────────────────────

    /// Follow a document in git, or follow it afresh: it arrived, was
    /// reloaded, or was saved under another name. What was marked stays
    /// until git answers about the new text, so nothing jumps meanwhile.
    pub(super) fn vcs_open(&mut self, id: DocId) {
        let Some(vcs) = self.vcs.as_ref() else { return };
        let Some(document) = self.docs.iter().find(|document| document.id == id) else { return };
        let Some(path) = document.buffer.path() else {
            if self.changes.docs.remove(&id).is_some() {
                vcs.send(Request::Close(id));
            }
            return;
        };
        let followed = self.changes.docs.entry(id).or_default();
        followed.sent += 1;
        followed.revision = document.buffer.revision();
        followed.seen = followed.revision;
        followed.unsent = None;
        // Absolute, because git is found from the file's folder upwards, and
        // a file named on the command line is held as it was typed.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        vcs.send(Request::Open {
            id,
            version: followed.sent,
            path,
            text: document.buffer.rope().clone(),
        });
    }

    /// Stop following a document that has gone.
    pub(super) fn vcs_close(&mut self, id: DocId) {
        if self.changes.docs.remove(&id).is_some()
            && let Some(vcs) = self.vcs.as_ref()
        {
            vcs.send(Request::Close(id));
        }
    }

    /// Notice what an event did: text that changed is sent once the typing
    /// pauses, and coming back from a terminal in the panel — where a commit
    /// or an add may just have happened — asks git to look again.
    pub(super) fn vcs_follow(&mut self, now: Instant) {
        for document in &self.docs {
            let Some(followed) = self.changes.docs.get_mut(&document.id) else { continue };
            let revision = document.buffer.revision();
            if revision != followed.seen {
                followed.seen = revision;
                let first = followed.unsent.map_or(now, |(first, _)| first);
                followed.unsent = Some((first, now));
            }
        }
        let in_terminal = self.focus == Focus::Terminal;
        if std::mem::replace(&mut self.changes.in_terminal, in_terminal) && !in_terminal {
            self.refresh_status();
        }
    }

    /// When the next document's text is due to be sent.
    pub(super) fn vcs_deadline(&self) -> Option<Instant> {
        self.changes.docs.values().filter_map(Followed::due).min()
    }

    /// Send every text whose typing has paused.
    pub(super) fn vcs_tick(&mut self, now: Instant) -> Outcome {
        let due: Vec<DocId> = self
            .changes
            .docs
            .iter()
            .filter(|(_, followed)| followed.due().is_some_and(|due| due <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.vcs_send(id);
        }
        Outcome::Continue
    }

    /// Send a document's text now, if it has changed since it was last sent.
    fn vcs_send(&mut self, id: DocId) {
        let Some(vcs) = self.vcs.as_ref() else { return };
        let Some(document) = self.docs.iter().find(|document| document.id == id) else { return };
        let Some(followed) = self.changes.docs.get_mut(&id) else { return };
        followed.unsent = None;
        let revision = document.buffer.revision();
        followed.seen = revision;
        if revision == followed.revision {
            return;
        }
        followed.sent += 1;
        followed.revision = revision;
        vcs.send(Request::Update {
            id,
            version: followed.sent,
            text: document.buffer.rope().clone(),
        });
    }

    /// Something git said about a document, if it is for the gutter; the
    /// reply back, for someone else, if it is not.
    pub(super) fn gutter_reply(&mut self, reply: Reply) -> Result<Outcome, Reply> {
        match reply {
            Reply::Hunks { id, version, diff } => Ok(self.vcs_hunks(id, version, diff)),
            Reply::Staged { result, .. } => Ok(self.vcs_staged(result)),
            other => Err(other),
        }
    }

    /// Git's hunks for a document.
    fn vcs_hunks(&mut self, id: DocId, version: u64, diff: Option<Arc<Diff>>) -> Outcome {
        let Some(followed) = self.changes.docs.get_mut(&id) else { return Outcome::Continue };
        // An answer about a text older than the one shown, or one from
        // before a reopen that has not been answered yet.
        if version < followed.version || version > followed.sent {
            return Outcome::Continue;
        }
        followed.version = version;
        if followed.diff == diff {
            return Outcome::Continue;
        }
        followed.diff = diff;
        Outcome::Redraw
    }

    /// What staging a hunk came to.
    fn vcs_staged(&mut self, result: Result<(), String>) -> Outcome {
        match result {
            Ok(()) => {
                self.message = Some("Staged.".into());
                self.refresh_status();
            }
            Err(why) => self.message = Some(format!("Could not stage that: {why}")),
        }
        Outcome::Redraw
    }

    /// The hunks of `id` as they are now: git's latest answer when it is
    /// about the text on screen, or else diffed here against the same base,
    /// with the text sent on so git's next answer agrees.
    fn current_diff(&mut self, id: DocId) -> Option<Arc<Diff>> {
        self.vcs_send(id);
        let document = self.docs.iter().find(|document| document.id == id)?;
        let followed = self.changes.docs.get_mut(&id)?;
        let diff = followed.diff.as_ref()?;
        if followed.version == followed.sent {
            return Some(Arc::clone(diff));
        }
        let fresh = Arc::new(Diff::of_rope(diff.base_shared(), document.buffer.rope()));
        followed.diff = Some(Arc::clone(&fresh));
        followed.version = followed.sent;
        Some(fresh)
    }

    /// Whether git follows `doc`: the rail has a column for its changes
    /// while it does, whether or not there are any, so the column does not
    /// come and go as they do.
    pub(super) fn has_change_rail(&self, doc: &Document) -> bool {
        self.changes.docs.get(&doc.id).is_some_and(|followed| followed.diff.is_some())
    }

    /// `doc`'s hunks as last diffed, if git follows it.
    fn diff_of(&self, doc: DocId) -> Option<&Arc<Diff>> {
        self.changes.docs.get(&doc)?.diff.as_ref()
    }

    // ── drawing ─────────────────────────────────────────────────────────────

    /// Split a pane's text area into the text, the changes' column of the
    /// rail, and the diagnostics' column, each only where there is one. The
    /// diagnostics keep the right edge; the changes go just inside them.
    pub(super) fn rails_of(
        &self,
        doc: &Document,
        text: Rect,
    ) -> (Rect, Option<Rect>, Option<Rect>) {
        let gutter = EditorView::new(&doc.buffer, &self.palette).gutter_width();
        let carve = |text: &mut Rect, wanted: bool| {
            if !wanted || text.width <= gutter.saturating_add(2) {
                return None;
            }
            text.width -= 1;
            Some(Rect { x: text.right(), width: 1, ..*text })
        };
        let mut text = text;
        let rail = carve(&mut text, self.has_rail(doc));
        let changes = carve(&mut text, self.has_change_rail(doc));
        (text, changes, rail)
    }

    /// The gutter's marks for the lines of `doc` in the first `height` rows
    /// of its view.
    pub(super) fn change_marks(&self, doc: &Document, height: u16) -> Vec<(usize, Change)> {
        let Some(diff) = self.diff_of(doc.id) else { return Vec::new() };
        let hidden = doc.buffer.hidden();
        let Some(last) = hidden.from(doc.scroll).take(usize::from(height)).last() else {
            return Vec::new();
        };
        let range = u32::try_from(doc.scroll).unwrap_or(u32::MAX)
            ..u32::try_from(last + 1).unwrap_or(u32::MAX);
        diff.marks(range).map(|(line, mark)| (line as usize, change_of_mark(mark))).collect()
    }

    /// Every hunk of `doc`, as the rail's column draws it.
    fn rail_changes(&self, doc: &Document) -> Vec<(Range<usize>, Change)> {
        let Some(diff) = self.diff_of(doc.id) else { return Vec::new() };
        let lines = git_lines(doc);
        diff.hunks()
            .iter()
            .map(|hunk| (marked_lines(hunk, lines), change_of(hunk, lines)))
            .collect()
    }

    /// Draw a pane's changes column, beside `text`.
    pub(super) fn render_change_rail(
        &self,
        pane: usize,
        doc: &Document,
        text: Rect,
        column: Rect,
        cells: &mut Cells,
    ) {
        use ratatui::widgets::Widget as _;

        let hovered = match self.hover.current() {
            Some(Target::ChangeRailMark(over, row)) if over == pane => Some(row),
            _ => None,
        };
        let view = EditorView::new(&doc.buffer, &self.palette).scrolled_to(doc.scroll);
        let first = view.line_at_row(0).unwrap_or(doc.scroll);
        let last = (0..usize::from(text.height)).rev().find_map(|row| view.line_at_row(row));
        ChangeRail::new(&self.rail_changes(doc), doc.buffer.len_lines(), &self.palette)
            .viewing(first..last.map_or(first, |last| last + 1))
            .hovered(hovered)
            .render(column, cells);
    }

    /// The document in `pane`, its text area, and its changes column.
    fn change_parts(&self, pane: usize) -> Option<(&Document, Rect, Option<Rect>)> {
        let id = self.panes.get(pane)?.current()?;
        let doc = self.doc_by(id)?;
        let (text, changes, _) = self.rails_of(doc, self.text_area_of(pane)?);
        Some((doc, text, changes))
    }

    /// Put every change bar in view, and the changes column, into the hit
    /// map: a bar is something to rest on or click, and so is a row of the
    /// column with changes on it.
    pub(super) fn layout_changes(&self, hits: &mut nun_input::HitMap<Target>) {
        for (pane, _) in self.panes.rects(self.panes_area()) {
            let Some((doc, text, column)) = self.change_parts(pane) else { continue };
            let Some(diff) = self.diff_of(doc.id) else { continue };
            let view = EditorView::new(&doc.buffer, &self.palette);
            let x = text.x + view.change_column();
            if view.change_column() < text.width {
                for (row, line) in
                    doc.buffer.hidden().from(doc.scroll).take(usize::from(text.height)).enumerate()
                {
                    let Ok(line) = u32::try_from(line) else { break };
                    let (Some(index), Ok(row)) = (diff.hunk_index_at(line), u16::try_from(row))
                    else {
                        continue;
                    };
                    let cell = Rect::new(x, text.y + row, 1, 1);
                    hits.push(super::cells(cell), Target::Change(pane, index), true);
                }
            }
            if let Some(column) = column {
                hits.push(super::cells(column), Target::ChangeRail(pane), false);
                let rows = ChangeRail::rows(
                    &self.rail_changes(doc),
                    doc.buffer.len_lines(),
                    column.height,
                );
                for (row, _) in rows {
                    let cell = Rect { y: column.y + row, height: 1, ..column };
                    hits.push(super::cells(cell), Target::ChangeRailMark(pane, row), true);
                }
            }
        }
    }

    // ── cards ───────────────────────────────────────────────────────────────

    /// What a card about `hunk` of `doc` says: what the hunk is, and the
    /// lines it replaced, highlighted as the file is.
    fn hunk_body(&self, doc: &Document, diff: &Diff, hunk: &Hunk) -> Vec<Paragraph> {
        let lines = marked_lines(hunk, git_lines(doc));
        let was = hunk.before.len();
        let (what, change) = match hunk.kind() {
            HunkKind::Added => (format!("Added {}", place(&lines)), Change::Added),
            HunkKind::Modified => (format!("Changed {}. It was:", place(&lines)), Change::Modified),
            HunkKind::Removed => {
                let change = change_of(hunk, git_lines(doc));
                let side = if change == Change::RemovedBelow { "below" } else { "above" };
                (format!("Removed {} {side} {}:", lines_word(was), place(&lines)), change)
            }
        };
        let mut body = vec![vec![
            Run::bold(format!("{} ", self.palette.glyph(change.glyph())), change.role()),
            Run::new(what, Role::Text),
        ]];
        if hunk.before.is_empty() {
            return body;
        }
        let old = diff.previous_text(hunk);
        let mut shown: String = old.split_inclusive('\n').take(MOST_OLD_LINES).collect();
        let more = old.split_inclusive('\n').count().saturating_sub(MOST_OLD_LINES);
        if shown.ends_with('\n') {
            shown.pop();
        }
        body.push(Vec::new());
        let old = match Self::language_of(doc) {
            Some(language) => Markdown::code(language, &shown, &mut CodeBudget::new()),
            None => Markdown::plain(&shown),
        };
        body.extend(old.body);
        if more > 0 {
            body.push(vec![Run::new(format!("…and {} more", lines_word(more)), Role::Dim)]);
        }
        body
    }

    /// The card's buttons: the hunk's actions, and the way on to the others
    /// when there are any.
    fn hunk_buttons(&self, diff: &Diff) -> Vec<(String, Command)> {
        let mut buttons = vec![
            ("Revert".to_string(), Command::RevertHunk),
            ("Stage".to_string(), Command::StageHunk),
        ];
        if diff.hunks().len() > 1 {
            buttons.push((
                format!("{} Previous", self.palette.glyph(Glyph::CardPrevious)),
                Command::PreviousHunk,
            ));
            buttons
                .push((format!("Next {}", self.palette.glyph(Glyph::CardNext)), Command::NextHunk));
        }
        buttons
    }

    /// Show the card about hunk `index` of `doc`, beside `anchor` or, with
    /// none, beside the hunk's first line in `pane`.
    fn show_hunk_card(
        &mut self,
        pane: usize,
        doc: DocId,
        index: usize,
        anchor: Option<Anchor>,
        opener: Option<Target>,
    ) -> Outcome {
        let Some(diff) = self.diff_of(doc).cloned() else { return Outcome::Continue };
        let Some(document) = self.doc_by(doc) else { return Outcome::Continue };
        let Some(hunk) = diff.hunks().get(index).cloned() else { return Outcome::Continue };
        let anchor = anchor.unwrap_or_else(|| {
            let line = marked_lines(&hunk, git_lines(document)).start;
            let at = document.buffer.line_start(line.min(document.buffer.len_lines() - 1));
            Anchor::Text { pane, doc, from: at, to: at }
        });
        let body = self.hunk_body(document, &diff, &hunk);
        let buttons = self.hunk_buttons(&diff);
        self.show_card(Card::new(anchor, body, buttons, opener).about_hunk(Aim { doc, hunk }));
        Outcome::Redraw
    }

    /// The pointer rested on a change bar or a row of the changes column.
    pub(super) fn change_dwelt(&mut self, target: Target) -> Outcome {
        match target {
            Target::Change(pane, index) => {
                let Some(doc) = self.panes.get(pane).and_then(super::panes::Pane::current) else {
                    return Outcome::Continue;
                };
                self.show_hunk_card(pane, doc, index, None, Some(target))
            }
            Target::ChangeRailMark(pane, row) => {
                let Some((doc, _, Some(column))) = self.change_parts(pane) else {
                    return Outcome::Continue;
                };
                let id = doc.id;
                let Some(&index) = self.hunks_on_row(doc, row, column.height).first() else {
                    return Outcome::Continue;
                };
                let anchor = Anchor::Screen(Rect { y: column.y + row, height: 1, ..column });
                self.show_hunk_card(pane, id, index, Some(anchor), Some(target))
            }
            _ => Outcome::Continue,
        }
    }

    /// Which of `doc`'s hunks are drawn on `row` of a column `height` tall.
    fn hunks_on_row(&self, doc: &Document, row: u16, height: u16) -> Vec<usize> {
        let lines = nun_ui::Rail::lines_at(row, doc.buffer.len_lines(), height);
        self.rail_changes(doc)
            .iter()
            .enumerate()
            .filter(|(_, (range, _))| {
                range.start < lines.end && lines.start < range.end.max(range.start + 1)
            })
            .map(|(index, _)| index)
            .collect()
    }

    // ── going to them ───────────────────────────────────────────────────────

    /// Put the caret on hunk `index` of the focused document and show its
    /// card.
    fn go_to_hunk(&mut self, index: usize) -> Outcome {
        let id = self.doc().id;
        let Some(diff) = self.diff_of(id).cloned() else { return Outcome::Continue };
        let Some(hunk) = diff.hunks().get(index) else { return Outcome::Continue };
        self.focus = Focus::Editor;
        let line = marked_lines(hunk, git_lines(self.doc())).start;
        let buffer = &self.doc().buffer;
        let at = buffer.line_start(line.min(buffer.len_lines() - 1));
        self.doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
        self.follow_caret();
        self.show_hunk_card(self.panes.focus(), id, index, None, None)
    }

    /// Say why there is no hunk to act on, and `None`; or the focused
    /// document's current hunks.
    fn followed_diff(&mut self) -> Option<Arc<Diff>> {
        let id = self.doc().id;
        match self.current_diff(id) {
            Some(diff) if diff.is_empty() => {
                self.message = Some("Nothing in this file differs from what is staged.".into());
                None
            }
            Some(diff) => Some(diff),
            None => {
                self.message = Some("Git is not following this file.".into());
                None
            }
        }
    }

    /// Run one of the commands about the change at the caret.
    pub(super) fn on_hunk(&mut self, command: Command) -> Outcome {
        match command {
            Command::NextHunk => self.step_hunk(true),
            Command::PreviousHunk => self.step_hunk(false),
            Command::ShowHunk => self.show_hunk(),
            Command::RevertHunk => self.revert_hunk(),
            Command::StageHunk => self.stage_hunk(),
            _ => Outcome::Continue,
        }
    }

    /// Go to the next hunk after the caret's line, or the one before it,
    /// coming round at the ends.
    fn step_hunk(&mut self, forward: bool) -> Outcome {
        let Some(diff) = self.followed_diff() else { return Outcome::Redraw };
        let buffer = &self.doc().buffer;
        let line =
            u32::try_from(buffer.line_of(buffer.selections().primary().head)).unwrap_or(u32::MAX);
        let hunk = if forward { diff.next_hunk(line) } else { diff.previous_hunk(line) };
        let Some(index) = hunk.and_then(|hunk| diff.hunks().iter().position(|h| h == hunk)) else {
            return Outcome::Redraw;
        };
        self.go_to_hunk(index)
    }

    /// Show the card of the hunk on the caret's line.
    fn show_hunk(&mut self) -> Outcome {
        let Some(diff) = self.followed_diff() else { return Outcome::Redraw };
        let buffer = &self.doc().buffer;
        let line =
            u32::try_from(buffer.line_of(buffer.selections().primary().head)).unwrap_or(u32::MAX);
        let Some(index) = diff.hunk_index_at(line) else {
            self.message = Some("No change on this line.".into());
            return Outcome::Redraw;
        };
        let id = self.doc().id;
        self.show_hunk_card(self.panes.focus(), id, index, None, None)
    }

    /// The hunk a command acts on — the one a card's button was pressed on,
    /// or the one on the caret's line — found in a diff of the text as it
    /// is now, with the diff. `None`, having said why, when there is none.
    fn aimed_hunk(&mut self) -> Option<(DocId, Arc<Diff>, Hunk)> {
        if let Some(Aim { doc, hunk }) = self.changes.aimed.take() {
            let diff = self.current_diff(doc)?;
            if diff.hunks().contains(&hunk) {
                return Some((doc, diff, hunk));
            }
            self.message = Some("That change is not there any more.".into());
            return None;
        }
        let diff = self.followed_diff()?;
        let buffer = &self.doc().buffer;
        let line =
            u32::try_from(buffer.line_of(buffer.selections().primary().head)).unwrap_or(u32::MAX);
        let Some(hunk) = diff.hunk_at(line).cloned() else {
            self.message = Some("No change on this line.".into());
            return None;
        };
        Some((self.doc().id, diff, hunk))
    }

    /// Put a hunk back the way the staged version has it, as one step undo
    /// takes back.
    fn revert_hunk(&mut self) -> Outcome {
        let Some((id, diff, hunk)) = self.aimed_hunk() else { return Outcome::Redraw };
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return Outcome::Redraw;
        };
        let edit = diff.revert(&hunk, document.buffer.rope());
        match document.buffer.apply_batch(vec![edit]) {
            Ok(_) => {
                self.message = Some("Reverted. Undo brings it back.".into());
                self.vcs_send(id);
                if id == self.doc().id {
                    self.follow_caret();
                }
            }
            Err(error) => self.message = Some(format!("Could not revert that: {error}")),
        }
        Outcome::Redraw
    }

    /// Write a hunk into the index, leaving the file as it is.
    fn stage_hunk(&mut self) -> Outcome {
        let Some((id, _, hunk)) = self.aimed_hunk() else { return Outcome::Redraw };
        let (Some(vcs), Some(followed)) = (self.vcs.as_ref(), self.changes.docs.get(&id)) else {
            return Outcome::Redraw;
        };
        vcs.send(Request::Stage { id, version: followed.sent, hunk });
        Outcome::Redraw
    }

    // ── the mouse ───────────────────────────────────────────────────────────

    /// A press on a change bar or the changes column. `y` is the screen row
    /// pressed.
    pub(super) fn changes_press(&mut self, target: Target, y: u16) -> Outcome {
        match target {
            Target::Change(pane, index) => {
                self.panes.set_focus(pane);
                // Pressed again with the caret already there, it goes on.
                let caret_in = {
                    let buffer = &self.doc().buffer;
                    let line = u32::try_from(buffer.line_of(buffer.selections().primary().head))
                        .unwrap_or(u32::MAX);
                    self.diff_of(self.doc().id).and_then(|diff| diff.hunk_index_at(line))
                };
                if caret_in == Some(index) {
                    return self.step_hunk(true);
                }
                self.go_to_hunk(index)
            }
            Target::ChangeRailMark(pane, row) => {
                let Some((doc, _, Some(column))) = self.change_parts(pane) else {
                    return Outcome::Continue;
                };
                let here = self.hunks_on_row(doc, row, column.height);
                let Some(&first) = here.first() else { return Outcome::Continue };
                self.panes.set_focus(pane);
                let caret = self.doc().buffer.selections().primary().head;
                let line = self.doc().buffer.line_of(caret);
                let changes = self.rail_changes(self.doc());
                let next = here
                    .iter()
                    .copied()
                    .find(|&index| changes[index].0.start > line)
                    .unwrap_or(first);
                self.go_to_hunk(next)
            }
            Target::ChangeRail(pane) => self.change_rail_press(pane, y),
            _ => Outcome::Continue,
        }
    }

    /// A click on the changes column away from its marks scrolls there, as
    /// the diagnostics' column beside it does.
    fn change_rail_press(&mut self, pane: usize, y: u16) -> Outcome {
        let Some((doc, text, Some(column))) = self.change_parts(pane) else {
            return Outcome::Continue;
        };
        if !(column.top()..column.bottom()).contains(&y) {
            return Outcome::Continue;
        }
        let lines = doc.buffer.len_lines();
        let line = nun_ui::Rail::lines_at(y - column.top(), lines, column.height)
            .start
            .min(lines.saturating_sub(1));
        let hidden = doc.buffer.hidden();
        let half = isize::try_from(text.height / 2).unwrap_or(0);
        let scroll = hidden.step(hidden.in_view(line), -half);
        let id = doc.id;
        if let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) {
            doc.scroll = scroll;
        }
        Outcome::Redraw
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command as Process;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_core::Buffer;
    use nun_lsp::Encoding;
    use nun_lsp::types::{Diagnostic, DiagnosticSeverity, Position, Range as LspRange};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use tempfile::TempDir;

    use super::*;
    use crate::commands::{KeySet, defaults};

    const COMMITTED: &str = "one\ntwo\nthree\nfour\nfive\nsix\n";

    /// Git with nothing from the environment, which a hook run sets.
    fn git(dir: &Path, args: &[&str]) -> Option<String> {
        let mut git = Process::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                git.env_remove(name);
            }
        }
        let out = git
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=nun", "-c", "user.email=nun@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// An editor on `a.txt`, committed as [`COMMITTED`], with git attached;
    /// `None` where there is no git to make the repository with.
    fn editor() -> Option<(App, Receiver<Reply>, TempDir)> {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"])?;
        let path = dir.path().join("a.txt");
        std::fs::write(&path, COMMITTED).unwrap();
        git(dir.path(), &["add", "a.txt"])?;
        git(dir.path(), &["commit", "-qm", "a"])?;
        let (buffer, _) = Buffer::load(&path).unwrap();
        let mut app =
            App::new(buffer, Palette::new(derive(&Probe::builtin_dark())), defaults(KeySet::Full));
        app.set_viewport(Rect::new(0, 0, 60, 12));
        let (send, replies) = mpsc::channel();
        app.attach_vcs(nun_vcs::Vcs::new(Box::new(move |reply| {
            let _ = send.send(reply);
        })));
        let mut app = app;
        settle(&mut app, &replies);
        Some((app, replies, dir))
    }

    /// Send whatever is due, and hand the editor everything git says until
    /// it has caught up.
    fn settle(app: &mut App, replies: &Receiver<Reply>) {
        if let Some(due) = app.vcs_deadline() {
            app.tick(due);
        }
        app.vcs.as_ref().unwrap().send(Request::Echo(7));
        loop {
            let reply = replies.recv_timeout(Duration::from_secs(20)).expect("git answers");
            if reply == Reply::Echo(7) {
                return;
            }
            app.handle(Event::Vcs(reply));
        }
    }

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(Event::Key(KeyEvent::new(code, modifiers)));
    }

    fn type_at(app: &mut App, at: usize, text: &str) {
        app.doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
        for ch in text.chars() {
            key(app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.handle(Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
        mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    }

    fn draw(app: &App) -> Cells {
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        cells
    }

    /// The screen row the first line in view is drawn on.
    fn top(app: &App) -> u16 {
        app.text_area_of(0).unwrap().y
    }

    fn text(app: &App) -> String {
        app.doc().buffer.rope().to_string()
    }

    fn marks(app: &App) -> Vec<(usize, Change)> {
        app.change_marks(app.doc(), 10)
    }

    #[test]
    fn unsaved_edits_are_marked_once_the_typing_pauses_each_kind_its_own_way() {
        let Some((mut app, replies, _dir)) = editor() else { return };
        assert!(marks(&app).is_empty(), "nothing changed yet");
        assert!(app.has_change_rail(app.doc()), "but git follows the file");

        type_at(&mut app, 4, "2");
        assert!(app.vcs_deadline().is_some(), "the text waits for the typing to pause");
        settle(&mut app, &replies);
        assert_eq!(marks(&app), [(1, Change::Modified)]);

        // A new line at the end, and `four` gone.
        let end = text(&app).len();
        type_at(&mut app, end, "seven\n");
        let four = text(&app).find("four").unwrap();
        app.doc_mut().buffer.apply_batch(vec![nun_core::Edit::delete(four, four + 5)]).unwrap();
        app.handle(Event::Focus(false));
        settle(&mut app, &replies);
        assert_eq!(
            marks(&app),
            [(1, Change::Modified), (3, Change::RemovedAbove), (5, Change::Added)]
        );

        let cells = draw(&app);
        let column = EditorView::new(&app.doc().buffer, &app.palette).change_column();
        for (line, change) in marks(&app) {
            let cell = &cells[(column, top(&app) + u16::try_from(line).unwrap())];
            assert_eq!(cell.symbol(), app.palette.glyph(change.glyph()));
            assert_eq!(cell.fg, app.palette.fg(change.role()).fg.unwrap());
        }
    }

    #[test]
    fn a_file_named_relative_to_where_nun_started_is_followed_too() {
        let Some((mut app, replies, dir)) = editor() else { return };
        // `nun a.txt`, from wherever the tests run, spelled with `..`s.
        let here = std::env::current_dir().unwrap();
        let mut relative = std::path::PathBuf::new();
        for _ in here.components().skip(1) {
            relative.push("..");
        }
        relative.push(dir.path().join("a.txt").strip_prefix("/").unwrap());
        assert!(relative.is_relative());
        app.doc_mut().buffer.set_path(&relative);
        app.vcs_open(app.doc().id);
        type_at(&mut app, 4, "2");
        settle(&mut app, &replies);
        assert_eq!(marks(&app), [(1, Change::Modified)]);
    }

    #[test]
    fn revert_puts_back_exactly_the_hunk_and_undo_brings_it_back() {
        let Some((mut app, replies, _dir)) = editor() else { return };
        type_at(&mut app, 4, "TWO ");
        let end = text(&app).len();
        type_at(&mut app, end, "seven\n");
        settle(&mut app, &replies);
        let edited = text(&app);
        assert_eq!(edited, "one\nTWO two\nthree\nfour\nfive\nsix\nseven\n");

        // On line 2, by key: only that hunk goes.
        type_at(&mut app, 5, "");
        key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(text(&app), "one\ntwo\nthree\nfour\nfive\nsix\nseven\n");
        settle(&mut app, &replies);
        assert_eq!(marks(&app), [(6, Change::Added)], "the other hunk is still there");

        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(text(&app), edited, "one undo step");
    }

    #[test]
    fn resting_on_a_bar_offers_its_old_text_and_stage_writes_only_the_index() {
        let Some((mut app, replies, dir)) = editor() else { return };
        type_at(&mut app, 4, "TWO ");
        let end = text(&app).len();
        type_at(&mut app, end, "seven\n");
        settle(&mut app, &replies);
        // The caret is on the last line; the card is about line 2.
        let column = EditorView::new(&app.doc().buffer, &app.palette).change_column();
        let now = Instant::now();
        app.handle_at(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row: top(&app) + 1,
                modifiers: KeyModifiers::NONE,
            }),
            now,
        );
        app.tick(app.deadline().expect("waiting for the pointer to rest"));
        let area = app.card_area().expect("a card about the change");
        let cells = draw(&app);
        let shown: String = (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
            .map(|at| cells[at].symbol().to_string())
            .collect();
        assert!(shown.contains("Changed line 2. It was:"), "{shown}");
        assert!(shown.contains("two") && shown.contains("Revert") && shown.contains("Stage"));

        let stage = app.card_buttons()[1];
        click(&mut app, stage.x, stage.y);
        settle(&mut app, &replies);
        assert_eq!(app.message(), Some("Staged."));
        let cached = git(dir.path(), &["diff", "--cached"]).unwrap();
        assert!(cached.contains("+TWO two") && !cached.contains("seven"), "{cached}");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), COMMITTED);
        assert_eq!(
            text(&app),
            "one\nTWO two\nthree\nfour\nfive\nsix\nseven\n",
            "the buffer is as it was"
        );
        assert_eq!(marks(&app), [(6, Change::Added)], "what was staged is no longer marked");
    }

    #[test]
    fn keys_step_between_changes_and_a_click_on_a_bar_goes_to_it() {
        let Some((mut app, replies, _dir)) = editor() else { return };
        type_at(&mut app, 0, "ZERO\n");
        let end = text(&app).len();
        type_at(&mut app, end, "seven\n");
        settle(&mut app, &replies);
        key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
        let line =
            |app: &App| app.doc().buffer.line_of(app.doc().buffer.selections().primary().head);
        assert_eq!(line(&app), 0, "round from the end to the first");
        assert!(app.card.is_some(), "and says what it is");
        key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
        assert_eq!(line(&app), 7);

        let column = EditorView::new(&app.doc().buffer, &app.palette).change_column();
        let row = top(&app);
        click(&mut app, column, row);
        assert_eq!(line(&app), 0);
        click(&mut app, column, row);
        assert_eq!(line(&app), 7, "again, and it goes on to the next");
    }

    #[test]
    fn the_rail_shows_changes_and_problems_side_by_side() {
        let Some((mut app, replies, _dir)) = editor() else { return };
        type_at(&mut app, 4, "TWO ");
        settle(&mut app, &replies);
        let id = app.doc().id;
        let rope = app.doc().buffer.rope().clone();
        app.diagnostics.opened(id, 0, &rope);
        let problem = Diagnostic {
            range: LspRange::new(Position::new(1, 0), Position::new(1, 3)),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "bad".into(),
            ..Diagnostic::default()
        };
        app.diagnostics.publish(id, Some((Some(0), &[problem])), Encoding::Utf16);
        app.relayout();

        let whole = app.text_area_of(0).unwrap();
        let (text_area, changes, rail) = app.rails_of(app.doc(), whole);
        let (changes, rail) = (changes.unwrap(), rail.unwrap());
        assert_eq!((changes.x, rail.x), (whole.right() - 2, whole.right() - 1));
        assert_eq!(text_area.right(), changes.x);

        // Both on the row for line 2, each in its own colour.
        let cells = draw(&app);
        let row = whole.y + 1;
        assert_eq!(cells[(changes.x, row)].fg, app.palette.fg(Role::Changed).fg.unwrap());
        assert_eq!(cells[(rail.x, row)].fg, app.palette.fg(Role::Error).fg.unwrap());
        assert_eq!(
            app.hits.at(changes.x, row).map(|hit| hit.target),
            Some(Target::ChangeRailMark(0, 1))
        );
        assert_eq!(app.hits.at(rail.x, row).map(|hit| hit.target), Some(Target::RailMark(0, 1)));
    }
}
