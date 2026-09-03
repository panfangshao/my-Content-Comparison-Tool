//! Syntax highlighting on top of syntect, made compatible with virtual
//! scrolling.
//!
//! # The problem
//!
//! syntect is a *sequential* parser: to know the state at line 90 000 you must
//! have parsed lines 0..90 000. A virtualized editor draws 50 lines wherever
//! the user happens to have scrolled, so the naive approach either re-parses
//! the whole file every frame or gives up on highlighting.
//!
//! # The approach
//!
//! Snapshot the parser state every [`STRIDE`] lines. Drawing a window then
//! costs "rewind to the nearest checkpoint, re-parse at most `STRIDE` lines,
//! plus the window itself" - bounded work, independent of where you scrolled.
//!
//! Building the checkpoints is still linear in the file, so it is spread across
//! frames with a per-frame budget: scroll to the end of a large file and the
//! text appears immediately, un-highlighted, and colours in over the next few
//! frames rather than freezing the window.
//!
//! Edits only discard the checkpoints *after* the edited line (see
//! `TextBuffer::min_dirty_line`), so typing at line 90 000 does not throw away
//! the work done for lines 0..90 000.
//!
//! # Why the result is cached
//!
//! Rewinding is bounded but not cheap: up to [`STRIDE`] lines of re-parsing for
//! every window drawn. Doing that on every frame cost 60 ms per pane on a
//! 14 000-line file - 120 ms for the two of them, against a 16 ms budget - and
//! made scrolling and even caret movement visibly lag.
//!
//! So a padded window is kept: scrolling anywhere inside it is free, and only
//! leaving it pays for a rewind. Editing or changing the theme drops it.
//!
//! The window is filled in across frames too, for the same reason the
//! checkpoints are. Parsing a whole padded window at once is another 500-odd
//! lines, which is another dropped frame - just moved to the moment the colour
//! finally lands rather than to the scroll that asked for it. Every frame here
//! is bounded by [`CATCHUP_SLICE`]; nothing is ever parsed twice.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;
use syntect::highlighting::{
    FontStyle, HighlightState, Highlighter, RangedHighlightIterator, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines between parser snapshots. 512 keeps the rewind cost small while
/// holding the checkpoint list to ~200 entries for a 100k-line file.
const STRIDE: usize = 256;

/// Extra lines parsed above and below the window actually asked for.
///
/// This is what makes scrolling free: a frame only pays for parsing when the
/// view leaves the padded region. Larger means fewer rebuilds and more work per
/// rebuild; 256 covers several screens in either direction.
const CACHE_PAD: usize = 256;

/// How long one frame may spend building checkpoints.
///
/// This used to be a line count, and that does not bound anything useful: how
/// long a line takes to parse varies by an order of magnitude between languages
/// and line lengths. A budget of 4096 lines turned into a 661 ms frame on a
/// 14 000-line Rust file - a visible freeze on the first scroll into the middle
/// of a document. Time is the thing being rationed, so time is what is counted.
const CATCHUP_SLICE: Duration = Duration::from_millis(8);

/// Bytes parsed between clock readings. Reading the clock per line would
/// itself show up in the measurement, and counting *bytes* rather than lines
/// is what keeps the reading cadence honest when lines vary from tens of
/// characters to the [`MAX_LINE_HIGHLIGHT_BYTES`] cap.
const CLOCK_EVERY_BYTES: usize = 8_192;

/// Lines parsed between clock readings, whichever limit is reached first.
///
/// Bytes alone are not enough. Ordinary source is tens of bytes a line, so
/// 8 KB is hundreds of lines - and parsing cost tracks lines far more than it
/// tracks bytes. On a 14 000-line file that made the gap between two readings
/// 30 ms and the worst frame 72 ms, against a slice of 8 ms. Bytes bound the
/// giant-line case, lines bound the ordinary one; whichever trips first wins.
const CLOCK_EVERY_LINES: usize = 32;

/// Above this many lines we do not highlight at all. Past this point the file
/// is machine-generated far more often than not, and the checkpoint list plus
/// the catch-up cost stop being worth it.
pub const MAX_HIGHLIGHT_LINES: usize = 200_000;

/// Above this many *bytes* we do not highlight at all, whatever the line
/// count. The line cap misses the dump shape - a 100MB+ SQL file can be
/// 50 000 lines of extended INSERTs - and at the measured ~2µs/byte of the
/// pure-Rust regex backend, that is minutes of parsing the checkpoint chain
/// would chase forever, burning its time slice on every frame. Whole-file
/// highlighting is simply not affordable past this point; the text still
/// renders, just uncoloured.
pub const MAX_HIGHLIGHT_BYTES: usize = 16 * 1024 * 1024;

/// Bytes past which a single line is not fed to the parser at all.
///
/// The time slice is only checked *between* lines - one `parse_line` call
/// cannot be interrupted, and its cost scales with the line. An extended SQL
/// INSERT or a minified bundle puts a megabyte on one line, and a single call
/// on it costs orders of magnitude more than the whole slice; a dump whose
/// lines grow toward the bottom froze one frame per checkpoint chunk, which
/// is exactly the "gets slower the further you scroll, then stops responding"
/// report this constant answers. An over-long line is drawn uncoloured, and
/// the parser state carries over as though the line were not there - for the
/// self-contained statements dumps are made of, that is the right state.
pub const MAX_LINE_HIGHLIGHT_BYTES: usize = 20_000;

/// One highlighted run within a line: a byte range and its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledRange {
    pub range: Range<usize>,
    pub color: Color32,
    pub italic: bool,
    pub bold: bool,
}

/// The syntax and theme definitions. Loading these takes tens of milliseconds
/// and a couple of megabytes, so exactly one is built and shared by both panes.
pub struct SyntaxAssets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

impl SyntaxAssets {
    pub fn load() -> Arc<Self> {
        Arc::new(Self {
            // `nonewlines` matches our line storage, which strips them.
            syntaxes: SyntaxSet::load_defaults_nonewlines(),
            themes: ThemeSet::load_defaults(),
        })
    }

    pub fn theme(&self, name: &str) -> &Theme {
        self.themes
            .themes
            .get(name)
            .or_else(|| self.themes.themes.values().next())
            .expect("syntect ships at least one default theme")
    }

    /// Guess a syntax from a filename, falling back to the first line.
    pub fn detect(&self, path: Option<&std::path::Path>, first_line: &str) -> &SyntaxReference {
        if let Some(p) = path {
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && let Some(s) = self.syntaxes.find_syntax_by_extension(ext)
            {
                return s;
            }
            if let Some(name) = p.file_name().and_then(|e| e.to_str())
                && let Some(s) = self.syntaxes.find_syntax_by_extension(name)
            {
                return s;
            }
        }
        self.syntaxes
            .find_syntax_by_first_line(first_line)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    /// Look up a syntax the user picked by name.
    pub fn by_name(&self, name: &str) -> Option<&SyntaxReference> {
        self.syntaxes
            .syntaxes()
            .iter()
            .find(|s| s.name == name)
    }

    pub fn plain_text_name(&self) -> &str {
        &self.syntaxes.find_syntax_plain_text().name
    }

    /// Syntax names for the language picker, alphabetical and deduplicated.
    pub fn language_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .syntaxes
            .syntaxes()
            .iter()
            .filter(|s| !s.hidden && !s.file_extensions.is_empty())
            .map(|s| s.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

#[derive(Clone)]
struct Checkpoint {
    parse: ParseState,
    highlight: HighlightState,
}

/// Per-pane highlighting state.
pub struct PaneHighlighter {
    /// Name of the syntax in use; `None` means plain text (nothing to do).
    syntax_name: Option<String>,
    theme_name: String,
    /// `checkpoints[i]` is the parser state *entering* line `i * STRIDE`.
    checkpoints: Vec<Checkpoint>,
    /// Set when the last request could not be satisfied within the budget, so
    /// the app knows to schedule another frame.
    catching_up: bool,
    /// The last window parsed, kept so that scrolling within it costs nothing.
    cache: Option<CachedWindow>,
    /// Lines fed to the parser since this counter was last read.
    ///
    /// The time slice cannot be tested with a clock: the assertion would
    /// measure how loaded the machine is, and it failed exactly that way when
    /// run beside 300 other tests. Work done is the thing being rationed, and
    /// counting it is load-independent. One integer add per line.
    parsed_this_frame: usize,
    /// Work part-way through the next checkpoint, carried across frames.
    ///
    /// Without this, a frame that runs out of time would throw away everything
    /// it had parsed, and a slice shorter than one checkpoint could never
    /// finish one at all.
    partial: Option<Partial>,
    /// An upward extension of the cached window, carried across frames.
    prefix: Option<PrefixFill>,
}

/// A checkpoint under construction: the state so far, and the next line to feed
/// it.
struct Partial {
    next_line: usize,
    state: Checkpoint,
}

/// A window being parsed, or already parsed.
///
/// `styles` is sized for the whole window from the start; entries at or past
/// `filled_to` are placeholders until the frames that fill them arrive.
struct CachedWindow {
    lines: Range<usize>,
    styles: Vec<Vec<StyledRange>>,
    /// Absolute line index parsed up to. Equal to `lines.end` when complete.
    filled_to: usize,
    /// Parser state sitting at `filled_to`, kept while the window is unfinished.
    state: Option<Box<Checkpoint>>,
}

/// An upward extension of the cached window, in progress.
///
/// Scrolling up just past the top of the cached window keeps the parsed
/// suffix: parsing is deterministic from a checkpoint, so only the lines
/// between the new checkpoint and the old window top need parsing - one time
/// slice at a time, like every other fill here - instead of rebuilding the
/// whole padded window for a handful of new lines.
struct PrefixFill {
    /// The window under construction; `lines.start` is a checkpoint line.
    lines: Range<usize>,
    /// Styles for the prefix parsed so far, one entry per line from
    /// `lines.start`.
    styles: Vec<Vec<StyledRange>>,
    /// The next line to feed the parser.
    next_line: usize,
    /// Where the parsed suffix begins: the old window's top.
    suffix_start: usize,
    /// The old window's styles, appended once the prefix is done.
    suffix: Vec<Vec<StyledRange>>,
    /// How far the old window had been parsed; becomes the new window's
    /// `filled_to`.
    filled_to: usize,
    /// The old window's parser state at `filled_to`, kept while it is
    /// unfinished so filling carries on where it left off.
    suffix_state: Option<Box<Checkpoint>>,
    /// Parser state sitting at `next_line`.
    state: Checkpoint,
}

impl Default for PaneHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneHighlighter {
    pub fn new() -> Self {
        Self {
            syntax_name: None,
            theme_name: String::new(),
            checkpoints: Vec::new(),
            catching_up: false,
            cache: None,
            parsed_this_frame: 0,
            partial: None,
            prefix: None,
        }
    }

    pub fn syntax_name(&self) -> Option<&str> {
        self.syntax_name.as_deref()
    }

    /// True when highlighting is still catching up and the app should ask for
    /// another frame.
    pub fn is_catching_up(&self) -> bool {
        self.catching_up
    }

    /// Point the pane at a different language. No-op if unchanged.
    pub fn set_syntax(&mut self, name: Option<&str>) {
        if self.syntax_name.as_deref() != name {
            self.syntax_name = name.map(str::to_owned);
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.checkpoints.clear();
        self.cache = None;
        self.partial = None;
        self.prefix = None;
    }

    /// Discard the checkpoints covering `line` and everything after it.
    ///
    /// Called after an edit. Checkpoints *before* the edit are still valid,
    /// which is what makes typing deep inside a large file cheap.
    pub fn invalidate_from(&mut self, line: usize) {
        let keep = line / STRIDE;
        if keep < self.checkpoints.len() {
            self.checkpoints.truncate(keep);
            self.partial = None;
        } else if self.partial.as_ref().is_some_and(|p| p.next_line > line) {
            // The half-built checkpoint had already read past the edit.
            self.partial = None;
        }
        // The cached window is only good up to the edit.
        if self.cache.as_ref().is_some_and(|c| c.lines.end > line) {
            self.cache = None;
        }
        if self.prefix.as_ref().is_some_and(|p| p.lines.end > line) {
            self.prefix = None;
        }
    }

    /// Highlight `range`, returning one entry per line in it.
    ///
    /// Returns empty vectors (meaning "draw in the plain foreground colour")
    /// when highlighting is off, the file is too large, or the parser has not
    /// caught up to this part of the file yet.
    /// Lines fed to the parser during the last [`Self::highlight`] call.
    ///
    /// Exposed so the time slice can be checked by the work it lets through
    /// rather than by a stopwatch. See [`Self::parsed_this_frame`].
    pub fn parsed_last_frame(&self) -> usize {
        self.parsed_this_frame
    }

    pub fn highlight(
        &mut self,
        assets: &SyntaxAssets,
        theme_name: &str,
        lines: &[String],
        range: Range<usize>,
    ) -> Vec<Vec<StyledRange>> {
        let empty = || vec![Vec::new(); range.len()];
        self.parsed_this_frame = 0;

        let Some(syntax_name) = self.syntax_name.clone() else {
            return empty();
        };
        if lines.len() > MAX_HIGHLIGHT_LINES || range.is_empty() {
            // Not catching up: nothing is going to start, so no repaints either.
            self.catching_up = false;
            return empty();
        }
        // The byte cap, short-circuited: a file over it stops summing as soon
        // as the running total crosses, so this costs nothing per frame even
        // on the files it exists for.
        //
        // Only the bytes that would actually reach the parser are counted.
        // Summing the whole file measures the wrong thing: a 135 MB SQL dump
        // measured here holds 134.6 MB inside lines over
        // [`MAX_LINE_HIGHLIGHT_BYTES`], which are skipped anyway, leaving
        // 0.58 MB of real work. Counting the raw size switched highlighting
        // off for a file that could have been coloured for well under a
        // hundredth of the budget.
        let mut total = 0usize;
        if lines
            .iter()
            .any(|l| {
                if l.len() <= MAX_LINE_HIGHLIGHT_BYTES {
                    total += l.len();
                }
                total > MAX_HIGHLIGHT_BYTES
            })
        {
            self.catching_up = false;
            return empty();
        }
        let Some(syntax) = assets.by_name(&syntax_name) else {
            return empty();
        };

        // A theme change repaints everything, so rebuild from scratch.
        if self.theme_name != theme_name {
            self.theme_name = theme_name.to_owned();
            self.reset();
        }

        // Already parsed? Then this frame costs nothing.
        if let Some(hit) = self.cached(&range) {
            self.catching_up = false;
            return hit;
        }

        let theme = assets.theme(theme_name);
        let highlighter = Highlighter::new(theme);
        let deadline = Instant::now() + CATCHUP_SLICE;

        // The window in hand may already cover this range but not have reached
        // it yet; carry on filling it rather than starting over.
        if self.extend_window(&range, lines, &highlighter, &assets.syntaxes, deadline) {
            self.catching_up = false;
            return self
                .cached(&range)
                .unwrap_or_else(|| vec![Vec::new(); range.len()]);
        }
        if self.cache.as_ref().is_some_and(|c| {
            range.start >= c.lines.start && range.end <= c.lines.end
        }) {
            // Same window, still filling. Draw plain and come back next frame.
            self.catching_up = true;
            return empty();
        }

        // Parse a padded window, not just what was asked for, so the next few
        // hundred lines of scrolling are served from the cache.
        let wanted = range.clone();
        let range = range.start.saturating_sub(CACHE_PAD)
            ..(range.end + CACHE_PAD).min(lines.len());

        let end = range.end.min(lines.len());
        if range.start >= end {
            return empty();
        }

        // ---- 1. Extend the checkpoint chain toward the requested window ----
        let needed = range.start / STRIDE;
        self.catching_up = false;
        if self.checkpoints.is_empty() {
            self.checkpoints.push(Checkpoint {
                parse: ParseState::new(syntax),
                highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            });
        }

        // Carried across chunks, not reset per chunk.
        //
        // A chunk is STRIDE lines; on a file of short lines that is well under
        // CLOCK_EVERY_BYTES, so a per-chunk counter never reached the
        // threshold and the deadline below was never read at all. The slice
        // was bypassed entirely and one frame ran until the whole catch-up
        // finished - 672 ms on a 14 000-line file, the very freeze the slice
        // exists to prevent.
        let mut since_clock = 0usize;
        let mut since_lines = 0usize;

        while self.checkpoints.len() <= needed {
            let chunk_start = (self.checkpoints.len() - 1) * STRIDE;
            if chunk_start >= lines.len() {
                break;
            }
            let chunk_end = (chunk_start + STRIDE).min(lines.len());

            // Resume the checkpoint this ran out of time on last frame, or
            // start a fresh one from the previous checkpoint.
            let mut partial = self.partial.take().filter(|p| {
                (chunk_start..chunk_end).contains(&p.next_line)
            });
            let (mut cursor, mut cp) = match partial.take() {
                Some(p) => (p.next_line, p.state),
                None => (
                    chunk_start,
                    self.checkpoints.last().expect("pushed above").clone(),
                ),
            };

            while cursor < chunk_end {
                let len = lines[cursor].len().max(1);
                advance(&mut cp, &lines[cursor], &highlighter, &assets.syntaxes);
                self.parsed_this_frame += 1;
                cursor += 1;
                since_clock += len;
                since_lines += 1;

                if since_clock >= CLOCK_EVERY_BYTES || since_lines >= CLOCK_EVERY_LINES {
                    since_clock = 0;
                    since_lines = 0;
                    // Only stop if this chunk still has work left. Bailing out
                    // with the chunk *complete* would save a `next_line` equal
                    // to its end, which no chunk range contains - so the next
                    // frame would throw the whole chunk away and start it
                    // again. That alone cost 3.5x the necessary parsing.
                    if cursor < chunk_end && Instant::now() >= deadline {
                        self.partial = Some(Partial {
                            next_line: cursor,
                            state: cp,
                        });
                        self.catching_up = true;
                        return empty();
                    }
                }
            }

            self.checkpoints.push(cp);
        }

        let Some(start_cp) = self.checkpoints.get(needed) else {
            self.catching_up = true;
            return empty();
        };
        let cp_line = needed * STRIDE;

        // ---- 2. Reuse the parsed suffix when only the window top moved up ---
        //
        // Scrolling up out of the padded window used to throw it away and
        // re-parse the whole thing from the new checkpoint - 500-odd lines to
        // show a handful of new ones. The old window's styles are still valid,
        // so only the prefix between the new checkpoint and the old window top
        // is parsed, one slice at a time. A window that is still being filled
        // keeps its parser state at `filled_to`, so filling simply continues
        // once the prefix lands.
        //
        // Because a window top is checkpoint-aligned, one line of upward
        // scroll moves the padded start down by CACHE_PAD + 1 lines, so "just
        // past the top" means at most STRIDE + CACHE_PAD of prefix; anything
        // larger is a jump, which rebuilds.
        if self.prefix.as_ref().is_some_and(|p| {
            p.lines.start != cp_line || end > p.lines.end
        }) {
            self.prefix = None; // the scroll moved on; start over
        }
        if self.prefix.is_none()
            && self.cache.as_ref().is_some_and(|old| {
                cp_line < old.lines.start
                    && old.lines.start - cp_line <= STRIDE + CACHE_PAD
                    && end <= old.lines.end
            })
        {
            let old = self.cache.take().expect("checked above");
            self.prefix = Some(PrefixFill {
                lines: cp_line..old.lines.end,
                styles: Vec::new(),
                next_line: cp_line,
                suffix_start: old.lines.start,
                suffix: old.styles,
                filled_to: old.filled_to,
                suffix_state: old.state,
                state: start_cp.clone(),
            });
        }
        if self.prefix.is_some() {
            if !self.fill_prefix(lines, &highlighter, &assets.syntaxes, deadline) {
                self.catching_up = true;
                return empty();
            }
            let pre = self.prefix.take().expect("checked above");
            let mut styles = pre.styles;
            styles.extend(pre.suffix);
            self.cache = Some(CachedWindow {
                filled_to: pre.filled_to,
                state: pre.suffix_state,
                styles,
                lines: pre.lines,
            });
            // The old window may not have been filled up to `wanted` yet.
            let complete =
                self.extend_window(&wanted, lines, &highlighter, &assets.syntaxes, deadline);
            if !complete {
                self.catching_up = true;
                return empty();
            }
            self.catching_up = false;
            return self
                .cached(&wanted)
                .unwrap_or_else(|| vec![Vec::new(); wanted.len()]);
        }

        // ---- 3. Start the window at the checkpoint itself -------------------
        //
        // The lines between the checkpoint and the window used to be re-parsed
        // in one go, and that was the last unbounded stretch in this function -
        // up to `STRIDE` lines, tens of milliseconds, in whichever frame
        // happened to ask. Making the window start at the checkpoint folds
        // those lines into the time-sliced fill below, and their colours end up
        // cached rather than thrown away.
        let cp = start_cp.clone();
        let span = cp_line..end;
        self.cache = Some(CachedWindow {
            styles: vec![Vec::new(); span.len()],
            filled_to: span.start,
            lines: span,
            state: Some(Box::new(cp)),
        });

        let complete = self.extend_window(
            &wanted,
            lines,
            &highlighter,
            &assets.syntaxes,
            deadline,
        );
        if !complete {
            self.catching_up = true;
            return empty();
        }
        self.cached(&wanted).unwrap_or_else(|| vec![Vec::new(); wanted.len()])
    }

    /// Serve `range` out of the cached window, if it is inside the part that
    /// has actually been parsed.
    fn cached(&self, range: &Range<usize>) -> Option<Vec<Vec<StyledRange>>> {
        let cache = self.cache.as_ref()?;
        if range.start < cache.lines.start || range.end > cache.filled_to {
            return None;
        }
        let from = range.start - cache.lines.start;
        Some(cache.styles[from..from + range.len()].to_vec())
    }

    /// Spend up to one slice extending the current window, if there is one that
    /// covers `wanted` but has not reached the end of it yet.
    ///
    /// Returns true when the window now covers `wanted`.
    fn extend_window(
        &mut self,
        wanted: &Range<usize>,
        lines: &[String],
        highlighter: &Highlighter<'_>,
        syntaxes: &SyntaxSet,
        deadline: Instant,
    ) -> bool {
        let Some(cache) = self.cache.as_mut() else {
            return false;
        };
        if wanted.start < cache.lines.start || wanted.end > cache.lines.end {
            return false; // a different window is needed entirely
        }
        let Some(state) = cache.state.as_mut() else {
            return cache.filled_to >= wanted.end;
        };

        let mut since_clock = 0usize;
        let mut since_lines = 0usize;
        while cache.filled_to < cache.lines.end {
            let index = cache.filled_to - cache.lines.start;
            let len = lines[cache.filled_to].len().max(1);
            cache.styles[index] = styles_for(state, &lines[cache.filled_to], highlighter, syntaxes);
            self.parsed_this_frame += 1;
            cache.filled_to += 1;
            since_clock += len;
            since_lines += 1;

            if since_clock >= CLOCK_EVERY_BYTES || since_lines >= CLOCK_EVERY_LINES {
                since_clock = 0;
                since_lines = 0;
                if cache.filled_to < cache.lines.end && Instant::now() >= deadline {
                    break;
                }
            }
        }
        if cache.filled_to >= cache.lines.end {
            cache.state = None; // finished; the parser state is no longer needed
        }
        cache.filled_to >= wanted.end
    }

    /// Spend up to one slice parsing the prefix of an upward extension.
    ///
    /// Returns true when the prefix is complete and the window can be
    /// assembled from it and the old window's styles.
    fn fill_prefix(
        &mut self,
        lines: &[String],
        highlighter: &Highlighter<'_>,
        syntaxes: &SyntaxSet,
        deadline: Instant,
    ) -> bool {
        let Some(pre) = self.prefix.as_mut() else {
            return false;
        };
        let mut since_clock = 0usize;
        let mut since_lines = 0usize;
        while pre.next_line < pre.suffix_start {
            let len = lines[pre.next_line].len().max(1);
            pre.styles.push(styles_for(
                &mut pre.state,
                &lines[pre.next_line],
                highlighter,
                syntaxes,
            ));
            self.parsed_this_frame += 1;
            pre.next_line += 1;
            since_clock += len;
            since_lines += 1;

            if since_clock >= CLOCK_EVERY_BYTES || since_lines >= CLOCK_EVERY_LINES {
                since_clock = 0;
                since_lines = 0;
                if pre.next_line < pre.suffix_start && Instant::now() >= deadline {
                    break;
                }
            }
        }
        pre.next_line >= pre.suffix_start
    }
}

/// Advance the parser past a line without collecting its styles.
fn advance(cp: &mut Checkpoint, line: &str, hl: &Highlighter<'_>, syntaxes: &SyntaxSet) {
    // An over-long line would cost more than the whole time slice in one
    // uninterruptible call; the state simply carries over it.
    if line.len() > MAX_LINE_HIGHLIGHT_BYTES {
        return;
    }
    // A malformed syntax definition can make `parse_line` fail; treat that as
    // "no scopes on this line" rather than losing highlighting for the file.
    let Ok(ops) = cp.parse.parse_line(line, syntaxes) else {
        return;
    };
    // Running the highlight iterator is what advances `cp.highlight`, so it is
    // needed even though the styles are dropped.
    for _ in RangedHighlightIterator::new(&mut cp.highlight, &ops, line, hl) {}
}

/// Advance the parser across a line *and* return its styled runs.
fn styles_for(
    cp: &mut Checkpoint,
    line: &str,
    hl: &Highlighter<'_>,
    syntaxes: &SyntaxSet,
) -> Vec<StyledRange> {
    // Same guard as `advance`: over-long lines are drawn uncoloured.
    if line.len() > MAX_LINE_HIGHLIGHT_BYTES {
        return Vec::new();
    }
    let Ok(ops) = cp.parse.parse_line(line, syntaxes) else {
        return Vec::new();
    };
    RangedHighlightIterator::new(&mut cp.highlight, &ops, line, hl)
        .filter(|(_, text, _)| !text.is_empty())
        .map(|(style, _, range)| StyledRange {
            range,
            color: Color32::from_rgb(
                style.foreground.r,
                style.foreground.g,
                style.foreground.b,
            ),
            italic: style.font_style.contains(FontStyle::ITALIC),
            bold: style.font_style.contains(FontStyle::BOLD),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> Arc<SyntaxAssets> {
        SyntaxAssets::load()
    }

    /// Drive a highlighter to completion, as the application does by asking for
    /// another frame whenever `is_catching_up` is set.
    fn settle(
        h: &mut PaneHighlighter,
        a: &SyntaxAssets,
        theme: &str,
        lines: &[String],
        range: Range<usize>,
    ) -> Vec<Vec<StyledRange>> {
        let mut out = h.highlight(a, theme, lines, range.clone());
        for _ in 0..2_000 {
            if !h.is_catching_up() {
                return out;
            }
            out = h.highlight(a, theme, lines, range.clone());
        }
        panic!("highlighting never settled");
    }

    fn rust_lines(n: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(match i % 4 {
                0 => format!("fn function_{i}() -> u32 {{"),
                1 => format!("    let value = {i}; // a comment"),
                2 => "    value + 1".to_owned(),
                _ => "}".to_owned(),
            });
        }
        out
    }

    #[test]
    fn plain_text_produces_no_styles() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        let out = h.highlight(&a, "base16-ocean.dark", &rust_lines(4), 0..4);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(Vec::is_empty), "no syntax was selected");
    }

    #[test]
    fn rust_keywords_and_comments_get_distinct_colours() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let lines = rust_lines(4);
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..4);

        assert!(!out[0].is_empty(), "the `fn` line should be highlighted");
        let colours: std::collections::HashSet<_> =
            out[1].iter().map(|s| s.color.to_array()).collect();
        assert!(
            colours.len() > 1,
            "a line with a comment should not be one flat colour"
        );
    }

    #[test]
    fn styled_ranges_tile_the_line_without_gaps_or_overlap() {
        let a = assets();
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let lines = rust_lines(8);
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..8);

        for (i, spans) in out.iter().enumerate() {
            if spans.is_empty() {
                continue;
            }
            for w in spans.windows(2) {
                assert!(
                    w[0].range.end <= w[1].range.start,
                    "line {i} has overlapping styles"
                );
            }
            let last = spans.last().expect("non-empty");
            assert!(
                last.range.end <= lines[i].len(),
                "line {i} style runs past the end of the line"
            );
            // Every range must be sliceable, or rendering panics.
            for s in spans {
                assert!(lines[i].is_char_boundary(s.range.start));
                assert!(lines[i].is_char_boundary(s.range.end));
            }
        }
    }

    /// The core promise: a window deep in a large file must produce the same
    /// colours as parsing that file straight through.
    #[test]
    fn a_window_deep_in_the_file_matches_a_sequential_parse() {
        let a = assets();
        let lines = rust_lines(2000);
        let window = 1500..1520;

        let mut jumped = PaneHighlighter::new();
        jumped.set_syntax(Some("Rust"));
        // Give it enough frames to build the checkpoint chain.
        let from_jump = settle(&mut jumped, &a, "base16-ocean.dark", &lines, window.clone());

        let mut sequential = PaneHighlighter::new();
        sequential.set_syntax(Some("Rust"));
        let all = settle(&mut sequential, &a, "base16-ocean.dark", &lines, 0..lines.len());

        assert_eq!(from_jump, all[window], "checkpoint rewind changed the result");
        assert!(!from_jump.iter().all(Vec::is_empty));
    }

    #[test]
    fn multi_line_constructs_survive_a_checkpoint_boundary() {
        let a = assets();
        // A block comment that spans a checkpoint boundary: the state carried
        // across the checkpoint is exactly what could go wrong here.
        let mut lines = vec!["fn main() {}".to_owned()];
        lines.push("/* opening a long block comment".to_owned());
        lines.extend((0..STRIDE + 50).map(|i| format!("still inside the comment {i}")));
        lines.push("*/".to_owned());
        lines.push("fn after() {}".to_owned());

        let target = lines.len() - 3;
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = settle(&mut h, &a, "base16-ocean.dark", &lines, target..target + 1);

        let mut seq = PaneHighlighter::new();
        seq.set_syntax(Some("Rust"));
        let all = settle(&mut seq, &a, "base16-ocean.dark", &lines, 0..lines.len());
        assert_eq!(out[0], all[target], "comment state was lost at a checkpoint");
    }

    #[test]
    fn editing_only_discards_checkpoints_after_the_edit() {
        let a = assets();
        let lines = rust_lines(3000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        settle(&mut h, &a, "base16-ocean.dark", &lines, 2500..2520);
        let before = h.checkpoints.len();
        assert!(before > 4);

        h.invalidate_from(2000);
        assert_eq!(h.checkpoints.len(), 2000 / STRIDE);
        assert!(
            h.checkpoints.len() < before,
            "checkpoints after the edit must go"
        );

        h.invalidate_from(0);
        assert!(h.checkpoints.is_empty());
    }

    /// Scrolling through a file must give the same colours as parsing it
    /// straight through - the cache must never serve a stale or misaligned
    /// window.
    #[test]
    fn scrolling_across_the_cache_matches_a_sequential_parse() {
        let a = assets();
        let lines = rust_lines(3_000);

        let mut sequential = PaneHighlighter::new();
        sequential.set_syntax(Some("Rust"));
        let all = settle(&mut sequential, &a, "base16-ocean.dark", &lines, 0..lines.len());

        let mut scrolled = PaneHighlighter::new();
        scrolled.set_syntax(Some("Rust"));
        // Walk down in small steps, as dragging a scrollbar would, crossing the
        // padded window boundary many times.
        for top in (0..2_800).step_by(37) {
            let window = top..top + 60;
            let got = settle(&mut scrolled, &a, "base16-ocean.dark", &lines, window.clone());
            assert_eq!(got, all[window.clone()], "window {window:?} disagreed");
        }
        // ...and back up, which crosses window tops and so exercises the
        // suffix-reusing upward extension.
        for top in (0..2_800).rev().step_by(37) {
            let window = top..top + 60;
            let got = settle(&mut scrolled, &a, "base16-ocean.dark", &lines, window.clone());
            assert_eq!(got, all[window.clone()], "window {window:?} disagreed");
        }
    }

    /// Scrolling up just past the top of the cached window reuses its parsed
    /// suffix: the lines below stay parsed, so scrolling back down costs
    /// nothing.
    #[test]
    fn scrolling_up_a_little_reuses_the_parsed_suffix() {
        let a = assets();
        let lines = rust_lines(3_000);

        let mut sequential = PaneHighlighter::new();
        sequential.set_syntax(Some("Rust"));
        let all = settle(&mut sequential, &a, "base16-ocean.dark", &lines, 0..lines.len());

        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        // A window whose padded cache starts at a checkpoint well past 0.
        let home = 1_000..1_060;
        settle(&mut h, &a, "base16-ocean.dark", &lines, home.clone());
        let cache_start = h.cache.as_ref().expect("cached").lines.start;
        assert!(cache_start > 0);

        // Scroll up just past the top of the cache.
        let up = cache_start - 10..cache_start + 50;
        let got = settle(&mut h, &a, "base16-ocean.dark", &lines, up.clone());
        assert_eq!(got, all[up.clone()], "reused window disagreed");

        // The parsed suffix must survive: scrolling back down to the original
        // window is served from the cache without parsing a single line.
        let mut parsed = 0;
        let got = loop {
            let out = h.highlight(&a, "base16-ocean.dark", &lines, home.clone());
            parsed += h.parsed_last_frame();
            if !h.is_catching_up() {
                break out;
            }
        };
        assert_eq!(got, all[home.clone()]);
        assert_eq!(parsed, 0, "the old window's parsed suffix was thrown away");
    }

    #[test]
    fn an_edit_drops_a_cache_that_covers_it() {
        let a = assets();
        let lines = rust_lines(2_000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));

        settle(&mut h, &a, "base16-ocean.dark", &lines, 1_000..1_060);
        assert!(h.cache.is_some(), "a window should have been cached");

        // An edit past the window leaves it alone...
        h.invalidate_from(1_900);
        assert!(h.cache.is_some(), "an edit past the window is irrelevant");

        // ...one inside it does not.
        h.invalidate_from(1_010);
        assert!(h.cache.is_none(), "the cache outlived an edit inside it");
    }

    #[test]
    fn a_theme_change_is_not_served_from_the_cache() {
        let a = assets();
        let lines = rust_lines(500);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));

        let dark = settle(&mut h, &a, "base16-ocean.dark", &lines, 0..60);
        let light = settle(&mut h, &a, "InspiredGitHub", &lines, 0..60);
        assert_ne!(dark, light, "the theme change was served from a stale cache");
    }

    #[test]
    fn changing_the_language_is_not_served_from_the_cache() {
        let a = assets();
        let lines = rust_lines(500);
        let mut h = PaneHighlighter::new();

        h.set_syntax(Some("Rust"));
        let as_rust = settle(&mut h, &a, "base16-ocean.dark", &lines, 0..40);
        h.set_syntax(Some("Python"));
        let as_python = settle(&mut h, &a, "base16-ocean.dark", &lines, 0..40);
        assert_ne!(as_rust, as_python);
    }

    /// The reason the cache exists.
    ///
    /// Redrawing an already-parsed window used to rewind to the nearest
    /// checkpoint and re-parse on every frame: 60 ms per pane on a 14 000-line
    /// file, against a 16 ms budget for the whole frame.
    #[test]
    fn redrawing_a_parsed_window_is_effectively_free() {
        use std::time::{Duration, Instant};

        let a = assets();
        let lines = rust_lines(14_000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));

        settle(&mut h, &a, "base16-ocean.dark", &lines, 9_000..9_060);

        let start = Instant::now();
        for i in 0..30 {
            h.highlight(&a, "base16-ocean.dark", &lines, 9_000 + i..9_060 + i);
        }
        let per_frame = start.elapsed() / 30;

        eprintln!("redraw with a warm cache: {per_frame:?} per pane");
        assert!(
            per_frame < Duration::from_millis(2),
            "redrawing cost {per_frame:?} per pane; the cache is not being hit"
        );
    }

    /// Catching up must not redo work it has already done.
    ///
    /// The time slice originally saved its progress *after* incrementing the
    /// cursor, so a chunk that finished exactly as the deadline fired stored a
    /// position no chunk range contained - and the next frame started that
    /// chunk again. It cost 3.5x the necessary parsing and turned a 2-second
    /// catch-up into 13.
    #[test]
    fn catching_up_makes_steady_progress() {
        let a = assets();
        let lines = rust_lines(4_000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));

        let mut frames = 0;
        while h.is_catching_up() || frames == 0 {
            h.highlight(&a, "base16-ocean.dark", &lines, 3_000..3_060);
            frames += 1;
            assert!(frames < 400, "catch-up is not converging");
        }

        // Every frame must have advanced the chain; if any threw its work away
        // the count would be far higher than the lines actually needed.
        assert!(
            h.checkpoints.len() * STRIDE >= 2_816,
            "the checkpoint chain did not reach the window"
        );
        eprintln!("caught up to line 3000 in {frames} frames");
    }

    /// A single frame must never block, however far the jump.
    #[test]
    fn no_single_frame_does_an_unbounded_amount_of_parsing() {
        use std::time::Instant;

        let a = assets();
        let lines = rust_lines(14_000);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));

        // Bounded by the clock, because the clock is what the slice rations.
        //
        // This was briefly asserted on lines parsed instead, on the theory that
        // work does not move with machine load the way a stopwatch does. That
        // was wrong twice over: lines per frame is exactly what varies with
        // machine speed - this machine fits over 9 000 trivial lines into 8 ms -
        // and the regression being guarded against, a budget of 4 096 *lines*,
        // did less work per frame than a healthy run does here. Work cannot
        // separate the two; only time can.
        //
        // The threshold is 30x the slice, far above any plausible scheduling
        // hiccup and far below the 661 ms freeze the line-based budget caused.
        const TOO_LONG: Duration = Duration::from_millis(250);

        // One untimed call first. The very first highlight of a document also
        // builds the theme's selector tables and warms the allocator - measured
        // at 59 ms here against 10 ms for every frame after it. That is a
        // one-off, not a stalled frame, and leaving it in made this assertion
        // fail only when the suite ran in parallel: it was measuring start-up
        // under load, not the time slice.
        h.highlight(&a, "base16-ocean.dark", &lines, 9_000..9_060);

        let mut frames = 0;
        let mut worst = Duration::ZERO;
        let mut worst_at = 0;
        let mut total = Duration::ZERO;
        loop {
            let start = Instant::now();
            h.highlight(&a, "base16-ocean.dark", &lines, 9_000..9_060);
            let took = start.elapsed();
            total += took;
            if took > worst {
                worst = took;
                worst_at = frames;
            }
            frames += 1;

            assert!(
                took < TOO_LONG,
                "one frame spent {took:?} parsing; a phase is ignoring the time slice"
            );
            if !h.is_catching_up() || frames > 600 {
                break;
            }
        }

        eprintln!(
            "caught up in {frames} frames, worst {worst:?} at frame {worst_at}, {:?} average",
            total / frames
        );
        assert!(frames > 1, "the work was not spread over frames at all");
    }

    #[test]
    fn a_huge_file_is_skipped_rather_than_stalling() {
        let a = assets();
        let lines = vec!["fn x() {}".to_owned(); MAX_HIGHLIGHT_LINES + 1];
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..10);
        assert!(out.iter().all(Vec::is_empty));
    }

    /// A dump is enormous but almost none of it is parsed, so it must still
    /// be coloured.
    ///
    /// The byte cap used to sum the whole file. On a real 135 MB SQL dump that
    /// was 134.6 MB of lines over [`MAX_LINE_HIGHLIGHT_BYTES`] - lines the
    /// parser skips anyway - leaving 0.58 MB of actual work, and highlighting
    /// was switched off for the entire file on the strength of bytes nobody
    /// was ever going to parse.
    #[test]
    fn giant_lines_do_not_count_toward_the_document_byte_cap() {
        let a = assets();
        let mut lines = rust_lines(40);
        // Enough over-long lines to blow the cap several times over.
        let giant = "x".repeat(MAX_LINE_HIGHLIGHT_BYTES + 1);
        for _ in 0..(4 * MAX_HIGHLIGHT_BYTES / giant.len()) {
            lines.push(giant.clone());
        }
        let raw: usize = lines.iter().map(String::len).sum();
        assert!(raw > MAX_HIGHLIGHT_BYTES, "fixture is not over the cap");

        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let mut out = h.highlight(&a, "base16-ocean.dark", &lines, 0..4);
        for _ in 0..20 {
            if !h.is_catching_up() {
                break;
            }
            out = h.highlight(&a, "base16-ocean.dark", &lines, 0..4);
        }
        assert!(
            out.iter().any(|s| !s.is_empty()),
            "a {raw}-byte document was left uncoloured although only \
             {} bytes of it are parseable",
            lines
                .iter()
                .filter(|l| l.len() <= MAX_LINE_HIGHLIGHT_BYTES)
                .map(String::len)
                .sum::<usize>()
        );
    }

    /// The cap still has to fire on a document that really is all parseable.
    #[test]
    fn a_document_of_parseable_bytes_over_the_cap_is_skipped() {
        let a = assets();
        let line = "let x = 1; // ".to_owned() + &"y".repeat(1_000);
        let lines = vec![line.clone(); MAX_HIGHLIGHT_BYTES / line.len() + 2];
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 0..4);
        assert!(out.iter().all(Vec::is_empty));
    }

    #[test]
    fn a_window_past_the_end_of_the_file_is_padded() {
        let a = assets();
        let lines = rust_lines(5);
        let mut h = PaneHighlighter::new();
        h.set_syntax(Some("Rust"));
        let out = h.highlight(&a, "base16-ocean.dark", &lines, 3..13);
        assert_eq!(out.len(), 10, "the caller expects one entry per row");
    }

    #[test]
    fn detection_by_extension_and_shebang() {
        let a = assets();
        assert_eq!(
            a.detect(Some(std::path::Path::new("main.rs")), "").name,
            "Rust"
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("a.py")), "").name,
            "Python"
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("data.json")), "").name,
            "JSON"
        );
        let by_line = a.detect(None, "#!/bin/bash");
        assert!(
            by_line.name.contains("Bash") || by_line.name.contains("Shell"),
            "shebang detection gave {}",
            by_line.name
        );
        assert_eq!(
            a.detect(Some(std::path::Path::new("notes.unknownext")), "").name,
            a.plain_text_name()
        );
    }

    /// The languages the brief calls out by name must all be available.
    #[test]
    fn the_expected_languages_are_available() {
        let a = assets();
        for name in [
            "Rust",
            "Python",
            "JavaScript",
            "JSON",
            "Markdown",
            "YAML",
            "HTML",
            "C++",
            "Go",
            "SQL",
            "XML",
        ] {
            assert!(a.by_name(name).is_some(), "missing syntax: {name}");
        }
        assert!(a.language_names().len() > 50);
    }

    #[test]
    fn both_palette_themes_exist() {
        let a = assets();
        for p in [
            crate::ui::theme::Palette::light(),
            crate::ui::theme::Palette::dark(),
        ] {
            assert!(
                a.themes.themes.contains_key(p.syntect_theme),
                "syntect has no theme named {}",
                p.syntect_theme
            );
        }
    }
}
