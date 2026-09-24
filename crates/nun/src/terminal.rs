//! The terminal side of the start-up probe.
//!
//! `nun-theme` and `nun-input` are deliberately free of I/O: they say what to
//! write and parse what comes back. This is the other half — raw mode, one
//! write carrying every question, a bounded wait, and putting the terminal
//! back afterwards.

use std::io::Write as _;
use std::time::{Duration, Instant};

use nun_input::{KEYBOARD_QUERY, KeyboardProbe};
use nun_theme::{Probe, ProbeSession};
use nun_ui::{
    Capabilities, CrosstermControl, Evidence, TerminalGuard, UNDERLINE_QUERY, UnderlineProbe,
};

/// Asks how big a cell is in pixels. A terminal that knows answers
/// `CSI 6 ; height ; width t`; the rest ignore it. Safe to ask of any: 16
/// is one of the window operations that only reports.
const CELL_QUERY: &str = "\x1b[16t";

/// How long to wait for a terminal that may never answer.
///
/// Long enough for a local terminal to reply and short enough that a terminal
/// which ignores the queries does not visibly delay start-up. Most never wait
/// this long: the device-attributes reply closes the probe as soon as it lands.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(120);

/// What the terminal said about itself.
#[derive(Debug, Clone)]
pub struct Startup {
    /// Its palette, or the fallback where it would not say.
    pub palette: Probe,
    /// Whether it speaks the Kitty keyboard protocol: `Some(false)` when it
    /// said no, `None` when it did not answer in time. Either way nun cannot
    /// rely on keys only the protocol reports, but they are different things to
    /// tell the user.
    pub kitty_keyboard: Option<bool>,
    /// What it said about underlines, and its name if it gave one.
    pub underlines: UnderlineProbe,
    /// The parameters of its device-attributes reply: what it says it can
    /// do. Empty when it did not answer.
    pub attributes: Vec<u16>,
    /// A cell's width and height in pixels, when it said.
    pub cell: Option<(u16, u16)>,
    /// Whether `COLORTERM` says 24-bit colour: what a terminal that cannot
    /// be asked sets to say it has it.
    pub colorterm: bool,
}

impl Startup {
    /// Whether it said it accepts a copy through OSC 52, with parameter 52
    /// in its device attributes. Nothing else it can say means that: its
    /// terminfo `Ms` says only that it knows the sequence, not that it lets
    /// a program use it.
    #[must_use]
    pub fn takes_osc52(&self) -> bool {
        // The first is the conformance level, not a feature.
        self.attributes.get(1..).is_some_and(|features| features.contains(&52))
    }

    /// Whether it is tmux, by the name it gave.
    fn is_tmux(&self) -> bool {
        self.underlines.version().is_some_and(|name| name.starts_with("tmux"))
    }

    /// Whether it draws 24-bit colour, and how that was found out.
    ///
    /// What it said is believed first. Where it said nothing, tmux is taken
    /// at its word — it converts exact colours for whatever terminal is
    /// outside it — and then `COLORTERM`, the variable a terminal sets
    /// because it cannot be asked (Alacritty, Terminal.app on macOS 26).
    /// With neither, 256 colours: an approximation is drawn right
    /// everywhere, and an exact colour a terminal cannot draw is misread as
    /// other attributes altogether.
    #[must_use]
    pub fn truecolor(&self) -> (bool, &'static str) {
        match self.underlines.direct_colour() {
            Evidence::Echoed => (true, "the terminal kept an exact colour it was sent"),
            Evidence::Terminfo => (true, "the terminal's terminfo has RGB or Tc (XTGETTCAP)"),
            Evidence::Misread => {
                (false, "the terminal kept only an approximation of an exact colour it was sent")
            }
            Evidence::Absent => (false, "the terminal's terminfo has neither RGB nor Tc"),
            Evidence::Silent if self.is_tmux() => {
                (true, "tmux, which converts exact colours for the terminal outside it")
            }
            Evidence::Silent if self.colorterm => {
                (true, "the terminal could not say, and COLORTERM says it has it")
            }
            Evidence::Silent => (
                false,
                "the terminal could not say, and COLORTERM does not; \
                 set ui.truecolor = \"on\" if it has it",
            ),
        }
    }

    /// A cell's size in pixels, now: from the window size the terminal
    /// keeps, which follows a change of font, or else from what it said at
    /// startup. tmux makes up 16 by 32 when it does not know, and that is not
    /// taken for an answer.
    #[must_use]
    pub fn cell_pixels(&self) -> Option<(u16, u16)> {
        let real =
            |cell: &(u16, u16)| cell.0 > 0 && cell.1 > 0 && !(self.is_tmux() && *cell == (16, 32));
        window_cell().filter(real).or(self.cell.filter(real))
    }
}

/// A cell's size in pixels, from the window size the terminal nun is drawn
/// on keeps: zero where it does not fill the pixels in.
fn window_cell() -> Option<(u16, u16)> {
    let size = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    if size.ws_col == 0 || size.ws_row == 0 {
        return None;
    }
    Some((size.ws_xpixel / size.ws_col, size.ws_ypixel / size.ws_row))
}

/// A cell size in a reply to [`CELL_QUERY`], `CSI 6 ; height ; width t`,
/// among whatever else `bytes` holds.
/// The first that parses: a key typed meanwhile, such as Ctrl+PageDown's
/// `CSI 6 ; 5 ~`, starts the same way.
fn cell_reply(bytes: &[u8]) -> Option<(u16, u16)> {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices("\x1b[6;").find_map(|(at, found)| {
        let rest = &text[at + found.len()..];
        let (numbers, _) = rest.split_once('t')?;
        let (height, width) = numbers.split_once(';')?;
        Some((width.parse().ok()?, height.parse().ok()?))
    })
}

/// Ask the terminal for its palette and its keyboard protocol in one round
/// trip, falling back where it will not say.
///
/// Palette fallback order matches the design: what the terminal answered, then
/// `COLORFGBG` for the polarity alone, then nun's built-in neutrals.
///
/// This enters raw mode for the duration and restores it before returning,
/// including on the error paths.
pub fn probe(timeout: Duration) -> Startup {
    let fallback = fallback_probe();
    let nothing = Startup {
        palette: fallback.clone(),
        kitty_keyboard: None,
        underlines: UnderlineProbe::new(),
        attributes: Vec::new(),
        cell: None,
        colorterm: colorterm(),
    };

    // A probe needs a terminal on stdin to answer it, and one on stdout to
    // ask: piped input has no one to answer, and redirected output would
    // write the questions into a file.
    if !rustix::termios::isatty(std::io::stdin()) || !rustix::termios::isatty(std::io::stdout()) {
        return nothing;
    }
    // Without raw mode the reply is line-buffered and echoed, so it would both
    // arrive too late and be printed to the screen. The guard puts it back on
    // every path out of here, a panic in a reply parser included.
    let Ok(guard) = TerminalGuard::enter(CrosstermControl, Capabilities::none()) else {
        return nothing;
    };
    let startup = probe_in_raw_mode(timeout, &fallback);
    drop(guard);
    startup.unwrap_or(nothing)
}

fn probe_in_raw_mode(timeout: Duration, fallback: &Probe) -> Option<Startup> {
    let mut colours = ProbeSession::new();
    let mut underlines = UnderlineProbe::new();
    let mut keyboard = KeyboardProbe::new();

    // One write, in this order: the colour queries, the underline queries,
    // the keyboard query, and last the device-attributes sentinel. Replies
    // come back in the order asked, so the sentinel's reply means every other
    // reply is in.
    let mut stdout = std::io::stdout();
    let request = format!("{}{UNDERLINE_QUERY}{CELL_QUERY}{KEYBOARD_QUERY}", colours.request());
    stdout.write_all(request.as_bytes()).ok()?;
    stdout.flush().ok()?;

    let stdin = std::io::stdin();
    let deadline = Instant::now() + timeout;
    let mut buffer = [0u8; 1024];
    // What the other parsers left: the cell size's reply, and keystrokes.
    let mut left = Vec::new();

    while !keyboard.is_complete() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !readable(&stdin, remaining) {
            break;
        }
        // Read the descriptor directly. std's stdin buffers, and anything it
        // buffered past the replies would be invisible to the input reader
        // that takes over from here.
        let Ok(count) = rustix::io::read(&stdin, &mut buffer) else { break };
        if count == 0 {
            break;
        }
        // Anything that was not a reply is real input. The probe runs before
        // the editor's input reader starts, so a keystroke landing inside this
        // window is dropped: a real if small cost, and the window closes as
        // soon as the terminal has answered.
        // Each parser takes its own replies and hands the rest along: OSC
        // for the colours, DCS for the underlines, CSI for the keyboard.
        let rest = colours.feed(&buffer[..count]);
        let rest = underlines.feed(&rest);
        left.extend(keyboard.feed(&rest));
    }

    Some(Startup {
        palette: colours.finish(fallback),
        kitty_keyboard: keyboard.supported(),
        underlines,
        attributes: keyboard.attributes().to_vec(),
        cell: cell_reply(&left),
        colorterm: colorterm(),
    })
}

/// Wait up to `timeout` for `stdin` to have something to read.
fn readable(stdin: &std::io::Stdin, timeout: Duration) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let Ok(timeout) = Timespec::try_from(timeout) else { return false };
    let mut fds = [PollFd::new(stdin, PollFlags::IN)];
    matches!(poll(&mut fds, Some(&timeout)), Ok(n) if n > 0)
}

/// Whether `COLORTERM` says 24-bit colour.
fn colorterm() -> bool {
    std::env::var("COLORTERM").is_ok_and(|value| matches!(value.as_str(), "truecolor" | "24bit"))
}

/// What to use when the terminal says nothing.
fn fallback_probe() -> Probe {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| Probe::from_color_fgbg(&value))
        .unwrap_or_else(Probe::builtin_dark)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startup(reply: &[u8], colorterm: bool) -> Startup {
        let mut underlines = UnderlineProbe::new();
        let _ = underlines.feed(reply);
        Startup {
            palette: Probe::builtin_dark(),
            kitty_keyboard: None,
            underlines,
            attributes: Vec::new(),
            cell: None,
            colorterm,
        }
    }

    #[test]
    fn a_cell_size_reply_is_found_among_other_bytes() {
        assert_eq!(cell_reply(b"a\x1b[6;18;9tb"), Some((9, 18)));
        assert_eq!(cell_reply(b"\x1b[A"), None);
        assert_eq!(cell_reply(b"\x1b[6;x;9t"), None);
        assert_eq!(cell_reply(b"\x1b[6;5~\x1b[6;18;9t"), Some((9, 18)), "past a key");
    }

    #[test]
    fn what_the_terminal_says_about_colour_outranks_colorterm() {
        let approximated = b"\x1bP1$r0m\x1b\\\x1bP1$r0m\x1b\\\x1bP1$r0;38:5:16m\x1b\\";
        assert!(!startup(approximated, true).truecolor().0);
        let kept = b"\x1bP1$r0m\x1b\\\x1bP1$r0m\x1b\\\x1bP1$r0;38:2::1:2:3m\x1b\\";
        assert!(startup(kept, false).truecolor().0);
    }

    #[test]
    fn silence_is_256_colours_unless_tmux_or_colorterm_says_otherwise() {
        let (on, why) = startup(b"", false).truecolor();
        assert!(!on);
        assert!(why.contains("ui.truecolor"), "names the setting: {why}");
        assert!(startup(b"", true).truecolor().0);
        assert!(startup(b"\x1bP0$r\x1b\\\x1bP>|tmux 3.6a\x1b\\", false).truecolor().0);
    }

    #[test]
    fn tmux_making_up_a_cell_size_is_not_an_answer() {
        let mut tmux = startup(b"\x1bP>|tmux 3.6a\x1b\\", false);
        tmux.cell = Some((16, 32));
        // The window size of the test's own terminal, if it has one, may
        // answer instead: only the made-up size is under test.
        assert_ne!(tmux.cell_pixels(), Some((16, 32)));
        let mut kitty = startup(b"", false);
        kitty.cell = Some((16, 32));
        assert!(kitty.cell_pixels().is_some());
    }
}
