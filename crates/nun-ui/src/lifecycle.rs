//! Entering and leaving the terminal, exactly once, on every path.
//!
//! A TUI that panics in raw mode leaves the user with a dead shell and no echo.
//! It is the rudest failure a terminal program has, so the teardown is tracked
//! rather than trusted: the guard records precisely what it turned on and turns
//! off precisely that, whether it is dropped, unwound through, or signalled.
//!
//! Everything goes through [`TerminalControl`] so the exactly-once behaviour is
//! testable without a tty.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Optional terminal features the guard can turn on.
// Four independent on/off features. Packing them into bitflags would hide
// which one a call site is asking for, which is the only thing that matters
// here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Switch to the alternate screen, so the user's scrollback survives.
    pub alternate_screen: bool,
    /// Enable SGR mouse reporting with button-motion tracking.
    pub mouse: bool,
    /// Negotiate the Kitty keyboard protocol for real modifier reporting.
    pub keyboard_enhancement: bool,
    /// Hide the terminal's own cursor; nun draws its own.
    pub hide_cursor: bool,
}

impl Default for Capabilities {
    /// Everything on, which is what the editor wants.
    fn default() -> Self {
        Self { alternate_screen: true, mouse: true, keyboard_enhancement: true, hide_cursor: true }
    }
}

impl Capabilities {
    /// Nothing on, for a one-shot command that only needs raw mode.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            alternate_screen: false,
            mouse: false,
            keyboard_enhancement: false,
            hide_cursor: false,
        }
    }
}

/// The terminal operations the guard needs.
///
/// A trait rather than direct crossterm calls so that the ordering and the
/// exactly-once property can be asserted against a recording fake.
pub trait TerminalControl {
    /// Put the terminal into raw mode.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn enable_raw_mode(&mut self) -> io::Result<()>;
    /// Take it out again.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn disable_raw_mode(&mut self) -> io::Result<()>;
    /// Switch to the alternate screen.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn enter_alternate_screen(&mut self) -> io::Result<()>;
    /// Switch back.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn leave_alternate_screen(&mut self) -> io::Result<()>;
    /// Turn on mouse reporting.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn enable_mouse(&mut self) -> io::Result<()>;
    /// Turn it off.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn disable_mouse(&mut self) -> io::Result<()>;
    /// Push keyboard-enhancement flags.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn push_keyboard_flags(&mut self) -> io::Result<()>;
    /// Pop them.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn pop_keyboard_flags(&mut self) -> io::Result<()>;
    /// Hide the terminal cursor.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn hide_cursor(&mut self) -> io::Result<()>;
    /// Show it again.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports.
    fn show_cursor(&mut self) -> io::Result<()>;
}

/// Owns whatever terminal state was entered, and undoes it.
// One flag per piece of terminal state actually entered. Tracking them
// separately is the entire mechanism: it is what makes the teardown undo
// precisely what was done and nothing else.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct TerminalGuard<C: TerminalControl> {
    control: C,
    raw: bool,
    alternate: bool,
    mouse: bool,
    keyboard_flags: bool,
    cursor_hidden: bool,
}

impl<C: TerminalControl> TerminalGuard<C> {
    /// Enter the terminal, turning on everything `capabilities` asks for.
    ///
    /// If any step fails, everything already entered is undone before the error
    /// is returned — a half-entered terminal is the same problem as a
    /// half-restored one.
    ///
    /// # Errors
    ///
    /// The first failing operation's error.
    pub fn enter(control: C, capabilities: Capabilities) -> Result<Self, io::Error> {
        let mut guard = Self {
            control,
            raw: false,
            alternate: false,
            mouse: false,
            keyboard_flags: false,
            cursor_hidden: false,
        };

        match guard.enter_all(capabilities) {
            Ok(()) => Ok(guard),
            Err(error) => {
                guard.restore();
                Err(error)
            }
        }
    }

    fn enter_all(&mut self, capabilities: Capabilities) -> io::Result<()> {
        self.control.enable_raw_mode()?;
        self.raw = true;

        if capabilities.alternate_screen {
            self.control.enter_alternate_screen()?;
            self.alternate = true;
        }
        if capabilities.mouse {
            self.control.enable_mouse()?;
            self.mouse = true;
        }
        if capabilities.keyboard_enhancement {
            self.control.push_keyboard_flags()?;
            self.keyboard_flags = true;
            KEYBOARD_FLAGS_PUSHED.fetch_add(1, Ordering::SeqCst);
        }
        if capabilities.hide_cursor {
            self.control.hide_cursor()?;
            self.cursor_hidden = true;
        }
        Ok(())
    }

    /// Undo everything that was entered. Safe to call more than once.
    ///
    /// Each flag is cleared before its operation runs, so a second call — from
    /// `Drop` after an explicit `restore`, say — is a no-op rather than a second
    /// pop. Order is the reverse of entry, and errors are swallowed: this runs
    /// on the panic path, where returning an error has nowhere to go and
    /// stopping early would strand the remaining state.
    pub fn restore(&mut self) {
        if std::mem::take(&mut self.cursor_hidden) {
            let _ = self.control.show_cursor();
        }
        if std::mem::take(&mut self.keyboard_flags) {
            let _ = self.control.pop_keyboard_flags();
            KEYBOARD_FLAGS_PUSHED.fetch_sub(1, Ordering::SeqCst);
        }
        if std::mem::take(&mut self.mouse) {
            let _ = self.control.disable_mouse();
        }
        if std::mem::take(&mut self.alternate) {
            let _ = self.control.leave_alternate_screen();
        }
        if std::mem::take(&mut self.raw) {
            let _ = self.control.disable_raw_mode();
        }
    }

    /// Whether any terminal state is still entered.
    #[must_use]
    pub const fn is_entered(&self) -> bool {
        self.raw || self.alternate || self.mouse || self.keyboard_flags || self.cursor_hidden
    }

    /// The control underneath, for tests and for re-entry after a suspend.
    pub const fn control_mut(&mut self) -> &mut C {
        &mut self.control
    }
}

impl<C: TerminalControl> Drop for TerminalGuard<C> {
    fn drop(&mut self) {
        self.restore();
    }
}

/// How many keyboard-flag pushes are outstanding process-wide.
///
/// The emergency teardown on the panic path has no access to the guard, so this
/// is what stops it popping flags that were never pushed — a double pop would
/// disturb whatever the user's shell had set.
static KEYBOARD_FLAGS_PUSHED: AtomicUsize = AtomicUsize::new(0);

/// Install a panic hook that puts the terminal back before printing.
///
/// Chains to the previous hook rather than replacing it, so the panic message
/// and backtrace still appear — on a terminal that can display them.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emergency_restore();
        previous(info);
    }));
}

/// Best-effort teardown with no guard to consult.
///
/// Only reached from the panic hook and the signal path. Everything here is
/// harmless to repeat except popping keyboard flags, which is why that one is
/// counted.
pub fn emergency_restore() {
    use crossterm::{cursor, event, execute, terminal};

    let mut out = io::stdout();
    let _ = execute!(out, cursor::Show);

    // Only pop what is actually outstanding.
    while KEYBOARD_FLAGS_PUSHED
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
    {
        let _ = execute!(out, event::PopKeyboardEnhancementFlags);
    }

    let _ = execute!(out, event::DisableMouseCapture);
    let _ = execute!(out, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// The real terminal.
#[derive(Debug, Default)]
pub struct CrosstermControl;

impl TerminalControl for CrosstermControl {
    fn enable_raw_mode(&mut self) -> io::Result<()> {
        crossterm::terminal::enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()
    }

    fn enter_alternate_screen(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)
    }

    fn leave_alternate_screen(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen)
    }

    fn enable_mouse(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)
    }

    fn disable_mouse(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture)
    }

    fn push_keyboard_flags(&mut self) -> io::Result<()> {
        use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
        // Asked for rather than assumed: a terminal without support ignores it,
        // and what actually arrived is decided by reading key events, never by
        // looking at $TERM.
        crossterm::execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )
    }

    fn pop_keyboard_flags(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        crossterm::execute!(io::stdout(), crossterm::cursor::Show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Records every operation, and can be told to fail on one of them.
    #[derive(Debug, Default)]
    struct Recorder {
        log: Rc<RefCell<Vec<&'static str>>>,
        fail_on: Option<&'static str>,
    }

    impl Recorder {
        fn new() -> (Self, Rc<RefCell<Vec<&'static str>>>) {
            let log = Rc::new(RefCell::new(Vec::new()));
            (Self { log: Rc::clone(&log), fail_on: None }, log)
        }

        fn failing_on(step: &'static str) -> (Self, Rc<RefCell<Vec<&'static str>>>) {
            let (mut recorder, log) = Self::new();
            recorder.fail_on = Some(step);
            (recorder, log)
        }

        fn record(&mut self, step: &'static str) -> io::Result<()> {
            if self.fail_on == Some(step) {
                return Err(io::Error::other(step));
            }
            self.log.borrow_mut().push(step);
            Ok(())
        }
    }

    impl TerminalControl for Recorder {
        fn enable_raw_mode(&mut self) -> io::Result<()> {
            self.record("raw on")
        }
        fn disable_raw_mode(&mut self) -> io::Result<()> {
            self.record("raw off")
        }
        fn enter_alternate_screen(&mut self) -> io::Result<()> {
            self.record("alt on")
        }
        fn leave_alternate_screen(&mut self) -> io::Result<()> {
            self.record("alt off")
        }
        fn enable_mouse(&mut self) -> io::Result<()> {
            self.record("mouse on")
        }
        fn disable_mouse(&mut self) -> io::Result<()> {
            self.record("mouse off")
        }
        fn push_keyboard_flags(&mut self) -> io::Result<()> {
            self.record("flags push")
        }
        fn pop_keyboard_flags(&mut self) -> io::Result<()> {
            self.record("flags pop")
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            self.record("cursor hide")
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            self.record("cursor show")
        }
    }

    #[test]
    fn everything_entered_is_undone_in_reverse_order() {
        let (recorder, log) = Recorder::new();
        drop(TerminalGuard::enter(recorder, Capabilities::default()).unwrap());

        assert_eq!(
            *log.borrow(),
            vec![
                "raw on",
                "alt on",
                "mouse on",
                "flags push",
                "cursor hide",
                "cursor show",
                "flags pop",
                "mouse off",
                "alt off",
                "raw off",
            ]
        );
    }

    #[test]
    fn restoring_twice_does_not_pop_twice() {
        let (recorder, log) = Recorder::new();
        let mut guard = TerminalGuard::enter(recorder, Capabilities::default()).unwrap();

        guard.restore();
        guard.restore();
        drop(guard);

        let popped = log.borrow().iter().filter(|step| **step == "flags pop").count();
        assert_eq!(popped, 1, "keyboard flags must be popped exactly once");
    }

    #[test]
    fn an_explicit_restore_leaves_nothing_for_drop_to_do() {
        let (recorder, log) = Recorder::new();
        let mut guard = TerminalGuard::enter(recorder, Capabilities::default()).unwrap();
        guard.restore();
        assert!(!guard.is_entered());

        let after_restore = log.borrow().len();
        drop(guard);
        assert_eq!(log.borrow().len(), after_restore, "drop repeated the teardown");
    }

    #[test]
    fn a_capability_that_is_not_asked_for_is_never_undone() {
        let (recorder, log) = Recorder::new();
        drop(TerminalGuard::enter(recorder, Capabilities::none()).unwrap());
        assert_eq!(*log.borrow(), vec!["raw on", "raw off"]);
    }

    #[test]
    fn a_failure_part_way_in_unwinds_what_already_succeeded() {
        // Mouse capture fails; raw mode and the alternate screen are already on
        // and must not be left that way.
        let (recorder, log) = Recorder::failing_on("mouse on");
        let error = TerminalGuard::enter(recorder, Capabilities::default()).unwrap_err();

        assert_eq!(error.to_string(), "mouse on");
        assert_eq!(*log.borrow(), vec!["raw on", "alt on", "alt off", "raw off"]);
    }

    #[test]
    fn a_failure_before_anything_succeeds_leaves_no_teardown() {
        let (recorder, log) = Recorder::failing_on("raw on");
        assert!(TerminalGuard::enter(recorder, Capabilities::default()).is_err());
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn unwinding_through_the_guard_still_restores() {
        let (recorder, log) = Recorder::new();
        let log_for_assert = Rc::clone(&log);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = TerminalGuard::enter(recorder, Capabilities::default()).unwrap();
            panic!("something went wrong mid-frame");
        }));

        assert!(result.is_err());
        assert_eq!(
            log_for_assert.borrow().last(),
            Some(&"raw off"),
            "a panic must leave the terminal usable"
        );
    }
}
