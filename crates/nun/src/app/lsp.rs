//! Language servers: keeping them told what the buffers hold, and showing how
//! they are.
//!
//! The handle never waits, so nothing here does either. Edits go over after
//! each event, in the order the buffer made them; what the servers say comes
//! back through the editor's channel like everything else. The status line
//! names the server of the file being edited and how it is, and clicking the
//! name restarts it — the way back from a server that crashed too often, and
//! the way to pick up one installed while nun was running.

use std::time::Duration;

use nun_lsp::{Handled, Lsp};
use nun_theme::Role;
use ratatui::buffer::Buffer as Cells;
use ratatui::layout::Rect;

use super::panes::DocId;
use super::{App, Outcome, Target};

/// How long the servers are given to exit when nun does, before they are
/// killed. Waited for after the terminal is back, so it is never a frozen
/// screen — at worst a prompt that takes a moment to come back.
pub const SHUTDOWN: Duration = Duration::from_secs(2);

impl App {
    /// Start talking to language servers, for the documents already open and
    /// every one opened after.
    pub fn attach_lsp(&mut self, lsp: Lsp) {
        self.lsp = Some(lsp);
        let ids: Vec<DocId> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.lsp_open(id);
        }
    }

    /// Follow a document, or follow it afresh: it was opened, reloaded, or
    /// renamed. A server starts if it is the first of its language and
    /// project; a document with no server is not followed, and its buffer
    /// keeps no edits for one.
    pub(super) fn lsp_open(&mut self, id: DocId) {
        let Some(lsp) = self.lsp.as_mut() else { return };
        let Some(document) = self.docs.iter_mut().find(|document| document.id == id) else {
            return;
        };
        // Whatever the journal holds is already in the text the server is
        // about to be sent whole; sent again as edits, it would be applied
        // twice.
        let _ = document.buffer.take_edits();
        let followed = if let Some(path) = document.buffer.path() {
            lsp.open(id, path, document.buffer.rope())
        } else {
            lsp.close(id);
            false
        };
        document.buffer.keep_edits(followed);
    }

    /// Stop following a document that has gone.
    pub(super) fn lsp_close(&mut self, id: DocId) {
        if let Some(lsp) = self.lsp.as_mut() {
            lsp.close(id);
        }
    }

    /// Send every edit made since the last time, for every document.
    ///
    /// All of them, not only the focused one's: a replace across the project
    /// edits documents nobody is looking at, and the server has to hear about
    /// those too.
    pub(super) fn lsp_flush(&mut self) {
        let Some(lsp) = self.lsp.as_mut() else { return };
        for document in &mut self.docs {
            let edits = document.buffer.take_edits();
            if !edits.is_empty() {
                lsp.change(document.id, edits, document.buffer.rope());
            }
        }
    }

    /// The focused document was written to disk.
    pub(super) fn lsp_saved(&mut self) {
        self.lsp_flush();
        let id = self.doc().id;
        if let Some(lsp) = self.lsp.as_mut() {
            lsp.save(id);
        }
    }

    /// A server said something.
    pub(super) fn lsp_event(&mut self, event: nun_lsp::Event) -> Outcome {
        let Some(lsp) = self.lsp.as_mut() else { return Outcome::Continue };
        match lsp.handle(event) {
            // Nothing asks a server anything yet. The features that do —
            // completion, hover, go to definition, rename — take their answers
            // from here, matching each by the id `request` returned.
            Handled::Nothing | Handled::Response(_) => Outcome::Continue,
            Handled::Redraw => Outcome::Redraw,
            Handled::Notice(notice) => {
                self.warn(notice);
                Outcome::Redraw
            }
        }
    }

    /// Restart the server of the file being edited.
    pub(super) fn lsp_restart(&mut self) -> Outcome {
        let id = self.doc().id;
        let restarting = self.lsp.as_mut().and_then(|lsp| lsp.restart(id)).map(str::to_string);
        self.message = Some(match restarting {
            Some(name) => format!("Restarting {name}…"),
            None => "This file has no language server to restart.".into(),
        });
        Outcome::Redraw
    }

    /// What the status line says about the focused file's server, and
    /// whether it is in trouble.
    pub(super) fn lsp_label(&self) -> Option<(String, bool)> {
        let indicator = self.lsp.as_ref()?.indicator(self.doc().id)?;
        Some((format!(" {} ", indicator.label()), indicator.is_trouble()))
    }

    /// Draw the server's name in the status line.
    pub(super) fn render_lsp(&self, area: Rect, cells: &mut Cells) {
        let Some((label, trouble)) = self.lsp_label() else { return };
        let style = if self.hover.current() == Some(Target::StatusLsp) {
            self.palette.on(Role::Accent, Role::OnAccent)
        } else if trouble {
            self.palette.on(Role::Raised, Role::Error)
        } else {
            self.palette.on(Role::Raised, Role::Dim)
        };
        super::write_at(cells, area, area.x, &label, style);
    }

    /// Shut the servers down, giving them [`SHUTDOWN`] to go on their own.
    pub fn shutdown_lsp(&mut self) {
        if let Some(lsp) = self.lsp.as_mut() {
            lsp.shutdown(SHUTDOWN);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use nun_core::Buffer;
    use nun_lsp::ServerSpec;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};

    use super::*;

    /// An editor over `name` in a fresh folder, with `command` as the Rust
    /// server, and the channel its events come back on.
    fn editor(command: &str, args: &[&str]) -> (App, Receiver<nun_lsp::Event>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let (buffer, _) = Buffer::load(&path).unwrap();
        let mut app = App::new(
            buffer,
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 200, 8));
        let spec = ServerSpec {
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
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
        (app, events, dir)
    }

    /// Hand events to the editor until the status line says `wanted`.
    fn until_status(app: &mut App, events: &Receiver<nun_lsp::Event>, wanted: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !app.lsp_label().is_some_and(|(label, _)| label.contains(wanted)) {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = events.recv_timeout(left).unwrap_or_else(|_| {
                panic!("the status line never said {wanted:?}; it says {:?}", app.lsp_label())
            });
            app.handle(Event::Lsp(event));
        }
    }

    fn click_at(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn a_missing_server_is_named_in_the_status_line_and_a_click_tries_again() {
        let (mut app, events, _dir) = editor("nun-test-no-such-server", &[]);
        until_status(&mut app, &events, "not installed");
        assert!(
            app.notices
                .iter()
                .any(|notice| notice.contains("nun-test-no-such-server is not installed")),
            "configured by hand, so its absence is said: {:?}",
            app.notices
        );

        // Read, and put away.
        app.handle(Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            KeyModifiers::NONE,
        )));

        // Where the label is drawn is where the click lands.
        let status = app.areas().1;
        let label = app.status_parts(status).lsp.unwrap_or_else(|| {
            panic!("the label is laid out beside {:?} and {:?}", app.status(), app.lsp_label())
        });
        assert_eq!(
            app.hits.at(label.x + 1, label.y).map(|hit| hit.target),
            Some(Target::StatusLsp)
        );
        let mut cells = Cells::empty(app.viewport);
        app.render(app.viewport, &mut cells);
        let drawn: String =
            (label.x..label.right()).map(|x| cells[(x, label.y)].symbol()).collect();
        assert_eq!(drawn.trim(), "nun-test-no-such-server not installed");

        app.handle(click_at(label.x + 1, label.y));
        assert_eq!(app.message(), Some("Restarting nun-test-no-such-server…"));
        until_status(&mut app, &events, "starting");
    }

    #[test]
    fn edits_reach_the_server_and_a_crash_loop_ends_in_a_notice() {
        // A server that dies the moment it starts, every time.
        let (mut app, events, _dir) = editor("sh", &["-c", "exit 7"]);
        app.handle(Event::Paste("// typed\n".into()));
        assert!(
            app.lsp.as_ref().and_then(|lsp| lsp.version(0)).is_some_and(|version| version > 0),
            "the edit went over"
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !app.notices.iter().any(|notice| notice.contains("crashed 5 times")) {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = events.recv_timeout(left).expect("gave up in time");
            app.handle(Event::Lsp(event));
        }
        let (label, trouble) = app.lsp_label().unwrap();
        assert!(trouble, "{label}");
    }

    #[test]
    fn a_file_with_no_server_shows_nothing_and_keeps_no_edits() {
        let mut app = App::new(
            Buffer::from_text("notes"),
            Palette::new(derive(&Probe::builtin_dark())),
            crate::commands::defaults(crate::commands::KeySet::Full),
        );
        let lsp = Lsp::start(BTreeMap::new(), None, Box::new(|_| {})).unwrap();
        app.attach_lsp(lsp);
        app.handle(Event::Paste("more".into()));
        assert_eq!(app.lsp_label(), None);
        assert!(app.doc_mut().buffer.take_edits().is_empty(), "nobody to keep them for");
        assert_eq!(app.status_parts(app.areas().1).lsp, None);
    }
}
