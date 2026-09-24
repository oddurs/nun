//! Every setting nun reads: what it holds, and who may set it.
//!
//! One table, so that reading a file, reporting a typo, explaining a value
//! and deciding what a project may touch all agree on what exists. A setting
//! is added here or it does not exist.
//!
//! Some keys name something the file chooses — a language, a colour role, a
//! key sequence. `*` in a key stands for one such name, and a trailing `**`
//! for one or more, dotted: glyph roles have dots in them.

use std::fmt;

/// Who may set a setting.
///
/// This is the API for a setting that a project may carry. A project's
/// `.nun.toml` is inert until its directory is trusted; after that, a
/// [`Scope::Project`] setting in it applies whatever it says, and a
/// [`Scope::Trusted`] one applies only while the trust covers the value it has
/// now. Editing a `Trusted` value asks again; editing a `Project` one does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the person's own `nun.toml`: how nun looks, which keys do what,
    /// what the terminal is asked for. A project has no business with these.
    User,
    /// The person's file, or a trusted project's. Harmless whatever the
    /// value: indentation, line endings.
    Project,
    /// The person's file, or a trusted project's — and a change to its value
    /// in the project asks again. Anything that runs a program or rewrites
    /// files: language server commands, formatting on save.
    Trusted,
}

impl Scope {
    /// Whether a project file may carry it at all.
    #[must_use]
    pub const fn project_may_set(self) -> bool {
        !matches!(self, Self::User)
    }

    /// Whether a change to it in a project asks for trust again.
    #[must_use]
    pub const fn needs_trust(self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A whole number in a range, inclusive.
    Int {
        /// The least it may be.
        min: u64,
        /// The most it may be.
        max: u64,
    },
    /// One of a few words.
    OneOf(&'static [&'static str]),
    /// Any string.
    Text,
    /// A string with something in it: a program to run.
    Command,
    /// A list of strings.
    List,
}

impl Kind {
    /// What a value of this kind looks like, for a message.
    #[must_use]
    pub fn expected(self) -> String {
        match self {
            Self::Bool => "true or false".to_string(),
            Self::Int { min, max } => format!("a whole number between {min} and {max}"),
            Self::OneOf(words) => {
                let quoted: Vec<String> = words.iter().map(|word| format!("\"{word}\"")).collect();
                format!("one of {}", quoted.join(", "))
            }
            Self::Text => "a string".to_string(),
            Self::Command => "the name or path of a program".to_string(),
            Self::List => "a list of strings, like [\"--stdio\"]".to_string(),
        }
    }
}

/// One setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// Its dotted key, as in the file.
    pub key: &'static str,
    /// What it holds.
    pub kind: Kind,
    /// Who may set it.
    pub scope: Scope,
    /// What it does, in a sentence, for `nun config --explain`.
    pub about: &'static str,
}

const fn setting(key: &'static str, kind: Kind, scope: Scope, about: &'static str) -> Setting {
    Setting { key, kind, scope, about }
}

const INDENT_STYLES: &[&str] = &["tab", "space"];
const LINE_ENDINGS: &[&str] = &["lf", "crlf"];

/// Every setting there is.
pub const SETTINGS: &[Setting] = &[
    setting(
        "editor.tab_width",
        Kind::Int { min: 1, max: 16 },
        Scope::Project,
        "Columns a tab advances to.",
    ),
    setting(
        "editor.indent_style",
        Kind::OneOf(INDENT_STYLES),
        Scope::Project,
        "What Tab types: a tab, or spaces to the next indent stop. Unset types a tab.",
    ),
    setting(
        "editor.indent_size",
        Kind::Int { min: 1, max: 16 },
        Scope::Project,
        "Columns one level of indentation takes, when indenting with spaces.",
    ),
    setting(
        "editor.end_of_line",
        Kind::OneOf(LINE_ENDINGS),
        Scope::Project,
        "The line ending files are saved with. Unset keeps each file's own.",
    ),
    setting(
        "editor.charset",
        Kind::OneOf(&["utf-8", "utf-8-bom"]),
        Scope::Project,
        "Save as UTF-8 with no byte-order mark (removing one), or with one. Unset keeps each file's own.",
    ),
    setting(
        "editor.trim_trailing_whitespace",
        Kind::Bool,
        Scope::Project,
        "Remove spaces and tabs from the ends of lines on save.",
    ),
    setting(
        "editor.insert_final_newline",
        Kind::Bool,
        Scope::Project,
        "End the file with a line break on save, if it does not already.",
    ),
    setting(
        "theme.polarity",
        Kind::OneOf(&["auto", "dark", "light"]),
        Scope::User,
        "Which polarity to derive the theme for; auto decides from the background.",
    ),
    setting(
        "theme.roles.*",
        Kind::Text,
        Scope::User,
        "One colour role, as #rrggbb, over the derived ramp.",
    ),
    setting("ui.mouse", Kind::Bool, Scope::User, "Report mouse events."),
    setting(
        "ui.alternate_screen",
        Kind::Bool,
        Scope::User,
        "Use the alternate screen, preserving scrollback.",
    ),
    setting(
        "ui.keyboard_enhancement",
        Kind::Bool,
        Scope::User,
        "Negotiate the Kitty keyboard protocol.",
    ),
    setting(
        "ui.undercurl",
        Kind::OneOf(&["auto", "on", "off"]),
        Scope::User,
        "Draw diagnostics with a curly, coloured underline.",
    ),
    setting(
        "ui.double_click_ms",
        Kind::Int { min: 100, max: 2000 },
        Scope::User,
        "Longest gap between presses that still makes a double click.",
    ),
    setting(
        "ui.hover_delay_ms",
        Kind::Int { min: 100, max: 5000 },
        Scope::User,
        "How long the pointer rests on a symbol before its card is asked for.",
    ),
    setting(
        "ui.hyperlinks",
        Kind::Bool,
        Scope::User,
        "Mark links in cards with OSC 8 so the terminal can open them.",
    ),
    setting(
        "ui.lightbulb",
        Kind::Bool,
        Scope::User,
        "Mark the caret's line when its language server has code actions there.",
    ),
    setting("keys.*", Kind::Text, Scope::User, "A key sequence, bound to a command id."),
    setting(
        "glyphs.preset",
        Kind::Text,
        Scope::User,
        "Which set of glyphs to start from: default or ascii.",
    ),
    setting("glyphs.**", Kind::Text, Scope::User, "One glyph role, over the preset."),
    setting(
        "lsp.*.command",
        Kind::Command,
        Scope::Trusted,
        "The language server's program, found on PATH unless it is a path.",
    ),
    setting("lsp.*.args", Kind::List, Scope::Trusted, "Arguments to the language server."),
    setting(
        "lsp.*.enabled",
        Kind::Bool,
        Scope::Trusted,
        "Whether to start the language server at all.",
    ),
    setting(
        "lsp.*.format_on_save",
        Kind::Bool,
        Scope::Trusted,
        "Have the language server format the file each time it is saved.",
    ),
];

/// The setting a dotted path names, if any.
#[must_use]
pub fn lookup(path: &[&str]) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| matches(setting.key, path))
}

/// The setting a dotted key names, if any. The key is split at every dot, so
/// this is for keys whose chosen names have none; a glyph role still finds
/// `glyphs.**`.
#[must_use]
pub fn find(key: &str) -> Option<&'static Setting> {
    let path: Vec<&str> = key.split('.').collect();
    lookup(&path)
}

/// Whether `path` is a table that holds settings, such as `lsp.rust`.
#[must_use]
pub fn is_section(path: &[&str]) -> bool {
    SETTINGS.iter().any(|setting| {
        let pattern: Vec<&str> = setting.key.split('.').collect();
        if pattern.last() == Some(&"**") && path.len() >= pattern.len() - 1 {
            return prefix_matches(&pattern[..pattern.len() - 1], &path[..pattern.len() - 1]);
        }
        pattern.len() > path.len() && prefix_matches(&pattern[..path.len()], path)
    })
}

fn prefix_matches(pattern: &[&str], path: &[&str]) -> bool {
    pattern.len() == path.len()
        && pattern.iter().zip(path).all(|(want, got)| *want == "*" || *want == "**" || want == got)
}

fn matches(pattern: &str, path: &[&str]) -> bool {
    let pattern: Vec<&str> = pattern.split('.').collect();
    if pattern.last() == Some(&"**") {
        let fixed = &pattern[..pattern.len() - 1];
        return path.len() > fixed.len() && prefix_matches(fixed, &path[..fixed.len()]);
    }
    prefix_matches(&pattern, path)
}

/// The key most likely meant by one that does not exist: a typo or two away
/// from a real one at the same depth.
#[must_use]
pub fn nearest(path: &[&str]) -> Option<String> {
    let wanted = path.join(".");
    let (distance, near) = SETTINGS
        .iter()
        .map(|setting| {
            // A wildcard takes the name that was written, so `lsp.rust.comand`
            // is compared with `lsp.rust.command`.
            let spelled: Vec<&str> = setting
                .key
                .split('.')
                .enumerate()
                .map(|(at, part)| match part {
                    "*" | "**" => path.get(at).copied().unwrap_or(part),
                    part => part,
                })
                .collect();
            let spelled = spelled.join(".");
            (edit_distance(&wanted, &spelled), spelled)
        })
        .min_by_key(|(distance, _)| *distance)?;
    (distance > 0 && distance <= 2.max(wanted.chars().count() / 6)).then_some(near)
}

/// Levenshtein distance, by char.
fn edit_distance(one: &str, other: &str) -> usize {
    let other: Vec<char> = other.chars().collect();
    let mut row: Vec<usize> = (0..=other.len()).collect();
    for (i, a) in one.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in other.iter().enumerate() {
            let substitution = previous + usize::from(a != *b);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(previous + 1);
        }
    }
    row[other.len()]
}

/// A setting's value, checked against its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `true` or `false`.
    Bool(bool),
    /// A whole number.
    Int(u64),
    /// A string.
    Text(String),
    /// A list of strings.
    List(Vec<String>),
}

impl fmt::Display for Value {
    /// As TOML, so `nun config` can be pasted back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Text(text) => f.write_str(&crate::toml_string(text)),
            Self::List(items) => {
                let items: Vec<String> =
                    items.iter().map(|item| crate::toml_string(item)).collect();
                write!(f, "[{}]", items.join(", "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_and_chosen_names_are_both_found() {
        assert_eq!(lookup(&["editor", "tab_width"]).unwrap().key, "editor.tab_width");
        assert_eq!(lookup(&["lsp", "rust", "command"]).unwrap().key, "lsp.*.command");
        assert_eq!(lookup(&["glyphs", "fold", "open"]).unwrap().key, "glyphs.**");
        assert_eq!(lookup(&["glyphs", "preset"]).unwrap().key, "glyphs.preset");
        assert_eq!(lookup(&["keys", "ctrl+."]).unwrap().key, "keys.*");
        assert!(lookup(&["lsp", "rust"]).is_none(), "a table, not a setting");
        assert!(lookup(&["editor", "tab_widht"]).is_none());
    }

    #[test]
    fn sections_are_the_tables_on_the_way_to_a_setting() {
        assert!(is_section(&["editor"]));
        assert!(is_section(&["lsp"]));
        assert!(is_section(&["lsp", "rust"]));
        assert!(is_section(&["glyphs", "fold"]));
        assert!(is_section(&["theme", "roles"]));
        assert!(!is_section(&["editor", "tab_width"]));
        assert!(!is_section(&["nonsense"]));
    }

    #[test]
    fn a_typo_suggests_the_setting_it_is_nearest() {
        assert_eq!(nearest(&["editor", "tab_widht"]).as_deref(), Some("editor.tab_width"));
        assert_eq!(nearest(&["lsp", "rust", "comand"]).as_deref(), Some("lsp.rust.command"));
        assert_eq!(nearest(&["ui", "mosue"]).as_deref(), Some("ui.mouse"));
        assert_eq!(nearest(&["completely", "different"]), None);
    }

    #[test]
    fn risky_settings_need_trust_and_personal_ones_stay_personal() {
        assert!(find("lsp.rust.command").unwrap().scope.needs_trust());
        assert!(find("lsp.go.format_on_save").unwrap().scope.needs_trust());
        assert!(find("editor.tab_width").unwrap().scope.project_may_set());
        assert!(!find("editor.tab_width").unwrap().scope.needs_trust());
        assert!(!find("ui.mouse").unwrap().scope.project_may_set());
        assert!(!find("keys.ctrl+s").unwrap().scope.project_may_set());
    }

    #[test]
    fn values_print_as_toml() {
        assert_eq!(Value::Text("a\"b".into()).to_string(), "\"a\\\"b\"");
        assert_eq!(Value::List(vec!["--stdio".into()]).to_string(), "[\"--stdio\"]");
        assert_eq!(Value::Int(4).to_string(), "4");
    }
}
