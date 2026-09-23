//! Configuration, in layers.
//!
//! This is the first two layers of the scheme `0042` will finish: built-in
//! defaults, then `~/.config/nun/nun.toml`. Project files, trust prompts,
//! `.editorconfig` and hot reload are still to come, so the loader is written
//! to have layers added rather than to have two hard-coded.
//!
//! Two rules shape it. **Zero config is a supported configuration** — every
//! field has a default and the file only ever holds overrides. And **a bad file
//! never takes the editor down**: parsing failures name the line and leave the
//! previous layer standing, so a typo costs you one setting rather than your
//! editor.
//!
//! No terminal dependency; all of this is unit tested directly.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// nun's built-in default.
    Default,
    /// A configuration file.
    File(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Something wrong with a configuration file, reported rather than fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The file it was found in.
    pub path: PathBuf,
    /// What is wrong, including the line where the parser gave up.
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

/// Which polarity to derive the theme for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Polarity {
    /// Decide from the terminal's background lightness.
    #[default]
    Auto,
    /// Force a dark ramp, for a terminal whose background is translucent and
    /// therefore reports something misleading.
    Dark,
    /// Force a light ramp.
    Light,
}

/// How to start the language server for one language: `[lsp.<language>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServer {
    /// The program, found on `PATH` unless it is a path itself.
    pub command: String,
    /// Arguments to it.
    pub args: Vec<String>,
    /// Whether to start it at all. A default server that is not installed is
    /// skipped quietly anyway; this is for one that is installed and unwanted.
    pub enabled: bool,
    /// Whether saving a file in this language has its server format it first.
    ///
    /// On by default only where the language has one formatter everybody
    /// uses and its server applies it — rustfmt, gofmt. Elsewhere the server's
    /// formatter is one opinion among several, and a save that quietly
    /// rewrote a file to the wrong one would be worse than no formatting.
    pub format_on_save: bool,
}

impl LspServer {
    fn new(command: &str, args: &[&str]) -> Self {
        Self {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            enabled: true,
            format_on_save: false,
        }
    }

    /// The same, formatting on save.
    fn formatting(self) -> Self {
        Self { format_on_save: true, ..self }
    }
}

/// The servers nun knows to try, by language.
///
/// Each is started only when a file in its language is opened, and one that
/// is not installed is skipped without a word: listing a server here costs a
/// person who does not have it nothing.
fn default_servers() -> BTreeMap<String, LspServer> {
    let typescript = LspServer::new("typescript-language-server", &["--stdio"]);
    [
        ("rust", LspServer::new("rust-analyzer", &[]).formatting()),
        ("python", LspServer::new("pyright-langserver", &["--stdio"])),
        ("typescript", typescript.clone()),
        ("javascript", typescript),
        ("go", LspServer::new("gopls", &[]).formatting()),
        ("c", LspServer::new("clangd", &[])),
        ("cpp", LspServer::new("clangd", &[])),
    ]
    .into_iter()
    .map(|(language, server)| (language.to_string(), server))
    .collect()
}

/// The effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Columns a tab advances to.
    pub tab_width: usize,
    /// Which polarity to derive for.
    pub polarity: Polarity,
    /// Role overrides, by the key names in [`nun_theme::Role::key`].
    pub roles: BTreeMap<String, String>,
    /// Report mouse events.
    pub mouse: bool,
    /// Use the alternate screen, preserving scrollback.
    pub alternate_screen: bool,
    /// Negotiate the Kitty keyboard protocol.
    pub keyboard_enhancement: bool,
    /// Longest gap between presses that still makes a double or triple click,
    /// in milliseconds. `None` uses the platform's usual value.
    pub double_click_ms: Option<u64>,
    /// Key bindings added over the defaults: key sequence to command id.
    ///
    /// Kept as text here. Which sequences and commands exist is the binary's
    /// business, and it reports anything it cannot use as a problem.
    pub keys: BTreeMap<String, String>,
    /// Language servers, by language: `[lsp.<language>]`.
    ///
    /// Which languages exist is the binary's business, like the commands in
    /// `keys`; it reports a language it does not know as a problem.
    pub lsp: BTreeMap<String, LspServer>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_width: 4,
            polarity: Polarity::Auto,
            roles: BTreeMap::new(),
            mouse: true,
            alternate_screen: true,
            keyboard_enhancement: true,
            double_click_ms: None,
            keys: BTreeMap::new(),
            lsp: default_servers(),
        }
    }
}

/// The configuration plus where each value came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The merged result.
    pub config: Config,
    /// Origin per setting key, for `nun config`.
    pub origins: BTreeMap<String, Origin>,
    /// Anything wrong with the files that were read.
    pub problems: Vec<Problem>,
}

impl Loaded {
    /// Defaults only.
    #[must_use]
    pub fn defaults() -> Self {
        Self { config: Config::default(), origins: BTreeMap::new(), problems: Vec::new() }
    }

    /// Where a setting came from.
    #[must_use]
    pub fn origin(&self, key: &str) -> Origin {
        self.origins.get(key).cloned().unwrap_or(Origin::Default)
    }

    /// The effective configuration, annotated, as `nun config` prints it.
    #[must_use]
    pub fn describe(&self) -> String {
        use fmt::Write as _;
        let c = &self.config;
        let mut out = String::new();

        let _ = writeln!(out, "[editor]");
        let _ = writeln!(out, "tab_width = {}{}", c.tab_width, self.note("tab_width"));

        let _ = writeln!(out, "\n[theme]");
        let _ = writeln!(
            out,
            "polarity = {:?}{}",
            format!("{:?}", c.polarity).to_lowercase(),
            self.note("polarity")
        );

        if c.roles.is_empty() {
            let _ = writeln!(out, "# [theme.roles] — none overridden; all derived");
        } else {
            let _ = writeln!(out, "\n[theme.roles]{}", self.note("roles"));
            for (role, color) in &c.roles {
                let _ = writeln!(out, "{role} = {color:?}");
            }
        }

        let _ = writeln!(out, "\n[ui]");
        for (key, value) in [
            ("mouse", c.mouse),
            ("alternate_screen", c.alternate_screen),
            ("keyboard_enhancement", c.keyboard_enhancement),
        ] {
            let _ = writeln!(out, "{key} = {value}{}", self.note(key));
        }
        match c.double_click_ms {
            Some(ms) => {
                let _ = writeln!(out, "double_click_ms = {ms}{}", self.note("double_click_ms"));
            }
            None => {
                let _ = writeln!(out, "# double_click_ms — the platform's usual value");
            }
        }

        if c.keys.is_empty() {
            let _ = writeln!(out, "\n# [keys] — none added; the defaults are listed by `nun keys`");
        } else {
            let _ = writeln!(out, "\n[keys]{}", self.note("keys"));
            for (sequence, command) in &c.keys {
                let _ = writeln!(out, "{sequence:?} = {command:?}");
            }
        }

        for (language, server) in &c.lsp {
            let _ = writeln!(out, "\n[lsp.{language}]{}", self.note(&format!("lsp.{language}")));
            let _ = writeln!(out, "command = {:?}", server.command);
            let _ = writeln!(out, "args = {:?}", server.args);
            if !server.enabled {
                let _ = writeln!(out, "enabled = false");
            }
            let _ = writeln!(out, "format_on_save = {}", server.format_on_save);
        }

        if !self.problems.is_empty() {
            let _ = writeln!(out, "\n# problems");
            for problem in &self.problems {
                let _ = writeln!(out, "# {problem}");
            }
        }
        out
    }

    fn note(&self, key: &str) -> String {
        match self.origin(key) {
            Origin::Default => String::new(),
            Origin::File(path) => format!("    # {}", path.display()),
        }
    }
}

/// Where nun looks for the user's file.
///
/// `$XDG_CONFIG_HOME` if set, otherwise `~/.config`, which is what every other
/// tool on this machine already uses.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("nun").join("nun.toml"));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config").join("nun").join("nun.toml"))
}

/// Load defaults, then the user's file if it exists.
#[must_use]
pub fn load() -> Loaded {
    let mut loaded = Loaded::defaults();
    if let Some(path) = user_config_path() {
        apply_file(&mut loaded, &path);
    }
    loaded
}

/// Apply one file over whatever is already loaded.
///
/// A missing file is not a problem — zero config is the expected case. A file
/// that exists but cannot be read or parsed is reported and skipped.
pub fn apply_file(loaded: &mut Loaded, path: &Path) {
    if !path.exists() {
        return;
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            loaded.problems.push(Problem {
                path: path.to_path_buf(),
                message: format!("could not be read: {error}"),
            });
            return;
        }
    };

    match toml::from_str::<RawConfig>(&text) {
        Ok(raw) => raw.apply(loaded, path),
        Err(error) => {
            // toml's Display already carries "at line N, column M", which is
            // the whole point of reporting it rather than saying "invalid".
            loaded
                .problems
                .push(Problem { path: path.to_path_buf(), message: error.message().to_string() });
        }
    }
}

/// The file's shape. Every field optional, unknown keys refused.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    editor: Option<RawEditor>,
    theme: Option<RawTheme>,
    ui: Option<RawUi>,
    keys: Option<BTreeMap<String, String>>,
    lsp: Option<BTreeMap<String, RawLsp>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLsp {
    command: Option<String>,
    args: Option<Vec<String>>,
    enabled: Option<bool>,
    format_on_save: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEditor {
    tab_width: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    polarity: Option<Polarity>,
    roles: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUi {
    mouse: Option<bool>,
    alternate_screen: Option<bool>,
    keyboard_enhancement: Option<bool>,
    double_click_ms: Option<u64>,
}

impl RawConfig {
    fn apply(self, loaded: &mut Loaded, path: &Path) {
        let mut set = |key: &str| {
            loaded.origins.insert(key.to_string(), Origin::File(path.to_path_buf()));
        };

        if let Some(editor) = self.editor
            && let Some(width) = editor.tab_width
        {
            if width == 0 || width > 16 {
                loaded.problems.push(Problem {
                    path: path.to_path_buf(),
                    message: format!("editor.tab_width must be between 1 and 16, not {width}"),
                });
            } else {
                loaded.config.tab_width = width;
                set("tab_width");
            }
        }

        if let Some(theme) = self.theme {
            if let Some(polarity) = theme.polarity {
                loaded.config.polarity = polarity;
                set("polarity");
            }
            if let Some(roles) = theme.roles {
                loaded.config.roles = roles;
                set("roles");
            }
        }

        if let Some(ui) = self.ui {
            if let Some(value) = ui.mouse {
                loaded.config.mouse = value;
                set("mouse");
            }
            if let Some(value) = ui.alternate_screen {
                loaded.config.alternate_screen = value;
                set("alternate_screen");
            }
            if let Some(value) = ui.keyboard_enhancement {
                loaded.config.keyboard_enhancement = value;
                set("keyboard_enhancement");
            }
            if let Some(ms) = ui.double_click_ms {
                // Below 100 ms nobody can double-click; above 2 s two separate
                // clicks start turning into one.
                if (100..=2000).contains(&ms) {
                    loaded.config.double_click_ms = Some(ms);
                    set("double_click_ms");
                } else {
                    loaded.problems.push(Problem {
                        path: path.to_path_buf(),
                        message: format!(
                            "ui.double_click_ms must be between 100 and 2000, not {ms}"
                        ),
                    });
                }
            }
        }

        if let Some(keys) = self.keys {
            // Added to, not replaced: a later layer's bindings sit over an
            // earlier layer's, the same way both sit over the defaults.
            loaded.config.keys.extend(keys);
            set("keys");
        }

        for (language, raw) in self.lsp.unwrap_or_default() {
            // Field by field over what is already there, so `args` alone
            // changes the arguments and keeps the command.
            let server = match (loaded.config.lsp.get(&language), raw.command) {
                (Some(existing), command) => LspServer {
                    command: command.unwrap_or_else(|| existing.command.clone()),
                    ..existing.clone()
                },
                (None, Some(command)) => {
                    LspServer { command, args: Vec::new(), enabled: true, format_on_save: false }
                }
                (None, None) => {
                    loaded.problems.push(Problem {
                        path: path.to_path_buf(),
                        message: format!(
                            "lsp.{language} needs a command: nun has no default server for it"
                        ),
                    });
                    continue;
                }
            };
            let server = LspServer {
                args: raw.args.unwrap_or(server.args),
                enabled: raw.enabled.unwrap_or(server.enabled),
                format_on_save: raw.format_on_save.unwrap_or(server.format_on_save),
                command: server.command,
            };
            if server.command.trim().is_empty() {
                loaded.problems.push(Problem {
                    path: path.to_path_buf(),
                    message: format!(
                        "lsp.{language}.command is empty; use `enabled = false` to turn it off"
                    ),
                });
                continue;
            }
            loaded.config.lsp.insert(language.clone(), server);
            set(&format!("lsp.{language}"));
        }
    }
}
