//! Find and replace, with literal / whole-word / regex modes.
//!
//! Matches are computed per line and cached against the buffer version, so the
//! "highlight all" overlay costs nothing to re-render while the user scrolls.

use anyhow::{Result, anyhow};
use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SearchMode {
    #[default]
    Literal,
    /// Literal, but only where the match is not flanked by word characters.
    WholeWord,
    Regex,
}

#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    pub needle: String,
    pub mode: SearchMode,
    pub case_sensitive: bool,
}

impl SearchQuery {
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Compile the query. Returns the regex error verbatim so the find bar can
    /// show the user what is wrong with their pattern.
    pub fn compile(&self) -> Result<Matcher> {
        if self.needle.is_empty() {
            return Err(anyhow!("empty pattern"));
        }
        let pattern = match self.mode {
            SearchMode::Literal => regex::escape(&self.needle),
            SearchMode::WholeWord => format!(r"\b{}\b", regex::escape(&self.needle)),
            SearchMode::Regex => self.needle.clone(),
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            // Without this a pattern like `.*` would run away on a minified
            // line; 1 MiB is far more than any real search needs.
            .size_limit(1 << 20)
            .build()?;
        Ok(Matcher { re })
    }
}

pub struct Matcher {
    re: Regex,
}

/// One match: a byte range inside a specific line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

impl Matcher {
    /// Every match in the document, in reading order.
    ///
    /// `limit` caps the result so a pattern matching every character in a huge
    /// file cannot exhaust memory; the caller reports when it was hit.
    pub fn find_all(&self, lines: &[String], limit: usize) -> (Vec<Match>, bool) {
        let mut out = Vec::new();
        for (line, text) in lines.iter().enumerate() {
            for m in self.re.find_iter(text) {
                if out.len() >= limit {
                    return (out, true);
                }
                // Zero-width matches (e.g. `^`, `\b`) would otherwise render as
                // invisible highlights and make "next match" spin in place.
                if m.start() == m.end() {
                    continue;
                }
                out.push(Match {
                    line,
                    start: m.start(),
                    end: m.end(),
                });
            }
        }
        (out, false)
    }

    /// Replace every match, returning the new lines and how many were replaced.
    ///
    /// In regex mode `replacement` supports `$1` / `${name}` capture references.
    pub fn replace_all(&self, lines: &[String], replacement: &str) -> (Vec<String>, usize) {
        let mut count = 0;
        let out = lines
            .iter()
            .map(|l| {
                count += self.re.find_iter(l).filter(|m| m.start() != m.end()).count();
                self.re.replace_all(l, replacement).into_owned()
            })
            .collect();
        (out, count)
    }

    /// Replace a single known match. Used by "Replace" (as opposed to
    /// "Replace All") so capture references still work.
    pub fn replace_one(&self, line: &str, m: &Match, replacement: &str) -> String {
        let mut out = String::with_capacity(line.len());
        out.push_str(&line[..m.start]);
        let slice = &line[m.start..m.end];
        // Re-run the regex on just the match so `$1` resolves against it.
        out.push_str(&self.re.replace(slice, replacement));
        out.push_str(&line[m.end..]);
        out
    }
}

/// Index of the first match at or after `(line, byte)`, wrapping around.
pub fn next_match(matches: &[Match], line: usize, byte: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    matches
        .iter()
        .position(|m| (m.line, m.start) >= (line, byte))
        .or(Some(0))
}

/// Index of the last match strictly before `(line, byte)`, wrapping around.
pub fn prev_match(matches: &[Match], line: usize, byte: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    matches
        .iter()
        .rposition(|m| (m.line, m.start) < (line, byte))
        .or(Some(matches.len() - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn query(needle: &str, mode: SearchMode, cs: bool) -> SearchQuery {
        SearchQuery {
            needle: needle.into(),
            mode,
            case_sensitive: cs,
        }
    }

    #[test]
    fn literal_search_is_case_insensitive_by_default() {
        let m = query("foo", SearchMode::Literal, false).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["Foo bar", "no match", "FOO FOO"]), 100);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0], Match { line: 0, start: 0, end: 3 });
        assert_eq!(hits[2].line, 2);
    }

    #[test]
    fn case_sensitive_search() {
        let m = query("Foo", SearchMode::Literal, true).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["Foo", "foo"]), 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 0);
    }

    #[test]
    fn literal_mode_escapes_regex_metacharacters() {
        let m = query("a.c", SearchMode::Literal, true).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["abc", "a.c"]), 100);
        assert_eq!(hits.len(), 1, "the dot must be literal");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn whole_word_mode() {
        let m = query("cat", SearchMode::WholeWord, true).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["cat", "concatenate", "a cat."]), 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].line, 2);
    }

    #[test]
    fn regex_mode() {
        let m = query(r"\d+", SearchMode::Regex, true).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["a1b22c333"]), 100);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[2], Match { line: 0, start: 6, end: 9 });
    }

    #[test]
    fn a_bad_regex_reports_an_error_instead_of_panicking() {
        assert!(query("(unclosed", SearchMode::Regex, true).compile().is_err());
        assert!(query("", SearchMode::Literal, true).compile().is_err());
    }

    #[test]
    fn zero_width_matches_are_skipped() {
        let m = query("^", SearchMode::Regex, true).compile().unwrap();
        let (hits, _) = m.find_all(&v(&["a", "b"]), 100);
        assert!(hits.is_empty(), "zero-width hits would be invisible");
    }

    #[test]
    fn the_match_limit_is_reported() {
        let lines: Vec<String> = (0..100).map(|_| "x x x".to_owned()).collect();
        let m = query("x", SearchMode::Literal, true).compile().unwrap();
        let (hits, truncated) = m.find_all(&lines, 10);
        assert_eq!(hits.len(), 10);
        assert!(truncated);
    }

    #[test]
    fn replace_all_counts_replacements() {
        let m = query("cat", SearchMode::Literal, false).compile().unwrap();
        let (out, n) = m.replace_all(&v(&["cat CAT", "dog"]), "bird");
        assert_eq!(out, v(&["bird bird", "dog"]));
        assert_eq!(n, 2);
    }

    #[test]
    fn replace_all_supports_capture_groups() {
        let m = query(r"(\w+)@(\w+)", SearchMode::Regex, true)
            .compile()
            .unwrap();
        let (out, n) = m.replace_all(&v(&["user@host"]), "$2:$1");
        assert_eq!(out, v(&["host:user"]));
        assert_eq!(n, 1);
    }

    #[test]
    fn replace_one_leaves_the_rest_alone() {
        let m = query("x", SearchMode::Literal, true).compile().unwrap();
        let line = "x and x";
        let hit = Match { line: 0, start: 6, end: 7 };
        assert_eq!(m.replace_one(line, &hit, "y"), "x and y");
    }

    #[test]
    fn navigation_wraps_around() {
        let hits = vec![
            Match { line: 0, start: 0, end: 1 },
            Match { line: 5, start: 2, end: 3 },
            Match { line: 9, start: 0, end: 1 },
        ];
        assert_eq!(next_match(&hits, 0, 0), Some(0));
        assert_eq!(next_match(&hits, 1, 0), Some(1));
        assert_eq!(next_match(&hits, 9, 1), Some(0), "wraps to the top");
        assert_eq!(prev_match(&hits, 9, 0), Some(1));
        assert_eq!(prev_match(&hits, 0, 0), Some(2), "wraps to the bottom");
        assert_eq!(next_match(&[], 0, 0), None);
        assert_eq!(prev_match(&[], 0, 0), None);
    }

    #[test]
    fn matches_are_byte_ranges_that_slice_cleanly() {
        let lines = v(&["对比 工具 对比"]);
        let m = query("对比", SearchMode::Literal, true).compile().unwrap();
        let (hits, _) = m.find_all(&lines, 100);
        assert_eq!(hits.len(), 2);
        for h in &hits {
            assert_eq!(&lines[h.line][h.start..h.end], "对比");
        }
    }
}
