//! The column between the two panes: change bands and merge arrows.
//!
//! Because both panes render the same aligned row list, a hunk occupies the
//! *same* vertical span on both sides. That turns what other diff tools draw as
//! a sheared connector polygon into a simple band, which is easier to read and
//! far easier to hit with the mouse.
//!
//! Each hunk gets two arrows. The direction is stated from the reader's point
//! of view: `>` pushes the left version onto the right, `<` pulls the right
//! version onto the left. Clicking either one is a single undo step.

use egui::{Align2, Color32, FontId, Rect, Sense, Ui, Vec2, pos2};

use crate::core::diff::{DiffResult, RowKind};
use crate::core::merge::Direction;
use crate::ui::rowlayout::RowLayout;
use crate::ui::theme::Palette;

/// Width of the whole column.
pub const GUTTER_WIDTH: f32 = 46.0;
/// Diameter of an arrow button.
const BUTTON: f32 = 17.0;

pub struct GutterParams<'a> {
    pub diff: &'a DiffResult,
    pub layout: &'a RowLayout,
    pub palette: &'a Palette,
    pub line_height: f32,
    /// Vertical scroll offset shared with the panes.
    pub scroll_y: f32,
    /// Hunk the caret is currently inside, drawn with emphasis.
    pub focused_hunk: Option<usize>,
    /// Merging is disabled while a comparison is still catching up.
    pub enabled: bool,
}

/// What the user asked for by clicking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MergeRequest {
    pub hunk: usize,
    pub direction: Direction,
}

#[derive(Default)]
pub struct GutterOutput {
    pub merge: Option<MergeRequest>,
    /// Horizontal drag on the column background, for resizing the split.
    ///
    /// The arrows are interacted with *after* the background, so a click on an
    /// arrow wins and only drags that miss the buttons resize the panes.
    pub drag_x: f32,
}

/// Draw the column.
pub fn show_gutter(ui: &mut Ui, p: GutterParams<'_>) -> GutterOutput {
    let GutterParams {
        diff,
        layout,
        palette,
        line_height,
        scroll_y,
        focused_hunk,
        enabled,
    } = p;

    let (rect, bg_response) = ui.allocate_exact_size(
        Vec2::new(GUTTER_WIDTH, ui.available_height()),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    let mut out = GutterOutput {
        drag_x: bg_response.drag_delta().x,
        ..Default::default()
    };
    if bg_response.hovered() || bg_response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }

    painter.rect_filled(rect, 0, palette.chrome_bg);
    painter.vline(
        rect.left() + 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, palette.border),
    );
    painter.vline(
        rect.right() - 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, palette.border),
    );

    // Only hunks whose band intersects the viewport are worth drawing, and a
    // binary search beats scanning thousands of hunks every frame.
    let first_row = layout.row_at(scroll_y, line_height);
    let last_row = layout.row_at(scroll_y + rect.height(), line_height) + 1;
    let start = diff.hunks.partition_point(|h| h.rows.end <= first_row);

    for (i, hunk) in diff.hunks.iter().enumerate().skip(start) {
        if hunk.rows.start > last_row {
            break;
        }

        let top = rect.top() + layout.row_top(hunk.rows.start, line_height) - scroll_y;
        let bottom = rect.top() + layout.row_top(hunk.rows.end, line_height) - scroll_y;
        let band = Rect::from_min_max(
            pos2(rect.left() + 1.0, top),
            pos2(rect.right() - 1.0, bottom.max(top + 2.0)),
        );

        let kind = dominant_kind(diff, hunk.rows.clone());
        let color = palette.marker(kind).unwrap_or(palette.text_faint);

        // A translucent wash ties the band to the rows on either side.
        painter.rect_filled(band, 0, color.gamma_multiply(0.18));
        if focused_hunk == Some(i) {
            painter.rect_stroke(
                band,
                2,
                egui::Stroke::new(1.5, palette.focus_hunk),
                egui::StrokeKind::Inside,
            );
        }

        if !enabled {
            continue;
        }

        // Arrows sit at the band's centre, but never leave the viewport: a
        // hunk taller than the window still has reachable buttons.
        //
        // Both bounds are clamped with `clamp_between` rather than `f32::clamp`
        // because the two limits can cross - a one-row hunk peeking in at the
        // very bottom of the viewport gives `lo > hi`, and `f32::clamp` panics
        // on that.
        let half = BUTTON / 2.0;
        let cy = clamp_between(
            (top + bottom) / 2.0,
            top.max(rect.top()) + half,
            bottom.min(rect.bottom()) - half,
        );
        let cy = clamp_between(cy, rect.top() + half, rect.bottom() - half);

        let to_right = Rect::from_center_size(
            pos2(rect.center().x - BUTTON * 0.58, cy),
            Vec2::splat(BUTTON),
        );
        let to_left = Rect::from_center_size(
            pos2(rect.center().x + BUTTON * 0.58, cy),
            Vec2::splat(BUTTON),
        );

        if arrow_button(ui, to_right, ">", color, palette, ("dbr", i)) {
            out.merge = Some(MergeRequest {
                hunk: i,
                direction: Direction::ToRight,
            });
        }
        if arrow_button(ui, to_left, "<", color, palette, ("dbl", i)) {
            out.merge = Some(MergeRequest {
                hunk: i,
                direction: Direction::ToLeft,
            });
        }
    }

    // A drag that landed on an arrow is a click, not a resize.
    if out.merge.is_some() {
        out.drag_x = 0.0;
    }
    out
}

/// `clamp` that tolerates the bounds being the wrong way round.
///
/// `f32::clamp` panics when `min > max`, which is easy to hit here: the limits
/// are derived from a hunk's band and the viewport, and those can cross when a
/// hunk is only partly on screen.
fn clamp_between(value: f32, a: f32, b: f32) -> f32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    value.clamp(lo, hi)
}

/// One arrow. Returns true on click.
fn arrow_button(
    ui: &mut Ui,
    rect: Rect,
    glyph: &str,
    accent: Color32,
    palette: &Palette,
    id: (&str, usize),
) -> bool {
    // Buttons outside the viewport must not steal clicks.
    if !ui.clip_rect().intersects(rect) {
        return false;
    }
    let response = ui.interact(rect, egui::Id::new(id), Sense::click());
    let painter = ui.painter();

    let (bg, fg) = if response.is_pointer_button_down_on() {
        (accent, palette.on_accent)
    } else if response.hovered() {
        (accent.gamma_multiply(0.55), palette.on_accent)
    } else {
        (palette.chrome_bg, accent)
    };

    painter.circle_filled(rect.center(), rect.width() / 2.0 - 1.0, bg);
    painter.circle_stroke(
        rect.center(),
        rect.width() / 2.0 - 1.0,
        egui::Stroke::new(1.0, accent.gamma_multiply(0.8)),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::monospace(rect.width() * 0.62),
        fg,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// The change kind that best describes a hunk.
///
/// A hunk with any modified line reads as "modified"; otherwise whichever of
/// insert/delete is present. This is what decides the band's colour.
fn dominant_kind(diff: &DiffResult, rows: std::ops::Range<usize>) -> RowKind {
    let mut has_ins = false;
    let mut has_del = false;
    for row in &diff.rows[rows.start.min(diff.rows.len())..rows.end.min(diff.rows.len())] {
        match row.kind {
            RowKind::Replace => return RowKind::Replace,
            RowKind::Insert => has_ins = true,
            RowKind::Delete => has_del = true,
            _ => {}
        }
    }
    match (has_ins, has_del) {
        // Both present but no paired line: still an edit of the region.
        (true, true) => RowKind::Replace,
        (true, false) => RowKind::Insert,
        (false, true) => RowKind::Delete,
        (false, false) => RowKind::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{Budget, DiffOptions, diff_lines};

    fn diff_of(a: &str, b: &str) -> DiffResult {
        let l: Vec<String> = a.lines().map(str::to_owned).collect();
        let r: Vec<String> = b.lines().map(str::to_owned).collect();
        diff_lines(&l, &r, &DiffOptions::default(), Budget::unlimited())
    }

    #[test]
    fn a_modified_hunk_reads_as_modified() {
        let d = diff_of("a\nb\nc", "a\nB\nc");
        assert_eq!(dominant_kind(&d, d.hunks[0].rows.clone()), RowKind::Replace);
    }

    #[test]
    fn a_pure_insertion_reads_as_inserted() {
        let d = diff_of("a\nc", "a\nb\nc");
        assert_eq!(dominant_kind(&d, d.hunks[0].rows.clone()), RowKind::Insert);
    }

    #[test]
    fn a_pure_deletion_reads_as_deleted() {
        let d = diff_of("a\nb\nc", "a\nc");
        assert_eq!(dominant_kind(&d, d.hunks[0].rows.clone()), RowKind::Delete);
    }

    #[test]
    fn a_mixed_hunk_reads_as_modified() {
        // Two lines replaced by three: paired lines make it a Replace, and even
        // the unpaired remainder should not downgrade the band's colour.
        let d = diff_of("x\na\nb\ny", "x\np\nq\nr\ny");
        assert_eq!(dominant_kind(&d, d.hunks[0].rows.clone()), RowKind::Replace);
    }

    #[test]
    fn an_out_of_range_row_span_does_not_panic() {
        let d = diff_of("a", "a");
        assert_eq!(dominant_kind(&d, 0..999), RowKind::Equal);
    }

    #[test]
    fn clamp_between_tolerates_crossed_bounds() {
        assert_eq!(clamp_between(5.0, 0.0, 10.0), 5.0);
        assert_eq!(clamp_between(-5.0, 0.0, 10.0), 0.0);
        assert_eq!(clamp_between(50.0, 0.0, 10.0), 10.0);
        // The case `f32::clamp` would panic on.
        assert_eq!(clamp_between(5.0, 10.0, 0.0), 5.0);
        assert_eq!(clamp_between(-1.0, 10.0, 0.0), 0.0);
    }

    #[test]
    fn merge_requests_name_a_real_hunk_and_direction() {
        let r = MergeRequest {
            hunk: 2,
            direction: Direction::ToRight,
        };
        assert_eq!(r.direction.source(), crate::core::diff::Side::Left);
        assert_eq!(r.hunk, 2);
    }
}
