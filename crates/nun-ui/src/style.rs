//! The only place in the UI that may name a colour.
//!
//! Widgets ask for a [`Role`] and get a `Style`. Nothing downstream constructs
//! a `ratatui::style::Color`, which is what keeps the terminal-derived palette
//! from being quietly bypassed one widget at a time.
//!
//! The palette carries the glyphs too, so the look travels as one value: a
//! widget handed a palette can colour a mark and knows which mark to draw,
//! with no second thing to thread through every signature.

use nun_term::{Attrs, Ink};
use nun_theme::{Ramp, Rgb, Role};
use ratatui::style::{Color, Modifier, Style};

use crate::glyph::{Glyph, Glyphs};

/// A derived ramp, ready to hand out ratatui styles, and the glyphs to draw.
#[derive(Debug, Clone)]
pub struct Palette {
    ramp: Ramp,
    glyphs: Glyphs,
}

impl Palette {
    /// Wrap a derived ramp, drawing the default glyphs.
    #[must_use]
    pub fn new(ramp: Ramp) -> Self {
        Self { ramp, glyphs: Glyphs::default() }
    }

    /// The same colours, drawing `glyphs`.
    #[must_use]
    pub fn with_glyphs(self, glyphs: Glyphs) -> Self {
        Self { glyphs, ..self }
    }

    /// What to draw for one glyph role.
    #[must_use]
    pub fn glyph(&self, glyph: Glyph) -> &str {
        self.glyphs.get(glyph)
    }

    /// Every glyph in use.
    #[must_use]
    pub const fn glyphs(&self) -> &Glyphs {
        &self.glyphs
    }

    /// The ramp underneath.
    #[must_use]
    pub const fn ramp(&self) -> &Ramp {
        &self.ramp
    }

    /// One role as a foreground colour on the editor ground.
    #[must_use]
    pub fn fg(&self, role: Role) -> Style {
        Style::default().fg(to_color(self.ramp.get(role))).bg(self.ground())
    }

    /// One role as a foreground only, over whatever background is already
    /// there — for a glyph on a row that has its own wash.
    #[must_use]
    pub fn ink(&self, role: Role) -> Style {
        Style::default().fg(to_color(self.ramp.get(role)))
    }

    /// One role as a background, with legible text on top.
    #[must_use]
    pub fn on(&self, role: Role, text: Role) -> Style {
        Style::default().fg(to_color(self.ramp.get(text))).bg(to_color(self.ramp.get(role)))
    }

    /// The editor background.
    #[must_use]
    pub fn ground(&self) -> Color {
        to_color(self.ramp.get(Role::Ground))
    }

    /// Body text on the ground.
    #[must_use]
    pub fn text(&self) -> Style {
        self.fg(Role::Text)
    }

    /// The selection wash, keeping whatever foreground the text already had.
    ///
    /// A reverse-video selection would throw away syntax highlighting; blending
    /// a background keeps it.
    #[must_use]
    pub fn selection(&self) -> Style {
        Style::default().bg(to_color(self.ramp.get(Role::Selection)))
    }

    /// The current line's wash.
    #[must_use]
    pub fn cursor_line(&self) -> Style {
        Style::default().bg(to_color(self.ramp.get(Role::CursorLine)))
    }

    /// A snippet tab-stop's wash, stronger on the stop being edited, keeping
    /// whatever foreground the text already had.
    #[must_use]
    pub fn tabstop(&self, current: bool) -> Style {
        let role = if current { Role::TabstopCurrent } else { Role::Tabstop };
        Style::default().bg(to_color(self.ramp.get(role)))
    }

    /// One role as a background only, keeping whatever foreground the text
    /// already had: a diff's washes, which syntax colours read through.
    #[must_use]
    pub fn wash(&self, role: Role) -> Style {
        Style::default().bg(to_color(self.ramp.get(role)))
    }

    /// An underline in one role's colour, over whatever the text already has.
    ///
    /// Asking for a colour is asking for a curl: the backend draws one where
    /// the terminal can, and a straight line, coloured or not, where it
    /// cannot. See `NunBackend`.
    #[must_use]
    pub fn underline(&self, role: Role) -> Style {
        Style::default()
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(to_color(self.ramp.get(role)))
    }

    /// A cell of a program's output in the terminal panel.
    ///
    /// Content, not chrome: the colours are the ones the program asked for,
    /// passed through as it asked for them — an ANSI index stays an index, so
    /// it comes out in the terminal's own palette, exactly as it would outside
    /// the editor. Only the program's default colours become roles, so an
    /// uncoloured shell sits on the editor's own ground in its own text.
    #[must_use]
    pub fn content(&self, fg: Ink, bg: Ink, attrs: Attrs) -> Style {
        let ink = |ink: Ink, default: Role| match ink {
            Ink::Default => to_color(self.ramp.get(default)),
            Ink::Indexed(index) => Color::Indexed(index),
            Ink::Rgb(r, g, b) => Color::Rgb(r, g, b),
        };
        let (mut fg, mut bg) = (ink(fg, Role::Text), ink(bg, Role::Ground));
        if attrs.contains(Attrs::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut style = Style::default().fg(fg).bg(bg);
        for (attr, modifier) in [
            (Attrs::BOLD, Modifier::BOLD),
            (Attrs::DIM, Modifier::DIM),
            (Attrs::ITALIC, Modifier::ITALIC),
            (Attrs::UNDERLINE, Modifier::UNDERLINED),
            (Attrs::STRIKE, Modifier::CROSSED_OUT),
            (Attrs::HIDDEN, Modifier::HIDDEN),
        ] {
            if attrs.contains(attr) {
                style = style.add_modifier(modifier);
            }
        }
        style
    }

    /// Gutter digits, emphasised on the caret's own line.
    #[must_use]
    pub fn gutter(&self, current: bool) -> Style {
        if current { self.fg(Role::Dim).add_modifier(Modifier::BOLD) } else { self.fg(Role::Faint) }
    }
}

/// The one conversion from nun's colour type to ratatui's.
fn to_color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nun_theme::{Probe, derive};

    fn palette() -> Palette {
        Palette::new(derive(&Probe::builtin_dark()))
    }

    #[test]
    fn roles_become_true_colour_styles() {
        let style = palette().fg(Role::Error);
        let expected = derive(&Probe::builtin_dark()).get(Role::Error);
        assert_eq!(style.fg, Some(Color::Rgb(expected.r, expected.g, expected.b)));
    }

    #[test]
    fn every_foreground_style_paints_the_ground_behind_it() {
        // A style with no background inherits whatever was underneath, which
        // shows through as the host terminal's colour rather than nun's.
        let palette = palette();
        for role in Role::ALL {
            assert_eq!(palette.fg(role).bg, Some(palette.ground()), "{role:?} left its bg unset");
        }
    }

    #[test]
    fn selection_keeps_the_foreground_it_lands_on() {
        assert_eq!(palette().selection().fg, None, "a selection must not flatten syntax colour");
    }

    #[test]
    fn a_tabstop_is_a_wash_stronger_on_the_current_one() {
        let palette = palette();
        let (other, current) = (palette.tabstop(false), palette.tabstop(true));
        assert_eq!((other.fg, current.fg), (None, None), "a stop keeps its syntax colour");
        assert_ne!(other.bg, current.bg);
    }

    #[test]
    fn an_underline_is_coloured_from_its_role_and_leaves_the_text_alone() {
        let palette = palette();
        let style = palette.underline(Role::Warn);
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        let expected = palette.ramp().get(Role::Warn);
        assert_eq!(style.underline_color, Some(Color::Rgb(expected.r, expected.g, expected.b)));
        assert_eq!((style.fg, style.bg), (None, None), "the syntax colour shows through");
    }

    #[test]
    fn a_programs_colours_pass_through_and_only_its_defaults_are_roles() {
        let palette = palette();
        let style = palette.content(Ink::Indexed(1), Ink::Rgb(1, 2, 3), Attrs::default());
        assert_eq!((style.fg, style.bg), (Some(Color::Indexed(1)), Some(Color::Rgb(1, 2, 3))));
        let plain = palette.content(Ink::Default, Ink::Default, Attrs::default());
        assert_eq!(plain, palette.text(), "an uncoloured cell is body text on the ground");
        let inverse = palette.content(Ink::Default, Ink::Indexed(4), Attrs::INVERSE);
        assert_eq!(inverse.fg, Some(Color::Indexed(4)));
        assert_eq!(inverse.bg, palette.text().fg);
        let bold = palette.content(Ink::Default, Ink::Default, Attrs::BOLD);
        assert!(bold.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_caret_line_gutter_is_stronger_than_the_rest() {
        let palette = palette();
        assert!(palette.gutter(true).add_modifier.contains(Modifier::BOLD));
        assert!(!palette.gutter(false).add_modifier.contains(Modifier::BOLD));
    }
}
