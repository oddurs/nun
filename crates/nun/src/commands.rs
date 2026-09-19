//! Every command the editor has, and the keys bound to them.
//!
//! There are two key sets. **Basic** uses only keys every terminal can report —
//! Ctrl and Alt with letters, function keys, the arrows. **Full** adds what the
//! Kitty keyboard protocol makes reportable: Cmd, and Ctrl+Shift combinations a
//! legacy terminal folds into plain Ctrl. Full is Basic plus more, never
//! instead, so no command is reachable in only one of them — which is checked
//! below rather than trusted.
//!
//! Typing, the arrows, Home and End, Backspace and the like are editing keys
//! rather than commands. They are not in either table and cannot be rebound.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use nun_input::{Keymap, Sequence, parse_sequence};

/// Something the editor can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Command {
    /// Write the buffer to disk.
    Save,
    /// Undo the last change.
    Undo,
    /// Redo what was undone.
    Redo,
    /// Select the whole buffer.
    SelectAll,
    /// Leave, asking first when something is unsaved.
    Quit,
    /// Show the file tree, focus it, or hide it.
    ToggleSidebar,
    /// Create a file in the selected folder.
    NewFile,
    /// Create a folder in the selected folder.
    NewFolder,
    /// Rename the selected file or folder.
    Rename,
    /// Move the selected file or folder to the trash, undoably.
    Delete,
    /// Show or hide files the ignore rules leave out.
    ToggleIgnored,
    /// Close the tab being edited.
    CloseTab,
    /// Go to the next tab.
    NextTab,
    /// Go to the previous tab.
    PreviousTab,
}

impl Command {
    /// Every command, in the order `nun keys` lists them.
    pub const ALL: &[Self] = &[
        Self::Save,
        Self::Undo,
        Self::Redo,
        Self::SelectAll,
        Self::Quit,
        Self::ToggleSidebar,
        Self::NewFile,
        Self::NewFolder,
        Self::Rename,
        Self::Delete,
        Self::ToggleIgnored,
        Self::CloseTab,
        Self::NextTab,
        Self::PreviousTab,
    ];

    /// The name used in `[keys]` in `nun.toml`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Save => "file.save",
            Self::Undo => "edit.undo",
            Self::Redo => "edit.redo",
            Self::SelectAll => "edit.select_all",
            Self::Quit => "app.quit",
            Self::ToggleSidebar => "view.toggle_sidebar",
            Self::NewFile => "files.new_file",
            Self::NewFolder => "files.new_folder",
            Self::Rename => "files.rename",
            Self::Delete => "files.delete",
            Self::ToggleIgnored => "files.toggle_ignored",
            Self::CloseTab => "tab.close",
            Self::NextTab => "tab.next",
            Self::PreviousTab => "tab.previous",
        }
    }

    /// What a person calls it.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::SelectAll => "Select all",
            Self::Quit => "Quit",
            Self::ToggleSidebar => "Toggle the file tree",
            Self::NewFile => "New file",
            Self::NewFolder => "New folder",
            Self::Rename => "Rename",
            Self::Delete => "Delete",
            Self::ToggleIgnored => "Show or hide ignored files",
            Self::CloseTab => "Close tab",
            Self::NextTab => "Next tab",
            Self::PreviousTab => "Previous tab",
        }
    }

    /// The command named `id`, if there is one.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|command| command.id() == id)
    }
}

/// Which key set is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySet {
    /// The Kitty keyboard protocol was negotiated.
    Full,
    /// It was not; only keys every terminal reports.
    Basic,
}

/// Bindings every terminal can report.
const BASIC: &[(&str, Command)] = &[
    ("ctrl+s", Command::Save),
    ("ctrl+z", Command::Undo),
    ("ctrl+y", Command::Redo),
    ("ctrl+a", Command::SelectAll),
    ("ctrl+q", Command::Quit),
    ("ctrl+w", Command::CloseTab),
    ("ctrl+pagedown", Command::NextTab),
    ("ctrl+pageup", Command::PreviousTab),
    ("ctrl+b", Command::ToggleSidebar),
    // File operations are chords on Ctrl+K rather than Alt bindings: on a Mac,
    // Option types characters unless the terminal is set to send it as Meta.
    ("ctrl+k n", Command::NewFile),
    ("ctrl+k shift+n", Command::NewFolder),
    ("f2", Command::Rename),
    ("ctrl+k delete", Command::Delete),
    ("ctrl+k i", Command::ToggleIgnored),
];

/// Bindings that need the Kitty keyboard protocol, added over [`BASIC`].
const FULL: &[(&str, Command)] = &[
    ("cmd+s", Command::Save),
    ("cmd+z", Command::Undo),
    // A legacy terminal reports Ctrl+Shift+Z as Ctrl+Z. Bound in Basic it
    // would undo when the user asked to redo.
    ("ctrl+shift+z", Command::Redo),
    ("cmd+shift+z", Command::Redo),
    ("cmd+a", Command::SelectAll),
    ("cmd+q", Command::Quit),
    ("cmd+w", Command::CloseTab),
    ("cmd+b", Command::ToggleSidebar),
];

/// The default bindings for `set`.
#[must_use]
pub fn defaults(set: KeySet) -> Keymap<Command> {
    let mut keymap = Keymap::new();
    let tables: &[&[(&str, Command)]] = match set {
        KeySet::Basic => &[BASIC],
        KeySet::Full => &[BASIC, FULL],
    };
    for (sequence, command) in tables.iter().copied().flatten() {
        let keys = parse_sequence(sequence).expect("a built-in binding is well formed");
        keymap.bind(keys, *command);
    }
    keymap
}

/// The defaults for `set` with the user's `[keys]` added over them.
///
/// Additive: a user binding takes its sequence and leaves every other default
/// alone. A binding that cannot be read is reported and skipped, never fatal.
#[must_use]
pub fn keymap(set: KeySet, user: &BTreeMap<String, String>) -> (Keymap<Command>, Vec<String>) {
    let mut keymap = defaults(set);
    let mut problems = Vec::new();
    for (sequence, id) in user {
        let Some(command) = Command::from_id(id) else {
            problems.push(format!("keys.\"{sequence}\": no command called `{id}`"));
            continue;
        };
        match parse_sequence(sequence) {
            Ok(keys) => keymap.bind(keys, command),
            Err(error) => problems.push(format!("keys.\"{sequence}\": {error}")),
        }
    }
    (keymap, problems)
}

/// Both key sets as a Markdown table: what `nun keys` prints and what
/// `docs/keys.md` holds.
#[must_use]
pub fn reference() -> String {
    let full = defaults(KeySet::Full);
    let basic = defaults(KeySet::Basic);
    let shown = |keymap: &Keymap<Command>, command: Command| {
        let sequences = keymap.sequences_for(&command);
        sequences.iter().map(|keys| format!("`{}`", Sequence(keys))).collect::<Vec<_>>().join(", ")
    };

    let mut out = String::new();
    out.push_str("# Keys\n\n");
    out.push_str(
        "nun has two key sets. The **full** set is used when the terminal speaks the\n\
         Kitty keyboard protocol, which is what lets it report Cmd and tell\n\
         Ctrl+Shift+Z from Ctrl+Z. The **basic** set is used everywhere else, and\n\
         nun says so once in the status line when it falls back to it. Every command\n\
         is in both.\n\n\
         Typing, the arrows, Home, End, Page Up and Down, Backspace, Delete, Enter,\n\
         Tab and Esc are editing keys and work the same in both.\n\n\
         Add or replace bindings in `~/.config/nun/nun.toml`; the defaults you do not\n\
         mention stay as they are:\n\n\
         ```toml\n[keys]\n\"ctrl+k ctrl+s\" = \"file.save\"\n```\n\n\
         This file is generated from the source by `nun keys`. Do not edit it by hand.\n\n",
    );
    out.push_str("| Command | Id | Full set | Basic set |\n");
    out.push_str("|---|---|---|---|\n");
    for &command in Command::ALL {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} |",
            command.title(),
            command.id(),
            shown(&full, command),
            shown(&basic, command)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nun_input::{Code, Key, Mods};

    #[test]
    fn every_command_is_bound_in_both_sets() {
        for set in [KeySet::Full, KeySet::Basic] {
            let keymap = defaults(set);
            for &command in Command::ALL {
                assert!(
                    !keymap.sequences_for(&command).is_empty(),
                    "{} has no binding in the {set:?} set",
                    command.id()
                );
            }
        }
    }

    #[test]
    fn the_basic_set_uses_nothing_a_legacy_terminal_cannot_report() {
        for (sequence, _) in BASIC {
            for key in parse_sequence(sequence).unwrap() {
                assert!(!key.mods.contains(Mods::CMD), "{sequence}: Cmd needs the protocol");
                let ctrl_shift = key.mods.contains(Mods::CTRL) && key.mods.contains(Mods::SHIFT);
                assert!(!ctrl_shift, "{sequence}: legacy terminals drop the Shift");
                let ambiguous = key.mods.contains(Mods::CTRL)
                    && matches!(key.code, Code::Char('i' | 'm' | '[' | 'h'));
                assert!(
                    !ambiguous,
                    "{sequence}: indistinguishable from Tab, Enter, Esc or Backspace"
                );
            }
        }
    }

    #[test]
    fn the_full_set_keeps_every_basic_binding() {
        let full = defaults(KeySet::Full);
        for (sequence, command) in BASIC {
            assert_eq!(full.get(&parse_sequence(sequence).unwrap()), Some(command), "{sequence}");
        }
    }

    #[test]
    fn command_ids_are_unique_and_round_trip() {
        for &command in Command::ALL {
            assert_eq!(Command::from_id(command.id()), Some(command));
        }
        let mut ids: Vec<_> = Command::ALL.iter().map(|command| command.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Command::ALL.len());
    }

    #[test]
    fn user_bindings_add_to_the_defaults_rather_than_replacing_them() {
        let user = BTreeMap::from([("ctrl+k ctrl+s".to_string(), "file.save".to_string())]);
        let (keymap, problems) = keymap(KeySet::Basic, &user);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keymap.get(&parse_sequence("ctrl+k ctrl+s").unwrap()), Some(&Command::Save));
        assert_eq!(
            keymap.get(&[Key::new(Code::Char('s'), Mods::CTRL)]),
            Some(&Command::Save),
            "the default is still there"
        );
    }

    #[test]
    fn a_user_binding_can_take_over_a_default_sequence() {
        let user = BTreeMap::from([("ctrl+w".to_string(), "file.save".to_string())]);
        let (keymap, _) = keymap(KeySet::Basic, &user);
        assert_eq!(keymap.get(&parse_sequence("ctrl+w").unwrap()), Some(&Command::Save));
    }

    #[test]
    fn bad_user_bindings_are_reported_and_skipped() {
        let user = BTreeMap::from([
            ("ctrl+s".to_string(), "file.explode".to_string()),
            ("ctrl+nope".to_string(), "file.save".to_string()),
        ]);
        let (keymap, problems) = keymap(KeySet::Basic, &user);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("no command called `file.explode`")));
        assert!(problems.iter().any(|p| p.contains("`nope`")));
        assert_eq!(
            keymap.get(&parse_sequence("ctrl+s").unwrap()),
            Some(&Command::Save),
            "the default survives a bad override"
        );
    }

    #[test]
    fn the_documented_key_sets_match_the_code() {
        let documented = include_str!("../../../docs/keys.md");
        assert_eq!(
            documented,
            reference(),
            "docs/keys.md is stale; regenerate it with `nun keys > docs/keys.md`"
        );
    }
}
