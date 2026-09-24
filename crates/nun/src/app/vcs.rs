//! Git: the file tree's colours, from a status worked out off this thread.
//!
//! The editor never waits on git. It asks for a status when the folder opens,
//! after a save, when the watcher sees a change and when the terminal gets
//! focus back, and draws whatever the latest answer said. On a large
//! repository the tree is drawn first and coloured when the walk is done;
//! asks that arrive while one is running fold into a single walk.

use nun_vcs::{Reply, Request, Vcs};

use super::{App, Outcome};

impl App {
    /// Start git, once the editor has somewhere to post its answers.
    pub fn attach_vcs(&mut self, vcs: Vcs) {
        self.vcs = Some(vcs);
        let ids: Vec<_> = self.docs.iter().map(|document| document.id).collect();
        for id in ids {
            self.vcs_open(id);
        }
        self.refresh_status();
    }

    /// Ask for the tree's status again, and every open document's hunks:
    /// whatever made the one stale may have changed the index too.
    pub(super) fn refresh_status(&self) {
        let Some(vcs) = &self.vcs else { return };
        vcs.send(Request::Refresh);
        if let Some(sidebar) = &self.sidebar {
            vcs.send(Request::Status(sidebar.tree.root().to_path_buf()));
        }
    }

    /// Something git worked out.
    pub(super) fn vcs_reply(&mut self, reply: Reply) -> Outcome {
        let reply = match self.gutter_reply(reply) {
            Ok(outcome) => return outcome,
            Err(reply) => reply,
        };
        let Reply::Status { root, status } = reply else {
            return Outcome::Continue;
        };
        let Some(sidebar) = self.sidebar.as_mut() else { return Outcome::Continue };
        // An answer for a folder the sidebar has since moved away from.
        if root != sidebar.tree.root() {
            return Outcome::Continue;
        }
        match status {
            Ok(status) => {
                sidebar.status_failed = false;
                // Compared by content: most walks find what the last one did.
                if sidebar.status == status {
                    return Outcome::Continue;
                }
                sidebar.status = status;
                Outcome::Redraw
            }
            Err(error) => {
                let first = !sidebar.status_failed;
                sidebar.status_failed = true;
                sidebar.status = None;
                if first {
                    self.warn(format!("Git status is unavailable: {error}"));
                }
                Outcome::Redraw
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyModifiers};
    use nun_core::Buffer;
    use nun_theme::{Probe, derive};
    use nun_ui::{Event, Palette};
    use nun_vcs::FileStatus;
    use ratatui::layout::Rect;
    use tempfile::TempDir;

    use super::*;
    use crate::commands::{KeySet, defaults};

    /// Git with nothing from the environment: run from a git hook, `GIT_DIR`
    /// points at the repository being pushed, and would be what changes.
    fn git(dir: &Path, args: &[&str]) -> bool {
        let mut git = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                git.env_remove(name);
            }
        }
        git.arg("-C")
            .arg(dir)
            .args(["-c", "user.name=nun", "-c", "user.email=nun@example.com"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// An editor over `dir` with git attached, and where git's answers land.
    fn app(dir: &Path) -> (App, Receiver<Reply>) {
        let mut app = App::new(
            Buffer::new(),
            Palette::new(derive(&Probe::builtin_dark())),
            defaults(KeySet::Full),
        );
        app.set_viewport(Rect::new(0, 0, 70, 12));
        app.open_folder(dir.to_path_buf(), dir.join(".trash"), true, Box::new(|_| {}));
        let (send, replies) = mpsc::channel();
        app.attach_vcs(Vcs::new(Box::new(move |reply| {
            let _ = send.send(reply);
        })));
        (app, replies)
    }

    /// Hand git's answers to the editor until the status comes, and say
    /// what that one did. The gutter's answers may come before it.
    fn answer(app: &mut App, replies: &Receiver<Reply>) -> Outcome {
        loop {
            let reply = replies.recv_timeout(Duration::from_secs(20)).expect("git answers");
            let status = matches!(reply, Reply::Status { .. });
            let outcome = app.handle(Event::Vcs(reply));
            if status {
                return outcome;
            }
        }
    }

    fn status_of(app: &App, path: &Path) -> Option<FileStatus> {
        app.sidebar.as_ref()?.status.as_ref()?.of(path)
    }

    #[test]
    fn the_tree_learns_what_changed_and_learns_again_after_a_save() {
        let dir = TempDir::new().unwrap();
        if !git(dir.path(), &["init", "-q"]) {
            return;
        }
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        assert!(git(dir.path(), &["add", "a.txt"]));
        assert!(git(dir.path(), &["commit", "-qm", "a"]));
        std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();

        let (mut app, replies) = app(dir.path());
        assert_eq!(answer(&mut app, &replies), Outcome::Redraw);
        assert_eq!(status_of(&app, &dir.path().join("new.txt")), Some(FileStatus::Added));
        assert_eq!(status_of(&app, &dir.path().join("a.txt")), None);

        // Nothing changed: the same answer again draws nothing.
        app.refresh_status();
        assert_eq!(answer(&mut app, &replies), Outcome::Continue);

        app.open_file(&dir.path().join("a.txt"));
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        app.handle(Event::Key(key));
        assert_eq!(app.buffer().text().to_string(), "ba\n");
        app.save();
        answer(&mut app, &replies);
        assert_eq!(status_of(&app, &dir.path().join("a.txt")), Some(FileStatus::Modified));
    }

    #[test]
    fn a_folder_in_no_repository_is_coloured_by_nothing_and_says_nothing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let (mut app, replies) = app(dir.path());
        answer(&mut app, &replies);
        assert!(app.sidebar.as_ref().unwrap().status.is_none());
        assert!(app.notices.is_empty());
    }
}
