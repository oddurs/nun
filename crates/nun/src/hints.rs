//! Saying, once, where the terminal keeps a click for itself.
//!
//! Go to definition is a Ctrl-click, and some terminals never report one: they
//! open a context menu instead. nun cannot see a click that never arrives, so
//! the only thing it can do is say so up front, once, and name the key that
//! does the same thing.
//!
//! Which terminal it is comes from what it said to the start-up probe, never
//! from `$TERM` or `$TERM_PROGRAM`, which are inherited through ssh and tmux
//! and are as often wrong as right:
//!
//! * iTerm2 names itself in its XTVERSION reply. By default it keeps
//!   Ctrl-click for its menu, behind a setting that can turn that off, and it
//!   never reports an Option-click at all.
//! * Terminal.app answers neither XTVERSION nor the Kitty keyboard query, and
//!   is the only terminal in the support matrix that does neither. It has no
//!   setting for Ctrl-click. Saying nothing to both is the signal; an older
//!   terminal saying nothing to both gets the same hint, which costs it a line
//!   it did not need.
//! * No answer at all — the probe timed out — says nothing about the
//!   terminal, so nun says nothing either. Nor does it for tmux: the reply is
//!   tmux's own, and whether the click gets through depends on the terminal
//!   outside it, which the probe cannot see.
//!
//! Having been seen, a hint is remembered in the state directory and not
//! shown again.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

/// What the terminal does with a Ctrl-click, as far as the probe can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlClick {
    /// Reported, or nothing is known to say otherwise.
    Reported,
    /// iTerm2: kept for its menu unless a setting says otherwise.
    ITerm2,
    /// Kept for the terminal's own menu, with no way to change that.
    Taken,
}

impl CtrlClick {
    /// Work it out from the XTVERSION reply and the Kitty keyboard answer:
    /// `Some(false)` there means the terminal answered the probe without
    /// knowing the protocol, `None` that it did not answer in time.
    #[must_use]
    pub fn detect(version: Option<&str>, kitty_keyboard: Option<bool>) -> Self {
        match version {
            Some(name) if name.starts_with("iTerm2 ") => Self::ITerm2,
            None if kitty_keyboard == Some(false) => Self::Taken,
            _ => Self::Reported,
        }
    }

    /// What `nun --capabilities` says of it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Reported => "reported, as far as nun can tell",
            Self::ITerm2 => {
                "iTerm2 keeps it for its menu unless \"Ctrl-click reported to apps\" is on"
            }
            Self::Taken => "kept by this terminal for its menu",
        }
    }

    /// The hint to show, and the name it is remembered by once seen. `key` is
    /// how go to definition is reached from the keyboard, as bound.
    #[must_use]
    pub fn hint(self, key: &str) -> Option<Hint> {
        let (name, message) = match self {
            Self::Reported => return None,
            Self::ITerm2 => (
                "ctrl-click-iterm2",
                format!(
                    "iTerm2 keeps Ctrl-click for its menu. To click through to definitions, \
                     turn on Settings > General > Pointer > \"Ctrl-click reported to apps\"; \
                     {key} goes to one from the caret."
                ),
            ),
            Self::Taken => (
                "ctrl-click-taken",
                format!(
                    "This terminal keeps Ctrl-click for its menu, so go to definition is \
                     {key} from the caret here."
                ),
            ),
        };
        Some(Hint { name, message })
    }
}

/// Something worth saying once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// What it is remembered by.
    pub name: &'static str,
    /// What the status line says.
    pub message: String,
}

/// A hint not yet seen, waiting to be shown and then remembered.
#[derive(Debug)]
pub struct Unseen {
    hint: Hint,
    seen: Seen,
}

impl Unseen {
    /// `hint`, unless it has been seen before.
    #[must_use]
    pub fn of(hint: Hint) -> Option<Self> {
        let seen = Seen::load();
        (!seen.contains(&hint)).then_some(Self { hint, seen })
    }

    /// What to say.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.hint.message
    }

    /// Remember it as seen, so it is not said again. Failing costs only the
    /// hint being said again, so callers are free to ignore it.
    ///
    /// # Errors
    ///
    /// Whatever writing the state file ran into.
    pub fn remember(mut self) -> io::Result<()> {
        self.seen.record(&self.hint)
    }
}

/// The hints already seen, and the file they are kept in.
#[derive(Debug, Default)]
pub struct Seen {
    /// Where to write them. `None` keeps them in memory only.
    path: Option<PathBuf>,
    names: BTreeSet<String>,
}

impl Seen {
    /// The hints seen so far: `hints` beside the session file, one name per
    /// line. Nothing, where there is no state directory or no file yet.
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = crate::session::Session::default_path().map(|p| p.with_file_name("hints"))
        else {
            return Self::default();
        };
        let names = read(&path);
        Self { path: Some(path), names }
    }

    /// Whether `hint` has been shown before.
    #[must_use]
    pub fn contains(&self, hint: &Hint) -> bool {
        self.names.contains(hint.name)
    }

    /// Remember that `hint` has been seen.
    ///
    /// # Errors
    ///
    /// Whatever creating the directory or writing the file ran into.
    pub fn record(&mut self, hint: &Hint) -> io::Result<()> {
        self.names.insert(hint.name.to_string());
        let Some(path) = self.path.as_ref() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Laid over what is there now, which another nun may have added to.
        let mut names = read(path);
        names.extend(self.names.iter().cloned());
        let text: String = names.iter().flat_map(|name| [name.as_str(), "\n"]).collect();
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&temporary, text)?;
        fs::rename(&temporary, path)
    }
}

fn read(path: &std::path::Path) -> BTreeSet<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterm2_is_named_by_its_version_reply() {
        assert_eq!(CtrlClick::detect(Some("iTerm2 3.5.11"), Some(false)), CtrlClick::ITerm2);
        let hint = CtrlClick::ITerm2.hint("F12").unwrap();
        assert!(hint.message.contains("Ctrl-click reported to apps"), "{}", hint.message);
        assert!(hint.message.contains("F12"), "{}", hint.message);
    }

    #[test]
    fn silence_to_both_questions_is_a_terminal_that_keeps_the_click() {
        // Terminal.app: only the device-attributes sentinel comes back.
        assert_eq!(CtrlClick::detect(None, Some(false)), CtrlClick::Taken);
        let hint = CtrlClick::Taken.hint("Ctrl+K D").unwrap();
        assert!(hint.message.contains("Ctrl+K D"), "{}", hint.message);
    }

    #[test]
    fn terminals_that_report_the_click_get_no_hint() {
        for name in
            ["ghostty 1.2.0", "kitty(0.39.1)", "WezTerm 20240203-110809-5046fc22", "foot(1.20.2)"]
        {
            let found = CtrlClick::detect(Some(name), Some(true));
            assert_eq!(found, CtrlClick::Reported, "{name}");
            assert_eq!(found.hint("F12"), None);
        }
        // Alacritty has no XTVERSION but does speak the keyboard protocol.
        assert_eq!(CtrlClick::detect(None, Some(true)), CtrlClick::Reported);
    }

    #[test]
    fn no_answer_in_time_says_nothing() {
        assert_eq!(CtrlClick::detect(None, None), CtrlClick::Reported);
    }

    #[test]
    fn tmux_says_nothing_about_the_terminal_outside_it() {
        assert_eq!(CtrlClick::detect(Some("tmux 3.5a"), Some(false)), CtrlClick::Reported);
    }

    #[test]
    fn a_seen_hint_is_remembered_across_loads() {
        let dir = std::env::temp_dir().join(format!("nun-hints-{}", std::process::id()));
        let path = dir.join("hints");
        let hint = CtrlClick::ITerm2.hint("F12").unwrap();
        let mut seen = Seen { path: Some(path.clone()), names: BTreeSet::new() };
        assert!(!seen.contains(&hint));
        seen.record(&hint).unwrap();
        let again = Seen { names: read(&path), path: Some(path) };
        assert!(again.contains(&hint));
        let _ = fs::remove_dir_all(dir);
    }
}
