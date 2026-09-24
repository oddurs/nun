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
use nun_ui::{Capabilities, CrosstermControl, TerminalGuard, UNDERLINE_QUERY, UnderlineProbe};

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
    let request = format!("{}{UNDERLINE_QUERY}{KEYBOARD_QUERY}", colours.request());
    stdout.write_all(request.as_bytes()).ok()?;
    stdout.flush().ok()?;

    let stdin = std::io::stdin();
    let deadline = Instant::now() + timeout;
    let mut buffer = [0u8; 1024];

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
        let _ = keyboard.feed(&rest);
    }

    Some(Startup {
        palette: colours.finish(fallback),
        kitty_keyboard: keyboard.supported(),
        underlines,
        attributes: keyboard.attributes().to_vec(),
    })
}

/// Wait up to `timeout` for `stdin` to have something to read.
fn readable(stdin: &std::io::Stdin, timeout: Duration) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let Ok(timeout) = Timespec::try_from(timeout) else { return false };
    let mut fds = [PollFd::new(stdin, PollFlags::IN)];
    matches!(poll(&mut fds, Some(&timeout)), Ok(n) if n > 0)
}

/// What to use when the terminal says nothing.
fn fallback_probe() -> Probe {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| Probe::from_color_fgbg(&value))
        .unwrap_or_else(Probe::builtin_dark)
}
