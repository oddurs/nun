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

/// Whether to draw diagnostics with a curly, coloured underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Undercurl {
    /// Ask the terminal, and use it only if it says it can.
    #[default]
    Auto,
    /// Use it whatever the terminal says: for a terminal that has it and
    /// cannot say so, such as tmux set up with `usstyle`, or Alacritty.
    On,
    /// Never use it; a plain underline, in the text's own colour.
    Off,
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
// A configuration is a list of switches, each one a setting a person turns
// on or off by name; folding them into enums would only rename `true`.
#[allow(clippy::struct_excessive_bools)]
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
    /// Draw diagnostics with a curly, coloured underline.
    pub undercurl: Undercurl,
    /// Longest gap between presses that still makes a double or triple click,
    /// in milliseconds. `None` uses the platform's usual value.
    pub double_click_ms: Option<u64>,
    /// How long the pointer rests on a symbol before its hover card is asked
    /// for, in milliseconds.
    pub hover_delay_ms: u64,
    /// Mark links in cards with OSC 8 as well as drawing them as links, so a
    /// terminal that knows it can show or copy where they go.
    pub hyperlinks: bool,
    /// Mark the caret's line in the gutter when its language server offers
    /// code actions there.
    pub lightbulb: bool,
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
    /// `glyphs.preset`: which set of glyphs to start from.
    pub glyph_preset: String,
    /// The rest of `[glyphs]`: role name to glyph, over the preset.
    ///
    /// Kept as text, like `keys`. Which roles and presets exist, and which
    /// glyphs are fit to draw, is the UI's business; it reports anything it
    /// cannot use as a problem.
    pub glyphs: BTreeMap<String, String>,
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
            undercurl: Undercurl::Auto,
            double_click_ms: None,
            hover_delay_ms: 400,
            hyperlinks: true,
            lightbulb: true,
            keys: BTreeMap::new(),
            lsp: default_servers(),
            glyph_preset: "default".to_string(),
            glyphs: BTreeMap::new(),
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
            ("hyperlinks", c.hyperlinks),
            ("lightbulb", c.lightbulb),
        ] {
            let _ = writeln!(out, "{key} = {value}{}", self.note(key));
        }
        let _ = writeln!(
            out,
            "undercurl = {:?}{}",
            format!("{:?}", c.undercurl).to_lowercase(),
            self.note("undercurl")
        );
        let _ =
            writeln!(out, "hover_delay_ms = {}{}", c.hover_delay_ms, self.note("hover_delay_ms"));
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

        let _ = writeln!(out, "\n[glyphs]");
        let _ = writeln!(out, "preset = {:?}{}", c.glyph_preset, self.note("glyphs.preset"));
        if c.glyphs.is_empty() {
            let _ = writeln!(out, "# none changed; every role is listed by `nun glyphs`");
        } else {
            for (role, glyph) in &c.glyphs {
                let note = self.note(&format!("glyphs.{role}"));
                let _ = writeln!(out, "{} = {}{note}", toml_string(role), toml_string(glyph));
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

/// `text` as a TOML basic string.
///
/// Not Rust's `{:?}`, which writes `\u{301}` for a combining mark where TOML
/// wants `\u0301`: a glyph can be exactly that, and `nun config` is meant to
/// paste back. Only what TOML requires is escaped; the rest stays as it is.
fn toml_string(text: &str) -> String {
    use fmt::Write as _;
    let mut out = String::from('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
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
    glyphs: Option<toml::Table>,
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
    undercurl: Option<Undercurl>,
    double_click_ms: Option<u64>,
    hover_delay_ms: Option<u64>,
    hyperlinks: Option<bool>,
    lightbulb: Option<bool>,
}

impl RawUi {
    fn apply(self, loaded: &mut Loaded, path: &Path) {
        let mut set = |key: &str| {
            loaded.origins.insert(key.to_string(), Origin::File(path.to_path_buf()));
        };

        if let Some(value) = self.mouse {
            loaded.config.mouse = value;
            set("mouse");
        }
        if let Some(value) = self.alternate_screen {
            loaded.config.alternate_screen = value;
            set("alternate_screen");
        }
        if let Some(value) = self.keyboard_enhancement {
            loaded.config.keyboard_enhancement = value;
            set("keyboard_enhancement");
        }
        if let Some(value) = self.undercurl {
            loaded.config.undercurl = value;
            set("undercurl");
        }
        if let Some(value) = self.hyperlinks {
            loaded.config.hyperlinks = value;
            set("hyperlinks");
        }
        if let Some(value) = self.lightbulb {
            loaded.config.lightbulb = value;
            set("lightbulb");
        }
        if let Some(ms) = self.hover_delay_ms {
            // Much under 100 ms and every symbol the pointer crosses on
            // its way somewhere asks its server; much over a few seconds
            // and nobody waits for it.
            if (100..=5000).contains(&ms) {
                loaded.config.hover_delay_ms = ms;
                set("hover_delay_ms");
            } else {
                loaded.problems.push(Problem {
                    path: path.to_path_buf(),
                    message: format!("self.hover_delay_ms must be between 100 and 5000, not {ms}"),
                });
            }
        }
        if let Some(ms) = self.double_click_ms {
            // Below 100 ms nobody can double-click; above 2 s two separate
            // clicks start turning into one.
            if (100..=2000).contains(&ms) {
                loaded.config.double_click_ms = Some(ms);
                set("double_click_ms");
            } else {
                loaded.problems.push(Problem {
                    path: path.to_path_buf(),
                    message: format!("self.double_click_ms must be between 100 and 2000, not {ms}"),
                });
            }
        }
    }
}

impl RawConfig {
    fn apply(mut self, loaded: &mut Loaded, path: &Path) {
        if let Some(ui) = self.ui.take() {
            ui.apply(loaded, path);
        }
        if let Some(glyphs) = self.glyphs.take() {
            apply_glyphs(glyphs, loaded, path);
        }
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

/// Apply `[glyphs]`: `preset`, and every other string in it, however deep, as
/// a role named by its dotted path.
///
/// Role names have dots in them, and TOML reads `fold.open = "v"` as a table
/// `fold` holding `open`. Flattening the tables back into dotted names lets
/// that, `"fold.open" = "v"` and a `[glyphs.fold]` section all mean the same
/// thing, which is what anyone writing one of them would expect.
fn apply_glyphs(table: toml::Table, loaded: &mut Loaded, path: &Path) {
    let mut glyphs = BTreeMap::new();
    let mut problems = Vec::new();
    flatten(table, "", &mut glyphs, &mut problems);

    let origin = || Origin::File(path.to_path_buf());
    if let Some(preset) = glyphs.remove("preset") {
        loaded.config.glyph_preset = preset;
        loaded.origins.insert("glyphs.preset".to_string(), origin());
    }
    for (role, glyph) in glyphs {
        // Added to, not replaced, like `[keys]`: a later layer changes the
        // roles it names and leaves an earlier layer's others alone.
        loaded.origins.insert(format!("glyphs.{role}"), origin());
        loaded.config.glyphs.insert(role, glyph);
    }
    for message in problems {
        loaded.problems.push(Problem { path: path.to_path_buf(), message });
    }
}

fn flatten(
    table: toml::Table,
    prefix: &str,
    out: &mut BTreeMap<String, String>,
    problems: &mut Vec<String>,
) {
    for (key, value) in table {
        let name = if prefix.is_empty() { key } else { format!("{prefix}.{key}") };
        match value {
            toml::Value::String(glyph) => {
                // `fold.open` and `"fold.open"` are different keys to TOML and
                // the same role here.
                if out.insert(name.clone(), glyph).is_some() {
                    problems.push(format!("glyphs.{name} is set twice; the second is used"));
                }
            }
            toml::Value::Table(inner) => flatten(inner, &name, out, problems),
            other => problems.push(format!(
                "glyphs.{name} must be a string, like \"v\", not {}",
                other.type_str()
            )),
        }
    }
}
