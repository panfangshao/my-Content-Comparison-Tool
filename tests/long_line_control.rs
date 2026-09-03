//! Opening an over-long line, and the control that does it.
//!
//! Rendering stops at 10 000 characters, because laying out a megabyte-long
//! `INSERT` costs a third of a second and scrolling past a run of them stopped
//! the window responding. A comparison tool whose text simply stops is broken
//! in its own way, so any one line can be opened in full - and only that line
//! pays.
//!
//! The control sits in the line-number column, which stays put however far the
//! text is scrolled sideways.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2, pos2};

use duibi::core::diff::{Budget, DiffOptions, Side, diff_lines};
use duibi::core::text::TextBuffer;
use duibi::ui::editing::VerticalMotion;
use duibi::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use duibi::ui::rowlayout::RowLayout;
use duibi::ui::theme::Palette;

/// Text drawn by the pane this frame, in paint order.
fn drawn_text(left: &str, right: &str, wrap: bool) -> Vec<String> {
    drawn_text_with(left, right, wrap, &Default::default())
}

/// The same, with a set of lines opened in full on the left.
fn drawn_text_with(
    left: &str,
    right: &str,
    wrap: bool,
    open: &std::collections::HashSet<usize>,
) -> Vec<String> {
    run(left, right, wrap, open, None).0
}

/// Click at `at` on the left pane and report which line's control fired.
fn click_reports(left: &str, right: &str, at: Pos2) -> Option<usize> {
    run(left, right, true, &Default::default(), Some(at)).1
}

/// Drive both panes for a few frames, optionally clicking, and report the text
/// drawn plus any expand control that fired on the left.
fn run(
    left: &str,
    right: &str,
    wrap: bool,
    open: &std::collections::HashSet<usize>,
    click: Option<Pos2>,
) -> (Vec<String>, Option<usize>) {
    let mut lbuf = TextBuffer::from_text(left);
    let mut rbuf = TextBuffer::from_text(right);
    let diff = diff_lines(
        lbuf.lines(),
        rbuf.lines(),
        &DiffOptions::default(),
        Budget::unlimited(),
    );
    let style = EditorStyle {
        font: egui::FontId::monospace(14.0),
        line_height: 20.0,
        show_line_numbers: true,
        show_whitespace: false,
        word_wrap: wrap,
        tab_width: 4,
        vertical_motion: VerticalMotion::KeepColumn,
    };
    let palette = Palette::dark();
    let mut layout = if wrap {
        RowLayout::wrapped(diff.rows.len())
    } else {
        RowLayout::uniform(diff.rows.len())
    };
    let mut inline = InlineCache::default();
    let ctx = egui::Context::default();
    let gutter_lines = lbuf.len_lines().max(rbuf.len_lines());
    let empty: std::collections::HashSet<usize> = Default::default();
    let mut hit: Option<usize> = None;

    let mut seen = Vec::new();
    let mut fired = None;
    // Move, press, release: `clicked()` needs the press and the release to
    // land on the same widget across frames.
    let script: Vec<Vec<Event>> = match click {
        None => vec![Vec::new(); 4],
        Some(at) => vec![
            Vec::new(),
            vec![Event::PointerMoved(at)],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        ],
    };

    for events in script {
        let (l, r, d, lay, inl) = (&mut lbuf, &mut rbuf, &diff, &mut layout, &mut inline);
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(
                    pos2(0.0, 0.0),
                    Vec2::new(1460.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.horizontal_top(|ui| {
                    for side in [Side::Left, Side::Right] {
                        let (buffer, other) = match side {
                            Side::Left => (&mut *l, r.lines()),
                            Side::Right => (&mut *r, l.lines()),
                        };
                        let mut composing = duibi::ui::ime::Composition::default();
                        ui.allocate_ui(Vec2::new(700.0, 600.0), |ui| {
                            let out = show_pane(
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
                                    style: &style,
                                    lang: duibi::i18n::Lang::English,
                                    gutter_lines,
                                    highlights: &[],
                                    highlight_rows: 0..0,
                                    search: &[],
                                    active_match: None,
                                    longest_line: 2_000_000.0,
                                    force_offset: None,
                                    focus_requested: false,
                                    goal_column: None,
                                    composing: &mut composing,
                                    expanded: if side == Side::Left {
                                        open
                                    } else {
                                        &empty
                                    },
                                },
                            );
                            if side == Side::Left && out.toggle_expand.is_some() {
                                hit = out.toggle_expand;
                            }
                        });
                    }
                });
            },
        );
        seen = collect_text(&out.shapes);
        out.textures_delta.clear();
        lay.commit_measurements();
        if hit.is_some() {
            fired = hit;
        }
    }
    (seen, fired)
}

fn collect_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn walk(s: &egui::epaint::Shape, out: &mut Vec<String>) {
        match s {
            egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// A megabyte on one line with one value changed near its end.
fn giant_pair() -> (String, String) {
    let head = "INSERT INTO `t` VALUES ".to_owned() + &"(1,'aaa'),".repeat(120_000);
    (format!("{head}(1,'aaa');"), format!("{head}(1,'bbb');"))
}

/// The point of the control: the rest of the line really does appear.
#[test]
fn opening_a_line_lays_out_all_of_it() {
    let (a, b) = giant_pair();
    let full_chars = a.chars().count();

    let cut = drawn_text(&a, &b, true).join("").chars().count();
    assert!(
        cut < full_chars / 10,
        "the cut line drew {cut} characters of {full_chars}; the cap is not on"
    );

    let open: std::collections::HashSet<usize> = [0usize].into_iter().collect();
    let whole = drawn_text_with(&a, &b, true, &open).join("").chars().count();
    assert!(
        whole > full_chars,
        "opening line 0 drew only {whole} characters of a {full_chars} character line"
    );
}

/// The control has to be where the reader is looking: in the line-number
/// column, which stays put however far the text is scrolled sideways.
///
/// It used to sit at the end of the line - hundreds of wrapped rows away, so
/// the reader had to go hunting before they could even learn the line was cut.
#[test]
fn clicking_the_gutter_control_opens_that_line() {
    let (a, b) = giant_pair();
    // Middle of the marker: just left of the change bar at the column's edge.
    let at = pos2(gutter_width() - 9.0, 10.0);
    assert_eq!(
        click_reports(&a, &b, at),
        Some(0),
        "clicking the control on line 0 did not report it"
    );
}

/// An ordinary line offers nothing there, so the click must do nothing.
#[test]
fn the_same_click_on_a_short_line_does_nothing() {
    let at = pos2(gutter_width() - 9.0, 10.0);
    assert_eq!(
        click_reports("hello", "world", at),
        None,
        "a short line offered an expand control"
    );
}

/// Nor does clicking in the text itself.
#[test]
fn clicking_the_text_does_not_open_a_line() {
    let (a, b) = giant_pair();
    assert_eq!(click_reports(&a, &b, pos2(300.0, 10.0)), None);
}

/// The column width the marker is positioned against.
fn gutter_width() -> f32 {
    EditorStyle {
        font: egui::FontId::monospace(14.0),
        line_height: 20.0,
        show_line_numbers: true,
        show_whitespace: false,
        word_wrap: true,
        tab_width: 4,
        vertical_motion: VerticalMotion::KeepColumn,
    }
    .gutter_for(1)
}
