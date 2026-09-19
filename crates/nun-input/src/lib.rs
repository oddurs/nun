//! Pointer and keyboard input, resolved against what is on screen.
//!
//! No terminal dependency. Events arrive here already decoded, as plain
//! coordinates and instants, so everything below is unit tested directly:
//!
//! * [`HitMap`] answers "what is at this cell" for every region the layout
//!   pass laid out, topmost first.
//! * [`Hover`] turns a stream of pointer positions into enter and leave events,
//!   exactly once per crossing, with a dwell delay for things like hover cards.
//! * [`Clicks`] turns presses into single, double and triple clicks.
//! * [`Keymap`] and [`Chords`] turn keystrokes into commands, chords included,
//!   and [`KeyboardProbe`] finds out whether the terminal can report the keys
//!   the full binding set needs.

mod clicks;
mod geometry;
mod hit;
mod hover;
mod keymap;
mod keys;
mod negotiate;

pub use clicks::{Clicks, PLATFORM_THRESHOLD};
pub use geometry::Rect;
pub use hit::{Hit, HitMap};
pub use hover::{Crossing, Hover};
pub use keymap::{Chords, Keymap, Resolved};
pub use keys::{Code, Key, Mods, ParseError, Sequence, parse_key, parse_sequence};
pub use negotiate::{KEYBOARD_QUERY, KeyboardProbe};
