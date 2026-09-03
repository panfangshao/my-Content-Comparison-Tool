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
//!
//! It also drives syntax highlighting, because the first version of this test
//! passed `highlights: &[]` and so measured a frame that never happened. The
//! real path was costing 60 ms per pane and this test said 143 us.

use std::time::{Duration, Instant};

use egui::{Event, RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editing::VerticalMotion;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::highlight::{PaneHighlighter, SyntaxAssets};
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

const LINES: usize = 10_000;
const WRAPPED_LINES: usize = 100_000;
const VIEW: Vec2 = Vec2::new(900.0, 700.0);

fn document(lines: usize) -> TextBuffer {
    let text: Vec<String> = (0..lines)
        .map(|i| format!("    let value_{i} = compute(input, {i}); // step {i}"))
        .collect();
    TextBuffer::from_text(&text.join("\n"))
}

/// A document whose rows genuinely wrap: most lines are short, but every
/// 32nd is long enough to fold into several visual lines at pane width, so
/// the row heights actually vary.
fn wrapping_document(lines: usize) -> TextBuffer {
    let text: Vec<String> = (0..lines)
        .map(|i| {
            if i % 32 == 0 {
                format!("    // step {i}: {}", "a very long comment that folds over ".repeat(10))
            } else {
                format!("    let value_{i} = compute(input, {i}); // step {i}")
            }
        })
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
    longest: f32,
    first: bool,
    assets: std::sync::Arc<SyntaxAssets>,
    highlighters: [PaneHighlighter; 2],
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
        let longest = left
            .lines()
            .iter()
            .map(|l| {
                l.chars()
                    .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
                    .sum::<f32>()
            })
            .fold(0.0, f32::max);

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
                vertical_motion: VerticalMotion::KeepColumn,
            },
            palette: Palette::dark(),
            longest,
            first: true,
            assets: SyntaxAssets::load(),
            highlighters: {
                let mut a = PaneHighlighter::new();
                let mut b = PaneHighlighter::new();
                a.set_syntax(Some("Rust"));
                b.set_syntax(Some("Rust"));
                [a, b]
            },
        }
    }

    /// The same harness with word wrap on, over a much larger document:
    /// rows have variable heights, so geometry goes through the Fenwick tree.
    fn new_wrapped(lines: usize) -> Self {
        let mut h = Self::new();
        h.left = wrapping_document(lines);
        h.right = wrapping_document(lines);
        h.right.replace_lines(
            lines / 2..lines / 2 + 1,
            &["    let value_mid = changed();".into()],
        );
        h.diff = diff_lines(
            h.left.lines(),
            h.right.lines(),
            &DiffOptions::default(),
            Budget::unlimited(),
        );
        h.layout = RowLayout::wrapped(h.diff.rows.len());
        h.style.word_wrap = true;
        h
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

        // Highlight the rows about to be drawn, as the application does. Ask
        // the layout which row the scroll offset lands on - under word wrap
        // that is a Fenwick lookup, not a division.
        let first_row = self.layout.row_at(scroll_y, self.style.line_height);

        let (left, right) = (&mut self.left, &mut self.right);
        let (diff, layout, inline) = (&self.diff, &mut self.layout, &mut self.inline);
        let (style, palette, longest) = (&self.style, &self.palette, self.longest);
        let opts = DiffOptions::default();

        let rows = first_row..(first_row + 60).min(diff.rows.len());
        let (assets, highlighters) = (&self.assets, &mut self.highlighters);
        let hl: Vec<Vec<_>> = [Side::Left, Side::Right]
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let src = if i == 0 { left.lines() } else { right.lines() };
                highlighters[i].highlight(assets, "base16-ocean.dark", src, rows.clone())
            })
            .collect();

        let gutter_lines = left.len_lines().max(right.len_lines());
        // Never typed into; the pane just needs somewhere to keep it.
        let mut composing = [
            duibi::ui::ime::Composition::default(),
            duibi::ui::ime::Composition::default(),
        ];

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
                                lang: duibi::i18n::Lang::English,
                                gutter_lines,
                                highlights: &hl[usize::from(side == Side::Right)],
                                highlight_rows: rows.clone(),
                                search: &[],
                                active_match: None,
                                longest_line: longest,
                                force_offset: Some(Vec2::new(0.0, scroll_y)),
                                focus_requested: false,
                                goal_column: None,
                                composing: &mut composing[usize::from(side == Side::Right)],
                                expanded: &Default::default(),
                            },
                        );
                    });
                }
            });
        });
        out.textures_delta.clear();
        // Fold this frame's row measurements into the geometry, as the app
        // does after both panes have drawn. A no-op in uniform mode.
        self.layout.commit_measurements();
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

/// The same frame budget over a 100 000-line document with word wrap on.
///
/// Wrapped rows have variable heights, so every "which row is at `y`?" goes
/// through the Fenwick tree in `RowLayout` instead of a division. If that
/// ever regresses to a linear scan - or if measuring a visible row starts
/// rebuilding geometry for the whole document - jumping to a random depth in
/// a huge file stutters, and only a measurement catches it.
///
/// The budget is 4x the uniform test's: wrapping pays a real galley layout
/// per visible row, so the threshold is deliberately loose. It is here to
/// catch work that scales with the *document*, not to police milliseconds.
#[test]
fn drawing_a_wrapped_huge_comparison_stays_interactive() {
    let mut h = Harness::new_wrapped(WRAPPED_LINES);
    assert!(
        !h.layout.is_uniform(),
        "the test must exercise the Fenwick path, not the uniform shortcut"
    );

    // Deterministic pseudo-random scroll positions (xorshift64*), spread over
    // the whole document so no two frames can be served from the same window.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut random_scroll = |layout: &RowLayout, line_h: f32| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let span = (layout.total_height(line_h) - VIEW.y).max(1.0);
        (state % span as u64) as f32
    };

    // Warm up: font atlas, galley cache, first measurements.
    for _ in 0..6 {
        let y = random_scroll(&h.layout, h.style.line_height);
        h.frame(y);
    }

    const FRAMES: usize = 30;
    let start = Instant::now();
    for _ in 0..FRAMES {
        let y = random_scroll(&h.layout, h.style.line_height);
        h.frame(y);
    }
    let per_frame = start.elapsed() / FRAMES as u32;

    // Sanity: wrapping actually produced variable-height rows - otherwise the
    // test would be timing the uniform path in disguise.
    let tallest = (0..h.layout.row_count())
        .map(|r| h.layout.lines_at(r))
        .max()
        .unwrap_or(0);
    assert!(tallest > 1, "no row was ever measured taller than one line");

    eprintln!(
        "{WRAPPED_LINES} wrapped lines, two panes: {per_frame:?} per frame ({:.1} fps equivalent)",
        1.0 / per_frame.as_secs_f64()
    );
    assert!(
        per_frame < Duration::from_millis(200),
        "a wrapped frame took {per_frame:?}; something is scaling with the document size"
    );
}
