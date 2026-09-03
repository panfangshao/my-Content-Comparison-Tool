//! The line-number column must not move where text folds.
//!
//! Each pane used to size its digits column from its own document's line
//! count. Two documents either side of a power of ten - 999 lines against
//! 1000 - therefore got columns one digit apart, and since the text width is
//! whatever the pane has left after the column, the two panes wrapped at
//! different widths.
//!
//! That is the same failure as the split-ratio bug in
//! `editor::tests::nearly_equal_panes_fold_long_lines_identically`: a long
//! line folds into a different number of visual lines on each side, the row
//! is drawn at the taller side's height, and the shorter side shows a dead
//! band at the bottom of a row whose text is identical on both sides.
//!
//! `aligned_wrap_width` does not cover it. One digit widens the column by
//! exactly `font.size * 0.62`, which is exactly the tolerance that method is
//! given, and the comparison is a strict `>` between two differently-rounded
//! floats - so it lands just outside and never snaps. Two digits apart (999
//! against 10000) is twice the tolerance and hopeless by construction.
//!
//! The fix is upstream of the tolerance: both panes size the column from the
//! same number, so the widths never diverge in the first place.

use egui::{RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, DiffResult, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editing::VerticalMotion;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

/// Long enough that a fraction of a character of width moves a fold.
fn long_line() -> String {
    format!(
        "INSERT INTO `t` VALUES {}",
        "(52516, 0, 4, 1, 'Sandals of Faith', 35148, 0, 0, 8, 16, -1, 86, 7, 19, 5, 22, 0, 89, 0, 21626, 1, 17371, 0, 50, 0, 300, 1);"
            .repeat(6)
    )
}

/// `n` lines, with the long line at index 5.
fn document(n: usize) -> String {
    let mut lines: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
    lines[5] = long_line();
    lines.join("\n")
}

fn style() -> EditorStyle {
    EditorStyle {
        font: egui::FontId::monospace(14.0),
        line_height: 20.0,
        show_line_numbers: true,
        show_whitespace: false,
        word_wrap: true,
        tab_width: 4,
        vertical_motion: VerticalMotion::KeepColumn,
    }
}

/// Draw both panes for a few frames and report how each side folded row 5.
///
/// The panes are given exactly equal widths, so the split ratio - the other
/// route to this failure, already fixed - cannot be the cause of anything
/// seen here.
fn fold_counts_at(left_lines: usize, right_lines: usize, pane_w: f32) -> (u32, u32) {
    let mut left = TextBuffer::from_text(&document(left_lines));
    let mut right = TextBuffer::from_text(&document(right_lines));
    let diff: DiffResult = diff_lines(
        left.lines(),
        right.lines(),
        &DiffOptions::default(),
        Budget::unlimited(),
    );
    let mut layout = RowLayout::wrapped(diff.rows.len());
    let mut inline = InlineCache::default();
    let s = style();
    let palette = Palette::dark();
    let ctx = egui::Context::default();

    // Both panes size their column from the larger document, exactly as the
    // application does.
    let gutter_lines = left.len_lines().max(right.len_lines());

    for _ in 0..3 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                Vec2::new(pane_w * 2.0 + 60.0, 600.0),
            )),
            ..Default::default()
        };
        let (l, r, d, lay, inl) = (&mut left, &mut right, &diff, &mut layout, &mut inline);

        let mut out = ctx.run_ui(input, |ui| {
            ui.horizontal_top(|ui| {
                for side in [Side::Left, Side::Right] {
                    let (buffer, other) = match side {
                        Side::Left => (&mut *l, r.lines()),
                        Side::Right => (&mut *r, l.lines()),
                    };
                    let mut composing = duibi::ui::ime::Composition::default();
                    // Exactly equal halves: no split-ratio difference at all.
                    ui.allocate_ui(Vec2::new(pane_w, 600.0), |ui| {
                        show_pane(
                            ui,
                            PaneParams {
                                side,
                                buffer,
                                diff: d,
                                diff_generation: 1,
                                diff_options: &DiffOptions::default(),
                                other_lines: other,
                                layout: lay,
                                inline: inl,
                                palette: &palette,
                                style: &s,
                                lang: duibi::i18n::Lang::English,
                                gutter_lines,
                                highlights: &[],
                                highlight_rows: 0..0,
                                search: &[],
                                active_match: None,
                                longest_line: 3000.0,
                                force_offset: None,
                                focus_requested: false,
                                goal_column: None,
                                composing: &mut composing,
                                expanded: &Default::default(),
                            },
                        );
                    });
                }
            });
        });
        out.textures_delta.clear();
        lay.commit_measurements();
    }

    (
        layout.lines_at_side(Side::Left, 5),
        layout.lines_at_side(Side::Right, 5),
    )
}

/// Sweep pane widths and return every width at which the two sides folded
/// row 5 differently.
///
/// A single fixed width proves nothing: a fold only moves when the two wrap
/// widths straddle a fold boundary, and at most widths they do not. The first
/// version of this test used one width, passed with the bug still in place,
/// and was worthless. Sweeping crosses roughly ten fold boundaries.
fn widths_where_the_sides_disagree(left_lines: usize, right_lines: usize) -> Vec<f32> {
    (0..200)
        .map(|i| 600.0 + i as f32 * 0.5)
        .filter(|&w| {
            let (l, r) = fold_counts_at(left_lines, right_lines, w);
            l != r
        })
        .collect()
}

/// The control: documents of the same length have always agreed.
#[test]
fn documents_of_equal_length_fold_identically() {
    let bad = widths_where_the_sides_disagree(999, 999);
    assert!(bad.is_empty(), "identical documents disagreed at {bad:?}");
    // And the fixture really is sensitive to width, or it proves nothing.
    let counts: Vec<u32> = [600.0f32, 650.0, 700.0]
        .iter()
        .map(|&w| fold_counts_at(999, 999, w).0)
        .collect();
    assert!(
        counts.iter().any(|c| *c != counts[0]),
        "the long line never changed its fold count across the sweep: {counts:?}"
    );
}

/// One line more takes the digits column from three to four.
///
/// This one is caught even without the shared column, but only by accident:
/// a single digit widens the column by exactly the tolerance
/// `aligned_wrap_width` is given, and the comparison lands on the snapping
/// side of the knife edge. Pinned so it stays fixed on purpose.
#[test]
fn one_digit_of_difference_does_not_move_a_fold() {
    let bad = widths_where_the_sides_disagree(999, 1000);
    assert!(bad.is_empty(), "999 against 1000 disagreed at {bad:?}");
}

/// Two digits apart is twice that tolerance, so snapping near-equal widths
/// could never rescue it. Before the shared column this diverged across a
/// nine-point band of pane widths: the left folded the row into 12 visual
/// lines and the right into 13, leaving a dead band at the bottom of a row
/// whose text is identical on both sides.
#[test]
fn two_digits_of_difference_does_not_move_a_fold() {
    let bad = widths_where_the_sides_disagree(999, 10_000);
    assert!(
        bad.is_empty(),
        "999 against 10000 folded row 5 differently at {} of 200 pane widths, \
         starting at {:?}",
        bad.len(),
        bad.first()
    );
}
