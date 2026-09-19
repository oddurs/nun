//! Pointer and keyboard input, resolved against what is on screen.
//!
//! No terminal dependency. Events arrive here already decoded, as plain
//! coordinates and instants, so everything below is unit tested directly:
//!
//! * [`HitMap`] answers "what is at this cell" for every region the layout
//!   pass laid out, topmost first.
//! * [`Hover`] turns a stream of pointer positions into enter and leave events,
//!   exactly once per crossing, with a dwell delay for things like hover cards.

mod geometry;
mod hit;
mod hover;

pub use geometry::Rect;
pub use hit::{Hit, HitMap};
pub use hover::{Crossing, Hover};
