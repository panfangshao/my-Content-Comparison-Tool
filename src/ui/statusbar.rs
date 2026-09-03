//! The status bar: the comparison summary plus per-pane facts.

use egui::{RichText, Ui};

use crate::core::diff::{DiffStats, HunkSummary, RowKind, Side};
use crate::core::text::{FileEncoding, TextBuffer};
use crate::i18n::{Lang, t, tf};
use crate::ui::theme::Palette;

pub struct StatusFacts<'a> {
    /// With one document there is no comparison to summarise.
    pub single_pane: bool,
    pub stats: DiffStats,
    pub identical: bool,
    pub truncated: bool,
    pub comparing: bool,
    /// How long the last comparison took.
    pub last_compare: std::time::Duration,
    pub focus_side: Side,
    pub buffer: &'a TextBuffer,
    pub encoding: &'a FileEncoding,
    pub hunk_position: Option<(usize, usize)>,
    /// One entry per difference, for the jump list.
    pub summaries: &'a [HunkSummary],
}

/// The difference the user asked to jump to, if any.
pub type JumpRequest = Option<usize>;

pub fn show_status_bar(
    ui: &mut Ui,
    f: &StatusFacts<'_>,
    palette: &Palette,
    lang: Lang,
) -> JumpRequest {
    let mut jump = None;
    ui.horizontal(|ui| {
        // ---- Comparison summary ----------------------------------------
        if f.single_pane {
            // Nothing to report: the right-hand group below still shows the
            // caret position, line count and encoding.
        } else if f.comparing {
            ui.spinner();
            ui.label(RichText::new(t(lang, "status.comparing")).color(palette.text_dim));
        } else if f.identical {
            // No tick glyph here: the bundled font has no U+2713 and would draw
            // a tofu box. The colour carries the meaning on its own.
            ui.label(
                RichText::new(t(lang, "status.identical"))
                    .color(palette.ok)
                    .strong(),
            );
        } else {
            counter(ui, "+", f.stats.added, palette.marker_ins, t(lang, "status.added"));
            counter(
                ui,
                "\u{2212}",
                f.stats.removed,
                palette.marker_del,
                t(lang, "status.removed"),
            );
            counter(
                ui,
                "\u{223C}",
                f.stats.modified,
                palette.marker_mod,
                t(lang, "status.modified"),
            );

            ui.separator();
            // Similarity is the number people actually quote, so give it room.
            let pct = f.stats.similarity * 100.0;
            let color = if pct > 90.0 {
                palette.ok
            } else if pct > 50.0 {
                palette.warn
            } else {
                palette.error
            };
            ui.label(RichText::new(t(lang, "status.similarity")).color(palette.text_dim));
            ui.label(
                RichText::new(format_similarity(&f.stats))
                    .color(color)
                    .strong(),
            );

            if let Some((i, n)) = f.hunk_position {
                ui.separator();
                jump = diff_jump_list(ui, f, palette, lang, i, n);
            }
        }

        if f.truncated {
            ui.separator();
            ui.label(RichText::new("\u{26A0}").color(palette.warn))
                .on_hover_text(t(lang, "status.truncated"));
        }

        // ---- Per-pane facts, right aligned -----------------------------
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} \u{00B7} {}",
                    f.encoding.label(),
                    f.encoding.newline.label()
                ))
                .color(palette.text_dim),
            );

            ui.separator();

            let sel = f.buffer.selection();
            if !sel.is_empty() {
                let n = sel.end() - sel.start();
                ui.label(
                    RichText::new(tf(lang, "status.selection", &[("n", &n.to_string())]))
                        .color(palette.accent),
                );
                ui.separator();
            }

            let (line, col) = f.buffer.line_col(sel.head);
            ui.label(
                RichText::new(tf(
                    lang,
                    "status.ln_col",
                    &[("l", &(line + 1).to_string()), ("c", &(col + 1).to_string())],
                ))
                .color(palette.text_dim),
            );

            ui.separator();
            // "共 N 行", not "N 行": this sits directly beside "行 L，列 C",
            // and two segments both starting with 行 read as one repeated
            // label rather than as a total and a position.
            let lines = f.buffer.len_lines();
            ui.label(
                RichText::new(if lines == 1 {
                    t(lang, "status.lines_total_one").to_owned()
                } else {
                    tf(lang, "status.lines_total", &[("n", &lines.to_string())])
                })
                .color(palette.text_dim),
            );

            if !f.single_pane {
                ui.separator();
                ui.label(
                    RichText::new(t(
                        lang,
                        if f.focus_side == Side::Left {
                            "app.left"
                        } else {
                            "app.right"
                        },
                    ))
                    .color(palette.text_dim),
                );
            }

            // Comparison timing, shown only when it is slow enough to matter.
            if f.last_compare.as_millis() >= 8 {
                ui.separator();
                ui.label(
                    RichText::new(format!("{} ms", f.last_compare.as_millis()))
                        .color(palette.text_faint)
                        .small(),
                )
                .on_hover_text(t(lang, "status.compare_time"));
            }
        });
    });
    jump
}

/// The "Diff 3/12" counter, as a button that drops a list of every difference.
///
/// The overview strip on the right already allows jumping, but it asks for a
/// precise click on a few pixels of a compressed map. This is the same
/// navigation as something you can actually read: line number, what changed,
/// and how large the block is.
fn diff_jump_list(
    ui: &mut Ui,
    f: &StatusFacts<'_>,
    palette: &Palette,
    lang: Lang,
    current: usize,
    total: usize,
) -> JumpRequest {
    let label = tf(
        lang,
        "status.diff_of",
        &[("i", &current.to_string()), ("n", &total.to_string())],
    );
    let button = ui
        .button(RichText::new(label).color(palette.text_dim))
        .on_hover_text(t(lang, "status.diff_list"));

    let mut jump = None;
    // The status bar is at the bottom of the window, so the list opens upwards
    // or it would be drawn off screen.
    egui::Popup::menu(&button)
        .align(egui::RectAlign::TOP_START)
        .show(|ui| {
            ui.set_min_width(380.0);
            ui.label(
                RichText::new(t(lang, "status.diff_list.title"))
                    .small()
                    .weak(),
            );
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for (i, summary) in f.summaries.iter().enumerate() {
                        if jump_entry(ui, palette, lang, summary, i + 1 == current) {
                            jump = Some(i);
                            ui.close();
                        }
                    }
                });
        });
    jump
}

/// One row of the jump list. Returns true when clicked.
///
/// Built as a `LayoutJob` so the marker, the line number and the text can carry
/// different colours inside a single clickable widget - three separate labels
/// in a row would not be clickable as one thing.
fn jump_entry(
    ui: &mut Ui,
    palette: &Palette,
    lang: Lang,
    summary: &HunkSummary,
    is_current: bool,
) -> bool {
    let (glyph, marker) = match summary.kind {
        RowKind::Insert => ('+', palette.marker_ins),
        RowKind::Delete => ('\u{2212}', palette.marker_del),
        _ => ('\u{223C}', palette.marker_mod),
    };

    let mono = egui::FontId::monospace(12.0);
    let mut job = egui::text::LayoutJob::default();
    let fmt = |color, font: egui::FontId| egui::text::TextFormat {
        font_id: font,
        color,
        ..Default::default()
    };

    job.append(&format!("{glyph} "), 0.0, fmt(marker, mono.clone()));
    job.append(
        &format!("{:>7}  ", summary.line + 1),
        0.0,
        fmt(palette.text_faint, mono.clone()),
    );
    if summary.text.is_empty() {
        job.append(
            t(lang, "status.blank_line"),
            0.0,
            fmt(palette.text_faint, mono),
        );
    } else {
        job.append(&summary.text, 0.0, fmt(palette.text, mono));
    }

    // A hunk spanning several lines says so, so a one-line label is not
    // mistaken for the whole block.
    if summary.rows > 1 {
        job.append(
            &format!("   +{}", summary.rows - 1),
            0.0,
            fmt(palette.text_faint, egui::FontId::monospace(10.0)),
        );
    }

    ui.selectable_label(is_current, job).clicked()
}

/// Render the similarity percentage.
///
/// Two changed lines out of ten thousand really is 99.98% similar, but at one
/// decimal place that prints as "100.0%" - which reads as "no differences" and
/// flatly contradicts the counters sitting next to it. So the precision grows
/// until the number is honest about not being whole.
fn format_similarity(stats: &DiffStats) -> String {
    if stats.is_identical() {
        return "100%".to_owned();
    }
    let pct = (stats.similarity * 100.0).clamp(0.0, 100.0);
    for decimals in 1..=3 {
        let text = format!("{pct:.*}", decimals);
        if text != "100" && !text.starts_with("100.") {
            return format!("{text}%");
        }
    }
    // Differences exist but round away even at three decimals; say so without
    // claiming a perfect match.
    "99.999%".to_owned()
}

/// One `+12` style counter, dimmed to nothing when the count is zero.
fn counter(ui: &mut Ui, glyph: &str, count: usize, color: egui::Color32, tip: &str) {
    let text = format!("{glyph}{count}");
    let color = if count == 0 {
        color.gamma_multiply(0.45)
    } else {
        color
    };
    ui.label(RichText::new(text).color(color).monospace())
        .on_hover_text(tip);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_keys_exist_in_both_languages() {
        for key in [
            "status.added",
            "status.removed",
            "status.modified",
            "status.similarity",
            "status.identical",
            "status.lines_total",
            "status.lines_total_one",
            "status.ln_col",
            "status.selection",
            "status.diff_of",
            "status.comparing",
            "status.truncated",
            "status.compare_time",
        ] {
            for lang in [Lang::Chinese, Lang::English] {
                assert!(!t(lang, key).is_empty(), "{key} missing for {lang:?}");
            }
        }
    }

    /// The line total sits immediately beside the caret position. When the
    /// total read `1 行` and the position read `行 1，列 1`, the two ran
    /// together as one repeated label - the 行 appeared to be printed twice.
    ///
    /// They have to be told apart at a glance, so they must not open with the
    /// same character.
    #[test]
    fn the_line_total_does_not_read_as_the_caret_position() {
        for lang in [Lang::Chinese, Lang::English] {
            let total = t(lang, "status.lines_total_one");
            let position = tf(lang, "status.ln_col", &[("l", "1"), ("c", "1")]);
            assert_ne!(
                total.chars().next(),
                position.chars().next(),
                "{lang:?}: `{total}` beside `{position}` reads as one label repeated"
            );
        }
    }

    /// `1 lines` was wrong; the phrasing has to work for a one-line document.
    #[test]
    fn the_line_total_reads_correctly_for_a_single_line() {
        let english = t(Lang::English, "status.lines_total_one");
        assert!(
            !english.contains("1 lines"),
            "`{english}` is ungrammatical for one line"
        );
    }

    fn stats(unchanged: usize, modified: usize, total_lines: usize) -> DiffStats {
        DiffStats {
            modified,
            unchanged,
            similarity: (2 * unchanged) as f32 / (2 * total_lines) as f32,
            ..DiffStats::default()
        }
    }

    #[test]
    fn identical_documents_read_as_a_clean_hundred() {
        let s = DiffStats {
            unchanged: 100,
            similarity: 1.0,
            ..DiffStats::default()
        };
        assert_eq!(format_similarity(&s), "100%");
    }

    /// The case that prompted this: two changed lines in ten thousand.
    #[test]
    fn a_tiny_difference_never_rounds_up_to_a_hundred() {
        let s = stats(9_998, 2, 10_000);
        let text = format_similarity(&s);
        assert!(
            !text.starts_with("100"),
            "two changed lines reported as {text}"
        );
        assert!(text.starts_with("99.9"), "unexpected value: {text}");
    }

    #[test]
    fn a_single_changed_line_in_a_million_still_is_not_a_hundred() {
        let s = stats(999_999, 1, 1_000_000);
        assert!(!format_similarity(&s).starts_with("100"));
    }

    #[test]
    fn ordinary_values_keep_one_decimal() {
        let s = stats(80, 20, 100);
        assert_eq!(format_similarity(&s), "80.0%");
    }

    #[test]
    fn completely_different_documents_read_as_zero() {
        let s = stats(0, 10, 10);
        assert_eq!(format_similarity(&s), "0.0%");
    }

    #[test]
    fn the_position_template_substitutes_both_fields() {
        let s = tf(Lang::English, "status.ln_col", &[("l", "3"), ("c", "7")]);
        assert_eq!(s, "Ln 3, Col 7");
        assert!(!s.contains('{'), "a placeholder was left unfilled");

        let s = tf(Lang::Chinese, "status.ln_col", &[("l", "3"), ("c", "7")]);
        assert!(!s.contains('{'), "a placeholder was left unfilled");
    }
}
