//! The overview strip down the right-hand edge.
//!
//! A whole-document map of where the differences are, at a glance. On a
//! 20 000-line file with eight scattered edits, this is the difference between
//! "scroll and hope" and "click the third mark".
//!
//! Rows are compressed onto pixels, so several hunks can land on the same
//! pixel. Each mark is therefore drawn with a minimum height and the strongest
//! change kind in that band wins the colour - losing a difference entirely
//! because it rounded away would defeat the point.

use egui::{Rect, Sense, Ui, Vec2, pos2};

use crate::core::diff::{DiffResult, RowKind};
use crate::ui::theme::Palette;

pub const OVERVIEW_WIDTH: f32 = 14.0;
/// Shortest a mark may be drawn, so a one-line change is still clickable.
const MIN_MARK: f32 = 3.0;

pub struct OverviewParams<'a> {
    pub diff: &'a DiffResult,
    pub palette: &'a Palette,
    /// Fraction of the document currently on screen, `0.0..=1.0`.
    pub viewport: std::ops::Range<f32>,
    pub focused_hunk: Option<usize>,
}

/// Draw the strip. Returns the row the user clicked, if any.
pub fn show_overview(ui: &mut Ui, p: OverviewParams<'_>) -> Option<usize> {
    let OverviewParams {
        diff,
        palette,
        viewport,
        focused_hunk,
    } = p;

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(OVERVIEW_WIDTH, ui.available_height()),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0, palette.gutter_bg);
    painter.vline(
        rect.left() + 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, palette.border),
    );

    let total = diff.rows.len();
    if total == 0 {
        return None;
    }

    // The band of the document currently on screen.
    let vp = Rect::from_min_max(
        pos2(rect.left() + 1.0, rect.top() + viewport.start * rect.height()),
        pos2(
            rect.right() - 1.0,
            (rect.top() + viewport.end * rect.height()).max(rect.top() + viewport.start * rect.height() + 6.0),
        ),
    );
    painter.rect_filled(vp, 2, palette.text_faint.gamma_multiply(0.22));

    let scale = rect.height() / total as f32;
    for (i, hunk) in diff.hunks.iter().enumerate() {
        let y = rect.top() + hunk.rows.start as f32 * scale;
        let h = (hunk.rows.len() as f32 * scale).max(MIN_MARK);
        let kind = strongest_kind(diff, hunk.rows.clone());
        let color = palette.marker(kind).unwrap_or(palette.text_faint);

        let mark = Rect::from_min_max(
            pos2(rect.left() + 3.0, y),
            pos2(rect.right() - 3.0, y + h),
        );
        painter.rect_filled(mark, 1, color);

        if focused_hunk == Some(i) {
            painter.rect_stroke(
                mark.expand(1.5),
                2,
                egui::Stroke::new(1.0, palette.accent),
                egui::StrokeKind::Outside,
            );
        }
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Click or drag anywhere to jump there.
    if let Some(pos) = response.interact_pointer_pos()
        && (response.clicked() || response.dragged())
    {
        let frac = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        return Some(((frac * total as f32) as usize).min(total.saturating_sub(1)));
    }
    None
}

/// Most significant change kind in a row span, for colouring a compressed mark.
fn strongest_kind(diff: &DiffResult, rows: std::ops::Range<usize>) -> RowKind {
    let end = rows.end.min(diff.rows.len());
    let start = rows.start.min(end);
    let mut kind = RowKind::Equal;
    for row in &diff.rows[start..end] {
        match row.kind {
            // Modified outranks both, since it means content was rewritten.
            RowKind::Replace => return RowKind::Replace,
            RowKind::Insert if kind == RowKind::Equal => kind = RowKind::Insert,
            RowKind::Delete if kind == RowKind::Equal => kind = RowKind::Delete,
            RowKind::Insert | RowKind::Delete => return RowKind::Replace,
            _ => {}
        }
    }
    kind
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
    fn a_single_kind_survives_compression() {
        let d = diff_of("a\nc", "a\nb\nc");
        assert_eq!(strongest_kind(&d, d.hunks[0].rows.clone()), RowKind::Insert);

        let d = diff_of("a\nb\nc", "a\nc");
        assert_eq!(strongest_kind(&d, d.hunks[0].rows.clone()), RowKind::Delete);
    }

    #[test]
    fn a_modification_outranks_everything() {
        let d = diff_of("a\nb\nc", "a\nB\nc");
        assert_eq!(strongest_kind(&d, d.hunks[0].rows.clone()), RowKind::Replace);
    }

    #[test]
    fn an_insert_and_a_delete_in_one_band_read_as_modified() {
        // Two separate hunks compressed onto one pixel must not claim to be
        // purely an insertion.
        let a = "keep\ngone\nsame1\nsame2\nsame3\nkeep2";
        let b = "keep\nsame1\nsame2\nsame3\nadded\nkeep2";
        let d = diff_of(a, b);
        assert!(d.hunks.len() >= 2);
        assert_eq!(strongest_kind(&d, 0..d.rows.len()), RowKind::Replace);
    }

    #[test]
    fn an_unchanged_span_has_no_colour() {
        let d = diff_of("a\nb", "a\nb");
        assert_eq!(strongest_kind(&d, 0..d.rows.len()), RowKind::Equal);
        assert!(Palette::dark().marker(RowKind::Equal).is_none());
    }

    #[test]
    fn out_of_range_spans_are_clamped() {
        let d = diff_of("a", "b");
        assert_eq!(strongest_kind(&d, 500..900), RowKind::Equal);
        // A reversed range must not panic either. Built rather than written
        // literally so the lint does not flag the test itself.
        let (lo, hi) = (5usize, 0usize);
        assert_eq!(strongest_kind(&d, lo..hi), RowKind::Equal);
    }
}
