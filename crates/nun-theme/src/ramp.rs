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
    /// A live snippet's tab-stops, other than the one being edited.
    Tabstop,
    /// The tab-stop being edited, in every place it appears.
    TabstopCurrent,
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
    /// A diff's added lines: a wash of [`Role::Added`] behind their text.
    AddedWash,
    /// A diff's removed lines: a wash of [`Role::Removed`] behind their text.
    RemovedWash,
    /// The words that changed within an added line, washed more strongly.
    AddedEmphasis,
    /// The words that changed within a removed line, washed more strongly.
    RemovedEmphasis,
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
    tabstop: Rgb,
    tabstop_current: Rgb,
    error: Rgb,
    warn: Rgb,
    info: Rgb,
    added: Rgb,
    removed: Rgb,
    changed: Rgb,
    added_wash: Rgb,
    removed_wash: Rgb,
    added_emphasis: Rgb,
    removed_emphasis: Rgb,
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
            Role::Tabstop => self.tabstop,
            Role::TabstopCurrent => self.tabstop_current,
            Role::Error => self.error,
            Role::Warn => self.warn,
            Role::Info => self.info,
            Role::Added => self.added,
            Role::Removed => self.removed,
            Role::Changed => self.changed,
            Role::AddedWash => self.added_wash,
            Role::RemovedWash => self.removed_wash,
            Role::AddedEmphasis => self.added_emphasis,
            Role::RemovedEmphasis => self.removed_emphasis,
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
            Role::Tabstop => self.tabstop = color,
            Role::TabstopCurrent => self.tabstop_current = color,
            Role::Error => self.error = color,
            Role::Warn => self.warn = color,
            Role::Info => self.info = color,
            Role::Added => self.added = color,
            Role::Removed => self.removed = color,
            Role::Changed => self.changed = color,
            Role::AddedWash => self.added_wash = color,
            Role::RemovedWash => self.removed_wash = color,
            Role::AddedEmphasis => self.added_emphasis = color,
            Role::RemovedEmphasis => self.removed_emphasis = color,
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
    pub const ALL: [Self; 32] = [
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
        Self::Tabstop,
        Self::TabstopCurrent,
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Added,
        Self::Removed,
        Self::Changed,
        Self::AddedWash,
        Self::RemovedWash,
        Self::AddedEmphasis,
        Self::RemovedEmphasis,
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
            Self::Tabstop => "tabstop",
            Self::TabstopCurrent => "tabstop_current",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
            Self::AddedWash => "added_wash",
            Self::RemovedWash => "removed_wash",
            Self::AddedEmphasis => "added_emphasis",
            Self::RemovedEmphasis => "removed_emphasis",
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
    let (added, removed) = (role(Ansi::Green), role(Ansi::Red));

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
        // Washes of the accent, like the selection but lighter: the one being
        // edited stands out from the rest, and a selection still reads over
        // either.
        tabstop: Rgb::from(ground.mix(accent, 0.08)),
        tabstop_current: Rgb::from(ground.mix(accent, 0.15)),
        error: Rgb::from(role(Ansi::Red)),
        warn: Rgb::from(role(Ansi::Yellow)),
        info: Rgb::from(role(Ansi::Cyan)),
        added: Rgb::from(added),
        removed: Rgb::from(removed),
        changed: Rgb::from(role(Ansi::Yellow)),
        // Washes of the diff colours, like the tab-stops': faint enough that
        // syntax colours still read over a whole line of them, and stronger
        // on the words within a line that actually changed.
        added_wash: Rgb::from(tint(ground, added, 0.14)),
        removed_wash: Rgb::from(tint(ground, removed, 0.14)),
        added_emphasis: Rgb::from(tint(ground, added, 0.32)),
        removed_emphasis: Rgb::from(tint(ground, removed, 0.32)),
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

/// `ground` washed with `colour` by `t`: the colour's chroma and hue by that
/// much, but only a third of its lightness. A wash sits behind text in every
/// syntax colour, faint comments included, and it is the lightness that
/// would take their contrast away; the hue is what says added or removed.
fn tint(ground: Oklch, colour: Oklch, t: f64) -> Oklch {
    let blended = ground.blend(colour, t);
    Oklch { l: ground.l + (blended.l - ground.l) / 3.0, ..blended }
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
