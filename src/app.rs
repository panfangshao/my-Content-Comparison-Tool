//! The application: state, the frame loop, and everything that wires the
//! pieces together.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, RichText, Vec2};

use crate::config::Config;
use crate::core::diff::{
    Budget, DiffResult, Side, diff_lines, to_unified, UnifiedOptions,
};
use crate::core::merge::{self, Direction};
use crate::core::text::{FileEncoding, TextBuffer, cleanup, encoding};
use crate::i18n::{Lang, t, tf};
use crate::ui::editor::{EditorStyle, InlineCache, PaneParams, show_pane};
use crate::ui::find::{FindAction, FindState, show_find_bar};
use crate::ui::gutter::{self, GutterParams};
use crate::ui::highlight::{PaneHighlighter, StyledRange, SyntaxAssets};
use crate::ui::overview::{OverviewParams, show_overview};
use crate::ui::rowlayout::RowLayout;
use crate::ui::statusbar::{StatusFacts, show_status_bar};
use crate::ui::theme::{Appearance, Palette, StyleKey};
use crate::ui::viewport::{Reveal, Viewport};
use crate::ui::toolbar::{Action, MenuFacts, show_toolbar};

/// How long after the last keystroke before the comparison re-runs.
///
/// Long enough that a fast typist never waits on a diff mid-word, short enough
/// that pausing feels instant.
const DIFF_DEBOUNCE: Duration = Duration::from_millis(90);

/// Above this many lines the debounce stretches, because the comparison itself
/// costs more than the delay.
const LARGE_DOC_LINES: usize = 20_000;
const LARGE_DEBOUNCE: Duration = Duration::from_millis(280);

/// How long a status message stays on screen.
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// One side of the comparison.
pub struct Pane {
    pub buffer: TextBuffer,
    pub path: Option<PathBuf>,
    pub encoding: FileEncoding,
    pub highlighter: PaneHighlighter,
    /// Language chosen by hand, overriding detection.
    pub syntax_override: Option<String>,
    /// Scroll offset as of the last frame, for detecting who scrolled.
    last_offset: Vec2,
    /// Rows this pane drew last frame. Highlighting for frame N is prepared
    /// from frame N-1's viewport, which is exact except on the frame a jump
    /// lands - and that one frame simply renders uncoloured.
    last_visible: std::ops::Range<usize>,
    /// Column the caret is trying to hold while moving vertically.
    goal_column: Option<usize>,
    /// Longest line in characters, and the document version it was measured
    /// at. Sizing the horizontal scrollbar needs this, and measuring it is
    /// O(document) - far too expensive to redo on every frame.
    longest_line: usize,
    longest_line_version: Option<u64>,
}

impl Default for Pane {
    fn default() -> Self {
        Self {
            buffer: TextBuffer::new(),
            path: None,
            encoding: FileEncoding::default(),
            highlighter: PaneHighlighter::new(),
            syntax_override: None,
            last_offset: Vec2::ZERO,
            last_visible: 0..0,
            goal_column: None,
            longest_line: 0,
            longest_line_version: None,
        }
    }
}

impl Pane {
    /// Longest line in characters, remeasured only when the document changes.
    fn longest_line(&mut self) -> usize {
        let version = self.buffer.version();
        if self.longest_line_version != Some(version) {
            self.longest_line = self
                .buffer
                .lines()
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            self.longest_line_version = Some(version);
        }
        self.longest_line
    }

    fn buffer_mut(&mut self) -> &mut TextBuffer {
        &mut self.buffer
    }

    fn title(&self, lang: Lang) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| t(lang, "app.untitled").to_owned());
        if self.buffer.is_dirty() {
            format!("{name} \u{2022}")
        } else {
            name
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    at: Instant,
}

/// A yes/no question that must be answered before the action proceeds.
struct Confirm {
    prompt: String,
    action: Action,
}

pub struct DuiBi {
    config: Config,
    lang: Lang,
    palette: Palette,
    appearance: Appearance,
    /// The style inputs the egui context was last built from. `None` until the
    /// first build. Compared against a freshly derived [`StyleKey`] each frame,
    /// so any change to the theme or the language takes effect immediately.
    applied_style: Option<StyleKey>,

    left: Pane,
    right: Pane,
    focus_side: Side,
    /// Ask the named pane to take keyboard focus next frame.
    focus_request: Option<Side>,

    diff: DiffResult,
    /// One entry per hunk for the status bar's jump list, rebuilt with the
    /// comparison rather than on every frame.
    summaries: Vec<crate::core::diff::HunkSummary>,
    diff_generation: u64,
    diff_dirty: bool,
    dirty_since: Instant,
    last_compare: Duration,
    /// Versions the current comparison was computed from.
    compared_versions: (u64, u64),

    layout: RowLayout,
    inline: InlineCache,
    syntax: Arc<SyntaxAssets>,

    find: FindState,
    toast: Option<Toast>,
    confirm: Option<Confirm>,
    show_shortcuts: bool,

    /// Where the two panes are looking, and anything about to move them.
    viewport: Viewport,
    /// Height of one pane, recorded while drawing so a jump knows how much of
    /// the document fits on screen.
    viewport_height: f32,
    focused_hunk: Option<usize>,

    /// Screen x where the left pane ends and the right one begins, recorded
    /// each frame so a dropped file lands on the half it was dropped over.
    pane_boundary_x: f32,
    /// Pointer x while files are hovering over the window. Captured during the
    /// hover because by the time the drop event arrives the pointer position
    /// is often already gone.
    hover_drop_x: Option<f32>,
}

impl DuiBi {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        install_fonts(&cc.egui_ctx);

        let mut app = Self {
            lang: config.lang,
            palette: Palette::dark(),
            appearance: Appearance::Dark,
            applied_style: None,
            config,
            left: Pane::default(),
            right: Pane::default(),
            focus_side: Side::Left,
            // Focus the left pane immediately: having to click before you can
            // type is a papercut, and it also means keyboard shortcuts that
            // act on "the focused side" work from the first keystroke.
            focus_request: Some(Side::Left),
            diff: DiffResult::default(),
            summaries: Vec::new(),
            diff_generation: 0,
            diff_dirty: true,
            dirty_since: Instant::now(),
            last_compare: Duration::ZERO,
            compared_versions: (u64::MAX, u64::MAX),
            layout: RowLayout::default(),
            inline: InlineCache::default(),
            syntax: SyntaxAssets::load(),
            find: FindState::default(),
            toast: None,
            confirm: None,
            show_shortcuts: false,
            viewport: Viewport::default(),
            viewport_height: 600.0,
            focused_hunk: None,
            pane_boundary_x: f32::INFINITY,
            hover_drop_x: None,
        };
        app.restyle_if_needed(&cc.egui_ctx);
        app
    }

    /// Load files named on the command line.
    pub fn open_at_startup(&mut self, left: Option<&Path>, right: Option<&Path>) {
        if let Some(p) = left {
            self.open_into(Side::Left, p);
        }
        if let Some(p) = right {
            self.open_into(Side::Right, p);
        }
        // Startup loads should not leave a "just opened" toast on screen.
        self.toast = None;
    }

    fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            kind: ToastKind::Info,
            at: Instant::now(),
        });
    }

    fn notify_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            kind: ToastKind::Error,
            at: Instant::now(),
        });
    }

    // ---- Theme ---------------------------------------------------------

    /// Rebuild the egui style if anything it depends on has moved.
    ///
    /// Everything that feeds the style lives in [`StyleKey`], so this is the
    /// single place that has to notice a change - adding a new style input
    /// means extending the key, not remembering to extend a condition here.
    fn restyle_if_needed(&mut self, ctx: &egui::Context) {
        let key = StyleKey::new(self.config.theme, self.config.lang, ctx.system_theme());
        if self.applied_style == Some(key) {
            return;
        }
        self.appearance = key.appearance;
        self.lang = key.lang;
        self.palette = Palette::of(key.appearance);
        ctx.set_visuals(self.palette.visuals(key.appearance.is_dark()));
        self.applied_style = Some(key);
    }

    fn editor_style(&self) -> EditorStyle {
        EditorStyle {
            font: egui::FontId::monospace(self.config.font_size),
            // Rounded to a whole pixel: half-pixel row heights make text shimmer
            // as you scroll.
            line_height: (self.config.font_size * self.config.line_height).round(),
            show_line_numbers: self.config.show_line_numbers,
            show_whitespace: self.config.show_whitespace,
            word_wrap: self.config.word_wrap,
            tab_width: crate::ui::tabs::TAB_WIDTH,
            vertical_motion: if self.config.caret_to_line_end {
                crate::ui::editing::VerticalMotion::LineEnd
            } else {
                crate::ui::editing::VerticalMotion::KeepColumn
            },
            gutter_width: 0.0,
        }
    }

    // ---- Comparison ----------------------------------------------------

    fn debounce(&self) -> Duration {
        let big = self.left.buffer.len_lines().max(self.right.buffer.len_lines())
            > LARGE_DOC_LINES;
        if big { LARGE_DEBOUNCE } else { DIFF_DEBOUNCE }
    }

    /// Re-run the comparison if the documents or the options have moved on.
    fn maybe_recompare(&mut self, ctx: &egui::Context) {
        let versions = (self.left.buffer.version(), self.right.buffer.version());
        if versions != self.compared_versions {
            if !self.diff_dirty {
                self.dirty_since = Instant::now();
            }
            self.diff_dirty = true;
        }
        if !self.diff_dirty {
            return;
        }
        if self.dirty_since.elapsed() < self.debounce() {
            // Come back when the debounce expires rather than spinning.
            ctx.request_repaint_after(self.debounce() - self.dirty_since.elapsed());
            return;
        }

        // Remember the line at the top of the view before the row list is
        // rebuilt. A re-comparison can add or drop filler rows *above* the
        // viewport - one inserted line higher up shifts everything below it -
        // and the scroll offset is measured in pixels, not lines. Without an
        // anchor the text slides out from under whatever the user was reading.
        let line_h = self.editor_style().line_height;
        let anchor = self
            .viewport
            .capture_anchor(&self.diff, &self.layout, self.focus_side, line_h);

        let started = Instant::now();
        self.diff = if self.config.single_pane {
            // Nothing to compare against; just lay the document out.
            crate::core::diff::single_document(self.left.buffer.len_lines())
        } else {
            diff_lines(
                self.left.buffer.lines(),
                self.right.buffer.lines(),
                &self.config.diff,
                Budget::default(),
            )
        };
        self.summaries = self
            .diff
            .hunk_summaries(self.left.buffer.lines(), self.right.buffer.lines());
        self.last_compare = started.elapsed();
        self.compared_versions = versions;
        self.diff_dirty = false;
        self.diff_generation = self.diff_generation.wrapping_add(1);
        self.layout = if self.config.word_wrap {
            RowLayout::wrapped(self.diff.rows.len())
        } else {
            RowLayout::uniform(self.diff.rows.len())
        };

        // Put that line back where it was.
        self.viewport
            .restore_anchor(anchor, &self.diff, &self.layout, line_h);
    }

    /// Options changed: force a comparison on the next tick.
    fn invalidate_diff(&mut self) {
        if !self.diff_dirty {
            self.dirty_since = Instant::now();
        }
        self.diff_dirty = true;
        self.compared_versions = (u64::MAX, u64::MAX);
    }

    // ---- Navigation ----------------------------------------------------

    fn caret_row(&self, side: Side) -> usize {
        let line = self.pane(side).buffer.line_of_char(self.pane(side).buffer.selection().head);
        self.diff.row_of(side, line).unwrap_or(0)
    }

    fn goto_hunk(&mut self, index: usize) {
        let Some(hunk) = self.diff.hunks.get(index) else {
            return;
        };
        let row = hunk.rows.start;
        self.focused_hunk = Some(index);
        let line_h = self.editor_style().line_height;
        self.viewport.scroll_to_row(
            row,
            Reveal::Centre,
            &self.layout,
            line_h,
            self.viewport_height,
        );

        // Put the caret on the hunk so the next Tab/typing lands where the user
        // is looking, and so "diff N of M" in the status bar agrees.
        let side = self.focus_side;
        let lines = hunk.lines(side);
        let line = if lines.is_empty() {
            lines.start.saturating_sub(1)
        } else {
            lines.start
        };
        let last_line = self.pane(side).buffer.len_lines().saturating_sub(1);
        let buffer = &mut self.pane_mut(side).buffer;
        let at = buffer.line_start(line.min(last_line));
        buffer.set_selection(crate::core::text::Selection::at(at));
    }

    fn next_diff(&mut self) {
        let row = self.caret_row(self.focus_side);
        let next = self
            .diff
            .hunk_after(row)
            .or(if self.diff.hunks.is_empty() { None } else { Some(0) });
        if let Some(i) = next {
            self.goto_hunk(i);
        }
    }

    fn prev_diff(&mut self) {
        let row = self.caret_row(self.focus_side);
        let prev = self.diff.hunk_before(row).or_else(|| {
            self.diff.hunks.len().checked_sub(1)
        });
        if let Some(i) = prev {
            self.goto_hunk(i);
        }
    }

    // ---- Files ---------------------------------------------------------

    fn open_into(&mut self, side: Side, path: &Path) {
        match encoding::read_file(path) {
            Ok(decoded) => {
                let lossy = decoded.encoding.lossy;
                {
                    let pane = self.pane_mut(side);
                    pane.buffer.load_text(&decoded.text);
                    pane.encoding = decoded.encoding;
                    pane.path = Some(path.to_path_buf());
                    pane.syntax_override = None;
                    pane.highlighter.invalidate_from(0);
                }
                self.config.push_recent(path);
                self.invalidate_diff();
                if lossy {
                    self.notify_error(t(self.lang, "msg.lossy_encoding"));
                } else {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_owned();
                    let msg = tf(self.lang, "msg.opened", &[("name", &name)]);
                    self.notify(msg);
                }
            }
            Err(e) => {
                let msg = tf(self.lang, "msg.open_failed", &[("err", &e.to_string())]);
                self.notify_error(msg);
            }
        }
    }

    fn pick_and_open(&mut self, side: Side) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = self.config.last_dir() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_into(side, &path);
        }
    }

    fn save(&mut self, side: Side, ask: bool) {
        let path = match (&self.pane(side).path, ask) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let mut dialog = rfd::FileDialog::new();
                if let Some(dir) = self.config.last_dir() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(name) = self
                    .pane(side)
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                {
                    dialog = dialog.set_file_name(name);
                }
                dialog.save_file()
            }
        };
        let Some(path) = path else {
            return;
        };

        let text = self.pane(side).buffer.text();
        let enc = self.pane(side).encoding;
        match encoding::write_file(&path, &text, &enc) {
            Ok(()) => {
                {
                    let pane = self.pane_mut(side);
                    pane.buffer.mark_saved();
                    pane.path = Some(path.clone());
                }
                self.config.push_recent(&path);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned();
                let msg = tf(self.lang, "msg.saved", &[("name", &name)]);
                self.notify(msg);
            }
            Err(e) => {
                let msg = tf(self.lang, "msg.save_failed", &[("err", &e.to_string())]);
                self.notify_error(msg);
            }
        }
    }

    fn unified(&self) -> String {
        let left_label = self
            .left
            .path
            .as_ref()
            .map_or_else(|| "a/left".to_owned(), |p| format!("a/{}", p.display()));
        let right_label = self
            .right
            .path
            .as_ref()
            .map_or_else(|| "b/right".to_owned(), |p| format!("b/{}", p.display()));
        to_unified(
            &self.diff,
            self.left.buffer.lines(),
            self.right.buffer.lines(),
            &UnifiedOptions {
                left_label: &left_label,
                right_label: &right_label,
                context: 3,
            },
        )
    }

    // ---- Actions -------------------------------------------------------

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Open(side) => self.pick_and_open(side),
            Action::OpenRecent(path, side) => self.open_into(side, &path),
            Action::ClearRecent => self.config.recent_files.clear(),
            Action::Save(side) => self.save(side, false),
            Action::SaveAs(side) => self.save(side, true),

            Action::CopyUnified => {
                let text = self.unified();
                if text.is_empty() {
                    self.notify(t(self.lang, "msg.no_diff_to_export"));
                } else {
                    ctx.copy_text(text);
                    self.notify(t(self.lang, "msg.copied"));
                }
            }
            Action::ExportUnified => {
                let text = self.unified();
                if text.is_empty() {
                    self.notify(t(self.lang, "msg.no_diff_to_export"));
                    return;
                }
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("changes.diff")
                    .save_file()
                {
                    match std::fs::write(&path, text) {
                        Ok(()) => {
                            let name = path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_owned();
                            let msg = tf(self.lang, "msg.saved", &[("name", &name)]);
                            self.notify(msg);
                        }
                        Err(e) => {
                            let msg =
                                tf(self.lang, "msg.save_failed", &[("err", &e.to_string())]);
                            self.notify_error(msg);
                        }
                    }
                }
            }
            Action::CopyText(side) => {
                let buf = &self.pane(side).buffer;
                let text = if buf.selection().is_empty() {
                    buf.text()
                } else {
                    buf.selected_text()
                };
                ctx.copy_text(text);
                self.notify(t(self.lang, "msg.copied"));
            }

            Action::Undo(side) => {
                self.pane_mut(side).buffer.undo();
            }
            Action::Redo(side) => {
                self.pane_mut(side).buffer.redo();
            }
            Action::SelectAll(side) => self.pane_mut(side).buffer.select_all(),
            Action::Clear(side) => {
                self.pane_mut(side).buffer.set_text("");
            }

            Action::Swap => {
                std::mem::swap(&mut self.left.buffer, &mut self.right.buffer);
                std::mem::swap(&mut self.left.path, &mut self.right.path);
                std::mem::swap(&mut self.left.encoding, &mut self.right.encoding);
                std::mem::swap(&mut self.left.syntax_override, &mut self.right.syntax_override);
                self.left.highlighter.invalidate_from(0);
                self.right.highlighter.invalidate_from(0);
                self.invalidate_diff();
            }

            Action::Find => {
                let seed = self.pane(self.focus_side).buffer.selected_text();
                self.find.target = self.focus_side;
                self.find.open(false, Some(seed));
            }
            Action::Replace => {
                let seed = self.pane(self.focus_side).buffer.selected_text();
                self.find.target = self.focus_side;
                self.find.open(true, Some(seed));
            }

            Action::Cleanup(op, side) => {
                let lines = cleanup::apply(op, self.pane(side).buffer.lines());
                let pane = self.pane_mut(side);
                let n = pane.buffer.len_lines();
                pane.buffer.replace_lines(0..n, &lines);
                pane.highlighter.invalidate_from(0);
            }

            Action::MergeAll(dir) => {
                // Overwriting every difference on one side is worth a
                // confirmation, even though it is undoable.
                self.confirm = Some(Confirm {
                    prompt: t(self.lang, "merge.confirm_all").to_owned(),
                    action: Action::MergeAllConfirmed(dir),
                });
            }
            Action::MergeAllConfirmed(dir) => self.merge_all(dir),

            Action::NextDiff => self.next_diff(),
            Action::PrevDiff => self.prev_diff(),
            Action::FirstDiff => self.goto_hunk(0),
            Action::LastDiff => {
                if let Some(i) = self.diff.hunks.len().checked_sub(1) {
                    self.goto_hunk(i);
                }
            }

            Action::ToggleShortcuts => self.show_shortcuts = !self.show_shortcuts,
            Action::ResetLayout => {
                let d = Config::default();
                self.config.split = d.split;
                self.config.font_size = d.font_size;
                self.config.line_height = d.line_height;
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn merge_one(&mut self, index: usize, dir: Direction) {
        let Some(hunk) = self.diff.hunks.get(index).cloned() else {
            return;
        };
        let source_lines = self.pane(dir.source()).buffer.lines().to_vec();
        let patch = merge::hunk_patch(&hunk, &source_lines, dir);

        let target = self.pane_mut(dir.target());
        target.buffer.replace_lines(patch.range.clone(), &patch.lines);
        target.highlighter.invalidate_from(patch.range.start);
        self.invalidate_diff();
    }

    fn merge_all(&mut self, dir: Direction) {
        let source_lines = self.pane(dir.source()).buffer.lines().to_vec();
        let patches = merge::all_patches(&self.diff, &source_lines, dir);
        let n = patches.len();
        if n == 0 {
            return;
        }

        // Compute the finished document first, then write it in one go.
        // Applying the patches one at a time would work, but it would cost the
        // user one Ctrl+Z per hunk to take back a single menu click.
        let target_lines = self.pane(dir.target()).buffer.lines().to_vec();
        let merged = merge::preview(&target_lines, &patches);

        let target = self.pane_mut(dir.target());
        let len = target.buffer.len_lines();
        target.buffer.replace_lines(0..len, &merged);
        target.highlighter.invalidate_from(0);

        self.invalidate_diff();
        let msg = tf(self.lang, "msg.merged", &[("n", &n.to_string())]);
        self.notify(msg);
    }

    // ---- Keyboard ------------------------------------------------------

    fn global_shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let side = self.focus_side;

        let hit = |ctx: &egui::Context, m: Modifiers, k: Key| {
            ctx.input_mut(|i| i.consume_key(m, k))
        };

        if hit(ctx, ctrl, Key::F) {
            actions.push(Action::Find);
        }
        if hit(ctx, ctrl, Key::H) {
            actions.push(Action::Replace);
        }
        if hit(ctx, ctrl, Key::S) {
            actions.push(Action::Save(side));
        }
        if hit(ctx, ctrl_shift, Key::S) {
            actions.push(Action::SaveAs(side));
        }
        if hit(ctx, ctrl, Key::O) {
            actions.push(Action::Open(side));
        }
        // Comparison shortcuts are hidden from the menus in single-document
        // mode, so they must not keep firing from the keyboard either.
        if !self.config.single_pane {
            if hit(ctx, ctrl_shift, Key::X) {
                actions.push(Action::Swap);
            }
            if hit(ctx, Modifiers::NONE, Key::F3) {
                actions.push(Action::NextDiff);
            }
            if hit(ctx, Modifiers::SHIFT, Key::F3) {
                actions.push(Action::PrevDiff);
            }
            // Ctrl+D is "next difference" in several diff tools; keep that
            // muscle memory working.
            if hit(ctx, ctrl, Key::D) {
                actions.push(Action::NextDiff);
            }
            if hit(ctx, ctrl_shift, Key::D) {
                actions.push(Action::PrevDiff);
            }
        }
        if hit(ctx, ctrl, Key::Plus) || hit(ctx, ctrl, Key::Equals) {
            self.config.font_size = (self.config.font_size + 1.0).min(crate::config::MAX_FONT_SIZE);
        }
        if hit(ctx, ctrl, Key::Minus) {
            self.config.font_size = (self.config.font_size - 1.0).max(crate::config::MIN_FONT_SIZE);
        }
        if hit(ctx, ctrl, Key::Num0) {
            self.config.font_size = Config::default().font_size;
        }
        if hit(ctx, Modifiers::NONE, Key::F1) {
            actions.push(Action::ToggleShortcuts);
        }
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

impl eframe::App for DuiBi {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.restyle_if_needed(&ctx);

        let before_options = self.config.diff;
        let before_wrap = self.config.word_wrap;
        let before_single = self.config.single_pane;

        // With one pane, "the focused side" can only mean the one on screen -
        // otherwise Ctrl+S would silently save the hidden document.
        if self.config.single_pane {
            self.focus_side = Side::Left;
            // The find bar hides its side switch here, so make sure the target
            // is the pane that is actually on screen.
            self.find.target = Side::Left;
        }

        self.maybe_recompare(&ctx);
        self.refresh_search();
        let mut actions: Vec<Action> = Vec::new();
        self.global_shortcuts(&ctx, &mut actions);

        self.draw_top(ui, &mut actions);
        self.draw_status(ui);
        self.draw_central(ui, &mut actions);
        // Both panes have reported their heights; fold them in for next frame.
        self.layout.commit_measurements();
        self.viewport.end_frame();
        self.draw_overlays(ui, &mut actions);

        // Handle dropped files: dragging a file onto the window loads it into
        // whichever half it was dropped on.
        self.handle_drops(&ctx);

        if self.config.diff.alignment_key() != before_options.alignment_key()
            || self.config.single_pane != before_single
        {
            self.invalidate_diff();
        } else if self.config.diff.granularity != before_options.granularity {
            // Only the painting changed. Drop the cached word-level diffs and
            // redraw; re-running the comparison here would be wasted work and
            // would shift the view away from wherever the user was looking.
            self.diff_generation = self.diff_generation.wrapping_add(1);
        }
        if self.config.single_pane != before_single {
            // The row list is about to change shape, and a stale scroll offset
            // into the old one would land somewhere arbitrary.
            self.viewport.reset();
            self.focused_hunk = None;
        }
        if self.config.word_wrap != before_wrap {
            self.layout
                .reconfigure(self.diff.rows.len(), self.config.word_wrap);
        }

        for action in actions {
            self.run(&ctx, action);
        }

        self.update_title(&ctx);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if let Err(e) = self.config.save() {
            eprintln!("could not save preferences: {e}");
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.config.save();
    }
}

impl DuiBi {
    fn update_title(&self, ctx: &egui::Context) {
        let title = if self.config.single_pane {
            format!(
                "{}  \u{2014}  {}",
                t(self.lang, "app.title"),
                self.left.title(self.lang)
            )
        } else {
            format!(
                "{}  \u{2014}  {}  \u{2194}  {}",
                t(self.lang, "app.title"),
                self.left.title(self.lang),
                self.right.title(self.lang),
            )
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Recompute the search hits for the pane being searched.
    ///
    /// [`FindState::refresh`] is cheap when nothing relevant has changed, so
    /// this runs every frame; without it the find bar had a pattern but never
    /// any matches.
    fn refresh_search(&mut self) {
        let side = self.find.target;
        let buffer = match side {
            Side::Left => &self.left.buffer,
            Side::Right => &self.right.buffer,
        };
        self.find.refresh(buffer, side);
    }

    fn draw_top(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let facts = MenuFacts {
            can_undo: [self.left.buffer.can_undo(), self.right.buffer.can_undo()],
            can_redo: [self.left.buffer.can_redo(), self.right.buffer.can_redo()],
            has_diffs: !self.diff.hunks.is_empty(),
            focus_side: self.focus_side,
            recent: self.config.recent_files.clone(),
            single_pane: self.config.single_pane,
        };

        let palette = self.palette;
        let lang = self.lang;
        egui::Panel::top("duibi_top")
            .frame(
                egui::Frame::new()
                    .fill(palette.chrome_bg)
                    .inner_margin(egui::Margin::symmetric(8, 5)),
            )
            .show(ui, |ui| {
                show_toolbar(ui, &mut self.config, &facts, lang, actions);
            });

        if self.find.open {
            let mut action = FindAction::None;
            let single = self.config.single_pane;
            egui::Panel::top("duibi_find")
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    action = show_find_bar(ui, &mut self.find, &palette, lang, single);
                });
            self.apply_find_action(action);
        }
    }

    fn apply_find_action(&mut self, action: FindAction) {
        match action {
            FindAction::None => {}
            FindAction::Close => {
                self.find.close();
                self.focus_request = Some(self.focus_side);
            }
            FindAction::Reveal => self.reveal_active_match(),
            FindAction::ReplaceOne => {
                let side = self.find.target;
                let mut find = std::mem::take(&mut self.find);
                let changed = find.replace_current(self.pane_mut(side).buffer_mut());
                self.find = find;
                if changed {
                    self.find.next();
                    self.invalidate_diff();
                }
            }
            FindAction::ReplaceAll => {
                let side = self.find.target;
                let mut find = std::mem::take(&mut self.find);
                let n = find.replace_all(self.pane_mut(side).buffer_mut());
                self.find = find;
                if n > 0 {
                    self.invalidate_diff();
                    self.pane_mut(side).highlighter.invalidate_from(0);
                    let msg = tf(self.lang, "msg.replaced", &[("n", &n.to_string())]);
                    self.notify(msg);
                }
            }
        }
    }

    /// Move the caret to the active search hit and scroll it into view.
    fn reveal_active_match(&mut self) {
        let Some(m) = self.find.active_match() else {
            return;
        };
        let side = self.find.target;
        let (anchor, head) = {
            let buf = &self.pane(side).buffer;
            let line = buf.line(m.line);
            let start_col = line[..m.start.min(line.len())].chars().count();
            let end_col = line[..m.end.min(line.len())].chars().count();
            (buf.char_at(m.line, start_col), buf.char_at(m.line, end_col))
        };
        self.pane_mut(side)
            .buffer
            .set_selection(crate::core::text::Selection { anchor, head });
        if let Some(row) = self.diff.row_of(side, m.line) {
            let line_h = self.editor_style().line_height;
            self.viewport.scroll_to_row(
                row,
                Reveal::Centre,
                &self.layout,
                line_h,
                self.viewport_height,
            );
        }
        self.focus_side = side;
    }

    fn draw_status(&mut self, ui: &mut egui::Ui) {
        let side = self.focus_side;
        let hunk_position = self.focused_hunk.map(|i| (i + 1, self.diff.hunks.len()));
        let facts = StatusFacts {
            single_pane: self.config.single_pane,
            stats: self.diff.stats,
            identical: self.diff.stats.is_identical() && !self.diff_dirty,
            truncated: self.diff.truncated,
            comparing: self.diff_dirty,
            last_compare: self.last_compare,
            focus_side: side,
            buffer: &self.pane(side).buffer,
            encoding: &self.pane(side).encoding,
            hunk_position,
            summaries: &self.summaries,
        };
        let palette = self.palette;
        let lang = self.lang;
        let mut jump = None;
        egui::Panel::bottom("duibi_status")
            .frame(
                egui::Frame::new()
                    .fill(palette.chrome_bg)
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ui, |ui| {
                jump = show_status_bar(ui, &facts, &palette, lang);
            });

        if let Some(index) = jump {
            self.goto_hunk(index);
        }
    }

    fn draw_central(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let palette = self.palette;
        let style = self.editor_style();
        let lang = self.lang;

        // An explicit fill rather than `no_frame`: any region a child does not
        // paint (a short header, a pane narrower than its column) would
        // otherwise show through as bare background.
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.bg))
            .show(ui, |ui| {
            if self.config.single_pane {
                self.viewport_height = ui.available_height();
                // One editor, full width, no gutter and no overview.
                let out = ui
                    .vertical(|ui| self.draw_pane(ui, Side::Left, &style, &palette, lang))
                    .inner;
                let previous = [self.left.last_offset, self.right.last_offset];
                self.viewport.observe(Some(out.offset), None, previous);
                self.left.last_offset = out.offset;
                self.focus_side = Side::Left;
                return;
            }

            // How much of the document fits on screen, so a jump can centre.
            self.viewport_height = ui.available_height();

            let total_w = ui.available_width();
            let overview_w = if self.config.show_overview {
                crate::ui::overview::OVERVIEW_WIDTH
            } else {
                0.0
            };
            let panes_w = (total_w - gutter::GUTTER_WIDTH - overview_w).max(120.0);
            let left_w = (panes_w * self.config.split).clamp(80.0, panes_w - 80.0);
            // Remember where the split actually is, for drop targeting.
            self.pane_boundary_x =
                ui.available_rect_before_wrap().left() + left_w + gutter::GUTTER_WIDTH / 2.0;

            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;

                // ---- Left pane -----------------------------------------
                let full_h = ui.available_height();
                let left_out = ui
                    .allocate_ui(Vec2::new(left_w, full_h), |ui| {
                        // `allocate_ui` inherits the parent's horizontal
                        // layout, which would put the header *beside* the
                        // editor instead of above it.
                        ui.vertical(|ui| self.draw_pane(ui, Side::Left, &style, &palette, lang))
                            .inner
                    })
                    .inner;

                // ---- Merge column --------------------------------------
                let gutter_out = show_gutter_column(
                    ui,
                    &self.diff,
                    &self.layout,
                    &palette,
                    style.line_height,
                    self.viewport.offset().y,
                    self.focused_hunk,
                    !self.diff_dirty,
                );

                // ---- Right pane ----------------------------------------
                let right_w = (panes_w - left_w).max(80.0);
                let right_out = ui
                    .allocate_ui(Vec2::new(right_w, full_h), |ui| {
                        ui.vertical(|ui| self.draw_pane(ui, Side::Right, &style, &palette, lang))
                            .inner
                    })
                    .inner;

                // ---- Overview strip ------------------------------------
                if self.config.show_overview {
                    let total_rows = self.diff.rows.len().max(1) as f32;
                    let first = self
                        .layout
                        .row_at(self.viewport.offset().y, style.line_height)
                        as f32;
                    let visible =
                        (ui.available_height() / style.line_height).max(1.0);
                    let params = OverviewParams {
                        diff: &self.diff,
                        palette: &palette,
                        viewport: (first / total_rows)
                            ..((first + visible) / total_rows).min(1.0),
                        focused_hunk: self.focused_hunk,
                    };
                    if let Some(row) = show_overview(ui, params) {
                        let line_h = style.line_height;
                        self.viewport.scroll_to_row(
                            row,
                            Reveal::Centre,
                            &self.layout,
                            line_h,
                            self.viewport_height,
                        );
                    }
                }

                // ---- Post-frame bookkeeping ----------------------------
                if gutter_out.drag_x.abs() > 0.0 {
                    self.config.split =
                        (self.config.split + gutter_out.drag_x / panes_w).clamp(0.15, 0.85);
                }
                if let Some(req) = gutter_out.merge {
                    self.merge_one(req.hunk, req.direction);
                }

                self.sync_scroll(left_out, right_out);
            });
        });

        let _ = actions;
    }

    /// Decide which pane drove the scroll this frame and record the shared
    /// offset the other one should adopt next frame.
    fn sync_scroll(&mut self, left: PaneFrame, right: PaneFrame) {
        let previous = [self.left.last_offset, self.right.last_offset];
        self.viewport
            .observe(Some(left.offset), Some(right.offset), previous);

        self.left.last_offset = left.offset;
        self.right.last_offset = right.offset;

        if left.focused {
            self.focus_side = Side::Left;
        } else if right.focused {
            self.focus_side = Side::Right;
        }

        // Keep the "current hunk" readout following the caret.
        let row = self.caret_row(self.focus_side);
        if let Some(i) = self.diff.hunk_at_row(row) {
            self.focused_hunk = Some(i);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_pane(
        &mut self,
        ui: &mut egui::Ui,
        side: Side,
        style: &EditorStyle,
        palette: &Palette,
        lang: Lang,
    ) -> PaneFrame {
        // Header strip with the file name.
        let title = self.pane(side).title(lang);
        let subtitle = self
            .pane(side)
            .path
            .as_ref()
            .map(|p| p.display().to_string());
        let is_focus = self.focus_side == side;

        egui::Frame::new()
            .fill(palette.chrome_bg)
            .inner_margin(egui::Margin::symmetric(10, 4))
            .show(ui, |ui| {
                // Without this the frame shrinks to its text and leaves the
                // rest of the strip unpainted.
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    // A focused pane's name is brighter, so it is obvious which
                    // half the keyboard is aimed at.
                    ui.label(
                        RichText::new(title)
                            .color(if is_focus { palette.text } else { palette.text_dim })
                            .strong(),
                    );
                    if let Some(sub) = subtitle {
                        ui.add_space(8.0);
                        // Truncated rather than wrapped: the header must stay
                        // exactly one line tall or it shifts the editor below.
                        ui.add(
                            egui::Label::new(
                                RichText::new(&sub).color(palette.text_faint).small(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(sub);
                    }
                });
            });

        // Syntax highlighting for the rows about to be drawn.
        let (highlights, highlight_rows) = self.highlights_for(side, style);

        let force_offset = self.desired_offset(side, style);
        // Read before the borrows below split `self` up.
        let goal_column = self.pane(side).goal_column;
        let longest_line = self.pane_mut(side).longest_line();

        let generation = self.diff_generation;
        let diff_options = self.config.diff;
        let focus_requested = self.focus_request == Some(side);
        let active_match = (self.find.target == side)
            .then_some(self.find.active)
            .flatten();
        let searching = self.find.open && self.find.target == side;

        // Borrow the two documents apart rather than copying them.
        //
        // This used to hand `show_pane` an owned `Vec<String>` of *each* side,
        // rebuilt every frame - and because `draw_pane` runs once per pane,
        // that was four full copies of the document per frame. On a 10k-line
        // file it meant tens of thousands of string allocations between one
        // frame and the next, which is what made a large comparison feel heavy.
        let (diff, layout, inline) = (&self.diff, &mut self.layout, &mut self.inline);
        let no_matches: Vec<crate::core::text::Match> = Vec::new();
        let search = if searching { &self.find.matches } else { &no_matches };

        let (buffer, other_lines) = match side {
            Side::Left => (&mut self.left.buffer, self.right.buffer.lines()),
            Side::Right => (&mut self.right.buffer, self.left.buffer.lines()),
        };

        let out = show_pane(
            ui,
            PaneParams {
                side,
                buffer,
                diff,
                diff_generation: generation,
                diff_options: &diff_options,
                other_lines,
                layout,
                inline,
                palette,
                style,
                highlights: &highlights,
                highlight_rows,
                search,
                active_match,
                longest_line,
                force_offset,
                focus_requested,
                goal_column,
            },
        );

        if focus_requested {
            self.focus_request = None;
        }
        if out.edited {
            let from = self
                .pane(side)
                .buffer
                .min_dirty_line()
                .unwrap_or(0);
            self.pane_mut(side).highlighter.invalidate_from(from);
            self.pane_mut(side).buffer.clear_dirty();
        }

        self.pane_mut(side).last_visible = out.visible_rows;
        self.pane_mut(side).goal_column = out.goal_column;

        PaneFrame {
            offset: out.offset,
            focused: out.response.has_focus(),
        }
    }

    /// Syntax colours for the rows this pane is about to draw.
    fn highlights_for(
        &mut self,
        side: Side,
        style: &EditorStyle,
    ) -> (Vec<Vec<StyledRange>>, std::ops::Range<usize>) {
        if !self.config.syntax_highlighting {
            self.pane_mut(side).highlighter.set_syntax(None);
            return (Vec::new(), 0..0);
        }

        // Decide the language: an explicit choice wins, otherwise detect from
        // the filename and fall back to the first line.
        let name = {
            let pane = self.pane(side);
            match &pane.syntax_override {
                Some(n) => n.clone(),
                None => {
                    let first = pane.buffer.line(0).to_owned();
                    self.syntax
                        .detect(pane.path.as_deref(), &first)
                        .name
                        .clone()
                }
            }
        };
        let plain = self.syntax.plain_text_name().to_owned();
        let assets = self.syntax.clone();
        let theme = self.palette.syntect_theme;

        // Rows on screen map to a *line* range on this side.
        let rows = self.visible_row_range(side, style);
        let lines = self.lines_for_rows(side, rows.clone());

        let pane = self.pane_mut(side);
        pane.highlighter
            .set_syntax((name != plain).then_some(name.as_str()));

        let per_line = pane.highlighter.highlight(
            &assets,
            theme,
            pane.buffer.lines(),
            lines.clone(),
        );

        // Re-key from lines back to rows, so the editor can index by row.
        let mut out = vec![Vec::new(); rows.len()];
        for (i, row_idx) in rows.clone().enumerate() {
            if let Some(row) = self.diff.rows.get(row_idx)
                && let Some(line) = row.side(side)
                && line >= lines.start
                && line - lines.start < per_line.len()
            {
                out[i] = per_line[line - lines.start].clone();
            }
        }
        (out, rows)
    }

    /// Rows to prepare highlighting for.
    ///
    /// Uses what the pane actually drew last frame, padded so an ordinary
    /// scroll does not outrun it. Falls back to an estimate before first draw.
    fn visible_row_range(&self, side: Side, style: &EditorStyle) -> std::ops::Range<usize> {
        let last = self.pane(side).last_visible.clone();
        let rows = if last.is_empty() {
            let height = 1200.0;
            self.layout
                .visible_rows(
                    self.viewport.offset().y..self.viewport.offset().y + height,
                    style.line_height,
                )
        } else {
            last
        };
        const PAD: usize = 24;
        rows.start.saturating_sub(PAD)..(rows.end + PAD).min(self.diff.rows.len())
    }

    /// Line range on `side` covered by `rows`.
    fn lines_for_rows(
        &self,
        side: Side,
        rows: std::ops::Range<usize>,
    ) -> std::ops::Range<usize> {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for r in rows {
            if let Some(row) = self.diff.rows.get(r)
                && let Some(line) = row.side(side)
            {
                lo = lo.min(line);
                hi = hi.max(line + 1);
            }
        }
        if lo == usize::MAX { 0..0 } else { lo..hi }
    }

    /// The scroll offset this pane should be forced to, if any.
    fn desired_offset(&self, side: Side, _style: &EditorStyle) -> Option<Vec2> {
        self.viewport.forced_offset(side, self.config.sync_scroll)
    }

    fn draw_overlays(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let palette = self.palette;
        let lang = self.lang;

        // ---- Empty-state hint -------------------------------------------
        //
        // Skipped in single-document mode: the hint is about pasting into two
        // sides, and there are no sides. An empty text editor showing nothing
        // but a caret is exactly what it should look like.
        if !self.config.single_pane
            && self.left.buffer.is_empty()
            && self.right.buffer.is_empty()
        {
            let screen = ui.ctx().viewport_rect();
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("duibi_hint"),
            ));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                t(lang, "empty.hint"),
                egui::FontId::proportional(15.0),
                palette.text_faint,
            );
        }

        // ---- Toast -------------------------------------------------------
        if let Some(toast) = &self.toast {
            if toast.at.elapsed() > TOAST_LIFETIME {
                self.toast = None;
            } else {
                let color = match toast.kind {
                    ToastKind::Info => palette.accent,
                    ToastKind::Error => palette.error,
                };
                let text = toast.text.clone();
                egui::Area::new(egui::Id::new("duibi_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -48.0))
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::new()
                            .fill(palette.chrome_bg)
                            .stroke(egui::Stroke::new(1.0, color))
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(14, 9))
                            .show(ui, |ui| {
                                ui.label(RichText::new(text).color(palette.text));
                            });
                    });
                ui.ctx()
                    .request_repaint_after(Duration::from_millis(250));
            }
        }

        // ---- Confirmation ------------------------------------------------
        if let Some(confirm) = &self.confirm {
            let prompt = confirm.prompt.clone();
            let pending = confirm.action.clone();
            let mut decided: Option<bool> = None;
            egui::Modal::new(egui::Id::new("duibi_confirm")).show(ui.ctx(), |ui| {
                ui.set_min_width(360.0);
                ui.label(RichText::new(prompt).color(palette.text));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(t(lang, "msg.cancel")).clicked() {
                        decided = Some(false);
                    }
                    if ui
                        .button(RichText::new(t(lang, "msg.confirm")).strong())
                        .clicked()
                    {
                        decided = Some(true);
                    }
                });
            });
            match decided {
                Some(true) => {
                    self.confirm = None;
                    actions.push(pending);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }

        // ---- Shortcut sheet ----------------------------------------------
        if self.show_shortcuts {
            let mut open = true;
            egui::Window::new(t(lang, "shortcuts.title"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    egui::Grid::new("duibi_shortcuts")
                        .num_columns(2)
                        .spacing([28.0, 6.0])
                        .show(ui, |ui| {
                            for (keys, key) in SHORTCUTS {
                                ui.label(RichText::new(*keys).monospace().color(palette.accent));
                                ui.label(t(lang, key));
                                ui.end_row();
                            }
                        });
                });
            self.show_shortcuts = open;
        }
    }

    /// Load any file dropped onto the window, into the half it was dropped on.
    ///
    /// # Why this does not use egui's pointer
    ///
    /// Neither `HoveredFile` nor `DroppedFile` carries a position, and on
    /// Windows winit receives the drag coordinates from `IDropTarget::DragOver`
    /// and then discards them - it emits no `CursorMoved` during a drag either.
    /// So `pointer.latest_pos()` is whatever it was before the drag began,
    /// which is why every drop used to be treated as landing on the left.
    /// [`cursor_x_in_window`] asks the OS directly instead.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            self.hover_drop_x = cursor_x_in_window(ctx);
            self.paint_drop_target(ctx);
            // No further events arrive while a drag hovers, so without this the
            // highlight would freeze instead of following the cursor.
            ctx.request_repaint();
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }

        // Prefer the position sampled during the hover: by the time the drop
        // is delivered the cursor may already have moved on.
        let x = self.hover_drop_x.or_else(|| cursor_x_in_window(ctx));
        self.hover_drop_x = None;

        // Two files at once fill both sides, left to right - the obvious
        // meaning of dragging a pair in.
        if dropped.len() >= 2 {
            self.open_into(Side::Left, &dropped[0]);
            self.open_into(Side::Right, &dropped[1]);
            return;
        }

        // With no position at all, fall back to whichever side is empty, then
        // to the focused one - never silently always-left.
        let side = match x {
            Some(x) if x < self.pane_boundary_x => Side::Left,
            Some(_) => Side::Right,
            None if self.left.buffer.is_empty() => Side::Left,
            None if self.right.buffer.is_empty() => Side::Right,
            None => self.focus_side,
        };
        for path in dropped {
            self.open_into(side, &path);
        }
    }

    /// Shade the half a hovering file would land on.
    fn paint_drop_target(&self, ctx: &egui::Context) {
        let Some(x) = self.hover_drop_x else {
            return;
        };
        let full = ctx.viewport_rect();
        let half = if x < self.pane_boundary_x {
            egui::Rect::from_min_max(
                full.min,
                egui::pos2(self.pane_boundary_x.min(full.right()), full.bottom()),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(self.pane_boundary_x.max(full.left()), full.top()),
                full.max,
            )
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("duibi_drop"),
        ));
        painter.rect_filled(half, 0, self.palette.accent.gamma_multiply(0.18));
        painter.rect_stroke(
            half.shrink(2.0),
            4,
            egui::Stroke::new(2.0, self.palette.accent),
            egui::StrokeKind::Inside,
        );
        // Name the target, so there is no doubt before letting go.
        let key = if x < self.pane_boundary_x {
            "app.left"
        } else {
            "app.right"
        };
        painter.text(
            half.center(),
            egui::Align2::CENTER_CENTER,
            t(self.lang, key),
            egui::FontId::proportional(28.0),
            self.palette.accent,
        );
    }
}

/// Cursor x relative to the window's client area, in egui points.
///
/// Windows only: winit discards the coordinates that come with a file drag, so
/// the OS is the only source left. Elsewhere the pointer egui already tracks is
/// good enough, because those backends do report drag positions.
#[cfg(windows)]
fn cursor_x_in_window(ctx: &egui::Context) -> Option<f32> {
    // user32, one argument, no dependency needed.
    unsafe extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
    }
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    let mut pt = Point { x: 0, y: 0 };
    // SAFETY: `pt` is a valid, correctly-shaped POINT for the duration.
    if unsafe { GetCursorPos(&raw mut pt) } == 0 {
        return None;
    }

    // `GetCursorPos` is in physical desktop pixels; `inner_rect` is the client
    // area in logical desktop points. Convert before subtracting.
    let inner = ctx.input(|i| i.viewport().inner_rect)?;
    let ppp = ctx.pixels_per_point().max(0.1);
    Some(pt.x as f32 / ppp - inner.left())
}

#[cfg(not(windows))]
fn cursor_x_in_window(ctx: &egui::Context) -> Option<f32> {
    ctx.input(|i| i.pointer.latest_pos().or(i.pointer.interact_pos()))
        .map(|p| p.x)
}

/// What one pane reported back this frame.
struct PaneFrame {
    offset: Vec2,
    focused: bool,
}

#[allow(clippy::too_many_arguments)]
fn show_gutter_column(
    ui: &mut egui::Ui,
    diff: &DiffResult,
    layout: &RowLayout,
    palette: &Palette,
    line_height: f32,
    scroll_y: f32,
    focused_hunk: Option<usize>,
    enabled: bool,
) -> gutter::GutterOutput {
    gutter::show_gutter(
        ui,
        GutterParams {
            diff,
            layout,
            palette,
            line_height,
            scroll_y,
            focused_hunk,
            enabled,
        },
    )
}

/// Keyboard reference, shown by F1.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O", "file.open_left"),
    ("Ctrl+S", "file.save_left"),
    ("Ctrl+F", "edit.find"),
    ("Ctrl+H", "edit.replace"),
    ("Ctrl+Z", "edit.undo"),
    ("Ctrl+Shift+Z", "edit.redo"),
    ("Ctrl+A", "edit.select_all"),
    ("Ctrl+Shift+X", "edit.swap"),
    ("F3 / Ctrl+D", "nav.next_diff"),
    ("Shift+F3", "nav.prev_diff"),
    ("Ctrl + / Ctrl -", "view.font_size"),
];

/// Install a font stack that can actually render the interface.
///
/// egui ships a Latin-only set, so a Chinese UI would be nothing but tofu
/// boxes. We look for a system CJK face and append it as a *fallback* to both
/// families, which keeps the crisp bundled monospace for code while still
/// rendering Chinese, Japanese and Korean text.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Ordered by preference. Microsoft YaHei is on every modern Windows;
    // the rest cover the other platforms we can be built for.
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhl.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ];

    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        // Appended, not prepended: Latin text keeps the bundled faces and only
        // characters the primary font lacks fall through to this one.
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        break;
    }

    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_row_has_a_label() {
        for (keys, key) in SHORTCUTS {
            assert!(!keys.is_empty());
            for lang in [Lang::Chinese, Lang::English] {
                assert!(!t(lang, key).is_empty(), "{key} missing for {lang:?}");
            }
        }
    }

    #[test]
    fn a_pane_without_a_file_is_titled_untitled() {
        let pane = Pane::default();
        assert_eq!(pane.title(Lang::English), "Untitled");
    }

    #[test]
    fn a_dirty_pane_is_marked_in_its_title() {
        let mut pane = Pane {
            path: Some(PathBuf::from("/tmp/notes.txt")),
            ..Default::default()
        };
        assert_eq!(pane.title(Lang::English), "notes.txt");
        pane.buffer.insert("x");
        assert!(pane.title(Lang::English).ends_with('\u{2022}'));
    }
}
