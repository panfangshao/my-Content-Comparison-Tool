//! Comparison options: everything that changes *what counts as a difference*.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Which line-matching algorithm `similar` should use.
///
/// The three behave identically on small edits; they diverge on files with
/// many repeated lines (closing braces, blank lines, table rows).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DiffAlgorithm {
    /// Classic Myers. Fast, the default in git.
    Myers,
    /// Patience: anchors on lines that appear exactly once on each side.
    /// Produces much more readable diffs on code that moved around.
    Patience,
    /// Plain LCS. Occasionally tighter than Myers, usually slower.
    Lcs,
}

impl DiffAlgorithm {
    pub const ALL: [Self; 3] = [Self::Patience, Self::Myers, Self::Lcs];

    pub fn to_similar(self) -> similar::Algorithm {
        match self {
            Self::Myers => similar::Algorithm::Myers,
            Self::Patience => similar::Algorithm::Patience,
            Self::Lcs => similar::Algorithm::Lcs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Myers => "Myers",
            Self::Patience => "Patience",
            Self::Lcs => "LCS",
        }
    }
}

/// How finely we highlight *inside* a changed line pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Granularity {
    /// Highlight the whole line. Cheapest, least informative.
    Line,
    /// Highlight changed words. The sensible default for prose and code.
    Word,
    /// Highlight changed characters. Best for minified text / long identifiers.
    Char,
}

impl Granularity {
    pub const ALL: [Self; 3] = [Self::Line, Self::Word, Self::Char];
}

/// How whitespace is treated when deciding whether two lines are "the same".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum WhitespaceMode {
    /// Every space matters.
    Exact,
    /// Ignore leading/trailing whitespace only.
    IgnoreTrailing,
    /// Ignore leading/trailing *and* collapse internal runs to one space.
    IgnoreAmount,
    /// Strip whitespace entirely before comparing.
    IgnoreAll,
}

impl WhitespaceMode {
    pub const ALL: [Self; 4] = [
        Self::Exact,
        Self::IgnoreTrailing,
        Self::IgnoreAmount,
        Self::IgnoreAll,
    ];
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffOptions {
    pub algorithm: DiffAlgorithm,
    pub granularity: Granularity,
    pub whitespace: WhitespaceMode,
    pub ignore_case: bool,
    /// Blank lines are excluded from matching entirely, then re-inserted as
    /// neutral rows. Stops a stray empty line from shifting a whole hunk.
    pub ignore_blank_lines: bool,
    /// Within a replaced block, pair up lines by similarity instead of by
    /// position, so "modified" lines sit opposite their real counterpart.
    pub smart_align: bool,
    /// Minimum Dice similarity for `smart_align` to consider two lines a pair.
    pub align_threshold: f32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            algorithm: DiffAlgorithm::Patience,
            granularity: Granularity::Word,
            whitespace: WhitespaceMode::Exact,
            ignore_case: false,
            ignore_blank_lines: false,
            smart_align: true,
            align_threshold: 0.35,
        }
    }
}

/// The subset of [`DiffOptions`] that decides how lines are *paired up*.
///
/// [`DiffOptions::granularity`] is deliberately absent: it only changes how an
/// already-matched pair is painted, so switching between line/word/character
/// must repaint, never re-compare. Re-comparing a large file needlessly is slow
/// *and* moves the view out from under the caret.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AlignmentKey {
    algorithm: DiffAlgorithm,
    whitespace: WhitespaceMode,
    ignore_case: bool,
    ignore_blank_lines: bool,
    smart_align: bool,
    /// Compared by bits so a NaN threshold cannot make the key unequal to
    /// itself and re-diff on every frame.
    align_threshold: u32,
}

impl DiffOptions {
    /// What the row alignment depends on. Compare two of these to decide
    /// whether a change of options needs a fresh comparison.
    pub fn alignment_key(&self) -> AlignmentKey {
        AlignmentKey {
            algorithm: self.algorithm,
            whitespace: self.whitespace,
            ignore_case: self.ignore_case,
            ignore_blank_lines: self.ignore_blank_lines,
            smart_align: self.smart_align,
            align_threshold: self.align_threshold.to_bits(),
        }
    }

    /// True when every line can be compared byte-for-byte, which lets the
    /// engine skip building a normalized copy of both sides.
    pub fn is_exact(&self) -> bool {
        self.whitespace == WhitespaceMode::Exact && !self.ignore_case
    }

    /// Map a raw line onto the key actually used for matching.
    ///
    /// Returns `Cow::Borrowed` in the common case so exact comparisons stay
    /// allocation-free even on very large inputs.
    pub fn normalize<'a>(&self, line: &'a str) -> Cow<'a, str> {
        let mut cow = match self.whitespace {
            WhitespaceMode::Exact => Cow::Borrowed(line),
            WhitespaceMode::IgnoreTrailing => Cow::Borrowed(line.trim()),
            WhitespaceMode::IgnoreAmount => {
                let mut out = String::with_capacity(line.len());
                for (i, word) in line.split_whitespace().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(word);
                }
                Cow::Owned(out)
            }
            WhitespaceMode::IgnoreAll => {
                Cow::Owned(line.chars().filter(|c| !c.is_whitespace()).collect())
            }
        };

        if self.ignore_case && cow.chars().any(char::is_uppercase) {
            cow = Cow::Owned(cow.to_lowercase());
        }
        cow
    }

    /// Whether a line should be skipped when `ignore_blank_lines` is on.
    pub fn is_ignorable_blank(&self, line: &str) -> bool {
        self.ignore_blank_lines && line.trim().is_empty()
    }
}

#[cfg(test)]
mod alignment_key_tests {
    use super::*;

    #[test]
    fn granularity_does_not_affect_alignment() {
        let base = DiffOptions::default();
        for g in Granularity::ALL {
            let other = DiffOptions {
                granularity: g,
                ..base
            };
            assert_eq!(
                base.alignment_key(),
                other.alignment_key(),
                "{g:?} must not trigger a re-comparison"
            );
        }
    }

    #[test]
    fn every_other_option_does_affect_alignment() {
        let base = DiffOptions::default();
        let variants = [
            DiffOptions { algorithm: DiffAlgorithm::Myers, ..base },
            DiffOptions { whitespace: WhitespaceMode::IgnoreAll, ..base },
            DiffOptions { ignore_case: !base.ignore_case, ..base },
            DiffOptions { ignore_blank_lines: !base.ignore_blank_lines, ..base },
            DiffOptions { smart_align: !base.smart_align, ..base },
            DiffOptions { align_threshold: base.align_threshold + 0.1, ..base },
        ];
        for v in variants {
            assert_ne!(base.alignment_key(), v.alignment_key(), "{v:?}");
        }
    }

    #[test]
    fn the_key_equals_itself() {
        let o = DiffOptions::default();
        assert_eq!(o.alignment_key(), o.alignment_key());
    }
}
