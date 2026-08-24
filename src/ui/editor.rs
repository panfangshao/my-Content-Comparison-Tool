//! The virtualized two-pane text editor.
//!
//! # What makes this not a `TextEdit`
//!
//! egui's built-in text editor lays out the entire string every frame. At
//! 100 000 lines that is hundreds of milliseconds per frame, so this widget
//! draws its own text instead:
//!
//! * The unit of layout is a **diff row**, not a buffer line. Both panes walk
//!   the same row list, which is what keeps line 40 on the left sitting beside
//!   its counterpart on the right even when one side has extra lines.
//! * Only rows inside the viewport are laid out - typically 40-60 of them,
//!   regardless of document size.
//! * Backgrounds (selection, word-level diff spans, search hits) are painted as
//!   rectangles positioned from the galley, rather than as text sections. That
//!   keeps one galley per row no matter how many overlapping decorations a row
//!   carries.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Align2, Color32, FontId, Galley, Painter, Pos2, Rect, Response, Sense, Ui, Vec2, pos2};

use crate::core::diff::{DiffOptions, DiffResult, InlineDiff, RowKind, Side, inline_diff};
use crate::core::text::{Match, Selection, TextBuffer};
use crate::ui::editing::{self, Motion};
use crate::ui::highlight::StyledRange;
use crate::ui::rowlayout::RowLayout;
use crate::ui::tabs;
use crate::ui::theme::Palette;

/// Space between the line numbers and the text.
const GUTTER_PAD: f32 = 10.0;
/// Left inset of the line-number column.
const GUTTER_INSET: f32 = 8.0;
/// Width of the change bar between the numbers and the text.
const CHANGE_BAR: f32 = 3.0;

#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub font: FontId,
    pub line_height: f32,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub word_wrap: bool,
    pub tab_width: usize,
    /// Width of the digits column, derived from the largest line number.
    pub gutter_width: f32,
}

impl EditorStyle {
    /// Width needed for the line-number column at this document size.
    pub fn gutter_for(&self, max_line: usize) -> f32 {
        if !self.show_line_numbers {
            return CHANGE_BAR + 4.0;
        }
        let digits = max_line.max(99).to_string().len();
        // 0.62em is a good approximation of a monospace digit advance and
        // avoids having to measure text just to size a column.
        GUTTER_INSET + digits as f32 * self.font.size * 0.62 + GUTTER_PAD + CHANGE_BAR
    }
}

/// Word-level diffs for the rows on screen.
///
/// Recomputing these for every row of a large file would cost more than the
/// line diff itself, so they are produced on demand and thrown away whenever
/// the underlying comparison changes.
#[derive(Default)]
pub struct InlineCache {
    generation: u64,
    map: HashMap<usize, InlineDiff>,
}

impl InlineCache {
    /// Drop everything if the comparison has been rebuilt since last time.
    pub fn sync(&mut self, generation: u64) {
        if self.generation != generation {
            self.generation = generation;
            self.map.clear();
        }
    }

    fn get(
        &mut self,
        row: usize,
        left: &str,
        right: &str,
        opts: &DiffOptions,
    ) -> &InlineDiff {
        self.map
            .entry(row)
            .or_insert_with(|| inline_diff(left, right, opts))
    }
}

/// Everything one pane needs for a frame.
pub struct PaneParams<'a> {
    pub side: Side,
    pub buffer: &'a mut TextBuffer,
    pub diff: &'a DiffResult,
    pub diff_generation: u64,
    pub diff_options: &'a DiffOptions,
    /// The *other* document, for computing word-level diffs.
    pub other_lines: &'a [String],
    pub layout: &'a mut RowLayout,
    pub inline: &'a mut InlineCache,
    pub palette: &'a Palette,
    pub style: &'a EditorStyle,
    /// Syntax colours for `visible_rows`, or empty for none.
    pub highlights: &'a [Vec<StyledRange>],
    /// The rows `highlights` describes.
    pub highlight_rows: std::ops::Range<usize>,
    pub search: &'a [Match],
    pub active_match: Option<usize>,
    /// Longest line in characters, used to size the horizontal extent.
    /// Supplied by the caller because measuring it is O(document) and must not
    /// happen every frame.
    pub longest_line: usize,
    /// Set to override the scroll position (scroll sync, jump-to-diff).
    pub force_offset: Option<Vec2>,
    pub focus_requested: bool,
    /// Column the caret is trying to keep while moving vertically. Owned by
    /// the caller so it survives between frames - that is what lets Up/Down
    /// travel through a short line and come back out at the original column.
    pub goal_column: Option<usize>,
}

pub struct PaneOutput {
    pub response: Response,
    /// Where the pane ended up scrolled to, for the other pane to follow.
    pub offset: Vec2,
    pub visible_rows: std::ops::Range<usize>,
    /// The document changed this frame.
    pub edited: bool,
    /// The row the caret is on, if the pane has focus.
    pub caret_row: Option<usize>,
    pub has_focus: bool,
    /// Goal column to hand back next frame.
    pub goal_column: Option<usize>,
}

/// Draw one pane and process its input.
pub fn show_pane(ui: &mut Ui, p: PaneParams<'_>) -> PaneOutput {
    let PaneParams {
        side,
        buffer,
        diff,
        diff_generation,
        diff_options,
        other_lines,
        layout,
        inline,
        palette,
        style,
        highlights,
        highlight_rows,
        search,
        active_match,
        longest_line,
        force_offset,
        focus_requested,
        goal_column,
    } = p;

    inline.sync(diff_generation);
    layout.reconfigure(diff.rows.len(), style.word_wrap);

    let gutter_w = style.gutter_for(buffer.len_lines());
    let line_h = style.line_height;
    let total_h = layout.total_height(line_h);

    // Horizontal extent, estimated from the longest line's character count -
    // the scrollbar is allowed to be approximate, the text is not.
    let est_text_w = (longest_line as f32 + 2.0) * style.font.size * 0.62;

    // The screen column this pane owns.
    //
    // This must be captured *before* the scroll area: inside it,
    // `ui.clip_rect()` is unreliable. A vertical-only scroll area (word wrap
    // on) does not clip horizontally, so it reports the whole window - and a
    // pane that sizes or fills itself from that will paint straight over its
    // neighbour.
    let pane_rect = ui.available_rect_before_wrap();
    let pane_w = pane_rect.width();

    // Wrapped text has nothing to scroll to horizontally.
    let mut area = if style.word_wrap {
        egui::ScrollArea::vertical()
    } else {
        egui::ScrollArea::both()
    }
    .id_salt(("duibi_pane", matches!(side, Side::Left)))
    .auto_shrink([false, false]);
    if let Some(off) = force_offset {
        area = area.scroll_offset(off);
    }

    let mut edited = false;
    let mut caret_row = None;
    let mut visible = 0..0;
    let mut goal = goal_column;

    let out = area.show_viewport(ui, |ui, viewport| {
        let text_w = if style.word_wrap {
            // Leave room for the vertical scrollbar.
            (pane_w - gutter_w - 16.0).max(64.0)
        } else {
            est_text_w.max(pane_w - gutter_w)
        };

        // The interactive area covers the whole pane, not just the text.
        //
        // Sizing it to the content meant a three-line document only accepted
        // clicks in its top 60 pixels - and an empty one only in a single row
        // beside line number 1. Clicking anywhere in an editor should put the
        // caret somewhere, which `char_at_pos` handles by clamping.
        // Each pane wraps at its own width - the split is rarely exactly half -
        // so the layout has to know both, and invalidate only the side that
        // actually changed.
        if style.word_wrap {
            layout.set_wrap_width(side, text_w);
        }

        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(gutter_w + text_w, total_h.max(pane_rect.height())),
            Sense::click_and_drag(),
        );

        if focus_requested || response.clicked() {
            response.request_focus();
        }
        let focused = response.has_focus();

        // Claim the navigation keys.
        //
        // egui's focus system owns Tab and the arrow keys by default and uses
        // them to move between widgets. A built-in `TextEdit` opts out of that;
        // a hand-drawn editor has to do it explicitly, or pressing Right walks
        // the focus onto the merge arrows instead of moving the caret.
        //
        // Escape is deliberately left to egui, so it still closes the find bar
        // and releases focus.
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }

        let rows = layout.visible_rows(viewport.top()..viewport.bottom(), line_h);
        visible = rows.clone();

        let painter = ui.painter().clone();

        // The part of this pane that is on screen: its own column
        // horizontally, the scroll viewport vertically. Everything this
        // function fills is bounded by it.
        let clip = Rect::from_x_y_ranges(pane_rect.x_range(), ui.clip_rect().y_range());

        // Text must not slide under the sticky line-number column.
        let text_clip =
            Rect::from_min_max(pos2(clip.left() + gutter_w, clip.top()), clip.max);
        let text_painter = painter.with_clip_rect(text_clip);

        painter.rect_filled(clip, 0, palette.editor_bg);

        let ctx = RowCtx {
            side,
            rect,
            gutter_w,
            text_w,
            line_h,
            palette,
            style,
        };

        let caret_line = buffer.line_of_char(buffer.selection().head);

        // ---- Rows -------------------------------------------------------
        for row_idx in rows.clone() {
            let Some(row) = diff.rows.get(row_idx) else {
                break;
            };
            let y = rect.top() + layout.row_top(row_idx, line_h);
            let h = layout.row_height(row_idx, line_h);
            let Some(line_idx) = row.side(side) else {
                // No line on this side: this is alignment filler opposite an
                // insertion or a deletion.
                //
                // It gets a hatch pattern, not just a tint. Filler that merely
                // looks slightly darker is indistinguishable from a genuine
                // blank line, which makes "remove empty lines" appear to do
                // nothing - the blanks are gone, but the other side still has
                // them, so filler takes their place.
                let filler =
                    Rect::from_min_max(pos2(clip.left(), y), pos2(clip.right(), y + h));
                painter.rect_filled(filler, 0, palette.filler_bg);
                paint_hatch(&painter, filler, palette.border);
                continue;
            };

            // Row wash.
            if let Some(bg) = palette.row_bg(row.kind) {
                painter.rect_filled(
                    Rect::from_min_max(pos2(clip.left(), y), pos2(clip.right(), y + h)),
                    0,
                    bg,
                );
            } else if focused && line_idx == caret_line && buffer.selection().is_empty() {
                painter.rect_filled(
                    Rect::from_min_max(pos2(clip.left(), y), pos2(clip.right(), y + h)),
                    0,
                    palette.current_line,
                );
            }

            let line = buffer.line(line_idx);
            let display = if tabs::has_tabs(line) {
                std::borrow::Cow::Owned(tabs::expand(line, style.tab_width))
            } else {
                std::borrow::Cow::Borrowed(line)
            };

            // ---- Lay the line out --------------------------------------
            let spans = highlights
                .get(row_idx.wrapping_sub(highlight_rows.start))
                .filter(|_| highlight_rows.contains(&row_idx))
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            let galley = build_galley(
                &text_painter,
                line,
                &display,
                spans,
                palette.text,
                style,
                if style.word_wrap { text_w } else { f32::INFINITY },
            );

            // Feed this side's true height back for the *next* frame.
            if style.word_wrap {
                layout.measure(side, row_idx, galley.rows.len() as u32);
            }

            let origin = pos2(rect.left() + gutter_w, y);

            // ---- Decorations under the text ----------------------------
            paint_inline_spans(
                &text_painter, &ctx, &galley, origin, row_idx, row.kind, line, diff, side,
                other_lines, inline, diff_options, buffer,
            );
            paint_search(
                &text_painter,
                &ctx,
                &galley,
                origin,
                line,
                line_idx,
                search,
                active_match,
            );
            paint_selection(&text_painter, &ctx, &galley, origin, line, line_idx, buffer);

            // ---- The text itself ---------------------------------------
            text_painter.galley(origin, galley.clone(), palette.text);

            if style.show_whitespace {
                paint_whitespace(&text_painter, &ctx, &galley, origin, line);
            }

            // ---- Caret --------------------------------------------------
            if focused && line_idx == caret_line {
                caret_row = Some(row_idx);
                paint_caret(ui, &text_painter, &ctx, &galley, origin, line, buffer);
            }

        }

        // Sticky gutter background, drawn under the numbers written above.
        // (Painted last so it wins over any text scrolled beneath it, using a
        // layer-free trick: an opaque strip plus a re-draw of the numbers.)
        paint_gutter_backdrop(&painter, &ctx, clip, layout, diff, &rows, focused, caret_line);

        // ---- Input ------------------------------------------------------
        let caret_before = buffer.selection().head;
        if focused {
            edited |= handle_keyboard(ui, buffer, style, &mut goal);
        }
        handle_mouse(ui, &response, buffer, diff, layout, &ctx, &mut goal);

        // ---- Keep the caret on screen -----------------------------------
        //
        // Run after input, so it follows the caret's *new* position. The x is
        // estimated from the monospace advance rather than measured: the exact
        // galley for the target row does not exist when the caret has just
        // moved off screen, and an estimate is plenty to scroll by - the next
        // frame lays the row out properly anyway.
        if buffer.selection().head != caret_before {
            let (line, col) = buffer.line_col(buffer.selection().head);
            if let Some(row) = diff.row_of(side, line) {
                let text = buffer.line(line);
                let col = tabs::to_display(text, style.tab_width, col);
                let wrap = if style.word_wrap {
                    text_w
                } else {
                    f32::INFINITY
                };
                let measured = measure_line(&text_painter, text, style, wrap, palette.text);
                let at = measured.pos_from_cursor(CCursor::new(col));

                let y = rect.top() + layout.row_top(row, line_h);
                let caret = Rect::from_min_max(
                    pos2(rect.left() + gutter_w + at.min.x, y + at.min.y),
                    pos2(rect.left() + gutter_w + at.min.x + 2.0, y + at.max.y),
                );
                // Reveal a little context around it rather than parking the
                // caret flush against the edge of the viewport.
                let margin = Vec2::new(style.font.size * 4.0, line_h);
                ui.scroll_to_rect(caret.expand2(margin), None);
            }
        }

        response
    });

    PaneOutput {
        response: out.inner,
        offset: out.state.offset,
        visible_rows: visible,
        edited,
        caret_row,
        has_focus: false,
        goal_column: goal,
    }
}

/// Geometry shared by the row painters.
struct RowCtx<'a> {
    side: Side,
    rect: Rect,
    gutter_w: f32,
    /// Width available to the text, i.e. the wrap width when wrapping.
    text_w: f32,
    line_h: f32,
    palette: &'a Palette,
    style: &'a EditorStyle,
}

/// Build the galley for one line, applying syntax colours.
fn build_galley(
    painter: &Painter,
    original: &str,
    display: &str,
    spans: &[StyledRange],
    base: Color32,
    style: &EditorStyle,
    wrap_width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.break_on_newline = false;

    let fmt = |color: Color32, italic: bool, bold: bool| TextFormat {
        font_id: FontId {
            size: style.font.size,
            family: style.font.family.clone(),
        },
        color,
        italics: italic,
        line_height: Some(style.line_height),
        valign: egui::Align::TOP,
        ..Default::default()
    }
    .tap_bold(bold);

    if spans.is_empty() || tabs::has_tabs(original) {
        // Tab expansion shifts every byte offset, so syntax spans computed
        // against the original line no longer line up. Rather than maintain a
        // second mapping for a rare case, indented-with-tabs lines fall back to
        // a single colour.
        job.append(display, 0.0, fmt(base, false, false));
        return painter.layout_job(job);
    }

    let mut at = 0usize;
    for s in spans {
        let start = s.range.start.min(display.len());
        let end = s.range.end.min(display.len());
        if start > at {
            job.append(&display[at..start], 0.0, fmt(base, false, false));
        }
        if end > start {
            job.append(&display[start..end], 0.0, fmt(s.color, s.italic, s.bold));
        }
        at = at.max(end);
    }
    if at < display.len() {
        job.append(&display[at..], 0.0, fmt(base, false, false));
    }
    if job.text.is_empty() {
        job.append("", 0.0, fmt(base, false, false));
    }
    painter.layout_job(job)
}

/// Small helper so the format builder above stays readable.
trait TapBold {
    fn tap_bold(self, bold: bool) -> Self;
}

impl TapBold for TextFormat {
    fn tap_bold(mut self, bold: bool) -> Self {
        if bold {
            // epaint has no synthetic bold; a heavier colour is the closest
            // honest approximation without shipping a second font.
            self.color = self.color.gamma_multiply(1.25);
        }
        self
    }
}

/// Diagonal hatching, used to mark rows that hold no text of their own.
///
/// Lines are placed on the absolute diagonals `x + y = k * STEP` rather than
/// relative to the row, so the pattern runs continuously across a stack of
/// filler rows instead of restarting on each one.
fn paint_hatch(painter: &Painter, rect: Rect, color: Color32) {
    const STEP: f32 = 8.0;
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let stroke = egui::Stroke::new(1.0, color);

    let first = ((rect.left() + rect.top()) / STEP).floor() * STEP;
    let last = rect.right() + rect.bottom();
    let mut k = first;
    while k <= last {
        painter.line_segment(
            [pos2(k - rect.top(), rect.top()), pos2(k - rect.bottom(), rect.bottom())],
            stroke,
        );
        k += STEP;
    }
}

/// Paint a character range as one or more rectangles, following wrapped rows.
fn paint_char_range(
    painter: &Painter,
    galley: &Galley,
    origin: Pos2,
    from: usize,
    to: usize,
    color: Color32,
) {
    if from >= to {
        return;
    }
    let a = galley.layout_from_cursor(CCursor::new(from));
    let b = galley.layout_from_cursor(CCursor::new(to));

    if a.row == b.row {
        let pa = galley.pos_from_layout_cursor(&a);
        let pb = galley.pos_from_layout_cursor(&b);
        painter.rect_filled(
            Rect::from_min_max(
                origin + pa.min.to_vec2(),
                origin + pos2(pb.min.x, pa.max.y).to_vec2(),
            ),
            2,
            color,
        );
        return;
    }

    for row in a.row..=b.row {
        let Some(placed) = galley.rows.get(row) else {
            break;
        };
        let r = placed.rect();
        let left = if row == a.row {
            galley.pos_from_layout_cursor(&a).min.x
        } else {
            r.left()
        };
        let right = if row == b.row {
            galley.pos_from_layout_cursor(&b).min.x
        } else {
            r.right()
        };
        if right > left {
            painter.rect_filled(
                Rect::from_min_max(
                    origin + pos2(left, r.top()).to_vec2(),
                    origin + pos2(right, r.bottom()).to_vec2(),
                ),
                2,
                color,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_inline_spans(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    row_idx: usize,
    kind: RowKind,
    line: &str,
    diff: &DiffResult,
    side: Side,
    other_lines: &[String],
    inline: &mut InlineCache,
    opts: &DiffOptions,
    _buffer: &TextBuffer,
) {
    if kind != RowKind::Replace {
        return;
    }
    let Some(row) = diff.rows.get(row_idx) else {
        return;
    };
    let (Some(l), Some(r)) = (row.left(), row.right()) else {
        return;
    };

    // The cache is keyed on the row, so both panes share one computation.
    let (left_text, right_text) = match side {
        Side::Left => (line, other_lines.get(r).map_or("", String::as_str)),
        Side::Right => (other_lines.get(l).map_or("", String::as_str), line),
    };
    let d = inline.get(row_idx, left_text, right_text, opts);
    let spans = match side {
        Side::Left => &d.left,
        Side::Right => &d.right,
    };

    let color = ctx.palette.span_bg(side);
    for s in spans {
        let from = tabs::to_display(line, ctx.style.tab_width, tabs::byte_to_char(line, s.start));
        let to = tabs::to_display(line, ctx.style.tab_width, tabs::byte_to_char(line, s.end));
        paint_char_range(painter, galley, origin, from, to, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_search(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    line: &str,
    line_idx: usize,
    search: &[Match],
    active: Option<usize>,
) {
    if search.is_empty() {
        return;
    }
    // Matches are in reading order, so the ones for this line are contiguous.
    let start = search.partition_point(|m| m.line < line_idx);
    for (i, m) in search[start..].iter().enumerate() {
        if m.line != line_idx {
            break;
        }
        let color = if active == Some(start + i) {
            ctx.palette.search_active_bg
        } else {
            ctx.palette.search_bg
        };
        let from = tabs::to_display(line, ctx.style.tab_width, tabs::byte_to_char(line, m.start));
        let to = tabs::to_display(line, ctx.style.tab_width, tabs::byte_to_char(line, m.end));
        paint_char_range(painter, galley, origin, from, to, color);
    }
}

fn paint_selection(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    line: &str,
    line_idx: usize,
    buffer: &TextBuffer,
) {
    let sel = buffer.selection();
    if sel.is_empty() {
        return;
    }
    let (l0, c0) = buffer.line_col(sel.start());
    let (l1, c1) = buffer.line_col(sel.end());
    if line_idx < l0 || line_idx > l1 {
        return;
    }

    let n = line.chars().count();
    let from = if line_idx == l0 { c0 } else { 0 };
    let to = if line_idx == l1 { c1 } else { n };

    let from = tabs::to_display(line, ctx.style.tab_width, from);
    let mut to = tabs::to_display(line, ctx.style.tab_width, to);
    // A selection spanning into the next line should show the newline as a
    // sliver of highlight, so the shape of a multi-line selection reads right.
    if line_idx < l1 {
        to = to.max(from + 1);
    }
    paint_char_range(painter, galley, origin, from, to, ctx.palette.selection);
}

fn paint_caret(
    ui: &Ui,
    painter: &Painter,
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    line: &str,
    buffer: &TextBuffer,
) {
    let (_, col) = buffer.line_col(buffer.selection().head);
    let col = tabs::to_display(line, ctx.style.tab_width, col);
    let p = galley.pos_from_cursor(CCursor::new(col));

    // Blink on a 1.06 s cycle, and keep the frame clock running while focused.
    let t = ui.input(|i| i.time);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(120));
    if (t * 1.9).fract() > 0.5 {
        return;
    }

    let x = (origin.x + p.min.x).round();
    painter.rect_filled(
        Rect::from_min_max(
            pos2(x, origin.y + p.min.y),
            pos2(x + 2.0, origin.y + p.max.y),
        ),
        0,
        ctx.palette.caret,
    );
}

fn paint_whitespace(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    line: &str,
) {
    // Cap the work: past a few hundred columns the marks are off screen anyway.
    const MAX: usize = 400;
    let color = ctx.palette.whitespace;

    for (i, c) in line.chars().take(MAX).enumerate() {
        if c != ' ' && c != '\t' {
            continue;
        }
        let from = tabs::to_display(line, ctx.style.tab_width, i);
        let p = galley.pos_from_cursor(CCursor::new(from));
        let y = origin.y + (p.min.y + p.max.y) / 2.0;

        if c == ' ' {
            painter.circle_filled(pos2(origin.x + p.min.x + 2.0, y), 1.0, color);
        } else {
            let to = tabs::to_display(line, ctx.style.tab_width, i + 1);
            let q = galley.pos_from_cursor(CCursor::new(to));
            painter.line_segment(
                [
                    pos2(origin.x + p.min.x + 2.0, y),
                    pos2(origin.x + q.min.x - 2.0, y),
                ],
                egui::Stroke::new(1.0, color),
            );
        }
    }
}

/// The line-number column, pinned to the left edge of the viewport.
#[allow(clippy::too_many_arguments)]
fn paint_gutter_backdrop(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    clip: Rect,
    layout: &RowLayout,
    diff: &DiffResult,
    rows: &std::ops::Range<usize>,
    focused: bool,
    caret_line: usize,
) {
    let pal = ctx.palette;
    let gutter = Rect::from_min_max(
        clip.min,
        pos2(clip.left() + ctx.gutter_w, clip.bottom()),
    );
    painter.rect_filled(gutter, 0, pal.gutter_bg);
    painter.vline(
        gutter.right() - 0.5,
        gutter.y_range(),
        egui::Stroke::new(1.0, pal.border),
    );

    let num_right = gutter.right() - GUTTER_PAD - CHANGE_BAR;

    for row_idx in rows.clone() {
        let Some(row) = diff.rows.get(row_idx) else {
            break;
        };
        let y = ctx.rect.top() + layout.row_top(row_idx, ctx.line_h);
        let h = layout.row_height(row_idx, ctx.line_h);
        if y + h < clip.top() || y > clip.bottom() {
            continue;
        }

        // Change bar: a solid stripe in the row's colour, which is what makes
        // the diff readable when scrolled far to the right.
        if let Some(c) = pal.marker(row.kind) {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(gutter.right() - CHANGE_BAR - 1.0, y),
                    pos2(gutter.right() - 1.0, y + h),
                ),
                0,
                c,
            );
        }

        let Some(line_idx) = row.side(ctx.side) else {
            continue;
        };
        if !ctx.style.show_line_numbers {
            continue;
        }
        let is_caret = focused && line_idx == caret_line;
        painter.text(
            pos2(num_right, y + ctx.line_h / 2.0),
            Align2::RIGHT_CENTER,
            line_idx + 1,
            FontId {
                size: ctx.style.font.size * 0.92,
                family: ctx.style.font.family.clone(),
            },
            if is_caret { pal.text_dim } else { pal.text_faint },
        );
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Translate a screen position to a character offset in the buffer.
fn char_at_pos(
    pos: Pos2,
    painter: &Painter,
    buffer: &TextBuffer,
    diff: &DiffResult,
    layout: &RowLayout,
    ctx: &RowCtx<'_>,
) -> Option<usize> {
    let (side, rect, style) = (ctx.side, ctx.rect, ctx.style);
    let row_idx = layout.row_at(pos.y - rect.top(), ctx.line_h);
    let row = diff.rows.get(row_idx)?;

    // Clicking on filler puts the caret on the nearest real line, so a click
    // anywhere in the pane does something sensible.
    let line_idx = match row.side(side) {
        Some(l) => l,
        None => nearest_line(diff, row_idx, side)?,
    };

    // Ask the same layout the renderer produced where that point falls.
    //
    // This used to divide by an assumed character advance. The assumption was
    // close for one glyph and wrong by several columns by the middle of a
    // line - and hopelessly wrong for CJK, which is double width - so the
    // caret landed somewhere other than where the user clicked.
    let line = buffer.line(line_idx);
    let wrap = if style.word_wrap {
        ctx.text_w
    } else {
        f32::INFINITY
    };
    let galley = measure_line(painter, line, style, wrap, ctx.palette.text);

    let origin = pos2(
        rect.left() + ctx.gutter_w,
        rect.top() + layout.row_top(row_idx, ctx.line_h),
    );
    let cursor = galley.cursor_from_pos(pos - origin);
    let col = tabs::from_display(line, style.tab_width, cursor.index.0);
    Some(buffer.char_at(line_idx, col))
}

/// Nearest row above (then below) that has a line on `side`.
fn nearest_line(diff: &DiffResult, row_idx: usize, side: Side) -> Option<usize> {
    diff.rows[..=row_idx.min(diff.rows.len().saturating_sub(1))]
        .iter()
        .rev()
        .find_map(|r| r.side(side))
        .or_else(|| diff.rows.iter().find_map(|r| r.side(side)))
}

/// Lay a line out exactly as the renderer does, for hit-testing and caret
/// geometry.
///
/// Colour and italics do not affect glyph advances, so the syntax spans can be
/// left out; the font, the tab expansion and the wrap width are what matter.
fn measure_line(
    painter: &Painter,
    line: &str,
    style: &EditorStyle,
    wrap_width: f32,
    color: Color32,
) -> Arc<Galley> {
    let display = tabs::expand(line, style.tab_width);
    build_galley(painter, line, &display, &[], color, style, wrap_width)
}

fn handle_mouse(
    ui: &Ui,
    response: &Response,
    buffer: &mut TextBuffer,
    diff: &DiffResult,
    layout: &RowLayout,
    ctx: &RowCtx<'_>,
    goal: &mut Option<usize>,
) {
    let Some(pos) = response.interact_pointer_pos() else {
        return;
    };
    let Some(ci) = char_at_pos(pos, ui.painter(), buffer, diff, layout, ctx) else {
        return;
    };
    // Any pointer interaction re-aims the caret, so a remembered column from
    // an earlier Up/Down run no longer applies.
    *goal = None;

    if response.triple_clicked() {
        let r = editing::line_range_at(buffer, ci);
        buffer.set_selection(Selection {
            anchor: r.start,
            head: r.end,
        });
    } else if response.double_clicked() {
        let r = editing::word_range_at(buffer, ci);
        buffer.set_selection(Selection {
            anchor: r.start,
            head: r.end,
        });
    } else if response.drag_started() || response.clicked() {
        let extend = ui.input(|i| i.modifiers.shift);
        buffer.set_selection(Selection {
            anchor: if extend { buffer.selection().anchor } else { ci },
            head: ci,
        });
    } else if response.dragged() {
        buffer.set_selection(Selection {
            anchor: buffer.selection().anchor,
            head: ci,
        });
    }
}

/// Consume keyboard input. Returns whether the document changed.
///
/// `goal` is the caret's remembered column. It belongs to the caller, not to
/// this function: resetting it every frame is what made Up/Down forget the
/// column as soon as it passed through a shorter line.
fn handle_keyboard(
    ui: &Ui,
    buffer: &mut TextBuffer,
    style: &EditorStyle,
    goal: &mut Option<usize>,
) -> bool {
    use egui::{Event, Key};

    let before = buffer.version();
    let page = ((ui.clip_rect().height() / style.line_height) as usize).max(1) - 1;

    let events = ui.input(|i| i.events.clone());

    for event in events {
        match event {
            Event::Text(text) if !text.is_empty() => {
                // egui sends control characters through `Key` events, so any
                // text that arrives here is real content.
                buffer.insert(&text);
                *goal = None;
            }
            Event::Paste(text) => {
                buffer.insert(&text);
                *goal = None;
            }
            Event::Copy => ui.ctx().copy_text(buffer.selected_text()),
            Event::Cut => {
                ui.ctx().copy_text(buffer.selected_text());
                if !buffer.selection().is_empty() {
                    buffer.replace_range(buffer.selection().range(), "");
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let shift = modifiers.shift;
                let ctrl = modifiers.command || modifiers.ctrl;

                let motion = match (key, ctrl) {
                    (Key::ArrowLeft, false) => Some(Motion::Left),
                    (Key::ArrowRight, false) => Some(Motion::Right),
                    (Key::ArrowLeft, true) => Some(Motion::WordLeft),
                    (Key::ArrowRight, true) => Some(Motion::WordRight),
                    (Key::ArrowUp, false) => Some(Motion::Up),
                    (Key::ArrowDown, false) => Some(Motion::Down),
                    (Key::Home, false) => Some(Motion::LineStart),
                    (Key::End, false) => Some(Motion::LineEnd),
                    (Key::Home, true) => Some(Motion::DocStart),
                    (Key::End, true) => Some(Motion::DocEnd),
                    (Key::PageUp, _) => Some(Motion::PageUp(page)),
                    (Key::PageDown, _) => Some(Motion::PageDown(page)),
                    _ => None,
                };

                if let Some(m) = motion {
                    let moved = editing::move_caret(buffer, m, shift, *goal);
                    // `move_caret` returns `None` for horizontal motions, which
                    // is how the remembered column gets cleared.
                    *goal = moved.goal_column;
                    buffer.set_selection(moved.selection);
                    continue;
                }

                *goal = None;
                match (key, ctrl, shift) {
                    (Key::Backspace, false, _) => buffer.backspace(),
                    (Key::Delete, false, _) => buffer.delete_forward(),
                    (Key::Enter, false, _) => editing::insert_newline(buffer),
                    (Key::Tab, false, false) => {
                        if buffer.selection().is_empty() {
                            buffer.insert(&" ".repeat(style.tab_width));
                        } else {
                            editing::indent_selection(buffer, &" ".repeat(style.tab_width));
                        }
                    }
                    (Key::Tab, false, true) => {
                        editing::outdent_selection(buffer, &" ".repeat(style.tab_width))
                    }
                    (Key::A, true, false) => buffer.select_all(),
                    (Key::Z, true, false) => {
                        buffer.undo();
                    }
                    (Key::Z, true, true) | (Key::Y, true, false) => {
                        buffer.redo();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    buffer.version() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{Budget, diff_lines};

    fn style() -> EditorStyle {
        EditorStyle {
            font: FontId::monospace(14.0),
            line_height: 20.0,
            show_line_numbers: true,
            show_whitespace: false,
            word_wrap: false,
            tab_width: 4,
            gutter_width: 60.0,
        }
    }

    #[test]
    fn gutter_grows_with_the_line_count() {
        let s = style();
        let small = s.gutter_for(10);
        let large = s.gutter_for(100_000);
        assert!(large > small, "a six-digit file needs a wider column");
        assert!(small > 20.0);
    }

    #[test]
    fn hiding_line_numbers_collapses_the_gutter() {
        let mut s = style();
        s.show_line_numbers = false;
        assert!(s.gutter_for(100_000) < 12.0);
    }

    #[test]
    fn the_inline_cache_clears_when_the_comparison_changes() {
        let opts = DiffOptions::default();
        let mut c = InlineCache::default();
        c.sync(1);
        let d = c.get(0, "the quick brown fox", "the quick red fox", &opts).clone();
        assert!(!d.left.is_empty(), "expected a word-level span: {d:?}");
        assert_eq!(c.map.len(), 1);

        c.sync(1);
        assert_eq!(c.map.len(), 1, "same generation keeps the cache");
        c.sync(2);
        assert!(c.map.is_empty(), "a new comparison must invalidate it");
    }

    #[test]
    fn the_inline_cache_computes_each_row_once() {
        let opts = DiffOptions::default();
        let mut c = InlineCache::default();
        c.sync(1);
        let first = c.get(3, "hello world", "hello there", &opts).clone();
        // Asking again with different text must return the cached answer -
        // that is what proves it is not recomputing on every frame.
        let again = c.get(3, "totally different", "text entirely", &opts).clone();
        assert_eq!(first, again);
    }

    #[test]
    fn nearest_line_finds_a_real_line_from_filler() {
        let left: Vec<String> = vec!["a".into(), "b".into()];
        let right: Vec<String> = vec!["a".into(), "x".into(), "y".into(), "b".into()];
        let d = diff_lines(&left, &right, &DiffOptions::default(), Budget::unlimited());

        // Find a row that has no left line (filler opposite the insertion).
        let filler = d
            .rows
            .iter()
            .position(|r| r.left().is_none())
            .expect("the insertion must produce filler on the left");
        let l = nearest_line(&d, filler, Side::Left).expect("a nearby left line");
        assert!(l < left.len());
    }

    #[test]
    fn nearest_line_handles_a_document_that_is_empty_on_one_side() {
        let left: Vec<String> = vec![];
        let right: Vec<String> = vec!["only".into()];
        let d = diff_lines(&left, &right, &DiffOptions::default(), Budget::unlimited());
        assert_eq!(nearest_line(&d, 0, Side::Left), None);
        assert_eq!(nearest_line(&d, 0, Side::Right), Some(0));
    }

    /// Hit-testing must go through a real layout.
    ///
    /// The previous implementation divided x by an assumed advance of
    /// `font_size * 0.62`. This shows how far that drifts: by the middle of an
    /// ordinary line the guess is already off by more than a whole character,
    /// which is exactly what made clicks land in the wrong place.
    #[test]
    fn the_old_advance_estimate_really_was_wrong() {
        let s = style();
        let ctx = egui::Context::default();
        let line = "fn compute(alpha: u32, beta: u32) -> u32 { alpha + beta }";

        let mut error_at_column_40 = 0.0_f32;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            let galley = measure_line(ui.painter(), line, &s, f32::INFINITY, Color32::WHITE);
            let real = galley.pos_from_cursor(CCursor::new(40)).min.x;
            let guessed = 40.0 * s.font.size * 0.62;
            error_at_column_40 = (real - guessed).abs();
        });
        out.textures_delta.clear();

        assert!(
            error_at_column_40 > s.font.size * 0.62,
            "the estimate was off by {error_at_column_40}pt at column 40, \
             less than one character - this test no longer proves anything"
        );
    }

    /// Every column must hit-test back to itself when probed at the point it
    /// is actually drawn.
    #[test]
    fn every_column_hit_tests_back_to_itself() {
        let s = style();
        let ctx = egui::Context::default();

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            for line in [
                "fn compute(alpha: u32) -> u32 { alpha }",
                "\tindented\twith\ttabs",
                "对比工具 mixed 宽度 text",
            ] {
                let galley = measure_line(ui.painter(), line, &s, f32::INFINITY, Color32::WHITE);
                let display_cols = tabs::expand(line, s.tab_width).chars().count();

                for col in 0..display_cols {
                    let here = galley.pos_from_cursor(CCursor::new(col)).min.x;
                    let next = galley.pos_from_cursor(CCursor::new(col + 1)).min.x;
                    let width = next - here;

                    // A caret sits *between* characters, so the left half of a
                    // glyph belongs to the position before it and the right
                    // half to the one after. Both halves must land where the
                    // pixels say, which is what the old advance estimate got
                    // progressively wrong along the line.
                    let left_half = galley
                        .cursor_from_pos(egui::vec2(here + width * 0.25, 1.0))
                        .index
                        .0;
                    assert_eq!(
                        left_half, col,
                        "the left half of column {col} of {line:?} gave {left_half}"
                    );

                    let right_half = galley
                        .cursor_from_pos(egui::vec2(here + width * 0.75, 1.0))
                        .index
                        .0;
                    assert_eq!(
                        right_half,
                        col + 1,
                        "the right half of column {col} of {line:?} gave {right_half}"
                    );
                }
            }
        });
        out.textures_delta.clear();
    }
}
