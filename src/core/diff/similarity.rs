//! Cheap string-similarity used to decide which lines inside a replaced block
//! are "the same line, edited" versus "a different line entirely".
//!
//! We deliberately avoid running a real diff per candidate pair: a replaced
//! block of 200x200 lines would mean 40 000 diffs. Sorensen-Dice over character
//! bigrams is O(n) per line, needs no allocation beyond one small buffer, and
//! ranks candidates well enough for alignment.

/// Sorensen-Dice coefficient over character bigrams, in `0.0..=1.0`.
///
/// Operates on `char`s (not bytes) so CJK and accented text behave sensibly.
pub fn dice(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    dice_sorted(&sorted_bigrams(a), &sorted_bigrams(b))
}

/// Sorted character bigrams of `s`, computed once so a similarity DP can
/// score many pairs without re-tokenizing and re-sorting the same lines.
pub fn sorted_bigrams(s: &str) -> Vec<u64> {
    let mut v = bigrams(s);
    v.sort_unstable();
    v
}

/// Sorensen-Dice coefficient over two *sorted* bigram lists, in `0.0..=1.0`.
///
/// Exact string equality must be handled by the caller (score 1.0): two
/// single-character strings have no bigrams, so their equality is invisible
/// here and would otherwise score 0.
pub fn dice_sorted(a: &[u64], b: &[u64]) -> f32 {
    if a.is_empty() || b.is_empty() {
        // Both are single characters: it's a match only if they're equal,
        // which the caller's equality check already ruled out.
        return 0.0;
    }

    // Multiset intersection: both lists are sorted, so walk them in lockstep.
    let (mut i, mut j, mut hits) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                hits += 1;
                i += 1;
                j += 1;
            }
        }
    }

    2.0 * hits as f32 / (a.len() + b.len()) as f32
}

/// Character bigrams packed into a `u64` so they sort and compare as integers.
fn bigrams(s: &str) -> Vec<u64> {
    // Cap the work on pathological lines (minified JS, base64 blobs). The
    // leading 4 KiB is more than enough signal to rank a line pair.
    const MAX_CHARS: usize = 4096;

    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    for c in s.chars().take(MAX_CHARS) {
        if let Some(p) = prev {
            out.push(((p as u64) << 32) | c as u64);
        }
        prev = Some(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_one() {
        assert_eq!(dice("hello world", "hello world"), 1.0);
    }

    #[test]
    fn disjoint_is_zero() {
        assert_eq!(dice("aaaa", "bbbb"), 0.0);
    }

    #[test]
    fn small_edit_scores_high() {
        let s = dice("let x = compute(a, b);", "let x = compute(a, c);");
        assert!(s > 0.8, "expected a high score, got {s}");
    }

    #[test]
    fn unrelated_code_scores_low() {
        let s = dice("let x = compute(a, b);", "fn main() {}");
        assert!(s < 0.3, "expected a low score, got {s}");
    }

    #[test]
    fn handles_cjk() {
        assert!(dice("对比工具", "对比工具箱") > 0.6);
    }

    #[test]
    fn empty_is_safe() {
        assert_eq!(dice("", ""), 1.0);
        assert_eq!(dice("", "x"), 0.0);
    }
}
