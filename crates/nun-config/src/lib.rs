//! Configuration, in layers.
//!
//! Four of them, each overriding the last: nun's built-in defaults, the
//! person's `~/.config/nun/nun.toml`, the `.editorconfig` sections that match
//! a file (whitespace only), and a project's `.nun.toml` — which is inert
//! until its directory is trusted, because a file from a freshly cloned
//! repository must not choose a formatter or a program to run on its own. See
//! [`trust`] for what is remembered, and [`schema::Scope`] for what a project
//! may set at all.
//!
//! Two rules shape it. **Zero config is a supported configuration** — every
//! setting has a default and a file only ever holds overrides. And **a bad file
//! never takes the editor down**: a bad value is reported with its line and
//! left out, the rest of the file still applies, and a file that is not TOML
//! at all keeps the settings it had until it reads again.
//!
//! Reading files is kept apart from resolving them ([`Files::read`], then
//! [`resolve`]), so the editor can read on a worker and swap the result in, and
//! so everything here is unit tested without a disk where it can be.
//!
//! No terminal dependency.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub mod editorconfig;
pub mod file;
pub mod schema;
pub mod trust;
pub mod whitespace;

pub use editorconfig::EditorConfig;
pub use file::{Entry, File};
pub use schema::{Scope, Setting, Value};
pub use trust::{Decision, Trust, TrustStore};
pub use whitespace::{Charset, EndOfLine, IndentStyle, Whitespace};

/// The name of a project's own file.
pub const PROJECT_FILE: &str = ".nun.toml";

/// One of the layers, in the order they apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// The person's own `nun.toml`.
    User,
    /// An `.editorconfig`, for whitespace.
    EditorConfig,
    /// A project's `.nun.toml`.
    Project,
}

impl Layer {
    /// Its name, as `nun config` prints it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::EditorConfig => "editorconfig",
            Self::Project => "project",
        }
    }
}

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// nun's built-in default.
    Default,
    /// A line of a file.
    File {
        /// Which layer the file is.
        layer: Layer,
        /// The file.
        path: PathBuf,
        /// The line, from 1.
        line: usize,
    },
}

impl Origin {
    /// The layer it is from; `None` for a default.
    #[must_use]
    pub const fn layer(&self) -> Option<Layer> {
        match self {
            Self::Default => None,
            Self::File { layer, .. } => Some(*layer),
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::File { layer, path, line } => {
                write!(f, "{}: {}:{line}", layer.name(), path.display())
            }
        }
    }
}

/// Something wrong with a configuration file, reported rather than fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The file it was found in.
    pub path: PathBuf,
    /// The line, when it is about one.
    pub line: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    /// The same, naming the file only by its name: for a status line, where
    /// a full path would push the message itself off the end.
    #[must_use]
    pub fn brief(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        match self.line {
            Some(line) => format!("{name}:{line}: {}", self.message),
            None => format!("{name}: {}", self.message),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.path.display(), self.message),
            None => write!(f, "{}: {}", self.path.display(), self.message),
        }
    }
}

/// Which polarity to derive the theme for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Whether to draw exact colours as they are, or as the nearest of the
/// 256-colour palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Truecolor {
    /// Ask the terminal, and draw exact colours only if it says it can.
    #[default]
    Auto,
    /// Draw them whatever the terminal says: for one that has 24-bit colour
    /// and cannot say so, such as Alacritty over ssh.
    On,
    /// Always the nearest of the 256.
    Off,
}

/// Where a copy from the terminal panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clipboard {
    /// A clipboard program, unless nun is running over ssh or there is none;
    /// then the terminal nun runs in, through OSC 52, if it said it accepts
    /// that.
    #[default]
    Auto,
    /// Only a clipboard program on the machine nun runs on.
    System,
    /// Only the terminal nun runs in, through OSC 52, whatever it said: for
    /// a terminal that accepts it and cannot say so, such as Alacritty.
    Osc52,
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
    /// Where a copy goes.
    pub clipboard: Clipboard,
    /// Draw exact colours as they are.
    pub truecolor: Truecolor,
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
            clipboard: Clipboard::Auto,
            truecolor: Truecolor::Auto,
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

impl Config {
    /// Take one checked setting. `editor.*` whitespace other than the tab
    /// width is per file, and read by [`Whitespace::resolve`] instead.
    fn set(&mut self, key: &str, value: &Value) {
        match (key, value) {
            ("editor.tab_width", Value::Int(width)) => {
                self.tab_width = usize::try_from(*width).unwrap_or(self.tab_width);
            }
            ("theme.polarity", Value::Text(word)) => {
                self.polarity = match word.as_str() {
                    "dark" => Polarity::Dark,
                    "light" => Polarity::Light,
                    _ => Polarity::Auto,
                };
            }
            ("ui.undercurl", Value::Text(word)) => {
                self.undercurl = match word.as_str() {
                    "on" => Undercurl::On,
                    "off" => Undercurl::Off,
                    _ => Undercurl::Auto,
                };
            }
            ("ui.truecolor", Value::Text(word)) => {
                self.truecolor = match word.as_str() {
                    "on" => Truecolor::On,
                    "off" => Truecolor::Off,
                    _ => Truecolor::Auto,
                };
            }
            ("ui.clipboard", Value::Text(word)) => {
                self.clipboard = match word.as_str() {
                    "system" => Clipboard::System,
                    "osc52" => Clipboard::Osc52,
                    _ => Clipboard::Auto,
                };
            }
            ("ui.mouse", Value::Bool(on)) => self.mouse = *on,
            ("ui.alternate_screen", Value::Bool(on)) => self.alternate_screen = *on,
            ("ui.keyboard_enhancement", Value::Bool(on)) => self.keyboard_enhancement = *on,
            ("ui.hyperlinks", Value::Bool(on)) => self.hyperlinks = *on,
            ("ui.lightbulb", Value::Bool(on)) => self.lightbulb = *on,
            ("ui.double_click_ms", Value::Int(ms)) => self.double_click_ms = Some(*ms),
            ("ui.hover_delay_ms", Value::Int(ms)) => self.hover_delay_ms = *ms,
            ("glyphs.preset", Value::Text(preset)) => self.glyph_preset.clone_from(preset),
            (key, value) => self.set_named(key, value),
        }
    }

    /// The settings whose keys hold a name the file chose.
    fn set_named(&mut self, key: &str, value: &Value) {
        if let (Some(role), Value::Text(color)) = (key.strip_prefix("theme.roles."), value) {
            self.roles.insert(role.to_string(), color.clone());
        } else if let (Some(sequence), Value::Text(command)) = (key.strip_prefix("keys."), value) {
            // Added to, not replaced: a later layer's bindings sit over an
            // earlier layer's, the same way both sit over the defaults.
            self.keys.insert(sequence.to_string(), command.clone());
        } else if let (Some(role), Value::Text(glyph)) = (key.strip_prefix("glyphs."), value) {
            self.glyphs.insert(role.to_string(), glyph.clone());
        } else if let Some((language, field)) =
            key.strip_prefix("lsp.").and_then(|rest| rest.rsplit_once('.'))
        {
            // Field by field over what is already there, so `args` alone
            // changes the arguments and keeps the command.
            let server = self.lsp.entry(language.to_string()).or_insert_with(|| LspServer {
                command: String::new(),
                args: Vec::new(),
                enabled: true,
                format_on_save: false,
            });
            match (field, value) {
                ("command", Value::Text(command)) => server.command.clone_from(command),
                ("args", Value::List(args)) => server.args.clone_from(args),
                ("enabled", Value::Bool(on)) => server.enabled = *on,
                ("format_on_save", Value::Bool(on)) => server.format_on_save = *on,
                _ => {}
            }
        }
    }

    /// A setting's value as text — a string without its quotes — for `nun
    /// config --explain`. `None` for one
    /// that has no value here, such as a key binding nobody added.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        let text = |text: &str| Some(text.to_string());
        let word = |debug: String| Some(debug.to_lowercase());
        match key {
            "editor.tab_width" => Some(self.tab_width.to_string()),
            "theme.polarity" => word(format!("{:?}", self.polarity)),
            "ui.undercurl" => word(format!("{:?}", self.undercurl)),
            "ui.clipboard" => word(format!("{:?}", self.clipboard)),
            "ui.truecolor" => word(format!("{:?}", self.truecolor)),
            "ui.mouse" => Some(self.mouse.to_string()),
            "ui.alternate_screen" => Some(self.alternate_screen.to_string()),
            "ui.keyboard_enhancement" => Some(self.keyboard_enhancement.to_string()),
            "ui.hyperlinks" => Some(self.hyperlinks.to_string()),
            "ui.lightbulb" => Some(self.lightbulb.to_string()),
            "ui.double_click_ms" => self.double_click_ms.map(|ms| ms.to_string()),
            "ui.hover_delay_ms" => Some(self.hover_delay_ms.to_string()),
            "glyphs.preset" => text(&self.glyph_preset),
            key => {
                if let Some(role) = key.strip_prefix("theme.roles.") {
                    return self.roles.get(role).and_then(|color| text(color));
                }
                if let Some(sequence) = key.strip_prefix("keys.") {
                    return self.keys.get(sequence).and_then(|command| text(command));
                }
                if let Some(role) = key.strip_prefix("glyphs.") {
                    return self.glyphs.get(role).and_then(|glyph| text(glyph));
                }
                let (language, field) = key.strip_prefix("lsp.")?.rsplit_once('.')?;
                let server = self.lsp.get(language)?;
                match field {
                    "command" => text(&server.command),
                    "args" => Some(Value::List(server.args.clone()).to_string()),
                    "enabled" => Some(server.enabled.to_string()),
                    "format_on_save" => Some(server.format_on_save.to_string()),
                    _ => None,
                }
            }
        }
    }
}

/// A project's `.nun.toml`, and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// The file, read — whether or not any of it applies.
    pub file: File,
    /// The directory it is trusted by: canonical, so two spellings of one
    /// project are one decision.
    pub dir: PathBuf,
    /// Whether it is trusted.
    pub trust: Trust,
    /// The fingerprint of what in it needs trust, to be remembered with a
    /// decision about it.
    pub fingerprint: String,
}

impl Project {
    /// Why `key`, set in this file, does not apply; `None` when it does.
    #[must_use]
    pub fn withheld(&self, key: &str) -> Option<String> {
        let risky = schema::find(key).is_some_and(|setting| setting.scope.needs_trust());
        match self.trust {
            Trust::Trusted => None,
            Trust::Changed if !risky => None,
            Trust::Changed => {
                Some("changed since this project was trusted; waiting to be trusted again".into())
            }
            Trust::Unknown => Some("this project's settings are not trusted yet".into()),
            Trust::Ignored => Some("this project's settings are ignored".into()),
        }
    }

    /// Whether anything in it is waiting on a decision: never decided on, or
    /// its risky settings changed since it was trusted.
    #[must_use]
    pub const fn asks(&self) -> bool {
        matches!(self.trust, Trust::Unknown | Trust::Changed)
    }

    /// What trusting it would change: each setting it holds that does not
    /// apply now, as `key = value`.
    #[must_use]
    pub fn would_change(&self) -> Vec<String> {
        self.file
            .entries
            .iter()
            .filter(|(key, _)| self.withheld(key).is_some())
            .map(|(key, entry)| format!("{key} = {}", entry.value))
            .collect()
    }
}

/// Where the files are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    /// The person's `nun.toml`.
    pub user: Option<PathBuf>,
    /// The project's `.nun.toml`, when there is one.
    pub project: Option<PathBuf>,
}

impl Sources {
    /// The person's file only.
    #[must_use]
    pub fn user_only() -> Self {
        Self { user: user_config_path(), project: None }
    }

    /// The person's file, and the project file that governs `root`: the
    /// nearest `.nun.toml` in it or above it, looking no higher than the
    /// repository it is in and never in the home directory itself, whose
    /// dotfiles are the person's rather than a project's.
    #[must_use]
    pub fn discover(root: &Path) -> Self {
        Self { user: user_config_path(), project: find_project(root) }
    }
}

/// The nearest `.nun.toml` governing `root`.
#[must_use]
pub fn find_project(root: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let home = std::env::var_os("HOME").and_then(|home| PathBuf::from(home).canonicalize().ok());
    for dir in root.ancestors() {
        if home.as_deref() == Some(dir) {
            return None;
        }
        let candidate = dir.join(PROJECT_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if dir.join(".git").exists() {
            return None;
        }
    }
    None
}

/// The files, as read: what [`resolve`] works from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Files {
    /// The person's file, when there is one.
    pub user: Option<File>,
    /// The project's, when there is one.
    pub project: Option<File>,
}

impl Files {
    /// Read the files `sources` names. `previous` is what they said the last
    /// time, so a file broken now keeps what it had.
    #[must_use]
    pub fn read(sources: &Sources, previous: &Self) -> Self {
        let read = |path: &Option<PathBuf>, layer, previous: &Option<File>| {
            let path = path.as_ref()?;
            let previous = previous.as_ref().filter(|file| file.path == *path);
            File::read(path, layer, previous)
        };
        Self {
            user: read(&sources.user, Layer::User, &previous.user),
            project: read(&sources.project, Layer::Project, &previous.project),
        }
    }
}

/// The configuration the files add up to, given what has been trusted.
#[must_use]
pub fn resolve(files: &Files, trust: &TrustStore) -> Loaded {
    let mut loaded = Loaded::defaults();
    loaded.user.clone_from(&files.user);
    loaded.project = files.project.as_ref().map(|file| {
        let dir = file.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let dir = dir.canonicalize().unwrap_or(dir);
        let fingerprint = trust::fingerprint(file);
        Project { trust: trust.trust(&dir, &fingerprint), file: file.clone(), dir, fingerprint }
    });

    let mut touched = BTreeMap::new();
    let layers = [
        loaded.user.as_ref().map(|file| (file, None)),
        loaded.project.as_ref().map(|project| (&project.file, Some(project))),
    ];
    for (file, project) in layers.into_iter().flatten() {
        loaded.problems.extend(file.problems.iter().cloned());
        for (key, entry) in &file.entries {
            if project.is_some_and(|project| project.withheld(key).is_some()) {
                continue;
            }
            loaded.config.set(key, &entry.value);
            let origin =
                Origin::File { layer: file.layer, path: file.path.clone(), line: entry.line };
            if let Some(language) = key.strip_prefix("lsp.").and_then(|rest| rest.split('.').next())
            {
                touched.insert(language.to_string(), origin.clone());
            }
            loaded.origins.insert(key.clone(), origin);
        }
    }

    // A language nun has no server for needs its command from somewhere.
    for (language, origin) in touched {
        if loaded.config.lsp.get(&language).is_some_and(|server| server.command.is_empty()) {
            loaded.config.lsp.remove(&language);
            if let Origin::File { path, line, .. } = origin {
                loaded.problems.push(Problem {
                    path,
                    line: Some(line),
                    message: format!(
                        "lsp.{language} needs a command: nun has no default server for it"
                    ),
                });
            }
        }
    }
    loaded
}

/// Load defaults, then the person's file if it exists. For the commands that
/// have no project: `nun keys`, `nun glyphs`.
#[must_use]
pub fn load() -> Loaded {
    let files = Files::read(&Sources::user_only(), &Files::default());
    resolve(&files, &TrustStore::in_memory())
}

/// Load every layer that governs `root`, with the decisions remembered about
/// its project file.
#[must_use]
pub fn load_in(root: &Path) -> Loaded {
    let files = Files::read(&Sources::discover(root), &Files::default());
    let trust = TrustStore::default_path().map_or_else(TrustStore::in_memory, TrustStore::load);
    resolve(&files, &trust)
}

/// The configuration plus where each value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The merged result.
    pub config: Config,
    /// Origin per dotted key, for every setting a file set.
    pub origins: BTreeMap<String, Origin>,
    /// Anything wrong with the files that were read.
    pub problems: Vec<Problem>,
    /// The person's file, as read.
    pub user: Option<File>,
    /// The project's file, as read, and whether it is trusted.
    pub project: Option<Project>,
}

impl Loaded {
    /// Defaults only.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            config: Config::default(),
            origins: BTreeMap::new(),
            problems: Vec::new(),
            user: None,
            project: None,
        }
    }

    /// Where a setting came from.
    #[must_use]
    pub fn origin(&self, key: &str) -> Origin {
        self.origins.get(key).cloned().unwrap_or(Origin::Default)
    }

    /// Whether anything under `prefix.` came from a file: whether a language
    /// server was set up by hand, say, for `lsp.go`.
    #[must_use]
    pub fn set_by_a_file(&self, prefix: &str) -> bool {
        let prefix = format!("{prefix}.");
        self.origins.keys().any(|key| key.starts_with(&prefix))
    }

    /// The effective configuration, annotated with the layer each value came
    /// from, as `nun config` prints it. With `whitespace`, the `[editor]`
    /// section is that file's, `.editorconfig` included.
    #[must_use]
    pub fn describe(&self, whitespace: Option<(&Path, &Whitespace)>) -> String {
        use fmt::Write as _;
        let mut out = self.describe_layers(whitespace.map(|(file, _)| file));
        let c = &self.config;

        let _ = writeln!(out, "\n[editor]");
        if let Some((_, whitespace)) = whitespace {
            describe_whitespace(&mut out, whitespace);
        } else {
            let _ = writeln!(out, "tab_width = {}{}", c.tab_width, self.note("editor.tab_width"));
            for key in whitespace::KEYS.iter().filter(|key| **key != "editor.tab_width") {
                let Some(Some(step)) = [Layer::Project, Layer::User]
                    .map(|layer| self.file_steps(key, layer).filter(|step| step.ignored.is_none()))
                    .into_iter()
                    .find(Option::is_some)
                else {
                    continue;
                };
                let name = key.trim_start_matches("editor.");
                let _ = writeln!(out, "{name} = {}{}", shown(key, &step.value), self.note(key));
            }
        }

        let _ = writeln!(out, "\n[theme]");
        let _ = writeln!(
            out,
            "polarity = {}{}",
            shown("theme.polarity", &c.get("theme.polarity").unwrap_or_default()),
            self.note("theme.polarity")
        );
        if c.roles.is_empty() {
            let _ = writeln!(out, "# [theme.roles] — none overridden; all derived");
        } else {
            let _ = writeln!(out, "\n[theme.roles]");
            for (role, color) in &c.roles {
                let note = self.note(&format!("theme.roles.{role}"));
                let _ = writeln!(out, "{} = {}{note}", toml_string(role), toml_string(color));
            }
        }

        let _ = writeln!(out, "\n[ui]");
        for key in [
            "ui.mouse",
            "ui.alternate_screen",
            "ui.keyboard_enhancement",
            "ui.hyperlinks",
            "ui.lightbulb",
            "ui.undercurl",
            "ui.clipboard",
            "ui.truecolor",
            "ui.hover_delay_ms",
        ] {
            let name = key.trim_start_matches("ui.");
            let value = shown(key, &c.get(key).unwrap_or_default());
            let _ = writeln!(out, "{name} = {value}{}", self.note(key));
        }
        match c.double_click_ms {
            Some(ms) => {
                let _ = writeln!(out, "double_click_ms = {ms}{}", self.note("ui.double_click_ms"));
            }
            None => {
                let _ = writeln!(out, "# double_click_ms — the platform's usual value");
            }
        }

        if c.keys.is_empty() {
            let _ = writeln!(out, "\n# [keys] — none added; the defaults are listed by `nun keys`");
        } else {
            let _ = writeln!(out, "\n[keys]");
            for (sequence, command) in &c.keys {
                let note = self.note(&format!("keys.{sequence}"));
                let _ = writeln!(out, "{} = {}{note}", toml_string(sequence), toml_string(command));
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
            let _ = writeln!(out, "\n[lsp.{language}]");
            for field in ["command", "args", "enabled", "format_on_save"] {
                let key = format!("lsp.{language}.{field}");
                if field == "enabled" && server.enabled && self.origin(&key) == Origin::Default {
                    continue;
                }
                let value = shown(&key, &c.get(&key).unwrap_or_default());
                let _ = writeln!(out, "{field} = {value}{}", self.note(&key));
            }
        }

        self.describe_withheld(&mut out);
        if !self.problems.is_empty() {
            let _ = writeln!(out, "\n# problems");
            for problem in &self.problems {
                let _ = writeln!(out, "# {problem}");
            }
        }
        out
    }

    /// The layers, in order, and whether each applies.
    fn describe_layers(&self, file: Option<&Path>) -> String {
        use fmt::Write as _;
        let mut out = String::from("# layers, each over the one before:\n#   default\n");
        if let Some(user) = &self.user {
            let _ = writeln!(out, "#   user          {}", user.path.display());
        } else {
            let shown = user_config_path()
                .map_or_else(|| "~/.config/nun/nun.toml".into(), |path| path.display().to_string());
            let _ = writeln!(out, "#   user          {shown} (none)");
        }
        match file {
            Some(file) => {
                let _ = writeln!(out, "#   editorconfig  as it applies to {}", file.display());
            }
            None => {
                let _ =
                    writeln!(out, "#   editorconfig  per file; `nun config <file>` includes it");
            }
        }
        match &self.project {
            Some(project) => {
                let state = match project.trust {
                    Trust::Trusted => "trusted",
                    Trust::Changed => "trusted, but its risky settings changed since: those wait",
                    Trust::Unknown => "not trusted yet: inert until accepted in the editor",
                    Trust::Ignored => "ignored",
                };
                let _ =
                    writeln!(out, "#   project       {} ({state})", project.file.path.display());
            }
            None => {
                let _ = writeln!(out, "#   project       no {PROJECT_FILE} here");
            }
        }
        out
    }

    /// What the project file holds that does not apply, commented out.
    fn describe_withheld(&self, out: &mut String) {
        use fmt::Write as _;
        let Some(project) = &self.project else { return };
        let waiting = project.would_change();
        if waiting.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n# {} would set, if trusted:", project.file.path.display());
        for change in waiting {
            let _ = writeln!(out, "# {change}");
        }
    }

    fn note(&self, key: &str) -> String {
        match self.origin(key) {
            Origin::Default => String::new(),
            origin @ Origin::File { .. } => format!("    # {origin}"),
        }
    }

    /// What `key` resolved to here, and why: every layer's say on it, which
    /// one won, and who may set it. With `file`, the `.editorconfig` files
    /// that bear on it are included.
    #[must_use]
    pub fn explain(&self, key: &str, file: Option<(&Path, &[EditorConfig])>) -> String {
        use fmt::Write as _;
        let Some(setting) = schema::find(key) else {
            let parts: Vec<&str> = key.split('.').collect();
            let hint = schema::nearest(&parts)
                .map_or_else(String::new, |near| format!(" Did you mean `{near}`?"));
            return format!("There is no setting called `{key}`.{hint}\n");
        };
        let steps = if key.starts_with("editor.") && whitespace::KEYS.contains(&key) {
            let (path, configs) = file.unwrap_or((Path::new(""), &[]));
            whitespace::chains(self, path, configs).remove(key).unwrap_or_default()
        } else {
            let default = Config::default().get(key);
            let default = default.map(|value| whitespace::Step {
                origin: Origin::Default,
                value,
                ignored: None,
            });
            let mut steps: Vec<_> = default.into_iter().collect();
            steps.extend(self.file_steps(key, Layer::User));
            steps.extend(self.file_steps(key, Layer::Project));
            steps
        };

        let used = steps.iter().rposition(|step| step.ignored.is_none());
        let mut out = String::new();
        match used {
            Some(at) => {
                let _ = writeln!(out, "{key} = {}", shown(key, &steps[at].value));
            }
            None => {
                let _ = writeln!(out, "{key} is not set");
            }
        }
        let _ = writeln!(out, "  {}\n", setting.about);
        if steps.is_empty() {
            let _ = writeln!(out, "  No layer sets it, and it has no default.");
        }
        for (at, step) in steps.iter().enumerate() {
            let mark = if Some(at) == used { "→" } else { " " };
            let why = match (&step.ignored, used) {
                (Some(why), _) => format!("  (not used: {why})"),
                (None, Some(used)) if at < used => "  (overridden)".to_string(),
                _ => String::new(),
            };
            let _ = writeln!(out, "  {mark} {:<16} {}{why}", shown(key, &step.value), step.origin);
        }
        if key.starts_with("editor.") && file.is_none() {
            let _ = writeln!(
                out,
                "\n  .editorconfig applies per file: `nun config --explain {key} <file>` includes it."
            );
        }
        let who = match setting.scope {
            Scope::User => "only your own nun.toml; a project's file cannot",
            Scope::Project => "your nun.toml, or a trusted project's .nun.toml",
            Scope::Trusted => {
                "your nun.toml, or a trusted project's .nun.toml — and changing it there asks for \
                 trust again"
            }
        };
        let _ = writeln!(out, "\n  Who may set it: {who}.");
        out
    }
}

/// A value as the file would write it: strings quoted, the rest as they are.
fn shown(key: &str, value: &str) -> String {
    match schema::find(key).map(|setting| setting.kind) {
        Some(schema::Kind::Text | schema::Kind::OneOf(_) | schema::Kind::Command) => {
            toml_string(value)
        }
        _ => value.to_string(),
    }
}

fn describe_whitespace(out: &mut String, whitespace: &Whitespace) {
    use fmt::Write as _;
    let note = |key: &str| match whitespace.origin(key) {
        Origin::Default => String::new(),
        origin @ Origin::File { .. } => format!("    # {origin}"),
    };
    let word = |value: Option<&str>| value.map_or_else(|| "# unset".to_string(), toml_string);
    let lines = [
        (
            "indent_style",
            word(whitespace.indent_style.map(|style| match style {
                IndentStyle::Tab => "tab",
                IndentStyle::Space => "space",
            })),
        ),
        ("indent_size", whitespace.indent_size.to_string()),
        ("tab_width", whitespace.tab_width.to_string()),
        (
            "end_of_line",
            word(whitespace.end_of_line.map(|ending| match ending {
                EndOfLine::Lf => "lf",
                EndOfLine::Crlf => "crlf",
            })),
        ),
        (
            "charset",
            word(whitespace.charset.map(|charset| match charset {
                Charset::Utf8 => "utf-8",
                Charset::Utf8Bom => "utf-8-bom",
            })),
        ),
        ("trim_trailing_whitespace", whitespace.trim_trailing_whitespace.to_string()),
        ("insert_final_newline", whitespace.insert_final_newline.to_string()),
    ];
    for (name, value) in lines {
        let note = note(&format!("editor.{name}"));
        if let Some(unset) = value.strip_prefix("# ") {
            let _ = writeln!(out, "# {name} — {unset}; each file keeps its own");
        } else {
            let _ = writeln!(out, "{name} = {value}{note}");
        }
    }
    for problem in &whitespace.problems {
        let _ = writeln!(out, "# {problem}");
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

/// News from whatever watches the files, for the editor to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum News {
    /// A layer changed: here is everything, resolved again.
    Settings(Box<Loaded>),
    /// The `.editorconfig` files that bear on `path`, nearest first — new, or
    /// changed since last said.
    EditorConfig {
        /// The file they are for.
        path: PathBuf,
        /// The files, read.
        configs: Vec<EditorConfig>,
    },
    /// A decision about a project could not be written down, and why.
    NotRemembered(String),
}

/// Where nun looks for the person's file.
///
/// `$XDG_CONFIG_HOME` if set, otherwise `~/.config`, which is what every other
/// tool on this machine already uses.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(xdg).join("nun").join("nun.toml"));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config").join("nun").join("nun.toml"))
}

#[cfg(test)]
mod tests;
