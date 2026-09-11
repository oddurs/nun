//! Turning a sampled palette into the colours the UI actually asks for.
//!
//! Sixteen ANSI slots plus a foreground and background are not enough to draw a
//! UI: surfaces, borders and dim text all have to be derived. They are derived
//! in OKLCH so a lightness step looks like one, and they are derived as *mixes
//! between the terminal's own ground and figure* so they can never clash with
//! either.
//!
//! Everything downstream reads a [`Role`], never a colour.

use crate::color::{Oklch, Rgb, contrast_ratio};
use crate::probe::{Ansi, Probe};

/// Whether the terminal is dark-on-light or light-on-dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Light text on a dark ground.
    Dark,
    /// Dark text on a light ground.
    Light,
}

impl Polarity {
    /// The one direction a derived surface can move away from the ground.
    ///
    /// Dark grounds go lighter, light grounds go darker. There is only one
    /// direction because the ground may be at an extreme: pure black has
    /// nothing below it and pure white nothing above, so every surface — even
    /// a nominally recessed one — steps the same way and differs by magnitude.
    const fn away(self) -> f64 {
        match self {
            Self::Dark => 1.0,
            Self::Light => -1.0,
        }
    }
}

/// A semantic slot. Widgets name one of these; they never name a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The editor background.
    Ground,
    /// Panels and sidebars, one step from the ground.
    Raised,
    /// Floating surfaces: the palette, popovers, menus.
    Overlay,
    /// Recessed areas, such as an inactive tab strip.
    Sunken,
    /// Hairlines and dividers.
    Line,
    /// A divider that needs to be seen rather than felt.
    LineStrong,
    /// Body text.
    Text,
    /// Secondary text: line numbers, status segments.
    Dim,
    /// Tertiary text: placeholders, disabled entries.
    Faint,
    /// The one accent colour.
    Accent,
    /// Text drawn on top of [`Role::Accent`].
    OnAccent,
    /// The selection wash.
    Selection,
    /// The current line's highlight.
    CursorLine,
    /// Errors.
    Error,
    /// Warnings.
    Warn,
    /// Informational diagnostics and hints.
    Info,
    /// Git: added lines.
    Added,
    /// Git: removed lines.
    Removed,
    /// Git: changed lines.
    Changed,
    /// Syntax: keywords.
    Keyword,
    /// Syntax: type names.
    Type,
    /// Syntax: string literals.
    StringLiteral,
    /// Syntax: numeric literals.
    Number,
    /// Syntax: comments.
    Comment,
    /// Syntax: function names.
    Function,
    /// Syntax: punctuation and operators.
    Punctuation,
}

/// Contrast floors nun guarantees regardless of what the terminal reported.
///
/// A palette configured with, say, mid-grey on mid-grey would otherwise produce
/// a technically faithful and completely unreadable editor. Derivation only
/// ever *increases* contrast, so a sane terminal passes through untouched.
mod floor {
    /// Body text against its ground. WCAG AA for normal text.
    pub const TEXT: f64 = 4.5;
    /// Secondary text against its ground.
    pub const DIM: f64 = 3.0;
    /// Tertiary text — below AA on purpose, but still legible.
    pub const FAINT: f64 = 2.2;
    /// Semantic and syntax colours against the ground.
    pub const ROLE: f64 = 3.0;
}

/// The full set of derived colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ramp {
    polarity: Polarity,
    ground: Rgb,
    raised: Rgb,
    overlay: Rgb,
    sunken: Rgb,
    line: Rgb,
    line_strong: Rgb,
    text: Rgb,
    dim: Rgb,
    faint: Rgb,
    accent: Rgb,
    on_accent: Rgb,
    selection: Rgb,
    cursor_line: Rgb,
    error: Rgb,
    warn: Rgb,
    info: Rgb,
    added: Rgb,
    removed: Rgb,
    changed: Rgb,
    keyword: Rgb,
    type_name: Rgb,
    string_literal: Rgb,
    number: Rgb,
    comment: Rgb,
    function: Rgb,
    punctuation: Rgb,
}

impl Ramp {
    /// Whether this ramp is for a dark or light terminal.
    #[must_use]
    pub const fn polarity(&self) -> Polarity {
        self.polarity
    }

    /// The colour for a role.
    #[must_use]
    pub const fn get(&self, role: Role) -> Rgb {
        match role {
            Role::Ground => self.ground,
            Role::Raised => self.raised,
            Role::Overlay => self.overlay,
            Role::Sunken => self.sunken,
            Role::Line => self.line,
            Role::LineStrong => self.line_strong,
            Role::Text => self.text,
            Role::Dim => self.dim,
            Role::Faint => self.faint,
            Role::Accent => self.accent,
            Role::OnAccent => self.on_accent,
            Role::Selection => self.selection,
            Role::CursorLine => self.cursor_line,
            Role::Error => self.error,
            Role::Warn => self.warn,
            Role::Info => self.info,
            Role::Added => self.added,
            Role::Removed => self.removed,
            Role::Changed => self.changed,
            Role::Keyword => self.keyword,
            Role::Type => self.type_name,
            Role::StringLiteral => self.string_literal,
            Role::Number => self.number,
            Role::Comment => self.comment,
            Role::Function => self.function,
            Role::Punctuation => self.punctuation,
        }
    }

    /// Replace one role, leaving the rest derived.
    ///
    /// This is the escape hatch behind `[theme.roles]` in the config: nudge the
    /// accent without opting out of inheritance for everything else.
    pub const fn set(&mut self, role: Role, color: Rgb) {
        match role {
            Role::Ground => self.ground = color,
            Role::Raised => self.raised = color,
            Role::Overlay => self.overlay = color,
            Role::Sunken => self.sunken = color,
            Role::Line => self.line = color,
            Role::LineStrong => self.line_strong = color,
            Role::Text => self.text = color,
            Role::Dim => self.dim = color,
            Role::Faint => self.faint = color,
            Role::Accent => self.accent = color,
            Role::OnAccent => self.on_accent = color,
            Role::Selection => self.selection = color,
            Role::CursorLine => self.cursor_line = color,
            Role::Error => self.error = color,
            Role::Warn => self.warn = color,
            Role::Info => self.info = color,
            Role::Added => self.added = color,
            Role::Removed => self.removed = color,
            Role::Changed => self.changed = color,
            Role::Keyword => self.keyword = color,
            Role::Type => self.type_name = color,
            Role::StringLiteral => self.string_literal = color,
            Role::Number => self.number = color,
            Role::Comment => self.comment = color,
            Role::Function => self.function = color,
            Role::Punctuation => self.punctuation = color,
        }
    }

    /// Every role paired with its colour, for `nun theme dump`.
    #[must_use]
    pub fn entries(&self) -> Vec<(Role, Rgb)> {
        Role::ALL.iter().map(|&role| (role, self.get(role))).collect()
    }

    /// The ramp as a TOML fragment, ready to paste into a config file.
    #[must_use]
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::from("[theme.roles]\n");
        for (role, color) in self.entries() {
            let _ = writeln!(out, "{} = \"{}\"", role.key(), color.to_hex());
        }
        out
    }
}

impl Role {
    /// Every role, in the order `nun theme dump` prints them.
    pub const ALL: [Self; 26] = [
        Self::Ground,
        Self::Raised,
        Self::Overlay,
        Self::Sunken,
        Self::Line,
        Self::LineStrong,
        Self::Text,
        Self::Dim,
        Self::Faint,
        Self::Accent,
        Self::OnAccent,
        Self::Selection,
        Self::CursorLine,
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Added,
        Self::Removed,
        Self::Changed,
        Self::Keyword,
        Self::Type,
        Self::StringLiteral,
        Self::Number,
        Self::Comment,
        Self::Function,
        Self::Punctuation,
    ];

    /// The name this role has in a config file.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Ground => "ground",
            Self::Raised => "raised",
            Self::Overlay => "overlay",
            Self::Sunken => "sunken",
            Self::Line => "line",
            Self::LineStrong => "line_strong",
            Self::Text => "text",
            Self::Dim => "dim",
            Self::Faint => "faint",
            Self::Accent => "accent",
            Self::OnAccent => "on_accent",
            Self::Selection => "selection",
            Self::CursorLine => "cursor_line",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
            Self::Keyword => "keyword",
            Self::Type => "type",
            Self::StringLiteral => "string",
            Self::Number => "number",
            Self::Comment => "comment",
            Self::Function => "function",
            Self::Punctuation => "punctuation",
        }
    }

    /// Look a role up by its config key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|role| role.key() == key)
    }
}

/// Derive the full role ramp from whatever the terminal reported.
#[must_use]
pub fn derive(probe: &Probe) -> Ramp {
    derive_with_polarity(probe, polarity_of(probe.background))
}

/// Which polarity a background implies.
#[must_use]
fn polarity_of(background: Rgb) -> Polarity {
    if Oklch::from(background).l < 0.5 { Polarity::Dark } else { Polarity::Light }
}

/// Derive with the polarity forced, for a terminal whose background is
/// translucent and therefore reports something misleading.
#[must_use]
pub fn derive_with_polarity(probe: &Probe, polarity: Polarity) -> Ramp {
    let ground = Oklch::from(probe.background);
    let figure = Oklch::from(probe.foreground);
    let away = polarity.away();

    // Surfaces are defined by how far they read from the ground, not by a fixed
    // lightness delta. A delta cannot work at both ends of the range: 0.042 of
    // OKLCH lightness is a clear step from mid-grey and still rounds to black
    // against pure black. Stepping until a contrast target is met does work
    // everywhere, and it is the separation a person actually perceives.
    let surface = |target| step_until_contrast(ground, probe.background, away, target);
    let sunken = surface(1.04);
    let raised = surface(1.09);
    let overlay = surface(1.22);
    let line = ground.mix(figure, 0.14);
    let line_strong = ground.mix(figure, 0.28);

    let text = enforce(figure, probe.background, floor::TEXT);
    let dim = enforce(ground.mix(figure, 0.62), probe.background, floor::DIM);
    let faint = enforce(ground.mix(figure, 0.42), probe.background, floor::FAINT);

    // The accent comes from the terminal's blue, but a palette where blue has
    // been desaturated to near-grey would give an accent indistinguishable from
    // body text, so chroma has a floor.
    let accent_source = Oklch::from(probe.ansi(Ansi::Blue)).chroma_floor(0.11);
    let accent = enforce(accent_source, probe.background, floor::ROLE);
    let accent_rgb = Rgb::from(accent);

    let role = |slot: Ansi| enforce(Oklch::from(probe.ansi(slot)), probe.background, floor::ROLE);

    Ramp {
        polarity,
        ground: probe.background,
        raised: Rgb::from(raised),
        overlay: Rgb::from(overlay),
        sunken: Rgb::from(sunken),
        line: Rgb::from(line),
        line_strong: Rgb::from(line_strong),
        text: Rgb::from(text),
        dim: Rgb::from(dim),
        faint: Rgb::from(faint),
        accent: accent_rgb,
        on_accent: readable_on(accent_rgb),
        selection: Rgb::from(ground.mix(accent, 0.22)),
        cursor_line: Rgb::from(surface(1.035)),
        error: Rgb::from(role(Ansi::Red)),
        warn: Rgb::from(role(Ansi::Yellow)),
        info: Rgb::from(role(Ansi::Cyan)),
        added: Rgb::from(role(Ansi::Green)),
        removed: Rgb::from(role(Ansi::Red)),
        changed: Rgb::from(role(Ansi::Yellow)),
        keyword: Rgb::from(role(Ansi::Magenta)),
        type_name: Rgb::from(role(Ansi::Cyan)),
        string_literal: Rgb::from(role(Ansi::Green)),
        number: Rgb::from(role(Ansi::Yellow)),
        comment: Rgb::from(enforce(
            ground.mix(figure, 0.48).chroma_ceiling(0.04),
            probe.background,
            floor::FAINT,
        )),
        function: Rgb::from(enforce(accent_source, probe.background, floor::ROLE)),
        punctuation: Rgb::from(enforce(ground.mix(figure, 0.72), probe.background, floor::DIM)),
    }
}

/// Step a colour away from `against` until it reads as separate from it.
///
/// Used for surfaces, where the target is a barely-perceptible difference
/// rather than legibility. Gives up once lightness clamps.
fn step_until_contrast(base: Oklch, against: Rgb, away: f64, target: f64) -> Oklch {
    let mut current = base;
    for _ in 0..160 {
        if contrast_ratio(Rgb::from(current), against) >= target {
            break;
        }
        let next = current.lighten(0.004 * away);
        if (next.l - current.l).abs() < 1e-12 {
            break;
        }
        current = next;
    }
    current
}

/// Push a colour away from `against` until it clears `minimum` contrast.
///
/// Only ever increases contrast, so a terminal whose palette is already legible
/// passes through unchanged. Gives up once lightness clamps, which is the point
/// past which nothing more can be done in this direction.
fn enforce(color: Oklch, against: Rgb, minimum: f64) -> Oklch {
    let away = if Oklch::from(against).l < 0.5 { 1.0 } else { -1.0 };
    let mut current = color;
    for _ in 0..64 {
        if contrast_ratio(Rgb::from(current), against) >= minimum {
            break;
        }
        let next = current.lighten(0.015 * away);
        if (next.l - current.l).abs() < 1e-9 {
            break;
        }
        current = next;
    }
    current
}

/// Black or white, whichever is legible on `background`.
fn readable_on(background: Rgb) -> Rgb {
    let black = Rgb::new(0, 0, 0);
    let white = Rgb::new(255, 255, 255);
    if contrast_ratio(background, black) >= contrast_ratio(background, white) {
        black
    } else {
        white
    }
}
