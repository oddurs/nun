//! The real terminal, owned.

use std::io::{self, Stdout};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::lifecycle::{Capabilities, CrosstermControl, TerminalGuard};

/// Owns the terminal for as long as the editor is running.
///
/// Dropping it restores the terminal, and so does panicking through it, because
/// the guard inside does both.
#[derive(Debug)]
pub struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    guard: TerminalGuard<CrosstermControl>,
    capabilities: Capabilities,
}

impl Screen {
    /// Enter the terminal and prepare to draw.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be entered, or the backend cannot be created.
    pub fn open(capabilities: Capabilities) -> io::Result<Self> {
        let guard = TerminalGuard::enter(CrosstermControl, capabilities)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Self { terminal, guard, capabilities })
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
        self.terminal.clear()?;
        Ok(())
    }

    /// Restore the terminal now rather than at drop.
    pub fn close(&mut self) {
        self.guard.restore();
    }
}
