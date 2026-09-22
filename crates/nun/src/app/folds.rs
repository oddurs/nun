//! Folding: the arrows in the gutter, the commands, and folds that outlast
//! the session.
//!
//! Where a file *can* fold comes from the parser, as line ranges, and arrives
//! as a message after every parse. What *is* folded belongs to the buffer,
//! which keeps it as offsets so it moves with the text. The two meet here.

use nun_syntax::FoldRange;

use super::panes::DocId;
use super::{App, Outcome};

impl App {
    /// Fold the innermost region around the caret that is not folded yet, so
    /// pressing again folds the one around that.
    pub(super) fn fold_here(&mut self) -> Outcome {
        let buffer = &self.doc().buffer;
        let line = buffer.line_of(buffer.selections().primary().head);
        let innermost = self
            .doc()
            .syntax
            .folding
            .ranges
            .iter()
            .filter(|range| contains(**range, line) && !buffer.is_folded(range.header as usize))
            .max_by_key(|range| range.header)
            .copied();
        match innermost {
            Some(range) => {
                self.doc_mut().buffer.fold(range.header as usize, range.last as usize);
                self.follow_caret();
            }
            None => self.message = Some("Nothing around the caret to fold.".into()),
        }
        Outcome::Redraw
    }

    /// Unfold the region folded under the caret's line.
    pub(super) fn unfold_here(&mut self) -> Outcome {
        let buffer = &self.doc().buffer;
        let line = buffer.line_of(buffer.selections().primary().head);
        if !self.doc_mut().buffer.unfold(line) {
            self.message = Some("Nothing is folded on this line.".into());
        }
        Outcome::Redraw
    }

    /// Fold every region in the file, nested ones included, so opening one
    /// shows its outline rather than all of it.
    pub(super) fn fold_all(&mut self) -> Outcome {
        let ranges = self.doc().syntax.folding.ranges.clone();
        if ranges.is_empty() {
            self.message = Some("Nothing in this file to fold.".into());
            return Outcome::Redraw;
        }
        for range in ranges {
            self.doc_mut().buffer.fold(range.header as usize, range.last as usize);
        }
        self.follow_caret();
        Outcome::Redraw
    }

    /// Unfold everything.
    pub(super) fn unfold_all(&mut self) -> Outcome {
        self.doc_mut().buffer.unfold_all();
        Outcome::Redraw
    }

    /// Whether the caret's line is the header of a folded region, or inside
    /// a region that could be folded — what the text menu offers.
    pub(super) fn fold_offers(&self) -> (bool, bool) {
        let buffer = &self.doc().buffer;
        let line = buffer.line_of(buffer.selections().primary().head);
        let can_fold = self
            .doc()
            .syntax
            .folding
            .ranges
            .iter()
            .any(|range| contains(*range, line) && !buffer.is_folded(range.header as usize));
        (can_fold, buffer.is_folded(line))
    }

    /// A press on the column the arrows are drawn in, on screen row `row` of
    /// the text. With Alt held it folds or unfolds every sibling of the
    /// region — the regions directly inside the same one — which is how a
    /// file's methods are all put away at once.
    ///
    /// `None` when there is no arrow on that row, so the press can be the
    /// gutter's instead.
    pub(super) fn fold_click(&mut self, row: usize, alt: bool) -> Option<Outcome> {
        let line = self.line_at_row(row)?;
        let ranges = &self.doc().syntax.folding.ranges;
        let buffer = &self.doc().buffer;
        let folded = buffer.is_folded(line);
        let index = ranges.iter().position(|range| range.header as usize == line);
        if !folded && index.is_none() {
            return None;
        }

        let targets: Vec<FoldRange> = match index {
            Some(index) if alt => siblings(ranges, index),
            Some(index) => vec![ranges[index]],
            // Folded, though the parser no longer offers the region — the
            // text changed under it. It can still be opened.
            None => Vec::new(),
        };
        if folded {
            self.doc_mut().buffer.unfold(line);
            for range in targets {
                self.doc_mut().buffer.unfold(range.header as usize);
            }
        } else {
            for range in targets {
                self.doc_mut().buffer.fold(range.header as usize, range.last as usize);
            }
        }
        Some(Outcome::Redraw)
    }

    /// The parser has said where a document can fold.
    pub(super) fn folds_arrived(
        &mut self,
        id: DocId,
        version: u64,
        folds: Vec<FoldRange>,
    ) -> Outcome {
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return Outcome::Continue;
        };
        // Line numbers from older text would put arrows on the wrong lines.
        if version < document.syntax.folding.version {
            return Outcome::Continue;
        }
        document.syntax.folding.version = version;
        document.syntax.folding.ranges = folds;

        // The first answer for a file is when the folds it had last time can
        // be put back: only now is it known which lines are still regions.
        // One that is not — the file changed while nun was not looking — is
        // left open rather than folded over something else.
        if !document.syntax.folding.restored {
            document.syntax.folding.restored = true;
            let remembered =
                document.buffer.path().map(|path| self.session.folds_of(path)).unwrap_or_default();
            for header in remembered {
                if let Some(range) = document
                    .syntax
                    .folding
                    .ranges
                    .iter()
                    .find(|range| range.header as usize == header)
                {
                    document.buffer.fold(header, range.last as usize);
                }
            }
        }
        Outcome::Redraw
    }

    /// Note what is folded in every open file, so the next session can put it
    /// back.
    pub(super) fn remember_folds(&mut self) {
        let ids: Vec<DocId> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.remember_folds_of(id);
        }
    }

    /// Note what is folded in one document, before it closes.
    pub(super) fn remember_folds_of(&mut self, id: DocId) {
        let Some(document) = self.doc_by(id) else { return };
        let Some(path) = document.buffer.path().map(std::path::Path::to_path_buf) else { return };
        // A file whose parse has not come back yet has had no chance to have
        // its folds put back, and remembering it now would forget them.
        if document.syntax.open && !document.syntax.folding.restored {
            return;
        }
        let headers = document.buffer.folded().into_iter().map(|(header, _)| header).collect();
        self.session.remember(&path, headers);
    }

    /// Write down what has been remembered, for the next session.
    ///
    /// # Errors
    ///
    /// Whatever writing the file ran into.
    pub fn save_session(&mut self) -> std::io::Result<()> {
        self.remember_folds();
        self.session.save()
    }

    /// Remember folds in `session`, and put back the ones it already has.
    pub fn attach_session(&mut self, session: crate::session::Session) {
        self.session = session;
    }
}

/// Whether `line` is inside `range`, its header included.
const fn contains(range: FoldRange, line: usize) -> bool {
    range.header as usize <= line && line <= range.last as usize
}

/// The regions directly inside the same region as `ranges[index]`, that one
/// included. `ranges` are in order of their headers.
fn siblings(ranges: &[FoldRange], index: usize) -> Vec<FoldRange> {
    let parent_of = |at: usize| {
        let range = ranges[at];
        ranges[..at]
            .iter()
            .rposition(|outer| outer.header < range.header && outer.last >= range.last)
    };
    let parent = parent_of(index);
    (0..ranges.len()).filter(|&at| parent_of(at) == parent).map(|at| ranges[at]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, KeySet, defaults};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use nun_syntax::{Reply, Worker};
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use ratatui::layout::Rect;
    use std::fs;
    use std::time::{Duration, Instant};

    /// A method, then a free function after it, in an impl block.
    const SOURCE: &str = "\
impl Point {
    fn one() {
        a();
        b();
    }
    fn two() {
        c();
    }
}
fn after() {
    d();
}
";

    /// An editor over a file, with the parser attached and pumped the way
    /// the event loop pumps it.
    struct Tester {
        app: App,
        replies: std::sync::mpsc::Receiver<Reply>,
    }

    impl Tester {
        fn new(dir: &tempfile::TempDir, session: crate::session::Session) -> Self {
            let path = dir.path().join("point.rs");
            if !path.exists() {
                fs::write(&path, SOURCE).unwrap();
            }
            let (buffer, _) = nun_core::Buffer::load(&path).unwrap();
            let mut app = App::new(
                buffer,
                Palette::new(derive(&Probe::builtin_dark())),
                defaults(KeySet::Full),
            );
            app.set_viewport(Rect::new(0, 0, 60, 8));
            app.attach_session(session);
            let (sender, replies) = std::sync::mpsc::channel();
            app.attach_syntax(Worker::new(Box::new(move |reply| {
                let _ = sender.send(reply);
            })));
            let mut tester = Self { app, replies };
            tester.settle();
            tester
        }

        fn settle(&mut self) {
            self.app.tick(Instant::now() + super::super::syntax::DEBOUNCE * 4);
            self.app.syntax_echo(7);
            loop {
                let reply = self.replies.recv_timeout(Duration::from_secs(10)).expect("answered");
                if reply == Reply::Echo(7) {
                    return;
                }
                self.app.handle(Event::Syntax(reply));
            }
        }

        fn folded(&self) -> Vec<(usize, usize)> {
            self.app.doc().buffer.folded()
        }

        /// The screen as text, one line per row.
        fn screen(&self) -> Vec<String> {
            let area = self.app.viewport;
            let mut cells = ratatui::buffer::Buffer::empty(area);
            self.app.render(area, &mut cells);
            (0..area.height)
                .map(|y| (0..area.width).map(|x| cells[(x, y)].symbol()).collect::<String>())
                .collect()
        }

        /// Press on the arrow column of screen row `row`.
        fn click_arrow(&mut self, row: u16, modifiers: KeyModifiers) {
            let (text, _) = self.app.areas();
            let column = text.x + self.app.gutter_width() - 2;
            let down = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: text.y + row,
                modifiers,
            };
            // Far enough apart that no two are a double-click.
            let at = Instant::now() + Duration::from_secs(u64::from(row) * 10 + 10);
            self.app.handle_at(Event::Mouse(down), at);
            let up = MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..down };
            self.app.handle_at(Event::Mouse(up), at);
        }
    }

    #[test]
    fn arrows_sit_only_beside_what_can_fold() {
        let dir = tempfile::tempdir().unwrap();
        let t = Tester::new(&dir, crate::session::Session::default());
        let screen = t.screen();
        let top = usize::from(t.app.areas().0.y);
        let arrowed: Vec<usize> = (0..7).filter(|&row| screen[top + row].contains('▾')).collect();
        assert_eq!(arrowed, [0, 1, 5], "impl, one, two — not the calls: {screen:#?}");
    }

    #[test]
    fn clicking_an_arrow_folds_and_clicking_again_unfolds() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        t.click_arrow(1, KeyModifiers::NONE);
        assert_eq!(t.folded(), [(1, 4)]);
        let screen = t.screen();
        let top = usize::from(t.app.areas().0.y);
        assert!(screen[top + 1].contains('▸') && screen[top + 1].contains('⋯'), "{screen:#?}");
        assert!(screen[top + 2].contains("fn two"), "the next line moves up: {screen:#?}");

        t.click_arrow(1, KeyModifiers::NONE);
        assert!(t.folded().is_empty());
    }

    #[test]
    fn alt_click_folds_every_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        t.click_arrow(1, KeyModifiers::ALT);
        assert_eq!(t.folded(), [(1, 4), (5, 7)], "both methods, not the impl");
    }

    #[test]
    fn a_click_below_a_fold_lands_on_the_line_drawn_there() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        t.click_arrow(1, KeyModifiers::NONE);
        // Row 2 now shows `    fn two() {`, line 5.
        let (text, _) = t.app.areas();
        let column = text.x + t.app.gutter_width() + 7;
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: text.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        t.app.handle_at(Event::Mouse(down), Instant::now() + Duration::from_secs(99));
        let buffer = &t.app.doc().buffer;
        let head = buffer.selections().primary().head;
        assert_eq!(buffer.line_of(head), 5, "not line 2, which is folded away");
        assert_eq!(t.folded(), [(1, 4)], "and the fold stays shut");
    }

    #[test]
    fn a_caret_moved_into_a_fold_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        t.app.run(Command::FoldAll);
        assert!(!t.folded().is_empty());
        // Down from the top steps over the fold; Right off a header's end
        // walks into it.
        let end = t.app.doc().buffer.line_end(0);
        t.app
            .doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(end)));
        t.app.handle(Event::Key(KeyEvent::from(KeyCode::Right)));
        assert!(!t.app.doc().buffer.is_folded(0), "the impl opened around the caret");
    }

    #[test]
    fn the_commands_fold_around_the_caret_and_open_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        let at = t.app.doc().buffer.line_start(2) + 4;
        t.app
            .doc_mut()
            .buffer
            .set_selections(nun_core::Selections::single(nun_core::Range::caret(at)));
        t.app.run(Command::Fold);
        assert_eq!(t.folded(), [(1, 4)], "the innermost region around the caret");
        t.app.run(Command::Fold);
        assert_eq!(t.folded(), [(0, 8), (1, 4)], "and then the one around that");
        t.app.run(Command::Unfold);
        assert_eq!(t.folded(), [(1, 4)]);
        t.app.run(Command::UnfoldAll);
        assert!(t.folded().is_empty());
    }

    #[test]
    fn the_view_scrolls_by_lines_in_view() {
        // With the impl folded, the lines in view are 0, then 9 onwards.
        let dir = tempfile::tempdir().unwrap();
        let mut t = Tester::new(&dir, crate::session::Session::default());
        t.click_arrow(0, KeyModifiers::NONE);
        let down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        t.app.handle(Event::Mouse(down));
        assert_eq!(t.app.scroll(), 11, "three lines in view: 9, 10, 11");
    }

    #[test]
    fn folds_come_back_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state").join("session");
        let mut t = Tester::new(&dir, crate::session::Session::load(state.clone()));
        t.click_arrow(5, KeyModifiers::NONE);
        assert_eq!(t.folded(), [(5, 7)]);
        t.app.save_session().unwrap();
        drop(t);

        let t = Tester::new(&dir, crate::session::Session::load(state));
        assert_eq!(t.folded(), [(5, 7)], "put back once the parse says it is still a region");
    }

    #[test]
    fn a_remembered_fold_that_is_no_longer_a_region_is_left_open() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("session");
        let mut session = crate::session::Session::load(state);
        // Line 2 is a call, not a region.
        session.remember(&dir.path().join("point.rs"), vec![2, 5]);
        let t = Tester::new(&dir, session);
        assert_eq!(t.folded(), [(5, 7)], "only the one that still is");
    }

    fn range(header: u32, last: u32) -> FoldRange {
        FoldRange { header, last }
    }

    #[test]
    fn siblings_share_the_region_around_them() {
        // impl 0..9 { fn 1..3, fn 4..8 { inner 5..6 } }, fn 10..12
        let ranges = [range(0, 9), range(1, 3), range(4, 8), range(5, 6), range(10, 12)];
        assert_eq!(siblings(&ranges, 1), [range(1, 3), range(4, 8)], "the methods");
        assert_eq!(siblings(&ranges, 0), [range(0, 9), range(10, 12)], "the top level");
        assert_eq!(siblings(&ranges, 3), [range(5, 6)], "an only child");
    }
}
