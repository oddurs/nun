//! "What is at this cell?"
//!
//! The layout pass pushes a region for everything it lays out, in paint order:
//! later regions sit on top of earlier ones, exactly as they are drawn. A point
//! then resolves to the topmost region containing it — which is how an overlay
//! such as the palette captures a click that lands above the editor beneath it.
//!
//! Resolution has to be cheap, because with hover tracking on it runs for every
//! cell the pointer crosses. So the map is flattened as regions are pushed:
//! each row keeps a sorted list of disjoint spans, each owned by the topmost
//! region covering it. A lookup is one row index and one binary search —
//! `O(log n)` in the number of regions on that row, whatever the overlap.

use crate::geometry::Rect;

/// A region the layout laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region<T> {
    area: Rect,
    target: T,
    hover: bool,
}

/// A run of cells on one row owned by one region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: u16,
    end: u16,
    region: usize,
}

/// What a point resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit<T> {
    /// The owner of the cell.
    pub target: T,
    /// The whole region, so the owner can turn the point into its own
    /// coordinates.
    pub area: Rect,
    /// Whether the owner reacts to hover.
    pub hover: bool,
}

/// Every region on screen, resolvable by cell.
#[derive(Debug, Clone)]
pub struct HitMap<T> {
    bounds: Rect,
    regions: Vec<Region<T>>,
    rows: Vec<Vec<Span>>,
    hover_regions: usize,
}

impl<T: Copy> HitMap<T> {
    /// An empty map for a screen of `bounds`.
    #[must_use]
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            regions: Vec::new(),
            rows: vec![Vec::new(); usize::from(bounds.height)],
            hover_regions: 0,
        }
    }

    /// Lay out a region on top of everything pushed so far.
    ///
    /// The part outside the screen is dropped. A region pushed with `hover`
    /// gets enter and leave events, and its presence is what turns the
    /// terminal's any-motion reporting on.
    pub fn push(&mut self, area: Rect, target: T, hover: bool) {
        let area = area.intersection(self.bounds);
        if area.is_empty() {
            return;
        }

        let index = self.regions.len();
        self.regions.push(Region { area, target, hover });
        if hover {
            self.hover_regions += 1;
        }

        for y in area.y..area.bottom() {
            let row = &mut self.rows[usize::from(y - self.bounds.y)];
            paint(row, Span { start: area.x, end: area.right(), region: index });
        }
    }

    /// The topmost region at `(x, y)`, if any.
    #[must_use]
    pub fn at(&self, x: u16, y: u16) -> Option<Hit<T>> {
        if !self.bounds.contains(x, y) {
            return None;
        }
        let row = &self.rows[usize::from(y - self.bounds.y)];
        let index = row.partition_point(|span| span.end <= x);
        let span = row.get(index).filter(|span| span.start <= x)?;
        let region = &self.regions[span.region];
        Some(Hit { target: region.target, area: region.area, hover: region.hover })
    }

    /// Whether anything laid out reacts to hover.
    ///
    /// Any-motion reporting floods the input stream, so it is only worth
    /// turning on while this is true.
    #[must_use]
    pub const fn has_hover_targets(&self) -> bool {
        self.hover_regions > 0
    }

    /// The area of the first region pushed for `target`.
    #[must_use]
    pub fn area_of(&self, target: T) -> Option<Rect>
    where
        T: PartialEq,
    {
        self.regions.iter().find(|region| region.target == target).map(|region| region.area)
    }
}

/// Paint `new` over a row of sorted, disjoint spans, keeping it that way.
fn paint(row: &mut Vec<Span>, new: Span) {
    // Spans wholly before and wholly after `new` are untouched; everything in
    // between is either covered, or clipped to the part that still shows.
    let first = row.partition_point(|span| span.end <= new.start);
    let last = row.partition_point(|span| span.start < new.end);

    let mut replacement = Vec::with_capacity(3);
    if let Some(left) = row.get(first).filter(|span| span.start < new.start && first < last) {
        replacement.push(Span { end: new.start, ..*left });
    }
    replacement.push(new);
    if last > first
        && let Some(right) = row.get(last - 1).filter(|span| span.end > new.end)
    {
        replacement.push(Span { start: new.end, ..*right });
    }

    row.splice(first..last, replacement);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        Editor,
        Status,
        Palette,
        Row(u16),
    }

    fn screen() -> HitMap<Target> {
        let mut map = HitMap::new(Rect::new(0, 0, 80, 24));
        map.push(Rect::new(0, 0, 80, 23), Target::Editor, false);
        map.push(Rect::new(0, 23, 80, 1), Target::Status, false);
        map
    }

    #[test]
    fn a_point_resolves_to_the_region_under_it() {
        let map = screen();
        assert_eq!(map.at(10, 5).map(|hit| hit.target), Some(Target::Editor));
        assert_eq!(map.at(10, 23).map(|hit| hit.target), Some(Target::Status));
    }

    #[test]
    fn a_point_off_screen_resolves_to_nothing() {
        let map = screen();
        assert_eq!(map.at(80, 0), None);
        assert_eq!(map.at(0, 24), None);
    }

    #[test]
    fn an_overlay_captures_clicks_above_what_is_beneath_it() {
        let mut map = screen();
        map.push(Rect::new(20, 2, 40, 10), Target::Palette, false);

        assert_eq!(map.at(30, 5).map(|hit| hit.target), Some(Target::Palette));
        assert_eq!(map.at(19, 5).map(|hit| hit.target), Some(Target::Editor), "just outside");
        assert_eq!(map.at(60, 5).map(|hit| hit.target), Some(Target::Editor), "right edge");
    }

    #[test]
    fn rows_inside_an_overlay_sit_above_the_overlay_itself() {
        let mut map = screen();
        map.push(Rect::new(20, 2, 40, 10), Target::Palette, false);
        map.push(Rect::new(21, 4, 38, 1), Target::Row(0), true);

        assert_eq!(map.at(30, 4).map(|hit| hit.target), Some(Target::Row(0)));
        assert_eq!(map.at(20, 4).map(|hit| hit.target), Some(Target::Palette), "the border");
    }

    #[test]
    fn a_hit_carries_the_whole_area_for_local_coordinates() {
        let mut map = screen();
        map.push(Rect::new(21, 4, 38, 1), Target::Row(0), true);
        let hit = map.at(30, 4).unwrap();
        assert_eq!(hit.area, Rect::new(21, 4, 38, 1));
        assert!(hit.hover);
    }

    #[test]
    fn regions_are_clipped_to_the_screen() {
        let mut map = HitMap::new(Rect::new(0, 0, 10, 10));
        map.push(Rect::new(5, 5, 100, 100), Target::Editor, false);
        assert_eq!(map.at(9, 9).map(|hit| hit.area), Some(Rect::new(5, 5, 5, 5)));
    }

    #[test]
    fn hover_tracking_is_only_wanted_when_a_hover_target_is_laid_out() {
        let mut map = screen();
        assert!(!map.has_hover_targets());
        map.push(Rect::new(0, 0, 10, 1), Target::Row(3), true);
        assert!(map.has_hover_targets());
    }

    #[test]
    fn a_hover_target_pushed_off_screen_does_not_count() {
        let mut map = screen();
        map.push(Rect::new(200, 0, 10, 1), Target::Row(3), true);
        assert!(!map.has_hover_targets(), "nothing on screen reacts to hover");
    }

    fn brute_force(regions: &[(Rect, usize)], bounds: Rect, x: u16, y: u16) -> Option<usize> {
        if !bounds.contains(x, y) {
            return None;
        }
        regions.iter().rev().find(|(area, _)| area.contains(x, y)).map(|(_, id)| *id)
    }

    fn rect() -> impl Strategy<Value = Rect> {
        (0u16..40, 0u16..12, 0u16..30, 0u16..8).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    proptest! {
        #[test]
        fn resolution_matches_the_topmost_containing_region(
            areas in prop::collection::vec(rect(), 0..24),
            x in 0u16..45,
            y in 0u16..14,
        ) {
            let bounds = Rect::new(0, 0, 40, 12);
            let mut map = HitMap::new(bounds);
            let mut pushed = Vec::new();
            for (id, area) in areas.iter().enumerate() {
                map.push(*area, id, false);
                pushed.push((*area, id));
            }
            prop_assert_eq!(map.at(x, y).map(|hit| hit.target), brute_force(&pushed, bounds, x, y));
        }

        #[test]
        fn every_row_stays_sorted_and_disjoint(areas in prop::collection::vec(rect(), 0..24)) {
            let mut map = HitMap::new(Rect::new(0, 0, 40, 12));
            for (id, area) in areas.iter().enumerate() {
                map.push(*area, id, false);
            }
            for row in &map.rows {
                for pair in row.windows(2) {
                    prop_assert!(pair[0].end <= pair[1].start, "{row:?}");
                }
                for span in row {
                    prop_assert!(span.start < span.end, "{row:?}");
                }
            }
        }
    }
}
