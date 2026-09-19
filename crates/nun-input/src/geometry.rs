//! Screen rectangles, in cells.

/// A rectangle of terminal cells.
///
/// nun-input keeps its own rather than borrowing ratatui's so that nothing here
/// depends on a rendering library; the binary converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Rect {
    /// A rectangle at `(x, y)`, `width` by `height`.
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    /// One past the right-most column.
    #[must_use]
    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// One past the bottom row.
    #[must_use]
    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// True when nothing fits inside.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Whether the cell at `(x, y)` is inside.
    #[must_use]
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// The part of this rectangle that is also inside `other`.
    #[must_use]
    pub fn intersection(&self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return Self::new(x, y, 0, 0);
        }
        Self::new(x, y, right - x, bottom - y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_is_half_open() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 7));
        assert!(!r.contains(6, 3), "right edge is exclusive");
        assert!(!r.contains(2, 8), "bottom edge is exclusive");
    }

    #[test]
    fn disjoint_rectangles_intersect_to_nothing() {
        let a = Rect::new(0, 0, 2, 2);
        let b = Rect::new(5, 5, 2, 2);
        assert!(a.intersection(b).is_empty());
        assert_eq!(a.intersection(Rect::new(1, 1, 5, 5)), Rect::new(1, 1, 1, 1));
    }

    #[test]
    fn edges_saturate_rather_than_wrap() {
        let r = Rect::new(u16::MAX - 1, 0, 10, 1);
        assert_eq!(r.right(), u16::MAX);
    }
}
