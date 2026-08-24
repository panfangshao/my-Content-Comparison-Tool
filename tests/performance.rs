//! Performance guards for the sizes the brief calls out.
//!
//! These are wall-clock assertions, so the thresholds are set generously -
//! several times the measured time on ordinary hardware. They are here to catch
//! an accidental O(n^2), not to police milliseconds. Run with `--release` for
//! representative numbers; the limits below hold in a debug build too because
//! `[profile.dev.package."*"]` keeps the dependencies optimized.

use std::time::{Duration, Instant};

use duibi::core::diff::{Budget, DiffOptions, diff_lines};
use duibi::core::text::{Selection, TextBuffer};

/// A realistic source-like document.
fn document(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| match i % 5 {
            0 => format!("fn handler_{i}(request: &Request) -> Result<Response> {{"),
            1 => format!("    let payload = request.body().read_to_end({i})?;"),
            2 => "    validate(&payload).context(\"bad payload\")?;".to_owned(),
            3 => format!("    Ok(Response::json(&payload, {i}))"),
            _ => "}".to_owned(),
        })
        .collect()
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    (out, t.elapsed())
}

#[test]
fn compares_one_hundred_thousand_lines_quickly() {
    let left = document(100_000);
    let mut right = left.clone();
    // Scatter realistic edits through the document.
    for i in (0..100_000).step_by(997) {
        right[i] = format!("    // edited line {i}");
    }
    right.remove(50_000);
    right.insert(75_000, "    let extra = inserted();".to_owned());

    let (diff, elapsed) = timed(|| {
        diff_lines(&left, &right, &DiffOptions::default(), Budget::unlimited())
    });

    assert!(diff.hunks.len() > 90, "expected the scattered edits to be found");
    assert_eq!(diff.row_of_left.len(), left.len());
    assert!(
        elapsed < Duration::from_secs(5),
        "100k-line comparison took {elapsed:?}"
    );
    eprintln!("100k lines compared in {elapsed:?}, {} hunks", diff.hunks.len());
}

#[test]
fn identical_large_documents_are_the_fast_path() {
    let left = document(100_000);
    let right = left.clone();
    let (diff, elapsed) = timed(|| {
        diff_lines(&left, &right, &DiffOptions::default(), Budget::unlimited())
    });
    assert!(diff.stats.is_identical());
    assert!(
        elapsed < Duration::from_secs(2),
        "identical 100k documents took {elapsed:?}"
    );
    eprintln!("100k identical lines compared in {elapsed:?}");
}

/// The interactive budget must actually bound the work: a pathological input
/// should come back with *something* rather than freezing the UI thread.
#[test]
fn the_time_budget_bounds_a_pathological_comparison() {
    // Two documents with no lines in common and heavy internal repetition -
    // close to the worst case for a line differ.
    let left: Vec<String> = (0..60_000).map(|i| format!("alpha {}", i % 700)).collect();
    let right: Vec<String> = (0..60_000).map(|i| format!("beta {}", i % 900)).collect();

    let budget = Budget {
        deadline: Some(Instant::now() + Duration::from_millis(250)),
        max_align_cells: 1 << 18,
    };
    let (diff, elapsed) = timed(|| diff_lines(&left, &right, &DiffOptions::default(), budget));

    assert!(!diff.rows.is_empty());
    assert!(
        elapsed < Duration::from_secs(4),
        "the budget did not bound the work: {elapsed:?}"
    );
    eprintln!("pathological 60k comparison bounded to {elapsed:?}");
}

/// Typing deep inside a large document must not cost more than typing at the
/// top - that is the whole reason for the rope plus patched line cache.
#[test]
fn editing_deep_in_a_large_document_is_cheap() {
    let text = document(100_000).join("\n");
    let mut buffer = TextBuffer::from_text(&text);

    let at_top = {
        buffer.set_selection(Selection::at(buffer.char_at(1, 0)));
        let (_, d) = timed(|| {
            for _ in 0..200 {
                buffer.insert("x");
            }
        });
        d
    };

    let deep = {
        buffer.set_selection(Selection::at(buffer.char_at(95_000, 0)));
        let (_, d) = timed(|| {
            for _ in 0..200 {
                buffer.insert("x");
            }
        });
        d
    };

    eprintln!("200 keystrokes: at line 1 {at_top:?}, at line 95000 {deep:?}");
    assert!(
        deep < Duration::from_millis(500),
        "typing at line 95 000 took {deep:?}"
    );
    // The rope makes position independent of depth; allow a wide margin for
    // scheduler noise but catch a genuine linear scan.
    assert!(
        deep < at_top * 20 + Duration::from_millis(50),
        "editing deep in the document is disproportionately slow: {at_top:?} vs {deep:?}"
    );
}

/// The line cache must stay in step with the rope across a long editing
/// session, because a drift there silently corrupts every later comparison.
#[test]
fn the_line_cache_survives_a_long_editing_session() {
    let mut buffer = TextBuffer::from_text(&document(5_000).join("\n"));

    for i in (0..5_000).step_by(11) {
        buffer.set_selection(Selection::at(buffer.char_at(i, 0)));
        buffer.insert("// ");
    }
    for _ in 0..200 {
        buffer.undo();
    }
    for _ in 0..100 {
        buffer.redo();
    }

    let rope_lines: Vec<String> = buffer
        .rope()
        .lines()
        .map(|l| {
            let s = l.to_string();
            s.strip_suffix('\n').unwrap_or(&s).to_owned()
        })
        .collect();
    assert_eq!(
        buffer.lines(),
        rope_lines.as_slice(),
        "the line cache drifted from the rope"
    );
}

/// Undo history must not grow without bound during a long session.
#[test]
fn undo_history_stays_bounded() {
    let mut buffer = TextBuffer::new();
    for i in 0..20_000 {
        buffer.insert(if i % 50 == 0 { "\n" } else { "a" });
    }
    let mut undone = 0;
    while buffer.undo() {
        undone += 1;
        assert!(undone < 10_000, "undo history grew unbounded");
    }
}
