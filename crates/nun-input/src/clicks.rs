//! Single, double and triple clicks.
//!
//! The terminal reports presses, not clicks. [`Clicks`] counts presses that
//! land on the same cell within the multi-click threshold: one, two, three,
//! then round to one again, the way every desktop editor cycles caret, word,
//! line.

use std::time::{Duration, Instant};

/// The platform's usual multi-click threshold.
///
/// macOS and Windows default to 500 ms; GTK, and so most Linux desktops, to
/// 400 ms. Neither can be read from inside a terminal, so this is the default
/// and `ui.double_click_ms` is how a user with a different system setting
/// makes nun agree with it.
pub const PLATFORM_THRESHOLD: Duration = if cfg!(any(target_os = "macos", windows)) {
    Duration::from_millis(500)
} else {
    Duration::from_millis(400)
};

/// Counts presses into clicks.
#[derive(Debug, Clone)]
pub struct Clicks {
    threshold: Duration,
    last: Option<(Instant, u16, u16)>,
    count: u8,
}

impl Clicks {
    /// A counter that joins presses closer together than `threshold`.
    #[must_use]
    pub const fn new(threshold: Duration) -> Self {
        Self { threshold, last: None, count: 0 }
    }

    /// A press at `(x, y)`. Returns 1, 2 or 3.
    ///
    /// The press must land on the same cell as the last one: a double-click
    /// that has moved is two single clicks in different places, and treating it
    /// otherwise selects a word the user was not pointing at.
    pub fn press(&mut self, x: u16, y: u16, now: Instant) -> u8 {
        let continues = self.last.is_some_and(|(at, last_x, last_y)| {
            last_x == x && last_y == y && now.saturating_duration_since(at) <= self.threshold
        });
        self.count = if continues { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, x, y));
        self.count
    }

    /// Forget the last press, so the next one counts from one.
    pub const fn reset(&mut self) {
        self.last = None;
        self.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_millis(400);

    #[test]
    fn presses_in_quick_succession_count_up_and_cycle() {
        let start = Instant::now();
        let mut clicks = Clicks::new(THRESHOLD);
        let step = Duration::from_millis(100);
        let counts: Vec<u8> = (0..5u32).map(|i| clicks.press(3, 4, start + step * i)).collect();
        assert_eq!(counts, vec![1, 2, 3, 1, 2]);
    }

    #[test]
    fn a_slow_second_press_is_a_new_click() {
        let start = Instant::now();
        let mut clicks = Clicks::new(THRESHOLD);
        clicks.press(3, 4, start);
        assert_eq!(clicks.press(3, 4, start + THRESHOLD + Duration::from_millis(1)), 1);
    }

    #[test]
    fn exactly_the_threshold_still_counts() {
        let start = Instant::now();
        let mut clicks = Clicks::new(THRESHOLD);
        clicks.press(0, 0, start);
        assert_eq!(clicks.press(0, 0, start + THRESHOLD), 2);
    }

    #[test]
    fn a_press_somewhere_else_starts_over() {
        let start = Instant::now();
        let mut clicks = Clicks::new(THRESHOLD);
        clicks.press(3, 4, start);
        assert_eq!(clicks.press(4, 4, start), 1);
        assert_eq!(clicks.press(4, 5, start), 1);
    }

    #[test]
    fn a_reset_forgets_the_last_press() {
        let start = Instant::now();
        let mut clicks = Clicks::new(THRESHOLD);
        clicks.press(3, 4, start);
        clicks.reset();
        assert_eq!(clicks.press(3, 4, start), 1);
    }

    #[test]
    fn the_platform_default_is_a_conventional_value() {
        assert!(PLATFORM_THRESHOLD >= Duration::from_millis(400));
        assert!(PLATFORM_THRESHOLD <= Duration::from_millis(500));
    }
}
