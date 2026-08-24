//! The document model for one editor pane.
//!
//! # Why both a rope and a line vector
//!
//! [`ropey::Rope`] is the source of truth: it gives O(log n) character/line
//! conversion and cheap edits in the middle of a huge document, which is what
//! makes typing at line 90 000 feel the same as typing at line 3.
//!
//! But the diff engine and the renderer both want `&[String]` - flat, O(1),
//! borrowable line slices. Deriving that from the rope on every keystroke would
//! mean 100 000 allocations per frame, so we keep a line vector alongside and
//! *patch* it in place: an edit only rebuilds the lines it actually touched.
//!
//! The cost is holding the text twice. For the file sizes this tool targets
//! (tens of megabytes at the very top end) that is a trade worth making, and it
//! is what keeps a 100k-line comparison interactive.

use ropey::Rope;

use super::history::{Edit, History, Selection};

pub struct TextBuffer {
    rope: Rope,
    /// One entry per rope line, newline stripped. Always in sync with `rope`.
    lines: Vec<String>,
    history: History,
    selection: Selection,
    /// Bumped on every content change; the app re-runs the diff when it moves.
    version: u64,
    /// Lowest line index touched since the last [`TextBuffer::clear_dirty`].
    /// The syntax highlighter uses it to keep the checkpoints before the edit
    /// instead of re-parsing the file from line 0 on every keystroke.
    min_dirty_line: usize,
    /// Version at the last save / load, for the "unsaved changes" indicator.
    saved_version: u64,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let lines = collect_lines(&rope);
        Self {
            rope,
            lines,
            history: History::new(),
            selection: Selection::at(0),
            version: 0,
            min_dirty_line: usize::MAX,
            saved_version: 0,
        }
    }

    // ---- Read access ----------------------------------------------------

    #[inline]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Newline-stripped lines. This is what the diff engine consumes.
    #[inline]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[inline]
    pub fn line(&self, index: usize) -> &str {
        self.lines.get(index).map_or("", String::as_str)
    }

    #[inline]
    pub fn len_lines(&self) -> usize {
        self.lines.len()
    }

    #[inline]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    #[inline]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Lowest line changed since the last [`Self::clear_dirty`], or `None` if
    /// nothing has changed.
    #[inline]
    pub fn min_dirty_line(&self) -> Option<usize> {
        (self.min_dirty_line != usize::MAX).then_some(self.min_dirty_line)
    }

    /// Acknowledge the dirty range. Call once per frame, after whatever caches
    /// depend on it have been invalidated.
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.min_dirty_line = usize::MAX;
    }

    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn mark_saved(&mut self) {
        self.saved_version = self.version;
        self.history.break_run();
    }

    // ---- Position conversion --------------------------------------------

    /// Character offset of the start of `line`, clamped to the document.
    #[inline]
    pub fn line_start(&self, line: usize) -> usize {
        let line = line.min(self.lines.len().saturating_sub(1));
        self.rope.line_to_char(line)
    }

    /// Character offset of the end of `line`, *before* its newline.
    #[inline]
    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.line(line).chars().count()
    }

    #[inline]
    pub fn line_of_char(&self, ci: usize) -> usize {
        self.rope.char_to_line(ci.min(self.rope.len_chars()))
    }

    /// `(line, column)` for a character offset, column counted in characters.
    pub fn line_col(&self, ci: usize) -> (usize, usize) {
        let ci = ci.min(self.rope.len_chars());
        let line = self.rope.char_to_line(ci);
        (line, ci - self.rope.line_to_char(line))
    }

    /// Character offset for a `(line, column)` pair, clamped into the line.
    pub fn char_at(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.lines.len().saturating_sub(1));
        let max_col = self.line(line).chars().count();
        self.rope.line_to_char(line) + col.min(max_col)
    }

    // ---- Selection -------------------------------------------------------

    #[inline]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Move the caret. Ends the current undo run, so typing after a click does
    /// not merge with typing before it.
    pub fn set_selection(&mut self, sel: Selection) {
        let max = self.rope.len_chars();
        let sel = Selection {
            anchor: sel.anchor.min(max),
            head: sel.head.min(max),
        };
        if sel != self.selection {
            self.history.break_run();
        }
        self.selection = sel;
    }

    pub fn selected_text(&self) -> String {
        let r = self.selection.range();
        if r.is_empty() {
            String::new()
        } else {
            self.rope.slice(r).to_string()
        }
    }

    pub fn select_all(&mut self) {
        self.set_selection(Selection {
            anchor: 0,
            head: self.rope.len_chars(),
        });
    }

    // ---- Editing ---------------------------------------------------------

    /// Replace `range` with `text` as one undo step, leaving the caret after
    /// the inserted text.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let max = self.rope.len_chars();
        let start = range.start.min(max);
        let end = range.end.min(max).max(start);
        if start == end && text.is_empty() {
            return;
        }

        let removed = self.rope.slice(start..end).to_string();
        let edit = Edit {
            start,
            removed,
            inserted: text.to_owned(),
        };
        let before = self.selection;
        self.apply(&edit);
        let after = Selection::at(edit.end_after());
        self.selection = after;
        self.history.push(edit, before, after);
    }

    /// Type `text`, replacing the selection if there is one.
    pub fn insert(&mut self, text: &str) {
        self.replace_range(self.selection.range(), text);
    }

    /// Backspace: delete the selection, or one character to the left.
    pub fn backspace(&mut self) {
        let sel = self.selection;
        if sel.is_empty() {
            if sel.head == 0 {
                return;
            }
            self.replace_range(sel.head - 1..sel.head, "");
        } else {
            self.replace_range(sel.range(), "");
        }
    }

    /// Delete: remove the selection, or one character to the right.
    pub fn delete_forward(&mut self) {
        let sel = self.selection;
        if sel.is_empty() {
            if sel.head >= self.rope.len_chars() {
                return;
            }
            self.replace_range(sel.head..sel.head + 1, "");
        } else {
            self.replace_range(sel.range(), "");
        }
    }

    /// Replace whole lines `range` with `new_lines`, as a single undo step.
    ///
    /// This is the primitive behind merging a hunk and behind every cleanup
    /// tool, so both are undoable in one keystroke.
    pub fn replace_lines(&mut self, range: std::ops::Range<usize>, new_lines: &[String]) {
        let n = self.lines.len();
        let eof = self.rope.len_chars();
        let start_line = range.start.min(n);
        let end_line = range.end.clamp(start_line, n);

        // Character span covering lines `[start_line, end_line)` *including*
        // the newline that terminates each of them. `line_to_char(k)` for
        // `k < n` already sits just past line `k-1`'s newline.
        let mut start = if start_line >= n {
            eof
        } else {
            self.rope.line_to_char(start_line)
        };
        let end = if end_line >= n {
            eof
        } else {
            self.rope.line_to_char(end_line)
        };

        let mut text = new_lines.join("\n");
        if start_line >= n {
            // Appending past the last line: the new text starts its own line.
            if eof > 0 && !text.is_empty() {
                text.insert(0, '\n');
            }
        } else if end_line >= n {
            // Replacing through EOF: no trailing newline to restore. If we are
            // deleting outright, swallow the newline that preceded the block so
            // it does not leave a blank line behind.
            if new_lines.is_empty() && start > 0 {
                start -= 1;
            }
        } else if !new_lines.is_empty() {
            // A middle block: put back the newline that terminates it.
            text.push('\n');
        }

        self.history.break_run();
        let removed = self.rope.slice(start..end).to_string();
        let edit = Edit {
            start,
            removed,
            inserted: text,
        };
        let before = self.selection;
        self.apply(&edit);
        let after = Selection::at(edit.end_after().min(self.rope.len_chars()));
        self.selection = after;
        self.history.push_group(vec![edit], before, after);
        self.history.break_run();
    }

    /// Replace the whole document as one undo step (open file, swap, clear).
    pub fn set_text(&mut self, text: &str) {
        self.history.break_run();
        self.replace_range(0..self.rope.len_chars(), text);
        self.history.break_run();
    }

    /// Replace the whole document *and* drop the history - used when loading a
    /// file, where undoing back into the previous document makes no sense.
    pub fn load_text(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.lines = collect_lines(&self.rope);
        self.history.clear();
        self.selection = Selection::at(0);
        self.version += 1;
        self.min_dirty_line = 0;
        self.saved_version = self.version;
    }

    // ---- Undo / redo -----------------------------------------------------

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo(&mut self) -> bool {
        let Some(step) = self.history.pop_undo() else {
            return false;
        };
        // Undo the edits back-to-front: each inverse is expressed in the
        // coordinates that existed just before its own edit.
        for edit in step.edits.iter().rev() {
            self.apply(&edit.inverse());
        }
        self.selection = clamp(step.before, self.rope.len_chars());
        self.history.commit_undo(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(step) = self.history.pop_redo() else {
            return false;
        };
        for edit in &step.edits {
            self.apply(edit);
        }
        self.selection = clamp(step.after, self.rope.len_chars());
        self.history.commit_redo(step);
        true
    }

    /// Force the next edit to start a fresh undo step.
    pub fn break_undo_run(&mut self) {
        self.history.break_run();
    }

    // ---- Internals -------------------------------------------------------

    /// Apply an edit to the rope and patch the line cache to match.
    ///
    /// Only the lines the edit spans are rebuilt, which is what keeps editing a
    /// very large document O(edit size) rather than O(document size).
    fn apply(&mut self, edit: &Edit) {
        let start = edit.start;
        let end = start + edit.removed.chars().count();
        debug_assert!(end <= self.rope.len_chars(), "edit outside the document");

        let first = self.rope.char_to_line(start);
        let last = self.rope.char_to_line(end);

        self.rope.remove(start..end);
        if !edit.inserted.is_empty() {
            self.rope.insert(start, &edit.inserted);
        }

        let new_last = self
            .rope
            .char_to_line(start + edit.inserted.chars().count());

        let replacement: Vec<String> = (first..=new_last)
            .map(|i| strip_newline(self.rope.line(i).to_string()))
            .collect();
        self.lines.splice(first..=last, replacement);

        self.version += 1;
        self.min_dirty_line = self.min_dirty_line.min(first);

        debug_assert_eq!(
            self.lines.len(),
            self.rope.len_lines(),
            "line cache drifted from the rope"
        );
    }
}

fn clamp(sel: Selection, max: usize) -> Selection {
    Selection {
        anchor: sel.anchor.min(max),
        head: sel.head.min(max),
    }
}

fn collect_lines(rope: &Rope) -> Vec<String> {
    rope.lines()
        .map(|l| strip_newline(l.to_string()))
        .collect()
}

/// Drop a trailing `\n` or `\r\n`.
fn strip_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line cache is a derived structure; every test asserts it still
    /// matches the rope, because a drift there corrupts the diff silently.
    fn check(b: &TextBuffer) {
        assert_eq!(b.lines.len(), b.rope.len_lines(), "line count drift");
        for (i, line) in b.lines.iter().enumerate() {
            let from_rope = strip_newline(b.rope.line(i).to_string());
            assert_eq!(line, &from_rope, "line {i} drifted");
        }
    }

    #[test]
    fn lines_strip_newlines() {
        let b = TextBuffer::from_text("a\nb\nc");
        assert_eq!(b.lines(), ["a", "b", "c"]);
        check(&b);
    }

    #[test]
    fn a_trailing_newline_makes_a_final_empty_line() {
        let b = TextBuffer::from_text("a\nb\n");
        assert_eq!(b.lines(), ["a", "b", ""]);
        check(&b);
    }

    #[test]
    fn crlf_is_stripped() {
        let b = TextBuffer::from_text("a\r\nb");
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn insert_patches_the_cache() {
        let mut b = TextBuffer::from_text("hello world");
        b.set_selection(Selection::at(5));
        b.insert(",");
        assert_eq!(b.text(), "hello, world");
        assert_eq!(b.lines(), ["hello, world"]);
        check(&b);
    }

    #[test]
    fn inserting_a_newline_splits_the_line() {
        let mut b = TextBuffer::from_text("ab");
        b.set_selection(Selection::at(1));
        b.insert("\n");
        assert_eq!(b.lines(), ["a", "b"]);
        check(&b);
    }

    #[test]
    fn multi_line_paste() {
        let mut b = TextBuffer::from_text("start\nend");
        b.set_selection(Selection::at(6));
        b.insert("one\ntwo\n");
        assert_eq!(b.lines(), ["start", "one", "two", "end"]);
        check(&b);
    }

    #[test]
    fn deleting_across_lines() {
        let mut b = TextBuffer::from_text("aaa\nbbb\nccc");
        b.replace_range(2..9, "");
        assert_eq!(b.text(), "aacc");
        check(&b);
    }

    #[test]
    fn backspace_and_delete() {
        let mut b = TextBuffer::from_text("abc");
        b.set_selection(Selection::at(2));
        b.backspace();
        assert_eq!(b.text(), "ac");
        b.delete_forward();
        assert_eq!(b.text(), "a");
        b.set_selection(Selection::at(0));
        b.backspace();
        assert_eq!(b.text(), "a", "backspace at the start is a no-op");
        check(&b);
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut b = TextBuffer::from_text("hello world");
        b.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        assert_eq!(b.selected_text(), "world");
        b.insert("there");
        assert_eq!(b.text(), "hello there");
        check(&b);
    }

    #[test]
    fn undo_redo_restores_text_and_caret() {
        let mut b = TextBuffer::from_text("abc");
        b.set_selection(Selection::at(3));
        b.insert("d");
        b.break_undo_run();
        b.insert("e");
        assert_eq!(b.text(), "abcde");

        assert!(b.undo());
        assert_eq!(b.text(), "abcd");
        assert!(b.undo());
        assert_eq!(b.text(), "abc");
        assert_eq!(b.selection(), Selection::at(3));
        assert!(!b.undo());

        assert!(b.redo());
        assert_eq!(b.text(), "abcd");
        assert!(b.redo());
        assert_eq!(b.text(), "abcde");
        assert!(!b.redo());
        check(&b);
    }

    #[test]
    fn undo_of_a_coalesced_run_is_one_step() {
        let mut b = TextBuffer::from_text("");
        for c in "hello".chars() {
            b.insert(&c.to_string());
        }
        assert_eq!(b.text(), "hello");
        assert!(b.undo());
        assert_eq!(b.text(), "", "a typing run undoes in one go");
        check(&b);
    }

    #[test]
    fn replace_lines_in_the_middle() {
        let mut b = TextBuffer::from_text("a\nb\nc\nd");
        b.replace_lines(1..3, &["X".into(), "Y".into(), "Z".into()]);
        assert_eq!(b.lines(), ["a", "X", "Y", "Z", "d"]);
        check(&b);
        assert!(b.undo());
        assert_eq!(b.lines(), ["a", "b", "c", "d"]);
        check(&b);
    }

    #[test]
    fn replace_lines_can_insert_without_removing() {
        let mut b = TextBuffer::from_text("a\nc");
        b.replace_lines(1..1, &["b".into()]);
        assert_eq!(b.lines(), ["a", "b", "c"]);
        check(&b);
    }

    #[test]
    fn replace_lines_can_delete_the_tail() {
        let mut b = TextBuffer::from_text("a\nb\nc");
        b.replace_lines(1..3, &[]);
        assert_eq!(b.lines(), ["a"]);
        check(&b);
    }

    #[test]
    fn replace_lines_at_the_tail_keeps_line_count() {
        let mut b = TextBuffer::from_text("a\nb\nc");
        b.replace_lines(2..3, &["C".into()]);
        assert_eq!(b.lines(), ["a", "b", "C"]);
        check(&b);
    }

    #[test]
    fn replace_lines_beyond_the_end_appends() {
        let mut b = TextBuffer::from_text("a");
        b.replace_lines(1..1, &["b".into()]);
        assert_eq!(b.lines(), ["a", "b"]);
        check(&b);
    }

    #[test]
    fn line_and_char_conversions_agree() {
        let b = TextBuffer::from_text("ab\ncde\n\nf");
        for ci in 0..=b.len_chars() {
            let (line, col) = b.line_col(ci);
            assert_eq!(b.char_at(line, col), ci, "round trip failed at {ci}");
        }
        assert_eq!(b.line_start(1), 3);
        assert_eq!(b.line_end(1), 6);
        assert_eq!(b.line_col(0), (0, 0));
    }

    #[test]
    fn char_at_clamps_past_the_line_end() {
        let b = TextBuffer::from_text("ab\ncd");
        assert_eq!(b.char_at(0, 99), 2, "clamped to the end of line 0");
        assert_eq!(b.char_at(99, 0), 3, "clamped to the last line");
    }

    #[test]
    fn dirty_tracking() {
        let mut b = TextBuffer::from_text("a");
        assert!(!b.is_dirty());
        b.insert("b");
        assert!(b.is_dirty());
        b.mark_saved();
        assert!(!b.is_dirty());
        assert!(b.undo());
        assert!(b.is_dirty(), "undoing past the save point is still dirty");
    }

    #[test]
    fn load_text_clears_history() {
        let mut b = TextBuffer::from_text("a");
        b.insert("b");
        b.load_text("fresh");
        assert!(!b.can_undo());
        assert!(!b.is_dirty());
        assert_eq!(b.text(), "fresh");
        check(&b);
    }

    #[test]
    fn unicode_offsets_are_character_based() {
        let mut b = TextBuffer::from_text("对比工具");
        assert_eq!(b.len_chars(), 4);
        b.set_selection(Selection::at(2));
        b.insert("X");
        assert_eq!(b.text(), "对比X工具");
        check(&b);
    }

    #[test]
    fn dirty_line_tracks_the_earliest_edit() {
        let mut b = TextBuffer::from_text("a
b
c
d");
        assert_eq!(b.min_dirty_line(), None);

        b.set_selection(Selection::at(b.char_at(2, 0)));
        b.insert("X");
        assert_eq!(b.min_dirty_line(), Some(2));

        // A later edit higher up lowers the watermark; an edit below leaves it.
        b.set_selection(Selection::at(b.char_at(3, 0)));
        b.insert("Y");
        assert_eq!(b.min_dirty_line(), Some(2));
        b.set_selection(Selection::at(b.char_at(0, 0)));
        b.insert("Z");
        assert_eq!(b.min_dirty_line(), Some(0));

        b.clear_dirty();
        assert_eq!(b.min_dirty_line(), None);
    }

    #[test]
    fn loading_marks_everything_dirty() {
        let mut b = TextBuffer::from_text("a");
        b.clear_dirty();
        b.load_text("x
y");
        assert_eq!(b.min_dirty_line(), Some(0));
    }

    #[test]
    fn many_edits_keep_the_cache_consistent() {
        let mut b = TextBuffer::from_text(&"line\n".repeat(200));
        for i in (0..200).step_by(7) {
            let at = b.char_at(i, 2);
            b.set_selection(Selection::at(at));
            b.insert("~");
        }
        check(&b);
        for _ in 0..30 {
            b.undo();
        }
        check(&b);
        for _ in 0..30 {
            b.redo();
        }
        check(&b);
    }
}
