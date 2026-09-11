//! Signals become messages, like everything else.
//!
//! The handler thread does no terminal work of its own: it posts to the
//! editor's channel and the main loop decides. That keeps the one owner of
//! terminal state on the main thread, which is the rule that stops a signal
//! arriving mid-frame from corrupting the screen.

use std::io;
use std::sync::mpsc::Sender;

/// A signal worth reacting to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGTERM` — shut down cleanly.
    Terminate,
    /// `SIGHUP` — the terminal went away; shut down cleanly.
    Hangup,
    /// `SIGTSTP` — restore the terminal, then stop.
    Suspend,
    /// `SIGCONT` — re-enter the terminal and repaint.
    Continue,
    /// `SIGWINCH` — the terminal was resized.
    Resize,
}

/// Keeps the handler thread alive; dropping it stops watching.
#[derive(Debug)]
pub struct Handle {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
}

impl Handle {
    /// Stop watching.
    pub fn close(&self) {
        #[cfg(unix)]
        self.handle.close();
    }
}

/// Watch for signals and post them to `sender`.
///
/// # Errors
///
/// If the signal handlers cannot be registered.
#[cfg(unix)]
pub fn watch(sender: Sender<Signal>) -> io::Result<Handle> {
    use signal_hook::consts::signal::{SIGCONT, SIGHUP, SIGTERM, SIGTSTP, SIGWINCH};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGHUP, SIGTSTP, SIGCONT, SIGWINCH])?;
    let handle = signals.handle();

    std::thread::spawn(move || {
        for signal in &mut signals {
            let message = match signal {
                SIGTERM => Signal::Terminate,
                SIGHUP => Signal::Hangup,
                SIGTSTP => Signal::Suspend,
                SIGCONT => Signal::Continue,
                SIGWINCH => Signal::Resize,
                _ => continue,
            };
            if sender.send(message).is_err() {
                break;
            }
        }
    });

    Ok(Handle { handle })
}

/// No-op on platforms without POSIX signals.
///
/// # Errors
///
/// Never.
#[cfg(not(unix))]
pub fn watch(_sender: Sender<Signal>) -> io::Result<Handle> {
    Ok(Handle {})
}

/// Stop this process the way `SIGTSTP` would have, having already restored the
/// terminal.
///
/// The signal is re-raised with the default handler so the shell sees a normal
/// job-control stop rather than a process that ignored it.
#[cfg(unix)]
pub fn suspend_self() {
    let _ = signal_hook::low_level::emulate_default_handler(signal_hook::consts::signal::SIGTSTP);
}

/// No-op on platforms without job control.
#[cfg(not(unix))]
pub fn suspend_self() {}
