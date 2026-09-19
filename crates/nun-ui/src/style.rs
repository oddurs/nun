//! The only place in the UI that may name a colour.
//!
//! Widgets ask for a [`Role`] and get a `Style`. Nothing downstream constructs
//! a `ratatui::style::Color`, which is what keeps the terminal-derived palette
//! from being quietly bypassed one widget at a time.

use nun_theme::{Ramp, Rgb, Role};
use ratatui::style::{Color, Modifier, Style};

/// A derived ramp, ready to hand out ratatui styles.
#[derive(Debug, Clone)]
pub struct Palette {
    ramp: Ramp,
}

impl Palette {
    /// Wrap a derived ramp.
    #[must_use]
    pub const fn new(ramp: Ramp) -> Self {
        Self { ramp }
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
    fn the_caret_line_gutter_is_stronger_than_the_rest() {
        let palette = palette();
        assert!(palette.gutter(true).add_modifier.contains(Modifier::BOLD));
        assert!(!palette.gutter(false).add_modifier.contains(Modifier::BOLD));
    }
}
