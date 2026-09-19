//! How panes divide the screen.
//!
//! A binary tree: every node splits its rectangle in two, along a direction,
//! at a ratio. Leaves are panes. That is enough for any arrangement of splits
//! people actually make, and it makes closing a pane obvious — the node
//! collapses and its sibling takes the whole rectangle back.
//!
//! Pure geometry, so the whole of it is tested against rectangles rather than
//! against a screen.

use ratatui::layout::Rect;

/// Which way a node divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Side by side, divided by a vertical line.
    Beside,
    /// One above the other, divided by a horizontal line.
    Below,
}

/// Which edge of a pane something is being dropped on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The left quarter.
    Left,
    /// The right quarter.
    Right,
    /// The top quarter.
    Top,
    /// The bottom quarter.
    Bottom,
    /// The middle: not a split, just this pane.
    Middle,
}

impl Edge {
    /// Where a drop on this edge puts the new pane, if it splits at all.
    #[must_use]
    pub const fn split(self) -> Option<(Dir, bool)> {
        match self {
            Self::Left => Some((Dir::Beside, true)),
            Self::Right => Some((Dir::Beside, false)),
            Self::Top => Some((Dir::Below, true)),
            Self::Bottom => Some((Dir::Below, false)),
            Self::Middle => None,
        }
    }

    /// The part of `area` a drop on this edge would take, for the preview.
    #[must_use]
    pub fn preview(self, area: Rect) -> Rect {
        let half_width = area.width / 2;
        let half_height = area.height / 2;
        match self {
            Self::Left => Rect { width: half_width, ..area },
            Self::Right => Rect { x: area.x + area.width - half_width, width: half_width, ..area },
            Self::Top => Rect { height: half_height, ..area },
            Self::Bottom => {
                Rect { y: area.y + area.height - half_height, height: half_height, ..area }
            }
            Self::Middle => area,
        }
    }

    /// Which edge of `area` the point `(x, y)` is in.
    ///
    /// The outer quarter each way; the middle half of the pane is the pane
    /// itself, so dropping a tab back where it came from is not a split.
    #[must_use]
    pub fn at(area: Rect, x: u16, y: u16) -> Self {
        if !area.contains((x, y).into()) {
            return Self::Middle;
        }
        let from_left = x - area.x;
        let from_top = y - area.y;
        let from_right = area.right() - 1 - x;
        let from_bottom = area.bottom() - 1 - y;

        let width_quarter = (area.width / 4).max(1);
        let height_quarter = (area.height / 4).max(1);

        // Whichever edge is nearest in proportion to the pane's own size, so a
        // tall narrow pane still has a usable top and bottom.
        let candidates = [
            (from_left < width_quarter, from_left, Self::Left),
            (from_right < width_quarter, from_right, Self::Right),
            (from_top < height_quarter, from_top, Self::Top),
            (from_bottom < height_quarter, from_bottom, Self::Bottom),
        ];
        candidates
            .into_iter()
            .filter(|(inside, _, _)| *inside)
            .min_by_key(|(_, distance, _)| *distance)
            .map_or(Self::Middle, |(_, _, edge)| edge)
    }
}

/// The tree of panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One pane, by id.
    Pane(usize),
    /// Two layouts, divided.
    Split {
        /// Which way.
        dir: Dir,
        /// How much of the rectangle the first one takes, in thousandths, so
        /// the layout compares equal and stays exact.
        ratio: u16,
        /// Above, or to the left.
        first: Box<Layout>,
        /// Below, or to the right.
        second: Box<Layout>,
    },
}

/// Half, as a ratio.
pub const EVEN: u16 = 500;
/// The narrowest a pane may be dragged to. Columns and rows differ: a pane
/// twelve columns wide can still show code, while three rows is a tab strip
/// and a line of it.
const MIN_COLS: u16 = 12;
/// The shortest a pane may be dragged to, in rows.
const MIN_ROWS: u16 = 3;

impl Layout {
    /// A layout of one pane.
    #[must_use]
    pub const fn single(pane: usize) -> Self {
        Self::Pane(pane)
    }

    /// Split the pane `at`, putting `new` beside or below it.
    ///
    /// Returns whether `at` was in the layout.
    #[must_use]
    pub fn split(&mut self, at: usize, dir: Dir, new: usize, first: bool) -> bool {
        match self {
            Self::Pane(pane) if *pane == at => {
                let (a, b) = if first { (new, at) } else { (at, new) };
                *self = Self::Split {
                    dir,
                    ratio: EVEN,
                    first: Box::new(Self::Pane(a)),
                    second: Box::new(Self::Pane(b)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first: a, second: b, .. } => {
                a.split(at, dir, new, first) || b.split(at, dir, new, first)
            }
        }
    }

    /// Remove a pane; its sibling takes the space back.
    ///
    /// Returns whether anything was removed. The last pane is never removed:
    /// a layout always has one.
    pub fn close(&mut self, pane: usize) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                if **first == Self::Pane(pane) {
                    *self = (**second).clone();
                    return true;
                }
                if **second == Self::Pane(pane) {
                    *self = (**first).clone();
                    return true;
                }
                first.close(pane) || second.close(pane)
            }
        }
    }

    /// Every pane and the rectangle it gets, given the whole area.
    #[must_use]
    pub fn rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        self.walk(area, &mut |pane, rect| out.push((pane, rect)));
        out
    }

    /// The rectangle of one pane.
    #[must_use]
    pub fn rect_of(&self, pane: usize, area: Rect) -> Option<Rect> {
        self.rects(area).into_iter().find(|(id, _)| *id == pane).map(|(_, rect)| rect)
    }

    /// The pane at a point.
    #[must_use]
    pub fn pane_at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        self.rects(area)
            .into_iter()
            .find(|(_, rect)| rect.contains((x, y).into()))
            .map(|(pane, _)| pane)
    }

    /// Every pane, in order.
    #[must_use]
    pub fn panes(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    /// How many panes there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.panes().len()
    }

    /// Always false: a layout always has at least one pane.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Every divider: the cells it occupies, its direction, and the path to
    /// the node it belongs to.
    #[must_use]
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        let mut out = Vec::new();
        self.walk_dividers(area, &mut Vec::new(), &mut out);
        out
    }

    /// Drag the divider at `path` so its line lands on `(x, y)`.
    ///
    /// Returns whether it moved. Panes stop at a dozen columns or three rows
    /// rather than vanishing under the drag.
    pub fn drag_divider(&mut self, path: &[Side], area: Rect, x: u16, y: u16) -> bool {
        let Some(node) = self.node_mut(path) else { return false };
        let Self::Split { dir, ratio, .. } = node else { return false };

        let (span, offset, least) = match dir {
            Dir::Beside => (area.width, x.saturating_sub(area.x), MIN_COLS),
            Dir::Below => (area.height, y.saturating_sub(area.y), MIN_ROWS),
        };
        if span <= least * 2 {
            return false;
        }
        let offset = offset.clamp(least, span - least);
        let next = u16::try_from(u32::from(offset) * 1000 / u32::from(span)).unwrap_or(EVEN);
        let moved = next != *ratio;
        *ratio = next;
        moved
    }

    /// Put the two sides of the node at `path` back to half and half.
    pub fn even(&mut self, path: &[Side]) -> bool {
        match self.node_mut(path) {
            Some(Self::Split { ratio, .. }) => {
                let moved = *ratio != EVEN;
                *ratio = EVEN;
                moved
            }
            _ => false,
        }
    }

    /// The area of the node at `path` within `area`.
    #[must_use]
    pub fn area_of(&self, path: &[Side], area: Rect) -> Option<Rect> {
        let mut node = self;
        let mut rect = area;
        for side in path {
            let Self::Split { dir, ratio, first, second } = node else { return None };
            let (a, b) = halves(rect, *dir, *ratio);
            match side {
                Side::First => {
                    node = first;
                    rect = a;
                }
                Side::Second => {
                    node = second;
                    rect = b;
                }
            }
        }
        Some(rect)
    }

    fn node_mut(&mut self, path: &[Side]) -> Option<&mut Self> {
        let mut node = self;
        for side in path {
            let Self::Split { first, second, .. } = node else { return None };
            node = match side {
                Side::First => first,
                Side::Second => second,
            };
        }
        Some(node)
    }

    fn walk(&self, area: Rect, out: &mut impl FnMut(usize, Rect)) {
        match self {
            Self::Pane(pane) => out(*pane, area),
            Self::Split { dir, ratio, first, second } => {
                let (a, b) = halves(area, *dir, *ratio);
                first.walk(a, out);
                second.walk(b, out);
            }
        }
    }

    fn collect(&self, out: &mut Vec<usize>) {
        match self {
            Self::Pane(pane) => out.push(*pane),
            Self::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    fn walk_dividers(&self, area: Rect, path: &mut Vec<Side>, out: &mut Vec<Divider>) {
        let Self::Split { dir, ratio, first, second } = self else { return };
        let (a, b) = halves(area, *dir, *ratio);
        let line = match dir {
            // The divider is the column or row between the two halves.
            Dir::Beside => Rect::new(b.x.saturating_sub(1), area.y, 1, area.height),
            Dir::Below => Rect::new(area.x, b.y.saturating_sub(1), area.width, 1),
        };
        out.push(Divider { area: line, dir: *dir, path: path.clone() });

        path.push(Side::First);
        first.walk_dividers(a, path, out);
        path.pop();
        path.push(Side::Second);
        second.walk_dividers(b, path, out);
        path.pop();
    }
}

/// Which side of a split a step of a path takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The first: left, or top.
    First,
    /// The second: right, or bottom.
    Second,
}

/// A divider between two panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divider {
    /// The cells it occupies.
    pub area: Rect,
    /// Which way its node divides.
    pub dir: Dir,
    /// How to reach the node it belongs to.
    pub path: Vec<Side>,
}

/// Divide `area` in two, leaving a cell between them for the divider.
fn halves(area: Rect, dir: Dir, ratio: u16) -> (Rect, Rect) {
    match dir {
        Dir::Beside => {
            let split = share(area.width, ratio);
            let first = Rect { width: split, ..area };
            let second =
                Rect { x: area.x + split + 1, width: area.width.saturating_sub(split + 1), ..area };
            (first, second)
        }
        Dir::Below => {
            let split = share(area.height, ratio);
            let first = Rect { height: split, ..area };
            let second = Rect {
                y: area.y + split + 1,
                height: area.height.saturating_sub(split + 1),
                ..area
            };
            (first, second)
        }
    }
}

/// How much of `span` the first side takes, leaving room for the divider and
/// for the other side.
fn share(span: u16, ratio: u16) -> u16 {
    if span < 3 {
        return span.saturating_sub(1);
    }
    let raw = u32::from(span) * u32::from(ratio) / 1000;
    u16::try_from(raw).unwrap_or(0).clamp(1, span - 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };

    #[test]
    fn one_pane_takes_the_whole_screen() {
        let layout = Layout::single(0);
        assert_eq!(layout.rects(SCREEN), vec![(0, SCREEN)]);
        assert!(layout.dividers(SCREEN).is_empty());
    }

    #[test]
    fn a_split_halves_the_screen_with_a_divider_between() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));

        let rects = layout.rects(SCREEN);
        assert_eq!(rects[0].1, Rect::new(0, 0, 40, 24));
        assert_eq!(rects[1].1, Rect::new(41, 0, 39, 24));

        let dividers = layout.dividers(SCREEN);
        assert_eq!(dividers.len(), 1);
        assert_eq!(dividers[0].area, Rect::new(40, 0, 1, 24));
        assert_eq!(dividers[0].dir, Dir::Beside);
    }

    #[test]
    fn splitting_below_divides_the_other_way() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Below, 1, false));
        let rects = layout.rects(SCREEN);
        assert_eq!(rects[0].1, Rect::new(0, 0, 80, 12));
        assert_eq!(rects[1].1, Rect::new(0, 13, 80, 11));
        assert_eq!(layout.dividers(SCREEN)[0].area, Rect::new(0, 12, 80, 1));
    }

    #[test]
    fn splits_nest_arbitrarily() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        assert!(layout.split(1, Dir::Below, 2, false));
        assert!(layout.split(2, Dir::Beside, 3, false));

        assert_eq!(layout.panes(), vec![0, 1, 2, 3]);
        assert_eq!(layout.len(), 4);
        let rects = layout.rects(SCREEN);
        assert_eq!(rects.len(), 4);
        // Nothing overlaps.
        for (index, (_, a)) in rects.iter().enumerate() {
            for (_, b) in rects.iter().skip(index + 1) {
                let overlap = a.intersection(*b);
                assert!(overlap.width == 0 || overlap.height == 0, "{a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn a_new_pane_can_go_first_instead_of_second() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, true));
        assert_eq!(layout.rects(SCREEN)[0].0, 1, "the new one is on the left");
    }

    #[test]
    fn closing_a_pane_gives_its_space_to_its_sibling() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        assert!(layout.split(1, Dir::Below, 2, false));

        assert!(layout.close(2));
        assert_eq!(
            layout.rects(SCREEN),
            vec![(0, Rect::new(0, 0, 40, 24)), (1, Rect::new(41, 0, 39, 24))]
        );

        assert!(layout.close(1));
        assert_eq!(layout.rects(SCREEN), vec![(0, SCREEN)], "and the last split collapses");
    }

    #[test]
    fn the_last_pane_is_never_closed() {
        let mut layout = Layout::single(0);
        assert!(!layout.close(0));
        assert_eq!(layout.panes(), vec![0]);
    }

    #[test]
    fn closing_something_that_is_not_there_changes_nothing() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        assert!(!layout.close(9));
        assert_eq!(layout.len(), 2);
    }

    #[test]
    fn a_point_resolves_to_the_pane_it_is_in() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        assert_eq!(layout.pane_at(SCREEN, 10, 5), Some(0));
        assert_eq!(layout.pane_at(SCREEN, 60, 5), Some(1));
        assert_eq!(layout.pane_at(SCREEN, 40, 5), None, "the divider belongs to neither");
    }

    #[test]
    fn dragging_a_divider_moves_it_and_evening_puts_it_back() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        let path = layout.dividers(SCREEN)[0].path.clone();

        assert!(layout.drag_divider(&path, SCREEN, 20, 5));
        assert_eq!(layout.rects(SCREEN)[0].1.width, 20);

        assert!(layout.even(&path));
        assert_eq!(layout.rects(SCREEN)[0].1.width, 40);
        assert!(!layout.even(&path), "already even");
    }

    #[test]
    fn a_divider_cannot_be_dragged_past_the_minimum_pane() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        let path = layout.dividers(SCREEN)[0].path.clone();

        layout.drag_divider(&path, SCREEN, 0, 5);
        assert!(layout.rects(SCREEN)[0].1.width >= MIN_COLS - 1, "{:?}", layout.rects(SCREEN));
        layout.drag_divider(&path, SCREEN, 79, 5);
        assert!(layout.rects(SCREEN)[1].1.width >= MIN_COLS - 1);
    }

    #[test]
    fn a_nested_divider_is_dragged_within_its_own_area() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        assert!(layout.split(1, Dir::Below, 2, false));

        let nested = layout.dividers(SCREEN).into_iter().find(|d| d.dir == Dir::Below).unwrap();
        let area = layout.area_of(&nested.path, SCREEN).unwrap();
        assert_eq!(area, Rect::new(41, 0, 39, 24));

        layout.drag_divider(&nested.path, area, 60, 18);
        assert_eq!(layout.rect_of(1, SCREEN).unwrap().height, 18);
    }

    #[test]
    fn the_edges_of_a_pane_are_its_outer_quarters() {
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(Edge::at(area, 1, 10), Edge::Left);
        assert_eq!(Edge::at(area, 38, 10), Edge::Right);
        assert_eq!(Edge::at(area, 20, 1), Edge::Top);
        assert_eq!(Edge::at(area, 20, 18), Edge::Bottom);
        assert_eq!(Edge::at(area, 20, 10), Edge::Middle);
        assert_eq!(Edge::at(area, 100, 100), Edge::Middle, "outside is nothing");
    }

    #[test]
    fn a_corner_belongs_to_the_nearer_edge() {
        let area = Rect::new(0, 0, 40, 20);
        // One cell from the left, two from the top: the left edge is nearer.
        assert_eq!(Edge::at(area, 1, 2), Edge::Left);
        assert_eq!(Edge::at(area, 3, 0), Edge::Top);
    }

    #[test]
    fn a_preview_is_the_half_the_drop_would_take() {
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(Edge::Left.preview(area), Rect::new(0, 0, 20, 20));
        assert_eq!(Edge::Right.preview(area), Rect::new(20, 0, 20, 20));
        assert_eq!(Edge::Top.preview(area), Rect::new(0, 0, 40, 10));
        assert_eq!(Edge::Bottom.preview(area), Rect::new(0, 10, 40, 10));
        assert_eq!(Edge::Middle.preview(area), area);
    }

    #[test]
    fn each_edge_says_which_way_it_splits() {
        assert_eq!(Edge::Left.split(), Some((Dir::Beside, true)));
        assert_eq!(Edge::Bottom.split(), Some((Dir::Below, false)));
        assert_eq!(Edge::Middle.split(), None);
    }

    #[test]
    fn a_tiny_area_still_gives_every_pane_something() {
        let mut layout = Layout::single(0);
        assert!(layout.split(0, Dir::Beside, 1, false));
        for width in 1..8u16 {
            let area = Rect::new(0, 0, width, 3);
            let rects = layout.rects(area);
            assert_eq!(rects.len(), 2);
            for (_, rect) in rects {
                assert!(rect.width <= width);
            }
        }
    }
}
