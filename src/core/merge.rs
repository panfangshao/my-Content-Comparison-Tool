//! Merging a hunk from one side into the other.
//!
//! Merges are expressed as [`LinePatch`]es rather than applied directly, which
//! keeps the logic testable without a buffer and lets the caller decide how to
//! group them for undo.
//!
//! # Three-way merge
//!
//! The current UI is two-way, but nothing here assumes that. A hunk already
//! carries an independent line range per side, so a future third pane means
//! adding a `Base` variant to [`Side`](crate::core::diff::Side) and resolving
//! conflicts before building the patches - the patch representation and the
//! reverse-order application below stay exactly as they are.

use std::ops::Range;

use super::diff::{DiffResult, Hunk, Side};

/// Which way the content flows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Take the left side's version and write it into the right document.
    ToRight,
    /// Take the right side's version and write it into the left document.
    ToLeft,
}

impl Direction {
    #[inline]
    pub fn source(self) -> Side {
        match self {
            Self::ToRight => Side::Left,
            Self::ToLeft => Side::Right,
        }
    }

    #[inline]
    pub fn target(self) -> Side {
        self.source().other()
    }
}

/// "Replace these lines of the target with these lines."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinePatch {
    pub range: Range<usize>,
    pub lines: Vec<String>,
}

/// The patch that makes one hunk of the target match the source.
///
/// A pure insertion yields an empty `range` (splice in place); a pure deletion
/// yields empty `lines`.
pub fn hunk_patch(hunk: &Hunk, source_lines: &[String], dir: Direction) -> LinePatch {
    let src = hunk.lines(dir.source());
    let dst = hunk.lines(dir.target());

    // Clamp against the source document: hunks are derived from it, so this is
    // belt-and-braces against a stale diff being handed to us.
    let start = src.start.min(source_lines.len());
    let end = src.end.min(source_lines.len()).max(start);

    LinePatch {
        range: dst,
        lines: source_lines[start..end].to_vec(),
    }
}

/// Patches that make the whole target document match the source.
///
/// Returned **last hunk first**: applying them in this order means each patch
/// only shifts lines that later patches no longer refer to, so the caller can
/// apply them one by one without re-diffing.
pub fn all_patches(diff: &DiffResult, source_lines: &[String], dir: Direction) -> Vec<LinePatch> {
    diff.hunks
        .iter()
        .rev()
        .map(|h| hunk_patch(h, source_lines, dir))
        .collect()
}

/// The lines the target document ends up with, for preview and for tests.
///
/// The patches arrive last-hunk-first and never overlap, so the result is
/// assembled in a single pass from the tail backwards - each untouched gap
/// is copied exactly once. Splicing every patch into one buffer instead
/// would shift the tail once per patch, costing O(hunks x document) on a
/// merge-all preview.
pub fn preview(target_lines: &[String], patches: &[LinePatch]) -> Vec<String> {
    let mut pieces: Vec<&[String]> = Vec::with_capacity(patches.len() * 2 + 1);
    let mut tail = target_lines.len();
    for p in patches {
        let start = p.range.start.min(tail);
        let end = p.range.end.clamp(start, tail);
        pieces.push(&target_lines[end..tail]);
        pieces.push(&p.lines);
        tail = start;
    }
    pieces.push(&target_lines[..tail]);
    let len = pieces.iter().map(|s| s.len()).sum();
    let mut out = Vec::with_capacity(len);
    for piece in pieces.iter().rev() {
        out.extend_from_slice(piece);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::diff::{Budget, DiffOptions, diff_lines};
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        if s.is_empty() {
            vec![]
        } else {
            s.lines().map(str::to_owned).collect()
        }
    }

    fn setup(a: &str, b: &str) -> (Vec<String>, Vec<String>, DiffResult) {
        let (l, r) = (lines(a), lines(b));
        let d = diff_lines(&l, &r, &DiffOptions::default(), Budget::unlimited());
        (l, r, d)
    }

    #[test]
    fn merging_a_modification_rightwards() {
        let (l, r, d) = setup("a\nB\nc", "a\nb\nc");
        let p = hunk_patch(&d.hunks[0], &l, Direction::ToRight);
        assert_eq!(p.range, 1..2);
        assert_eq!(p.lines, vec!["B".to_owned()]);
        assert_eq!(preview(&r, &[p]), lines("a\nB\nc"));
    }

    #[test]
    fn merging_a_modification_leftwards() {
        let (l, r, d) = setup("a\nB\nc", "a\nb\nc");
        let p = hunk_patch(&d.hunks[0], &r, Direction::ToLeft);
        assert_eq!(preview(&l, &[p]), lines("a\nb\nc"));
    }

    #[test]
    fn accepting_an_insertion_adds_the_line() {
        // The right document has an extra line; pull it into the left.
        let (l, r, d) = setup("a\nc", "a\nb\nc");
        let p = hunk_patch(&d.hunks[0], &r, Direction::ToLeft);
        assert_eq!(p.range, 1..1, "nothing to remove on the left");
        assert_eq!(p.lines, vec!["b".to_owned()]);
        assert_eq!(preview(&l, &[p]), lines("a\nb\nc"));
    }

    #[test]
    fn rejecting_an_insertion_removes_the_line() {
        // Push the left (which lacks the line) onto the right.
        let (l, r, d) = setup("a\nc", "a\nb\nc");
        let p = hunk_patch(&d.hunks[0], &l, Direction::ToRight);
        assert!(p.lines.is_empty(), "left has no lines in this hunk");
        assert_eq!(p.range, 1..2);
        assert_eq!(preview(&r, &[p]), lines("a\nc"));
    }

    #[test]
    fn accept_all_makes_the_documents_identical() {
        let a = "one\ntwo\nthree\nfour\nfive\nsix";
        let b = "one\nTWO\nthree\ninserted\nfour\nsix\nseven";
        let (l, r, d) = setup(a, b);
        assert!(d.hunks.len() >= 3);

        let to_right = preview(&r, &all_patches(&d, &l, Direction::ToRight));
        assert_eq!(to_right, l, "right should now equal left");

        let to_left = preview(&l, &all_patches(&d, &r, Direction::ToLeft));
        assert_eq!(to_left, r, "left should now equal right");
    }

    #[test]
    fn accept_all_survives_hunks_that_change_line_counts() {
        let a = "a\nb\nc\nd\ne\nf\ng";
        let b = "a\nX\nY\nZ\nd\ng";
        let (l, r, d) = setup(a, b);
        assert_eq!(preview(&r, &all_patches(&d, &l, Direction::ToRight)), l);
        assert_eq!(preview(&l, &all_patches(&d, &r, Direction::ToLeft)), r);
    }

    #[test]
    fn merging_into_an_empty_document() {
        let (l, r, d) = setup("a\nb\nc", "");
        assert_eq!(preview(&r, &all_patches(&d, &l, Direction::ToRight)), l);
        assert_eq!(preview(&l, &all_patches(&d, &r, Direction::ToLeft)), r);
    }

    #[test]
    fn identical_documents_produce_no_patches() {
        let (l, _r, d) = setup("a\nb", "a\nb");
        assert!(all_patches(&d, &l, Direction::ToRight).is_empty());
    }

    #[test]
    fn direction_sides_are_consistent() {
        assert_eq!(Direction::ToRight.source(), Side::Left);
        assert_eq!(Direction::ToRight.target(), Side::Right);
        assert_eq!(Direction::ToLeft.source(), Side::Right);
        assert_eq!(Direction::ToLeft.target(), Side::Left);
    }
}
