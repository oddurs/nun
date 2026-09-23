//! Asking the terminal what kind of underline it can draw.
//!
//! Diagnostics want a curly underline in the colour of their severity:
//! `CSI 4:3 m` for the curl, `CSI 58 … m` for the colour. Both are widely but
//! not universally understood, and getting it wrong is worse than not using
//! them: a terminal that misreads the colon form draws an underline *and*
//! italics, and one that does not know 58 reads its parameters as dim and
//! whatever else the colour's numbers happen to be. Tmux is the subtle case —
//! it keeps `4:3`, then drops the underline entirely when redrawing it to an
//! outer terminal it does not think has one.
//!
//! So both are asked, never assumed from `$TERM`, and there are two ways of
//! asking, because no one way reaches every terminal:
//!
//! * **DECRQSS** (`DCS $ q m ST`), twice: once after setting the curl, once
//!   after setting a colour, so a misreading of one cannot be blamed on the
//!   other. The terminal reports the SGR state it now holds. `4:3` in the
//!   first reply means it understood; `4` and `3` as separate parameters
//!   means it read underline plus italic, and the curl must never be sent. A
//!   bare `1`, `2`, `3` or `5` in the second means the colour's numbers
//!   leaked into other attributes. iTerm2, kitty, Ghostty and foot answer
//!   this way; a reported state is believed over terminfo.
//! * **XTGETTCAP** (`DCS + q … ST`) for `Smulx` and `Setulc`, the terminfo
//!   capabilities for the curl and the colour. `WezTerm` answers this and not
//!   the other; kitty, Ghostty and foot answer both.
//!
//! Tmux, Alacritty and Terminal.app cannot say either way, so there nun falls
//! back to a plain underline, with severity on the rail. That is the right
//! answer for tmux unless it has been told about the outer terminal, and
//! wrong for Alacritty, which draws both; a person who knows better says
//! `undercurl = "on"`.
//!
//! Like the other probes this is free of I/O: it says what to write and
//! parses what comes back. The replies arrive before the device-attributes
//! sentinel the keyboard probe waits for, so asking costs no extra wait.

/// The bytes to write, before the keyboard query and its sentinel.
///
/// Sets the curl and asks what the SGR state now is; sets a colour and asks
/// again; puts the state back; asks for the two terminfo capabilities; and
/// asks the terminal's name and version for `nun --capabilities`. Nothing is
/// printed while the attributes are set, so setting them has no visible
/// effect.
pub const UNDERLINE_QUERY: &str = concat!(
    "\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\",
    "\x1b[0m\x1b[58:2::1:2:3m\x1bP$qm\x1b\\",
    "\x1b[0m",
    "\x1bP+q536d756c78;536574756c63\x1b\\",
    "\x1b[>0q",
);

/// `Smulx`, hex-encoded as XTGETTCAP names it.
const SMULX: &str = "536d756c78";
/// `Setulc`, hex-encoded.
const SETULC: &str = "536574756c63";

/// The longest DCS payload kept. Replies are a few dozen bytes; the bound only
/// stops a terminal that never terminates one from growing memory.
const MOST_PAYLOAD: usize = 512;

/// What kind of underline to draw, and whether it may be coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Underlines {
    /// Draw a curly underline (`4:3`) where one is asked for.
    pub curly: bool,
    /// Colour underlines (`58`).
    pub colour: bool,
}

impl Underlines {
    /// A plain, uncoloured underline: what any terminal can draw.
    pub const PLAIN: Self = Self { curly: false, colour: false };
    /// Both the curl and the colour.
    pub const FULL: Self = Self { curly: true, colour: true };
}

/// How the terminal answered, so `nun --capabilities` can say why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// It echoed `4:3` back in its SGR state.
    Echoed,
    /// Its terminfo, over XTGETTCAP, has the capability.
    Terminfo,
    /// It read the colon form as separate attributes, so sending it would
    /// draw the wrong thing.
    Misread,
    /// It said it does not have it.
    Absent,
    /// It could not say.
    Silent,
}

impl Evidence {
    /// Whether this means the feature can be used.
    #[must_use]
    pub const fn supported(self) -> bool {
        matches!(self, Self::Echoed | Self::Terminfo)
    }

    /// A short explanation, for `nun --capabilities`.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Echoed => "the terminal reported it back in its SGR state",
            Self::Terminfo => "the terminal's terminfo has it (XTGETTCAP)",
            Self::Misread => "the terminal read the colon form as other attributes",
            Self::Absent => "the terminal said it does not have it",
            Self::Silent => {
                "the terminal could not say (tmux, Alacritty and Terminal.app cannot); \
                 set ui.undercurl = \"on\" if it has it"
            }
        }
    }
}

/// What the terminal has said so far.
#[derive(Debug, Clone, Default)]
pub struct UnderlineProbe {
    state: State,
    payload: Vec<u8>,
    /// Its replies to the two state requests, in order: the parameters, or
    /// `None` where it could not report its state.
    states: Vec<Option<Vec<String>>>,
    /// What its terminfo says of the curl and of the colour.
    smulx: Terminfo,
    setulc: Terminfo,
    /// Its name and version, from XTVERSION.
    version: Option<String>,
}

/// What terminfo said about one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Terminfo {
    #[default]
    Unasked,
    Has,
    Lacks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    /// Inside `ESC P`, collecting the payload.
    Dcs,
    /// An `ESC` inside the payload: the start of the terminator, usually.
    DcsEscape,
}

impl UnderlineProbe {
    /// A probe with nothing heard yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes from the terminal. Returns everything that was not a DCS
    /// reply, in order, for the next parser along.
    ///
    /// Replies split across reads are handled; parser state persists between
    /// calls.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut passthrough = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Ground if byte == 0x1b => self.state = State::Escape,
                State::Ground => passthrough.push(byte),
                State::Escape if byte == b'P' => {
                    self.state = State::Dcs;
                    self.payload.clear();
                }
                State::Escape if byte == 0x1b => passthrough.push(0x1b),
                State::Escape => {
                    self.state = State::Ground;
                    passthrough.extend_from_slice(&[0x1b, byte]);
                }
                State::Dcs if byte == 0x1b => self.state = State::DcsEscape,
                // No terminal here ends a reply with BEL, but it costs
                // nothing to accept one.
                State::Dcs if byte == 0x07 => {
                    self.state = State::Ground;
                    self.finish();
                }
                // A terminal that cancels mid-sequence leaves nothing useful.
                State::Dcs if byte == 0x18 || byte == 0x1a => self.state = State::Ground,
                State::Dcs => {
                    if self.payload.len() < MOST_PAYLOAD {
                        self.payload.push(byte);
                    }
                }
                State::DcsEscape => {
                    self.finish();
                    self.state = State::Ground;
                    // An unterminated reply cut off by the next one.
                    if byte == b'P' {
                        self.state = State::Dcs;
                    } else if byte != b'\\' {
                        // Not a terminator after all: the escape starts
                        // whatever comes next, which is not ours.
                        passthrough.extend_from_slice(&[0x1b, byte]);
                    }
                }
            }
        }
        passthrough
    }

    /// Take in the payload collected so far, as one complete reply.
    fn finish(&mut self) {
        let payload = std::mem::take(&mut self.payload);
        self.absorb(&String::from_utf8_lossy(&payload));
    }

    /// Take in one DCS payload.
    fn absorb(&mut self, payload: &str) {
        if let Some(rest) = payload.strip_prefix("1$r") {
            let params = rest.strip_suffix('m').unwrap_or(rest);
            self.states.push(Some(params.split(';').map(str::to_string).collect()));
        } else if payload.starts_with("0$r") {
            // Not "no": it cannot report its state, which says nothing about
            // what it can draw. WezTerm says this and has both.
            self.states.push(None);
        } else if let Some((found, rest)) = payload
            .strip_prefix("1+r")
            .map(|rest| (Terminfo::Has, rest))
            .or_else(|| payload.strip_prefix("0+r").map(|rest| (Terminfo::Lacks, rest)))
        {
            // One or more `name=value` pairs, or bare names, in either case
            // of hex.
            for pair in rest.split(';') {
                let name = pair.split('=').next().unwrap_or_default().to_ascii_lowercase();
                if name == SMULX {
                    self.smulx = found;
                } else if name == SETULC {
                    self.setulc = found;
                }
            }
        } else if let Some(name) = payload.strip_prefix(">|") {
            self.version = Some(name.chars().filter(|ch| !ch.is_control()).collect());
        }
    }

    /// Whether the curl can be drawn, and how that was found out.
    #[must_use]
    pub fn curly(&self) -> Evidence {
        if let Some(Some(params)) = self.states.first() {
            let has = |wanted: &str| params.iter().any(|param| param == wanted);
            if has("4:3") {
                return Evidence::Echoed;
            }
            // Read as `4` then `3`: underline, then italic.
            if has("4") && has("3") {
                return Evidence::Misread;
            }
            // It reported its state and the curl is not in it. Believed over
            // terminfo: sent `4:3`, this terminal would draw no line at all.
            return Evidence::Absent;
        }
        from_terminfo(self.smulx)
    }

    /// Whether underlines can be coloured, and how that was found out.
    #[must_use]
    pub fn colour(&self) -> Evidence {
        if let Some(Some(params)) = self.states.get(1) {
            if params.iter().any(|param| param == "58" || param.starts_with("58:")) {
                return Evidence::Echoed;
            }
            // The colour's numbers taken as attributes of their own: bold,
            // dim, italic, blink.
            if params.iter().any(|param| matches!(param.as_str(), "1" | "2" | "3" | "5")) {
                return Evidence::Misread;
            }
            // Otherwise it may simply leave the colour out of its report, as
            // Ghostty does; terminfo decides.
        }
        from_terminfo(self.setulc)
    }

    /// What to draw, from what was said. A terminal that misread the curl
    /// gets neither: something that confused about `4:3` is not trusted with
    /// the colon form of 58 either.
    #[must_use]
    pub fn underlines(&self) -> Underlines {
        let curly = self.curly();
        Underlines {
            curly: curly.supported(),
            colour: curly != Evidence::Misread && self.colour().supported(),
        }
    }

    /// The terminal's name and version, when it said.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

const fn from_terminfo(answer: Terminfo) -> Evidence {
    match answer {
        Terminfo::Has => Evidence::Terminfo,
        Terminfo::Lacks => Evidence::Absent,
        Terminfo::Unasked => Evidence::Silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(reply: &[u8]) -> UnderlineProbe {
        let mut probe = UnderlineProbe::new();
        assert!(probe.feed(reply).is_empty(), "every byte was a reply");
        probe
    }

    // The replies below are what each terminal sends to this query, from
    // its source.

    #[test]
    fn kitty_echoes_both_and_its_terminfo_agrees() {
        let probe = probe(
            b"\x1bP1$r0;4:3m\x1b\\\x1bP1$r0;58:2:1:2:3m\x1b\\\
              \x1bP1+r536d756c78=1b5b343a25703125646d\x1b\\\x1bP1+r536574756c63=78\x1b\\",
        );
        assert_eq!((probe.curly(), probe.colour()), (Evidence::Echoed, Evidence::Echoed));
        assert_eq!(probe.underlines(), Underlines::FULL);
    }

    #[test]
    fn iterm2_echoes_both_and_its_terminfo_is_overruled() {
        let probe = probe(
            b"\x1bP1$r4:3m\x1b\\\x1bP1$r58:2:1:2:3m\x1b\\\x1bP0+r536d756c78;536574756c63\x1b\\",
        );
        assert_eq!(probe.underlines(), Underlines::FULL);
    }

    #[test]
    fn ghostty_leaves_the_colour_out_of_its_state_and_terminfo_supplies_it() {
        let probe = probe(
            b"\x1bP1$r0;4:3m\x1b\\\x1bP1$r0m\x1b\\\
              \x1bP1+r536D756C78=1B5B343A25703125646D\x1b\\\x1bP1+r536574756C63=78\x1b\\",
        );
        assert_eq!((probe.curly(), probe.colour()), (Evidence::Echoed, Evidence::Terminfo));
        assert_eq!(probe.underlines(), Underlines::FULL);
    }

    #[test]
    fn wezterm_cannot_report_its_state_and_answers_through_terminfo() {
        let probe = probe(
            b"\x1bP0$r\x1b\\\x1bP0$r\x1b\\\
              \x1bP1+r536D756C78=1B5B343A25703125646D\x1b\\\x1bP1+r536574756C63=78\x1b\\",
        );
        assert_eq!((probe.curly(), probe.colour()), (Evidence::Terminfo, Evidence::Terminfo));
        assert_eq!(probe.underlines(), Underlines::FULL);
    }

    #[test]
    fn a_terminal_that_splits_the_curl_into_underline_and_italic_gets_a_plain_line() {
        let probe = probe(b"\x1bP1$r0;4;3m\x1b\\\x1bP1$r0;58;2;1;2;3m\x1b\\");
        assert_eq!(probe.curly(), Evidence::Misread);
        assert_eq!(probe.underlines(), Underlines::PLAIN);
    }

    #[test]
    fn a_colour_whose_numbers_leak_into_other_attributes_is_never_sent() {
        let probe =
            probe(b"\x1bP1$r0;4:3m\x1b\\\x1bP1$r0;1;2;3m\x1b\\\x1bP1+r536574756c63=78\x1b\\");
        assert_eq!(probe.colour(), Evidence::Misread, "the leak outranks terminfo");
        assert_eq!(probe.underlines(), Underlines { curly: true, colour: false });
    }

    #[test]
    fn a_reported_state_with_no_curl_outranks_terminfo() {
        let probe = probe(b"\x1bP1$r0m\x1b\\\x1bP1$r0m\x1b\\\x1bP1+r536d756c78=00\x1b\\");
        assert_eq!(probe.curly(), Evidence::Absent);
        assert!(!probe.underlines().curly);
    }

    #[test]
    fn tmux_cannot_say_and_nothing_is_risked() {
        // tmux 3.6 and later answers DECRQSS with 0$r; earlier, nothing.
        let probe = probe(b"\x1bP0$r\x1b\\\x1bP0$r\x1b\\\x1bP>|tmux 3.6a\x1b\\");
        assert_eq!(probe.curly(), Evidence::Silent, "cannot say is not no");
        assert_eq!(probe.underlines(), Underlines::PLAIN);
    }

    #[test]
    fn silence_is_a_plain_underline_and_is_said_to_be_silence() {
        let probe = UnderlineProbe::new();
        assert_eq!(probe.curly(), Evidence::Silent);
        assert_eq!(probe.underlines(), Underlines::PLAIN);
    }

    #[test]
    fn a_reply_cut_off_by_the_next_is_not_taken_for_a_keystroke() {
        let mut probe = UnderlineProbe::new();
        assert!(probe.feed(b"\x1bP1$r0;4:3m\x1bP>|foot(1.20)\x1b\\").is_empty());
        assert_eq!(probe.curly(), Evidence::Echoed);
        assert_eq!(probe.version(), Some("foot(1.20)"));
    }

    #[test]
    fn the_version_reply_is_kept_for_the_report() {
        let probe = probe(b"\x1bP>|kitty(0.39.1)\x1b\\");
        assert_eq!(probe.version(), Some("kitty(0.39.1)"));
    }

    #[test]
    fn replies_split_across_reads_are_still_understood() {
        let mut probe = UnderlineProbe::new();
        for piece in [&b"\x1bP1$"[..], b"r0;4:", b"3m\x1b", b"\\"] {
            assert!(probe.feed(piece).is_empty());
        }
        assert_eq!(probe.curly(), Evidence::Echoed);
    }

    #[test]
    fn everything_that_is_not_a_dcs_reply_is_handed_on_in_order() {
        let mut probe = UnderlineProbe::new();
        let rest = probe.feed(b"a\x1b[?0u\x1bP0$r\x1b\\b\x1b\x1b[c");
        assert_eq!(rest, b"a\x1b[?0ub\x1b\x1b[c");
    }

    #[test]
    fn a_terminal_that_never_ends_a_reply_cannot_grow_memory() {
        let mut probe = UnderlineProbe::new();
        probe.feed(b"\x1bP");
        probe.feed(&[b'x'; 10_000]);
        assert!(probe.payload.len() <= MOST_PAYLOAD);
    }

    #[test]
    fn the_query_restores_the_attributes_it_sets() {
        let reset = UNDERLINE_QUERY.rfind("\x1b[0m").unwrap();
        let curl = UNDERLINE_QUERY.find("\x1b[4:3m").unwrap();
        assert!(curl < reset, "the curl is set, then put back");
        assert!(!UNDERLINE_QUERY.contains("\x1b[K"), "nothing on the user's screen is erased");
    }
}
