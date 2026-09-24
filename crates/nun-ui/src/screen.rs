//! The real terminal, owned.

use std::io::{self, Stdout};

use ratatui::Terminal;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::backend::NunBackend;
use crate::lifecycle::{Capabilities, CrosstermControl, TerminalGuard};
use crate::underline::Underlines;

/// Owns the terminal for as long as the editor is running.
///
/// Dropping it restores the terminal, and so does panicking through it, because
/// the guard inside does both.
#[derive(Debug)]
pub struct Screen {
    terminal: Terminal<NunBackend<Stdout>>,
    guard: TerminalGuard<CrosstermControl>,
    capabilities: Capabilities,
    /// Whether the caller wants any-motion tracking, kept so a resume from
    /// suspend can put it back.
    motion_wanted: bool,
}

impl Screen {
    /// Enter the terminal and prepare to draw, with the underlines it said
    /// it can draw.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be entered, or the backend cannot be created.
    pub fn open(capabilities: Capabilities, underlines: Underlines) -> io::Result<Self> {
        let guard = TerminalGuard::enter(CrosstermControl, capabilities)?;
        let terminal = Terminal::new(NunBackend::new(io::stdout(), underlines))?;
        Ok(Self { terminal, guard, capabilities, motion_wanted: false })
    }

    /// Draw underlines as `underlines` from now on, and draw every cell again
    /// at the next frame, since a cell that has not changed is otherwise
    /// never written.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be cleared.
    pub fn set_underlines(&mut self, underlines: Underlines) -> io::Result<()> {
        self.terminal.backend_mut().set_underlines(underlines);
        self.terminal.clear()
    }

    /// Draw exact colours as they are, or as their nearest in the
    /// 256-colour palette, from now on; and draw every cell again at the
    /// next frame, as [`Screen::set_underlines`] does.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be cleared.
    pub fn set_truecolor(&mut self, truecolor: bool) -> io::Result<()> {
        self.terminal.backend_mut().set_truecolor(truecolor);
        self.terminal.clear()
    }

    /// Draw one frame.
    ///
    /// Only the cells that differ from the previous frame are written, so an
    /// unchanged screen costs nothing and a one-character edit writes one cell.
    ///
    /// # Errors
    ///
    /// If writing to the terminal fails.
    pub fn draw(&mut self, widget: impl Widget) -> io::Result<()> {
        self.terminal.draw(|frame| frame.render_widget(widget, frame.area()))?;
        Ok(())
    }

    /// Write an escape that draws nothing — a copy through OSC 52 — to the
    /// terminal, between frames, so it cannot land inside one.
    ///
    /// # Errors
    ///
    /// If writing to the terminal fails.
    pub fn send(&mut self, escape: &str) -> io::Result<()> {
        use std::io::Write as _;
        let backend = self.terminal.backend_mut();
        backend.write_all(escape.as_bytes())?;
        backend.flush()
    }

    /// Report pointer motion with no button held, for as long as something on
    /// screen reacts to hover.
    ///
    /// Call it with the current answer as often as is convenient — once per
    /// frame is fine — because only a change is written to the terminal. See
    /// [`TerminalGuard::track_motion`].
    ///
    /// # Errors
    ///
    /// If writing to the terminal fails.
    pub fn track_motion(&mut self, on: bool) -> io::Result<()> {
        self.motion_wanted = on;
        self.guard.track_motion(on)
    }

    /// The drawable area.
    ///
    /// # Errors
    ///
    /// If the terminal size cannot be read.
    pub fn area(&self) -> io::Result<Rect> {
        self.terminal.size().map(|size| Rect::new(0, 0, size.width, size.height))
    }

    /// Re-read the terminal size and discard the cached frame.
    ///
    /// Called on `SIGWINCH`. The next draw repaints everything, because after a
    /// resize the previous frame describes a screen that no longer exists.
    ///
    /// # Errors
    ///
    /// If the terminal size cannot be read, or the redraw fails.
    pub fn resized(&mut self) -> io::Result<()> {
        let size = self.terminal.size()?;
        self.terminal.resize(Rect::new(0, 0, size.width, size.height))?;
        Ok(())
    }

    /// Restore the terminal, stop this process, and re-enter on resume.
    ///
    /// This is the `SIGTSTP` path. The terminal has to go back to how the shell
    /// left it before the process stops, or the user gets a shell with no echo.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be re-entered afterwards.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.guard.restore();
        crate::signals::suspend_self();
        // Execution resumes here on SIGCONT.
        self.guard = TerminalGuard::enter(CrosstermControl, self.capabilities)?;
        // Hover is optional. If it cannot be put back, the guard records it as
        // off and the next `track_motion` call tries again; it is not a reason
        // to end the session.
        let _ = self.guard.track_motion(self.motion_wanted);
        self.terminal.clear()?;
        Ok(())
    }

    /// Restore the terminal now rather than at drop.
    pub fn close(&mut self) {
        self.guard.restore();
    }
}
