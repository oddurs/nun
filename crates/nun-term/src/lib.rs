//! The integrated terminal: a shell in a pseudo-terminal, and the screen it
//! draws.
//!
//! Four pieces, kept apart so each can be tested on its own:
//!
//! - [`Pty`] runs a program in a pseudo-terminal. Reading and writing happen
//!   on threads of their own, and what is read comes back as a [`Report`]
//!   through a callback, so nothing on the main thread ever waits on the
//!   program. Dropping it hangs the program up, and kills it if it will not
//!   go: nothing it started outlives the panel it ran in.
//! - [`Emulator`] is the screen: the bytes a program writes, parsed into a
//!   grid of cells with scrollback, a cursor, the modes the program asked for
//!   and a selection. It never touches a file descriptor, so it is fed
//!   recorded byte streams in the tests.
//! - [`input`] turns keys, the mouse and a paste into the bytes a program
//!   expects for them, in whichever modes it has turned on.
//! - [`links`] finds what in a line of output can be followed: a file with a
//!   line and column, or a URL.
//!
//! The emulation itself is `alacritty_terminal`'s. A terminal is judged by
//! the programs it runs — `less`, `top`, an editor inside the editor — and
//! those need the alternate screen, scroll regions, the cursor and mouse
//! modes, bracketed paste and wide characters to be right, not approximately
//! right. Its pty layer is used too, because spawning into a new session with
//! a controlling terminal needs code this workspace forbids itself from
//! writing.
//!
//! No terminal of nun's own is touched here: the crate knows nothing of the
//! screen nun is drawn on, and colours come out as [`Ink`], which the UI turns
//! into styles in the one place that is allowed to name a colour.

pub mod emulator;
pub mod input;
pub mod links;
pub mod pty;

pub use emulator::{Answers, Attrs, Cell, Effect, Emulator, Ink, Modes, Pick, Width};
pub use input::{Button, Key, Mods, Pointer};
pub use links::{Found, Target};
pub use pty::{Id, Pty, Report, Size, Spec};
