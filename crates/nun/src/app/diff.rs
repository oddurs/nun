//! The diff view: a file's changes against the index or `HEAD`, in its pane.
//!
//! The view takes the place of a document's text in the pane it was opened
//! from, rather than opening a tab or a split of its own. It is a way of
//! looking at that document, not a document: the tab strip still names the
//! file, closing the view gives the pane its text back exactly as it was,
//! and a pane is never left holding something that is not a file. A split
//! would also halve the width that side-by-side needs most; anyone who wants
//! the text beside the diff splits first and opens the diff in one half,
//! and the diff follows the edits made in the other.
//!
//! What is compared is the buffer as it is now, unsaved edits included. The
//! view keeps its own copy of the document in git's thread, under an id of
//! its own, so it asks for exactly the comparison it shows and never depends
//! on what the gutter has asked for: the text is sent when typing settles,
//! and the comparison comes back as a message, matched to the question by a
//! serial so a stale answer is dropped. Both versions are highlighted by the
//! parser, again under ids of their own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nun_core::{Range, Selections};
use nun_ui::{DiffHunk, DiffLayout, DiffRow, DiffSide, DiffSpot, DiffView, Emphasis};
use nun_vcs::{Against, Diff, HunkKind, Reply, Request};
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ropey::Rope;

use super::panes::DocId;
use super::{App, Focus, Outcome, Target};
use crate::commands::{self, Command};

/// The view's own document in git's thread. Ids count up from zero for open
/// documents, so the top of the range is free.
pub(super) const VCS_ID: nun_vcs::DocId = u32::MAX - 1;

/// The parser's ids for the two versions the view draws.
const SYNTAX_BEFORE: nun_syntax::DocId = u32::MAX - 2;
const SYNTAX_AFTER: nun_syntax::DocId = u32::MAX - 3;

/// How long after an edit the text is compared again: a pause in typing.
const SETTLE: Duration = Duration::from_millis(150);

/// A hunk longer than this, on either side, is washed as whole lines without
/// working out which words changed: a word diff of a rewritten file is slow
/// and says nothing a whole-line wash does not.
const MOST_INLINE_LINES: usize = 200;

/// The same for chars, on either side of one hunk — a minified file has few
/// lines and very long ones — and across every hunk of one diff.
const MOST_INLINE_CHARS: usize = 20_000;
const MOST_INLINE_TOTAL: usize = 200_000;

/// Rows a wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// The diff view, when one is open.
#[derive(Debug, Default)]
pub(super) struct Diffing {
    view: Option<View>,
    /// How many comparisons have been asked for, ever.
    serials: u64,
    /// How many versions of the text have been sent, ever: carried on from
    /// one view to the next, so an answer about the last one is never taken
    /// for one about this.
    versions: u64,
}

/// An open diff view.
#[derive(Debug)]
struct View {
    /// The pane it is in, and the document it is of.
    pane: usize,
    doc: DocId,
    /// Where git was told the file is, and the buffer's own spelling of it,
    /// which is what is watched for a change.
    path: std::path::PathBuf,
    spelled: std::path::PathBuf,
    against: Against,
    layout: DiffLayout,
    /// The first row shown.
    scroll: usize,
    /// The hunk the keyboard is on.
    current: Option<usize>,
    /// The version of the text git was last sent, and the text.
    version: u64,
    sent: Rope,
    /// When the text changed since, the time to send it.
    due: Option<Instant>,
    /// The comparison waited for.
    serial: u64,
    /// The hunks against the index as of `version`: which of the shown
    /// hunks can be staged as they are.
    index: Option<Arc<Diff>>,
    /// What is on screen.
    shown: Shown,
}

/// What the view is showing.
#[derive(Debug)]
enum Shown {
    /// The first answer is not in yet.
    Waiting,
    /// There is nothing to compare with, and why.
    Nothing(&'static str),
    /// A diff.
    Diff(Box<Drawn>),
}

/// A diff, laid out.
#[derive(Debug)]
struct Drawn {
    diff: Arc<Diff>,
    /// The version of the text it is of.
    version: u64,
    before: Rope,
    after: Rope,
    hunks: Vec<DiffHunk>,
    rows: Vec<DiffRow>,
    emphasis: (Emphasis, Emphasis),
    /// Highlight runs for each side, and the parse each is waited from.
    spans: (Vec<nun_syntax::Span>, Vec<nun_syntax::Span>),
    parse: (u64, u64),
}

impl App {
    // ── opening and closing ──────────────────────────────────────────────────

    /// One of the diff's commands.
    pub(super) fn diff_command(&mut self, action: commands::Diff) -> Outcome {
        match action {
            commands::Diff::Toggle => self.toggle_diff(),
            commands::Diff::ToggleLayout => self.toggle_diff_layout(),
            commands::Diff::ToggleBase => self.toggle_diff_base(),
        }
    }

    /// The gutter's commands for stepping between changes and staging one,
    /// when the view has the keyboard: the same keys do the same thing in
    /// the diff. `None` for anything the view leaves to the gutter.
    pub(super) fn diff_on_hunk(&mut self, command: Command) -> Option<Outcome> {
        if self.focus != Focus::Diff {
            return None;
        }
        match command {
            Command::NextHunk => Some(self.diff_step(true)),
            Command::PreviousHunk => Some(self.diff_step(false)),
            Command::StageHunk => Some(self.stage_diff_hunk(None)),
            _ => None,
        }
    }

    /// Show the diff of the file being edited, or put it away if it is
    /// already showing.
    pub(super) fn toggle_diff(&mut self) -> Outcome {
        let pane = self.panes.focus();
        let doc = self.doc().id;
        if self.diffing.view.as_ref().is_some_and(|view| view.pane == pane && view.doc == doc) {
            return self.close_diff();
        }
        // In full: git finds the repository from the file's folder, and a
        // path as typed on the command line may have none.
        let Some((spelled, path)) = self
            .doc()
            .buffer
            .path()
            .and_then(|path| Some((path.to_path_buf(), std::path::absolute(path).ok()?)))
        else {
            self.message = Some("A file that has never been saved has no history to diff.".into());
            return Outcome::Redraw;
        };
        let Some(vcs) = self.vcs.as_ref() else {
            self.message = Some("Git is not running.".into());
            return Outcome::Redraw;
        };
        self.diffing.versions += 1;
        self.diffing.serials += 1;
        let (version, serial) = (self.diffing.versions, self.diffing.serials);
        let text = self.doc().buffer.rope().clone();
        vcs.send(Request::Open { id: VCS_ID, version, path: path.clone(), text: text.clone() });
        vcs.send(Request::Compare { id: VCS_ID, serial, against: Against::Index });
        self.diffing.view = Some(View {
            pane,
            doc,
            path,
            spelled,
            against: Against::Index,
            layout: DiffLayout::Split,
            scroll: 0,
            current: None,
            version,
            sent: text,
            due: None,
            serial,
            index: None,
            shown: Shown::Waiting,
        });
        self.focus = Focus::Diff;
        self.relayout();
        Outcome::Redraw
    }

    /// Whether the file being edited can be diffed: it has been saved
    /// somewhere, and git is running. Whether it is in a repository is for
    /// the view to say.
    pub(super) fn can_diff(&self) -> bool {
        self.vcs.is_some() && self.doc().buffer.path().is_some()
    }

    /// Put the view away, giving the pane its text back.
    pub(super) fn close_diff(&mut self) -> Outcome {
        if self.diffing.view.take().is_none() {
            return Outcome::Continue;
        }
        if let Some(vcs) = self.vcs.as_ref() {
            vcs.send(Request::Close(VCS_ID));
        }
        if let Some(syntax) = self.syntax.as_ref() {
            syntax.send(nun_syntax::Request::Close(SYNTAX_BEFORE));
            syntax.send(nun_syntax::Request::Close(SYNTAX_AFTER));
        }
        if self.focus == Focus::Diff {
            self.focus = Focus::Editor;
        }
        self.relayout();
        Outcome::Redraw
    }

    /// Whether the view is what `pane` shows.
    pub(super) fn diff_in(&self, pane: usize) -> bool {
        self.diffing.view.as_ref().is_some_and(|view| {
            view.pane == pane
                && self.panes.get(pane).and_then(super::panes::Pane::current) == Some(view.doc)
        })
    }

    /// Keep the view in step with everything else, after any event: close it
    /// when its document or pane has gone, give it the keyboard when its pane
    /// has it, and note an edit to compare once typing settles.
    ///
    /// `edited` is whether the event could have changed any text: a pointer
    /// merely moving cannot, and it comes by the hundred.
    pub(super) fn diff_follow(&mut self, now: Instant, edited: bool) {
        let Some(view) = self.diffing.view.as_ref() else { return };
        let (pane, doc) = (view.pane, view.doc);
        if !self.diff_in(pane) {
            self.close_diff();
            return;
        }
        // A pane showing the view has nothing else to type into: the text
        // behind it is out of sight.
        if self.focus == Focus::Editor && self.panes.focus() == pane {
            self.focus = Focus::Diff;
        } else if self.focus == Focus::Diff && self.panes.focus() != pane {
            self.focus = Focus::Editor;
        }
        if !edited || view.due.is_some() {
            return;
        }
        let Some(document) = self.doc_by(doc) else { return };
        // Saved under another name: git has to look for it somewhere else,
        // and perhaps in another repository.
        if let Some(spelled) = document.buffer.path().filter(|path| *path != view.spelled)
            && let Ok(path) = std::path::absolute(spelled)
        {
            let spelled = spelled.to_path_buf();
            if let Some(vcs) = self.vcs.as_ref() {
                vcs.send(Request::Moved { id: VCS_ID, path: path.clone() });
            }
            if let Some(view) = self.diffing.view.as_mut() {
                view.path = path;
                view.spelled = spelled;
            }
            self.compare();
        }
        let Some(view) = self.diffing.view.as_ref() else { return };
        let Some(text) = self.doc_by(doc).map(|document| document.buffer.rope()) else { return };
        let changed = text.len_chars() != view.sent.len_chars() || *text != view.sent;
        if let Some(view) = self.diffing.view.as_mut()
            && changed
            && view.due.is_none()
        {
            view.due = Some(now + SETTLE);
        }
    }

    /// When an edit is next due to be compared.
    pub(super) fn diff_deadline(&self) -> Option<Instant> {
        self.diffing.view.as_ref().and_then(|view| view.due)
    }

    /// Send the text once typing has settled, and ask for the comparison.
    pub(super) fn diff_tick(&mut self, now: Instant) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else { return Outcome::Continue };
        if view.due.is_none_or(|due| now < due) {
            return Outcome::Continue;
        }
        let Some(text) = self.doc_by(view.doc).map(|document| document.buffer.rope().clone())
        else {
            return Outcome::Continue;
        };
        self.diffing.versions += 1;
        let version = self.diffing.versions;
        if let Some(view) = self.diffing.view.as_mut() {
            view.due = None;
            view.version = version;
            view.sent = text.clone();
        }
        if let Some(vcs) = self.vcs.as_ref() {
            vcs.send(Request::Update { id: VCS_ID, version, text });
        }
        self.compare();
        Outcome::Continue
    }

    /// Ask for the comparison the view is showing, again.
    fn compare(&mut self) {
        let Some(view) = self.diffing.view.as_mut() else { return };
        self.diffing.serials += 1;
        view.serial = self.diffing.serials;
        if let Some(vcs) = self.vcs.as_ref() {
            vcs.send(Request::Compare { id: VCS_ID, serial: view.serial, against: view.against });
        }
    }

    // ── what git says ───────────────────────────────────────────────────────

    /// Take git's answer if it is the view's, handing anything else back.
    pub(super) fn diff_vcs(&mut self, reply: Reply) -> Result<Outcome, Reply> {
        match reply {
            Reply::Compared { id: VCS_ID, version, serial, against, diff } => {
                Ok(self.compared(version, serial, against, diff))
            }
            Reply::Hunks { id: VCS_ID, version, diff } => {
                let Some(view) = self.diffing.view.as_mut() else { return Ok(Outcome::Continue) };
                if version != view.version {
                    return Ok(Outcome::Continue);
                }
                view.index = diff;
                view.restage();
                Ok(Outcome::Redraw)
            }
            Reply::Staged { id: VCS_ID, result } => Ok(self.staged(result)),
            // The tree's status is asked for whenever something outside may
            // have changed the index or `HEAD`: a save, the terminal getting
            // focus back. The view's comparison may be stale for the same
            // reason, and so may git's idea of the view's staged version.
            Reply::Status { .. } if self.diffing.view.is_some() => {
                if let Some(vcs) = self.vcs.as_ref() {
                    vcs.send(Request::Refresh);
                }
                self.compare();
                Err(reply)
            }
            reply => Err(reply),
        }
    }

    fn compared(
        &mut self,
        version: u64,
        serial: u64,
        against: Against,
        diff: Option<Arc<Diff>>,
    ) -> Outcome {
        let Some(view) = self.diffing.view.as_mut() else { return Outcome::Continue };
        // Asked before the base was switched, or before the text moved on:
        // the answer to the newer question is on its way.
        if serial != view.serial || version != view.version || against != view.against {
            return Outcome::Continue;
        }
        let Some(diff) = diff else {
            view.shown = Shown::Nothing(
                "Nothing to compare with: the file is in no repository, is not tracked, or is \
                 binary or too large to diff.",
            );
            return Outcome::Redraw;
        };
        if let Shown::Diff(drawn) = &view.shown
            && drawn.version == version
            && *drawn.diff == *diff
        {
            return Outcome::Continue;
        }
        // Keep the same place in the file across the change.
        let at = view.top_line();
        // The old side changes only with the index or `HEAD`; while it has
        // not, its rope and its colours carry over.
        let before = match &view.shown {
            Shown::Diff(drawn) if drawn.diff.base().text() == diff.base().text() => {
                Some(drawn.before.clone())
            }
            _ => None,
        };
        let base_changed = before.is_none();
        let before = before.unwrap_or_else(|| Rope::from_str(diff.base().text()));
        let mut drawn =
            Drawn::new(diff, version, before, view.sent.clone(), view.layout, view.index.as_ref());
        // The colours already on screen stay until the new ones come, as the
        // editor's own do: a few chars stale at worst, where blank would
        // flash on every edit.
        if let Shown::Diff(old) = &mut view.shown {
            drawn.spans.1 = std::mem::take(&mut old.spans.1);
            if !base_changed {
                drawn.spans.0 = std::mem::take(&mut old.spans.0);
                drawn.parse.0 = old.parse.0;
            }
        }
        view.scroll = at.map_or(0, |line| nun_ui::diff_row_of(&drawn.rows, line));
        view.current = view.current.filter(|hunk| *hunk < drawn.hunks.len());
        view.shown = Shown::Diff(Box::new(drawn));
        self.diff_highlight(base_changed);
        self.clamp_diff_scroll();
        Outcome::Redraw
    }

    fn staged(&mut self, result: Result<(), String>) -> Outcome {
        match result {
            Ok(()) => {
                self.message = Some("Staged.".into());
                self.compare();
                self.refresh_status();
            }
            Err(error) => self.message = Some(format!("Not staged: {error}")),
        }
        Outcome::Redraw
    }

    // ── what the parser says ────────────────────────────────────────────────

    /// Have the parser colour both versions, the old one only if it changed.
    fn diff_highlight(&mut self, base_changed: bool) {
        let (Some(syntax), Some(view)) = (self.syntax.as_ref(), self.diffing.view.as_mut()) else {
            return;
        };
        let language = self
            .docs
            .iter()
            .find(|document| document.id == view.doc)
            .and_then(|document| document.buffer.path())
            .and_then(nun_syntax::of_path);
        let (Some(language), Shown::Diff(drawn)) = (language, &mut view.shown) else { return };
        let versions = &mut self.diffing.versions;
        let mut send = |id, text: &Rope| {
            *versions += 1;
            syntax.send(nun_syntax::Request::Open { id, language, text: text.clone() });
            let window = 0..u32::try_from(text.len_chars()).unwrap_or(u32::MAX);
            syntax.send(nun_syntax::Request::Window { id, version: *versions, window });
            *versions
        };
        if base_changed {
            drawn.parse.0 = send(SYNTAX_BEFORE, &drawn.before);
        }
        drawn.parse.1 = send(SYNTAX_AFTER, &drawn.after);
    }

    /// Take the parser's answer if it is about one of the view's versions.
    pub(super) fn diff_syntax(&mut self, reply: &nun_syntax::Reply) -> Option<Outcome> {
        match reply {
            nun_syntax::Reply::Highlights { id, version, spans, .. }
                if [SYNTAX_BEFORE, SYNTAX_AFTER].contains(id) =>
            {
                if let Some(View { shown: Shown::Diff(drawn), .. }) = self.diffing.view.as_mut() {
                    let (side, waited) = if *id == SYNTAX_BEFORE {
                        (&mut drawn.spans.0, drawn.parse.0)
                    } else {
                        (&mut drawn.spans.1, drawn.parse.1)
                    };
                    if *version == waited {
                        side.clone_from(spans);
                        return Some(Outcome::Redraw);
                    }
                }
                Some(Outcome::Continue)
            }
            // A grammar that gave up on one of the versions leaves it
            // uncoloured; the editor's own copy says so, if it happens there.
            nun_syntax::Reply::Disabled { id, .. }
                if [SYNTAX_BEFORE, SYNTAX_AFTER].contains(id) =>
            {
                Some(Outcome::Continue)
            }
            _ => None,
        }
    }

    // ── moving about ────────────────────────────────────────────────────────

    /// The rows the view has, or none.
    fn diff_rows(&self) -> &[DiffRow] {
        match self.diffing.view.as_ref().map(|view| &view.shown) {
            Some(Shown::Diff(drawn)) => &drawn.rows,
            _ => &[],
        }
    }

    /// Where the view is drawn, while it is on screen.
    fn diff_area(&self) -> Option<Rect> {
        let view = self.diffing.view.as_ref()?;
        self.diff_in(view.pane).then(|| self.text_area_of(view.pane)).flatten()
    }

    fn visible_diff_rows(&self) -> usize {
        self.diff_area().map_or(1, DiffView::visible_rows).max(1)
    }

    fn clamp_diff_scroll(&mut self) {
        let most = self.diff_rows().len().saturating_sub(self.visible_diff_rows());
        if let Some(view) = self.diffing.view.as_mut() {
            view.scroll = view.scroll.min(most);
        }
    }

    fn scroll_diff_by(&mut self, delta: isize) -> Outcome {
        if let Some(view) = self.diffing.view.as_mut() {
            view.scroll = view.scroll.saturating_add_signed(delta);
        }
        self.clamp_diff_scroll();
        Outcome::Redraw
    }

    /// The wheel over the view: both sides move together, because they are
    /// one list of rows.
    pub(super) fn diff_scroll(&mut self, down: bool) -> Outcome {
        let rows = isize::try_from(WHEEL_ROWS).unwrap_or(1);
        self.scroll_diff_by(if down { rows } else { -rows })
    }

    /// Go to the next hunk, or the one before, wrapping.
    pub(super) fn diff_step(&mut self, forward: bool) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else {
            self.message = Some("No diff is open.".into());
            return Outcome::Redraw;
        };
        let Shown::Diff(drawn) = &view.shown else { return Outcome::Continue };
        let count = drawn.hunks.len();
        if count == 0 {
            self.message = Some("No changes.".into());
            return Outcome::Redraw;
        }
        // From the hunk the keyboard is on, or else from what is on screen.
        let next = match view.current {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None => {
                let top = view.scroll;
                let starts: Vec<usize> = (0..count)
                    .filter_map(|hunk| nun_ui::diff_header_row(&drawn.rows, hunk))
                    .collect();
                if forward {
                    starts.iter().position(|row| *row >= top).unwrap_or(0)
                } else {
                    starts.iter().rposition(|row| *row < top).unwrap_or(count - 1)
                }
            }
        };
        self.show_diff_hunk(next);
        self.message = Some(format!("Change {} of {count}.", next + 1));
        Outcome::Redraw
    }

    /// Make `hunk` the current one and scroll it into view, a little below
    /// the top so what comes before it shows too.
    fn show_diff_hunk(&mut self, hunk: usize) {
        let height = self.visible_diff_rows();
        let Some(view) = self.diffing.view.as_mut() else { return };
        let Shown::Diff(drawn) = &view.shown else { return };
        let Some(row) = nun_ui::diff_header_row(&drawn.rows, hunk) else { return };
        view.current = Some(hunk);
        let end = row + 1 + drawn.hunks[hunk].before.len().max(drawn.hunks[hunk].after.len());
        if row < view.scroll || end > view.scroll + height {
            view.scroll = row.saturating_sub(height / 4);
        }
        self.clamp_diff_scroll();
    }

    /// Switch between side by side and unified, keeping the same place.
    pub(super) fn toggle_diff_layout(&mut self) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else {
            self.message = Some("No diff is open.".into());
            return Outcome::Redraw;
        };
        self.set_diff_layout(view.layout.toggled())
    }

    fn set_diff_layout(&mut self, layout: DiffLayout) -> Outcome {
        let Some(view) = self.diffing.view.as_mut() else { return Outcome::Continue };
        if view.layout == layout {
            return Outcome::Continue;
        }
        let at = view.top_line();
        view.layout = layout;
        if let Shown::Diff(drawn) = &mut view.shown {
            drawn.rows = nun_ui::align_diff(
                &drawn.hunks,
                drawn.diff.base().line_count(),
                nun_ui::diff_line_count(&drawn.after),
                layout,
            );
            view.scroll = at.map_or(0, |line| nun_ui::diff_row_of(&drawn.rows, line));
        }
        self.clamp_diff_scroll();
        Outcome::Redraw
    }

    /// Switch between comparing with the index and with `HEAD`.
    pub(super) fn toggle_diff_base(&mut self) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else {
            self.message = Some("No diff is open.".into());
            return Outcome::Redraw;
        };
        let other = match view.against {
            Against::Index => Against::Head,
            Against::Head => Against::Index,
        };
        self.set_diff_base(other)
    }

    fn set_diff_base(&mut self, against: Against) -> Outcome {
        let Some(view) = self.diffing.view.as_mut() else { return Outcome::Continue };
        if view.against == against {
            return Outcome::Continue;
        }
        view.against = against;
        view.current = None;
        self.compare();
        Outcome::Redraw
    }

    /// Stage the hunk the keyboard is on, or `hunk`.
    pub(super) fn stage_diff_hunk(&mut self, hunk: Option<usize>) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else {
            self.message = Some("No diff is open.".into());
            return Outcome::Redraw;
        };
        let Shown::Diff(drawn) = &view.shown else { return Outcome::Continue };
        let Some(index) = hunk.or(view.current).or_else(|| {
            // With none chosen, the one on screen, if there is only one.
            let shown = view.scroll..view.scroll + self.visible_diff_rows();
            let mut on_screen =
                drawn.rows.get(shown.start..shown.end.min(drawn.rows.len()))?.iter();
            let first = on_screen.find_map(|row| row.hunk())?;
            on_screen.all(|row| row.hunk().is_none_or(|other| other == first)).then_some(first)
        }) else {
            self.message = Some("Choose a change to stage first: F7 goes to the next one.".into());
            return Outcome::Redraw;
        };
        let staged = view.index.as_ref().filter(|_| drawn.version == view.version);
        let Some(hunk) = drawn
            .diff
            .hunks()
            .get(index)
            .and_then(|hunk| staged.and_then(|staged| as_staged(&drawn.diff, hunk, staged)))
        else {
            self.message = Some(
                "That change is already staged, wholly or in part: the index view shows the rest."
                    .into(),
            );
            return Outcome::Redraw;
        };
        // The hunk as it is against the index, as of this version of the
        // text, which is what staging checks it against.
        if let Some(vcs) = self.vcs.as_ref() {
            vcs.send(Request::Stage { id: VCS_ID, version: drawn.version, hunk });
        }
        if let Some(view) = self.diffing.view.as_mut() {
            view.current = Some(index);
        }
        Outcome::Redraw
    }

    /// Go to row `row` in the editor: the view closes and the caret lands on
    /// the line, or where the lines were for a removal.
    fn diff_go_to(&mut self, row: usize) -> Outcome {
        let Some(view) = self.diffing.view.as_ref() else { return Outcome::Continue };
        let Shown::Diff(drawn) = &view.shown else { return Outcome::Continue };
        let Some(line) = drawn.rows.get(row).and_then(|row| row.after_line(&drawn.hunks)) else {
            return Outcome::Continue;
        };
        let pane = view.pane;
        self.close_diff();
        self.panes.set_focus(pane);
        self.focus = Focus::Editor;
        let buffer = &self.doc().buffer;
        let line = (line as usize).min(buffer.len_lines().saturating_sub(1));
        let at = buffer.line_start(line);
        self.doc_mut().buffer.set_selections(Selections::single(Range::caret(at)));
        // With some of what comes before it in view, not on the top row.
        let height = self.text_height();
        self.doc_mut().scroll = line.saturating_sub(height / 3);
        self.follow_caret();
        Outcome::Redraw
    }

    // ── keys and the mouse ──────────────────────────────────────────────────

    /// A key while the view has the keyboard. Nothing is typed: the text is
    /// behind the view, and Enter goes to it.
    pub(super) fn diff_key(&mut self, key: &KeyEvent) -> Outcome {
        let page = isize::try_from(self.visible_diff_rows()).unwrap_or(1);
        match key.code {
            KeyCode::Esc => self.close_diff(),
            KeyCode::Up => self.scroll_diff_by(-1),
            KeyCode::Down => self.scroll_diff_by(1),
            KeyCode::PageUp => self.scroll_diff_by(-page),
            KeyCode::PageDown => self.scroll_diff_by(page),
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_diff_by(isize::MIN / 2)
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_diff_by(isize::MAX / 2)
            }
            KeyCode::Home => self.scroll_diff_by(isize::MIN / 2),
            KeyCode::End => self.scroll_diff_by(isize::MAX / 2),
            KeyCode::Enter => {
                let row = self.diffing.view.as_ref().map_or(0, |view| {
                    let Shown::Diff(drawn) = &view.shown else { return view.scroll };
                    view.current
                        .and_then(|hunk| nun_ui::diff_header_row(&drawn.rows, hunk))
                        .unwrap_or(view.scroll)
                });
                self.diff_go_to(row)
            }
            _ => {
                self.message =
                    Some("The diff cannot be edited: Enter goes to the text, Esc closes.".into());
                Outcome::Redraw
            }
        }
    }

    /// A click in the view.
    pub(super) fn diff_press(&mut self, spot: DiffSpot) -> Outcome {
        self.acknowledge();
        if let Some(pane) = self.diffing.view.as_ref().map(|view| view.pane) {
            self.panes.set_focus(pane);
            self.focus = Focus::Diff;
        }
        match spot {
            DiffSpot::Close => self.close_diff(),
            DiffSpot::Layout(layout) => self.set_diff_layout(layout).and(Outcome::Redraw),
            DiffSpot::Base(head) => self
                .set_diff_base(if head { Against::Head } else { Against::Index })
                .and(Outcome::Redraw),
            DiffSpot::Stage(hunk) => self.stage_diff_hunk(Some(hunk)),
            DiffSpot::Row(row) => self.diff_go_to(row),
            DiffSpot::Header | DiffSpot::Empty => Outcome::Redraw,
        }
    }

    // ── layout and drawing ───────────────────────────────────────────────────

    /// Record where the view's buttons and rows are.
    pub(super) fn layout_diff(&self, hits: &mut nun_input::HitMap<Target>) {
        let (Some(area), Some(view)) = (self.diff_area(), self.diffing.view.as_ref()) else {
            return;
        };
        let (rows, hunks): (&[DiffRow], &[DiffHunk]) = match &view.shown {
            Shown::Diff(drawn) => (&drawn.rows, &drawn.hunks),
            _ => (&[], &[]),
        };
        for (rect, spot) in DiffView::spots(area, rows, hunks, view.scroll) {
            // Only the buttons light up under the pointer: rows are many, and
            // the whole view reporting motion would be a lot of reports for
            // no change on screen.
            let hover = matches!(
                spot,
                DiffSpot::Close | DiffSpot::Layout(_) | DiffSpot::Base(_) | DiffSpot::Stage(_)
            );
            hits.push(super::cells(rect), Target::Diff(spot), hover);
        }
    }

    /// Draw the view into `pane`'s text area, if it is showing there.
    pub(super) fn render_diff(&self, pane: usize, area: Rect, cells: &mut Cells) -> bool {
        let Some(view) = self.diffing.view.as_ref().filter(|_| self.diff_in(pane)) else {
            return false;
        };
        let Some(document) = self.doc_by(view.doc) else { return false };
        let title = document
            .buffer
            .path()
            .and_then(std::path::Path::file_name)
            .map_or_else(|| "[no name]".to_string(), |name| name.to_string_lossy().into_owned());
        let hovered = match self.hover.current() {
            Some(Target::Diff(spot)) => Some(spot),
            _ => None,
        };
        let widget = DiffView::new(&title, &self.palette)
            .laid_out(view.layout, view.against == Against::Head)
            .scrolled_to(view.scroll)
            .current(view.current)
            .hovered(hovered)
            .focused(self.focus == Focus::Diff)
            .tab_width(document.buffer.tab_width());
        match &view.shown {
            Shown::Waiting => widget.note(Some("Comparing…")).render(area, cells),
            Shown::Nothing(why) => widget.note(Some(why)).render(area, cells),
            Shown::Diff(drawn) if drawn.hunks.is_empty() => {
                let note = match view.against {
                    Against::Index => "No changes against the index.",
                    Against::Head => "No changes against HEAD.",
                };
                widget.note(Some(note)).render(area, cells);
            }
            Shown::Diff(drawn) => widget
                .showing(
                    &drawn.rows,
                    &drawn.hunks,
                    DiffSide {
                        text: &drawn.before,
                        spans: &drawn.spans.0,
                        emphasis: &drawn.emphasis.0,
                    },
                    DiffSide {
                        text: &drawn.after,
                        spans: &drawn.spans.1,
                        emphasis: &drawn.emphasis.1,
                    },
                )
                .render(area, cells),
        }
        true
    }
}

impl View {
    /// The line of the new version at the top of the view, to keep in view
    /// when the rows change under it.
    fn top_line(&self) -> Option<u32> {
        let Shown::Diff(drawn) = &self.shown else { return None };
        drawn.rows.get(self.scroll).and_then(|row| row.after_line(&drawn.hunks))
    }

    /// Which hunks can be staged, now the index diff is known.
    fn restage(&mut self) {
        if let Shown::Diff(drawn) = &mut self.shown {
            let index = self.index.as_ref().filter(|_| drawn.version == self.version);
            mark_stageable(&mut drawn.hunks, &drawn.diff, index);
        }
    }
}

/// Mark each hunk stageable if staging it would do what it says. See
/// [`as_staged`].
fn mark_stageable(hunks: &mut [DiffHunk], diff: &Diff, index: Option<&Arc<Diff>>) {
    for (shown, hunk) in hunks.iter_mut().zip(diff.hunks()) {
        shown.stageable = index.is_some_and(|index| as_staged(diff, hunk, index).is_some());
    }
}

/// `hunk` of `diff` as a hunk against the index, when there is one that
/// makes the same change: the same new lines, replacing the same old text.
/// Against the index that is the hunk itself. Against `HEAD` the old lines
/// are numbered differently wherever something above is staged, so they are
/// matched by what they say; and a hunk that is partly staged already has no
/// match, and is staged from the index view.
fn as_staged(diff: &Diff, hunk: &nun_vcs::Hunk, index: &Diff) -> Option<nun_vcs::Hunk> {
    index
        .hunks()
        .iter()
        .find(|staged| {
            staged.after == hunk.after && index.previous_text(staged) == diff.previous_text(hunk)
        })
        .cloned()
}

impl Drawn {
    fn new(
        diff: Arc<Diff>,
        version: u64,
        before: Rope,
        after: Rope,
        layout: DiffLayout,
        index: Option<&Arc<Diff>>,
    ) -> Self {
        let mut hunks: Vec<DiffHunk> = diff
            .hunks()
            .iter()
            .map(|hunk| DiffHunk {
                before: hunk.before.clone(),
                after: hunk.after.clone(),
                stageable: false,
            })
            .collect();
        mark_stageable(&mut hunks, &diff, index);
        let rows = nun_ui::align_diff(
            &hunks,
            diff.base().line_count(),
            nun_ui::diff_line_count(&after),
            layout,
        );
        let emphasis = emphasis(&diff, &after);
        Self {
            diff,
            version,
            before,
            after,
            hunks,
            rows,
            emphasis,
            spans: (Vec::new(), Vec::new()),
            parse: (0, 0),
        }
    }
}

/// The words that changed within each modified hunk, by line, on each side.
///
/// Worked out over the whole hunk rather than line by line, so a line split
/// in two or two joined into one still lines up word for word.
fn emphasis(diff: &Diff, after: &Rope) -> (Emphasis, Emphasis) {
    let mut out = (Emphasis::new(), Emphasis::new());
    let mut budget = MOST_INLINE_TOTAL;
    for hunk in diff.hunks() {
        if hunk.kind() != HunkKind::Modified
            || hunk.before.len() > MOST_INLINE_LINES
            || hunk.after.len() > MOST_INLINE_LINES
        {
            continue;
        }
        let old = diff.previous_text(hunk);
        let lines = after.len_lines();
        let char_of = |line: u32| {
            let line = line as usize;
            if line >= lines { after.len_chars() } else { after.line_to_char(line) }
        };
        let new = after.slice(char_of(hunk.after.start)..char_of(hunk.after.end));
        let size = old.len().max(new.len_chars());
        if size > MOST_INLINE_CHARS || size > budget {
            continue;
        }
        budget -= size;
        let new = new.to_string();
        let inline = nun_vcs::inline_changes(old, &new);
        by_line(old, hunk.before.start, &inline.before, &mut out.0);
        by_line(&new, hunk.after.start, &inline.after, &mut out.1);
    }
    out
}

/// Split char ranges into a text that starts at line `first` into ranges
/// within each line, leaving out the line breaks.
fn by_line(text: &str, first: u32, ranges: &[std::ops::Range<usize>], out: &mut Emphasis) {
    // Where each line starts and ends, in chars, its break not included.
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(start..at);
            start = at + 1;
        }
        at += 1;
    }
    if start < at {
        lines.push(start..at);
    }
    for range in ranges {
        for (index, line) in lines.iter().enumerate() {
            let from = range.start.max(line.start);
            let to = range.end.min(line.end);
            if from < to {
                let Ok(offset) = u32::try_from(index) else { break };
                out.entry(first + offset).or_default().push(from - line.start..to - line.start);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn span(start: usize, end: usize) -> std::ops::Range<usize> {
        start..end
    }

    #[test]
    fn changed_words_are_split_by_the_line_they_are_on() {
        let mut out = Emphasis::new();
        // "ab\ncd\n": chars 1..5 run from the b, over the break, into the c.
        by_line("ab\ncd\n", 7, &[span(1, 5)], &mut out);
        assert_eq!(out.get(&7).map(Vec::as_slice), Some(&[span(1, 2)][..]));
        assert_eq!(out.get(&8).map(Vec::as_slice), Some(&[span(0, 2)][..]));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_modified_hunk_gets_its_changed_words_and_nothing_else_does() {
        let base = Arc::new(nun_vcs::Base::from_text("keep\nlet x = 1;\nold\n"));
        let text = "keep\nlet x = 2;\nnew line\nadded\n";
        let diff = Diff::new(base, text);
        let (before, after) = emphasis(&diff, &Rope::from_str(text));
        assert_eq!(before.get(&1).map(Vec::as_slice), Some(&[span(8, 9)][..]), "{before:?}");
        assert_eq!(after.get(&1).map(Vec::as_slice), Some(&[span(8, 9)][..]), "{after:?}");
        assert!(!before.contains_key(&0) && !after.contains_key(&0), "context is untouched");
    }

    // ── against a real repository ──────────────────────────────────────────

    use std::path::Path;
    use std::process::Command as Process;
    use std::sync::mpsc::{self, Receiver};

    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use tempfile::TempDir;

    use crate::commands::{KeySet, defaults};

    /// Git with nothing from the environment, as in the status tests.
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
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    const BEFORE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    const AFTER: &str =
        "one\nTWO\nTWO-B\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\nextra\nmore\n";

    struct Rig {
        dir: TempDir,
        app: App,
        replies: Receiver<Reply>,
    }

    impl Rig {
        /// A repository with `a.txt` committed as [`BEFORE`] and changed on
        /// disk to [`AFTER`], open in an editor with git attached. `None`
        /// where there is no git to make one with.
        fn new() -> Option<Self> {
            let dir = TempDir::new().unwrap();
            git(dir.path(), &["init", "-q"])?;
            std::fs::write(dir.path().join("a.txt"), BEFORE).unwrap();
            git(dir.path(), &["add", "a.txt"])?;
            git(dir.path(), &["commit", "-qm", "a"])?;
            std::fs::write(dir.path().join("a.txt"), AFTER).unwrap();
            let mut app = App::new(
                Buffer::new(),
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 90, 20));
            let (send, replies) = mpsc::channel();
            app.attach_vcs(nun_vcs::Vcs::new(Box::new(move |reply| {
                let _ = send.send(reply);
            })));
            app.open_in_tab(&dir.path().join("a.txt"));
            Some(Self { dir, app, replies })
        }

        /// Hand git's answers to the editor until `done` holds.
        fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done(&self.app) {
                let left = deadline.saturating_duration_since(Instant::now());
                let reply = self.replies.recv_timeout(left).unwrap_or_else(|_| panic!("{what}"));
                self.app.handle(Event::Vcs(reply));
            }
        }

        fn hunks(&self) -> Vec<(std::ops::Range<u32>, std::ops::Range<u32>, bool)> {
            match self.app.diffing.view.as_ref().map(|view| &view.shown) {
                Some(Shown::Diff(drawn)) => drawn
                    .hunks
                    .iter()
                    .map(|hunk| (hunk.before.clone(), hunk.after.clone(), hunk.stageable))
                    .collect(),
                _ => Vec::new(),
            }
        }

        fn open(&mut self) {
            self.app.run(Command::Diff(commands::Diff::Toggle));
            self.until("the diff arrives", |app| {
                matches!(app.diffing.view.as_ref().map(|view| &view.shown), Some(Shown::Diff(_)))
                    && app.diffing.view.as_ref().is_some_and(|view| view.index.is_some())
            });
        }

        fn screen(&self) -> String {
            let mut harness = nun_ui::Harness::new(90, 20);
            harness.draw(crate::AppView(&self.app));
            harness.to_text()
        }

        fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.app.handle(Event::Key(KeyEvent::new(code, modifiers)));
        }

        fn click(&mut self, spot: DiffSpot) {
            let (x, y) = self.spot(spot);
            for kind in
                [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)]
            {
                let mouse = MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
                self.app.handle(Event::Mouse(mouse));
            }
        }

        /// Where `spot` is on screen.
        fn spot(&self, spot: DiffSpot) -> (u16, u16) {
            let area = self.app.diff_area().expect("the diff is on screen");
            let view = self.app.diffing.view.as_ref().unwrap();
            let Shown::Diff(drawn) = &view.shown else { panic!("no diff") };
            let (rect, _) = DiffView::spots(area, &drawn.rows, &drawn.hunks, view.scroll)
                .into_iter()
                .rev()
                .find(|(_, found)| *found == spot)
                .unwrap_or_else(|| panic!("{spot:?} is not on screen"));
            (rect.x + rect.width / 2, rect.y)
        }
    }

    #[test]
    fn the_diff_shows_both_versions_level_and_changes_layout_in_place() {
        let Some(mut rig) = Rig::new() else { return };
        rig.open();
        assert_eq!(rig.hunks(), vec![(1..2, 1..3, true), (10..10, 11..13, true)]);
        let screen = rig.screen();
        assert!(screen.contains("a.txt against the index · 2 changes"), "{screen}");
        assert!(screen.contains("two") && screen.contains("TWO"), "{screen}");
        let hatch = rig.app.palette.glyph(nun_ui::Glyph::DiffFiller).repeat(4);
        let extra = screen.lines().find(|line| line.contains("extra")).unwrap();
        assert!(extra.starts_with(&hatch), "the old side of an added line is filler: {screen}");

        rig.click(DiffSpot::Layout(DiffLayout::Unified));
        let screen = rig.screen();
        assert!(screen.contains("- two") && screen.contains("+ TWO"), "{screen}");
        assert!(!screen.contains(&hatch), "unified has no filler: {screen}");
        rig.app.run(Command::Diff(commands::Diff::ToggleLayout));
        assert_eq!(rig.app.diffing.view.as_ref().unwrap().layout, DiffLayout::Split);
    }

    #[test]
    fn a_hunk_staged_from_the_diff_is_in_the_index_and_leaves_the_diff() {
        let Some(mut rig) = Rig::new() else { return };
        rig.open();
        rig.click(DiffSpot::Stage(0));
        rig.until("the stage lands", |app| app.message.as_deref() == Some("Staged."));
        let staged = git(rig.dir.path(), &["diff", "--cached"]).unwrap();
        assert!(staged.contains("-two\n+TWO\n+TWO-B"), "{staged}");
        assert!(!staged.contains("extra"), "only the one hunk: {staged}");
        let unstaged = git(rig.dir.path(), &["diff"]).unwrap();
        assert!(unstaged.contains("+extra") && !unstaged.contains("TWO"), "{unstaged}");
        rig.until("the diff follows", |app| {
            matches!(app.diffing.view.as_ref().map(|view| &view.shown),
                Some(Shown::Diff(drawn)) if drawn.hunks.len() == 1)
        });

        // Against HEAD, both changes are there, and only the unstaged one
        // can be staged again.
        rig.app.run(Command::Diff(commands::Diff::ToggleBase));
        rig.until("the HEAD diff arrives", |app| {
            matches!(app.diffing.view.as_ref().map(|view| (&view.shown, view.against)),
                Some((Shown::Diff(drawn), Against::Head)) if drawn.hunks.len() == 2)
        });
        let screen = rig.screen();
        assert!(screen.contains("against HEAD"), "{screen}");
        // Staged above it, the second change's old lines are numbered one
        // way in the index and another in HEAD; it is staged all the same.
        assert_eq!(rig.hunks(), vec![(1..2, 1..3, false), (10..10, 11..13, true)]);
        rig.click(DiffSpot::Stage(1));
        assert_eq!(rig.app.message, None);
        rig.until("the second stage lands", |app| app.message.as_deref() == Some("Staged."));
        let staged = git(rig.dir.path(), &["diff", "--cached"]).unwrap();
        assert!(staged.contains("+TWO-B") && staged.contains("+more"), "{staged}");
    }

    #[test]
    fn unsaved_edits_are_in_the_diff_once_typing_settles() {
        let Some(mut rig) = Rig::new() else { return };
        rig.open();
        // Go to the text, edit it, and come back.
        rig.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(rig.app.diffing.view.is_none());
        rig.key(KeyCode::Char('x'), KeyModifiers::NONE);
        rig.open();
        rig.key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(!rig.app.doc().buffer.rope().to_string().contains('y'), "nothing is typed");
        // An edit from elsewhere — another pane, a format — is followed.
        rig.app.doc_mut().buffer.insert("z");
        let now = Instant::now();
        rig.app.handle_at(Event::Focus(false), now);
        assert!(rig.app.diff_deadline().is_some());
        rig.app.tick(now + SETTLE);
        rig.until("the edit is compared", |app| {
            matches!(app.diffing.view.as_ref().map(|view| &view.shown),
                Some(Shown::Diff(drawn)) if drawn.after.to_string().starts_with("xzone"))
        });
        assert_eq!(rig.hunks()[0].1, 0..3, "{:?}", rig.hunks());
    }

    #[test]
    fn keys_step_through_changes_and_a_click_goes_to_the_line() {
        let Some(mut rig) = Rig::new() else { return };
        rig.app.set_viewport(Rect::new(0, 0, 90, 8));
        rig.open();
        rig.key(KeyCode::F(7), KeyModifiers::NONE);
        assert_eq!(rig.app.diffing.view.as_ref().unwrap().current, Some(0));
        rig.key(KeyCode::F(7), KeyModifiers::NONE);
        assert_eq!(rig.app.diffing.view.as_ref().unwrap().current, Some(1));
        let scrolled = rig.app.diffing.view.as_ref().unwrap().scroll;
        assert!(scrolled > 0, "the second change is scrolled into view");
        rig.key(KeyCode::F(7), KeyModifiers::SHIFT);
        assert_eq!(rig.app.diffing.view.as_ref().unwrap().current, Some(0));

        // The wheel moves both sides at once, in one list of rows.
        let top = rig.app.diffing.view.as_ref().unwrap().scroll;
        let (x, y) = rig.spot(DiffSpot::Row(top));
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let before = rig.app.diffing.view.as_ref().unwrap().scroll;
        rig.app.handle(Event::Mouse(wheel));
        assert!(rig.app.diffing.view.as_ref().unwrap().scroll >= before);

        let view = rig.app.diffing.view.as_ref().unwrap();
        let Shown::Diff(drawn) = &view.shown else { panic!() };
        let row = drawn
            .rows
            .iter()
            .position(|row| matches!(row, DiffRow::Change { after: Some(11), .. }))
            .unwrap();
        rig.app.diffing.view.as_mut().unwrap().scroll = row;
        rig.app.relayout();
        rig.click(DiffSpot::Row(row));
        assert!(rig.app.diffing.view.is_none(), "going to the text closes the diff");
        assert_eq!(rig.app.focus, Focus::Editor);
        let buffer = &rig.app.doc().buffer;
        assert_eq!(buffer.line_of(buffer.selections().primary().head), 11);
    }

    #[test]
    fn the_text_menu_opens_the_diff() {
        let Some(mut rig) = Rig::new() else { return };
        let (text, _) = rig.app.areas();
        let (x, y) = (text.x + 10, text.y + 1);
        let press = |kind| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        rig.app.handle(Event::Mouse(press(MouseEventKind::Down(MouseButton::Right))));
        let menu = rig.app.menu.as_ref().expect("a menu");
        let item = menu
            .commands
            .iter()
            .position(|command| *command == Command::Diff(commands::Diff::Toggle))
            .expect("the menu offers the diff");
        let row = menu.area.y + u16::try_from(item).unwrap();
        let at = MouseEvent {
            row,
            column: menu.area.x + 1,
            ..press(MouseEventKind::Down(MouseButton::Left))
        };
        rig.app.handle(Event::Mouse(at));
        assert!(rig.app.diffing.view.is_some());
        assert_eq!(rig.app.focus, Focus::Diff);
    }

    #[test]
    fn switching_tabs_away_from_the_diff_puts_it_away() {
        let Some(mut rig) = Rig::new() else { return };
        std::fs::write(rig.dir.path().join("b.txt"), "b\n").unwrap();
        rig.open();
        rig.app.open_in_tab(&rig.dir.path().join("b.txt"));
        rig.app.handle(Event::Focus(false));
        assert!(rig.app.diffing.view.is_none());
        assert_eq!(rig.app.focus, Focus::Editor);
    }

    #[test]
    fn both_versions_are_coloured_the_way_the_editor_colours_them() {
        let dir = TempDir::new().unwrap();
        if git(dir.path(), &["init", "-q"]).is_none() {
            return;
        }
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn old() {}\n").unwrap();
        git(dir.path(), &["add", "main.rs"]).unwrap();
        std::fs::write(&file, "fn new() {}\n").unwrap();
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 12));
        let (send, events) = mpsc::channel();
        let vcs = send.clone();
        app.attach_vcs(nun_vcs::Vcs::new(Box::new(move |reply| {
            let _ = vcs.send(Event::Vcs(reply));
        })));
        app.attach_syntax(nun_syntax::Worker::new(Box::new(move |reply| {
            let _ = send.send(Event::Syntax(reply));
        })));
        app.open_in_tab(&file);
        app.run(Command::Diff(commands::Diff::Toggle));
        let coloured = |app: &App| {
            matches!(app.diffing.view.as_ref().map(|view| &view.shown),
                Some(Shown::Diff(drawn)) if !drawn.spans.0.is_empty() && !drawn.spans.1.is_empty())
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !coloured(&app) {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = events.recv_timeout(left).expect("both sides are highlighted");
            app.handle(event);
        }
        let Some(Shown::Diff(drawn)) = app.diffing.view.as_ref().map(|view| &view.shown) else {
            unreachable!()
        };
        assert!(drawn.spans.0.iter().any(|span| span.capture.starts_with("keyword")));
        assert!(drawn.spans.1.iter().any(|span| span.capture.starts_with("keyword")));
    }
}
