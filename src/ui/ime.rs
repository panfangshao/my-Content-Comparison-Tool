//! Text an input method is still composing.
//!
//! Typing Chinese, Japanese or Korean does not produce one character per
//! keystroke. The input method collects keystrokes into a *pre-edit* string -
//! `nihao` on the way to `你好` - and only later commits a result. The
//! windowing layer reports this as a run of pre-edit events followed by a
//! commit, and never as ordinary typed text.
//!
//! # Why the pre-edit stays out of the document
//!
//! egui's own `TextEdit` inserts the pre-edit straight into the buffer and
//! selects it, replacing that selection on every keystroke. That is the
//! shortest path to something on screen, and it is wrong here for three
//! reasons:
//!
//! - every keystroke of composition would become an undo entry, so one `你好`
//!   would take five presses of Ctrl+Z to remove;
//! - every keystroke would bump the document version and re-run the diff. On
//!   the 14 000-line files this tool is built for, that is the whole
//!   comparison redone per pinyin letter;
//! - the document would be marked modified by merely *starting* to type and
//!   then thinking better of it.
//!
//! So the pre-edit lives here instead, and the document sees nothing until
//! [`Outcome::Commit`]. The pane paints the pre-edit at the caret itself.

use std::ops::Range;

/// One step of a composition, named in this module's own terms rather than the
/// windowing layer's.
///
/// This is the seam: the pane translates the UI framework's event into one of
/// these, and everything below it is testable without a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<'a> {
    /// The composition is still being edited.
    ///
    /// `active` is the sub-range, in characters, that the input method has
    /// focus on - the clause being converted, when there is more than one.
    Preedit {
        text: &'a str,
        active: Option<Range<usize>>,
    },
    /// The composition ended with this result. Empty means it was abandoned.
    Commit(&'a str),
}

/// What the pane should do about a [`Step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing happened; do not disturb the document or the caret.
    Ignored,
    /// The preview changed. Repaint, but leave the document alone.
    Composing,
    /// Composition finished. Insert this text at the caret.
    Commit(String),
}

/// The pre-edit currently on screen, if any.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Composition {
    text: String,
    active: Option<Range<usize>>,
}

impl Composition {
    /// The pre-edit text to draw at the caret. Empty when nothing is being
    /// composed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The clause the input method has focus on, in characters into
    /// [`Self::text`], if it reported one.
    pub fn active(&self) -> Option<Range<usize>> {
        self.active.clone()
    }

    /// Whether a composition is in progress.
    pub fn is_active(&self) -> bool {
        !self.text.is_empty()
    }

    /// Abandon the composition, dropping whatever was being composed.
    ///
    /// Called when the caret moves for a reason the input method did not
    /// cause: a click, an arrow key, a jump to a difference.
    ///
    /// Returns whether there was anything to abandon, which is also the signal
    /// to tell the input method to stop.
    pub fn cancel(&mut self) -> bool {
        let had = self.is_active();
        self.text.clear();
        self.active = None;
        had
    }

    /// Fold one step of a composition into this state.
    pub fn apply(&mut self, step: Step<'_>) -> Outcome {
        let incoming = match &step {
            Step::Preedit { text, .. } => *text,
            Step::Commit(text) => *text,
        };

        // Some integrations emit an empty pre-edit or commit merely because
        // `set_ime_allowed` or `set_ime_cursor_area` was called. Acting on
        // those would clear a selection the user was about to type over.
        if incoming.is_empty() && !self.is_active() {
            return Outcome::Ignored;
        }

        // Enter arrives as a key press, and separately as a commit on some
        // input methods. Taking both would insert two newlines.
        if incoming == "\n" || incoming == "\r" {
            return Outcome::Ignored;
        }

        match step {
            Step::Preedit { text, active } => {
                // An empty pre-edit means the composition was dismissed - the
                // user backspaced through the last letter of it.
                self.text = text.to_owned();
                self.active = active.filter(|_| !self.text.is_empty());
                Outcome::Composing
            }
            Step::Commit(text) => {
                self.text.clear();
                self.active = None;
                Outcome::Commit(text.to_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preedit(text: &str) -> Step<'_> {
        Step::Preedit { text, active: None }
    }

    /// Typing `niha` and picking a candidate.
    #[test]
    fn a_pinyin_word_reaches_the_document_exactly_once() {
        let mut c = Composition::default();

        for typed in ["n", "ni", "nih", "niha"] {
            assert_eq!(c.apply(preedit(typed)), Outcome::Composing);
            assert_eq!(c.text(), typed, "the preview should track the keystrokes");
        }

        // Only the commit produces text for the document.
        assert_eq!(c.apply(Step::Commit("你好")), Outcome::Commit("你好".into()));
        assert!(!c.is_active(), "the preview should be gone after committing");
        assert_eq!(c.text(), "");
    }

    /// The bug that made this module necessary: the pane used to watch only for
    /// typed text, which an input method never sends, so nothing arrived at all.
    #[test]
    fn a_commit_is_the_only_thing_that_reaches_the_document() {
        let mut c = Composition::default();
        for typed in ["z", "zh", "zho"] {
            assert_eq!(c.apply(preedit(typed)), Outcome::Composing);
        }
        assert_eq!(c.apply(Step::Commit("中")), Outcome::Commit("中".into()));
    }

    #[test]
    fn a_spurious_empty_event_is_ignored() {
        let mut c = Composition::default();
        // These arrive just from enabling the input method on some platforms.
        assert_eq!(c.apply(preedit("")), Outcome::Ignored);
        assert_eq!(c.apply(Step::Commit("")), Outcome::Ignored);
    }

    #[test]
    fn backspacing_out_of_a_composition_clears_the_preview() {
        let mut c = Composition::default();
        c.apply(preedit("ni"));
        // Backspacing to nothing: here the empty pre-edit is meaningful.
        assert_eq!(c.apply(preedit("")), Outcome::Composing);
        assert!(!c.is_active());
        // ...and the next empty one is spurious again.
        assert_eq!(c.apply(preedit("")), Outcome::Ignored);
    }

    #[test]
    fn an_abandoned_composition_commits_nothing() {
        let mut c = Composition::default();
        c.apply(preedit("nihao"));
        assert_eq!(c.apply(Step::Commit("")), Outcome::Commit(String::new()));
        assert!(!c.is_active());
    }

    /// Enter is delivered as a key press. Some input methods *also* report it
    /// as a commit; taking both would insert two newlines.
    #[test]
    fn a_newline_commit_is_left_to_the_key_handler() {
        let mut c = Composition::default();
        c.apply(preedit("a"));
        assert_eq!(c.apply(Step::Commit("\n")), Outcome::Ignored);
        assert_eq!(c.apply(Step::Commit("\r")), Outcome::Ignored);
        assert!(c.is_active(), "the composition should survive an ignored event");
    }

    #[test]
    fn the_active_clause_is_reported_while_composing() {
        let mut c = Composition::default();
        c.apply(Step::Preedit {
            text: "にほんご",
            active: Some(0..2),
        });
        assert_eq!(c.active(), Some(0..2));

        // An empty pre-edit cannot have an active clause.
        c.apply(Step::Preedit {
            text: "",
            active: Some(0..2),
        });
        assert_eq!(c.active(), None);
    }

    #[test]
    fn cancelling_reports_whether_there_was_anything_to_cancel() {
        let mut c = Composition::default();
        assert!(!c.cancel(), "nothing was being composed");
        c.apply(preedit("ni"));
        assert!(c.cancel(), "a composition was thrown away");
        assert!(!c.is_active());
    }
}
