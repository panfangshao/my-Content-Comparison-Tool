//! Diagnostic probes for the 132 MB SQL-dump report: two documents that size,
//! scrolled from top to bottom, degrade until the window stops responding.
//!
//! These are *measurements*, not assertions - they exist to name the hot path.
//! They are `#[ignore]`d because they allocate hundreds of megabytes and take
//! minutes in a debug build. Run them with:
//!
//! ```text
//! cargo test --release --test deep_scroll_probe -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use egui::{Rect, Vec2, pos2};

use duibi::core::text::TextBuffer;
use duibi::ui::highlight::{PaneHighlighter, SyntaxAssets};

/// One synthetic INSERT line of about `bytes` bytes, shaped like a mysqldump
/// extended-insert: a prefix, then many `(n,'text')` tuples.
fn insert_line(bytes: usize) -> String {
    let mut s = String::from("INSERT INTO `big_table` VALUES ");
    let mut n = 0u32;
    while s.len() < bytes {
        let _ = std::fmt::Write::write_fmt(
            &mut s,
            format_args!("({n},'some text value {n}',42.5,'2024-01-01'),"),
        );
        n += 1;
    }
    s.push_str("(0,'end',0.0,'2024-01-01');");
    s
}

/// A SQL dump of about `target_bytes`: short lines up top, extended inserts
/// that grow toward the bottom, the way real dumps do (biggest table last).
fn sql_dump(target_bytes: usize) -> TextBuffer {
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    // Header: short comment/DDL lines.
    while total < target_bytes / 4 {
        let i = lines.len();
        lines.push(format!("-- dump line {i}: CREATE TABLE stub {i} (id int);"));
        total += lines[i].len() + 1;
    }
    // Body: 2.5k-char INSERT lines (what the user's screenshot showed).
    while total < target_bytes * 3 / 4 {
        let l = insert_line(2_500);
        total += l.len() + 1;
        lines.push(l);
    }
    // Tail: a few enormous single-statement inserts, 100kB..1MB.
    while total < target_bytes {
        let l = insert_line(100_000 + (lines.len() % 9) * 100_000);
        total += l.len() + 1;
        lines.push(l);
    }
    TextBuffer::from_text(&lines.join("\n"))
}

/// Plain long lines, tens of characters each, `lines` of them.
#[allow(dead_code, reason = "kept for the next time this probe is needed")]
fn plain_doc(lines: usize) -> TextBuffer {
    let text: Vec<String> = (0..lines)
        .map(|i| format!("INSERT INTO t VALUES ({i}, 'row {i}', {i}.5);"))
        .collect();
    TextBuffer::from_text(&text.join("\n"))
}

/// What does one `parse_line` cost as the line grows? This is the call the
/// 8 ms catch-up slice cannot interrupt: the clock is only read between lines.
#[test]
#[ignore]
fn probe_syntect_cost_vs_line_length() {
    let syntaxes = syntect::parsing::SyntaxSet::load_defaults_nonewlines();
    let sql = syntaxes.find_syntax_by_extension("sql").expect("SQL syntax");
    let themes = syntect::highlighting::ThemeSet::load_defaults();
    let theme = themes.themes.get("base16-ocean.dark").expect("theme");
    let highlighter = syntect::highlighting::Highlighter::new(theme);

    for size in [2_500usize, 16_000, 64_000, 100_000, 500_000, 1_000_000] {
        let line = insert_line(size);
        let mut state = syntect::parsing::ParseState::new(sql);
        let mut hl =
            syntect::highlighting::HighlightState::new(&highlighter, syntect::parsing::ScopeStack::new());
        // Warm up: the first parse of a syntax pays lazy regex compilation,
        // which is real but amortised over the whole file - what matters for
        // scrolling is the steady-state cost of the *next* line.
        let small = insert_line(64);
        for _ in 0..20 {
            let ops = state.parse_line(&small, &syntaxes).expect("parse");
            for _ in syntect::highlighting::RangedHighlightIterator::new(&mut hl, &ops, &small, &highlighter) {}
        }
        let start = Instant::now();
        let ops = state.parse_line(&line, &syntaxes).expect("parse");
        for _ in syntect::highlighting::RangedHighlightIterator::new(&mut hl, &ops, &line, &highlighter) {}
        let t = start.elapsed();
        eprintln!("parse_line SQL, {size:>9} bytes: {t:?} (warm)");
    }

    // And the rate that actually decides whether catch-up ever finishes:
    // a run of ordinary 2.5k extended inserts, steady state.
    let line = insert_line(2_500);
    let mut state = syntect::parsing::ParseState::new(sql);
    let mut hl =
        syntect::highlighting::HighlightState::new(&highlighter, syntect::parsing::ScopeStack::new());
    let ops = state.parse_line(&line, &syntaxes).expect("parse");
    for _ in syntect::highlighting::RangedHighlightIterator::new(&mut hl, &ops, &line, &highlighter) {}
    let start = Instant::now();
    const N: usize = 50;
    for _ in 0..N {
        let ops = state.parse_line(&line, &syntaxes).expect("parse");
        for _ in syntect::highlighting::RangedHighlightIterator::new(&mut hl, &ops, &line, &highlighter) {}
    }
    let per = start.elapsed() / N as u32;
    eprintln!("parse_line SQL, steady 2500-byte INSERT: {per:?}/line ({:.0} kB/s)", 2.5 / per.as_secs_f64());
}

/// What does one galley layout cost for a megabyte-long wrapped line, and
/// what does a *repeat* frame cost (the cache should answer that)?
#[test]
#[ignore]
fn probe_galley_cost_vs_line_length() {
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(900.0, 700.0))),
        ..Default::default()
    };
    for size in [2_500usize, 100_000, 1_000_000] {
        let line = insert_line(size);
        let (mut first, mut second) = (Duration::ZERO, Duration::ZERO);
        let _ = ctx.run_ui(input.clone(), |ui| {
            for t in [&mut first, &mut second] {
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = 380.0;
                job.break_on_newline = false;
                job.append(
                    &line,
                    0.0,
                    egui::text::TextFormat {
                        font_id: egui::FontId::monospace(14.0),
                        ..Default::default()
                    },
                );
                let start = Instant::now();
                let galley = ui.painter().layout_job(job);
                *t = start.elapsed();
                std::hint::black_box(galley.rows.len());
            }
        });
        eprintln!("galley wrap 380px, {size:>9} bytes: first {first:?}, cached {second:?}");
    }
}

/// The O(document) `chars().count()` scan behind the horizontal scrollbar.
#[test]
#[ignore]
fn probe_longest_line_scan() {
    let doc = sql_dump(132 * 1024 * 1024);
    let start = Instant::now();
    let longest = doc
        .lines()
        .iter()
        .map(|l| {
            l.chars()
                .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
                .sum::<f32>()
        })
        .fold(0.0, f32::max);
    eprintln!(
        "longest_line over {} lines / 132MB: {:?} (max {longest:.0})",
        doc.len_lines(),
        start.elapsed()
    );
}

/// Drive the real highlighter down a dump the way scrolling does: ask for a
/// window, keep asking while it catches up, record the worst single call.
/// One call == one frame; the app's budget for that call is 8 ms.
#[test]
#[ignore]
fn probe_highlight_catchup_on_a_dump() {
    let assets = SyntaxAssets::load();
    let doc = sql_dump(132 * 1024 * 1024);
    let lines = doc.lines();
    eprintln!("dump: {} lines, {} bytes", lines.len(), lines.iter().map(String::len).sum::<usize>());

    let mut h = PaneHighlighter::new();
    h.set_syntax(Some("SQL"));

    let mut worst = Duration::ZERO;
    let mut frames = 0u32;
    // Scroll down in ~screenful steps from top to bottom.
    let mut top = 0usize;
    while top < lines.len() {
        let window = top..(top + 60).min(lines.len());
        loop {
            let start = Instant::now();
            h.highlight(&assets, "base16-ocean.dark", lines, window.clone());
            let t = start.elapsed();
            worst = worst.max(t);
            frames += 1;
            if !h.is_catching_up() {
                break;
            }
            if frames % 200 == 0 {
                eprintln!("  frame {frames}, at line {top}, last call {t:?}, worst {worst:?}");
            }
        }
        top += 60;
    }
    eprintln!("highlight walk: {frames} frames, worst single call {worst:?}");
}
