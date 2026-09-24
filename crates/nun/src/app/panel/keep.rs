//! The terminal panel, written down with the session and put back.
//!
//! What is kept is the panel's shape — whether it was showing, how tall it
//! was dragged, its tabs, the splits in each, which tab was showing and
//! which terminal of each had the keyboard — and the directory each shell was
//! in: the one it last reported with OSC 7, or where it started. It goes in
//! the session file as `[panels.terminal]`:
//!
//! ```toml
//! [panels.terminal]
//! visible = true
//! height = 12
//! active = 1
//!
//! [[panels.terminal.tabs]]
//! focus = 0
//! dirs = ["/home/me/project"]
//!
//! [[panels.terminal.tabs]]
//! focus = 1
//! dirs = ["/home/me/project/web", "/home/me/project/api"]
//! ```
//!
//! A running program cannot be brought back, so each terminal comes back as
//! a fresh shell in its directory. Scrollback is not kept at all: it is
//! whatever went past in a shell — tokens, passwords typed where echo was not
//! off, the output of `env` — and the session file is plain text on disk that
//! outlives it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Group;
use crate::app::App;

/// The kind the panel is written down under, in the session's panels.
pub const KIND: &str = "terminal";

/// The panel as it is written down.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app) struct Kept {
    /// Whether it was showing.
    #[serde(default)]
    visible: bool,
    /// Rows it was dragged to, when it had been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u16>,
    /// Which tab was showing.
    #[serde(default)]
    active: usize,
    /// Its tabs, in order.
    #[serde(default)]
    tabs: Vec<KeptTab>,
}

/// One tab: its terminals left to right, by the directory each was in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct KeptTab {
    /// Which of them had, or last had, the keyboard.
    #[serde(default)]
    focus: usize,
    #[serde(default)]
    dirs: Vec<PathBuf>,
}

impl App {
    /// The panel as it would be written down: `None` with no terminal in it,
    /// so that a session without one has no table for it.
    pub(in crate::app) fn terminal_snapshot(&self) -> Option<toml::Table> {
        let kept = match &self.panel.kept {
            // Not put back yet: kept as it was, rather than lost.
            Some(kept) => kept.clone(),
            None => self.keep_panel()?,
        };
        toml::Table::try_from(kept).ok()
    }

    fn keep_panel(&self) -> Option<Kept> {
        let panel = &self.panel;
        let mut tabs = Vec::new();
        let mut active = 0;
        for (index, group) in panel.groups.iter().enumerate() {
            let mut dirs = Vec::new();
            let mut focus = 0;
            for (at, id) in group.terms.iter().enumerate() {
                let Some(term) = panel.term(*id) else { continue };
                let dir = term.emulator.cwd().unwrap_or(&term.cwd);
                // TOML holds only UTF-8. A shell somewhere that cannot be
                // written down is left out, as a file there would be.
                if dir.to_str().is_none() {
                    continue;
                }
                if at <= group.focus {
                    focus = dirs.len();
                }
                dirs.push(dir.to_path_buf());
            }
            if dirs.is_empty() {
                continue;
            }
            if index <= panel.active {
                active = tabs.len();
            }
            tabs.push(KeptTab { focus, dirs });
        }
        if tabs.is_empty() {
            return None;
        }
        Some(Kept { visible: panel.visible, height: panel.height, active, tabs })
    }

    /// Put the panel back as `table` describes, once shells can be started:
    /// now, if they can, or when the panel is attached. A table that is not
    /// the shape the panel writes is dropped, with a notice.
    pub(in crate::app) fn restore_terminal(&mut self, table: &toml::Table) {
        match table.clone().try_into::<Kept>() {
            Ok(kept) => {
                self.panel.kept = Some(kept);
                self.start_kept();
            }
            Err(_) => self.warn(
                "The terminal panel's part of the last session could not be read, so it was not restored.",
            ),
        }
    }

    /// Start a fresh shell for each terminal the last session had, in the
    /// directory it was in. One whose directory has gone since starts where
    /// a new shell would, and a notice says so.
    pub(super) fn start_kept(&mut self) {
        if self.panel.post.is_none() {
            return;
        }
        let Some(kept) = self.panel.kept.take() else { return };
        let home = self
            .panel
            .start
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut gone = Vec::new();
        let mut failed = None;
        let mut active = self.panel.active;
        for (index, tab) in kept.tabs.iter().enumerate() {
            let mut terms = Vec::new();
            for dir in &tab.dirs {
                let dir = if dir.is_absolute() && dir.is_dir() {
                    dir.clone()
                } else {
                    gone.push(dir.clone());
                    home.clone()
                };
                match self.start_shell(dir) {
                    Ok(id) => terms.push(id),
                    Err(why) => failed = Some(why),
                }
            }
            if terms.is_empty() {
                continue;
            }
            if index <= kept.active {
                active = self.panel.groups.len();
            }
            let focus = tab.focus.min(terms.len() - 1);
            self.panel.groups.push(Group { terms, focus });
        }
        if let Some(why) = failed {
            self.warn(why);
        }
        if !gone.is_empty() {
            self.warn(gone_notice(&gone, &home));
        }
        if self.panel.groups.is_empty() {
            // Nothing could be started: the next session may do better, so
            // what was kept is written down again rather than lost.
            if !kept.tabs.is_empty() {
                self.panel.kept = Some(kept);
            }
            return;
        }
        self.panel.active = active;
        self.panel.visible = kept.visible;
        self.panel.height = kept.height;
        self.relayout();
    }
}

/// What to say about terminals whose directories are gone.
fn gone_notice(gone: &[PathBuf], home: &Path) -> String {
    let home = home.display();
    match gone {
        [one] => format!(
            "A terminal was in {} last time, which is gone, so it started in {home}.",
            one.display()
        ),
        many => {
            let names: Vec<String> = many.iter().map(|dir| dir.display().to_string()).collect();
            format!(
                "{} terminals were in folders that are gone, so they started in {home}: {}.",
                many.len(),
                names.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nun_core::Buffer;
    use nun_term::Report;
    use nun_theme::{Probe, derive};
    use nun_ui::Palette;
    use ratatui::layout::Rect;

    use super::*;
    use crate::app::{Focus, Outcome};
    use crate::commands::{Command, KeySet, defaults};

    /// An editor whose terminals start in `start` and run a shell that
    /// waits, once attached.
    fn editor() -> App {
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 80, 24));
        app.panel.program = Some(vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()]);
        app
    }

    fn attach(app: &mut App, start: &Path) {
        app.attach_terminal(start.to_path_buf(), Arc::new(|_| {}));
    }

    /// Each tab's terminals, by the directory each started in.
    fn dirs(app: &App) -> Vec<Vec<PathBuf>> {
        let panel = &app.panel;
        let dir = |id| panel.term(id).map(|term| term.cwd.clone()).unwrap_or_default();
        panel.groups.iter().map(|group| group.terms.iter().map(|id| dir(*id)).collect()).collect()
    }

    /// Tell the terminal with the keyboard, as its shell would, that it is
    /// in `dir` now.
    fn cd(app: &mut App, dir: &Path) {
        let id = app.panel.focused().unwrap();
        let bytes = format!("\x1b]7;file://{}\x07", dir.display()).into_bytes();
        app.terminal_report(Report::Output { id, bytes });
    }

    #[test]
    fn tabs_splits_and_directories_come_back_as_fresh_shells() {
        let (home, web, api) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let mut app = editor();
        attach(&mut app, home.path());
        assert_eq!(app.terminal_snapshot(), None, "no terminal, no table");
        app.run(Command::NewTerminal);
        app.run(Command::NewTerminal);
        cd(&mut app, web.path());
        app.run(Command::SplitTerminal);
        cd(&mut app, api.path());
        app.run(Command::NextTerminal);
        app.run(Command::NextTerminal);
        app.run(Command::NextTerminal);
        app.panel.height = Some(9);
        let table = app.terminal_snapshot().unwrap();
        app.shutdown_terminals();

        let mut again = editor();
        again.restore_terminal(&table);
        assert!(again.panel.groups.is_empty(), "nothing starts before the panel is attached");
        assert_eq!(again.terminal_snapshot().as_ref(), Some(&table), "and nothing is lost");
        attach(&mut again, home.path());
        assert_eq!(
            dirs(&again),
            [
                vec![home.path().to_path_buf()],
                vec![web.path().to_path_buf(), api.path().to_path_buf()]
            ]
        );
        assert_eq!(again.panel.active, 1);
        assert_eq!(again.panel.groups[1].focus, 1);
        assert!(again.panel.visible);
        assert_eq!(again.panel.height, Some(9));
        assert_eq!(again.focus, Focus::Editor, "the keyboard stays with the editor");
        assert_eq!(again.terminal_snapshot(), Some(table));
        again.shutdown_terminals();
    }

    #[test]
    fn a_hidden_panel_comes_back_hidden() {
        let home = tempfile::tempdir().unwrap();
        let mut app = editor();
        attach(&mut app, home.path());
        app.run(Command::NewTerminal);
        assert_eq!(app.run(Command::HideTerminal), Outcome::Redraw);
        let table = app.terminal_snapshot().unwrap();
        app.shutdown_terminals();

        let mut again = editor();
        again.restore_terminal(&table);
        attach(&mut again, home.path());
        assert_eq!(dirs(&again), [[home.path().to_path_buf()]]);
        assert!(!again.panel.visible);
        assert!(again.panel_area().is_none());
        again.shutdown_terminals();
    }

    #[test]
    fn a_directory_gone_since_starts_where_a_new_shell_would_with_a_notice() {
        let (home, kept) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let gone = kept.path().join("gone");
        let table = toml::Table::try_from(Kept {
            visible: true,
            height: None,
            active: 0,
            tabs: vec![KeptTab { focus: 0, dirs: vec![gone.clone(), kept.path().to_path_buf()] }],
        })
        .unwrap();

        let mut again = editor();
        again.restore_terminal(&table);
        attach(&mut again, home.path());
        assert_eq!(dirs(&again), [[home.path().to_path_buf(), kept.path().to_path_buf()]]);
        let notice = again.shown_message().unwrap();
        assert!(notice.contains(&gone.display().to_string()), "{notice}");
        again.shutdown_terminals();
    }

    #[test]
    fn a_table_of_the_wrong_shape_is_dropped_with_a_notice() {
        let table: toml::Table = toml::from_str("tabs = 3").unwrap();
        let mut app = editor();
        app.restore_terminal(&table);
        assert!(app.shown_message().unwrap().contains("terminal"));
        assert_eq!(app.terminal_snapshot(), None);
    }

    #[test]
    fn fields_left_out_are_their_defaults() {
        let home = tempfile::tempdir().unwrap();
        let text = format!("[[tabs]]\ndirs = [{:?}]\n", home.path().to_str().unwrap());
        let mut app = editor();
        app.restore_terminal(&toml::from_str(&text).unwrap());
        attach(&mut app, home.path());
        assert_eq!(dirs(&app), [[home.path().to_path_buf()]]);
        assert!(!app.panel.visible);
        app.shutdown_terminals();
    }
}
