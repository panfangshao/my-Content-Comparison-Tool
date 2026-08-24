//! Unified-diff export - the format `patch(1)` and `git apply` understand.
//!
//! Built from the aligned row list rather than re-running a diff, so what you
//! export is exactly what you were looking at, including the effect of the
//! ignore-whitespace / ignore-case options.

use super::engine::{DiffResult, RowKind};

/// Number of unchanged lines kept around each change. Three is the `diff -u`
/// default and what code review tools expect.
pub const DEFAULT_CONTEXT: usize = 3;

pub struct UnifiedOptions<'a> {
    pub left_label: &'a str,
    pub right_label: &'a str,
    pub context: usize,
}

impl Default for UnifiedOptions<'_> {
    fn default() -> Self {
        Self {
            left_label: "a",
            right_label: "b",
            context: DEFAULT_CONTEXT,
        }
    }
}

/// Render `diff` as a unified diff over the two documents.
///
/// Returns an empty string when the documents are identical, matching `diff -u`.
pub fn to_unified(
    diff: &DiffResult,
    left: &[String],
    right: &[String],
    opts: &UnifiedOptions<'_>,
) -> String {
    if diff.hunks.is_empty() {
        return String::new();
    }

    // Merge hunks whose context windows touch, so we do not emit two headers
    // for changes three lines apart.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for h in &diff.hunks {
        let start = h.rows.start.saturating_sub(opts.context);
        let end = (h.rows.end + opts.context).min(diff.rows.len());
        match windows.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => windows.push((start, end)),
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- {}\n", opts.left_label));
    out.push_str(&format!("+++ {}\n", opts.right_label));

    for (start, end) in windows {
        emit_window(&mut out, diff, left, right, start..end);
    }
    out
}

fn emit_window(
    out: &mut String,
    diff: &DiffResult,
    left: &[String],
    right: &[String],
    rows: std::ops::Range<usize>,
) {
    let window = &diff.rows[rows.clone()];

    // Hunk header ranges. A side with no lines in the window still needs a
    // position: `diff -u` uses "lines before it", with a count of zero.
    let (l_start, l_count) = side_range(window.iter().filter_map(|r| r.left()));
    let (r_start, r_count) = side_range(window.iter().filter_map(|r| r.right()));

    let l_start = l_start.unwrap_or_else(|| lines_before(diff, rows.start, true));
    let r_start = r_start.unwrap_or_else(|| lines_before(diff, rows.start, false));

    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        if l_count == 0 { l_start } else { l_start + 1 },
        l_count,
        if r_count == 0 { r_start } else { r_start + 1 },
        r_count,
    ));

    // Within a change block, unified diff lists every removal before every
    // addition, so buffer them and flush at the next unchanged line.
    let mut dels: Vec<&str> = Vec::new();
    let mut inss: Vec<&str> = Vec::new();

    let flush = |out: &mut String, dels: &mut Vec<&str>, inss: &mut Vec<&str>| {
        for l in dels.drain(..) {
            out.push('-');
            out.push_str(l);
            out.push('\n');
        }
        for l in inss.drain(..) {
            out.push('+');
            out.push_str(l);
            out.push('\n');
        }
    };

    for row in window {
        match row.kind {
            RowKind::Equal | RowKind::Ignored => {
                flush(out, &mut dels, &mut inss);
                // An `Ignored` row can be one-sided (a blank line that only one
                // document has); emit it from whichever side has it, so the
                // patch still applies cleanly.
                if let Some(l) = row.left() {
                    out.push(' ');
                    out.push_str(&left[l]);
                    out.push('\n');
                } else if let Some(r) = row.right() {
                    out.push(' ');
                    out.push_str(&right[r]);
                    out.push('\n');
                }
            }
            RowKind::Delete => dels.push(&left[row.left().expect("delete row has a left line")]),
            RowKind::Insert => inss.push(&right[row.right().expect("insert row has a right line")]),
            RowKind::Replace => {
                dels.push(&left[row.left().expect("replace row has a left line")]);
                inss.push(&right[row.right().expect("replace row has a right line")]);
            }
        }
    }
    flush(out, &mut dels, &mut inss);
}

/// `(first line index, count)` for one side of a window.
fn side_range(mut it: impl Iterator<Item = usize>) -> (Option<usize>, usize) {
    let Some(first) = it.next() else {
        return (None, 0);
    };
    (Some(first), 1 + it.count())
}

/// How many lines of one side precede `row` - the anchor for an empty range.
fn lines_before(diff: &DiffResult, row: usize, left_side: bool) -> usize {
    diff.rows[..row]
        .iter()
        .rev()
        .find_map(|r| if left_side { r.left() } else { r.right() })
        .map_or(0, |l| l + 1)
}

#[cfg(test)]
mod tests {
    use super::super::engine::{Budget, diff_lines};
    use super::super::options::DiffOptions;
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    fn unified(a: &str, b: &str) -> String {
        let (l, r) = (lines(a), lines(b));
        let d = diff_lines(&l, &r, &DiffOptions::default(), Budget::unlimited());
        to_unified(
            &d,
            &l,
            &r,
            &UnifiedOptions {
                left_label: "a/x.txt",
                right_label: "b/x.txt",
                context: 1,
            },
        )
    }

    #[test]
    fn identical_documents_export_nothing() {
        assert_eq!(unified("a\nb", "a\nb"), "");
    }

    #[test]
    fn single_replacement() {
        let out = unified("a\nb\nc", "a\nB\nc");
        assert_eq!(
            out,
            "--- a/x.txt\n+++ b/x.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        );
    }

    #[test]
    fn removals_precede_additions() {
        let out = unified("a\nb\nc\nd", "a\nB\nC\nd");
        let body: Vec<&str> = out.lines().skip(3).collect();
        assert_eq!(body, vec![" a", "-b", "-c", "+B", "+C", " d"]);
    }

    #[test]
    fn pure_insertion_uses_a_zero_length_left_range() {
        let out = unified("a\nc", "a\nb\nc");
        assert!(out.contains("@@ -1,2 +1,3 @@"), "{out}");
        assert!(out.contains("\n+b\n"), "{out}");
    }

    #[test]
    fn insertion_at_the_very_top() {
        let out = unified("a", "x\na");
        assert!(out.contains("@@ -1,1 +1,2 @@"), "{out}");
    }

    #[test]
    fn distant_changes_get_separate_headers() {
        let a: Vec<String> = (0..40).map(|i| format!("l{i}")).collect();
        let mut b = a.clone();
        b[2] = "changed".into();
        b[30] = "changed".into();
        let d = diff_lines(&a, &b, &DiffOptions::default(), Budget::unlimited());
        let out = to_unified(&d, &a, &b, &UnifiedOptions::default());
        assert_eq!(out.matches("@@").count() / 2, 2, "{out}");
    }

    #[test]
    fn nearby_changes_are_merged_into_one_header() {
        let a: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut b = a.clone();
        b[5] = "changed".into();
        b[6] = "changed too".into();
        let d = diff_lines(&a, &b, &DiffOptions::default(), Budget::unlimited());
        let out = to_unified(&d, &a, &b, &UnifiedOptions::default());
        assert_eq!(out.matches("@@").count() / 2, 1, "{out}");
    }

    #[test]
    fn every_body_line_carries_a_marker() {
        let out = unified("a\nb\nc\nd\ne", "a\nX\nc\nY\ne");
        for line in out.lines().skip(2) {
            assert!(
                line.starts_with([' ', '-', '+', '@']),
                "unprefixed line: {line:?}"
            );
        }
    }
}
