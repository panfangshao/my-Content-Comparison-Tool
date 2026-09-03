//! End-to-end checks on the composition that the UI actually performs:
//! compare -> build patches -> apply them to a real buffer -> compare again.
//!
//! The unit tests cover each stage in isolation; these cover the seam between
//! them, which is where an off-by-one in line ranges would show up.

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::merge::{self, Direction};
use duibi::core::text::TextBuffer;

fn compare(a: &TextBuffer, b: &TextBuffer) -> DiffResult {
    diff_lines(
        a.lines(),
        b.lines(),
        &DiffOptions::default(),
        Budget::unlimited(),
    )
}

/// Apply one hunk the way the merge arrow does.
fn merge_hunk(left: &mut TextBuffer, right: &mut TextBuffer, index: usize, dir: Direction) {
    let diff = compare(left, right);
    let hunk = diff.hunks[index].clone();
    let source = match dir.source() {
        Side::Left => left.lines().to_vec(),
        Side::Right => right.lines().to_vec(),
    };
    let patch = merge::hunk_patch(&hunk, &source, dir);
    let target = match dir.target() {
        Side::Left => left,
        Side::Right => right,
    };
    target.replace_lines(patch.range, &patch.lines);
}

/// Apply every hunk the way "accept all" does: compute the finished document,
/// then write it once.
fn merge_all(left: &mut TextBuffer, right: &mut TextBuffer, dir: Direction) -> usize {
    let diff = compare(left, right);
    let source = match dir.source() {
        Side::Left => left.lines().to_vec(),
        Side::Right => right.lines().to_vec(),
    };
    let patches = merge::all_patches(&diff, &source, dir);
    let n = patches.len();

    let target = match dir.target() {
        Side::Left => left,
        Side::Right => right,
    };
    let merged = merge::preview(target.lines(), &patches);
    let len = target.len_lines();
    target.replace_lines(0..len, &merged);
    n
}

const OLD: &str = "\
use std::collections::HashMap;

pub struct Metrics {
    counts: HashMap<String, u64>,
    total: u64,
}

impl Metrics {
    pub fn record(&mut self, key: &str) {
        *self.counts.entry(key.into()).or_insert(0) += 1;
    }
}
";

const NEW: &str = "\
use std::collections::BTreeMap;

/// Stable ordering for reports.
pub struct Metrics {
    counts: BTreeMap<String, u64>,
    total: u64,
    slowest: u64,
}

impl Metrics {
    pub fn record(&mut self, key: &str, ms: u64) {
        *self.counts.entry(key.into()).or_insert(0) += 1;
        self.slowest = self.slowest.max(ms);
    }
}
";

#[test]
fn accept_all_rightwards_makes_the_sides_identical() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);
    assert!(!compare(&left, &right).stats.is_identical());

    let n = merge_all(&mut left, &mut right, Direction::ToRight);
    assert!(n > 0);

    let after = compare(&left, &right);
    assert!(after.stats.is_identical(), "left != right: {:?}", after.stats);
    assert_eq!(left.lines(), right.lines());
}

#[test]
fn accept_all_leftwards_makes_the_sides_identical() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);

    merge_all(&mut left, &mut right, Direction::ToLeft);
    let after = compare(&left, &right);
    assert!(after.stats.is_identical(), "{:?}", after.stats);
}

#[test]
fn accept_all_is_a_single_undo_step() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);
    let before = right.text();

    merge_all(&mut left, &mut right, Direction::ToRight);
    assert_ne!(right.text(), before);

    assert!(right.undo());
    assert_eq!(
        right.text(),
        before,
        "one Ctrl+Z must take back the whole merge"
    );
    assert!(!right.can_undo(), "no leftover steps");
}

#[test]
fn merging_hunks_one_at_a_time_converges() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);

    // Always take the first remaining hunk: after each merge the comparison is
    // rebuilt, exactly as the app does it.
    let mut guard = 0;
    while !compare(&left, &right).stats.is_identical() {
        merge_hunk(&mut left, &mut right, 0, Direction::ToRight);
        guard += 1;
        assert!(guard < 50, "merging one hunk at a time did not converge");
    }
    assert_eq!(left.lines(), right.lines());
    assert!(guard > 1, "the fixture should need several merges");
}

#[test]
fn merging_the_last_hunk_first_also_converges() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);

    let mut guard = 0;
    loop {
        let diff = compare(&left, &right);
        if diff.stats.is_identical() {
            break;
        }
        merge_hunk(&mut left, &mut right, diff.hunks.len() - 1, Direction::ToLeft);
        guard += 1;
        assert!(guard < 50, "did not converge merging from the end");
    }
    assert_eq!(left.lines(), right.lines());
}

#[test]
fn merging_into_an_empty_side_works() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::new();

    merge_all(&mut left, &mut right, Direction::ToRight);
    assert_eq!(left.lines(), right.lines());
    assert!(compare(&left, &right).stats.is_identical());
}

#[test]
fn merging_from_an_empty_side_clears_the_other() {
    let mut left = TextBuffer::new();
    let mut right = TextBuffer::from_text(NEW);

    merge_all(&mut left, &mut right, Direction::ToRight);
    assert!(
        compare(&left, &right).stats.is_identical(),
        "right should have been emptied to match left"
    );
}

#[test]
fn a_merge_leaves_the_line_cache_consistent() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);
    merge_all(&mut left, &mut right, Direction::ToRight);

    let from_rope: Vec<String> = right
        .rope()
        .lines()
        .map(|l| {
            let s = l.to_string();
            s.strip_suffix('\n').unwrap_or(&s).to_owned()
        })
        .collect();
    assert_eq!(right.lines(), from_rope.as_slice());
}

/// Merging must survive documents that differ in whether they end with a
/// newline - the classic source of trailing-line bugs.
#[test]
fn trailing_newline_differences_merge_cleanly() {
    for (a, b) in [
        ("one\ntwo\n", "one\ntwo"),
        ("one\ntwo", "one\ntwo\n"),
        ("one\n", "one\n\n\n"),
    ] {
        let mut left = TextBuffer::from_text(a);
        let mut right = TextBuffer::from_text(b);
        merge_all(&mut left, &mut right, Direction::ToRight);
        assert_eq!(
            left.lines(),
            right.lines(),
            "merging {a:?} onto {b:?} left them different"
        );
    }
}

/// The full loop the UI runs when a user merges a hunk, takes it back, and
/// then pulls changes across the other way: merge -> undo -> compare again ->
/// merge the other direction. The undo has to restore not just the text but
/// the dirty flag (the save point in the history), and the re-comparison has
/// to reproduce the first result exactly - any drift means undo left the
/// line cache inconsistent.
#[test]
fn merge_undo_recompare_then_merge_the_other_way_converges() {
    let mut left = TextBuffer::from_text(OLD);
    let mut right = TextBuffer::from_text(NEW);

    let first = compare(&left, &right);
    assert!(first.hunks.len() >= 2, "the fixture should offer several hunks");
    // DiffRow and Hunk carry no PartialEq; their Debug form is the snapshot.
    let first_rows = format!("{:?}", first.rows);
    let first_hunks = format!("{:?}", first.hunks);

    // Merge the first hunk rightwards, like clicking its arrow.
    let original_right = right.text();
    merge_hunk(&mut left, &mut right, 0, Direction::ToRight);
    assert!(right.is_dirty(), "a merge edits the target document");

    // Take it back. The text returns to what it was, and because a freshly
    // loaded document sits on its history's save point, the dirty flag clears
    // too - the title bar loses its bullet.
    assert!(right.undo());
    assert_eq!(right.text(), original_right);
    assert!(!right.is_dirty(), "undo back to the save point reads clean");

    // Comparing again must produce exactly the first result.
    let second = compare(&left, &right);
    assert_eq!(format!("{:?}", second.rows), first_rows, "rows drifted");
    assert_eq!(format!("{:?}", second.hunks), first_hunks, "hunks drifted");

    // Now pull the same first hunk the other way, then finish leftwards.
    merge_hunk(&mut left, &mut right, 0, Direction::ToLeft);
    merge_all(&mut left, &mut right, Direction::ToLeft);

    let after = compare(&left, &right);
    assert!(after.stats.is_identical(), "{:?}", after.stats);
    assert_eq!(left.lines(), right.lines());
}

/// The unified export must describe the same change the merge would make.
#[test]
fn the_exported_patch_matches_what_merging_does() {
    let left = TextBuffer::from_text(OLD);
    let right = TextBuffer::from_text(NEW);
    let diff = compare(&left, &right);

    let patch = duibi::core::diff::to_unified(
        &diff,
        left.lines(),
        right.lines(),
        &duibi::core::diff::UnifiedOptions::default(),
    );

    // Every removed line should be a left line, every added line a right line.
    for line in patch.lines().skip(2) {
        if let Some(rest) = line.strip_prefix('-') {
            assert!(
                left.lines().iter().any(|l| l == rest),
                "patch removes a line that is not on the left: {rest:?}"
            );
        } else if let Some(rest) = line.strip_prefix('+') {
            assert!(
                right.lines().iter().any(|l| l == rest),
                "patch adds a line that is not on the right: {rest:?}"
            );
        }
    }
    assert!(patch.contains("BTreeMap"));
}
