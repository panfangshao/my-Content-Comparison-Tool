//! Syntax highlighting on top of syntect, made compatible with virtual
//! scrolling.
//!
//! # The problem
//!
//! syntect is a *sequential* parser: to know the state at line 90 000 you must
//! have parsed lines 0..90 000. A virtualized editor draws 50 lines wherever
//! the user happens to have scrolled, so the naive approach either re-parses
//! the whole file every frame or gives up on highlighting.
//!
//! # The approach
//!
//! Snapshot the parser state every [`STRIDE`] lines. Drawing a window then
//! costs "rewind to the nearest checkpoint, re-parse at most `STRIDE` lines,
//! plus the window itself" - bounded work, independent of where you scrolled.
//!
//! Building the checkpoints is still linear in the file, so it is spread across
//! frames with a per-frame budget: scroll to the end of a large file and the
//! text appears immediately, un-highlighted, and colours in over the next few
//! frames rather than freezing the window.
//!
//! Edits only discard the checkpoints *after* the edited line (see
//! `TextBuffer::min_dirty_line`), so typing at line 90 000 does not throw away
//! the work done for lines 0..90 000.

use std::ops::Range;
use std::sync::Arc;

use egui::Color32;
use syntect::highlighting::{
    FontStyle, HighlightState, Highlighter, RangedHighlightIterator, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines between parser snapshots. 512 keeps the rewind cost small while
/// holding the checkpoint list to ~200 entries for a 100k-line file.
const STRIDE: usize = 512;

/// Lines we are willing to parse in one frame while catching up. Sized to stay
/// under a millisecond or two on ordinary source files.
const CATCHUP_BUDGET: usize = 4096;

/// Above this many lines we do not highlight at all. Past this point the file
/// is machine-generated far more often than not, and the checkpoint list plus
/// the catch-up cost stop being worth it.
pub const MAX_HIGHLIGHT_LINES: usize = 200_000;

/// One highlighted run within a line: a byte range and its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledRange {
    pub range: Range<usize>,
    pub color: Color32,
    pub italic: bool,
    pub bold: bool,
}

/// The syntax and theme definitions. Loading these takes tens of milliseconds
/// and a couple of megabytes, so exactly one is built and shared by both panes.
pub struct SyntaxAssets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

impl SyntaxAssets {
    pub fn load() -> Arc<Self> {
        Arc::new(Self {
            // `nonewlines` matches our line storage, which strips them.
            syntaxes: SyntaxSet::load_defaults_nonewlines(),
            themes: ThemeSet::load_defaults(),
        })
    }

    pub fn theme(&self, name: &str) -> &Theme {
        self.themes
            .themes
            .get(name)
            .or_else(|| self.themes.themes.values().next())
            .expect("syntect ships at least one default theme")
    }

    /// Guess a syntax from a filename, falling back to the first line.
    pub fn detect(&self, path: Option<&std::path::Path>, first_line: &str) -> &SyntaxReference {
        if let Some(p) = path {
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && let Some(s) = self.syntaxes.find_syntax_by_extension(ext)
            {
                return s;
            }
            if let Some(name) = p.file_name().and_then(|e| e.to_str())
                && let Some(s) = self.syntaxes.find_syntax_by_extension(name)
            {
                return s;
            }
        }
        self.syntaxes
            .find_syntax_by_first_line(first_line)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    /// Look up a syntax the user picked by name.
    pub fn by_name(&self, name: &str) -> Option<&SyntaxReference> {
        self.syntaxes
            .syntaxes()
            .iter()
            .find(|s| s.name == name)
    }

    pub fn plain_text_name(&self) -> &str {
        &self.syntaxes.find_syntax_plain_text().name
    }

    /// Syntax names for the language picker, alphabetical and deduplicated.
    pub fn language_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .syntaxes
            .syntaxes()
            .iter()
            .filter(|s| !s.hidden && !s.file_extensions.is_empty())
            .map(|s| s.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

#[derive(Clone)]
struct Checkpoint {
    parse: ParseState,
    highlight: HighlightState,
}

/// Per-pane highlighting state.
pub struct PaneHighlighter {
    /// Name of the syntax in use; `None` means plain text (nothing to do).
    syntax_name: Option<String>,
    theme_name: String,
    /// `checkpoints[i]` is the parser state *entering* line `i * STRIDE`.
    checkpoints: Vec<Checkpoint>,
    /// Lines covered by `checkpoints`, i.e. `checkpoints.len() * STRIDE`.
    parsed_upto: usize,
    /// Set when the last request could not be satisfied within the budget, so
    /// the app knows to schedule another frame.
    catching_up: bool,
}

impl Default for PaneHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneHighlighter {
    pub fn new() -> Self {
        Self {
            syntax_name: None,
            theme_name: String::new(),
            checkpoints: Vec::new(),
            parsed_upto: 0,
            catching_up: false,
        }
    }

    pub fn syntax_name(&self) -> Option<&str> {
        self.syntax_name.as_deref()
    }

    /// True when highlighting is still catching up and the app should ask for
    /// another frame.
    pub fn is_catching_up(&self) -> bool {
        self.catching_up
    }

    /// Point the pane at a different language. No-op if unchanged.
    pub fn set_syntax(&mut self, name: Option<&str>) {
        if self.syntax_name.as_deref() != name {
            self.syntax_name = name.map(str::to_owned);
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.checkpoints.clear();
        self.parsed_upto = 0;
    }

    /// Discard the checkpoints covering `line` and everything after it.
    ///
    /// Called after an edit. Checkpoints *before* the edit are still valid,
    /// which is what makes typing deep inside a large file cheap.
    pub fn invalidate_from(&mut self, line: usize) {
        let keep = line / STRIDE;
        if keep < self.checkpoints.len() {
            self.checkpoints.truncate(keep);
            self.parsed_upto = keep * STRIDE;
        }
    }

    /// Highlight `range`, returning one entry per line in it.
    ///
    /// Returns empty vectors (meaning "draw in the plain foreground colour")
    /// when highlighting is off, the file is too large, or the parser has not
    /// caught up to this part of the file yet.
    pub fn highlight(
        &mut self,
        assets: &SyntaxAssets,
        theme_name: &str,
        lines: &[String],
        range: Range<usize>,
    ) -> Vec<Vec<StyledRange>> {
        let empty = || vec![Vec::new(); range.len()];

        let Some(syntax_name) = self.syntax_name.clone() else {
            return empty();
        };
        if lines.len() > MAX_HIGHLIGHT_LINES || range.is_empty() {
            return empty();
        }
        let Some(syntax) = assets.by_name(&syntax_name) else {
            return empty();
        };

        // A theme change repaints everything, so rebuild from scratch.
        if self.theme_name != theme_name {
            self.theme_name = theme_name.to_owned();
            self.reset();
        }

        let theme = assets.theme(theme_name);
        let highlighter = Highlighter::new(theme);
        let end = range.end.min(lines.len());
        if range.start >= end {
            return empty();
        }

        // ---- 1. Extend the checkpoint chain toward the requested window ----
        let needed = range.start / STRIDE;
        self.catching_up = false;
        if self.checkpoints.is_empty() {
            self.checkpoints.push(Checkpoint {
                parse: ParseState::new(syntax),
                highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            });
            self.parsed_upto = 0;
        }

        let mut budget = CATCHUP_BUDGET;
        while self.checkpoints.len() <= needed {
            let mut cp = self
                .checkpoints
                .last()
                .expect("pushed above")
                .clone();
            let from = (self.checkpoints.len() - 1) * STRIDE;
            let to = (from + STRIDE).min(lines.len());
            if from >= lines.len() {
                break;
            }
            if budget == 0 {
                // Out of time. Draw plain this frame and continue next frame.
                self.catching_up = true;
                return empty();
            }
            budget = budget.saturating_sub(to - from);

            for line in &lines[from..to] {
                advance(&mut cp, line, &highlighter, &assets.syntaxes);
            }
            self.checkpoints.push(cp);
            self.parsed_upto = self.checkpoints.len().saturating_sub(1) * STRIDE;
        }

        let Some(start_cp) = self.checkpoints.get(needed) else {
            self.catching_up = true;
            return empty();
        };

        // ---- 2. Re-parse from the checkpoint up to the window --------------
        let mut cp = start_cp.clone();
        let cp_line = needed * STRIDE;
        for line in &lines[cp_line..range.start] {
            advance(&mut cp, line, &highlighter, &assets.syntaxes);
        }

        // ---- 3. Collect the styles for the window itself -------------------
        let mut out = Vec::with_capacity(range.len());
        for line in &lines[range.start..end] {
            out.push(styles_for(&mut cp, line, &highlighter, &assets.syntaxes));
        }
        // The caller expects one entry per requested line even if the document
        // is shorter than the window.
        out.resize(range.len(), Vec::new());
        out
    }
}

/// Advance the parser past a line without collecting its styles.
fn advance(cp: &mut Checkpoint, line: &str, hl: &Highlighter<'_>, syntaxes: &SyntaxSet) {
    // A malformed syntax definition can make `parse_line` fail; treat that as
    // "no scopes on this line" rather than losing highlighting for the file.
    let Ok(ops) = cp.parse.parse_line(line, syntaxes) else {
        return;
    };
    // Running the highlight iterator is what advances `cp.highlight`, so it is
    // needed even though the styles are dropped.
    for _ in RangedHighlightIterator::new(&mut cp.highlight, &ops, line, hl) {}
}

/// Advance the parser across a line *and* return its styled runs.
fn styles_for(
    cp: &mut Checkpoint,
    line: &str,
    hl: &Highlighter<'_>,
    syntaxes: &SyntaxSet,
) -> Vec<StyledRange> {
    let Ok(ops) = cp.parse.parse_line(line, syntaxes) else {
        return Vec::new();
    };
    RangedHighlightIterator::new(&mut cp.highlight, &ops, line, hl)
        .filter(|(_, text, _)| !text.is_empty())
        .map(|(style, _, range)| StyledRange {
            range,
            color: Color32::from_rgb(
                style.foreground.r,
                style.foreground.g,
                style.foreground.b,
            ),
            italic: style.font_style.contains(FontStyle::ITALIC),
            bold: style.font_style.contains(FontStyle::BOLD),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> Arc<SyntaxAssets> {
        SyntaxAssets::load()
    }

    fn rust_lines(n: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(match i % 4 {
                0 => format!("fn function_{i}() -> u32 {{"),
                1 => format!("    let value = {i}; // a comment"),
                2 => "    value + 1".to_owned(),
                _ => "}".to_owned(),
            });
        }
        out
    }

    #[test]
    fn plain_text_produces_no_styles() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        let out = h.highlight(&a, "base16-ocean.dark", &rust_lines(4), 0..4);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(Vec::is_empty), "no syntax was selected");
    }

    #[test]
    fn rust_keywords_and_comments_get_distinct_colours() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let lines = rust_lines(4);
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..4);

        assert!(!out[0].is_empty(), "the `fn` line should be highlighted");
        let colours: std::collections::HashSet<_> =
            out[1].iter().map(|s| s.color.to_array()).collect();
        assert!(
            colours.len() > 1,
            "a line with a comment should not be one flat colour"
        );
    }

    #[test]
    fn styled_ranges_tile_the_line_without_gaps_or_overlap() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let lines = rust_lines(8);
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..8);

        for (i, spans) in out.iter().enumerate() {
            if spans.is_empty() {
                continue;
            }
            for w in spans.windows(2) {
                assert!(
                    w[0].range.end <= w[1].range.start,
                    "line {i} has overlapping styles"
                );
            }
            let last = spans.last().expect("non-empty");
            assert!(
                last.range.end <= lines[i].len(),
                "line {i} style runs past the end of the line"
            );
            // Every range must be sliceable, or rendering panics.
            for s in spans {
                assert!(lines[i].is_char_boundary(s.range.start));
                assert!(lines[i].is_char_boundary(s.range.end));
            }
        }
    }

    /// The core promise: a window deep in a large file must produce the same
    /// colours as parsing that file straight through.
    #[test]
    fn a_window_deep_in_the_file_matches_a_sequential_parse() {
        let a = assets();
        let lines = rust_lines(2000);
        let window = 1500..1520;

        let mut jumped = PaneHighlighter::new();
        jumped.set_syntax(Some("Rust"));
        // Give it enough frames to build the checkpoint chain.
        let mut from_jump = Vec::new();
        for _ in 0..10 {
            from_jump = jumped.highlight(&a, "base16-ocean.dark", &lines, window.clone());
            if !jumped.is_catching_up() {
                break;
            }
        }

        let mut sequential = PaneHighlighter::new();
        sequential.set_syntax(Some("Rust"));
        let all = sequential.highlight(&a, "base16-ocean.dark", &lines, 0..lines.len());

        assert_eq!(from_jump, all[window], "checkpoint rewind changed the result");
        assert!(!from_jump.iter().all(Vec::is_empty));
    }

    #[test]
    fn multi_line_constructs_survive_a_checkpoint_boundary() {
        let a = assets();
        // A block comment that spans a checkpoint boundary: the state carried
        // across the checkpoint is exactly what could go wrong here.
        let mut lines = vec!["fn main() {}".to_owned()];
        lines.push("/* opening a long block comment".to_owned());
        lines.extend((0..STRIDE + 50).map(|i| format!("still inside the comment {i}")));
        lines.push("*/".to_owned());
        lines.push("fn after() {}".to_owned());

        let target = lines.len() - 3;
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let mut out = Vec::new();
        for _ in 0..10 {
            out = h.highlight(&a, "base16-ocean.dark", &lines, target..target + 1);
            if !h.is_catching_up() {
                break;
            }
        }

        let mut seq = PaneHighlighter::new();
        seq.set_syntax(Some("Rust"));
        let all = seq.highlight(&a, "base16-ocean.dark", &lines, 0..lines.len());
        assert_eq!(out[0], all[target], "comment state was lost at a checkpoint");
    }

    #[test]
    fn editing_only_discards_checkpoints_after_the_edit() {
        let a = assets();
        let lines = rust_lines(3000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        for _ in 0..10 {
            h.highlight(&a, "base16-ocean.dark", &lines, 2500..2520);
            if !h.is_catching_up() {
                break;
            }
        }
        let before = h.checkpoints.len();
        assert!(before > 4);

        h.invalidate_from(2000);
        assert_eq!(h.checkpoints.len(), 2000 / STRIDE);
        assert!(
            h.checkpoints.len() < before,
            "checkpoints after the edit must go"
        );

        h.invalidate_from(0);
        assert!(h.checkpoints.is_empty());
    }

    #[test]
    fn a_huge_file_is_skipped_rather_than_stalling() {
        let a = assets();
        let lines = vec!["fn x() {}".to_owned(); MAX_HIGHLIGHT_LINES + 1];
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..10);
        assert!(out.iter().all(Vec::is_empty));
    }

    #[test]
    fn a_window_past_the_end_of_the_file_is_padded() {
        let a = assets();
        let lines = rust_lines(5);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 3..13);
        assert_eq!(out.len(), 10, "the caller expects one entry per row");
    }

    #[test]
    fn detection_by_extension_and_shebang() {
        let a = assets();
        assert_eq!(
            a.detect(Some(std::path::Path::new("main.rs")), "").name,
            "Rust"
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("a.py")), "").name,
            "Python"
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("data.json")), "").name,
            "JSON"
        );
        let by_line = a.detect(None, "#!/bin/bash");
        assert!(
            by_line.name.contains("Bash") || by_line.name.contains("Shell"),
            "shebang detection gave {}",
            by_line.name
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("notes.unknownext")), "").name,
            a.plain_text_name()
        );
    }

    /// The languages the brief calls out by name must all be available.
    #[test]
    fn the_expected_languages_are_available() {
        let a = assets();
        for name in [
            "Rust",
            "Python",
            "JavaScript",
            "JSON",
            "Markdown",
            "YAML",
            "HTML",
            "C++",
            "Go",
            "SQL",
            "XML",
        ] {
            assert!(a.by_name(name).is_some(), "missing syntax: {name}");
        }
        assert!(a.language_names().len() > 50);
    }

    #[test]
    fn both_palette_themes_exist() {
        let a = assets();
        for p in [
            crate::ui::theme::Palette::light(),
            crate::ui::theme::Palette::dark(),
        ] {
            assert!(
                a.themes.themes.contains_key(p.syntect_theme),
                "syntect has no theme named {}",
                p.syntect_theme
            );
        }
    }
}
