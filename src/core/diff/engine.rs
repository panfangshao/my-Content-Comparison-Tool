//! Line-level diff: turns two documents into a single list of *aligned rows*
//! that both panes render against.
//!
//! The row list is what makes the two panes line up visually. Row `i` shows
//! left line `rows[i].left()` next to right line `rows[i].right()`; either may
//! be absent, which is how insertions and deletions get their blank filler on
//! the opposite side.

use std::borrow::Cow;
use std::ops::Range;

use similar::DiffOp;

use super::options::DiffOptions;
use super::similarity::dice;

/// Sentinel for "this row has no line on this side".
const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RowKind {
    /// Both sides present and equal (under the current options).
    Equal,
    /// Both sides present but different - the pair a word-diff runs on.
    Replace,
    /// Right side only.
    Insert,
    /// Left side only.
    Delete,
    /// A blank line skipped by `ignore_blank_lines`. Rendered neutral and
    /// excluded from the statistics.
    Ignored,
}

impl RowKind {
    #[inline]
    pub fn is_change(self) -> bool {
        matches!(self, Self::Replace | Self::Insert | Self::Delete)
    }
}

/// One rendered row. Kept to 12 bytes: a 300k-line comparison allocates ~3.6 MB
/// for the row table, which matters when the user is retyping and we rebuild it
/// on every keystroke.
#[derive(Clone, Copy, Debug)]
pub struct DiffRow {
    left: u32,
    right: u32,
    pub kind: RowKind,
}

impl DiffRow {
    #[inline]
    pub fn left(&self) -> Option<usize> {
        (self.left != NONE).then_some(self.left as usize)
    }

    #[inline]
    pub fn right(&self) -> Option<usize> {
        (self.right != NONE).then_some(self.right as usize)
    }

    #[inline]
    pub fn side(&self, side: Side) -> Option<usize> {
        match side {
            Side::Left => self.left(),
            Side::Right => self.right(),
        }
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, Debug, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    #[inline]
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// A contiguous run of changed rows - the unit the merge arrows operate on.
#[derive(Clone, Debug)]
pub struct Hunk {
    /// Range into [`DiffResult::rows`].
    pub rows: Range<usize>,
    /// Left lines covered (may be empty for a pure insertion).
    pub left: Range<usize>,
    /// Right lines covered (may be empty for a pure deletion).
    pub right: Range<usize>,
}

impl Hunk {
    #[inline]
    pub fn lines(&self, side: Side) -> Range<usize> {
        match side {
            Side::Left => self.left.clone(),
            Side::Right => self.right.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub unchanged: usize,
    /// Line-level Sorensen-Dice over the two documents, in `0.0..=1.0`.
    pub similarity: f32,
}

impl DiffStats {
    pub fn is_identical(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.modified == 0
    }
}

#[derive(Clone, Debug, Default)]
pub struct DiffResult {
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<Hunk>,
    pub stats: DiffStats,
    /// `row_of_left[line]` is the row that renders that left line. Used to keep
    /// the caret, the scroll sync and the search results in step with the view.
    pub row_of_left: Vec<u32>,
    pub row_of_right: Vec<u32>,
    /// Set when the diff hit its time budget and fell back to a coarse result.
    pub truncated: bool,
}

impl DiffResult {
    #[inline]
    pub fn row_of(&self, side: Side, line: usize) -> Option<usize> {
        let table = match side {
            Side::Left => &self.row_of_left,
            Side::Right => &self.row_of_right,
        };
        table.get(line).map(|&r| r as usize)
    }

    /// The hunk containing `row`, if any.
    pub fn hunk_at_row(&self, row: usize) -> Option<usize> {
        self.hunks
            .binary_search_by(|h| {
                if row < h.rows.start {
                    std::cmp::Ordering::Greater
                } else if row >= h.rows.end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()
    }

    /// First hunk starting at or after `row`.
    pub fn hunk_after(&self, row: usize) -> Option<usize> {
        self.hunks.iter().position(|h| h.rows.start > row)
    }

    /// Last hunk starting strictly before `row`.
    pub fn hunk_before(&self, row: usize) -> Option<usize> {
        self.hunks.iter().rposition(|h| h.rows.start < row)
    }
}

/// Budget for a single comparison. Beyond it we stop refining and emit a
/// coarser (but still correct) diff, so typing never blocks the UI thread.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub deadline: Option<std::time::Instant>,
    /// Max `old_len * new_len` cells we will spend on similarity alignment
    /// inside one replaced block.
    pub max_align_cells: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(250)),
            max_align_cells: 1 << 20,
        }
    }
}

impl Budget {
    /// No time limit - used by tests and by the "compare anyway" path.
    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            max_align_cells: 1 << 22,
        }
    }
}

/// Row geometry for a single document with nothing to compare against.
///
/// Single-document mode still renders through the same aligned-row machinery as
/// the two-pane view; it just needs a result that says "one row per line, no
/// changes". Building it directly is O(n) and avoids the silliness of diffing a
/// document against itself.
pub fn single_document(lines: usize) -> DiffResult {
    DiffResult {
        rows: (0..lines)
            .map(|i| DiffRow {
                left: i as u32,
                right: NONE,
                kind: RowKind::Equal,
            })
            .collect(),
        hunks: Vec::new(),
        stats: DiffStats {
            unchanged: lines,
            similarity: 1.0,
            ..DiffStats::default()
        },
        row_of_left: (0..lines as u32).collect(),
        row_of_right: Vec::new(),
        truncated: false,
    }
}

/// Compare two documents line by line.
pub fn diff_lines(
    left: &[String],
    right: &[String],
    opts: &DiffOptions,
    budget: Budget,
) -> DiffResult {
    // ---- 1. Decide which lines take part in the match -------------------
    //
    // With `ignore_blank_lines` we diff only the non-blank lines and splice the
    // blanks back in afterwards, so an added empty line cannot shift a hunk.
    let (left_idx, right_idx): (Vec<usize>, Vec<usize>) = if opts.ignore_blank_lines {
        (
            (0..left.len())
                .filter(|&i| !opts.is_ignorable_blank(&left[i]))
                .collect(),
            (0..right.len())
                .filter(|&i| !opts.is_ignorable_blank(&right[i]))
                .collect(),
        )
    } else {
        ((0..left.len()).collect(), (0..right.len()).collect())
    };

    // ---- 2. Build the comparison keys -----------------------------------
    //
    // `is_exact` is the common case; there we borrow the original strings and
    // allocate nothing per line.
    let alg = opts.algorithm.to_similar();
    let ops = if opts.is_exact() {
        let a: Vec<&str> = left_idx.iter().map(|&i| left[i].as_str()).collect();
        let b: Vec<&str> = right_idx.iter().map(|&i| right[i].as_str()).collect();
        similar::capture_diff_slices_deadline(alg, &a, &b, budget.deadline)
    } else {
        let a: Vec<Cow<'_, str>> = left_idx.iter().map(|&i| opts.normalize(&left[i])).collect();
        let b: Vec<Cow<'_, str>> = right_idx
            .iter()
            .map(|&i| opts.normalize(&right[i]))
            .collect();
        similar::capture_diff_slices_deadline(alg, &a, &b, budget.deadline)
    };

    let truncated = budget
        .deadline
        .is_some_and(|d| std::time::Instant::now() > d);

    // ---- 3. Turn ops into aligned rows ----------------------------------
    let mut b = RowBuilder::new(left, right, &left_idx, &right_idx, opts);
    for op in &ops {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    b.push_pair(old_index + k, new_index + k, RowKind::Equal);
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    b.push_left(old_index + k, RowKind::Delete);
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    b.push_right(new_index + k, RowKind::Insert);
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                b.push_replace(
                    old_index..old_index + old_len,
                    new_index..new_index + new_len,
                    opts,
                    budget,
                );
            }
        }
    }

    let mut result = b.finish();
    result.truncated = truncated;
    result
}

/// Accumulates rows while translating between *filtered* indices (what the diff
/// algorithm saw) and *raw* line numbers (what the editor shows).
struct RowBuilder<'a> {
    left: &'a [String],
    right: &'a [String],
    left_idx: &'a [usize],
    right_idx: &'a [usize],
    blank_aware: bool,

    rows: Vec<DiffRow>,
    row_of_left: Vec<u32>,
    row_of_right: Vec<u32>,
    stats: DiffStats,

    /// Next raw line on each side that has not been emitted yet. Used to splice
    /// ignored blank lines back into the row list.
    next_raw_left: usize,
    next_raw_right: usize,
}

impl<'a> RowBuilder<'a> {
    fn new(
        left: &'a [String],
        right: &'a [String],
        left_idx: &'a [usize],
        right_idx: &'a [usize],
        opts: &DiffOptions,
    ) -> Self {
        let cap = left.len().max(right.len()) + 16;
        Self {
            left,
            right,
            left_idx,
            right_idx,
            blank_aware: opts.ignore_blank_lines,
            rows: Vec::with_capacity(cap),
            row_of_left: vec![NONE; left.len()],
            row_of_right: vec![NONE; right.len()],
            stats: DiffStats::default(),
            next_raw_left: 0,
            next_raw_right: 0,
        }
    }

    /// Emit any blank lines that sit before the given raw targets.
    ///
    /// Blanks pending on both sides are paired up so they occupy one row rather
    /// than two, which keeps the panes tight.
    fn flush_blanks_before(&mut self, left_target: Option<usize>, right_target: Option<usize>) {
        if !self.blank_aware {
            return;
        }
        let l_end = left_target.unwrap_or(self.next_raw_left);
        let r_end = right_target.unwrap_or(self.next_raw_right);

        loop {
            let l_pending = self.next_raw_left < l_end;
            let r_pending = self.next_raw_right < r_end;
            match (l_pending, r_pending) {
                (false, false) => break,
                (true, true) => {
                    let (l, r) = (self.next_raw_left, self.next_raw_right);
                    self.emit(Some(l), Some(r), RowKind::Ignored);
                }
                (true, false) => {
                    let l = self.next_raw_left;
                    self.emit(Some(l), None, RowKind::Ignored);
                }
                (false, true) => {
                    let r = self.next_raw_right;
                    self.emit(None, Some(r), RowKind::Ignored);
                }
            }
        }
    }

    fn emit(&mut self, l: Option<usize>, r: Option<usize>, kind: RowKind) {
        let row = self.rows.len() as u32;
        if let Some(l) = l {
            self.row_of_left[l] = row;
            self.next_raw_left = self.next_raw_left.max(l + 1);
        }
        if let Some(r) = r {
            self.row_of_right[r] = row;
            self.next_raw_right = self.next_raw_right.max(r + 1);
        }
        match kind {
            RowKind::Equal => self.stats.unchanged += 1,
            RowKind::Replace => self.stats.modified += 1,
            RowKind::Insert => self.stats.added += 1,
            RowKind::Delete => self.stats.removed += 1,
            RowKind::Ignored => {}
        }
        self.rows.push(DiffRow {
            left: l.map_or(NONE, |v| v as u32),
            right: r.map_or(NONE, |v| v as u32),
            kind,
        });
    }

    fn push_pair(&mut self, li: usize, ri: usize, kind: RowKind) {
        let (l, r) = (self.left_idx[li], self.right_idx[ri]);
        self.flush_blanks_before(Some(l), Some(r));
        self.emit(Some(l), Some(r), kind);
    }

    fn push_left(&mut self, li: usize, kind: RowKind) {
        let l = self.left_idx[li];
        self.flush_blanks_before(Some(l), None);
        self.emit(Some(l), None, kind);
    }

    fn push_right(&mut self, ri: usize, kind: RowKind) {
        let r = self.right_idx[ri];
        self.flush_blanks_before(None, Some(r));
        self.emit(None, Some(r), kind);
    }

    /// A replaced block. This is where "smart alignment" happens: rather than
    /// zipping the two sides positionally, we find the best order-preserving
    /// pairing by similarity so an edited line sits opposite its original.
    fn push_replace(
        &mut self,
        old: Range<usize>,
        new: Range<usize>,
        opts: &DiffOptions,
        budget: Budget,
    ) {
        let (n, m) = (old.len(), new.len());
        let cells = n.saturating_mul(m);
        let use_smart = opts.smart_align && n > 0 && m > 0 && cells <= budget.max_align_cells;

        let pairs: Vec<(usize, usize)> = if use_smart {
            self.align_block(&old, &new, opts)
        } else {
            // Positional fallback: pair index-for-index, leftovers one-sided.
            (0..n.min(m)).map(|k| (k, k)).collect()
        };

        // Walk both sides in order, emitting one-sided rows between the pairs.
        let (mut i, mut j) = (0usize, 0usize);
        for (pi, pj) in pairs {
            while i < pi {
                self.push_left(old.start + i, RowKind::Delete);
                i += 1;
            }
            while j < pj {
                self.push_right(new.start + j, RowKind::Insert);
                j += 1;
            }
            self.push_pair(old.start + pi, new.start + pj, RowKind::Replace);
            i = pi + 1;
            j = pj + 1;
        }
        while i < n {
            self.push_left(old.start + i, RowKind::Delete);
            i += 1;
        }
        while j < m {
            self.push_right(new.start + j, RowKind::Insert);
            j += 1;
        }
    }

    /// Order-preserving maximum-similarity matching between two blocks.
    ///
    /// Same shape as an LCS DP, except the "match" reward is a continuous
    /// similarity score instead of a boolean equality, and pairs scoring below
    /// `align_threshold` are never taken.
    fn align_block(
        &self,
        old: &Range<usize>,
        new: &Range<usize>,
        opts: &DiffOptions,
    ) -> Vec<(usize, usize)> {
        let (n, m) = (old.len(), new.len());

        // Materialize the comparison keys once per block.
        let a: Vec<Cow<'_, str>> = old
            .clone()
            .map(|i| opts.normalize(&self.left[self.left_idx[i]]))
            .collect();
        let b: Vec<Cow<'_, str>> = new
            .clone()
            .map(|j| opts.normalize(&self.right[self.right_idx[j]]))
            .collect();

        // `prev`/`cur` are rolling DP rows; `choice` records the backtrack.
        // 0 = skip old, 1 = skip new, 2 = pair them.
        let mut prev = vec![0.0f32; m + 1];
        let mut cur = vec![0.0f32; m + 1];
        let mut choice = vec![0u8; (n + 1) * (m + 1)];

        for i in 1..=n {
            for j in 1..=m {
                let skip_old = prev[j];
                let skip_new = cur[j - 1];
                let (mut best, mut pick) = if skip_old >= skip_new {
                    (skip_old, 0u8)
                } else {
                    (skip_new, 1u8)
                };

                let s = dice(&a[i - 1], &b[j - 1]);
                if s >= opts.align_threshold {
                    let paired = prev[j - 1] + s;
                    if paired > best {
                        best = paired;
                        pick = 2;
                    }
                }
                cur[j] = best;
                choice[i * (m + 1) + j] = pick;
            }
            std::mem::swap(&mut prev, &mut cur);
            cur.fill(0.0);
        }

        let mut pairs = Vec::new();
        let (mut i, mut j) = (n, m);
        while i > 0 && j > 0 {
            match choice[i * (m + 1) + j] {
                2 => {
                    pairs.push((i - 1, j - 1));
                    i -= 1;
                    j -= 1;
                }
                0 => i -= 1,
                _ => j -= 1,
            }
        }
        pairs.reverse();
        pairs
    }

    fn finish(mut self) -> DiffResult {
        // Trailing blank lines that no op referenced.
        self.flush_blanks_before(Some(self.left.len()), Some(self.right.len()));

        // Line-level similarity: equal lines count double against the combined
        // length, matching the usual Sorensen-Dice definition.
        let total = self.left.len() + self.right.len();
        self.stats.similarity = if total == 0 {
            1.0
        } else {
            (2 * self.stats.unchanged) as f32 / total as f32
        };

        let hunks = build_hunks(&self.rows);
        DiffResult {
            rows: self.rows,
            hunks,
            stats: self.stats,
            row_of_left: self.row_of_left,
            row_of_right: self.row_of_right,
            truncated: false,
        }
    }
}

/// Group consecutive changed rows into hunks.
///
/// `Ignored` rows do *not* break a hunk - an ignored blank line in the middle of
/// a changed block should not split it into two merge targets.
fn build_hunks(rows: &[DiffRow]) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut start: Option<usize> = None;

    for i in 0..=rows.len() {
        let kind = rows.get(i).map(|r| r.kind);
        let is_change = kind.is_some_and(RowKind::is_change);
        let is_bridge = kind == Some(RowKind::Ignored);

        match (start, is_change) {
            (None, true) => start = Some(i),
            (Some(s), false) if !is_bridge => {
                // Trim trailing `Ignored` rows back off the hunk.
                let mut end = i;
                while end > s && rows[end - 1].kind == RowKind::Ignored {
                    end -= 1;
                }
                hunks.push(make_hunk(rows, s..end));
                start = None;
            }
            _ => {}
        }
    }
    hunks
}

fn make_hunk(rows: &[DiffRow], range: Range<usize>) -> Hunk {
    let mut left: Option<Range<usize>> = None;
    let mut right: Option<Range<usize>> = None;

    for row in &rows[range.clone()] {
        if let Some(l) = row.left() {
            left = Some(match left {
                Some(r) => r.start.min(l)..r.end.max(l + 1),
                None => l..l + 1,
            });
        }
        if let Some(r) = row.right() {
            right = Some(match right {
                Some(rr) => rr.start.min(r)..rr.end.max(r + 1),
                None => r..r + 1,
            });
        }
    }

    // An empty range still needs a *position*: it tells the merge code where to
    // splice inserted lines on the side that has none.
    let left = left.unwrap_or_else(|| {
        let at = anchor(rows, range.start, Side::Left);
        at..at
    });
    let right = right.unwrap_or_else(|| {
        let at = anchor(rows, range.start, Side::Right);
        at..at
    });

    Hunk {
        rows: range,
        left,
        right,
    }
}

/// Insertion point on `side` for a hunk that has no lines there: one past the
/// last line before the hunk.
fn anchor(rows: &[DiffRow], row_start: usize, side: Side) -> usize {
    rows[..row_start]
        .iter()
        .rev()
        .find_map(|r| r.side(side))
        .map_or(0, |l| l + 1)
}

#[cfg(test)]
mod tests {
    use super::super::options::WhitespaceMode;
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        if s.is_empty() {
            vec![]
        } else {
            s.lines().map(str::to_owned).collect()
        }
    }

    fn run(a: &str, b: &str, opts: &DiffOptions) -> DiffResult {
        diff_lines(&lines(a), &lines(b), opts, Budget::unlimited())
    }

    #[test]
    fn identical_documents_have_no_hunks() {
        let d = run("a\nb\nc", "a\nb\nc", &DiffOptions::default());
        assert!(d.hunks.is_empty());
        assert!(d.stats.is_identical());
        assert_eq!(d.stats.similarity, 1.0);
        assert_eq!(d.rows.len(), 3);
    }

    #[test]
    fn pure_insertion() {
        let d = run("a\nc", "a\nb\nc", &DiffOptions::default());
        assert_eq!(d.stats.added, 1);
        assert_eq!(d.stats.removed, 0);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].left, 1..1, "insertion anchors after left line 0");
        assert_eq!(d.hunks[0].right, 1..2);
    }

    #[test]
    fn pure_deletion() {
        let d = run("a\nb\nc", "a\nc", &DiffOptions::default());
        assert_eq!(d.stats.removed, 1);
        assert_eq!(d.hunks[0].left, 1..2);
        assert_eq!(d.hunks[0].right, 1..1);
    }

    #[test]
    fn every_line_maps_to_exactly_one_row() {
        let d = run("a\nb\nc\nd", "a\nx\nd\ne", &DiffOptions::default());
        for (line, &row) in d.row_of_left.iter().enumerate() {
            assert_ne!(row, NONE, "left line {line} was never emitted");
            assert_eq!(d.rows[row as usize].left(), Some(line));
        }
        for (line, &row) in d.row_of_right.iter().enumerate() {
            assert_ne!(row, NONE, "right line {line} was never emitted");
            assert_eq!(d.rows[row as usize].right(), Some(line));
        }
    }

    #[test]
    fn smart_align_pairs_the_edited_line() {
        // Positional pairing would put "a brand new statement" opposite the
        // edited `let x`; similarity alignment should not.
        let a = "fn main() {\n    let x = compute(a, b);\n}";
        let b = "fn main() {\n    a brand new statement;\n    let x = compute(a, c);\n}";
        let d = run(a, b, &DiffOptions::default());

        let replace: Vec<_> = d
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Replace)
            .collect();
        assert_eq!(replace.len(), 1, "exactly one line should read as modified");
        assert_eq!(replace[0].left(), Some(1));
        assert_eq!(replace[0].right(), Some(2));
        assert_eq!(d.stats.added, 1);
    }

    #[test]
    fn positional_fallback_when_smart_align_is_off() {
        let opts = DiffOptions {
            smart_align: false,
            ..Default::default()
        };
        let a = "fn main() {\n    let x = compute(a, b);\n}";
        let b = "fn main() {\n    a brand new statement;\n    let x = compute(a, c);\n}";
        let d = run(a, b, &opts);
        // Positional pairing lines up line 1 with line 1 regardless of content.
        let replace: Vec<_> = d
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Replace)
            .collect();
        assert_eq!(replace[0].right(), Some(1));
    }

    #[test]
    fn ignore_case_and_whitespace() {
        let opts = DiffOptions {
            ignore_case: true,
            whitespace: WhitespaceMode::IgnoreAmount,
            ..Default::default()
        };
        let d = run("Hello   World", "hello world", &opts);
        assert!(d.stats.is_identical(), "{:?}", d.stats);
    }

    #[test]
    fn ignore_blank_lines_keeps_every_line_visible() {
        let opts = DiffOptions {
            ignore_blank_lines: true,
            ..Default::default()
        };
        let d = run("a\n\n\nb", "a\nb", &opts);
        assert!(d.stats.is_identical(), "{:?}", d.stats);
        // The blank lines still occupy rows so the editor can render them.
        assert_eq!(d.rows.iter().filter(|r| r.left().is_some()).count(), 4);
        assert_eq!(d.rows.iter().filter(|r| r.right().is_some()).count(), 2);
    }

    #[test]
    fn empty_sides() {
        let d = run("", "a\nb", &DiffOptions::default());
        assert_eq!(d.stats.added, 2);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].left, 0..0);

        let d = run("a\nb", "", &DiffOptions::default());
        assert_eq!(d.stats.removed, 2);
        assert_eq!(d.hunks[0].right, 0..0);

        let d = run("", "", &DiffOptions::default());
        assert!(d.rows.is_empty());
        assert_eq!(d.stats.similarity, 1.0);
    }

    #[test]
    fn hunks_are_sorted_and_disjoint() {
        let a: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        b[10] = "changed".into();
        b[50] = "changed too".into();
        b.remove(120);
        b.insert(150, "inserted".into());

        let d = diff_lines(&a, &b, &DiffOptions::default(), Budget::unlimited());
        assert!(d.hunks.len() >= 4);
        for w in d.hunks.windows(2) {
            assert!(w[0].rows.end <= w[1].rows.start, "hunks overlap");
        }
        for (i, h) in d.hunks.iter().enumerate() {
            assert_eq!(d.hunk_at_row(h.rows.start), Some(i));
        }
    }

    #[test]
    fn ignored_rows_do_not_split_a_hunk() {
        let opts = DiffOptions {
            ignore_blank_lines: true,
            ..Default::default()
        };
        let d = run("x\n\ny", "p\n\nq", &opts);
        assert_eq!(d.hunks.len(), 1, "the blank line must not split the hunk");
    }

    #[test]
    fn a_single_document_is_one_row_per_line() {
        let d = single_document(5);
        assert_eq!(d.rows.len(), 5);
        assert!(d.hunks.is_empty());
        assert!(d.stats.is_identical());
        for line in 0..5 {
            let row = d.row_of(Side::Left, line).expect("every line has a row");
            assert_eq!(row, line, "rows must map 1:1 in single-document mode");
            assert_eq!(d.rows[row].left(), Some(line));
            assert_eq!(d.rows[row].right(), None, "there is no other side");
            assert_eq!(d.rows[row].kind, RowKind::Equal, "nothing should be tinted");
        }
    }

    #[test]
    fn an_empty_single_document_is_safe() {
        let d = single_document(0);
        assert!(d.rows.is_empty());
        assert_eq!(d.row_of(Side::Left, 0), None);
    }

    #[test]
    fn hunk_navigation_walks_every_hunk() {
        let d = run("a\nb\nc\nd\ne", "a\nB\nc\nD\ne", &DiffOptions::default());
        assert_eq!(d.hunks.len(), 2);
        let first = d.hunk_after(0).unwrap();
        assert_eq!(first, 1.min(first));
        assert_eq!(d.hunk_before(d.rows.len()), Some(d.hunks.len() - 1));
        assert_eq!(d.hunk_before(0), None);
    }
}

/// A one-line description of a hunk, for the jump list in the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkSummary {
    /// Zero-based line the jump list should label this entry with, taken from
    /// whichever side actually has content.
    pub line: usize,
    pub side: Side,
    pub kind: RowKind,
    /// Condensed content of the first changed line: indentation stripped,
    /// internal whitespace collapsed, and cut to a readable length. A list of
    /// twenty entries is only useful if each fits on one line.
    pub text: String,
    /// How many rows the hunk spans, so a large block can say so.
    pub rows: usize,
}

/// Longest summary text kept, in characters.
const SUMMARY_LEN: usize = 60;

impl DiffResult {
    /// Describe one hunk for the jump list.
    pub fn summarize_hunk(
        &self,
        index: usize,
        left: &[String],
        right: &[String],
    ) -> Option<HunkSummary> {
        let hunk = self.hunks.get(index)?;

        // The first row that actually changed - a hunk can open with an
        // `Ignored` blank line, and describing the hunk by that would be
        // useless.
        let row = self.rows[hunk.rows.clone()]
            .iter()
            .find(|r| r.kind.is_change())?;

        // Prefer the side that has the content. For a modification either side
        // works; the left is the "before", which reads more naturally as a
        // label for where you are going.
        let (side, line) = match (row.left(), row.right()) {
            (Some(l), _) => (Side::Left, l),
            (None, Some(r)) => (Side::Right, r),
            (None, None) => return None,
        };
        let source = if side == Side::Left { left } else { right };

        Some(HunkSummary {
            line,
            side,
            kind: row.kind,
            text: condense(source.get(line).map_or("", String::as_str)),
            rows: hunk.rows.len(),
        })
    }

    /// Summaries for every hunk, in document order.
    pub fn hunk_summaries(&self, left: &[String], right: &[String]) -> Vec<HunkSummary> {
        (0..self.hunks.len())
            .filter_map(|i| self.summarize_hunk(i, left, right))
            .collect()
    }
}

/// Squash a line down to something that fits on one row of a list.
fn condense(line: &str) -> String {
    let mut out = String::with_capacity(SUMMARY_LEN);
    let mut chars = 0usize;
    let mut pending_space = false;

    for c in line.trim().chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            chars += 1;
            pending_space = false;
        }
        if chars >= SUMMARY_LEN {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
        chars += 1;
    }
    out
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use crate::core::diff::options::DiffOptions;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    fn setup(a: &str, b: &str) -> (Vec<String>, Vec<String>, DiffResult) {
        let (l, r) = (lines(a), lines(b));
        let d = diff_lines(&l, &r, &DiffOptions::default(), Budget::unlimited());
        (l, r, d)
    }

    #[test]
    fn a_modification_is_labelled_by_its_left_line() {
        let (l, r, d) = setup("a\n    let x = 1;\nc", "a\n    let x = 2;\nc");
        let s = d.summarize_hunk(0, &l, &r).expect("one hunk");
        assert_eq!(s.side, Side::Left);
        assert_eq!(s.line, 1);
        assert_eq!(s.kind, RowKind::Replace);
        assert_eq!(s.text, "let x = 1;", "indentation should be stripped");
    }

    #[test]
    fn an_insertion_is_labelled_from_the_right() {
        let (l, r, d) = setup("a\nc", "a\nbrand new line\nc");
        let s = d.summarize_hunk(0, &l, &r).expect("one hunk");
        assert_eq!(s.side, Side::Right);
        assert_eq!(s.kind, RowKind::Insert);
        assert_eq!(s.text, "brand new line");
    }

    #[test]
    fn a_deletion_is_labelled_from_the_left() {
        let (l, r, d) = setup("a\ngone forever\nc", "a\nc");
        let s = d.summarize_hunk(0, &l, &r).expect("one hunk");
        assert_eq!(s.side, Side::Left);
        assert_eq!(s.kind, RowKind::Delete);
        assert_eq!(s.text, "gone forever");
    }

    #[test]
    fn internal_whitespace_is_collapsed() {
        assert_eq!(condense("  fn  foo (a,   b)  "), "fn foo (a, b)");
        assert_eq!(condense("\t\tindented"), "indented");
        assert_eq!(condense(""), "");
        assert_eq!(condense("   "), "");
    }

    #[test]
    fn a_long_line_is_cut_with_an_ellipsis() {
        let long = "x".repeat(200);
        let text = condense(&long);
        assert!(text.chars().count() <= SUMMARY_LEN + 1);
        assert!(text.ends_with('\u{2026}'));
    }

    #[test]
    fn cutting_lands_on_a_character_boundary() {
        let long = "对比工具".repeat(40);
        let text = condense(&long);
        // Slicing a broken boundary would have panicked in `condense` already,
        // but assert the result is valid and bounded.
        assert!(text.chars().count() <= SUMMARY_LEN + 1);
        assert!(text.starts_with("对比工具"));
    }

    #[test]
    fn every_hunk_gets_a_summary() {
        let a = "one\ntwo\nthree\nfour\nfive\nsix\nseven";
        let b = "one\nTWO\nthree\nfour\ninserted\nfive\nsix";
        let (l, r, d) = setup(a, b);
        let all = d.hunk_summaries(&l, &r);
        assert_eq!(all.len(), d.hunks.len());
        assert!(all.len() >= 2);
        for s in &all {
            assert!(s.kind.is_change(), "a summary must describe a change");
            assert!(s.rows >= 1);
        }
        // Entries must be in document order, or the list is confusing.
        for w in all.windows(2) {
            assert!(w[0].line <= w[1].line || w[0].side != w[1].side);
        }
    }

    #[test]
    fn an_identical_document_has_no_entries() {
        let (l, r, d) = setup("a\nb", "a\nb");
        assert!(d.hunk_summaries(&l, &r).is_empty());
        assert_eq!(d.summarize_hunk(0, &l, &r), None);
    }

    #[test]
    fn an_out_of_range_index_is_none() {
        let (l, r, d) = setup("a", "b");
        assert!(d.summarize_hunk(99, &l, &r).is_none());
    }

    #[test]
    fn a_blank_line_change_still_produces_an_entry() {
        let (l, r, d) = setup("a\n\nc", "a\nx\nc");
        let s = d.summarize_hunk(0, &l, &r).expect("one hunk");
        // The left line is empty, so the text is empty - the caller shows the
        // line number and the kind, which is enough.
        assert!(s.text.is_empty() || s.text == "x");
    }
}
