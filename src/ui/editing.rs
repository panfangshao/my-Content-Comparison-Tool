//! Caret movement and selection, expressed purely over a [`TextBuffer`].
//!
//! Kept free of egui types so the fiddly parts - word boundaries, clamping at
//! the ends of the document, what Shift does to an existing selection - can be
//! tested directly instead of through a rendered window.

use crate::core::text::{Selection, TextBuffer};

/// A caret movement request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// To the start of the previous word.
    WordLeft,
    /// To the start of the next word.
    WordRight,
    /// First non-whitespace character, then column 0 on a second press - the
    /// "smart home" every editor has.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp(usize),
    PageDown(usize),
}

/// What kind of character a position sits on. Word motion moves between runs
/// of the same class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

fn classify(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        // CJK ideographs are `alphanumeric`, so they count as word characters.
        // That makes Ctrl+Arrow step across a run of Chinese text rather than
        // one character at a time, which is what users expect.
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// The result of a movement: where the caret went, and the column it should
/// try to keep on further vertical moves.
pub struct Moved {
    pub selection: Selection,
    pub goal_column: Option<usize>,
}

/// Move the caret.
///
/// `extend` is Shift: keep the anchor and drag the head. Without it, a
/// collapsed selection moves normally, while a non-empty selection *collapses*
/// to its near edge for Left/Right - the standard behaviour that surprises
/// people when it is missing.
///
/// `goal_column` carries the column the user was last at, so moving down
/// through a short line and back out does not lose their place.
pub fn move_caret(
    buf: &TextBuffer,
    motion: Motion,
    extend: bool,
    goal_column: Option<usize>,
) -> Moved {
    let sel = buf.selection();
    let (line, col) = buf.line_col(sel.head);

    // Collapsing a selection with an unshifted Left/Right.
    if !extend && !sel.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
        let head = if motion == Motion::Left {
            sel.start()
        } else {
            sel.end()
        };
        return Moved {
            selection: Selection::at(head),
            goal_column: None,
        };
    }

    let (head, new_goal) = match motion {
        Motion::Left => (sel.head.saturating_sub(1), None),
        Motion::Right => ((sel.head + 1).min(buf.len_chars()), None),

        Motion::Up => vertical(buf, line, col, goal_column, -1),
        Motion::Down => vertical(buf, line, col, goal_column, 1),
        Motion::PageUp(n) => vertical(buf, line, col, goal_column, -(n as isize)),
        Motion::PageDown(n) => vertical(buf, line, col, goal_column, n as isize),

        Motion::WordLeft => (word_left(buf, sel.head), None),
        Motion::WordRight => (word_right(buf, sel.head), None),

        Motion::LineStart => (smart_home(buf, line, col), None),
        Motion::LineEnd => (buf.line_end(line), None),
        Motion::DocStart => (0, None),
        Motion::DocEnd => (buf.len_chars(), None),
    };

    Moved {
        selection: Selection {
            anchor: if extend { sel.anchor } else { head },
            head,
        },
        goal_column: new_goal,
    }
}

/// Vertical movement by `delta` logical lines, remembering the goal column.
fn vertical(
    buf: &TextBuffer,
    line: usize,
    col: usize,
    goal_column: Option<usize>,
    delta: isize,
) -> (usize, Option<usize>) {
    let goal = goal_column.unwrap_or(col);
    let last = buf.len_lines().saturating_sub(1);
    let target = (line as isize + delta).clamp(0, last as isize) as usize;

    // Moving past either end parks the caret at the document boundary, which
    // is what pressing Up on the first line should do.
    if target == line {
        let head = if delta < 0 { 0 } else { buf.len_chars() };
        return (head, Some(goal));
    }
    (buf.char_at(target, goal), Some(goal))
}

/// First non-whitespace character of the line, unless we are already there, in
/// which case column 0.
fn smart_home(buf: &TextBuffer, line: usize, col: usize) -> usize {
    let text = buf.line(line);
    let indent = text.chars().take_while(|c| c.is_whitespace()).count();
    let start = buf.line_start(line);
    if col == indent { start } else { start + indent }
}

fn word_left(buf: &TextBuffer, ci: usize) -> usize {
    if ci == 0 {
        return 0;
    }
    let rope = buf.rope();
    let mut i = ci;

    // Step back over whitespace, then over one run of a single class.
    while i > 0 && classify(rope.char(i - 1)) == CharClass::Whitespace {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let class = classify(rope.char(i - 1));
    while i > 0 && classify(rope.char(i - 1)) == class {
        i -= 1;
    }
    i
}

fn word_right(buf: &TextBuffer, ci: usize) -> usize {
    let len = buf.len_chars();
    if ci >= len {
        return len;
    }
    let rope = buf.rope();
    let mut i = ci;

    // Step over one run of a single class, then over the whitespace after it,
    // landing at the start of the next word.
    let class = classify(rope.char(i));
    while i < len && classify(rope.char(i)) == class {
        i += 1;
    }
    while i < len && classify(rope.char(i)) == CharClass::Whitespace {
        i += 1;
    }
    i
}

/// The word under `ci`, for double-click selection.
///
/// On whitespace, selects the whitespace run; that matches how editors behave
/// and gives double-click-drag something sensible to extend from.
pub fn word_range_at(buf: &TextBuffer, ci: usize) -> std::ops::Range<usize> {
    let len = buf.len_chars();
    if len == 0 {
        return 0..0;
    }
    let rope = buf.rope();
    let probe = ci.min(len.saturating_sub(1));
    let class = classify(rope.char(probe));

    let mut start = probe;
    while start > 0 && classify(rope.char(start - 1)) == class {
        start -= 1;
    }
    let mut end = probe;
    while end < len && classify(rope.char(end)) == class {
        end += 1;
    }
    start..end
}

/// The whole line under `ci`, including its newline, for triple-click.
pub fn line_range_at(buf: &TextBuffer, ci: usize) -> std::ops::Range<usize> {
    let line = buf.line_of_char(ci);
    let start = buf.line_start(line);
    let end = if line + 1 < buf.len_lines() {
        buf.line_start(line + 1)
    } else {
        buf.len_chars()
    };
    start..end
}

/// The whitespace prefix of a line, used to keep indentation on Enter.
pub fn indent_of(buf: &TextBuffer, line: usize) -> String {
    buf.line(line)
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

/// Insert a newline, carrying the current line's indentation onto the new one.
pub fn insert_newline(buf: &mut TextBuffer) {
    let (line, _) = buf.line_col(buf.selection().head);
    let indent = indent_of(buf, line);
    let mut text = String::with_capacity(indent.len() + 1);
    text.push('\n');
    text.push_str(&indent);
    buf.insert(&text);
}

/// Indent every line the selection touches by one unit.
pub fn indent_selection(buf: &mut TextBuffer, unit: &str) {
    let sel = buf.selection();
    let first = buf.line_of_char(sel.start());
    let last = buf.line_of_char(sel.end());
    let added = unit.chars().count();

    // Capture the caret in (line, column) terms *before* editing: character
    // offsets are about to shift underneath us.
    let anchor_lc = buf.line_col(sel.anchor);
    let head_lc = buf.line_col(sel.head);

    let lines: Vec<String> = (first..=last)
        .map(|l| format!("{unit}{}", buf.line(l)))
        .collect();
    buf.replace_lines(first..last + 1, &lines);

    // Every touched line grew by `added` at its start, so a caret on one of
    // them keeps its place in the text by moving the same amount.
    let shift = |(line, col): (usize, usize)| {
        if (first..=last).contains(&line) {
            (line, col + added)
        } else {
            (line, col)
        }
    };
    let (al, ac) = shift(anchor_lc);
    let (hl, hc) = shift(head_lc);
    buf.set_selection(Selection {
        anchor: buf.char_at(al, ac),
        head: buf.char_at(hl, hc),
    });
}

/// Remove up to one unit of indentation from every line the selection touches.
pub fn outdent_selection(buf: &mut TextBuffer, unit: &str) {
    let sel = buf.selection();
    let first = buf.line_of_char(sel.start());
    let last = buf.line_of_char(sel.end());
    let width = unit.chars().count().max(1);

    let anchor_lc = buf.line_col(sel.anchor);
    let head_lc = buf.line_col(sel.head);

    // How much each line actually loses - a line with no indent loses nothing,
    // so the shift has to be tracked per line rather than assumed uniform.
    let mut stripped = Vec::with_capacity(last - first + 1);
    let lines: Vec<String> = (first..=last)
        .map(|l| {
            let text = buf.line(l);
            let strip = text
                .chars()
                .take(width)
                .take_while(|c| c.is_whitespace())
                .count();
            stripped.push(strip);
            text.chars().skip(strip).collect()
        })
        .collect();
    buf.replace_lines(first..last + 1, &lines);

    let shift = |(line, col): (usize, usize)| {
        if (first..=last).contains(&line) {
            (line, col.saturating_sub(stripped[line - first]))
        } else {
            (line, col)
        }
    };
    let (al, ac) = shift(anchor_lc);
    let (hl, hc) = shift(head_lc);
    buf.set_selection(Selection {
        anchor: buf.char_at(al, ac),
        head: buf.char_at(hl, hc),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, caret: usize) -> TextBuffer {
        let mut b = TextBuffer::from_text(text);
        b.set_selection(Selection::at(caret));
        b
    }

    fn head(b: &TextBuffer, motion: Motion, extend: bool) -> usize {
        move_caret(b, motion, extend, None).selection.head
    }

    #[test]
    fn horizontal_movement_clamps_at_the_ends() {
        let b = buf("abc", 0);
        assert_eq!(head(&b, Motion::Left, false), 0);
        let b = buf("abc", 3);
        assert_eq!(head(&b, Motion::Right, false), 3);
    }

    #[test]
    fn an_unshifted_arrow_collapses_a_selection() {
        let mut b = TextBuffer::from_text("hello world");
        b.set_selection(Selection {
            anchor: 2,
            head: 8,
        });
        assert_eq!(head(&b, Motion::Left, false), 2, "collapses to the near edge");
        assert_eq!(head(&b, Motion::Right, false), 8);
    }

    #[test]
    fn shift_extends_from_the_anchor() {
        let mut b = TextBuffer::from_text("hello");
        b.set_selection(Selection::at(2));
        let m = move_caret(&b, Motion::Right, true, None);
        assert_eq!(m.selection.anchor, 2);
        assert_eq!(m.selection.head, 3);
    }

    #[test]
    fn vertical_movement_keeps_the_goal_column() {
        // Down from a long line, through a short one, back to a long one.
        let b = buf("abcdefgh\nxy\nijklmnop", 6); // line 0, column 6

        let down = move_caret(&b, Motion::Down, false, None);
        assert_eq!(b.line_col(down.selection.head), (1, 2), "clamped to short line");
        assert_eq!(down.goal_column, Some(6));

        let mut b2 = b;
        b2.set_selection(down.selection);
        let down2 = move_caret(&b2, Motion::Down, false, down.goal_column);
        assert_eq!(
            b2.line_col(down2.selection.head),
            (2, 6),
            "the goal column must be restored"
        );
    }

    #[test]
    fn up_from_the_first_line_goes_to_the_start_of_the_document() {
        let b = buf("abc\ndef", 2);
        assert_eq!(head(&b, Motion::Up, false), 0);
    }

    #[test]
    fn down_from_the_last_line_goes_to_the_end() {
        let b = buf("abc\ndef", 5);
        assert_eq!(head(&b, Motion::Down, false), 7);
    }

    #[test]
    fn page_movement_travels_many_lines() {
        let text = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let b = buf(&text, 0);
        let m = move_caret(&b, Motion::PageDown(30), false, None);
        assert_eq!(b.line_col(m.selection.head).0, 30);
    }

    #[test]
    fn smart_home_toggles_between_indent_and_column_zero() {
        let b = buf("    indented", 8); // inside the text
        let at_indent = head(&b, Motion::LineStart, false);
        assert_eq!(b.line_col(at_indent), (0, 4), "first stop is the indent");

        let mut b2 = b;
        b2.set_selection(Selection::at(at_indent));
        assert_eq!(
            b2.line_col(head(&b2, Motion::LineStart, false)),
            (0, 0),
            "pressing again goes to column zero"
        );
    }

    #[test]
    fn line_end_stops_before_the_newline() {
        let b = buf("abc\ndef", 0);
        assert_eq!(head(&b, Motion::LineEnd, false), 3);
    }

    #[test]
    fn word_motion_steps_over_words_not_characters() {
        let mut pos = 0;
        let mut stops = vec![];
        for _ in 0..6 {
            let b = buf("let value = compute(a);", pos);
            pos = head(&b, Motion::WordRight, false);
            stops.push(pos);
        }
        // Word starts, not individual characters.
        assert_eq!(stops[0], 4, "start of `value`");
        assert_eq!(stops[1], 10, "start of `=`");
        assert!(stops.windows(2).all(|w| w[0] < w[1]), "must make progress");
    }

    #[test]
    fn word_left_lands_on_word_starts() {
        let b = buf("let value = compute", 19);
        let p = head(&b, Motion::WordLeft, false);
        assert_eq!(&b.text()[p..], "compute");
    }

    #[test]
    fn word_motion_terminates_at_the_document_edges() {
        let b = buf("   ", 3);
        assert_eq!(head(&b, Motion::WordLeft, false), 0);
        let b = buf("   ", 0);
        assert_eq!(head(&b, Motion::WordRight, false), 3);
        let b = buf("", 0);
        assert_eq!(head(&b, Motion::WordLeft, false), 0);
        assert_eq!(head(&b, Motion::WordRight, false), 0);
    }

    #[test]
    fn cjk_counts_as_word_characters() {
        let b = buf("对比工具 next", 0);
        // One Ctrl+Right should clear the whole CJK run plus its space.
        assert_eq!(head(&b, Motion::WordRight, false), 5);
    }

    #[test]
    fn double_click_selects_a_word() {
        let b = TextBuffer::from_text("hello world");
        assert_eq!(word_range_at(&b, 2), 0..5);
        assert_eq!(word_range_at(&b, 8), 6..11);
        // On the space between them, select the space run.
        assert_eq!(word_range_at(&b, 5), 5..6);
    }

    #[test]
    fn double_click_on_an_empty_document_is_safe() {
        let b = TextBuffer::from_text("");
        assert_eq!(word_range_at(&b, 0), 0..0);
    }

    #[test]
    fn triple_click_selects_the_line_including_its_newline() {
        let b = TextBuffer::from_text("one\ntwo\nthree");
        assert_eq!(line_range_at(&b, 1), 0..4);
        assert_eq!(line_range_at(&b, 5), 4..8);
        assert_eq!(line_range_at(&b, 10), 8..13, "the last line has no newline");
    }

    #[test]
    fn enter_carries_the_indentation() {
        let mut b = TextBuffer::from_text("    indented");
        b.set_selection(Selection::at(12));
        insert_newline(&mut b);
        assert_eq!(b.lines(), ["    indented", "    "]);
        assert_eq!(b.selection().head, b.len_chars());
    }

    #[test]
    fn enter_on_an_unindented_line_adds_nothing() {
        let mut b = TextBuffer::from_text("plain");
        b.set_selection(Selection::at(5));
        insert_newline(&mut b);
        assert_eq!(b.lines(), ["plain", ""]);
    }

    #[test]
    fn indent_and_outdent_are_inverses_over_a_selection() {
        let mut b = TextBuffer::from_text("a\nb\nc");
        b.set_selection(Selection { anchor: 0, head: 5 });
        indent_selection(&mut b, "    ");
        assert_eq!(b.lines(), ["    a", "    b", "    c"]);

        b.set_selection(Selection {
            anchor: 0,
            head: b.len_chars(),
        });
        outdent_selection(&mut b, "    ");
        assert_eq!(b.lines(), ["a", "b", "c"]);
    }

    #[test]
    fn outdent_stops_at_column_zero() {
        let mut b = TextBuffer::from_text("  a\nb");
        b.set_selection(Selection {
            anchor: 0,
            head: b.len_chars(),
        });
        outdent_selection(&mut b, "    ");
        assert_eq!(b.lines(), ["a", "b"], "an unindented line is left alone");
    }

    #[test]
    fn indent_preserves_the_selected_text() {
        let mut b = TextBuffer::from_text("alpha\nbeta\ngamma");
        // Select "pha\nbeta\nga".
        let (a0, h0) = (2, 13);
        b.set_selection(Selection {
            anchor: a0,
            head: h0,
        });
        let before = b.selected_text();
        assert_eq!(before, "pha\nbeta\nga");

        indent_selection(&mut b, "  ");
        assert_eq!(b.lines(), ["  alpha", "  beta", "  gamma"]);
        // The endpoints stay on the same characters. The selection *content*
        // grows, because the indentation was inserted inside it - that is the
        // point of the operation, not drift.
        assert_eq!(
            b.selected_text(),
            "pha\n  beta\n  ga",
            "an endpoint drifted off its character"
        );

        outdent_selection(&mut b, "  ");
        assert_eq!(b.lines(), ["alpha", "beta", "gamma"]);
        assert_eq!(b.selected_text(), before);
        assert_eq!(b.selection().anchor, a0);
        assert_eq!(b.selection().head, h0);
    }

    #[test]
    fn outdent_keeps_the_caret_on_partially_indented_lines() {
        let mut b = TextBuffer::from_text("    deep\nshallow");
        b.set_selection(Selection {
            anchor: 0,
            head: b.len_chars(),
        });
        outdent_selection(&mut b, "    ");
        assert_eq!(b.lines(), ["deep", "shallow"]);
        assert_eq!(b.selection().head, b.len_chars());
    }

    #[test]
    fn indent_is_a_single_undo_step() {
        let mut b = TextBuffer::from_text("a\nb");
        b.set_selection(Selection { anchor: 0, head: 3 });
        indent_selection(&mut b, "\t");
        assert!(b.undo());
        assert_eq!(b.lines(), ["a", "b"]);
    }

    #[test]
    fn movement_never_leaves_the_document() {
        let b = TextBuffer::from_text("abc\ndef\n");
        let motions = [
            Motion::Left,
            Motion::Right,
            Motion::Up,
            Motion::Down,
            Motion::WordLeft,
            Motion::WordRight,
            Motion::LineStart,
            Motion::LineEnd,
            Motion::DocStart,
            Motion::DocEnd,
            Motion::PageUp(50),
            Motion::PageDown(50),
        ];
        for start in 0..=b.len_chars() {
            let mut b2 = TextBuffer::from_text("abc\ndef\n");
            b2.set_selection(Selection::at(start));
            for m in motions {
                let r = move_caret(&b2, m, false, None);
                assert!(
                    r.selection.head <= b2.len_chars(),
                    "{m:?} from {start} escaped the document"
                );
            }
        }
    }
}
