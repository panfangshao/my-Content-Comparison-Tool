//! Where the two panes are looking.
//!
//! Both panes scroll as one, so "the view" is a single thing even though two
//! widgets draw it. This module owns that thing: the shared offset, which pane
//! is currently leading, and any jump the application has asked for.
//!
//! # Why this is one module
//!
//! This started as five fields on the application struct, each added while
//! fixing a different bug, with the logic that used them spread across the
//! frame loop. Four separate defects came out of that arrangement:
//!
//! * moving the caret off screen did not bring the view along;
//! * a jump request was only cleared by the right pane, so single-document
//!   mode - which never draws a right pane - pinned the view permanently;
//! * a re-comparison shifted rows above the viewport and slid the text out
//!   from under the reader;
//! * switching the highlight granularity re-compared and did the same.
//!
//! None of those were hard problems. They were hard to *see*, because no
//! single place could answer "where is the view, and what is about to move
//! it?". Now one can, and it answers without an `egui` context, so the
//! behaviour is testable as ordinary logic.

use egui::Vec2;

use crate::core::diff::{DiffResult, Side};
use crate::ui::rowlayout::RowLayout;

/// How much of the viewport a jump should use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reveal {
    /// Put the target near the middle, so there is context above and below.
    /// Used when jumping somewhere the reader has not been.
    Centre,
    /// Move as little as possible. Used to keep the caret on screen, where a
    /// larger jump would be disorienting.
    Minimal,
}

/// A line the view is holding on to across a rebuild of the row list.
///
/// Captured before the comparison is re-run and handed back afterwards; see
/// [`Viewport::capture_anchor`].
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    side: Side,
    line: usize,
    /// Distance from the top of the anchored row to the top of the viewport,
    /// so the view holds still to the pixel rather than to the nearest row.
    offset_into_row: f32,
}

#[derive(Default)]
pub struct Viewport {
    /// The offset both panes share.
    offset: Vec2,
    /// Which pane last moved. The other one follows it.
    leader: Option<Side>,
    /// An offset every pane must adopt on the next frame.
    pending: Option<Vec2>,
}

impl Viewport {
    pub fn offset(&self) -> Vec2 {
        self.offset
    }

    /// Reset to the top, for when the row list changes shape entirely.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // ---- What a pane should do this frame -------------------------------

    /// The offset `side` must be forced to, or `None` to leave it alone.
    ///
    /// `sync` is the user's "synchronised scrolling" setting; a pending jump
    /// ignores it, because a jump is an explicit instruction rather than one
    /// pane following another.
    pub fn forced_offset(&self, side: Side, sync: bool) -> Option<Vec2> {
        if let Some(pending) = self.pending {
            return Some(pending);
        }
        if !sync {
            return None;
        }
        match self.leader {
            Some(leader) if leader != side => Some(self.offset),
            _ => None,
        }
    }

    /// Record where each pane ended up after drawing, and decide who leads.
    ///
    /// `None` for a pane that was not drawn at all, which is how
    /// single-document mode reports.
    pub fn observe(&mut self, left: Option<Vec2>, right: Option<Vec2>, previous: [Vec2; 2]) {
        let moved = |now: Option<Vec2>, before: Vec2| {
            now.is_some_and(|now| (now - before).length() > 0.5)
        };

        self.leader = if moved(left, previous[0]) {
            Some(Side::Left)
        } else if moved(right, previous[1]) {
            Some(Side::Right)
        } else {
            None
        };

        if let Some(offset) = match self.leader {
            Some(Side::Left) => left,
            Some(Side::Right) => right,
            // Nobody scrolled; in single-document mode the one pane still
            // defines the view.
            None => left.filter(|_| right.is_none()),
        } {
            self.offset = offset;
        }
    }

    /// Consume this frame's jump request.
    ///
    /// Called once per frame by the application, not by whichever pane happens
    /// to read it last: single-document mode draws only one pane, and letting
    /// the second one do the clearing left the request set forever.
    pub fn end_frame(&mut self) {
        if let Some(offset) = self.pending.take() {
            self.offset = offset;
        }
    }

    // ---- Asking the view to move ----------------------------------------

    /// Bring `row` into view on the next frame.
    pub fn scroll_to_row(
        &mut self,
        row: usize,
        reveal: Reveal,
        layout: &RowLayout,
        line_height: f32,
        viewport_height: f32,
    ) {
        let top = layout.row_top(row, line_height);
        let bottom = top + layout.row_height(row, line_height);

        let y = match reveal {
            Reveal::Centre => (top - viewport_height / 2.0).max(0.0),
            Reveal::Minimal => {
                if top < self.offset.y {
                    top
                } else if bottom > self.offset.y + viewport_height {
                    (bottom - viewport_height).max(0.0)
                } else {
                    // Already on screen; moving would only be a distraction.
                    return;
                }
            }
        };
        self.pending = Some(Vec2::new(self.offset.x, y));
    }

    // ---- Holding still across a re-comparison ----------------------------

    /// Note the line at the top of the view, before the row list is rebuilt.
    ///
    /// A comparison can add or remove filler rows *above* the viewport, and the
    /// offset is in pixels rather than lines, so without this the text slides
    /// out from under whatever the reader was looking at.
    pub fn capture_anchor(
        &self,
        diff: &DiffResult,
        layout: &RowLayout,
        side: Side,
        line_height: f32,
    ) -> Option<Anchor> {
        let row = layout.row_at(self.offset.y, line_height);
        let line = diff.rows.get(row)?.side(side)?;
        Some(Anchor {
            side,
            line,
            offset_into_row: self.offset.y - layout.row_top(row, line_height),
        })
    }

    /// Put the anchored line back where it was, against the rebuilt row list.
    pub fn restore_anchor(
        &mut self,
        anchor: Option<Anchor>,
        diff: &DiffResult,
        layout: &RowLayout,
        line_height: f32,
    ) {
        let Some(anchor) = anchor else {
            return;
        };
        let Some(row) = diff.row_of(anchor.side, anchor.line) else {
            return;
        };
        let y = (layout.row_top(row, line_height) + anchor.offset_into_row).max(0.0);
        if (y - self.offset.y).abs() > 0.5 {
            self.offset.y = y;
            self.pending = Some(self.offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{Budget, DiffOptions, diff_lines};

    const LH: f32 = 20.0;
    const VIEW: f32 = 400.0;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    fn compare(a: &[String], b: &[String]) -> DiffResult {
        diff_lines(a, b, &DiffOptions::default(), Budget::unlimited())
    }

    fn layout_for(d: &DiffResult) -> RowLayout {
        RowLayout::uniform(d.rows.len())
    }

    /// Drive a frame: panes report, the app ends the frame.
    fn frame(v: &mut Viewport, left: Vec2, right: Vec2, previous: [Vec2; 2]) {
        v.observe(Some(left), Some(right), previous);
        v.end_frame();
    }

    #[test]
    fn a_pane_that_scrolls_becomes_the_leader() {
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 200.0), Vec2::ZERO, [Vec2::ZERO; 2]);

        assert_eq!(v.offset().y, 200.0);
        assert_eq!(v.forced_offset(Side::Right, true), Some(Vec2::new(0.0, 200.0)));
        assert_eq!(v.forced_offset(Side::Left, true), None, "the leader is not forced");
    }

    #[test]
    fn nothing_is_forced_when_sync_is_off() {
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 200.0), Vec2::ZERO, [Vec2::ZERO; 2]);
        assert_eq!(v.forced_offset(Side::Right, false), None);
    }

    #[test]
    fn a_jump_is_obeyed_even_with_sync_off() {
        let d = compare(&lines(100), &lines(100));
        let l = layout_for(&d);
        let mut v = Viewport::default();

        v.scroll_to_row(50, Reveal::Centre, &l, LH, VIEW);
        // A jump is an instruction, not one pane trailing another.
        assert!(v.forced_offset(Side::Left, false).is_some());
        assert!(v.forced_offset(Side::Right, false).is_some());
    }

    /// Single-document mode draws one pane. The request must still clear.
    #[test]
    fn a_jump_clears_even_when_only_one_pane_is_drawn() {
        let d = compare(&lines(100), &lines(100));
        let l = layout_for(&d);
        let mut v = Viewport::default();

        v.scroll_to_row(40, Reveal::Centre, &l, LH, VIEW);
        assert!(v.forced_offset(Side::Left, true).is_some());

        v.observe(Some(Vec2::new(0.0, 600.0)), None, [Vec2::ZERO; 2]);
        v.end_frame();

        assert_eq!(
            v.forced_offset(Side::Left, true),
            None,
            "the view stayed pinned to the jump"
        );
    }

    #[test]
    fn centring_puts_the_row_in_the_middle() {
        let d = compare(&lines(200), &lines(200));
        let l = layout_for(&d);
        let mut v = Viewport::default();

        v.scroll_to_row(100, Reveal::Centre, &l, LH, VIEW);
        let target = v.forced_offset(Side::Left, true).expect("a jump");
        let row_top = l.row_top(100, LH);
        assert!(
            (row_top - target.y - VIEW / 2.0).abs() < LH,
            "row at {row_top} but offset {} does not centre it",
            target.y
        );
    }

    #[test]
    fn a_minimal_reveal_does_nothing_when_the_row_is_already_visible() {
        let d = compare(&lines(200), &lines(200));
        let l = layout_for(&d);
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 100.0), Vec2::new(0.0, 100.0), [Vec2::ZERO; 2]);

        // Row 8 sits at y=160, inside 100..500.
        v.scroll_to_row(8, Reveal::Minimal, &l, LH, VIEW);
        assert_eq!(v.forced_offset(Side::Left, true), None);
    }

    #[test]
    fn a_minimal_reveal_scrolls_just_far_enough() {
        let d = compare(&lines(200), &lines(200));
        let l = layout_for(&d);
        let mut v = Viewport::default();

        // Row 30 is at y=600, below a viewport showing 0..400.
        v.scroll_to_row(30, Reveal::Minimal, &l, LH, VIEW);
        let target = v.forced_offset(Side::Left, true).expect("a jump").y;
        assert_eq!(target, 620.0 - VIEW, "should stop with the row at the bottom");
        assert!(target < l.row_top(30, LH), "the row must be on screen");
    }

    #[test]
    fn a_minimal_reveal_upwards_stops_at_the_row() {
        let d = compare(&lines(200), &lines(200));
        let l = layout_for(&d);
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 1000.0), Vec2::new(0.0, 1000.0), [Vec2::ZERO; 2]);

        v.scroll_to_row(10, Reveal::Minimal, &l, LH, VIEW);
        assert_eq!(v.forced_offset(Side::Left, true).unwrap().y, l.row_top(10, LH));
    }

    /// The anchor is the whole point: an edit above the viewport must not slide
    /// the text the reader is looking at.
    #[test]
    fn an_insertion_above_the_viewport_does_not_move_the_view() {
        let left = lines(500);
        let before = compare(&left, &left);
        let layout_before = layout_for(&before);

        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 4000.0), Vec2::new(0.0, 4000.0), [Vec2::ZERO; 2]);
        let top_line = {
            let row = layout_before.row_at(v.offset().y, LH);
            before.rows[row].left().expect("a left line")
        };

        // Three lines appear near the top, well above the viewport.
        let anchor = v.capture_anchor(&before, &layout_before, Side::Left, LH);
        let mut right = left.clone();
        for i in 0..3 {
            right.insert(10 + i, format!("inserted {i}"));
        }
        let after = compare(&left, &right);
        let layout_after = layout_for(&after);
        v.restore_anchor(anchor, &after, &layout_after, LH);
        v.end_frame();

        let now_top = {
            let row = layout_after.row_at(v.offset().y, LH);
            after.rows[row].left().expect("a left line")
        };
        assert_eq!(
            now_top, top_line,
            "the reader was looking at line {top_line} and ended up at {now_top}"
        );
        assert!(
            v.offset().y > 4000.0,
            "three rows were added above, so the offset should have grown"
        );
    }

    #[test]
    fn the_anchor_holds_position_to_the_pixel() {
        let doc = lines(300);
        let d = compare(&doc, &doc);
        let l = layout_for(&d);
        let mut v = Viewport::default();

        // Deliberately part-way through a row.
        frame(&mut v, Vec2::new(0.0, 1007.0), Vec2::new(0.0, 1007.0), [Vec2::ZERO; 2]);
        let anchor = v.capture_anchor(&d, &l, Side::Left, LH);
        v.restore_anchor(anchor, &d, &l, LH);
        v.end_frame();

        assert_eq!(v.offset().y, 1007.0, "an unchanged comparison must not move");
    }

    #[test]
    fn anchoring_is_harmless_when_the_line_disappears() {
        let doc = lines(100);
        let d = compare(&doc, &doc);
        let l = layout_for(&d);
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 800.0), Vec2::new(0.0, 800.0), [Vec2::ZERO; 2]);

        let anchor = v.capture_anchor(&d, &l, Side::Left, LH);

        // The document is replaced by something much shorter.
        let short = lines(5);
        let after = compare(&short, &short);
        let layout_after = layout_for(&after);
        v.restore_anchor(anchor, &after, &layout_after, LH);
        v.end_frame();
        // No panic, and the offset stays sane.
        assert!(v.offset().y >= 0.0);
    }

    #[test]
    fn an_empty_document_has_no_anchor() {
        let d = compare(&[], &[]);
        let l = layout_for(&d);
        let v = Viewport::default();
        assert!(v.capture_anchor(&d, &l, Side::Left, LH).is_none());
    }

    #[test]
    fn reset_returns_to_the_top() {
        let mut v = Viewport::default();
        frame(&mut v, Vec2::new(0.0, 900.0), Vec2::new(0.0, 900.0), [Vec2::ZERO; 2]);
        v.reset();
        assert_eq!(v.offset(), Vec2::ZERO);
        assert_eq!(v.forced_offset(Side::Right, true), None);
    }
}
