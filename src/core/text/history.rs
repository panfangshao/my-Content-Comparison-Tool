//! Undo / redo for a single editor pane.
//!
//! Each side keeps its own history - undoing in the left pane must never touch
//! the right one. A *step* is one user-visible undo unit; a step may contain
//! several edits, which is how a cleanup tool ("remove duplicate lines") undoes
//! in one keystroke.

use std::time::{Duration, Instant};

/// A caret plus its selection anchor, in character offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection was started (stays put while dragging).
    pub anchor: usize,
    /// Where the caret is now.
    pub head: usize,
}

impl Selection {
    #[inline]
    pub fn at(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    #[inline]
    pub fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    #[inline]
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    #[inline]
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
}

/// One replacement: at character `start`, `removed` became `inserted`.
///
/// Storing both halves makes the inverse trivial, which is all undo needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

impl Edit {
    pub fn inverse(&self) -> Self {
        Self {
            start: self.start,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }

    /// Character offset just past the inserted text.
    pub fn end_after(&self) -> usize {
        self.start + self.inserted.chars().count()
    }

    /// Rough memory footprint: both halves of the edit are stored.
    fn bytes(&self) -> usize {
        self.removed.len() + self.inserted.len()
    }
}

/// One undo unit.
#[derive(Clone, Debug)]
pub struct Step {
    pub edits: Vec<Edit>,
    pub before: Selection,
    pub after: Selection,
    /// When the last edit landed - used to decide whether the next keystroke
    /// joins this step or starts a new one.
    at: Instant,
}

/// How long a run of typing keeps merging into one undo step.
const COALESCE_WINDOW: Duration = Duration::from_millis(700);

/// Hard cap so a long session cannot grow the history without bound. At ~100
/// bytes per typical step this is a few megabytes at worst.
const MAX_STEPS: usize = 4096;

/// Byte budget for the undo stack. The step count alone does not bound
/// memory: a single step can hold a whole pasted document. Once the budget is
/// exceeded the oldest steps are dropped, newest kept.
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Total bytes held by the undo stack, kept incrementally so enforcing
    /// the budget does not walk every step on every keystroke.
    bytes: usize,
    /// Undo position (== `undo.len()`) at the last save, or `None` when the
    /// saved state can no longer be reached by undoing. Compared against the
    /// current position for the "unsaved changes" indicator, so undo/redo
    /// back to the saved state reads clean again.
    saved: Option<usize>,
    /// Set by [`History::break_run`]; forces the next edit to start a new step.
    barrier: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            bytes: 0,
            saved: Some(0),
            barrier: false,
        }
    }
}

impl Step {
    fn bytes(&self) -> usize {
        self.edits.iter().map(Edit::bytes).sum()
    }
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.saved = Some(0);
        self.barrier = false;
    }

    /// Whether the document differs from the last saved state. Position-based,
    /// so undo followed by redo back to the save point reads clean again.
    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    /// Record the current position as the saved state.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    /// End the current run, so the next edit cannot merge into it.
    ///
    /// Called when the caret moves, on save, and before a bulk operation.
    pub fn break_run(&mut self) {
        self.barrier = true;
    }

    /// Record an edit. Any redo history is discarded, as in every editor.
    pub fn push(&mut self, edit: Edit, before: Selection, after: Selection) {
        self.push_group(vec![edit], before, after);
    }

    /// Record several edits as a single undo unit. Never coalesces.
    pub fn push_group(&mut self, edits: Vec<Edit>, before: Selection, after: Selection) {
        if edits.is_empty() {
            return;
        }
        // Editing after an undo throws the redo stack away, as in every
        // editor. If the saved state lived on it, it is now unreachable, so
        // no amount of undoing can make the document read "clean" again.
        if !self.redo.is_empty() {
            self.redo.clear();
            if self.saved.is_some_and(|s| s > self.undo.len()) {
                self.saved = None;
            }
        }

        let now = Instant::now();
        if edits.len() == 1 && !self.barrier && self.can_coalesce(&edits[0], now) {
            let edit = edits.into_iter().next().expect("len == 1");
            self.bytes += edit.bytes();
            let step = self.undo.last_mut().expect("can_coalesce checked non-empty");
            step.edits.push(edit);
            step.after = after;
            step.at = now;
            return;
        }

        self.barrier = false;
        let step = Step {
            edits,
            before,
            after,
            at: now,
        };
        self.bytes += step.bytes();
        self.undo.push(step);
        if self.undo.len() > MAX_STEPS {
            self.drop_oldest();
        }
        // A single step can hold a whole pasted document, so cap by bytes as
        // well as by count. Keep at least the newest step - dropping it would
        // make the edit it just recorded un-undoable.
        while self.bytes > MAX_BYTES && self.undo.len() > 1 {
            self.drop_oldest();
        }
    }

    /// Drop the oldest step, fixing up the byte count and the save point.
    fn drop_oldest(&mut self) {
        let step = self.undo.remove(0);
        self.bytes -= step.bytes();
        self.saved = match self.saved {
            // The saved state itself just fell off: it is unreachable now.
            Some(0) => None,
            Some(s) => Some(s - 1),
            None => None,
        };
    }

    /// Typing merges into the previous step when it continues straight on from
    /// it, within the time window, and contains no newline.
    ///
    /// A newline ends the run so that undo stops at line boundaries, which is
    /// what people expect from Ctrl+Z.
    fn can_coalesce(&self, edit: &Edit, now: Instant) -> bool {
        let Some(last) = self.undo.last() else {
            return false;
        };
        if now.duration_since(last.at) > COALESCE_WINDOW {
            return false;
        }
        if edit.inserted.contains('\n') || edit.removed.contains('\n') {
            return false;
        }
        let Some(prev) = last.edits.last() else {
            return false;
        };
        // A newline closes the run from *both* sides: the edit that added it
        // does not join the run before it, and nothing joins on after it.
        if prev.inserted.contains('\n') || prev.removed.contains('\n') {
            return false;
        }

        let typing = prev.removed.is_empty()
            && edit.removed.is_empty()
            && edit.start == prev.end_after();
        let backspacing = prev.inserted.is_empty()
            && edit.inserted.is_empty()
            && edit.start + edit.removed.chars().count() == prev.start;

        typing || backspacing
    }

    /// Pop a step for the caller to apply in reverse. The caller must call
    /// [`History::commit_undo`] with the same step once it has been applied.
    pub fn pop_undo(&mut self) -> Option<Step> {
        self.barrier = true;
        let step = self.undo.pop()?;
        self.bytes -= step.bytes();
        Some(step)
    }

    pub fn commit_undo(&mut self, step: Step) {
        self.redo.push(step);
    }

    pub fn pop_redo(&mut self) -> Option<Step> {
        self.barrier = true;
        self.redo.pop()
    }

    pub fn commit_redo(&mut self, step: Step) {
        self.bytes += step.bytes();
        self.undo.push(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(start: usize, text: &str) -> Edit {
        Edit {
            start,
            removed: String::new(),
            inserted: text.to_owned(),
        }
    }

    #[test]
    fn inverse_round_trips() {
        let e = Edit {
            start: 3,
            removed: "old".into(),
            inserted: "new!".into(),
        };
        assert_eq!(e.inverse().inverse(), e);
        assert_eq!(e.end_after(), 7);
    }

    #[test]
    fn consecutive_typing_becomes_one_step() {
        let mut h = History::new();
        for (i, c) in "hello".chars().enumerate() {
            h.push(
                ins(i, &c.to_string()),
                Selection::at(i),
                Selection::at(i + 1),
            );
        }
        assert_eq!(h.undo.len(), 1, "typing should collapse into one step");
        assert_eq!(h.undo[0].edits.len(), 5);
        assert_eq!(h.undo[0].after, Selection::at(5));
    }

    #[test]
    fn a_newline_ends_the_run() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.push(ins(1, "\n"), Selection::at(1), Selection::at(2));
        h.push(ins(2, "b"), Selection::at(2), Selection::at(3));
        assert_eq!(h.undo.len(), 3);
    }

    #[test]
    fn non_adjacent_edits_do_not_merge() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.push(ins(50, "b"), Selection::at(50), Selection::at(51));
        assert_eq!(h.undo.len(), 2);
    }

    #[test]
    fn break_run_forces_a_new_step() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.break_run();
        h.push(ins(1, "b"), Selection::at(1), Selection::at(2));
        assert_eq!(h.undo.len(), 2);
    }

    #[test]
    fn backspacing_collapses_too() {
        let mut h = History::new();
        for i in (0..4).rev() {
            h.push(
                Edit {
                    start: i,
                    removed: "x".into(),
                    inserted: String::new(),
                },
                Selection::at(i + 1),
                Selection::at(i),
            );
        }
        assert_eq!(h.undo.len(), 1);
    }

    #[test]
    fn a_group_is_one_step_and_never_coalesces() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.push_group(
            vec![ins(1, "b"), ins(2, "c")],
            Selection::at(1),
            Selection::at(3),
        );
        assert_eq!(h.undo.len(), 2);
        assert_eq!(h.undo[1].edits.len(), 2);
    }

    #[test]
    fn a_new_edit_discards_redo() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        let step = h.pop_undo().unwrap();
        h.commit_undo(step);
        assert!(h.can_redo());
        h.push(ins(0, "z"), Selection::at(0), Selection::at(1));
        assert!(!h.can_redo(), "editing after undo must clear the redo stack");
    }

    #[test]
    fn undo_redo_cycle_preserves_order() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.break_run();
        h.push(ins(1, "b"), Selection::at(1), Selection::at(2));

        let s2 = h.pop_undo().unwrap();
        assert_eq!(s2.edits[0].inserted, "b");
        h.commit_undo(s2);
        let s1 = h.pop_undo().unwrap();
        assert_eq!(s1.edits[0].inserted, "a");
        h.commit_undo(s1);
        assert!(!h.can_undo());

        let r1 = h.pop_redo().unwrap();
        assert_eq!(r1.edits[0].inserted, "a");
        h.commit_redo(r1);
        assert!(h.can_undo());
    }

    #[test]
    fn history_is_bounded() {
        let mut h = History::new();
        for i in 0..MAX_STEPS + 100 {
            h.break_run();
            h.push(ins(i, "x"), Selection::at(i), Selection::at(i + 1));
        }
        assert_eq!(h.undo.len(), MAX_STEPS);
    }

    #[test]
    fn history_is_bounded_by_bytes_too() {
        // One step can hold a whole pasted document, so the step count alone
        // does not bound memory.
        let mut h = History::new();
        let big = "x".repeat(MAX_BYTES / 2 + 1);
        h.push(ins(0, &big), Selection::at(0), Selection::at(big.len()));
        h.break_run();
        h.push(ins(0, &big), Selection::at(0), Selection::at(big.len()));
        assert_eq!(h.undo.len(), 1, "the oldest step must be dropped");
        assert!(h.bytes <= MAX_BYTES);
    }

    #[test]
    fn save_point_survives_undo_redo() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.mark_saved();
        assert!(!h.is_dirty());
        let s = h.pop_undo().unwrap();
        h.commit_undo(s);
        assert!(h.is_dirty(), "undoing past the save point is dirty");
        let s = h.pop_redo().unwrap();
        h.commit_redo(s);
        assert!(!h.is_dirty(), "redoing back to the save point is clean");
    }

    #[test]
    fn editing_after_undo_forgets_the_save_point() {
        let mut h = History::new();
        h.push(ins(0, "a"), Selection::at(0), Selection::at(1));
        h.mark_saved();
        let s = h.pop_undo().unwrap();
        h.commit_undo(s);
        // Diverge instead of redoing: the redo stack, and with it the saved
        // state, is gone.
        h.push(ins(0, "b"), Selection::at(0), Selection::at(1));
        assert!(h.is_dirty(), "a divergent edit must not read as saved");
        let s = h.pop_undo().unwrap();
        h.commit_undo(s);
        assert!(h.is_dirty(), "undoing the divergence cannot bring it back");
    }
}
