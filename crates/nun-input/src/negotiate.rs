//! Asking the terminal whether it speaks the Kitty keyboard protocol.
//!
//! Without the protocol a terminal cannot report Cmd, or tell `Ctrl+I` from
//! Tab, `Ctrl+M` from Enter, or `Ctrl+Shift+Z` from `Ctrl+Z`. Whether it is
//! there decides which key set nun uses, so it is asked, never guessed from
//! `$TERM`.
//!
//! The question is `CSI ? u`: a terminal with the protocol answers with its
//! current flags, `CSI ? <flags> u`. A terminal without it says nothing — and
//! silence is ambiguous, because a slow terminal is silent too. So it is
//! followed by a Primary Device Attributes query, `CSI c`, which every terminal
//! answers. Replies come back in the order asked, so once the attributes reply
//! arrives, a missing keyboard reply means "no", not "not yet".
//!
//! The attributes reply is kept, too, because it says more than that it
//! arrived: its parameters list what the terminal can do, and parameter 52
//! is the one honest way a terminal says it accepts a copy through OSC 52.

/// The bytes to write: the keyboard query, then the sentinel.
pub const KEYBOARD_QUERY: &str = "\x1b[?u\x1b[c";

/// What the terminal has said so far.
#[derive(Debug, Clone, Default)]
pub struct KeyboardProbe {
    state: State,
    params: Vec<u8>,
    flags: Option<u16>,
    attributes_seen: bool,
    attributes: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    /// Inside `CSI ?`, collecting parameters.
    Private,
}

impl KeyboardProbe {
    /// A probe with nothing heard yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes from the terminal. Returns everything that was not part of
    /// a reply to these two queries, in order, for the input layer.
    ///
    /// Replies split across reads are handled; parser state persists between
    /// calls.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut passthrough = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Ground if byte == 0x1b => self.state = State::Escape,
                State::Ground => passthrough.push(byte),
                State::Escape if byte == b'[' => self.state = State::Private,
                State::Escape => {
                    self.state = State::Ground;
                    passthrough.extend_from_slice(&[0x1b, byte]);
                }
                State::Private => self.private(byte, &mut passthrough),
            }
        }
        passthrough
    }

    fn private(&mut self, byte: u8, passthrough: &mut Vec<u8>) {
        let is_param = byte.is_ascii_digit() || byte == b';';
        let first = self.params.is_empty();
        match byte {
            b'?' if first => self.params.push(b'?'),
            _ if is_param && self.params.first() == Some(&b'?') => self.params.push(byte),
            b'u' if self.params.first() == Some(&b'?') => {
                let flags =
                    std::str::from_utf8(&self.params[1..]).ok().and_then(|s| s.parse().ok());
                self.flags = Some(flags.unwrap_or(0));
                self.reset();
            }
            b'c' if self.params.first() == Some(&b'?') => {
                self.attributes_seen = true;
                // `?62;22;52c`, and kitty's `?62;52;c` with an empty last
                // field: the empty ones are dropped.
                self.attributes = std::str::from_utf8(&self.params[1..])
                    .unwrap_or_default()
                    .split(';')
                    .filter_map(|param| param.parse().ok())
                    .collect();
                self.reset();
            }
            _ => {
                // Some other CSI sequence — a key press, most likely. Hand it
                // back untouched.
                passthrough.extend_from_slice(b"\x1b[");
                passthrough.extend_from_slice(&self.params);
                passthrough.push(byte);
                self.reset();
            }
        }
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.params.clear();
    }

    /// Whether the sentinel has arrived, so every reply to anything asked
    /// before it is in, and nothing more will come.
    ///
    /// Stopping at the keyboard reply alone would leave the attributes reply
    /// still in flight, to be read later as if it were a keystroke.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.attributes_seen
    }

    /// The parameters of the terminal's device-attributes reply, in order:
    /// its conformance level first, then what it can do. Empty until the
    /// reply arrives.
    #[must_use]
    pub fn attributes(&self) -> &[u16] {
        &self.attributes
    }

    /// Whether the terminal has the protocol, as far as is known.
    ///
    /// `None` until one of the two replies has arrived.
    #[must_use]
    pub const fn supported(&self) -> Option<bool> {
        if self.flags.is_some() {
            Some(true)
        } else if self.attributes_seen {
            Some(false)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flags_reply_means_the_protocol_is_there() {
        let mut probe = KeyboardProbe::new();
        assert!(probe.feed(b"\x1b[?0u\x1b[?62;22c").is_empty());
        assert_eq!(probe.supported(), Some(true));
    }

    #[test]
    fn attributes_with_no_flags_before_them_means_it_is_not() {
        let mut probe = KeyboardProbe::new();
        probe.feed(b"\x1b[?1;2c");
        assert_eq!(probe.supported(), Some(false));
    }

    #[test]
    fn the_probe_is_not_over_until_the_sentinel_arrives() {
        let mut probe = KeyboardProbe::new();
        probe.feed(b"\x1b[?0u");
        assert_eq!(probe.supported(), Some(true));
        assert!(!probe.is_complete(), "the attributes reply is still on its way");
        probe.feed(b"\x1b[?62c");
        assert!(probe.is_complete());
    }

    #[test]
    fn silence_is_not_an_answer() {
        let probe = KeyboardProbe::new();
        assert_eq!(probe.supported(), None);
        assert!(!probe.is_complete());
    }

    #[test]
    fn a_reply_split_across_reads_is_still_understood() {
        let mut probe = KeyboardProbe::new();
        probe.feed(b"\x1b[?");
        probe.feed(b"1");
        probe.feed(b"5u");
        assert_eq!(probe.supported(), Some(true));
    }

    #[test]
    fn keystrokes_typed_during_the_probe_are_handed_back_in_order() {
        let mut probe = KeyboardProbe::new();
        let rest = probe.feed(b"a\x1b[A\x1b[?0ub\x1b[?62c");
        assert_eq!(rest, b"a\x1b[Ab");
        assert_eq!(probe.supported(), Some(true));
    }

    #[test]
    fn the_attributes_are_kept_for_what_they_say() {
        let mut probe = KeyboardProbe::new();
        probe.feed(b"\x1b[?62;22;52c");
        assert_eq!(probe.attributes(), [62, 22, 52]);
    }

    #[test]
    fn an_empty_last_attribute_is_dropped() {
        // kitty 0.43 ends its list with a separator.
        let mut probe = KeyboardProbe::new();
        probe.feed(b"\x1b[?62;52;c");
        assert_eq!(probe.attributes(), [62, 52]);
    }

    #[test]
    fn a_lone_escape_is_passed_through() {
        let mut probe = KeyboardProbe::new();
        assert_eq!(probe.feed(b"\x1bx"), b"\x1bx");
    }
}
