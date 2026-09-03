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
    if opts.granularity == Granularity::Line {
        return InlineDiff::default();
    }

    // Set aside what the two lines already agree on at each end.
    //
    // The length cap below used to be applied to the whole line, so an
    // extended SQL `INSERT` - a megabyte on one line - fell straight through
    // to "tint the whole row", and changing one value near its end showed no
    // mark at all beyond the row tint. Almost all of such a line is shared;
    // comparing bytes from both ends is linear and cheap, and what is left in
    // the middle is the part actually worth diffing.
    let (pre, suf) = common_affixes(left, right);
    let lmid = &left[pre..left.len() - suf];
    let rmid = &right[pre..right.len() - suf];

    // Whatever survives trimming really is different, and a word diff over
    // that is quadratic-ish and unreadable past a point.
    if lmid.len() > MAX_INLINE_LEN || rmid.len() > MAX_INLINE_LEN {
        return InlineDiff::default();
    }

    let alg = opts.algorithm.to_similar();
    let changes: Vec<(ChangeTag, &str)> = match opts.granularity {
        Granularity::Char => similar::utils::diff_chars(alg, lmid, rmid),
        // `diff_unicode_words` splits on Unicode word boundaries, which keeps
        // CJK text from being treated as one enormous word.
        Granularity::Word => similar::utils::diff_unicode_words(alg, lmid, rmid),
        Granularity::Line => unreachable!("handled above"),
    };

    // The returned slices are contiguous and in order for each side, so byte
    // offsets fall out of a running total - no pointer arithmetic needed.
    // They start after the shared prefix, which is where the middles begin.
    let mut out = InlineDiff::default();
    let (mut lpos, mut rpos) = (pre, pre);
    // The trimmed ends are shared by definition, and the confetti test below
    // weighs shared against the *whole* line, so they have to be counted.
    let mut shared =
        significant_len(&left[..pre]) + significant_len(&left[left.len() - suf..]);

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

/// Byte lengths of the prefix and suffix the two lines have in common.
///
/// Both land on character boundaries in *both* strings, and they never
/// overlap, so `a[pre..a.len() - suf]` is always a valid slice of either.
fn common_affixes(a: &str, b: &str) -> (usize, usize) {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let max = ab.len().min(bb.len());

    let mut pre = 0;
    while pre < max && ab[pre] == bb[pre] {
        pre += 1;
    }
    // A shared byte run can stop in the middle of a character; back off to
    // where both strings agree it is safe to cut.
    while pre > 0 && !(a.is_char_boundary(pre) && b.is_char_boundary(pre)) {
        pre -= 1;
    }

    // Bounded by what the prefix left, so the two slices cannot overlap even
    // when one line is a prefix of the other.
    let mut suf = 0;
    while suf < max - pre && ab[ab.len() - 1 - suf] == bb[bb.len() - 1 - suf] {
        suf += 1;
    }
    while suf > 0
        && !(a.is_char_boundary(a.len() - suf) && b.is_char_boundary(b.len() - suf))
    {
        suf -= 1;
    }

    (pre, suf)
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

    /// Lines that share nothing are still too expensive to analyse.
    #[test]
    fn lines_that_differ_all_the_way_through_are_skipped() {
        let a = "x".repeat(MAX_INLINE_LEN + 1);
        let b = "y".repeat(MAX_INLINE_LEN + 1);
        assert!(inline_diff(&a, &b, &opts(Granularity::Char)).is_whole_line());
    }

    /// The case a SQL dump actually produces: a megabyte on one line with one
    /// value changed near its end.
    ///
    /// The cap used to be applied to the whole line, so this fell through to
    /// "tint the whole row" and the changed part carried no mark at all. Only
    /// the middle is over-long now, and here the middle is three characters.
    #[test]
    fn a_change_late_in_a_giant_line_is_still_pinpointed() {
        let head = "INSERT INTO `t` VALUES ".to_owned() + &"(1,'aaa'),".repeat(100_000);
        let a = format!("{head}(1,'aaa');");
        let b = format!("{head}(1,'bbb');");
        assert!(a.len() > 1_000_000, "fixture is not giant");

        let d = inline_diff(&a, &b, &opts(Granularity::Char));
        assert!(!d.is_whole_line(), "a giant line got no span-level marking");
        assert!(d.left.iter().all(|s| s.end <= a.len()));
        assert!(d.right.iter().all(|s| s.end <= b.len()));

        // The mark must sit on the change, not span the line.
        let marked: usize = d.left.iter().map(|s| s.end - s.start).sum();
        assert!(marked <= 8, "marked {marked} bytes for a three-byte change");
        assert!(
            d.left.iter().all(|s| s.start > a.len() - 100),
            "the mark is not where the change is"
        );
        assert_eq!(slices(&a, &d.left), vec!["aaa"]);
        assert_eq!(slices(&b, &d.right), vec!["bbb"]);
    }

    /// Trimming must not cut a multi-byte character in half.
    #[test]
    fn trimming_shared_ends_is_safe_for_multibyte_text() {
        let head = "对比工具".repeat(5_000);
        let a = format!("{head}甲{head}");
        let b = format!("{head}乙{head}");
        let d = inline_diff(&a, &b, &opts(Granularity::Char));
        assert_eq!(slices(&a, &d.left), vec!["甲"]);
        assert_eq!(slices(&b, &d.right), vec!["乙"]);
    }

    /// One line being a prefix of the other must not make the trimmed ends
    /// overlap.
    #[test]
    fn one_line_being_a_prefix_of_the_other_is_handled() {
        let d = inline_diff("abcdef", "abc", &opts(Granularity::Char));
        assert_eq!(slices("abcdef", &d.left), vec!["def"]);
        assert!(d.right.is_empty());
    }

    #[test]
    fn identical_lines_produce_no_spans() {
        let d = inline_diff("same", "same", &opts(Granularity::Word));
        assert!(d.left.is_empty() && d.right.is_empty());
    }
}
