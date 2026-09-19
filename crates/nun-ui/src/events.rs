//! One channel for everything the editor reacts to.
//!
//! The reader thread *blocks* on the terminal rather than polling it, and the
//! main loop blocks on the channel. Nothing anywhere spins, which is what makes
//! an idle editor cost nothing — a poll loop with even a generous interval
//! shows up on a battery.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crossterm::event::{self as term, Event as TermEvent};

use crate::signals::Signal;

/// Anything the editor has to respond to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key was pressed.
    Key(term::KeyEvent),
    /// The mouse moved, was clicked, or scrolled.
    Mouse(term::MouseEvent),
    /// A bracketed paste arrived as one unit.
    Paste(String),
    /// The window changed size.
    Resize(u16, u16),
    /// Focus entered or left the terminal window.
    Focus(bool),
    /// A signal was delivered.
    Signal(Signal),
    /// A filesystem job the editor asked for is done.
    Workspace(nun_workspace::Done),
    /// Something changed inside a watched directory, or watching one failed.
    Files {
        /// The directory whose contents changed.
        dir: std::path::PathBuf,
        /// Why the directory could not be watched, when that is the news.
        error: Option<String>,
    },
    /// The terminal closed, or reading from it failed.
    Closed,
}

/// The editor's single input channel.
#[derive(Debug)]
pub struct Events {
    receiver: Receiver<Event>,
    sender: Sender<Event>,
}

impl Events {
    /// Start reading the terminal and watching for signals.
    ///
    /// # Errors
    ///
    /// If the signal handlers cannot be registered.
    pub fn start() -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();

        let terminal_sender = sender.clone();
        thread::spawn(move || {
            loop {
                // Blocks. No timeout, no polling.
                let Ok(event) = term::read() else {
                    let _ = terminal_sender.send(Event::Closed);
                    break;
                };
                let message = match event {
                    TermEvent::Key(key) => Event::Key(key),
                    TermEvent::Mouse(mouse) => Event::Mouse(mouse),
                    TermEvent::Paste(text) => Event::Paste(text),
                    TermEvent::Resize(width, height) => Event::Resize(width, height),
                    TermEvent::FocusGained => Event::Focus(true),
                    TermEvent::FocusLost => Event::Focus(false),
                };
                if terminal_sender.send(message).is_err() {
                    break;
                }
            }
        });

        let (signal_sender, signal_receiver) = mpsc::channel();
        let _handle = crate::signals::watch(signal_sender)?;

        let forward = sender.clone();
        thread::spawn(move || {
            for signal in signal_receiver {
                if forward.send(Event::Signal(signal)).is_err() {
                    break;
                }
            }
        });

        Ok(Self { receiver, sender })
    }

    /// Wait for the next event.
    ///
    /// Blocks until something happens. Returns [`Event::Closed`] once every
    /// sender is gone.
    #[must_use]
    pub fn next(&self) -> Event {
        self.receiver.recv().unwrap_or(Event::Closed)
    }

    /// Wait for the next event, but no later than `deadline`.
    ///
    /// `None` means the deadline passed first. With no deadline this is
    /// [`Events::next`]: the loop only ever sleeps with a timeout while
    /// something is actually pending — a hover dwell, a chord, an autoscroll —
    /// so an idle editor still wakes for nothing.
    #[must_use]
    pub fn next_before(&self, deadline: Option<std::time::Instant>) -> Option<Event> {
        let Some(deadline) = deadline else {
            return Some(self.next());
        };
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        match self.receiver.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Event::Closed),
        }
    }

    /// Take everything already queued without waiting.
    ///
    /// Used to coalesce a burst — a held-down key, or a flood of mouse motion —
    /// into one redraw instead of one per event.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        self.receiver.try_iter().collect()
    }

    /// A sender, for posting an event from elsewhere in the program.
    #[must_use]
    pub fn sender(&self) -> Sender<Event> {
        self.sender.clone()
    }
}
