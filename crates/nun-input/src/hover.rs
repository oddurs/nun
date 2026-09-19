//! Hover, derived from the same resolution as clicks.
//!
//! The terminal reports positions, not crossings. [`Hover`] remembers which
//! target the pointer was last over and reports a change only when that
//! target changes, so a pointer wandering across the cells of one tab produces
//! one enter, not one per cell.
//!
//! Some hover reactions — a hover card, a tooltip — should wait until the
//! pointer has settled. That is the dwell: [`Hover::deadline`] says when to
//! wake up, and [`Hover::dwell`] fires once per visit when it has elapsed.

use std::time::{Duration, Instant};

/// What changed when the pointer moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing<T> {
    /// The target the pointer just left, if it was over one.
    pub left: Option<T>,
    /// The target the pointer just entered, if it is over one now.
    pub entered: Option<T>,
}

impl<T> Crossing<T> {
    /// True when the pointer is still over the same thing.
    #[must_use]
    pub const fn is_none(&self) -> bool {
        self.left.is_none() && self.entered.is_none()
    }
}

/// Which hover target the pointer is over.
#[derive(Debug, Clone)]
pub struct Hover<T> {
    current: Option<T>,
    entered_at: Option<Instant>,
    delay: Duration,
    dwelt: bool,
}

impl<T: Copy + PartialEq> Hover<T> {
    /// A tracker whose dwell fires after `delay` of stillness over one target.
    #[must_use]
    pub const fn new(delay: Duration) -> Self {
        Self { current: None, entered_at: None, delay, dwelt: false }
    }

    /// The target the pointer is over.
    #[must_use]
    pub const fn current(&self) -> Option<T> {
        self.current
    }

    /// The pointer is now over `target` — `None` for nothing that reacts.
    ///
    /// Returns what was left and what was entered, each at most once: moving
    /// within one target changes nothing, and moving from one target straight
    /// to another is a leave and an enter in the same crossing.
    pub fn update(&mut self, target: Option<T>, now: Instant) -> Crossing<T> {
        if target == self.current {
            return Crossing { left: None, entered: None };
        }
        let left = self.current;
        self.current = target;
        self.entered_at = target.map(|_| now);
        self.dwelt = false;
        Crossing { left, entered: target }
    }

    /// The pointer has gone somewhere nun cannot see — out of the window, or
    /// focus lost. Equivalent to moving over nothing.
    pub fn clear(&mut self, now: Instant) -> Crossing<T> {
        self.update(None, now)
    }

    /// When the current visit's dwell will elapse, if it has not already.
    ///
    /// The event loop sleeps until the earliest deadline anywhere and no
    /// longer, so an idle pointer costs nothing once this is `None`.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        if self.dwelt {
            return None;
        }
        self.entered_at.map(|at| at + self.delay)
    }

    /// The target the pointer has now rested on for the full delay.
    ///
    /// Fires once per visit: after it has returned the target, it returns
    /// `None` until the pointer leaves and comes back.
    pub fn dwell(&mut self, now: Instant) -> Option<T> {
        let due = self.deadline()?;
        if now < due {
            return None;
        }
        self.dwelt = true;
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const DELAY: Duration = Duration::from_millis(400);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Tab {
        A,
        B,
    }

    #[test]
    fn entering_a_target_fires_once_however_many_cells_are_crossed() {
        let now = Instant::now();
        let mut hover = Hover::new(DELAY);

        assert_eq!(hover.update(Some(Tab::A), now).entered, Some(Tab::A));
        for _ in 0..10 {
            assert!(hover.update(Some(Tab::A), now).is_none(), "same target, no event");
        }
    }

    #[test]
    fn moving_straight_from_one_target_to_another_is_a_leave_and_an_enter() {
        let now = Instant::now();
        let mut hover = Hover::new(DELAY);
        hover.update(Some(Tab::A), now);

        let crossing = hover.update(Some(Tab::B), now);
        assert_eq!(crossing, Crossing { left: Some(Tab::A), entered: Some(Tab::B) });
    }

    #[test]
    fn leaving_for_nothing_is_a_leave_alone() {
        let now = Instant::now();
        let mut hover = Hover::new(DELAY);
        hover.update(Some(Tab::A), now);
        assert_eq!(hover.clear(now), Crossing { left: Some(Tab::A), entered: None });
        assert!(hover.clear(now).is_none(), "and only once");
    }

    #[test]
    fn dwell_fires_once_after_the_delay() {
        let start = Instant::now();
        let mut hover = Hover::new(DELAY);
        hover.update(Some(Tab::A), start);

        assert_eq!(hover.deadline(), Some(start + DELAY));
        assert_eq!(hover.dwell(start + DELAY / 2), None, "too early");
        assert_eq!(hover.dwell(start + DELAY), Some(Tab::A));
        assert_eq!(hover.dwell(start + DELAY * 3), None, "already fired this visit");
        assert_eq!(hover.deadline(), None, "nothing left to wake up for");
    }

    #[test]
    fn moving_to_another_target_restarts_the_dwell() {
        let start = Instant::now();
        let mut hover = Hover::new(DELAY);
        hover.update(Some(Tab::A), start);
        hover.update(Some(Tab::B), start + DELAY / 2);

        assert_eq!(hover.dwell(start + DELAY), None, "B has not been rested on long enough");
        assert_eq!(hover.dwell(start + DELAY / 2 + DELAY), Some(Tab::B));
    }

    #[test]
    fn nothing_hovered_means_nothing_to_wake_up_for() {
        let hover: Hover<Tab> = Hover::new(DELAY);
        assert_eq!(hover.deadline(), None);
    }

    proptest! {
        /// Replaying any path, the enters and leaves pair up: every enter is
        /// matched by exactly one leave (or is still current at the end), and
        /// no target is entered twice without leaving in between.
        #[test]
        fn enters_and_leaves_pair_up(path in prop::collection::vec(prop::option::of(0u8..4), 0..64)) {
            let now = Instant::now();
            let mut hover = Hover::new(DELAY);
            let mut inside: Option<u8> = None;

            for target in path {
                let crossing = hover.update(target, now);
                if let Some(left) = crossing.left {
                    prop_assert_eq!(Some(left), inside, "left something it was not in");
                    inside = None;
                }
                if let Some(entered) = crossing.entered {
                    prop_assert_eq!(inside, None, "entered without leaving");
                    inside = Some(entered);
                }
                prop_assert_eq!(inside, target);
            }
        }
    }
}
