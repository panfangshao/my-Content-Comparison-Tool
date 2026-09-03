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
use crate::ui::editing::{self, Motion, VerticalMotion};
use crate::ui::highlight::StyledRange;
use crate::ui::ime::{self, Composition};
use crate::ui::rowlayout::RowLayout;
use crate::ui::tabs;
use crate::ui::theme::Palette;

/// Space between the line numbers and the text.
const GUTTER_PAD: f32 = 10.0;
/// Left inset of the line-number column.
const GUTTER_INSET: f32 = 8.0;
/// Width of the change bar between the numbers and the text.
const CHANGE_BAR: f32 = 3.0;
/// How long the caret stays solid after input before the blink resumes.
const CARET_HOLD: f64 = 0.5;

/// Characters of one line that are actually laid out.
///
/// Text layout is linear in the length of the line, and a SQL dump puts the
/// whole of a table in a single `INSERT`. Measured on a real 132 MB dump: the
/// longest line is 1 045 539 characters, 52 lines are over a million, and one
/// 749 000-character line takes **323 ms** to lay out at a 600 pt wrap width -
/// where it becomes 11 269 visual lines. Two panes, and the frame a line like
/// that scrolls into view costs two thirds of a second. Scrolling through a
/// run of them is what made the window stop responding.
///
/// So a line is laid out up to this many characters and the rest is replaced
/// by a marker. The document is untouched: the diff, the similarity, search,
/// merge and saving all read the buffer, not the galley. Only what is drawn is
/// capped. VS Code caps rendering at 10 000 characters for the same reason.
///
/// Beyond this width a line is unreadable anyway - at 14 pt it is already 145
/// wrapped rows.
pub const MAX_RENDERED_CHARS: usize = 10_000;

/// Appended to a line that was too long to lay out in full.
const TRUNCATION_MARK: &str = " …";

/// No cap: the line is laid out however long it is.
///
/// Only ever used for a line the reader has explicitly opened, because that is
/// the one case where several hundred milliseconds is a fair price - it was
/// asked for, and it is paid once, for one row.
const NO_CAP: usize = usize::MAX;

#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub font: FontId,
    pub line_height: f32,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub word_wrap: bool,
    pub tab_width: usize,
    /// Where Up and Down land horizontally.
    pub vertical_motion: VerticalMotion,
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
    /// Only for the one label this widget can draw: the note that a row's
    /// change fell past the rendered part of an over-long line.
    pub lang: crate::i18n::Lang,
    /// Line count the digits column is sized for.
    ///
    /// The larger of the two documents, supplied by the caller rather than
    /// taken from this pane's own buffer. A pane that sized its own column
    /// from its own line count gave 999 lines a three-digit column and 1000
    /// lines a four-digit one - and since the text gets whatever width is
    /// left over, the two panes then wrapped at widths one character apart,
    /// which is enough to fold a long line differently on each side. One
    /// number for both panes removes that at the source; it also stops the
    /// two columns of text starting at visibly different offsets.
    pub gutter_lines: usize,
    /// Syntax colours for `visible_rows`, or empty for none.
    pub highlights: &'a [Vec<StyledRange>],
    /// The rows `highlights` describes.
    pub highlight_rows: std::ops::Range<usize>,
    pub search: &'a [Match],
    pub active_match: Option<usize>,
    /// Width in points of the widest line, for the horizontal extent.
    ///
    /// Supplied by the caller because finding which line is widest is
    /// O(document) and must not happen every frame. Zero when wrapping, where
    /// there is no horizontal extent to size.
    pub longest_line: f32,
    /// Set to override the scroll position (scroll sync, jump-to-diff).
    pub force_offset: Option<Vec2>,
    pub focus_requested: bool,
    /// Column the caret is trying to keep while moving vertically. Owned by
    /// the caller so it survives between frames - that is what lets Up/Down
    /// travel through a short line and come back out at the original column.
    pub goal_column: Option<usize>,
    /// Text an input method is still composing. Owned by the caller for the
    /// same reason as `goal_column`: composing `nihao` into `你好` spans many
    /// frames. See [`crate::ui::ime`].
    pub composing: &'a mut Composition,
    /// Lines the reader has opened in full, by line index on this side.
    ///
    /// Over-long lines are cut at [`MAX_RENDERED_CHARS`] so that scrolling
    /// past a megabyte-long `INSERT` does not cost a third of a second a
    /// frame. That keeps the tool usable and makes the rest of such a line
    /// unreadable, which for a comparison tool is its own kind of broken - so
    /// any one line can be opened in full on demand, and only that line pays.
    pub expanded: &'a std::collections::HashSet<usize>,
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
    /// A line whose "show whole line" control was clicked this frame.
    pub toggle_expand: Option<usize>,
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
        lang,
        gutter_lines,
        highlights,
        highlight_rows,
        search,
        active_match,
        longest_line,
        force_offset,
        focus_requested,
        goal_column,
        composing,
        expanded,
    } = p;

    inline.sync(diff_generation);
    layout.reconfigure(diff.rows.len(), style.word_wrap);

    let gutter_w = style.gutter_for(gutter_lines);
    let line_h = style.line_height;
    let total_h = layout.total_height(line_h);

    // Horizontal extent: the longest line's width, plus two characters of
    // slack so the caret has somewhere to sit past the last glyph.
    let est_text_w = longest_line + 2.0 * ascii_advance(ui.painter(), style);

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
    let mut toggle_expand = None;
    let mut long_rows: Vec<LongRow> = Vec::new();
    let mut caret_row = None;
    let mut visible = 0..0;
    let mut goal = goal_column;
    let mut has_focus = false;

    let out = area.show_viewport(ui, |ui, viewport| {
        let text_w = if style.word_wrap {
            // Leave room for the vertical scrollbar.
            (pane_w - gutter_w - 16.0).max(64.0)
        } else {
            est_text_w.max(pane_w - gutter_w)
        };

        // The width this pane wraps text at. A pane-width difference too small
        // to see still flips where a long line folds - and since a row is as
        // tall as its side with more visual lines, the wider pane then shows a
        // dead band at the bottom of a row whose text is identical on both
        // sides. Within a character of each other, both panes wrap at the
        // narrower width so identical lines fold identically.
        let wrap_w = if style.word_wrap {
            layout.aligned_wrap_width(side, text_w, style.font.size * 0.62)
        } else {
            text_w
        };

        // The interactive area covers the whole pane, not just the text.
        //
        // Sizing it to the content meant a three-line document only accepted
        // clicks in its top 60 pixels - and an empty one only in a single row
        // beside line number 1. Clicking anywhere in an editor should put the
        // caret somewhere, which `char_at_pos` handles by clamping.
        // The layout records the width the galley wraps at - possibly snapped
        // to the other pane's - and invalidates only the side that changed.
        if style.word_wrap {
            layout.set_wrap_width(side, wrap_w);
        }

        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(gutter_w + text_w, total_h.max(pane_rect.height())),
            Sense::click_and_drag(),
        );

        if focus_requested || response.clicked() {
            response.request_focus();
        }
        let focused = response.has_focus();
        has_focus = focused;

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
            text_w: wrap_w,
            line_h,
            palette,
            style,
            expanded,
        };

        let caret_line = buffer.line_of_char(buffer.selection().head);
        // Where the input method should put its candidate window. Captured
        // while the caret's row is laid out, because that is the only place the
        // galley for it exists.
        let mut ime_anchor: Option<Rect> = None;

        // Cross-frame caret state, keyed per pane: where the caret was last
        // seen (the IME anchor fallback) and until when the blink stays solid
        // after input.
        let caret_track_id = egui::Id::new(("duibi_caret_rect", matches!(side, Side::Left)));
        let caret_hold_id = egui::Id::new(("duibi_caret_hold", matches!(side, Side::Left)));
        let caret_hold_until: Option<f64> = ui.ctx().data(|d| d.get_temp(caret_hold_id));

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

            let cap = ctx.cap_for(line_idx);
            let galley = build_galley(
                &text_painter,
                line,
                &display,
                spans,
                palette.text,
                style,
                if style.word_wrap { wrap_w } else { f32::INFINITY },
                cap,
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

            // Over-long rows get a control in the line-number column, which
            // is drawn after every row so it sits on top. Registered here
            // because this is where the line has already been measured.
            let cut = truncate_for_display(&display, cap).1;
            if cut || expanded.contains(&line_idx) {
                long_rows.push(LongRow { line_idx, y, h, cut });
            }

            // ---- Caret and any composition sitting on it ----------------
            if focused && line_idx == caret_line {
                caret_row = Some(row_idx);
                let caret = caret_rect(&ctx, &galley, origin, line, buffer);
                // Remember where the caret was last seen, for the IME anchor
                // fallback on frames where its row is off screen.
                ui.ctx().data_mut(|d| d.insert_temp(caret_track_id, caret));
                if caret_is_showing(ui, caret_hold_until) {
                    text_painter.rect_filled(caret, 0, palette.caret);
                }
                ime_anchor = Some(paint_composition(&text_painter, &ctx, composing, caret));
            }

        }

        // Sticky gutter background, drawn under the numbers written above.
        // (Painted last so it wins over any text scrolled beneath it, using a
        // layer-free trick: an opaque strip plus a re-draw of the numbers.)
        paint_gutter_backdrop(&painter, &ctx, clip, layout, diff, &rows, focused, caret_line);

        // On top of the column, so it is visible however far the text is
        // scrolled sideways - which is exactly when an over-long line needs
        // saying so.
        for row in &long_rows {
            if expand_control(ui, &painter, &ctx, clip, row, lang) {
                toggle_expand = Some(row.line_idx);
            }
        }

        // ---- Input ------------------------------------------------------
        let caret_before = buffer.selection().head;
        if focused {
            edited |= handle_keyboard(ui, buffer, style, &mut goal, composing);
        }

        // Moving the caret by hand, or losing focus, abandons whatever was
        // being composed - the input method has no way to know the caret went
        // somewhere else.
        let mut interrupt = false;
        if response.clicked() || response.drag_started() || !focused {
            interrupt = composing.cancel();
        }
        handle_mouse(ui, &response, buffer, diff, layout, &ctx, &mut goal);

        // A caret move from the keyboard relocates the composition's anchor
        // just as a click does, so abandon the composition too. A commit that
        // just landed also moves the caret, but it cleared the composition
        // first, so `cancel` reports nothing to interrupt.
        if buffer.selection().head != caret_before {
            interrupt |= composing.cancel();
        }

        // Fresh input keeps the caret solid for a moment: blinking straight
        // off under the user's keystroke reads as the caret getting lost.
        if edited || buffer.selection().head != caret_before {
            let until = ui.input(|i| i.time) + CARET_HOLD;
            ui.ctx().data_mut(|d| d.insert_temp(caret_hold_id, until));
        }

        // ---- Tell the input method where to draw ------------------------
        //
        // This is also what *enables* it: the integration calls
        // `set_ime_allowed` with whether this is set, so a pane that never
        // reports an area cannot be typed into with an input method at all.
        if focused {
            let anchor = ime_anchor.unwrap_or_else(|| {
                // The caret's row is off screen this frame: anchor to where
                // the caret was last seen, clamped into the viewport, rather
                // than jumping the candidate window to the pane's corner.
                let last: Option<Rect> = ui.ctx().data(|d| d.get_temp(caret_track_id));
                let x = last
                    .map_or(clip.left(), |r| r.left())
                    .clamp(clip.left(), clip.right());
                let y = last
                    .map_or(clip.top(), |r| r.top())
                    .clamp(clip.top(), (clip.bottom() - line_h).max(clip.top()));
                Rect::from_min_size(pos2(x, y), Vec2::new(2.0, line_h))
            });
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: clip,
                    cursor_rect: anchor,
                    should_interrupt_composition: interrupt,
                });
            });
        }

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
                    wrap_w
                } else {
                    f32::INFINITY
                };
                let measured = measure_line(&text_painter, text, style, wrap, palette.text, ctx.cap_for(line));
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
        has_focus,
        goal_column: goal,
        toggle_expand,
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
    /// Lines the reader has opened in full, by line index on this side.
    expanded: &'a std::collections::HashSet<usize>,
}

impl RowCtx<'_> {
    /// How much of `line_idx` may be laid out.
    fn cap_for(&self, line_idx: usize) -> usize {
        if self.expanded.contains(&line_idx) {
            NO_CAP
        } else {
            MAX_RENDERED_CHARS
        }
    }
}

/// A row whose line is too long to draw in full, and where it sits.
struct LongRow {
    line_idx: usize,
    /// Top of the row, which may be far above the viewport: an over-long line
    /// wraps into hundreds of visual rows.
    y: f32,
    h: f32,
    /// Currently cut. `false` means it is open and can be closed again.
    cut: bool,
}

/// The control in the line-number column that opens or closes an over-long
/// line. Returns whether it was clicked this frame.
///
/// # Why it lives in the gutter
///
/// It started at the end of the line, which is where the text stops being
/// drawn - and that is hundreds of wrapped rows away, so the reader had to go
/// looking for it before they could even learn the line was cut. The column is
/// sticky: it stays put however far the text is scrolled sideways, so a marker
/// here is both the first thing seen on such a row and always in reach.
///
/// The marker follows the viewport down a tall row rather than sitting at its
/// top, because the top of a row that is 145 visual lines high is usually off
/// screen.
fn expand_control(
    ui: &Ui,
    painter: &Painter,
    ctx: &RowCtx<'_>,
    clip: Rect,
    row: &LongRow,
    lang: crate::i18n::Lang,
) -> bool {
    let gutter_right = clip.left() + ctx.gutter_w;
    let rect = Rect::from_min_max(
        pos2(gutter_right - GUTTER_PAD - CHANGE_BAR - 2.0, 0.0),
        pos2(gutter_right - CHANGE_BAR, ctx.line_h),
    );
    // Keep it on screen for as long as any part of the row is.
    let top = row
        .y
        .max(clip.top())
        .min((row.y + row.h - ctx.line_h).max(row.y));
    let rect = rect.translate(egui::vec2(0.0, top));

    let response = ui
        .interact(
            rect,
            ui.id().with(("expand", ctx.side as u8, row.line_idx)),
            Sense::click(),
        )
        .on_hover_text(crate::i18n::t(
            lang,
            if row.cut {
                "editor.show_whole_line"
            } else {
                "editor.collapse_line"
            },
        ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.rect_filled(rect, 2, ctx.palette.hover_bg);
    }

    // A triangle rather than a glyph: the bundled fonts are not guaranteed to
    // carry any particular arrow, and this is three points of geometry.
    let c = rect.center();
    let r = (ctx.line_h * 0.22).min(5.0);
    let colour = ctx.palette.accent;
    let points = if row.cut {
        // Pointing right: there is more this way.
        vec![
            pos2(c.x - r * 0.6, c.y - r),
            pos2(c.x - r * 0.6, c.y + r),
            pos2(c.x + r * 0.8, c.y),
        ]
    } else {
        // Pointing down: it is open.
        vec![
            pos2(c.x - r, c.y - r * 0.6),
            pos2(c.x + r, c.y - r * 0.6),
            pos2(c.x, c.y + r * 0.8),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        colour,
        egui::Stroke::NONE,
    ));

    response.clicked()
}

/// Points one ASCII character advances in the editor font.
///
/// Measured, not assumed. The rest of this file approximates it as 0.62 em,
/// which is fine for sizing a column of digits and useless for a line 620 000
/// characters long: the real advance at 14 pt is 8.43 pt against an assumed
/// 8.68, and that three percent is 156 000 points - some 18 000 characters -
/// of empty space the view would still scroll through after the text ended.
///
/// One 64-character layout, which egui then serves from its galley cache.
fn ascii_advance(painter: &Painter, style: &EditorStyle) -> f32 {
    const SAMPLE: usize = 64;
    let width = painter
        .layout_no_wrap("0".repeat(SAMPLE), style.font.clone(), Color32::WHITE)
        .size()
        .x;
    width / SAMPLE as f32
}

/// The exact width of one line, laid out as the pane will draw it.
///
/// Used to size the horizontal extent, which has to agree with the text to the
/// point rather than approximately. Per-character arithmetic cannot manage
/// that: egui accumulates each glyph's advance in `f32`, and over hundreds of
/// thousands of characters the rounding wanders. Measured against a
/// 4 096-character sample, the effective advance drifted by +0.05%, -0.10% and
/// +0.38% at 100 000, 300 000 and 620 383 characters - and on a line that long
/// even a tenth of a percent is tens of screens of blank past the last glyph.
///
/// The cost is one layout of one line, and it is the same `LayoutJob` the row
/// painter builds, so egui serves it from the galley cache it already filled.
pub fn line_width(painter: &Painter, style: &EditorStyle, line: &str, cap: usize) -> f32 {
    measure_line(painter, line, style, f32::INFINITY, Color32::WHITE, cap)
        .size()
        .x
}

/// Cut a line down to what will be laid out.
///
/// Returns the text to lay out and whether anything was dropped. The cut lands
/// on a character boundary; a line at or under the cap is returned untouched,
/// which is every line in ordinary source.
fn truncate_for_display(display: &str, cap: usize) -> (&str, bool) {
    // `char_indices` stops as soon as the cap is passed, so this costs nothing
    // on a normal line and does not walk a million characters on a huge one.
    match display.char_indices().nth(cap) {
        Some((byte, _)) => (&display[..byte], true),
        None => (display, false),
    }
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
    cap: usize,
) -> Arc<Galley> {
    // Cap the work before anything else, so every caller - the row painter,
    // the caret and the hit test - sees exactly the same galley. Doing this in
    // one place is what keeps the caret sitting where the click landed.
    let (display, truncated) = truncate_for_display(display, cap);

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
        if truncated {
            job.append(TRUNCATION_MARK, 0.0, fmt(base, false, false));
        }
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
    if truncated {
        job.append(TRUNCATION_MARK, 0.0, fmt(base, false, false));
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

/// Where the caret sits on screen, whether or not it is currently blinking on.
///
/// Split out from the painting because the input method needs this rectangle
/// every frame - it is what positions the candidate window - while the caret
/// itself is only drawn for half of each blink.
fn caret_rect(
    ctx: &RowCtx<'_>,
    galley: &Galley,
    origin: Pos2,
    line: &str,
    buffer: &TextBuffer,
) -> Rect {
    let (_, col) = buffer.line_col(buffer.selection().head);
    let col = tabs::to_display(line, ctx.style.tab_width, col);
    let p = galley.pos_from_cursor(CCursor::new(col));
    let x = (origin.x + p.min.x).round();
    Rect::from_min_max(
        pos2(x, origin.y + p.min.y),
        pos2(x + 2.0, origin.y + p.max.y),
    )
}

/// Whether the caret is visible this instant, keeping the blink clock running.
///
/// `hold_until` ends the solid phase that follows input: a caret that blinks
/// off the moment a key lands reads as a lost caret.
fn caret_is_showing(ui: &Ui, hold_until: Option<f64>) -> bool {
    let t = ui.input(|i| i.time);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(120));
    if hold_until.is_some_and(|until| t < until) {
        return true;
    }
    (t * 1.9).fract() <= 0.5
}

/// Draw the text an input method is still composing, and report where it ends.
///
/// The pre-edit is not in the document (see [`crate::ui::ime`]), so it is
/// painted here over an opaque strip, in the conventional style: underlined
/// throughout, with a heavier underline beneath the clause the input method has
/// focus on.
///
/// Returns the rectangle the candidate window should be anchored to - the end
/// of the pre-edit, so the candidate list follows what is being typed rather than
/// sitting where composition began.
fn paint_composition(
    painter: &Painter,
    ctx: &RowCtx<'_>,
    composing: &Composition,
    caret: Rect,
) -> Rect {
    let text = composing.text();
    if text.is_empty() {
        return caret;
    }

    let galley = painter.layout_no_wrap(
        text.to_owned(),
        ctx.style.font.clone(),
        ctx.palette.text,
    );
    let origin = pos2(caret.left(), caret.top());
    let width = galley.size().x;
    let box_rect = Rect::from_min_max(
        origin,
        pos2(origin.x + width, caret.bottom()),
    );

    // An opaque backdrop: the pre-edit floats above the line, so whatever text
    // follows the caret must not show through it.
    painter.rect_filled(box_rect.expand2(Vec2::new(1.0, 0.0)), 0, ctx.palette.editor_bg);
    painter.galley(origin, galley.clone(), ctx.palette.text);

    let underline = |from: f32, to: f32, thickness: f32| {
        let y = box_rect.bottom() - thickness;
        painter.rect_filled(
            Rect::from_min_max(pos2(from, y), pos2(to, box_rect.bottom())),
            0,
            ctx.palette.accent,
        );
    };
    underline(box_rect.left(), box_rect.right(), 1.0);

    if let Some(active) = composing.active() {
        let x = |chars: usize| origin.x + galley.pos_from_cursor(CCursor::new(chars)).min.x;
        underline(x(active.start), x(active.end), 2.0);
    }

    Rect::from_min_max(pos2(box_rect.right(), caret.top()), box_rect.max)
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
    let galley = measure_line(painter, line, style, wrap, ctx.palette.text, ctx.cap_for(line_idx));

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
    cap: usize,
) -> Arc<Galley> {
    let display = tabs::expand(line, style.tab_width);
    build_galley(painter, line, &display, &[], color, style, wrap_width, cap)
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
/// Remove characters either side of the caret, at an input method's request.
///
/// Used by input methods that reconvert text already in the document rather
/// than only appending - the surrounding characters are withdrawn so the
/// commit can replace them.
fn delete_around_caret(buffer: &mut TextBuffer, before_chars: usize, after_chars: usize) {
    let head = buffer.selection().head;
    let start = head.saturating_sub(before_chars);
    let end = (head + after_chars).min(buffer.len_chars());
    if start < end {
        buffer.replace_range(start..end, "");
    }
}

fn handle_keyboard(
    ui: &Ui,
    buffer: &mut TextBuffer,
    style: &EditorStyle,
    goal: &mut Option<usize>,
    composing: &mut Composition,
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
            // An input method never sends `Event::Text`. Chinese, Japanese and
            // Korean input arrives only down this branch, as a run of pre-edits
            // followed by a commit.
            Event::Ime(event) => {
                let step = match &event {
                    egui::ImeEvent::Preedit {
                        text,
                        active_range_chars,
                    } => Some(ime::Step::Preedit {
                        text,
                        active: active_range_chars.clone(),
                    }),
                    egui::ImeEvent::Commit(text) => Some(ime::Step::Commit(text)),
                    egui::ImeEvent::DeleteSurrounding {
                        before_chars,
                        after_chars,
                    } => {
                        delete_around_caret(buffer, *before_chars, *after_chars);
                        *goal = None;
                        None
                    }
                    _ => None,
                };
                if let Some(step) = step
                    && let ime::Outcome::Commit(text) = composing.apply(step)
                    && !text.is_empty()
                {
                    buffer.insert(&text);
                    *goal = None;
                }
            }
            Event::Paste(text) => {
                // The clipboard can carry CRLF or lone CR line endings; the
                // buffer is LF-only, and writing a stray `\r` back out through
                // a CRLF encoding would produce `\r\r\n`.
                buffer.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
                *goal = None;
            }
            // An empty selection must not clobber the clipboard.
            Event::Copy => {
                if !buffer.selection().is_empty() {
                    ui.ctx().copy_text(buffer.selected_text());
                }
            }
            Event::Cut => {
                if !buffer.selection().is_empty() {
                    ui.ctx().copy_text(buffer.selected_text());
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
                    let moved =
                        editing::move_caret(buffer, m, shift, *goal, style.vertical_motion, style.tab_width);
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
                vertical_motion: VerticalMotion::KeepColumn,
        }
    }

    /// A single line can be longer than an entire ordinary document, and text
    /// layout is linear in its length. Measured on a real 132 MB SQL dump: one
    /// 749 000-character line took 323 ms to lay out wrapped, and the frame it
    /// scrolled into view cost 573 ms across the two panes.
    ///
    /// Asserted on the work done rather than on the clock, so the guard does
    /// not turn into a measurement of how busy the machine is.
    #[test]
    fn a_giant_line_is_not_laid_out_in_full() {
        let s = style();
        let line = "SELECT ".to_owned() + &"abcdefgh, ".repeat(200_000);
        assert!(line.chars().count() > 1_000_000, "fixture is not giant");

        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            let g = measure_line(ui.painter(), &line, &s, 600.0, Color32::WHITE, MAX_RENDERED_CHARS);
            let laid_out = g.text().chars().count();
            assert!(
                laid_out <= MAX_RENDERED_CHARS + TRUNCATION_MARK.chars().count(),
                "laid out {laid_out} characters of a {} character line",
                line.chars().count()
            );
            // Uncapped and wrapped, this line was over 11 000 visual lines.
            assert!(g.rows.len() < 400, "{} visual lines", g.rows.len());
            assert!(g.text().ends_with(TRUNCATION_MARK), "no truncation marker");
        });
        out.textures_delta.clear();
    }

    /// The cap must be invisible to ordinary text - no marker, nothing dropped.
    #[test]
    fn an_ordinary_line_is_laid_out_untouched() {
        let s = style();
        let line = "INSERT INTO `t` VALUES (1, 'a'), (2, 'b');";
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            let g = measure_line(ui.painter(), line, &s, f32::INFINITY, Color32::WHITE, MAX_RENDERED_CHARS);
            assert_eq!(g.text(), line);
        });
        out.textures_delta.clear();
    }

    /// The cut is by characters, not bytes, so it cannot split a code point.
    #[test]
    fn the_cut_lands_on_a_character_boundary() {
        let line = "对比工具".repeat(MAX_RENDERED_CHARS);
        let (kept, truncated) = truncate_for_display(&line, MAX_RENDERED_CHARS);
        assert!(truncated);
        assert_eq!(kept.chars().count(), MAX_RENDERED_CHARS);
        assert!(line.starts_with(kept));
    }

    #[test]
    fn a_line_at_the_cap_is_left_alone() {
        let line = "x".repeat(MAX_RENDERED_CHARS);
        let (kept, truncated) = truncate_for_display(&line, MAX_RENDERED_CHARS);
        assert!(!truncated, "a line exactly at the cap was cut");
        assert_eq!(kept.len(), line.len());
    }

    /// The clock version of the same guard, with a threshold loose enough to
    /// survive a loaded machine but far under the 323 ms an uncapped line cost.
    #[test]
    fn laying_out_a_giant_line_stays_far_under_a_frame_budget() {
        use std::time::{Duration, Instant};

        let s = style();
        let line = "SELECT ".to_owned() + &"abcdefgh, ".repeat(200_000);
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            let t = Instant::now();
            measure_line(ui.painter(), &line, &s, 600.0, Color32::WHITE, MAX_RENDERED_CHARS);
            let took = t.elapsed();
            eprintln!("giant line layout: {took:?}");
            assert!(
                took < Duration::from_millis(60),
                "laying out one line took {took:?}"
            );
        });
        out.textures_delta.clear();
    }

    /// The scrollable width must end where the text does.
    ///
    /// It was computed as an assumed 0.62 em per character. On the 620 383
    /// character line this was reported against, that overshot the real width
    /// by three percent - 156 000 points, some 18 000 characters - so the view
    /// kept scrolling long after the last `;` had gone past. Per-character
    /// arithmetic cannot be made exact either: egui accumulates advances in
    /// `f32` and the rounding wanders both ways over a long line. So the line
    /// is measured.
    #[test]
    fn the_measured_width_agrees_with_the_text_to_the_point() {
        let s = style();
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            for n in [20usize, 4_000, 200_000] {
                let line = "0".repeat(n);
                let drawn =
                    measure_line(ui.painter(), &line, &s, f32::INFINITY, Color32::WHITE, NO_CAP)
                        .size()
                        .x;
                let measured = line_width(ui.painter(), &s, &line, NO_CAP);
                assert_eq!(
                    measured, drawn,
                    "{n} chars: the extent is sized from a different width than the text draws at"
                );
            }
        });
        out.textures_delta.clear();
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
            let galley = measure_line(ui.painter(), line, &s, f32::INFINITY, Color32::WHITE, MAX_RENDERED_CHARS);
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

    /// Two panes whose widths differ by half a character must still fold a
    /// long line identically.
    ///
    /// This is the xmas_event.sql bug: the split had been dragged 0.2% off
    /// centre, so the panes wrapped 4pt apart - and that flipped one fold on
    /// a 2528-character INSERT line. The row took the taller side's height and
    /// the wider pane showed a dead band at the bottom of the row, looking for
    /// all the world like an extra empty line. The fixture below reproduces
    /// that sensitivity: half a point of width moves it from 14 visual lines
    /// to 13.
    #[test]
    fn nearly_equal_panes_fold_long_lines_identically() {
        let s = style();
        let line = format!(
            "INSERT INTO `t` VALUES {}",
            "(52516, 0, 4, 1, 'Sandals of Faith', 35148, 0, 0, 8, 16, -1, 86, 7, 19, 5, 22, 0, 89, 0, 21626, 1, 17371, 0, 50, 0, 300, 1);"
                .repeat(6)
        );
        let narrow = 513.25;
        let wide = 513.75;
        let tol = s.font.size * 0.62;
        let ctx = egui::Context::default();
        let mut layout = RowLayout::wrapped(1);

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui: &mut Ui| {
            // The widths really are sensitive: left alone, they fold the same
            // text into different counts of visual lines...
            let a = measure_line(ui.painter(), &line, &s, narrow, Color32::WHITE, MAX_RENDERED_CHARS);
            let b = measure_line(ui.painter(), &line, &s, wide, Color32::WHITE, MAX_RENDERED_CHARS);
            assert_eq!(a.rows.len(), 14, "fixture lost its sensitivity");
            assert_eq!(b.rows.len(), 13, "fixture lost its sensitivity");
            // ...and neither galley carries a glyph-less trailing segment -
            // the dead band never came from an empty fold.
            for (w, g) in [(narrow, &a), (wide, &b)] {
                assert!(
                    g.rows.iter().all(|r| !r.glyphs.is_empty()),
                    "empty segment at width {w}"
                );
            }

            // Frame 1: the left pane reports first; the right snaps to it.
            let l = layout.aligned_wrap_width(Side::Left, narrow, tol);
            layout.set_wrap_width(Side::Left, l);
            let r = layout.aligned_wrap_width(Side::Right, wide, tol);
            layout.set_wrap_width(Side::Right, r);
            assert_eq!(r, narrow);

            // Frame 2: both sides agree, so both fold the line the same way.
            let l = layout.aligned_wrap_width(Side::Left, narrow, tol);
            assert_eq!(l, narrow);
            let g = measure_line(ui.painter(), &line, &s, l, Color32::WHITE, MAX_RENDERED_CHARS);
            assert_eq!(g.rows.len(), a.rows.len());
        });
        out.textures_delta.clear();
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
                let galley = measure_line(ui.painter(), line, &s, f32::INFINITY, Color32::WHITE, MAX_RENDERED_CHARS);
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
