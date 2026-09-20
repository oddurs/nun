//! Rendering, styling and terminal lifecycle.
//!
//! Two things here are load-bearing:
//!
//! * **The terminal is always restored.** [`TerminalGuard`] tracks exactly what
//!   it entered and undoes precisely that, on drop, on panic, and on signal. It
//!   is generic over [`TerminalControl`] so the exactly-once behaviour can be
//!   tested with no tty in sight.
//! * **Nothing names a colour.** [`Palette`] turns a [`nun_theme::Ramp`] into
//!   ratatui styles, and it is the only place in the UI that touches a
//!   `ratatui::style::Color`.

mod events;
mod harness;
mod layout;
mod lifecycle;
mod menu;
mod palette;
mod screen;
mod signals;
mod style;
mod syntax;
mod tabs;
mod tree;
mod view;

pub use events::{Event, Events};
pub use harness::{Harness, changed_cells, changed_rows};
pub use layout::{Dir, Divider, Edge, Layout, Side};
pub use lifecycle::{
    Capabilities, CrosstermControl, TerminalControl, TerminalGuard, install_panic_hook,
};
pub use menu::{Menu, MenuItem};
pub use palette::{Entry as PaletteEntry, MOST_ROWS, PaletteView};
pub use screen::Screen;
pub use signals::{Signal, suspend_self, watch as watch_signals};
pub use style::Palette;
pub use syntax::{FALLBACK, role_of};
pub use tabs::{Tab, TabStrip};
pub use tree::{TreeButton, TreeView};
pub use view::EditorView;
