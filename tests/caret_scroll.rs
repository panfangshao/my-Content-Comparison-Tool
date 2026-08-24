//! The view must follow the caret.
//!
//! Moving the caret past the bottom of the viewport used to leave the view
//! where it was, so the caret simply vanished off screen.
//!
//! This drives the pane widget against a real `egui::Context`. The screenshot
//! harness cannot cover this: `Response::has_focus()` is gated on
//! `input.focused`, i.e. whether the *window* has OS focus, and a window
//! launched in the background does not - so keyboard input is correctly
//! ignored there and nothing ever moves.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

const LINES: usize = 200;
/// Short enough that 200 lines cannot possibly fit.
const VIEW_H: f32 = 300.0;

fn arrow(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

struct Pane {
    ctx: egui::Context,
    buffer: TextBuffer,
    diff: DiffResult,
    layout: RowLayout,
    inline: InlineCache,
    style: EditorStyle,
    palette: Palette,
    offset: Vec2,
    first: bool,
}

impl Pane {
    fn new() -> Self {
        Self::with_lines(LINES)
    }

    fn with_lines(n: usize) -> Self {
        let text: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
        let buffer = TextBuffer::from_text(&text.join("\n"));
        // Comparing the document with itself gives one Equal row per line,
        // which is exactly the geometry a single pane renders against.
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
            style: EditorStyle {
                font: egui::FontId::monospace(14.0),
                line_height: 20.0,
                show_line_numbers: true,
                show_whitespace: false,
                word_wrap: false,
                tab_width: 4,
                gutter_width: 0.0,
            },
            palette: Palette::dark(),
            offset: Vec2::ZERO,
            first: true,
        }
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        if self.first {
            // Tell egui the window is focused, or the pane ignores keys.
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

        let mut offset = self.offset;
        let (buffer, diff, layout, inline, style, palette) = (
            &mut self.buffer,
            &self.diff,
            &mut self.layout,
            &mut self.inline,
            &self.style,
            &self.palette,
        );
        let empty: Vec<String> = Vec::new();

        let mut out = self.ctx.run_ui(input, |ui| {
            let result = show_pane(
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
                },
            );
            offset = result.offset;
        });
        out.textures_delta.clear();
        self.offset = offset;
    }

    /// Idle frames, so any scroll animation finishes.
    fn settle(&mut self) {
        for _ in 0..40 {
            self.frame(Vec::new());
        }
    }

    fn caret_line(&self) -> usize {
        self.buffer.line_col(self.buffer.selection().head).0
    }

    /// Is the caret's row inside the visible band?
    fn caret_visible(&self) -> bool {
        let row = self
            .diff
            .row_of(Side::Left, self.caret_line())
            .expect("every line has a row");
        let top = self.layout.row_top(row, self.style.line_height);
        let bottom = top + self.layout.row_height(row, self.style.line_height);
        // Allow a little slack for the header and scrollbar chrome.
        top >= self.offset.y - 1.0 && bottom <= self.offset.y + VIEW_H + 1.0
    }

    fn press(&mut self, key: Key, times: usize) {
        for _ in 0..times {
            self.frame(vec![arrow(key)]);
        }
        self.settle();
    }

    /// A full press-and-release at a point inside the pane.
    fn click(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }]);
        self.frame(vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        self.settle();
    }

    fn caret(&self) -> usize {
        self.buffer.selection().head
    }
}

#[test]
fn moving_down_past_the_fold_scrolls_the_view() {
    let mut p = Pane::new();
    p.settle();
    assert_eq!(p.offset.y, 0.0, "should start at the top");

    p.press(Key::ArrowDown, 80);

    assert_eq!(p.caret_line(), 80, "the caret should have moved 80 lines");
    assert!(
        p.offset.y > 0.0,
        "the view never scrolled: offset is still {}",
        p.offset.y
    );
    assert!(
        p.caret_visible(),
        "the caret is off screen: row top {} vs viewport {}..{}",
        p.layout
            .row_top(p.caret_line(), p.style.line_height),
        p.offset.y,
        p.offset.y + VIEW_H
    );
}

#[test]
fn moving_back_up_scrolls_the_view_back() {
    let mut p = Pane::new();
    p.settle();
    p.press(Key::ArrowDown, 120);
    let deep = p.offset.y;
    assert!(deep > 0.0);

    p.press(Key::ArrowUp, 120);
    assert_eq!(p.caret_line(), 0);
    assert!(
        p.offset.y < deep,
        "coming back up should scroll back, but the offset stayed at {}",
        p.offset.y
    );
    assert!(p.caret_visible());
}

/// The scroll should be *minimal* - stepping one line past the edge should not
/// jump the caret to the middle of the screen.
#[test]
fn the_view_scrolls_only_as_far_as_it_has_to() {
    let mut p = Pane::new();
    p.settle();

    // Walk down to just past the bottom edge of the viewport.
    let rows_visible = (VIEW_H / p.style.line_height) as usize;
    p.press(Key::ArrowDown, rows_visible + 2);

    assert!(p.caret_visible());
    // A minimal scroll keeps the caret near the bottom, not centred.
    let caret_top = p
        .layout
        .row_top(p.caret_line(), p.style.line_height);
    let from_bottom = p.offset.y + VIEW_H - caret_top;
    assert!(
        from_bottom < VIEW_H / 2.0,
        "the caret ended up {from_bottom} from the bottom - the view over-scrolled"
    );
}

#[test]
fn an_idle_frame_does_not_scroll() {
    let mut p = Pane::new();
    p.settle();
    p.press(Key::ArrowDown, 60);
    let settled = p.offset.y;

    p.settle();
    assert_eq!(
        p.offset.y, settled,
        "the view drifted with no input at all"
    );
}

/// Clicking below the last line must still place the caret.
///
/// The pane used to allocate only as much height as its text, so a short
/// document left most of the pane inert - with an empty one, only the single
/// row beside line number 1 responded at all.
#[test]
fn clicking_in_the_empty_area_below_the_text_places_the_caret() {
    let mut p = Pane::with_lines(3);
    p.settle();
    assert_eq!(p.caret(), 0);

    // Far below the third line, but well inside the viewport.
    p.click(pos2(300.0, VIEW_H - 40.0));

    assert_eq!(
        p.caret_line(),
        2,
        "clicking past the end should land on the last line, not do nothing"
    );
}

/// The case that prompted this: a brand new, empty document.
#[test]
fn an_empty_document_accepts_a_click_anywhere() {
    let mut p = Pane::with_lines(0);
    p.settle();

    p.click(pos2(400.0, VIEW_H / 2.0));
    p.frame(vec![Event::Text("hello".to_owned())]);
    p.settle();

    assert_eq!(
        p.buffer.line(0),
        "hello",
        "clicking an empty editor should let you start typing straight away"
    );
}

#[test]
fn clicking_past_the_end_of_a_line_goes_to_that_line_end() {
    let mut p = Pane::with_lines(3);
    p.settle();

    // Line 1 is "line 1"; click far to its right.
    p.click(pos2(700.0, 20.0 * 1.5));
    let (line, col) = p.buffer.line_col(p.caret());
    assert_eq!(line, 1);
    assert_eq!(col, p.buffer.line(1).chars().count(), "should clamp to the line end");
}

#[test]
fn a_click_still_lands_on_the_right_line_in_the_middle_of_the_text() {
    let mut p = Pane::with_lines(50);
    p.settle();

    // Row 5 of a 20pt grid.
    p.click(pos2(120.0, 20.0 * 5.0 + 5.0));
    assert_eq!(p.caret_line(), 5);
}
