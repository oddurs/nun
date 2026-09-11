//! The terminal side of the colour probe.
//!
//! `nun-theme` is deliberately free of I/O: it says what to write and parses
//! what comes back. This is the other half — raw mode, the write, the bounded
//! wait, and putting the terminal back afterwards.

use std::io::{Read as _, Write as _};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::terminal;
use nun_theme::{Probe, ProbeSession};

/// How long to wait for a terminal that may never answer.
///
/// Long enough for a local terminal to reply and short enough that a terminal
/// which ignores OSC queries does not visibly delay start-up.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(120);

/// Ask the terminal for its palette, falling back when it will not say.
///
/// Fallback order matches the design: what the terminal answered, then
/// `COLORFGBG` for the polarity alone, then nun's built-in neutrals.
///
/// This enters raw mode for the duration and restores it before returning,
/// including on the error paths.
pub fn probe_palette(timeout: Duration) -> Probe {
    let fallback = fallback_probe();

    // Without raw mode the reply is line-buffered and echoed, so it would both
    // arrive too late and be printed to the screen.
    let Ok(()) = terminal::enable_raw_mode() else {
        return fallback;
    };
    let probe = probe_in_raw_mode(timeout, &fallback);
    let _ = terminal::disable_raw_mode();
    probe
}

fn probe_in_raw_mode(timeout: Duration, fallback: &Probe) -> Probe {
    let mut session = ProbeSession::new();

    let mut stdout = std::io::stdout();
    if stdout.write_all(session.request().as_bytes()).is_err() || stdout.flush().is_err() {
        return fallback.clone();
    }

    // stdin has no portable read-with-deadline, so the read lives on its own
    // thread and the deadline is enforced on the channel. The thread is left to
    // finish on its own; this path is only reached by a one-shot command that
    // exits immediately afterwards.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 1024];
        while let Ok(count) = stdin.read(&mut buffer) {
            if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(bytes) => {
                // Anything that was not a reply is real input. A one-shot probe
                // has nowhere to put it; the editor's own probe will hand it to
                // the input layer instead.
                let _ = session.feed(&bytes);
                if session.is_complete() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    session.finish(fallback)
}

/// What to use when the terminal says nothing.
fn fallback_probe() -> Probe {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| Probe::from_color_fgbg(&value))
        .unwrap_or_else(Probe::builtin_dark)
}
