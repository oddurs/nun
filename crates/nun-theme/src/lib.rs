//! Terminal palette probing and colour derivation.
//!
//! nun ships no themes. It asks the terminal what its colours are and derives a
//! full semantic ramp from the answer, so it always matches the window it is
//! running in.
//!
//! This crate has no terminal dependency. The probe is written sans-I/O:
//! [`ProbeSession`] says what bytes to write and is fed whatever comes back,
//! leaving the actual raw-mode reads, writes and timeout to the caller. That
//! keeps the escape-sequence parsing — the part most likely to be wrong —
//! testable without a tty anywhere near it.

mod color;
mod probe;
mod ramp;

pub use color::{Oklch, Rgb, contrast_ratio};
pub use probe::{Ansi, Probe, ProbeSession, Source};
pub use ramp::{Polarity, Ramp, Role, derive, derive_with_polarity};
