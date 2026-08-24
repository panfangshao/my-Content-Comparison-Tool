//! The find / replace bar.
//!
//! Search results are recomputed only when something that affects them
//! changes: the pattern, the options, the target pane, or that pane's
//! contents. That matters because "highlight all matches" runs against the
//! whole document while the editor redraws at 60 fps.

use egui::{Key, Ui};

use crate::core::diff::Side;
use crate::core::text::{Match, SearchMode, SearchQuery, TextBuffer, search};
use crate::i18n::{Lang, t, tf};
use crate::ui::theme::Palette;

/// Cap on highlighted matches. A pattern like `.` in a large file would
/// otherwise build a vector with one entry per character.
const MATCH_LIMIT: usize = 50_000;

/// What the bar wants the application to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FindAction {
    #[default]
    None,
    Close,
    /// Scroll the target pane to the active match.
    Reveal,
    ReplaceOne,
    ReplaceAll,
}

/// Everything that decides whether the cached results are still valid.
#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    side: Side,
    version: u64,
    needle: String,
    mode: SearchMode,
    case_sensitive: bool,
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub show_replace: bool,
    pub query: SearchQuery,
    pub replacement: String,
    /// Which pane is being searched.
    pub target: Side,
    pub matches: Vec<Match>,
    pub active: Option<usize>,
    /// Set when the pattern will not compile, shown under the field.
    pub error: Option<String>,
    /// Set when [`MATCH_LIMIT`] was hit.
    pub truncated: bool,
    /// Ask egui to put the caret in the search field next frame.
    pub focus_input: bool,
    cache: Option<CacheKey>,
}

impl FindState {
    /// Open the bar, optionally with the replace row showing.
    pub fn open(&mut self, replace: bool, seed: Option<String>) {
        self.open = true;
        self.show_replace = replace;
        self.focus_input = true;
        if let Some(s) = seed
            && !s.is_empty()
            && !s.contains('\n')
        {
            self.query.needle = s;
            self.cache = None;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.active = None;
        self.cache = None;
    }

    /// Recompute the match list if anything relevant changed.
    pub fn refresh(&mut self, buffer: &TextBuffer, side: Side) {
        if !self.open {
            if !self.matches.is_empty() {
                self.matches.clear();
                self.cache = None;
            }
            return;
        }

        let key = CacheKey {
            side,
            version: buffer.version(),
            needle: self.query.needle.clone(),
            mode: self.query.mode,
            case_sensitive: self.query.case_sensitive,
        };
        if self.cache.as_ref() == Some(&key) {
            return;
        }
        self.cache = Some(key);

        self.matches.clear();
        self.truncated = false;
        self.error = None;
        self.active = None;

        if self.query.is_empty() {
            return;
        }
        match self.query.compile() {
            Ok(m) => {
                let (hits, truncated) = m.find_all(buffer.lines(), MATCH_LIMIT);
                self.matches = hits;
                self.truncated = truncated;
                // Start from wherever the caret is, so Enter continues from
                // the user's position rather than from the top of the file.
                let (line, col) = buffer.line_col(buffer.selection().head);
                let byte = byte_of_col(buffer.line(line), col);
                self.active = search::next_match(&self.matches, line, byte);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.active = Some(match self.active {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
    }

    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.active = Some(match self.active {
            Some(i) => (i + self.matches.len() - 1) % self.matches.len(),
            None => self.matches.len() - 1,
        });
    }

    pub fn active_match(&self) -> Option<Match> {
        self.active.and_then(|i| self.matches.get(i)).copied()
    }

    /// Replace the active match, leaving the caret after the replacement.
    ///
    /// Returns whether anything changed.
    pub fn replace_current(&mut self, buffer: &mut TextBuffer) -> bool {
        let Some(m) = self.active_match() else {
            return false;
        };
        let Ok(matcher) = self.query.compile() else {
            return false;
        };
        let line = buffer.line(m.line).to_owned();
        if m.end > line.len() {
            return false; // the buffer moved under us
        }
        let new_line = matcher.replace_one(&line, &m, &self.replacement);
        buffer.replace_lines(m.line..m.line + 1, &[new_line]);
        self.cache = None;
        true
    }

    /// Replace every match. Returns how many.
    pub fn replace_all(&mut self, buffer: &mut TextBuffer) -> usize {
        let Ok(matcher) = self.query.compile() else {
            return 0;
        };
        let (lines, count) = matcher.replace_all(buffer.lines(), &self.replacement);
        if count > 0 {
            buffer.replace_lines(0..buffer.len_lines(), &lines);
            self.cache = None;
        }
        count
    }
}

/// Byte offset of a character column within a line.
fn byte_of_col(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map_or(line.len(), |(b, _)| b)
}

/// Draw the bar.
pub fn show_find_bar(
    ui: &mut Ui,
    state: &mut FindState,
    palette: &Palette,
    lang: Lang,
    single_pane: bool,
) -> FindAction {
    let mut action = FindAction::None;

    egui::Frame::new()
        .fill(palette.chrome_bg)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // ---- Pattern -------------------------------------------
                let field = egui::TextEdit::singleline(&mut state.query.needle)
                    .hint_text(t(lang, "find.placeholder"))
                    .desired_width(220.0);
                let response = ui.add(field);
                if state.focus_input {
                    response.request_focus();
                    state.focus_input = false;
                }

                // Enter steps forward, Shift+Enter back - the convention.
                if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if ui.input(|i| i.modifiers.shift) {
                        state.prev();
                    } else {
                        state.next();
                    }
                    action = FindAction::Reveal;
                }

                if ui
                    .add_enabled(
                        !state.matches.is_empty(),
                        egui::Button::new("\u{2191}").small(),
                    )
                    .on_hover_text(t(lang, "nav.prev_diff"))
                    .clicked()
                {
                    state.prev();
                    action = FindAction::Reveal;
                }
                if ui
                    .add_enabled(
                        !state.matches.is_empty(),
                        egui::Button::new("\u{2193}").small(),
                    )
                    .on_hover_text(t(lang, "nav.next_diff"))
                    .clicked()
                {
                    state.next();
                    action = FindAction::Reveal;
                }

                // ---- Result count --------------------------------------
                let label = if let Some(err) = &state.error {
                    ui.colored_label(palette.error, t(lang, "find.bad_regex"))
                        .on_hover_text(err);
                    None
                } else if state.query.is_empty() {
                    None
                } else if state.matches.is_empty() {
                    Some(t(lang, "find.no_results").to_owned())
                } else {
                    let n = state.matches.len();
                    Some(match state.active {
                        Some(i) => tf(
                            lang,
                            "find.result_of",
                            &[("i", &(i + 1).to_string()), ("n", &n.to_string())],
                        ),
                        None => tf(lang, "find.results", &[("n", &n.to_string())]),
                    })
                };
                if let Some(text) = label {
                    let color = if state.matches.is_empty() {
                        palette.warn
                    } else {
                        palette.text_dim
                    };
                    ui.colored_label(color, text);
                    if state.truncated {
                        ui.colored_label(palette.warn, "\u{2026}")
                            .on_hover_text(format!("showing the first {MATCH_LIMIT}"));
                    }
                }

                ui.separator();

                // ---- Options -------------------------------------------
                let mut changed = false;
                if ui
                    .selectable_label(state.query.case_sensitive, "Aa")
                    .on_hover_text(t(lang, "find.case"))
                    .clicked()
                {
                    state.query.case_sensitive = !state.query.case_sensitive;
                    changed = true;
                }
                changed |= toggle_mode(ui, state, SearchMode::WholeWord, "ab", t(lang, "find.word"));
                changed |= toggle_mode(ui, state, SearchMode::Regex, ".*", t(lang, "find.regex"));

                // ---- Which pane ----------------------------------------
                //
                // Only meaningful with two of them.
                if !single_pane {
                    ui.separator();
                    for (side, key) in
                        [(Side::Left, "find.in_left"), (Side::Right, "find.in_right")]
                    {
                        if ui
                            .selectable_label(state.target == side, t(lang, key))
                            .clicked()
                            && state.target != side
                        {
                            state.target = side;
                            changed = true;
                        }
                    }
                }

                if changed {
                    state.cache = None;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // A plain multiplication sign: every font we might fall
                    // back to has it, unlike the dingbat cross.
                    if ui
                        .button("\u{00D7}")
                        .on_hover_text(format!("{} (Esc)", t(lang, "find.close")))
                        .clicked()
                    {
                        action = FindAction::Close;
                    }
                    if ui
                        .selectable_label(state.show_replace, t(lang, "edit.replace"))
                        .on_hover_text(format!("{} (Ctrl+H)", t(lang, "edit.replace")))
                        .clicked()
                    {
                        state.show_replace = !state.show_replace;
                    }
                });
            });

            // ---- Replace row --------------------------------------------
            if state.show_replace {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.replacement)
                            .hint_text(t(lang, "find.replace_placeholder"))
                            .desired_width(220.0),
                    );
                    let any = !state.matches.is_empty();
                    if ui
                        .add_enabled(any, egui::Button::new(t(lang, "find.replace_one")))
                        .clicked()
                    {
                        action = FindAction::ReplaceOne;
                    }
                    if ui
                        .add_enabled(any, egui::Button::new(t(lang, "find.replace_all")))
                        .clicked()
                    {
                        action = FindAction::ReplaceAll;
                    }
                    if state.query.mode == SearchMode::Regex {
                        ui.label(
                            egui::RichText::new("$1 $2 \u{2026}")
                                .color(palette.text_faint)
                                .small(),
                        )
                        .on_hover_text("Capture groups are available in the replacement");
                    }
                });
            }
        });

    if ui.input(|i| i.key_pressed(Key::Escape)) {
        action = FindAction::Close;
    }
    action
}

/// A mode toggle that flips back to `Literal` when switched off.
fn toggle_mode(
    ui: &mut Ui,
    state: &mut FindState,
    mode: SearchMode,
    glyph: &str,
    tip: &str,
) -> bool {
    let on = state.query.mode == mode;
    if ui.selectable_label(on, glyph).on_hover_text(tip).clicked() {
        state.query.mode = if on { SearchMode::Literal } else { mode };
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> TextBuffer {
        TextBuffer::from_text(text)
    }

    fn state(needle: &str) -> FindState {
        FindState {
            open: true,
            query: SearchQuery {
                needle: needle.to_owned(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn refresh_finds_matches() {
        let b = buffer("one two\ntwo three");
        let mut s = state("two");
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 2);
        assert!(s.error.is_none());
    }

    #[test]
    fn refresh_is_cached_until_something_changes() {
        let b = buffer("aaa");
        let mut s = state("a");
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 3);

        // Corrupt the results; an unchanged refresh must not rebuild them.
        s.matches.clear();
        s.refresh(&b, Side::Left);
        assert!(s.matches.is_empty(), "the cache should have short-circuited");

        // Changing the pattern invalidates it.
        s.query.needle = "aa".into();
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 1);
    }

    #[test]
    fn editing_the_buffer_invalidates_the_cache() {
        let mut b = buffer("cat");
        let mut s = state("cat");
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 1);

        b.set_selection(crate::core::text::Selection::at(3));
        b.insert(" cat");
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 2, "a new buffer version must re-search");
    }

    #[test]
    fn switching_pane_invalidates_the_cache() {
        let b = buffer("x");
        let mut s = state("x");
        s.refresh(&b, Side::Left);
        s.matches.clear();
        s.refresh(&b, Side::Right);
        assert_eq!(s.matches.len(), 1);
    }

    #[test]
    fn a_bad_regex_reports_an_error_and_clears_results() {
        let b = buffer("text");
        let mut s = state("(unclosed");
        s.query.mode = SearchMode::Regex;
        s.refresh(&b, Side::Left);
        assert!(s.error.is_some());
        assert!(s.matches.is_empty());
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let b = buffer("a\na\na");
        let mut s = state("a");
        s.refresh(&b, Side::Left);
        assert_eq!(s.matches.len(), 3);

        s.active = Some(0);
        s.next();
        assert_eq!(s.active, Some(1));
        s.next();
        s.next();
        assert_eq!(s.active, Some(0), "next wraps to the top");
        s.prev();
        assert_eq!(s.active, Some(2), "prev wraps to the bottom");
    }

    #[test]
    fn navigation_with_no_matches_is_a_no_op() {
        let mut s = FindState::default();
        s.next();
        s.prev();
        assert_eq!(s.active, None);
    }

    #[test]
    fn the_active_match_starts_from_the_caret() {
        let mut b = buffer("target\nfiller\ntarget");
        // Put the caret on line 1, past the first match.
        b.set_selection(crate::core::text::Selection::at(b.char_at(1, 0)));
        let mut s = state("target");
        s.refresh(&b, Side::Left);
        assert_eq!(
            s.active.map(|i| s.matches[i].line),
            Some(2),
            "search should continue from the caret, not the top"
        );
    }

    #[test]
    fn replace_current_changes_only_that_match() {
        let mut b = buffer("cat cat cat");
        let mut s = state("cat");
        s.replacement = "dog".into();
        s.refresh(&b, Side::Left);
        s.active = Some(1);
        assert!(s.replace_current(&mut b));
        assert_eq!(b.text(), "cat dog cat");
    }

    #[test]
    fn replace_current_with_no_active_match_is_a_no_op() {
        let mut b = buffer("abc");
        let mut s = state("zzz");
        s.refresh(&b, Side::Left);
        assert!(!s.replace_current(&mut b));
        assert_eq!(b.text(), "abc");
    }

    #[test]
    fn replace_all_reports_a_count_and_is_one_undo_step() {
        let mut b = buffer("cat\ncat\ndog");
        let mut s = state("cat");
        s.replacement = "bird".into();
        s.refresh(&b, Side::Left);
        assert_eq!(s.replace_all(&mut b), 2);
        assert_eq!(b.lines(), ["bird", "bird", "dog"]);
        assert!(b.undo());
        assert_eq!(b.lines(), ["cat", "cat", "dog"]);
    }

    #[test]
    fn replace_all_with_captures() {
        let mut b = buffer("a=1\nb=2");
        let mut s = state(r"(\w)=(\d)");
        s.query.mode = SearchMode::Regex;
        s.replacement = "$2=$1".into();
        s.refresh(&b, Side::Left);
        assert_eq!(s.replace_all(&mut b), 2);
        assert_eq!(b.lines(), ["1=a", "2=b"]);
    }

    #[test]
    fn replace_all_on_nothing_is_harmless() {
        let mut b = buffer("abc");
        let mut s = state("zzz");
        s.refresh(&b, Side::Left);
        assert_eq!(s.replace_all(&mut b), 0);
        assert_eq!(b.text(), "abc");
        assert!(!b.can_undo(), "a no-op must not create an undo step");
    }

    #[test]
    fn closing_clears_the_results() {
        let b = buffer("x x x");
        let mut s = state("x");
        s.refresh(&b, Side::Left);
        assert!(!s.matches.is_empty());
        s.close();
        assert!(s.matches.is_empty());
        assert!(!s.open);
    }

    #[test]
    fn a_closed_bar_does_no_work() {
        let b = buffer("x");
        let mut s = state("x");
        s.open = false;
        s.refresh(&b, Side::Left);
        assert!(s.matches.is_empty());
    }

    #[test]
    fn opening_seeds_from_the_selection_but_not_multiline_text() {
        let mut s = FindState::default();
        s.open(false, Some("word".into()));
        assert_eq!(s.query.needle, "word");

        let mut s2 = FindState::default();
        s2.open(false, Some("two\nlines".into()));
        assert!(s2.query.needle.is_empty(), "a multi-line seed is ignored");
    }

    #[test]
    fn byte_of_col_handles_multibyte_text() {
        assert_eq!(byte_of_col("对比x", 0), 0);
        assert_eq!(byte_of_col("对比x", 1), 3);
        assert_eq!(byte_of_col("对比x", 2), 6);
        assert_eq!(byte_of_col("对比x", 99), 7);
    }
}
