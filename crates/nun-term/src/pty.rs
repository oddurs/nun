//! A program in a pseudo-terminal, read and written off the main thread.
//!
//! Two threads per terminal. The reader blocks on the pty and posts whatever
//! arrives as a [`Report`]; the writer takes bytes from a channel and blocks
//! writing them. The main thread only ever sends, so a program that stops
//! reading its input, or floods its output, never holds up a frame.
//!
//! A flood is bounded all the same. Every byte read counts against an
//! allowance until the main thread says it has parsed it
//! ([`Pty::consumed`]); past [`MOST_UNREAD`] the reader stops reading, which
//! leaves the program blocked on a full pty, exactly as it would be under a
//! slow terminal. The reader parks rather than spins, and is unparked by the
//! main thread, so there is no lock anywhere between the two.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::tty;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};

/// Which terminal, as the reports from it name it.
pub type Id = u32;

/// How much output may be read and not yet parsed before the reader waits.
pub const MOST_UNREAD: usize = 4 * 1024 * 1024;

/// How much the reader asks for at once.
const CHUNK: usize = 64 * 1024;

/// How long a hung-up program has to go before it is killed.
const GRACE: Duration = Duration::from_millis(500);

/// Variables that tell a program which terminal it is in. Inherited, they
/// would name the terminal nun is running in, and a program inside the panel
/// would talk to that one instead: `tmux` passthrough, Kitty's graphics, a
/// shell integration's escape codes.
///
/// The outer terminal's size and colours go too: `COLUMNS` and `LINES` would
/// win over the pty's own size in curses, and `COLORFGBG` describes a
/// background that is not the panel's.
const OUTER: &[&str] = &[
    "ALACRITTY_LOG",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "COLORFGBG",
    "COLUMNS",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_PROFILE",
    "ITERM_SESSION_ID",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_SHELL_INTEGRATION",
    "KITTY_WINDOW_ID",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "LINES",
    "NVIM",
    "STY",
    "TERMINFO",
    "TERM_FEATURES",
    "TERM_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "VTE_VERSION",
    "WEZTERM_CONFIG_DIR",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WINDOWID",
    "WT_SESSION",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
];

/// News from a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// The program wrote this.
    Output {
        /// Which terminal.
        id: Id,
        /// What it wrote, as it came.
        bytes: Vec<u8>,
    },
    /// The pty closed: the program, and everything that shared its terminal,
    /// is gone.
    Exited {
        /// Which terminal.
        id: Id,
    },
}

/// A terminal's size, in cells, and a cell's size in pixels where that is
/// known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// A cell's width and height in pixels: the cells of the terminal nun
    /// is drawn in, since those are the ones the panel's are drawn with.
    /// Zero where that terminal did not say.
    pub cell: (u16, u16),
}

impl Size {
    /// A size, never smaller than one cell each way: a program told it has no
    /// room at all divides by it.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols: cols.max(1), rows: rows.max(1), cell: (0, 0) }
    }

    /// The same, with cells `cell` pixels wide and high, if that is known.
    #[must_use]
    pub fn with_cell(self, cell: Option<(u16, u16)>) -> Self {
        Self { cell: cell.unwrap_or((0, 0)), ..self }
    }

    /// The whole screen in pixels, width then height: zero where a cell's
    /// size is not known.
    #[must_use]
    pub fn pixels(self) -> (u16, u16) {
        (self.cols.saturating_mul(self.cell.0), self.rows.saturating_mul(self.cell.1))
    }

    /// As the pty layer takes it. It multiplies the cell by the screen in
    /// `u16`, so the cell is kept to what fits.
    fn window(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell.0.min(u16::MAX / self.cols),
            cell_height: self.cell.1.min(u16::MAX / self.rows),
        }
    }
}

/// What to run, where, and how big.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The directory it starts in.
    pub cwd: PathBuf,
    /// Variables set for it, over what nun was started with.
    pub env: Vec<(String, String)>,
    /// How big the terminal is to begin with.
    pub size: Size,
}

impl Spec {
    /// The person's shell — `$SHELL`, or `/bin/sh` without one — in `cwd`,
    /// told it is in an xterm-compatible terminal with true colour, and not
    /// in whichever terminal nun itself is running in. See
    /// [`Spec::without_truecolor`] for a terminal that has none.
    #[must_use]
    pub fn shell(cwd: PathBuf, size: Size) -> Self {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|shell| !shell.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        Self::program(&shell, &[], cwd, size)
    }

    /// `program` with `args`, set up as [`Spec::shell`] sets up the shell.
    #[must_use]
    pub fn program(program: &str, args: &[&str], cwd: PathBuf, size: Size) -> Self {
        // `env -u` rather than leaving them out of the environment: the pty
        // layer only adds variables, and a variable set to nothing is still
        // set to a program that asks whether it is.
        let mut command: Vec<String> = Vec::new();
        for name in OUTER {
            command.push("-u".into());
            command.push((*name).into());
        }
        command.push(program.into());
        command.extend(args.iter().map(|arg| (*arg).to_string()));
        Self {
            program: "/usr/bin/env".into(),
            args: command,
            cwd,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
                ("TERM_PROGRAM".into(), "nun".into()),
                ("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()),
            ],
            size,
        }
    }
}

impl Spec {
    /// The same, not told it has 24-bit colour: the terminal nun is drawn in
    /// was not found to have it, so exact colours would be approximated
    /// anyway, and a program that knows it picks its own approximation.
    /// `COLORTERM` is unset, not merely left out, so the outer terminal's
    /// does not leak in.
    #[must_use]
    pub fn without_truecolor(mut self) -> Self {
        self.env.retain(|(name, _)| name != "COLORTERM");
        if self.program == "/usr/bin/env" {
            self.args.splice(0..0, ["-u".to_string(), "COLORTERM".to_string()]);
        }
        self
    }
}

/// A running program in a pty, and the threads that read and write it.
///
/// Dropping it is the guarantee that nothing leaks: the program's process
/// group is hung up, given [`GRACE`] to go, killed if it has not, and reaped.
/// That runs however the terminal goes — closed, the editor quit, or a panic
/// unwinding through it — and a signal that ends the editor ends it through
/// the same quit. Only `SIGKILL` skips it, and then the kernel closes the pty,
/// which hangs the program up all the same.
///
/// Dropping one waits up to [`GRACE`] for a program that ignores the hang-up,
/// so the editor never drops one where it would hold up a frame: it hands it
/// to [`Pty::close_in_background`] instead.
pub struct Pty {
    id: Id,
    /// Taken in `drop`, after the hang-up, so its own drop only reaps.
    child: Option<tty::Pty>,
    pid: Pid,
    /// Taken in `drop`, which ends the writer.
    writer: Option<Sender<Vec<u8>>>,
    reader: Thread,
    unread: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    size: Size,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty").field("id", &self.id).field("pid", &self.pid).finish_non_exhaustive()
    }
}

impl Pty {
    /// Start `spec` in a new pty, reporting its output through `report`.
    ///
    /// # Errors
    ///
    /// If the pty cannot be opened, the program cannot be started, or the
    /// threads cannot be.
    pub fn spawn(
        id: Id,
        spec: &Spec,
        report: Arc<dyn Fn(Report) + Send + Sync>,
    ) -> io::Result<Self> {
        let options = tty::Options {
            shell: Some(tty::Shell::new(spec.program.clone(), spec.args.clone())),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: false,
            env: spec.env.iter().cloned().collect::<HashMap<_, _>>(),
        };
        let child = tty::new(&options, spec.size.window(), u64::from(id))?;
        let pid = Pid::from_child(child.child());

        // The pty layer leaves the master non-blocking, for a poll loop this
        // crate does not have. Blocking reads on a thread of their own are
        // simpler and cost nothing while the program is quiet.
        let master = child.file().try_clone()?;
        rustix::io::ioctl_fionbio(&master, false)?;
        let mut guard = Self {
            id,
            child: Some(child),
            pid,
            writer: None,
            reader: thread::current(),
            unread: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicBool::new(false)),
            size: spec.size,
        };

        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let mut output = master.try_clone()?;
        thread::Builder::new().name(format!("nun-term-{id}-write")).spawn(move || {
            for bytes in receiver {
                if output.write_all(&bytes).is_err() {
                    break;
                }
            }
        })?;
        guard.writer = Some(sender);

        let reader = thread::Builder::new().name(format!("nun-term-{id}-read")).spawn({
            let unread = Arc::clone(&guard.unread);
            let closed = Arc::clone(&guard.closed);
            move || read(id, master, &unread, &closed, &*report)
        })?;
        guard.reader = reader.thread().clone();
        Ok(guard)
    }

    /// Which terminal this is.
    #[must_use]
    pub const fn id(&self) -> Id {
        self.id
    }

    /// The program's process id, which is also its process group's and its
    /// session's.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_nonzero().get().unsigned_abs()
    }

    /// The size the program was last told.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    /// Send the program `bytes`, as though typed. Never blocks: the writer
    /// thread does the waiting.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        if let Some(writer) = &self.writer {
            // Gone only once the program is, and then there is no one to
            // tell.
            let _ = writer.send(bytes);
        }
    }

    /// Say that `bytes` of what was reported have been parsed, so the reader
    /// may read that much more.
    pub fn consumed(&self, bytes: usize) {
        let before =
            self.unread.fetch_sub(bytes.min(self.unread.load(Ordering::Acquire)), Ordering::AcqRel);
        if before >= MOST_UNREAD {
            self.reader.unpark();
        }
    }

    /// Tell the program its terminal is now `size`. The kernel sends its
    /// foreground process group `SIGWINCH` when the size changes, and not
    /// when it does not.
    ///
    /// # Errors
    ///
    /// If the pty refuses the new size.
    pub fn resize(&mut self, size: Size) -> io::Result<()> {
        if size == self.size {
            return Ok(());
        }
        let Some(child) = &self.child else { return Ok(()) };
        // The pixels too, which image viewers read before they think of
        // asking.
        let (width, height) = size.pixels();
        let window = rustix::termios::Winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: width,
            ws_ypixel: height,
        };
        rustix::termios::tcsetwinsize(child.file(), window)?;
        self.size = size;
        Ok(())
    }

    /// Hang the program up on a thread of its own, so a program slow to go
    /// holds up nothing. Join the handle before exiting, or a program that
    /// ignores the hang-up outlives the editor.
    #[must_use]
    pub fn close_in_background(self) -> JoinHandle<()> {
        thread::spawn(move || drop(self))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.reader.unpark();
        self.writer = None;
        hang_up(self.pid);
        // Its own drop hangs up once more and waits, which now only reaps.
        self.child = None;
    }
}

/// Read until the pty closes, posting what arrives.
fn read(
    id: Id,
    mut master: File,
    unread: &AtomicUsize,
    closed: &AtomicBool,
    report: &dyn Fn(Report),
) {
    let mut buffer = vec![0; CHUNK];
    loop {
        while unread.load(Ordering::Acquire) >= MOST_UNREAD && !closed.load(Ordering::Acquire) {
            thread::park();
        }
        // Once the terminal is closing, what is read is thrown away, but it
        // is still read until the pty says the program has gone. A program
        // exiting waits for the terminal to take what it wrote last, and a
        // reader that stopped would leave it — and the drop waiting on it —
        // stuck there for good.
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) if closed.load(Ordering::Acquire) => {}
            Ok(read) => {
                unread.fetch_add(read, Ordering::AcqRel);
                report(Report::Output { id, bytes: buffer[..read].to_vec() });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // EIO is how a pty says the other side has closed.
            Err(_) => break,
        }
    }
    if !closed.load(Ordering::Acquire) {
        report(Report::Exited { id });
    }
}

/// Hang up the program's process group, and kill it if it is still there
/// after [`GRACE`]. The program leads its own session and group, so this
/// reaches everything it started that has not moved itself elsewhere, and
/// the kernel hangs up the terminal's foreground group when it goes.
fn hang_up(pid: Pid) {
    let _ = rustix::process::kill_process_group(pid, Signal::HUP);
    // A stopped job cannot act on the hang-up until it is continued.
    let _ = rustix::process::kill_process_group(pid, Signal::CONT);
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        if exited(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = rustix::process::kill_process_group(pid, Signal::KILL);
    let _ = rustix::process::kill_process(pid, Signal::KILL);
}

/// Whether the program has exited, without reaping it: it stays a zombie, so
/// its pid cannot be reused by anything a later signal would reach.
fn exited(pid: Pid) -> bool {
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    match rustix::process::waitid(WaitId::Pid(pid), options) {
        Ok(status) => status.is_some(),
        // Not a child of this process any more: gone.
        Err(_) => true,
    }
}
