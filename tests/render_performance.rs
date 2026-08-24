//! Per-frame cost of drawing a pane over a large document.
//!
//! The comparison itself is covered by `performance.rs`. This covers the other
//! half: what it costs to draw a frame *between* comparisons, which is what
//! decides whether scrolling and typing in a big file feel smooth.
//!
//! It exists because that path once rebuilt an owned copy of **both** documents
//! on every call - and since a frame draws two panes, that was four full copies
//! of the text per frame. A 10k-line file meant tens of thousands of string
//! allocations between one frame and the next. Nothing about the output looked
//! wrong, so only a measurement catches it.

use std::time::{Duration, Instant};

use egui::{Event, RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

const LINES: usize = 10_000;
const VIEW: Vec2 = Vec2::new(900.0, 700.0);

fn document(lines: usize) -> TextBuffer {
    let text: Vec<String> = (0..lines)
        .map(|i| format!("    let value_{i} = compute(input, {i}); // step {i}"))
        .collect();
    TextBuffer::from_text(&text.join("\n"))
}

struct Harness {
    ctx: egui::Context,
    left: TextBuffer,
    right: TextBuffer,
    diff: DiffResult,
    layout: RowLayout,
    inline: InlineCache,
    style: EditorStyle,
    palette: Palette,
    longest: usize,
    first: bool,
}

impl Harness {
    fn new() -> Self {
        let left = document(LINES);
        let mut right = document(LINES);
        // A couple of scattered edits, as in a real comparison.
        right.replace_lines(1_000..1_001, &["    let value_1000 = changed();".into()]);
        right.replace_lines(7_500..7_501, &["    let value_7500 = also_changed();".into()]);

        let diff = diff_lines(
            left.lines(),
            right.lines(),
            &DiffOptions::default(),
            Budget::unlimited(),
        );
        let layout = RowLayout::uniform(diff.rows.len());
        let longest = left.lines().iter().map(|l| l.chars().count()).max().unwrap_or(0);

        Self {
            ctx: egui::Context::default(),
            left,
            right,
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
            longest,
            first: true,
        }
    }

    /// Draw both panes once, exactly as a real frame does.
    fn frame(&mut self, scroll_y: f32) {
        let mut events = Vec::new();
        if std::mem::take(&mut self.first) {
            events.push(Event::WindowFocused(true));
        }
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), VIEW)),
            events,
            ..Default::default()
        };

        let (left, right) = (&mut self.left, &mut self.right);
        let (diff, layout, inline) = (&self.diff, &mut self.layout, &mut self.inline);
        let (style, palette, longest) = (&self.style, &self.palette, self.longest);
        let opts = DiffOptions::default();

        let mut out = self.ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                for side in [Side::Left, Side::Right] {
                    let (buffer, other) = match side {
                        Side::Left => (&mut *left, right.lines()),
                        Side::Right => (&mut *right, left.lines()),
                    };
                    ui.allocate_ui(Vec2::new(VIEW.x / 2.0, VIEW.y), |ui| {
                        show_pane(
                            ui,
                            PaneParams {
                                side,
                                buffer,
                                diff,
                                diff_generation: 1,
                                diff_options: &opts,
                                other_lines: other,
                                layout,
                                inline,
                                palette,
                                style,
                                highlights: &[],
                                highlight_rows: 0..0,
                                search: &[],
                                active_match: None,
                                longest_line: longest,
                                force_offset: Some(Vec2::new(0.0, scroll_y)),
                                focus_requested: false,
                                goal_column: None,
                            },
                        );
                    });
                }
            });
        });
        out.textures_delta.clear();
    }
}

/// A frame must stay well inside a 60 fps budget while scrolled deep into a
/// 10 000-line comparison.
///
/// The threshold is deliberately loose - this is here to catch work that scales
/// with the *document*, not to police milliseconds on a busy machine.
#[test]
fn drawing_a_large_comparison_stays_interactive() {
    let mut h = Harness::new();

    // Warm up: font atlas, galley cache, first layout.
    for i in 0..10 {
        h.frame(i as f32 * 100.0);
    }

    const FRAMES: usize = 40;
    let start = Instant::now();
    for i in 0..FRAMES {
        // Scroll around so no frame can be served entirely from cache.
        h.frame(60_000.0 + (i as f32) * 37.0);
    }
    let per_frame = start.elapsed() / FRAMES as u32;

    eprintln!(
        "{LINES} lines, two panes: {per_frame:?} per frame ({:.1} fps equivalent)",
        1.0 / per_frame.as_secs_f64()
    );
    assert!(
        per_frame < Duration::from_millis(50),
        "a frame took {per_frame:?}; something is scaling with the document size"
    );
}

/// Drawing must not allocate proportionally to the document.
///
/// Measured indirectly: a pane over 10x the text should not cost anything like
/// 10x the time, because only the visible rows are touched.
#[test]
fn frame_cost_does_not_scale_with_document_size() {
    fn measure(lines: usize) -> Duration {
        let mut h = Harness::new();
        h.left = document(lines);
        h.right = document(lines);
        h.diff = diff_lines(
            h.left.lines(),
            h.right.lines(),
            &DiffOptions::default(),
            Budget::unlimited(),
        );
        h.layout = RowLayout::uniform(h.diff.rows.len());

        for _ in 0..8 {
            h.frame(0.0);
        }
        let start = Instant::now();
        for i in 0..20 {
            h.frame(i as f32 * 13.0);
        }
        start.elapsed() / 20
    }

    let small = measure(1_000);
    let large = measure(20_000);
    eprintln!("1k lines: {small:?} per frame, 20k lines: {large:?} per frame");

    // Twenty times the text, nowhere near twenty times the work. The generous
    // factor absorbs the parts that genuinely are O(document) but cheap, such
    // as building the row layout.
    assert!(
        large < small * 6 + Duration::from_millis(4),
        "frame cost grew with the document: {small:?} -> {large:?}"
    );
}
