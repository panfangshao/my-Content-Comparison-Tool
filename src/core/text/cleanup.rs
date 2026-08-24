//! The line-tidying operations on the Tools menu.
//!
//! Each one is a pure `&[String] -> Vec<String>` transform. The caller applies
//! the result through `TextBuffer::replace_lines`, so every tool is a single
//! Ctrl+Z away from being undone.

use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CleanupOp {
    /// Drop lines seen before, keeping the first occurrence and the order.
    RemoveDuplicates,
    /// Drop every line that is empty or whitespace-only.
    RemoveEmptyLines,
    /// Collapse runs of two or more blank lines into one.
    SqueezeBlankLines,
    TrimLeading,
    TrimTrailing,
    TrimBoth,
    /// Join everything into one line, separated by single spaces.
    NewlinesToSpaces,
    Sort,
    SortCaseInsensitive,
    SortDescending,
    /// Sort and drop duplicates in one pass.
    SortUnique,
    /// Reverse the line order.
    Reverse,
    ToLowercase,
    ToUppercase,
    /// Collapse runs of internal whitespace to a single space, and trim.
    NormalizeWhitespace,
}

impl CleanupOp {
    /// Everything the Tools menu offers, in menu order.
    pub const ALL: [Self; 15] = [
        Self::RemoveDuplicates,
        Self::SortUnique,
        Self::RemoveEmptyLines,
        Self::SqueezeBlankLines,
        Self::TrimBoth,
        Self::TrimLeading,
        Self::TrimTrailing,
        Self::NormalizeWhitespace,
        Self::NewlinesToSpaces,
        Self::Sort,
        Self::SortCaseInsensitive,
        Self::SortDescending,
        Self::Reverse,
        Self::ToLowercase,
        Self::ToUppercase,
    ];

    /// Key for the one-line description shown on hover.
    ///
    /// Several of these tools are easy to confuse with each other - three
    /// different kinds of trim, two ways to remove blank lines - so the menu
    /// label alone is not enough to pick the right one with confidence.
    pub const fn help_key(self) -> &'static str {
        match self {
            Self::RemoveDuplicates => "tool.remove_duplicates.help",
            Self::RemoveEmptyLines => "tool.remove_empty.help",
            Self::SqueezeBlankLines => "tool.squeeze_blank.help",
            Self::TrimLeading => "tool.trim_leading.help",
            Self::TrimTrailing => "tool.trim_trailing.help",
            Self::TrimBoth => "tool.trim_both.help",
            Self::NewlinesToSpaces => "tool.newlines_to_spaces.help",
            Self::Sort => "tool.sort.help",
            Self::SortCaseInsensitive => "tool.sort_ci.help",
            Self::SortDescending => "tool.sort_desc.help",
            Self::SortUnique => "tool.sort_unique.help",
            Self::Reverse => "tool.reverse.help",
            Self::ToLowercase => "tool.lowercase.help",
            Self::ToUppercase => "tool.uppercase.help",
            Self::NormalizeWhitespace => "tool.normalize_ws.help",
        }
    }

    /// Key into the translation table; see `i18n`.
    pub const fn i18n_key(self) -> &'static str {
        match self {
            Self::RemoveDuplicates => "tool.remove_duplicates",
            Self::RemoveEmptyLines => "tool.remove_empty",
            Self::SqueezeBlankLines => "tool.squeeze_blank",
            Self::TrimLeading => "tool.trim_leading",
            Self::TrimTrailing => "tool.trim_trailing",
            Self::TrimBoth => "tool.trim_both",
            Self::NewlinesToSpaces => "tool.newlines_to_spaces",
            Self::Sort => "tool.sort",
            Self::SortCaseInsensitive => "tool.sort_ci",
            Self::SortDescending => "tool.sort_desc",
            Self::SortUnique => "tool.sort_unique",
            Self::Reverse => "tool.reverse",
            Self::ToLowercase => "tool.lowercase",
            Self::ToUppercase => "tool.uppercase",
            Self::NormalizeWhitespace => "tool.normalize_ws",
        }
    }
}

/// Run a cleanup over `lines`, returning the new lines.
pub fn apply(op: CleanupOp, lines: &[String]) -> Vec<String> {
    match op {
        CleanupOp::RemoveDuplicates => {
            let mut seen = HashSet::with_capacity(lines.len());
            lines
                .iter()
                .filter(|l| seen.insert(l.as_str()))
                .cloned()
                .collect()
        }
        CleanupOp::RemoveEmptyLines => lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .cloned()
            .collect(),
        CleanupOp::SqueezeBlankLines => {
            let mut out: Vec<String> = Vec::with_capacity(lines.len());
            for l in lines {
                let blank = l.trim().is_empty();
                if blank && out.last().is_some_and(|p: &String| p.trim().is_empty()) {
                    continue;
                }
                out.push(l.clone());
            }
            out
        }
        CleanupOp::TrimLeading => lines.iter().map(|l| l.trim_start().to_owned()).collect(),
        CleanupOp::TrimTrailing => lines.iter().map(|l| l.trim_end().to_owned()).collect(),
        CleanupOp::TrimBoth => lines.iter().map(|l| l.trim().to_owned()).collect(),
        CleanupOp::NormalizeWhitespace => lines
            .iter()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect(),
        CleanupOp::NewlinesToSpaces => {
            let joined = lines
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                Vec::new()
            } else {
                vec![joined]
            }
        }
        CleanupOp::Sort => {
            let mut v = lines.to_vec();
            v.sort();
            v
        }
        CleanupOp::SortCaseInsensitive => {
            let mut v = lines.to_vec();
            v.sort_by_key(|l| l.to_lowercase());
            v
        }
        CleanupOp::SortDescending => {
            let mut v = lines.to_vec();
            v.sort_by(|a, b| b.cmp(a));
            v
        }
        CleanupOp::SortUnique => {
            let mut v = lines.to_vec();
            v.sort();
            v.dedup();
            v
        }
        CleanupOp::Reverse => lines.iter().rev().cloned().collect(),
        CleanupOp::ToLowercase => lines.iter().map(|l| l.to_lowercase()).collect(),
        CleanupOp::ToUppercase => lines.iter().map(|l| l.to_uppercase()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn remove_duplicates_keeps_first_and_order() {
        let out = apply(CleanupOp::RemoveDuplicates, &v(&["b", "a", "b", "c", "a"]));
        assert_eq!(out, v(&["b", "a", "c"]));
    }

    #[test]
    fn remove_empty_lines_drops_whitespace_only() {
        let out = apply(CleanupOp::RemoveEmptyLines, &v(&["a", "", "   ", "\t", "b"]));
        assert_eq!(out, v(&["a", "b"]));
    }

    #[test]
    fn squeeze_blank_lines_keeps_one() {
        let out = apply(
            CleanupOp::SqueezeBlankLines,
            &v(&["a", "", "", "  ", "b", "", "c"]),
        );
        assert_eq!(out, v(&["a", "", "b", "", "c"]));
    }

    #[test]
    fn trimming_variants() {
        let input = v(&["  padded  ", "\tboth\t"]);
        assert_eq!(
            apply(CleanupOp::TrimLeading, &input),
            v(&["padded  ", "both\t"])
        );
        assert_eq!(
            apply(CleanupOp::TrimTrailing, &input),
            v(&["  padded", "\tboth"])
        );
        assert_eq!(apply(CleanupOp::TrimBoth, &input), v(&["padded", "both"]));
    }

    #[test]
    fn normalize_whitespace_collapses_runs() {
        let out = apply(
            CleanupOp::NormalizeWhitespace,
            &v(&["  a   b \t c  ", "single"]),
        );
        assert_eq!(out, v(&["a b c", "single"]));
    }

    #[test]
    fn newlines_to_spaces_joins_everything() {
        let out = apply(CleanupOp::NewlinesToSpaces, &v(&["one", "", "two", "three"]));
        assert_eq!(out, v(&["one two three"]));
    }

    #[test]
    fn newlines_to_spaces_on_blank_input_yields_nothing() {
        assert!(apply(CleanupOp::NewlinesToSpaces, &v(&["", "  "])).is_empty());
    }

    #[test]
    fn sorting_variants() {
        let input = v(&["b", "A", "a", "B"]);
        assert_eq!(apply(CleanupOp::Sort, &input), v(&["A", "B", "a", "b"]));
        assert_eq!(
            apply(CleanupOp::SortDescending, &input),
            v(&["b", "a", "B", "A"])
        );
        // Case-insensitive sort is stable, so equal keys keep input order.
        assert_eq!(
            apply(CleanupOp::SortCaseInsensitive, &input),
            v(&["A", "a", "b", "B"])
        );
    }

    #[test]
    fn sort_unique_sorts_and_dedupes() {
        let out = apply(CleanupOp::SortUnique, &v(&["c", "a", "b", "a", "c"]));
        assert_eq!(out, v(&["a", "b", "c"]));
    }

    #[test]
    fn reverse_and_case_changes() {
        assert_eq!(apply(CleanupOp::Reverse, &v(&["a", "b"])), v(&["b", "a"]));
        assert_eq!(apply(CleanupOp::ToLowercase, &v(&["AbC"])), v(&["abc"]));
        assert_eq!(apply(CleanupOp::ToUppercase, &v(&["AbC"])), v(&["ABC"]));
    }

    #[test]
    fn every_op_handles_empty_input() {
        for op in CleanupOp::ALL {
            assert!(apply(op, &[]).is_empty(), "{op:?} misbehaved on empty input");
        }
    }

    #[test]
    fn every_menu_op_has_a_distinct_translation_key() {
        let mut keys: Vec<&str> = CleanupOp::ALL.iter().map(|o| o.i18n_key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate i18n key");
    }

    #[test]
    fn every_menu_op_has_a_distinct_help_key() {
        let mut keys: Vec<&str> = CleanupOp::ALL.iter().map(|o| o.help_key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate help key");
        // A help key must never collide with a label key, or the tooltip would
        // just repeat the menu entry.
        for op in CleanupOp::ALL {
            assert_ne!(op.help_key(), op.i18n_key());
        }
    }

    #[test]
    fn cjk_sorting_does_not_panic() {
        let out = apply(CleanupOp::Sort, &v(&["工具", "对比", "文本"]));
        assert_eq!(out.len(), 3);
    }
}
