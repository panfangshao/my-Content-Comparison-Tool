//! Vertical geometry for the two panes.
//!
//! Both panes scroll as one list of *diff rows*, so row `i` is at the same `y`
//! on the left as on the right - that is what makes the comparison line up.
//!
//! # Uniform vs. wrapped
//!
//! With word wrap off every row is one text line tall, so "which row is at
//! `y`?" is a division and the whole structure collapses to a couple of
//! multiplications. That is the default and the fast path.
//!
//! With word wrap on a row is as tall as the *taller* of its two sides, and we
//! cannot know that without laying the text out. Laying out 100 000 rows to
//! draw 40 of them defeats the purpose, so instead:
//!
//! * every row starts out assumed to be one line tall;
//! * rows are measured as they are drawn, and their true height is folded in;
//! * a Fenwick tree keeps prefix sums correct under those point updates, so
//!   both "row at `y`" and "`y` of row" stay O(log n).
//!
//! The visible consequence is that the scrollbar on a freshly opened wrapped
//! document is an estimate that tightens as you scroll through it - the same
//! behaviour as every other editor that wraps large files.
//!
//! # Two panes, one geometry
//!
//! Both panes share one layout, which is what guarantees that row `i` sits at
//! the same `y` on both sides. Two rules keep that guarantee under wrapping:
//!
//! * A row is as tall as the **taller** of its two sides. Each side's
//!   measurement is stored separately, so whichever pane draws second cannot
//!   overwrite what the first one reported.
//! * Measurements taken while drawing apply to the **next** frame. Changing a
//!   row's height mid-frame means the pane drawn first laid itself out against
//!   different geometry from the pane drawn second - which is exactly how the
//!   two columns' line numbers drifted apart.

use crate::core::diff::Side;

/// Fenwick (binary indexed) tree over row heights, measured in text lines.
///
/// `u32` per row: a 100k-row document costs 400 KB, and no single row can
/// plausibly wrap to more than `u32::MAX` lines.
#[derive(Clone, Debug, Default)]
struct Fenwick {
    /// 1-based internal storage; `tree[0]` is unused.
    tree: Vec<u32>,
    /// Per-row values, kept alongside so a point update knows the delta.
    values: Vec<u32>,
}

impl Fenwick {
    fn new(len: usize, initial: u32) -> Self {
        let mut f = Self {
            tree: vec![0; len + 1],
            values: vec![initial; len],
        };
        // Build in O(n) rather than n insertions of O(log n).
        for i in 1..=len {
            f.tree[i] += initial;
            let parent = i + (i & i.wrapping_neg());
            if parent <= len {
                let carried = f.tree[i];
                f.tree[parent] += carried;
            }
        }
        f
    }

    /// Rebuild from scratch. Used when a whole side's measurements are thrown
    /// away at once, which is cheaper than n point updates.
    fn rebuild(&mut self, values: Vec<u32>) {
        let len = values.len();
        self.tree = vec![0; len + 1];
        for i in 1..=len {
            self.tree[i] += values[i - 1];
            let parent = i + (i & i.wrapping_neg());
            if parent <= len {
                let carried = self.tree[i];
                self.tree[parent] += carried;
            }
        }
        self.values = values;
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn get(&self, row: usize) -> u32 {
        self.values[row]
    }

    /// Sum of rows `0..row`.
    fn prefix(&self, row: usize) -> u64 {
        let mut i = row.min(self.values.len());
        let mut sum = 0u64;
        while i > 0 {
            sum += self.tree[i] as u64;
            i -= i & i.wrapping_neg();
        }
        sum
    }

    fn total(&self) -> u64 {
        self.prefix(self.values.len())
    }

    fn set(&mut self, row: usize, value: u32) {
        let old = self.values[row];
        if old == value {
            return;
        }
        self.values[row] = value;
        let delta = value as i64 - old as i64;
        let mut i = row + 1;
        while i < self.tree.len() {
            self.tree[i] = (self.tree[i] as i64 + delta) as u32;
            i += i & i.wrapping_neg();
        }
    }

    /// Largest `row` such that `prefix(row) <= target`.
    ///
    /// Standard Fenwick descent: O(log n) with no allocation.
    fn upper_bound(&self, target: u64) -> usize {
        let n = self.values.len();
        if n == 0 {
            return 0;
        }
        let mut pos = 0usize;
        let mut remaining = target;
        let mut step = (n + 1).next_power_of_two() / 2;
        while step > 0 {
            let next = pos + step;
            if next <= n && (self.tree[next] as u64) <= remaining {
                remaining -= self.tree[next] as u64;
                pos = next;
            }
            step /= 2;
        }
        pos
    }
}

/// Vertical layout of the row list.
pub struct RowLayout {
    /// `None` when every row is exactly one line tall (word wrap off).
    heights: Option<Wrapped>,
    row_count: usize,
}

/// Height bookkeeping for wrapped rows.
struct Wrapped {
    /// What every pane reads while drawing: the taller of the two sides.
    active: Fenwick,
    /// Per-side measurements. Kept apart so the pane drawn second cannot
    /// clobber what the first one reported.
    sides: [Vec<u32>; 2],
    /// Wrap width each side last reported, so a resize invalidates only that
    /// side's measurements.
    widths: [f32; 2],
    /// Rows measured since the last commit. Only visible rows ever land here,
    /// so this stays tiny.
    pending: Vec<usize>,
}

impl Wrapped {
    /// Recompute every row's height from both sides at once.
    fn rebuild_active(&mut self) {
        let combined: Vec<u32> = self.sides[0]
            .iter()
            .zip(&self.sides[1])
            .map(|(a, b)| (*a).max(*b))
            .collect();
        self.active.rebuild(combined);
        self.pending.clear();
    }
}

#[inline]
fn side_index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

impl Default for RowLayout {
    fn default() -> Self {
        Self::uniform(0)
    }
}

impl RowLayout {
    /// Every row one line tall.
    pub fn uniform(row_count: usize) -> Self {
        Self {
            heights: None,
            row_count,
        }
    }

    /// Rows may wrap; all start out assumed to be one line tall.
    pub fn wrapped(row_count: usize) -> Self {
        Self {
            heights: Some(Wrapped {
                active: Fenwick::new(row_count, 1),
                sides: [vec![1; row_count], vec![1; row_count]],
                widths: [f32::NAN, f32::NAN],
                pending: Vec::new(),
            }),
            row_count,
        }
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn is_uniform(&self) -> bool {
        self.heights.is_none()
    }

    /// Rebuild for a new row count / wrap setting, keeping measurements when
    /// nothing that invalidates them changed.
    pub fn reconfigure(&mut self, row_count: usize, wrap: bool) {
        let shape_changed = row_count != self.row_count || wrap == self.is_uniform();
        if !shape_changed {
            return;
        }
        *self = if wrap {
            Self::wrapped(row_count)
        } else {
            Self::uniform(row_count)
        };
    }

    /// Record that `side` renders `row` as `lines` visual lines.
    ///
    /// Takes effect at the next [`Self::commit_measurements`], not straight
    /// away: moving a row while the frame is being painted would shift rows
    /// that are already on screen.
    ///
    /// Ignored in uniform mode, where a row is one line tall by definition.
    pub fn measure(&mut self, side: Side, row: usize, lines: u32) {
        let Some(w) = &mut self.heights else {
            return;
        };
        let slot = &mut w.sides[side_index(side)];
        if row >= slot.len() {
            return;
        }
        let lines = lines.max(1);
        if slot[row] != lines {
            slot[row] = lines;
            w.pending.push(row);
        }
    }

    /// Tell the layout what width `side` wraps at.
    ///
    /// A different width invalidates that side's measurements - every line
    /// could now wrap somewhere else - and leaves the other side's alone.
    pub fn set_wrap_width(&mut self, side: Side, width: f32) {
        let Some(w) = &mut self.heights else {
            return;
        };
        let i = side_index(side);
        // A sub-pixel change cannot move where a line wraps.
        if (w.widths[i] - width).abs() < 0.5 {
            return;
        }
        w.widths[i] = width;
        w.sides[i].iter_mut().for_each(|h| *h = 1);
        w.rebuild_active();
    }

    /// Fold the measurements taken while drawing into the geometry the next
    /// frame reads. Call once per frame, after every pane has been drawn.
    pub fn commit_measurements(&mut self) {
        let Some(w) = &mut self.heights else {
            return;
        };
        if w.pending.is_empty() {
            return;
        }
        for row in std::mem::take(&mut w.pending) {
            let combined = w.sides[0][row].max(w.sides[1][row]);
            w.active.set(row, combined);
        }
    }

    /// How many visual lines `row` occupies.
    pub fn lines_at(&self, row: usize) -> u32 {
        match &self.heights {
            Some(w) if row < w.active.len() => w.active.get(row),
            _ => 1,
        }
    }

    /// Total content height in points.
    pub fn total_height(&self, line_height: f32) -> f32 {
        let lines = match &self.heights {
            Some(w) => w.active.total(),
            None => self.row_count as u64,
        };
        lines as f32 * line_height
    }

    /// `y` of the top of `row`, relative to the top of the content.
    pub fn row_top(&self, row: usize, line_height: f32) -> f32 {
        let lines = match &self.heights {
            Some(w) => w.active.prefix(row),
            None => row.min(self.row_count) as u64,
        };
        lines as f32 * line_height
    }

    /// Height of `row` in points.
    pub fn row_height(&self, row: usize, line_height: f32) -> f32 {
        self.lines_at(row) as f32 * line_height
    }

    /// The row containing `y`, clamped into range.
    pub fn row_at(&self, y: f32, line_height: f32) -> usize {
        if self.row_count == 0 || line_height <= 0.0 {
            return 0;
        }
        let target = (y / line_height).floor().max(0.0);
        let row = match &self.heights {
            Some(w) => w.active.upper_bound(target as u64),
            None => target as usize,
        };
        row.min(self.row_count.saturating_sub(1))
    }

    /// Rows intersecting the vertical span `y_range`, padded by one row on each
    /// side so partially visible rows still paint.
    pub fn visible_rows(&self, y_range: std::ops::Range<f32>, line_height: f32) -> std::ops::Range<usize> {
        if self.row_count == 0 {
            return 0..0;
        }
        let first = self.row_at(y_range.start, line_height);
        let mut last = self.row_at(y_range.end, line_height) + 2;
        last = last.min(self.row_count);
        first..last.max(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::Side;

    const LH: f32 = 20.0;

    /// Measure and publish in one step. Production defers the commit to the
    /// end of the frame; most tests only care about the resulting geometry.
    fn measure_now(l: &mut RowLayout, side: Side, row: usize, lines: u32) {
        l.measure(side, row, lines);
        l.commit_measurements();
    }

    #[test]
    fn uniform_layout_is_pure_arithmetic() {
        let l = RowLayout::uniform(100);
        assert!(l.is_uniform());
        assert_eq!(l.total_height(LH), 2000.0);
        assert_eq!(l.row_top(10, LH), 200.0);
        assert_eq!(l.row_at(205.0, LH), 10);
        assert_eq!(l.row_height(3, LH), LH);
    }

    #[test]
    fn uniform_layout_ignores_measurements() {
        let mut l = RowLayout::uniform(10);
        measure_now(&mut l, Side::Left, 3, 5);
        assert_eq!(l.lines_at(3), 1);
        assert_eq!(l.total_height(LH), 200.0);
    }

    #[test]
    fn wrapped_layout_starts_out_uniform() {
        let l = RowLayout::wrapped(50);
        assert!(!l.is_uniform());
        assert_eq!(l.total_height(LH), 1000.0);
        assert_eq!(l.row_top(25, LH), 500.0);
    }

    #[test]
    fn measuring_a_row_shifts_everything_below_it() {
        let mut l = RowLayout::wrapped(10);
        measure_now(&mut l, Side::Left, 2, 3);
        assert_eq!(l.lines_at(2), 3);
        assert_eq!(l.row_top(2, LH), 40.0, "rows above are unaffected");
        assert_eq!(l.row_top(3, LH), 100.0, "row 2 now occupies three lines");
        assert_eq!(l.total_height(LH), 12.0 * LH);
    }

    #[test]
    fn row_at_and_row_top_are_inverses() {
        let mut l = RowLayout::wrapped(200);
        for row in (0..200).step_by(7) {
            l.measure(Side::Left, row, (row % 5 + 1) as u32);
        }
        l.commit_measurements();
        for row in 0..200 {
            let top = l.row_top(row, LH);
            assert_eq!(l.row_at(top, LH), row, "row {row} did not round trip");
            // Anywhere inside the row must also resolve to it.
            let mid = top + l.row_height(row, LH) / 2.0;
            assert_eq!(l.row_at(mid, LH), row, "midpoint of row {row} missed");
        }
    }

    #[test]
    fn prefix_sums_match_a_naive_walk() {
        let mut l = RowLayout::wrapped(300);
        let mut expected = vec![1u32; 300];
        for (i, e) in expected.iter_mut().enumerate() {
            let h = ((i * 37) % 6 + 1) as u32;
            *e = h;
            l.measure(Side::Left, i, h);
        }
        l.commit_measurements();
        let mut running = 0u64;
        for (row, h) in expected.iter().enumerate() {
            assert_eq!(
                l.row_top(row, 1.0),
                running as f32,
                "prefix diverged at row {row}"
            );
            running += *h as u64;
        }
        assert_eq!(l.total_height(1.0), running as f32);
    }

    #[test]
    fn remeasuring_a_row_replaces_rather_than_accumulates() {
        let mut l = RowLayout::wrapped(5);
        measure_now(&mut l, Side::Left, 1, 4);
        measure_now(&mut l, Side::Left, 1, 2);
        assert_eq!(l.lines_at(1), 2);
        assert_eq!(l.total_height(1.0), 6.0);
    }

    #[test]
    fn a_measurement_of_zero_is_clamped_to_one_line() {
        let mut l = RowLayout::wrapped(3);
        measure_now(&mut l, Side::Left, 0, 0);
        assert_eq!(l.lines_at(0), 1);
    }

    #[test]
    fn visible_rows_covers_the_viewport_with_margin() {
        let l = RowLayout::uniform(1000);
        let r = l.visible_rows(100.0..300.0, LH);
        assert!(r.start <= 5, "must include the first partially visible row");
        assert!(r.end >= 15, "must include the last partially visible row");
        assert!(r.end <= 1000);
    }

    #[test]
    fn visible_rows_is_clamped_at_the_end_of_the_document() {
        let l = RowLayout::uniform(10);
        let r = l.visible_rows(0.0..10_000.0, LH);
        assert_eq!(r, 0..10);
    }

    #[test]
    fn empty_layout_is_safe() {
        let l = RowLayout::uniform(0);
        assert_eq!(l.total_height(LH), 0.0);
        assert_eq!(l.row_at(500.0, LH), 0);
        assert_eq!(l.visible_rows(0.0..100.0, LH), 0..0);
    }

    #[test]
    fn reconfigure_only_rebuilds_when_the_shape_changes() {
        let mut l = RowLayout::wrapped(10);
        measure_now(&mut l, Side::Left, 4, 7);
        l.reconfigure(10, true);
        assert_eq!(l.lines_at(4), 7, "an identical reconfigure kept measurements");

        l.reconfigure(10, false);
        assert!(l.is_uniform());
        l.reconfigure(20, false);
        assert_eq!(l.row_count(), 20);
    }

    #[test]
    fn out_of_range_access_does_not_panic() {
        let mut l = RowLayout::wrapped(5);
        measure_now(&mut l, Side::Left, 99, 3);
        assert_eq!(l.lines_at(99), 1);
        assert_eq!(l.row_at(f32::MAX, LH), 4);
        assert_eq!(l.row_top(99, LH), l.total_height(LH));
    }

    /// A row is as tall as its taller side, whichever pane reports first.
    ///
    /// This is the bug that made the two columns' line numbers drift apart:
    /// each pane used to write the shared height directly, so whichever drew
    /// second simply overwrote the other's measurement.
    #[test]
    fn a_row_takes_the_taller_of_its_two_sides() {
        for (first, second) in [(Side::Left, Side::Right), (Side::Right, Side::Left)] {
            let mut l = RowLayout::wrapped(5);
            l.measure(first, 2, 3);
            l.measure(second, 2, 1);
            l.commit_measurements();
            assert_eq!(
                l.lines_at(2),
                3,
                "{second:?} drew second and flattened {first:?}'s three lines"
            );
        }
    }

    #[test]
    fn the_shorter_side_cannot_shrink_a_row_later() {
        let mut l = RowLayout::wrapped(4);
        measure_now(&mut l, Side::Left, 1, 4);
        assert_eq!(l.lines_at(1), 4);

        // A later frame where only the right pane reports must not undo it.
        measure_now(&mut l, Side::Right, 1, 1);
        assert_eq!(l.lines_at(1), 4);
    }

    /// Both panes must see identical geometry *while* a frame is drawn.
    ///
    /// Measuring used to take effect immediately, so the pane drawn first laid
    /// itself out against one set of heights and the pane drawn second against
    /// another - and row `i` landed at a different `y` in each column.
    #[test]
    fn measuring_during_a_frame_does_not_move_rows_already_drawn() {
        let mut l = RowLayout::wrapped(10);

        // Frame 1: the left pane draws, reading row tops as it goes, and
        // reports that row 2 wraps to four lines.
        let before: Vec<f32> = (0..10).map(|r| l.row_top(r, LH)).collect();
        l.measure(Side::Left, 2, 4);

        // The right pane now draws in the same frame. It must see exactly what
        // the left pane saw.
        let during: Vec<f32> = (0..10).map(|r| l.row_top(r, LH)).collect();
        assert_eq!(before, during, "geometry shifted mid-frame");

        // Only after the frame ends does the new height apply: rows 0 and 1
        // are one line each, row 2 is now four.
        l.commit_measurements();
        assert_eq!(l.row_top(3, LH), 6.0 * LH);
    }

    #[test]
    fn a_width_change_invalidates_only_that_side() {
        let mut l = RowLayout::wrapped(4);
        l.set_wrap_width(Side::Left, 400.0);
        l.set_wrap_width(Side::Right, 400.0);
        measure_now(&mut l, Side::Left, 0, 3);
        measure_now(&mut l, Side::Right, 1, 5);

        // The left pane is resized; its wrapping is now unknown, but the right
        // pane's measurements are still good.
        l.set_wrap_width(Side::Left, 250.0);
        assert_eq!(l.lines_at(0), 1, "the resized side should be re-measured");
        assert_eq!(l.lines_at(1), 5, "the untouched side should be preserved");
    }

    #[test]
    fn an_unchanged_width_keeps_the_measurements() {
        let mut l = RowLayout::wrapped(3);
        l.set_wrap_width(Side::Left, 300.0);
        measure_now(&mut l, Side::Left, 0, 4);
        l.set_wrap_width(Side::Left, 300.2); // sub-pixel jitter
        assert_eq!(l.lines_at(0), 4);
    }

    /// The whole point: with the same content and different pane widths, both
    /// columns still agree on where every row starts.
    #[test]
    fn two_panes_of_different_widths_stay_row_aligned() {
        let mut l = RowLayout::wrapped(200);
        l.set_wrap_width(Side::Left, 520.0);
        l.set_wrap_width(Side::Right, 480.0);

        // The narrower right pane wraps more lines on the long rows.
        for row in 0..200 {
            let long = row % 7 == 0;
            l.measure(Side::Left, row, if long { 2 } else { 1 });
            l.measure(Side::Right, row, if long { 3 } else { 1 });
        }
        l.commit_measurements();

        // One shared geometry, so a row is at one `y`. Check it is the tall
        // one, or the wider pane's text would overlap the row beneath it.
        for row in (0..200).step_by(7) {
            assert_eq!(l.lines_at(row), 3, "row {row} was not given the taller height");
        }
        let expected: f32 = (0..200).map(|r| if r % 7 == 0 { 3.0 } else { 1.0 }).sum();
        assert_eq!(l.total_height(1.0), expected);
    }

    /// The Fenwick descent has an off-by-one trap at power-of-two sizes.
    #[test]
    fn fenwick_descent_is_correct_at_awkward_sizes() {
        for n in [1usize, 2, 3, 4, 7, 8, 9, 15, 16, 17, 63, 64, 65] {
            let mut l = RowLayout::wrapped(n);
            for i in 0..n {
                l.measure(Side::Left, i, (i % 3 + 1) as u32);
            }
            l.commit_measurements();
            for row in 0..n {
                let top = l.row_top(row, 1.0);
                assert_eq!(l.row_at(top, 1.0), row, "n={n} row={row}");
            }
        }
    }
}
