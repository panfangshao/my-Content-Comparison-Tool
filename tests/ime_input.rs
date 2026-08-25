//! Typing Chinese, Japanese or Korean into a pane.
//!
//! An input method never produces `Event::Text`. It sends a run of
//! `ImeEvent::Preedit` while the user is still choosing, then one
//! `ImeEvent::Commit` with the result. The pane used to watch only for
//! `Event::Text`, so **nothing typed with an input method arrived at all**.
//!
//! Worse, the pane never reported an IME area, and the winit integration
//! decides whether to allow an input method at all from exactly that:
//!
//! ```ignore
//! let allow_ime = ime.is_some();
//! window.set_ime_allowed(allow_ime);
//! ```
//!
//! So the input method was switched off for the window and could not have
//! delivered anything even if the pane had been listening.
//!
//! Both halves are covered here, against a real `egui::Context` - the same
//! reasoning as `caret_scroll.rs`: a screenshot harness cannot test this,
//! because a background window has no OS focus and correctly ignores input.

use egui::{Event, ImeEvent, Key, Modifiers, RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editing::VerticalMotion;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::ime::Composition;
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

const VIEW_H: f32 = 300.0;

fn preedit(text: &str) -> Event {
    Event::Ime(ImeEvent::Preedit {
        text: text.to_owned(),
        active_range_chars: None,
    })
}

fn commit(text: &str) -> Event {
    Event::Ime(ImeEvent::Commit(text.to_owned()))
}

struct Harness {
    ctx: egui::Context,
    buffer: TextBuffer,
    diff: DiffResult,
    layout: RowLayout,
    inline: InlineCache,
    composing: Composition,
    style: EditorStyle,
    palette: Palette,
    first: bool,
    /// What the last frame told the integration about the input method. Read
    /// from the frame's own output: `Context::output()` is cleared each frame,
    /// so it cannot answer this after the fact.
    ime: Option<egui::output::IMEOutput>,
}

impl Harness {
    fn new(text: &str) -> Self {
        let buffer = TextBuffer::from_text(text);
        let diff = diff_lines(
            buffer.lines(),
            buffer.lines(),
            &DiffOptions::default(),
            Budget::unlimited(),
        );
        let layout = RowLayout::uniform(diff.rows.len());

        Self {
            ctx: egui::Context::default(),
            buffer,
            diff,
            layout,
            inline: InlineCache::default(),
            composing: Composition::default(),
            style: EditorStyle {
                font: egui::FontId::monospace(14.0),
                line_height: 20.0,
                show_line_numbers: true,
                show_whitespace: false,
                word_wrap: false,
                tab_width: 4,
                vertical_motion: VerticalMotion::KeepColumn,
                gutter_width: 0.0,
            },
            palette: Palette::dark(),
            first: true,
            ime: None,
        }
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        if self.first {
            // Without this the pane has no focus and ignores everything.
            events.insert(0, Event::WindowFocused(true));
        }
        let focus_requested = std::mem::take(&mut self.first);

        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                Vec2::new(800.0, VIEW_H),
            )),
            events,
            ..Default::default()
        };

        let (buffer, diff, layout, inline, style, palette, composing) = (
            &mut self.buffer,
            &self.diff,
            &mut self.layout,
            &mut self.inline,
            &self.style,
            &self.palette,
            &mut self.composing,
        );
        let empty: Vec<String> = Vec::new();

        let mut out = self.ctx.run_ui(input, |ui| {
            show_pane(
                ui,
                PaneParams {
                    side: Side::Left,
                    buffer,
                    diff,
                    diff_generation: 1,
                    diff_options: &DiffOptions::default(),
                    other_lines: &empty,
                    layout,
                    inline,
                    palette,
                    style,
                    highlights: &[],
                    highlight_rows: 0..0,
                    search: &[],
                    active_match: None,
                    longest_line: 32,
                    force_offset: None,
                    focus_requested,
                    goal_column: None,
                    composing,
                },
            );
        });
        self.ime = out.platform_output.ime;
        out.textures_delta.clear();
    }

    fn text(&self) -> String {
        self.buffer.text()
    }
}

/// The root cause. Without an IME area reported to the integration,
/// `set_ime_allowed(false)` is called on the window and no input method can
/// compose in it at all.
#[test]
fn a_focused_pane_switches_the_input_method_on() {
    let mut h = Harness::new("");
    h.frame(Vec::new());
    assert!(
        h.ime.is_some(),
        "the pane reported no IME area, so the integration disables the input method"
    );
}

/// The candidate window has to follow the caret, or it sits in the corner of
/// the screen while the text appears somewhere else.
#[test]
fn the_candidate_window_follows_the_caret_down_the_document() {
    let mut h = Harness::new(&(0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
    h.frame(Vec::new());

    let at_top = h.ime.map(|i| i.cursor_rect);
    for _ in 0..8 {
        h.frame(vec![Event::Key {
            key: Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]);
    }
    let lower = h.ime.map(|i| i.cursor_rect);

    let (top, lower) = (at_top.expect("declared"), lower.expect("declared"));
    assert!(
        lower.top() > top.top(),
        "the candidate anchor stayed at {top:?} while the caret moved down to {lower:?}"
    );
}

/// The whole point: pinyin in, characters out.
#[test]
fn composing_pinyin_puts_the_characters_in_the_document() {
    let mut h = Harness::new("");
    h.frame(Vec::new());

    for typed in ["n", "ni", "nih", "niha", "nihao"] {
        h.frame(vec![preedit(typed)]);
        assert_eq!(
            h.text(),
            "",
            "the pre-edit `{typed}` leaked into the document"
        );
    }

    h.frame(vec![commit("你好")]);
    assert_eq!(h.text(), "你好", "the committed text never arrived");
}

/// Why the pre-edit is kept out of the buffer: one word must be one undo step.
///
/// egui's own `TextEdit` inserts each pre-edit into the buffer, which would
/// make `你好` cost five presses of Ctrl+Z to remove.
#[test]
fn a_composed_word_is_a_single_undo_step() {
    let mut h = Harness::new("");
    h.frame(Vec::new());

    for typed in ["n", "ni", "nih", "niha", "nihao"] {
        h.frame(vec![preedit(typed)]);
    }
    h.frame(vec![commit("你好")]);
    assert_eq!(h.text(), "你好");

    assert!(h.buffer.undo(), "there should be something to undo");
    assert_eq!(h.text(), "", "one undo did not remove the whole word");
}

/// Composing must not touch the document version, or every pinyin letter
/// re-runs the diff over the whole file.
#[test]
fn composing_does_not_disturb_the_document() {
    let mut h = Harness::new("hello");
    h.frame(Vec::new());
    let before = h.buffer.version();

    for typed in ["z", "zh", "zho", "zhon", "zhong"] {
        h.frame(vec![preedit(typed)]);
    }
    assert_eq!(
        h.buffer.version(),
        before,
        "composing bumped the document version, which re-runs the whole comparison"
    );

    h.frame(vec![commit("中")]);
    assert_ne!(h.buffer.version(), before, "the commit should be an edit");
}

/// Ordinary typing must keep working alongside the new path.
#[test]
fn plain_typing_is_unaffected() {
    let mut h = Harness::new("");
    h.frame(Vec::new());
    h.frame(vec![Event::Text("abc".into())]);
    assert_eq!(h.text(), "abc");
}

/// An abandoned composition leaves nothing behind.
#[test]
fn dismissing_a_composition_writes_nothing() {
    let mut h = Harness::new("x");
    h.frame(Vec::new());

    h.frame(vec![preedit("ni")]);
    h.frame(vec![preedit("")]); // backspaced away
    h.frame(vec![commit("")]); // and dismissed
    assert_eq!(h.text(), "x", "an abandoned composition changed the document");
}
