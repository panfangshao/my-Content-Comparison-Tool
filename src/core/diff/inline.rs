//! Intra-line diff: given a `Replace` row, work out *which part* of the line
//! actually changed so the editor can tint only those characters.
//!
//! This runs lazily, only for rows currently on screen, and the results are
//! cached by the caller. Running it eagerly over a 100k-line file would cost
//! far more than the line diff itself.

use similar::ChangeTag;

use super::options::{DiffOptions, Granularity};

/// A byte range within one line that differs from the other side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[inline]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// Changed spans for a line pair: `left` indexes into the left line, `right`
/// into the right line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineDiff {
    pub left: Vec<Span>,
    pub right: Vec<Span>,
}

impl InlineDiff {
    /// True when the whole line should be tinted rather than individual spans.
    /// Happens at `Granularity::Line`, and when the two lines share nothing.
    pub fn is_whole_line(&self) -> bool {
        self.left.is_empty() && self.right.is_empty()
    }
}

/// Lines longer than this skip inline analysis: a word diff over a minified
/// bundle line is quadratic-ish and the result is unreadable anyway.
const MAX_INLINE_LEN: usize = 8 * 1024;

/// Compute the changed spans between two versions of a line.
///
/// Returns an empty [`InlineDiff`] (meaning "tint the whole line") when the
/// granularity is [`Granularity::Line`], when either line is too long, or when
/// the two lines have nothing in common.
pub fn inline_diff(left: &str, right: &str, opts: &DiffOptions) -> InlineDiff {
    if opts.granularity == Granularity::Line
        || left.len() > MAX_INLINE_LEN
        || right.len() > MAX_INLINE_LEN
    {
        return InlineDiff::default();
    }

    let alg = opts.algorithm.to_similar();
    let changes: Vec<(ChangeTag, &str)> = match opts.granularity {
        Granularity::Char => similar::utils::diff_chars(alg, left, right),
        // `diff_unicode_words` splits on Unicode word boundaries, which keeps
        // CJK text from being treated as one enormous word.
        Granularity::Word => similar::utils::diff_unicode_words(alg, left, right),
        Granularity::Line => unreachable!("handled above"),
    };

    // The returned slices are contiguous and in order for each side, so byte
    // offsets fall out of a running total - no pointer arithmetic needed.
    let mut out = InlineDiff::default();
    let (mut lpos, mut rpos) = (0usize, 0usize);
    let mut shared = 0usize;

    for (tag, text) in changes {
        let n = text.len();
        match tag {
            ChangeTag::Equal => {
                // Only non-whitespace counts as "shared". Two unrelated lines
                // still match on their spaces, and letting that count would
                // keep the confetti fallback below from ever firing.
                shared += significant_len(text);
                lpos += n;
                rpos += n;
            }
            ChangeTag::Delete => {
                push_merged(&mut out.left, lpos, lpos + n);
                lpos += n;
            }
            ChangeTag::Insert => {
                push_merged(&mut out.right, rpos, rpos + n);
                rpos += n;
            }
        }
    }

    // If the lines share almost nothing, per-span highlighting turns into
    // confetti. Fall back to tinting the whole line.
    let total = significant_len(left) + significant_len(right);
    if total > 0 && (2 * shared) as f32 / (total as f32) < 0.15 {
        return InlineDiff::default();
    }

    out
}

/// Byte length ignoring whitespace.
fn significant_len(s: &str) -> usize {
    s.bytes().filter(|b| !b.is_ascii_whitespace()).count()
}

/// Append a span, merging it with the previous one when they touch.
fn push_merged(spans: &mut Vec<Span>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.end == start => last.end = end,
        _ => spans.push(Span { start, end }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(g: Granularity) -> DiffOptions {
        DiffOptions {
            granularity: g,
            ..Default::default()
        }
    }

    fn slices<'a>(line: &'a str, spans: &[Span]) -> Vec<&'a str> {
        spans.iter().map(|s| &line[s.start..s.end]).collect()
    }

    #[test]
    fn word_level_isolates_the_changed_word() {
        let (a, b) = ("the quick brown fox", "the quick red fox");
        let d = inline_diff(a, b, &opts(Granularity::Word));
        assert_eq!(slices(a, &d.left), vec!["brown"]);
        assert_eq!(slices(b, &d.right), vec!["red"]);
    }

    #[test]
    fn char_level_isolates_the_changed_char() {
        let (a, b) = ("compute(a, b)", "compute(a, c)");
        let d = inline_diff(a, b, &opts(Granularity::Char));
        assert_eq!(slices(a, &d.left), vec!["b"]);
        assert_eq!(slices(b, &d.right), vec!["c"]);
    }

    #[test]
    fn line_granularity_returns_whole_line() {
        let d = inline_diff("abc", "abd", &opts(Granularity::Line));
        assert!(d.is_whole_line());
    }

    #[test]
    fn unrelated_lines_fall_back_to_whole_line() {
        let d = inline_diff(
            "fn main() { println!(\"hi\"); }",
            "SELECT * FROM users;",
            &opts(Granularity::Word),
        );
        assert!(d.is_whole_line(), "expected whole-line fallback: {d:?}");
    }

    #[test]
    fn spans_are_in_bounds_and_ordered() {
        let (a, b) = (
            "alpha beta gamma delta epsilon",
            "alpha BETA gamma DELTA epsilon",
        );
        let d = inline_diff(a, b, &opts(Granularity::Word));
        for spans in [&d.left, &d.right] {
            for w in spans.windows(2) {
                assert!(w[0].end <= w[1].start, "spans out of order: {spans:?}");
            }
        }
        assert!(d.left.iter().all(|s| s.end <= a.len()));
        assert!(d.right.iter().all(|s| s.end <= b.len()));
        assert_eq!(slices(a, &d.left), vec!["beta", "delta"]);
    }

    #[test]
    fn cjk_is_split_into_words_not_one_blob() {
        let (a, b) = ("这是一个测试文本", "这是另一个测试文本");
        let d = inline_diff(a, b, &opts(Granularity::Char));
        assert!(!d.is_whole_line());
        // Every span must land on a char boundary or the slicing below panics.
        for s in d.right.iter() {
            assert!(b.is_char_boundary(s.start) && b.is_char_boundary(s.end));
        }
    }

    #[test]
    fn very_long_lines_are_skipped() {
        let a = "x".repeat(MAX_INLINE_LEN + 1);
        let b = format!("{}y", "x".repeat(MAX_INLINE_LEN));
        assert!(inline_diff(&a, &b, &opts(Granularity::Char)).is_whole_line());
    }

    #[test]
    fn identical_lines_produce_no_spans() {
        let d = inline_diff("same", "same", &opts(Granularity::Word));
        assert!(d.left.is_empty() && d.right.is_empty());
    }
}
