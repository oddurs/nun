//! Asking the terminal what its colours are.
//!
//! Written sans-I/O: [`ProbeSession`] produces the bytes to write and is fed
//! whatever arrives, returning the bytes that were not part of a reply so the
//! caller can hand them on to the input layer. The caller owns raw mode, the
//! reads, and the timeout.
//!
//! That split exists because the failure mode here is not "the query was not
//! sent", it is "a reply arrived interleaved with a keystroke and one of them
//! was eaten". Keeping the state machine free of I/O is what makes that
//! testable.

use crate::color::Rgb;

/// A slot in the terminal's 16-colour palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Ansi {
    /// Slot 0.
    Black = 0,
    /// Slot 1 — errors.
    Red = 1,
    /// Slot 2 — strings and additions.
    Green = 2,
    /// Slot 3 — warnings.
    Yellow = 3,
    /// Slot 4 — the accent.
    Blue = 4,
    /// Slot 5 — keywords.
    Magenta = 5,
    /// Slot 6 — types and information.
    Cyan = 6,
    /// Slot 7.
    White = 7,
    /// Slot 8.
    BrightBlack = 8,
    /// Slot 9.
    BrightRed = 9,
    /// Slot 10.
    BrightGreen = 10,
    /// Slot 11.
    BrightYellow = 11,
    /// Slot 12.
    BrightBlue = 12,
    /// Slot 13.
    BrightMagenta = 13,
    /// Slot 14.
    BrightCyan = 14,
    /// Slot 15.
    BrightWhite = 15,
}

/// Where a probe's colours actually came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The terminal answered the queries.
    Terminal,
    /// Nothing answered; `COLORFGBG` supplied the polarity.
    ColorFgBg,
    /// Nothing answered at all; these are nun's built-in neutrals.
    Builtin,
}

/// A sampled terminal palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Default background, from OSC 11.
    pub background: Rgb,
    /// Default foreground, from OSC 10.
    pub foreground: Rgb,
    /// Cursor colour, from OSC 12, when the terminal reported one.
    pub cursor: Option<Rgb>,
    /// The 16 palette slots, from OSC 4.
    pub palette: [Rgb; 16],
    /// How much of this was actually answered.
    pub source: Source,
}

impl Probe {
    /// One palette slot.
    #[must_use]
    pub fn ansi(&self, slot: Ansi) -> Rgb {
        self.palette[slot as usize]
    }

    /// Neutral built-ins for a dark terminal, used when nothing answers.
    #[must_use]
    pub const fn builtin_dark() -> Self {
        Self {
            background: Rgb::new(0x12, 0x14, 0x18),
            foreground: Rgb::new(0xd0, 0xd6, 0xdc),
            cursor: None,
            palette: DARK_PALETTE,
            source: Source::Builtin,
        }
    }

    /// Neutral built-ins for a light terminal.
    #[must_use]
    pub const fn builtin_light() -> Self {
        Self {
            background: Rgb::new(0xfa, 0xfa, 0xf8),
            foreground: Rgb::new(0x1c, 0x20, 0x24),
            cursor: None,
            palette: LIGHT_PALETTE,
            source: Source::Builtin,
        }
    }

    /// Read `COLORFGBG`, the one hint available when nothing answers a query.
    ///
    /// The value is `fg;bg` in palette indices, sometimes with a third field.
    /// It carries no actual colours — only enough to choose a polarity — so the
    /// result is a built-in palette, not a sampled one.
    #[must_use]
    pub fn from_color_fgbg(value: &str) -> Option<Self> {
        let background = value.split(';').next_back()?.trim().parse::<u8>().ok()?;
        // Slots 0-6 and 8 are the dark half of the palette; 7 and 9-15 the light.
        let dark = matches!(background, 0..=6 | 8);
        let mut probe = if dark { Self::builtin_dark() } else { Self::builtin_light() };
        probe.source = Source::ColorFgBg;
        Some(probe)
    }
}

const DARK_PALETTE: [Rgb; 16] = [
    Rgb::new(0x22, 0x27, 0x2b),
    Rgb::new(0xc5, 0x5b, 0x54),
    Rgb::new(0x77, 0xb8, 0x7f),
    Rgb::new(0xd6, 0xa7, 0x5b),
    Rgb::new(0x6d, 0x9e, 0xc9),
    Rgb::new(0xb0, 0x84, 0xc4),
    Rgb::new(0x5f, 0xb3, 0xb8),
    Rgb::new(0xc8, 0xcf, 0xd5),
    Rgb::new(0x4a, 0x54, 0x5c),
    Rgb::new(0xe0, 0x79, 0x6f),
    Rgb::new(0x93, 0xd1, 0x9a),
    Rgb::new(0xed, 0xc0, 0x76),
    Rgb::new(0x8c, 0xb9, 0xe0),
    Rgb::new(0xc8, 0x9e, 0xda),
    Rgb::new(0x7a, 0xce, 0xd3),
    Rgb::new(0xea, 0xf0, 0xf5),
];

const LIGHT_PALETTE: [Rgb; 16] = [
    Rgb::new(0x2b, 0x2f, 0x33),
    Rgb::new(0xb2, 0x3c, 0x31),
    Rgb::new(0x35, 0x74, 0x43),
    Rgb::new(0x8a, 0x61, 0x08),
    Rgb::new(0x2c, 0x5f, 0x94),
    Rgb::new(0x7c, 0x4a, 0x96),
    Rgb::new(0x20, 0x6c, 0x71),
    Rgb::new(0xe8, 0xea, 0xec),
    Rgb::new(0x6b, 0x74, 0x7c),
    Rgb::new(0x94, 0x2f, 0x25),
    Rgb::new(0x27, 0x5e, 0x35),
    Rgb::new(0x70, 0x4e, 0x05),
    Rgb::new(0x1f, 0x4c, 0x78),
    Rgb::new(0x63, 0x39, 0x79),
    Rgb::new(0x17, 0x56, 0x5a),
    Rgb::new(0xfb, 0xfb, 0xfa),
];

/// Which question a reply is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Foreground,
    Background,
    Cursor,
    Palette(u8),
}

/// A probe in flight.
#[derive(Debug)]
pub struct ProbeSession {
    parser: OscParser,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
    palette: [Option<Rgb>; 16],
}

impl Default for ProbeSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeSession {
    /// Start a probe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: OscParser::new(),
            foreground: None,
            background: None,
            cursor: None,
            palette: [None; 16],
        }
    }

    /// The bytes to write to the terminal to ask every question.
    ///
    /// BEL terminates rather than ST because a few terminals answer only the
    /// BEL form, and every terminal that accepts ST also accepts BEL.
    #[must_use]
    pub fn request(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(256);
        out.push_str("\x1b]10;?\x07");
        out.push_str("\x1b]11;?\x07");
        out.push_str("\x1b]12;?\x07");
        for slot in 0..16 {
            let _ = write!(out, "\x1b]4;{slot};?\x07");
        }
        out
    }

    /// Feed bytes read from the terminal.
    ///
    /// Returns everything that was **not** part of a colour reply, in order, so
    /// the caller can pass it to the input layer. A reply split across two
    /// reads is handled: parser state persists between calls.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut passthrough = Vec::new();
        for &byte in bytes {
            if let Some(payload) = self.parser.push(byte, &mut passthrough) {
                self.absorb(&payload);
            }
        }
        passthrough
    }

    /// Record one parsed OSC payload if it is a colour reply we asked for.
    fn absorb(&mut self, payload: &str) {
        let Some((slot, spec)) = split_reply(payload) else { return };
        let Some(rgb) = parse_color_spec(spec) else { return };
        match slot {
            Slot::Foreground => self.foreground = Some(rgb),
            Slot::Background => self.background = Some(rgb),
            Slot::Cursor => self.cursor = Some(rgb),
            Slot::Palette(index) => self.palette[index as usize] = Some(rgb),
        }
    }

    /// How many of the nineteen questions have been answered.
    #[must_use]
    pub fn answered(&self) -> usize {
        usize::from(self.foreground.is_some())
            + usize::from(self.background.is_some())
            + usize::from(self.cursor.is_some())
            + self.palette.iter().filter(|slot| slot.is_some()).count()
    }

    /// Whether everything that matters has been answered.
    ///
    /// The cursor colour is excluded: plenty of terminals answer every other
    /// query and stay silent on OSC 12, and waiting for it would spend the whole
    /// timeout on every start-up.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.foreground.is_some()
            && self.background.is_some()
            && self.palette.iter().all(Option::is_some)
    }

    /// Whether the terminal answered anything at all.
    #[must_use]
    pub fn answered_anything(&self) -> bool {
        self.answered() > 0
    }

    /// Build the probe, filling anything unanswered from `fallback`.
    ///
    /// A terminal that answers the background but not the palette still gets a
    /// theme built around its real background, which is the colour that matters
    /// most.
    #[must_use]
    pub fn finish(self, fallback: &Probe) -> Probe {
        let answered_anything = self.answered() > 0;
        let mut palette = fallback.palette;
        for (slot, answer) in palette.iter_mut().zip(self.palette) {
            if let Some(rgb) = answer {
                *slot = rgb;
            }
        }
        Probe {
            background: self.background.unwrap_or(fallback.background),
            foreground: self.foreground.unwrap_or(fallback.foreground),
            cursor: self.cursor.or(fallback.cursor),
            palette,
            source: if answered_anything { Source::Terminal } else { fallback.source },
        }
    }
}

/// Split `4;3;rgb:...` or `11;rgb:...` into the slot and the colour spec.
fn split_reply(payload: &str) -> Option<(Slot, &str)> {
    let (head, rest) = payload.split_once(';')?;
    match head {
        "10" => Some((Slot::Foreground, rest)),
        "11" => Some((Slot::Background, rest)),
        "12" => Some((Slot::Cursor, rest)),
        "4" => {
            let (index, spec) = rest.split_once(';')?;
            let index: u8 = index.trim().parse().ok()?;
            (index < 16).then_some((Slot::Palette(index), spec))
        }
        _ => None,
    }
}

/// Parse an `XParseColor` specification.
///
/// Terminals answer in whichever form they like: `rgb:` with one to four hex
/// digits per channel, or `#` with three, six or twelve. Components are scaled
/// by digit width, so `rgb:f/0/0` and `rgb:ffff/0000/0000` are the same red —
/// truncating to the first two digits would turn the former into near-black.
fn parse_color_spec(spec: &str) -> Option<Rgb> {
    let spec = spec.trim().trim_end_matches('\u{0}');

    if let Some(body) = spec.strip_prefix("rgb:").or_else(|| spec.strip_prefix("rgba:")) {
        let mut parts = body.split('/');
        let r = scale_component(parts.next()?)?;
        let g = scale_component(parts.next()?)?;
        let b = scale_component(parts.next()?)?;
        // An `rgba:` reply carries alpha last; nun composites onto an opaque
        // terminal background, so it is read and dropped rather than refused.
        return Some(Rgb::new(r, g, b));
    }

    if let Some(body) = spec.strip_prefix('#') {
        if !body.bytes().all(|b| b.is_ascii_hexdigit()) || body.len() % 3 != 0 {
            return None;
        }
        let width = body.len() / 3;
        if !(1..=4).contains(&width) {
            return None;
        }
        return Some(Rgb::new(
            scale_component(&body[0..width])?,
            scale_component(&body[width..width * 2])?,
            scale_component(&body[width * 2..width * 3])?,
        ));
    }

    None
}

/// Scale a 1-to-4 hex-digit component to 8 bits.
fn scale_component(text: &str) -> Option<u8> {
    let text = text.trim();
    if text.is_empty() || text.len() > 4 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(text, 16).ok()?;
    // Guarded to 1..=4 above, so this cannot fail or overflow the shift.
    let digits = u32::try_from(text.len()).ok()?;
    let max = (1u32 << (4 * digits)) - 1;
    // Round rather than truncate so `rgb:ffff/.../...` comes back as 255.
    u8::try_from((value * 255 + max / 2) / max).ok()
}

/// Pulls OSC sequences out of a byte stream, passing everything else through.
#[derive(Debug)]
struct OscParser {
    state: State,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Osc,
    OscEscape,
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

impl OscParser {
    const fn new() -> Self {
        Self { state: State::Ground, payload: Vec::new() }
    }

    /// Consume one byte, appending anything that is not part of an OSC sequence
    /// to `passthrough`. Returns a completed OSC payload when one ends here.
    fn push(&mut self, byte: u8, passthrough: &mut Vec<u8>) -> Option<String> {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                } else {
                    passthrough.push(byte);
                }
                None
            }
            State::Escape => {
                if byte == b']' {
                    self.state = State::Osc;
                    self.payload.clear();
                } else {
                    // Not an OSC introducer, so the escape belonged to the
                    // input stream after all. Hand both bytes back intact.
                    passthrough.push(ESC);
                    if byte == ESC {
                        self.state = State::Escape;
                    } else {
                        passthrough.push(byte);
                        self.state = State::Ground;
                    }
                }
                None
            }
            State::Osc => match byte {
                BEL => {
                    self.state = State::Ground;
                    Some(self.take_payload())
                }
                ESC => {
                    self.state = State::OscEscape;
                    None
                }
                // A terminal that cancels mid-sequence leaves nothing useful.
                CAN | SUB => {
                    self.state = State::Ground;
                    self.payload.clear();
                    None
                }
                _ => {
                    // Bound the buffer so a terminal that opens an OSC and
                    // never terminates it cannot grow memory without limit.
                    if self.payload.len() < 1024 {
                        self.payload.push(byte);
                    }
                    None
                }
            },
            State::OscEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                    Some(self.take_payload())
                } else {
                    // A bare escape inside the payload; keep both and continue.
                    if self.payload.len() < 1024 {
                        self.payload.push(ESC);
                        self.payload.push(byte);
                    }
                    self.state = State::Osc;
                    None
                }
            }
        }
    }

    fn take_payload(&mut self) -> String {
        let payload = String::from_utf8_lossy(&self.payload).into_owned();
        self.payload.clear();
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(session: &mut ProbeSession, input: &str) -> String {
        String::from_utf8(session.feed(input.as_bytes())).unwrap()
    }

    #[test]
    fn parses_the_sixteen_bit_reply_form() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:1010/1414/1818\x07");
        let probe = s.finish(&Probe::builtin_dark());
        assert_eq!(probe.background, Rgb::new(0x10, 0x14, 0x18));
    }

    #[test]
    fn parses_the_eight_bit_reply_form() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:10/14/18\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x10, 0x14, 0x18));
    }

    #[test]
    fn parses_the_hash_reply_form() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]10;#e0a44b\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).foreground, Rgb::new(0xe0, 0xa4, 0x4b));
    }

    #[test]
    fn scales_narrow_components_rather_than_truncating() {
        // `rgb:f/0/0` is full red. Reading only the first two digits of each
        // component would make it near-black.
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:f/0/0\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(255, 0, 0));

        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:ffff/ffff/ffff\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(255, 255, 255));
    }

    #[test]
    fn accepts_the_string_terminator_as_well_as_bel() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:2020/2020/2020\x1b\\");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x20, 0x20, 0x20));
    }

    #[test]
    fn reads_palette_slots() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]4;4;rgb:6d6d/9e9e/c9c9\x07");
        let probe = s.finish(&Probe::builtin_dark());
        assert_eq!(probe.ansi(Ansi::Blue), Rgb::new(0x6d, 0x9e, 0xc9));
    }

    #[test]
    fn an_rgba_reply_drops_the_alpha_rather_than_being_refused() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgba:1010/1414/1818/ffff\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x10, 0x14, 0x18));
    }

    // ── the part that actually breaks in the wild ───────────────────────────

    #[test]
    fn key_input_around_a_reply_is_passed_through_intact() {
        let mut s = ProbeSession::new();
        let passthrough = feed_all(&mut s, "ab\x1b]11;rgb:1010/1414/1818\x07cd");
        assert_eq!(passthrough, "abcd", "typing either side of a reply survives");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x10, 0x14, 0x18));
    }

    #[test]
    fn a_reply_split_across_reads_is_reassembled() {
        let mut s = ProbeSession::new();
        for chunk in ["\x1b]11;rgb:10", "10/1414/18", "18\x07"] {
            assert!(feed_all(&mut s, chunk).is_empty());
        }
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x10, 0x14, 0x18));
    }

    #[test]
    fn a_reply_split_immediately_after_the_escape_is_reassembled() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b");
        feed_all(&mut s, "]11;rgb:1010/1414/1818\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x10, 0x14, 0x18));
    }

    #[test]
    fn an_escape_that_is_not_an_osc_is_handed_back_whole() {
        let mut s = ProbeSession::new();
        // A real arrow key: ESC [ A. Eating the escape would lose the keypress.
        assert_eq!(feed_all(&mut s, "\x1b[A"), "\x1b[A");
    }

    #[test]
    fn two_escapes_in_a_row_both_survive() {
        let mut s = ProbeSession::new();
        assert_eq!(feed_all(&mut s, "\x1b\x1b[A"), "\x1b\x1b[A");
    }

    #[test]
    fn a_cancelled_sequence_does_not_poison_what_follows() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:10\x18");
        feed_all(&mut s, "\x1b]11;rgb:2020/2020/2020\x07");
        assert_eq!(s.finish(&Probe::builtin_dark()).background, Rgb::new(0x20, 0x20, 0x20));
    }

    #[test]
    fn an_unterminated_sequence_cannot_grow_memory_without_limit() {
        let mut s = ProbeSession::new();
        let flood = format!("\x1b]11;{}", "a".repeat(100_000));
        feed_all(&mut s, &flood);
        // Still usable afterwards, and nothing was recorded.
        assert_eq!(s.answered(), 0);
    }

    #[test]
    fn a_malformed_colour_is_ignored_rather_than_guessed_at() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:zz/zz/zz\x07");
        feed_all(&mut s, "\x1b]11;not-a-colour\x07");
        assert_eq!(s.answered(), 0);
        assert_eq!(s.finish(&Probe::builtin_dark()).source, Source::Builtin);
    }

    #[test]
    fn an_unrelated_osc_is_consumed_without_being_mistaken_for_an_answer() {
        let mut s = ProbeSession::new();
        // A window title report, which shares the OSC introducer.
        let passthrough = feed_all(&mut s, "\x1b]0;some title\x07");
        assert!(passthrough.is_empty());
        assert_eq!(s.answered(), 0);
    }

    // ── completion and fallback ─────────────────────────────────────────────

    #[test]
    fn completion_does_not_wait_on_the_cursor_colour() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]10;rgb:d0d0/d6d6/dcdc\x07");
        feed_all(&mut s, "\x1b]11;rgb:1212/1414/1818\x07");
        for slot in 0..16 {
            feed_all(&mut s, &format!("\x1b]4;{slot};rgb:1111/2222/3333\x07"));
        }
        assert!(s.is_complete(), "silence on OSC 12 must not stall start-up");
    }

    #[test]
    fn a_partial_answer_still_uses_the_real_background() {
        let mut s = ProbeSession::new();
        feed_all(&mut s, "\x1b]11;rgb:2a2a/1f1f/3c3c\x07");
        let probe = s.finish(&Probe::builtin_dark());
        assert_eq!(probe.background, Rgb::new(0x2a, 0x1f, 0x3c), "the answer wins");
        assert_eq!(probe.foreground, Probe::builtin_dark().foreground, "the rest falls back");
        assert_eq!(probe.source, Source::Terminal);
    }

    #[test]
    fn no_answer_at_all_falls_back_wholesale() {
        let s = ProbeSession::new();
        let probe = s.finish(&Probe::builtin_light());
        assert_eq!(probe, Probe::builtin_light());
    }

    #[test]
    fn color_fgbg_supplies_a_polarity_when_nothing_answers() {
        assert_eq!(
            Probe::from_color_fgbg("15;0").unwrap().background,
            Probe::builtin_dark().background
        );
        assert_eq!(
            Probe::from_color_fgbg("0;15").unwrap().background,
            Probe::builtin_light().background
        );
        assert_eq!(Probe::from_color_fgbg("15;default;0").unwrap().source, Source::ColorFgBg);
        assert!(Probe::from_color_fgbg("nonsense").is_none());
    }

    #[test]
    fn the_request_asks_every_question_once() {
        let request = ProbeSession::new().request();
        assert_eq!(request.matches("\x1b]4;").count(), 16);
        assert!(request.contains("\x1b]10;?"));
        assert!(request.contains("\x1b]11;?"));
        assert!(request.contains("\x1b]12;?"));
    }
}
