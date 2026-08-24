//! Tab expansion for display.
//!
//! epaint gives a `\t` whatever advance the font happens to define, which for
//! the bundled monospace face is one space - so indented code collapses. We
//! therefore render tabs as spaces, which means the text the *galley* holds is
//! not the text the *buffer* holds, and every offset that crosses that boundary
//! has to be translated.
//!
//! Both directions are `O(line length)` walks. That is fine: they run only for
//! the handful of positions per visible row that actually need a coordinate
//! (the caret, the selection ends, the diff spans, the search hits).

/// Columns a tab advances to. Four is the common default for the languages
/// this tool is most often pointed at.
pub const TAB_WIDTH: usize = 4;

/// Whether a line needs expansion at all. Almost all do not, and skipping the
/// work keeps the common path allocation-free.
#[inline]
pub fn has_tabs(line: &str) -> bool {
    line.as_bytes().contains(&b'\t')
}

/// Expand tabs to spaces, aligning to the next multiple of `tab_width`.
pub fn expand(line: &str, tab_width: usize) -> String {
    if !has_tabs(line) {
        return line.to_owned();
    }
    let tab_width = tab_width.max(1);
    let mut out = String::with_capacity(line.len() + 8);
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let stop = tab_width - (col % tab_width);
            out.extend(std::iter::repeat_n(' ', stop));
            col += stop;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Map a character index in the original line to one in the expanded line.
pub fn to_display(line: &str, tab_width: usize, char_index: usize) -> usize {
    if !has_tabs(line) {
        return char_index;
    }
    let tab_width = tab_width.max(1);
    let mut col = 0usize;
    for (i, c) in line.chars().enumerate() {
        if i == char_index {
            return col;
        }
        col += if c == '\t' {
            tab_width - (col % tab_width)
        } else {
            1
        };
    }
    col
}

/// Map a character index in the expanded line back to the original line.
///
/// A position inside the run of spaces a tab expanded into resolves to that
/// tab, so clicking in the middle of an indent puts the caret somewhere real.
pub fn from_display(line: &str, tab_width: usize, display_index: usize) -> usize {
    if !has_tabs(line) {
        return display_index.min(line.chars().count());
    }
    let tab_width = tab_width.max(1);
    let mut col = 0usize;
    for (i, c) in line.chars().enumerate() {
        let width = if c == '\t' {
            tab_width - (col % tab_width)
        } else {
            1
        };
        // Land on whichever side of the character the click was nearer to.
        if display_index < col + width {
            return if display_index > col + width / 2 && c == '\t' {
                i + 1
            } else {
                i
            };
        }
        col += width;
    }
    line.chars().count()
}

/// Byte offset -> character index within a line.
#[inline]
pub fn byte_to_char(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 4;

    #[test]
    fn lines_without_tabs_pass_through() {
        assert_eq!(expand("plain text", W), "plain text");
        assert_eq!(to_display("plain", W, 3), 3);
        assert_eq!(from_display("plain", W, 3), 3);
    }

    #[test]
    fn a_leading_tab_becomes_a_full_stop() {
        assert_eq!(expand("\tx", W), "    x");
        assert_eq!(expand("\t\tx", W), "        x");
    }

    #[test]
    fn tabs_align_to_the_next_stop_not_a_fixed_width() {
        // "ab" is 2 columns, so the tab only advances 2 more to reach column 4.
        assert_eq!(expand("ab\tc", W), "ab  c");
        assert_eq!(expand("abc\td", W), "abc d");
        assert_eq!(expand("abcd\te", W), "abcd    e");
    }

    #[test]
    fn to_display_tracks_the_expansion() {
        let line = "ab\tc";
        assert_eq!(to_display(line, W, 0), 0);
        assert_eq!(to_display(line, W, 2), 2, "the tab itself starts at col 2");
        assert_eq!(to_display(line, W, 3), 4, "`c` sits at col 4");
    }

    #[test]
    fn round_trip_on_every_position() {
        for line in ["\tx", "a\tb\tc", "\t\t", "no tabs", "abcd\te", ""] {
            let n = line.chars().count();
            for i in 0..=n {
                let d = to_display(line, W, i);
                assert_eq!(
                    from_display(line, W, d),
                    i,
                    "{line:?} index {i} did not round trip"
                );
            }
        }
    }

    #[test]
    fn expanded_length_matches_the_last_display_index() {
        for line in ["\tx", "a\tb", "abcd\te", "plain"] {
            let expanded = expand(line, W);
            let n = line.chars().count();
            assert_eq!(
                to_display(line, W, n),
                expanded.chars().count(),
                "{line:?} length disagreed"
            );
        }
    }

    #[test]
    fn a_click_inside_an_indent_snaps_to_the_tab() {
        let line = "\tcode";
        // Columns 0..4 are all the one tab character.
        assert_eq!(from_display(line, W, 0), 0);
        assert_eq!(from_display(line, W, 1), 0);
        assert_eq!(from_display(line, W, 3), 1, "past the midpoint goes after");
        assert_eq!(from_display(line, W, 4), 1, "`c` is character 1");
    }

    #[test]
    fn clicking_past_the_end_clamps() {
        assert_eq!(from_display("\tab", W, 999), 3);
        assert_eq!(from_display("abc", W, 999), 3);
    }

    #[test]
    fn byte_to_char_handles_multibyte() {
        let line = "对比 x";
        assert_eq!(byte_to_char(line, 0), 0);
        assert_eq!(byte_to_char(line, 3), 1);
        assert_eq!(byte_to_char(line, 6), 2);
        assert_eq!(byte_to_char(line, 999), 4);
    }

    #[test]
    fn a_zero_tab_width_does_not_divide_by_zero() {
        assert_eq!(expand("\tx", 0), " x");
        assert_eq!(to_display("\tx", 0, 1), 1);
    }

    #[test]
    fn cjk_and_tabs_together() {
        let line = "\t对比";
        assert_eq!(expand(line, W), "    对比");
        assert_eq!(to_display(line, W, 1), 4);
        assert_eq!(from_display(line, W, 5), 2);
    }
}
