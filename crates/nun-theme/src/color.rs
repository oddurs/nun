//! sRGB and OKLCH, and the conversions between them.
//!
//! Derivation happens in OKLCH because a lightness step there looks like a
//! lightness step. The same nudge in sRGB is visibly larger on some hues than
//! others, which is what makes naively-derived terminal themes look blotchy.

/// An 8-bit-per-channel sRGB colour, as terminals report and accept them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// A colour from its channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rrggbb`, with or without the leading hash.
    ///
    /// # Errors
    ///
    /// Returns `None` if the string is not six hex digits.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.strip_prefix('#').unwrap_or(text);
        if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&text[i..i + 2], 16).ok();
        Some(Self { r: channel(0)?, g: channel(2)?, b: channel(4)? })
    }

    /// Render as `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Relative luminance per WCAG 2.1, used for contrast ratios.
    #[must_use]
    pub fn luminance(self) -> f64 {
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }
}

/// The contrast ratio between two colours, from 1.0 to 21.0.
///
/// WCAG asks for 4.5 for body text and 3.0 for large text and UI boundaries.
#[must_use]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (a.luminance(), b.luminance());
    let (lighter, darker) = if x > y { (x, y) } else { (y, x) };
    (lighter + 0.05) / (darker + 0.05)
}

/// A colour in cylindrical Oklab: perceptual lightness, chroma, and hue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    /// Perceptual lightness, 0.0 (black) to 1.0 (white).
    pub l: f64,
    /// Chroma. Roughly 0.0 to 0.37 for colours inside sRGB.
    pub c: f64,
    /// Hue in radians.
    pub h: f64,
}

/// sRGB transfer function, gamma-encoded to linear.
fn to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Linear back to gamma-encoded sRGB, clamped into range.
fn from_linear(c: f64) -> u8 {
    let c = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    // Out-of-gamut results are clamped rather than wrapped; a derived colour
    // must always be displayable even when the arithmetic overshoots.
    let scaled = (c * 255.0).round();
    if scaled < 0.0 {
        0
    } else if scaled > 255.0 {
        255
    } else {
        // Exact after the clamp above.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            scaled as u8
        }
    }
}

impl From<Rgb> for Oklch {
    // The single-letter names are Ottosson's, from the reference definition of
    // the transform. Renaming them to something "clearer" would make the code
    // harder to check against the source it came from, not easier.
    #[allow(clippy::many_single_char_names)]
    fn from(rgb: Rgb) -> Self {
        let (r, g, b) = (to_linear(rgb.r), to_linear(rgb.g), to_linear(rgb.b));

        // Ottosson's linear sRGB to LMS matrix, then the cube root.
        let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
        let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
        let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
        let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());

        let lightness = 0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s;
        let a = 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s;
        let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s;

        Self { l: lightness, c: a.hypot(bb), h: bb.atan2(a) }
    }
}

impl From<Oklch> for Rgb {
    #[allow(clippy::many_single_char_names)]
    fn from(oklch: Oklch) -> Self {
        let (a, bb) = (oklch.c * oklch.h.cos(), oklch.c * oklch.h.sin());

        let l = oklch.l + 0.396_337_777_4 * a + 0.215_803_757_3 * bb;
        let m = oklch.l - 0.105_561_345_8 * a - 0.063_854_172_8 * bb;
        let s = oklch.l - 0.089_484_177_5 * a - 1.291_485_548_0 * bb;
        let (l, m, s) = (l * l * l, m * m * m, s * s * s);

        Self {
            r: from_linear(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
            g: from_linear(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
            b: from_linear(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s),
        }
    }
}

impl Oklch {
    /// Shift lightness, clamped to the representable range.
    #[must_use]
    pub fn lighten(self, amount: f64) -> Self {
        Self { l: (self.l + amount).clamp(0.0, 1.0), ..self }
    }

    /// Blend toward `other` by `t`, from 0.0 (self) to 1.0 (other).
    ///
    /// Hue is interpolated the short way around the circle, so a mix between
    /// two nearly-red colours never travels through green.
    #[must_use]
    pub fn mix(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mut delta = other.h - self.h;
        if delta > std::f64::consts::PI {
            delta -= std::f64::consts::TAU;
        } else if delta < -std::f64::consts::PI {
            delta += std::f64::consts::TAU;
        }
        Self {
            l: self.l + (other.l - self.l) * t,
            c: self.c + (other.c - self.c) * t,
            h: self.h + delta * t,
        }
    }

    /// Raise chroma to at least `floor`, leaving more saturated colours alone.
    ///
    /// A terminal configured with a nearly grey "blue" would otherwise give an
    /// accent indistinguishable from the text.
    #[must_use]
    pub fn chroma_floor(self, floor: f64) -> Self {
        Self { c: self.c.max(floor), ..self }
    }

    /// Cap chroma at `ceiling`.
    #[must_use]
    pub fn chroma_ceiling(self, ceiling: f64) -> Self {
        Self { c: self.c.min(ceiling), ..self }
    }
}

/// The levels each channel of the xterm 256-colour cube takes.
const CUBE: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

impl Rgb {
    /// The nearest of the fixed colours of the xterm 256-colour palette —
    /// the 6×6×6 cube and the 24 greys, indices 16 to 255 — judged in Oklab,
    /// so the nearest is the one that looks nearest.
    ///
    /// The first sixteen are never chosen: they are the terminal's own
    /// palette, and could be any colour at all.
    #[must_use]
    pub fn nearest_indexed(self) -> u8 {
        let level = |channel: u8| {
            (0u8..6)
                .min_by_key(|&index| channel.abs_diff(CUBE[usize::from(index)]))
                .unwrap_or_default()
        };
        let (r, g, b) = (level(self.r), level(self.g), level(self.b));
        let cube = (
            16 + 36 * r + 6 * g + b,
            Self::new(CUBE[usize::from(r)], CUBE[usize::from(g)], CUBE[usize::from(b)]),
        );
        // Greys run from 8 to 238 in steps of 10.
        let mean = (u16::from(self.r) + u16::from(self.g) + u16::from(self.b)) / 3;
        let step = u8::try_from((mean.saturating_sub(3) / 10).min(23)).unwrap_or(23);
        let value = 8 + 10 * step;
        let grey = (232 + step, Self::new(value, value, value));
        let here = Oklch::from(self);
        let far = |other: Self| here.distance(Oklch::from(other));
        if far(grey.1) < far(cube.1) { grey.0 } else { cube.0 }
    }
}

impl Oklch {
    /// How far apart two colours look: the straight-line distance in Oklab.
    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        let (a, b) = (self.c * self.h.cos(), self.c * self.h.sin());
        let (x, y) = (other.c * other.h.cos(), other.c * other.h.sin());
        (self.l - other.l).hypot(a - x).hypot(b - y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_in_the_cube_is_itself() {
        assert_eq!(Rgb::new(0xff, 0x00, 0x00).nearest_indexed(), 196);
        assert_eq!(Rgb::new(0x5f, 0x87, 0xaf).nearest_indexed(), 16 + 36 + 12 + 3);
        assert_eq!(Rgb::new(0xff, 0xff, 0xff).nearest_indexed(), 231);
    }

    #[test]
    fn a_grey_between_the_cube_levels_goes_to_the_grey_ramp() {
        assert_eq!(Rgb::new(0x80, 0x80, 0x80).nearest_indexed(), 244);
        assert_eq!(Rgb::new(0x12, 0x12, 0x12).nearest_indexed(), 233);
    }

    #[test]
    fn the_terminals_own_sixteen_are_never_chosen() {
        for value in [0u8, 1, 50, 128, 200, 255] {
            for rgb in
                [Rgb::new(value, 0, 0), Rgb::new(0, value, value), Rgb::new(value, value, value)]
            {
                assert!(rgb.nearest_indexed() >= 16, "{}", rgb.to_hex());
            }
        }
    }

    #[test]
    fn a_colour_stays_close_to_its_own_hue() {
        // An orange, nowhere near a cube corner, stays orange.
        let index = Rgb::new(0xe0, 0x80, 0x30).nearest_indexed();
        assert!((166..=215).contains(&index), "{index}");
    }

    #[track_caller]
    fn assert_round_trips(rgb: Rgb) {
        let back = Rgb::from(Oklch::from(rgb));
        let delta = |a: u8, b: u8| i16::from(a) - i16::from(b);
        assert!(
            delta(back.r, rgb.r).abs() <= 1
                && delta(back.g, rgb.g).abs() <= 1
                && delta(back.b, rgb.b).abs() <= 1,
            "{} round-tripped to {}",
            rgb.to_hex(),
            back.to_hex()
        );
    }

    #[test]
    fn conversion_round_trips_within_one_step_per_channel() {
        for rgb in [
            Rgb::new(0, 0, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(128, 128, 128),
            Rgb::new(255, 0, 0),
            Rgb::new(0, 255, 0),
            Rgb::new(0, 0, 255),
            Rgb::new(16, 20, 24),
            Rgb::new(224, 164, 75),
            Rgb::new(1, 2, 3),
            Rgb::new(254, 253, 252),
        ] {
            assert_round_trips(rgb);
        }
    }

    #[test]
    fn conversion_round_trips_across_the_whole_grey_axis() {
        for v in 0..=255u8 {
            assert_round_trips(Rgb::new(v, v, v));
        }
    }

    #[test]
    fn black_and_white_sit_at_the_ends_of_lightness() {
        assert!(Oklch::from(Rgb::new(0, 0, 0)).l.abs() < 1e-6);
        assert!((Oklch::from(Rgb::new(255, 255, 255)).l - 1.0).abs() < 1e-3);
    }

    #[test]
    fn out_of_gamut_results_clamp_rather_than_wrap() {
        // Lightness past white would overshoot every channel.
        let over = Oklch { l: 1.4, c: 0.0, h: 0.0 };
        assert_eq!(Rgb::from(over), Rgb::new(255, 255, 255));
        let under = Oklch { l: -0.4, c: 0.0, h: 0.0 };
        assert_eq!(Rgb::from(under), Rgb::new(0, 0, 0));
    }

    #[test]
    fn hex_parses_with_and_without_the_hash() {
        assert_eq!(Rgb::from_hex("#e0a44b"), Some(Rgb::new(0xe0, 0xa4, 0x4b)));
        assert_eq!(Rgb::from_hex("E0A44B"), Some(Rgb::new(0xe0, 0xa4, 0x4b)));
        assert_eq!(Rgb::from_hex("#abc"), None);
        assert_eq!(Rgb::from_hex("#gggggg"), None);
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mixing_takes_the_short_way_around_the_hue_circle() {
        // Two hues either side of the wrap point should meet near it, not
        // travel the long way through the opposite side of the wheel.
        let a = Oklch { l: 0.5, c: 0.1, h: 3.0 };
        let b = Oklch { l: 0.5, c: 0.1, h: -3.0 };
        let mid = a.mix(b, 0.5);
        let distance_to_pi = (mid.h.abs() - std::f64::consts::PI).abs();
        assert!(distance_to_pi < 0.2, "midpoint hue {} is not near the wrap", mid.h);
    }

    #[test]
    fn chroma_floor_leaves_saturated_colours_alone() {
        let vivid = Oklch { l: 0.6, c: 0.25, h: 1.0 };
        assert!((vivid.chroma_floor(0.11).c - 0.25).abs() < 1e-9);
        let grey = Oklch { l: 0.6, c: 0.01, h: 1.0 };
        assert!((grey.chroma_floor(0.11).c - 0.11).abs() < 1e-9);
    }
}
