//! Colour system.
//!
//! Two hand-tuned palettes rather than one palette run through a lightness
//! flip: diff colours that read well on white are muddy on black, and vice
//! versa. The light palette follows GitHub's diff colours (very well tested for
//! contrast); the dark palette follows VS Code Dark+, deepened slightly so the
//! chrome recedes and the text carries the page.
//!
//! # The colour scheme, and why
//!
//! Red-for-deleted / green-for-added is the convention and we keep it. But a
//! *modified* line is neither, and painting one side red and the other green
//! makes every edit look like a delete-plus-insert. So modified rows get their
//! own amber wash on **both** sides, and the word-level spans inside them carry
//! the red/green. At a glance you can tell "this line was edited" from "this
//! line was removed" without reading either.
//!
//! Red and green are also the classic colour-blindness collision. Two things
//! mitigate it here: the change markers in the gutter differ in *shape*
//! (`-` / `+` / `~`), and modified lines are amber, which is the case where the
//! distinction actually matters.

use egui::Color32;

use crate::config::ThemeMode;
use crate::i18n::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn resolve(mode: ThemeMode, system: Option<egui::Theme>) -> Self {
        match mode {
            ThemeMode::Light => Self::Light,
            ThemeMode::Dark => Self::Dark,
            ThemeMode::System => match system {
                Some(egui::Theme::Light) => Self::Light,
                _ => Self::Dark,
            },
        }
    }

    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }
}

/// Everything the egui style is derived from.
///
/// The style is rebuilt whenever this value changes. Deriving the trigger from
/// the inputs beats maintaining a hand-written list of conditions: the original
/// code asked "did the language change, or is the theme set to follow the
/// system?", which is true for every case *except* the user picking Light or
/// Dark outright - so an explicit choice silently did nothing until something
/// else happened to force a rebuild.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyleKey {
    pub appearance: Appearance,
    pub lang: Lang,
}

impl StyleKey {
    pub fn new(theme: ThemeMode, lang: Lang, system: Option<egui::Theme>) -> Self {
        Self {
            appearance: Appearance::resolve(theme, system),
            lang,
        }
    }
}

/// Every colour the app draws with. Flat on purpose - one place to look.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    // ---- Surfaces ----
    /// Window background, behind the panels.
    pub bg: Color32,
    /// The text area itself.
    pub editor_bg: Color32,
    /// Line-number column.
    pub gutter_bg: Color32,
    /// Toolbars, menus, status bar.
    pub chrome_bg: Color32,
    /// Hovered menu / button background.
    pub hover_bg: Color32,
    /// Pressed / active background.
    pub active_bg: Color32,
    /// Hairlines between regions.
    pub border: Color32,
    /// A stronger divider, e.g. the centre gutter edges.
    pub border_strong: Color32,

    // ---- Text ----
    pub text: Color32,
    /// Labels, secondary information.
    pub text_dim: Color32,
    /// Line numbers, placeholder text.
    pub text_faint: Color32,
    pub accent: Color32,
    /// Text drawn on top of `accent`.
    pub on_accent: Color32,

    // ---- Editor decorations ----
    pub caret: Color32,
    pub selection: Color32,
    /// Wash over the line the caret is on.
    pub current_line: Color32,
    /// Rendered whitespace dots and arrows.
    pub whitespace: Color32,
    /// The indent/wrap guide.
    pub guide: Color32,

    // ---- Diff ----
    /// Row background for a deleted line.
    pub del_bg: Color32,
    /// Word-level span inside a deleted or modified line.
    pub del_span: Color32,
    pub ins_bg: Color32,
    pub ins_span: Color32,
    /// Row background for a modified line, both sides.
    pub mod_bg: Color32,
    /// The blank filler opposite an insertion or deletion.
    pub filler_bg: Color32,
    /// Outline drawn around the hunk the caret is in.
    pub focus_hunk: Color32,

    // ---- Markers (gutter glyphs, overview bar) ----
    pub marker_del: Color32,
    pub marker_ins: Color32,
    pub marker_mod: Color32,

    // ---- Search ----
    pub search_bg: Color32,
    /// The match the caret is currently on.
    pub search_active_bg: Color32,
    pub search_border: Color32,

    // ---- Feedback ----
    pub warn: Color32,
    pub error: Color32,
    pub ok: Color32,

    /// syntect theme to pull syntax colours from.
    pub syntect_theme: &'static str,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u16 * a as u16 / 255) as u8,
        (g as u16 * a as u16 / 255) as u8,
        (b as u16 * a as u16 / 255) as u8,
        a,
    )
}

impl Palette {
    pub fn of(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }

    pub const fn light() -> Self {
        Self {
            bg: rgb(0xF6, 0xF7, 0xF9),
            editor_bg: rgb(0xFF, 0xFF, 0xFF),
            gutter_bg: rgb(0xFA, 0xFB, 0xFC),
            chrome_bg: rgb(0xF0, 0xF2, 0xF5),
            hover_bg: rgb(0xE4, 0xE7, 0xEC),
            active_bg: rgb(0xD6, 0xDB, 0xE2),
            border: rgb(0xE0, 0xE3, 0xE8),
            border_strong: rgb(0xC7, 0xCC, 0xD4),

            text: rgb(0x1F, 0x23, 0x28),
            text_dim: rgb(0x5A, 0x62, 0x6E),
            text_faint: rgb(0x7A, 0x84, 0x93),
            accent: rgb(0x09, 0x69, 0xDA),
            on_accent: rgb(0xFF, 0xFF, 0xFF),

            caret: rgb(0x09, 0x69, 0xDA),
            selection: rgba(0x09, 0x69, 0xDA, 0x38),
            current_line: rgba(0x09, 0x69, 0xDA, 0x0E),
            whitespace: rgb(0xC4, 0xCA, 0xD2),
            guide: rgb(0xEB, 0xEE, 0xF2),

            // GitHub's diff palette.
            del_bg: rgb(0xFF, 0xEB, 0xE9),
            del_span: rgb(0xFF, 0xC1, 0xBC),
            ins_bg: rgb(0xE6, 0xFF, 0xEC),
            ins_span: rgb(0xAB, 0xF2, 0xBC),
            mod_bg: rgb(0xFF, 0xF8, 0xC5),
            filler_bg: rgb(0xF2, 0xF3, 0xF5),
            focus_hunk: rgba(0x09, 0x69, 0xDA, 0x60),

            marker_del: rgb(0xCF, 0x22, 0x2E),
            marker_ins: rgb(0x1A, 0x7F, 0x37),
            marker_mod: rgb(0xBF, 0x87, 0x00),

            search_bg: rgb(0xFF, 0xF1, 0xA8),
            search_active_bg: rgb(0xFF, 0xB7, 0x00),
            search_border: rgb(0xD4, 0xA0, 0x00),

            warn: rgb(0xBF, 0x87, 0x00),
            error: rgb(0xCF, 0x22, 0x2E),
            ok: rgb(0x1A, 0x7F, 0x37),

            syntect_theme: "InspiredGitHub",
        }
    }

    pub const fn dark() -> Self {
        Self {
            bg: rgb(0x16, 0x18, 0x1C),
            editor_bg: rgb(0x1B, 0x1E, 0x23),
            gutter_bg: rgb(0x17, 0x1A, 0x1E),
            chrome_bg: rgb(0x20, 0x23, 0x28),
            hover_bg: rgb(0x2C, 0x30, 0x37),
            active_bg: rgb(0x37, 0x3C, 0x45),
            border: rgb(0x2A, 0x2E, 0x35),
            border_strong: rgb(0x3A, 0x40, 0x49),

            text: rgb(0xD7, 0xDC, 0xE3),
            text_dim: rgb(0x9A, 0xA3, 0xB0),
            text_faint: rgb(0x6B, 0x75, 0x83),
            accent: rgb(0x58, 0xA6, 0xFF),
            on_accent: rgb(0x0D, 0x11, 0x17),

            caret: rgb(0x58, 0xA6, 0xFF),
            selection: rgba(0x58, 0xA6, 0xFF, 0x40),
            current_line: rgba(0xFF, 0xFF, 0xFF, 0x08),
            whitespace: rgb(0x3F, 0x46, 0x50),
            guide: rgb(0x26, 0x2B, 0x32),

            // Deepened VS Code Dark+ diff colours: saturated enough to read at
            // a glance, dark enough that syntax colours still show through.
            del_bg: rgb(0x3A, 0x1D, 0x25),
            del_span: rgb(0x6E, 0x2B, 0x31),
            ins_bg: rgb(0x1B, 0x33, 0x27),
            ins_span: rgb(0x2A, 0x5A, 0x3E),
            mod_bg: rgb(0x38, 0x34, 0x1A),
            filler_bg: rgb(0x15, 0x17, 0x1A),
            focus_hunk: rgba(0x58, 0xA6, 0xFF, 0x70),

            marker_del: rgb(0xF8, 0x5B, 0x5B),
            marker_ins: rgb(0x4E, 0xC9, 0x7B),
            marker_mod: rgb(0xE3, 0xB3, 0x41),

            search_bg: rgb(0x5C, 0x4B, 0x16),
            search_active_bg: rgb(0xB8, 0x8A, 0x1E),
            search_border: rgb(0xE3, 0xB3, 0x41),

            warn: rgb(0xE3, 0xB3, 0x41),
            error: rgb(0xF8, 0x5B, 0x5B),
            ok: rgb(0x4E, 0xC9, 0x7B),

            syntect_theme: "base16-ocean.dark",
        }
    }

    /// Row background for a diff row kind, per side.
    ///
    /// Returns `None` for unchanged rows so the caller can skip the fill
    /// entirely - that is the common case on every frame.
    pub fn row_bg(&self, kind: crate::core::diff::RowKind) -> Option<Color32> {
        use crate::core::diff::RowKind as K;
        match kind {
            K::Equal | K::Ignored => None,
            K::Delete => Some(self.del_bg),
            K::Insert => Some(self.ins_bg),
            K::Replace => Some(self.mod_bg),
        }
    }

    /// The word-level span colour inside a changed row, per side.
    pub fn span_bg(&self, side: crate::core::diff::Side) -> Color32 {
        match side {
            crate::core::diff::Side::Left => self.del_span,
            crate::core::diff::Side::Right => self.ins_span,
        }
    }

    /// Marker colour for the change bar and the overview strip.
    pub fn marker(&self, kind: crate::core::diff::RowKind) -> Option<Color32> {
        use crate::core::diff::RowKind as K;
        match kind {
            K::Equal | K::Ignored => None,
            K::Delete => Some(self.marker_del),
            K::Insert => Some(self.marker_ins),
            K::Replace => Some(self.marker_mod),
        }
    }

    /// Build the egui visuals that match this palette, so built-in widgets
    /// (menus, scrollbars, combo boxes) sit in the same world as our painting.
    pub fn visuals(&self, dark: bool) -> egui::Visuals {
        let mut v = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        v.panel_fill = self.chrome_bg;
        v.window_fill = self.chrome_bg;
        v.extreme_bg_color = self.editor_bg;
        v.faint_bg_color = self.hover_bg;
        v.window_stroke = egui::Stroke::new(1.0, self.border);
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = egui::Stroke::new(1.0, self.accent);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.chrome_bg;
        w.noninteractive.weak_bg_fill = self.chrome_bg;
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = egui::Stroke::new(1.0, self.text_dim);

        w.inactive.bg_fill = self.hover_bg;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.fg_stroke = egui::Stroke::new(1.0, self.text);
        w.inactive.bg_stroke = egui::Stroke::new(1.0, self.border);

        w.hovered.bg_fill = self.hover_bg;
        w.hovered.weak_bg_fill = self.hover_bg;
        w.hovered.fg_stroke = egui::Stroke::new(1.0, self.text);
        w.hovered.bg_stroke = egui::Stroke::new(1.0, self.border_strong);

        w.active.bg_fill = self.active_bg;
        w.active.weak_bg_fill = self.active_bg;
        w.active.fg_stroke = egui::Stroke::new(1.0, self.text);
        w.active.bg_stroke = egui::Stroke::new(1.0, self.accent);

        w.open.bg_fill = self.active_bg;
        w.open.weak_bg_fill = self.active_bg;
        w.open.fg_stroke = egui::Stroke::new(1.0, self.text);

        // Flatter than egui's default; closer to an IDE than to a game menu.
        for s in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            s.corner_radius = egui::CornerRadius::same(4);
            s.expansion = 0.0;
        }

        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{RowKind, Side};

    /// Relative luminance per WCAG 2.1.
    fn luminance(c: Color32) -> f32 {
        let f = |v: u8| {
            let s = v as f32 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Body text must clear WCAG AA (4.5:1) against every surface it lands on,
    /// including the diff washes - that is the whole point of tinting the row
    /// background rather than the text.
    #[test]
    fn text_is_readable_on_every_background() {
        for p in [Palette::light(), Palette::dark()] {
            for (name, bg) in [
                ("editor", p.editor_bg),
                ("gutter", p.gutter_bg),
                ("chrome", p.chrome_bg),
                ("deleted", p.del_bg),
                ("inserted", p.ins_bg),
                ("modified", p.mod_bg),
                ("del span", p.del_span),
                ("ins span", p.ins_span),
                ("search", p.search_bg),
            ] {
                let ratio = contrast(p.text, bg);
                assert!(
                    ratio >= 4.5,
                    "text on {name} is only {ratio:.2}:1 (need 4.5:1)"
                );
            }
        }
    }

    /// Dim text is used for labels and line numbers; hold it to AA for large
    /// text / non-essential UI (3:1).
    #[test]
    fn secondary_text_is_legible() {
        for p in [Palette::light(), Palette::dark()] {
            assert!(contrast(p.text_dim, p.chrome_bg) >= 4.5);
            assert!(contrast(p.text_faint, p.gutter_bg) >= 3.0);
        }
    }

    /// The three diff washes must be distinguishable from the editor
    /// background, or a change reads as unchanged.
    #[test]
    fn diff_backgrounds_are_visible() {
        for p in [Palette::light(), Palette::dark()] {
            for bg in [p.del_bg, p.ins_bg, p.mod_bg] {
                let d = (luminance(bg) - luminance(p.editor_bg)).abs();
                assert!(d > 0.004, "a diff wash is nearly invisible: {bg:?}");
            }
        }
    }

    /// Chromaticity: each channel as a fraction of the total, which strips out
    /// brightness and leaves hue. Raw RGB distance is the wrong measure here -
    /// two very dark colours are numerically close no matter how different
    /// they look.
    fn chromaticity(c: Color32) -> [f32; 3] {
        let sum = (c.r() as f32 + c.g() as f32 + c.b() as f32).max(1.0);
        [
            c.r() as f32 / sum,
            c.g() as f32 / sum,
            c.b() as f32 / sum,
        ]
    }

    fn hue_distance(a: Color32, b: Color32) -> f32 {
        let (x, y) = (chromaticity(a), chromaticity(b));
        (0..3).map(|i| (x[i] - y[i]).abs()).sum()
    }

    /// ...and from each other, which is what makes "modified" readable at a
    /// glance rather than looking like a delete next to an insert.
    #[test]
    fn diff_backgrounds_differ_from_one_another() {
        for p in [Palette::light(), Palette::dark()] {
            let pairs = [
                ("deleted/inserted", p.del_bg, p.ins_bg),
                ("deleted/modified", p.del_bg, p.mod_bg),
                ("inserted/modified", p.ins_bg, p.mod_bg),
            ];
            for (name, a, b) in pairs {
                let d = hue_distance(a, b);
                assert!(d > 0.06, "{name} washes share a hue ({d:.3}): {a:?} vs {b:?}");
            }
        }
    }

    /// The word-level span must stand out from the row wash it sits on.
    #[test]
    fn spans_stand_out_from_their_row() {
        for p in [Palette::light(), Palette::dark()] {
            assert!(contrast(p.del_span, p.del_bg) > 1.15);
            assert!(contrast(p.ins_span, p.ins_bg) > 1.15);
        }
    }

    #[test]
    fn row_and_marker_lookups_agree() {
        let p = Palette::dark();
        assert!(p.row_bg(RowKind::Equal).is_none());
        assert!(p.row_bg(RowKind::Ignored).is_none());
        assert_eq!(p.row_bg(RowKind::Delete), Some(p.del_bg));
        assert_eq!(p.row_bg(RowKind::Insert), Some(p.ins_bg));
        assert_eq!(p.row_bg(RowKind::Replace), Some(p.mod_bg));
        assert_eq!(p.marker(RowKind::Replace), Some(p.marker_mod));
        assert!(p.marker(RowKind::Equal).is_none());
        assert_eq!(p.span_bg(Side::Left), p.del_span);
        assert_eq!(p.span_bg(Side::Right), p.ins_span);
    }

    /// The bug this key exists to prevent: choosing Light explicitly, on a
    /// machine whose system theme is Dark, must register as a change.
    #[test]
    fn picking_a_theme_explicitly_changes_the_style_key() {
        let system = Some(egui::Theme::Dark);
        let following = StyleKey::new(ThemeMode::System, Lang::English, system);
        let explicit_light = StyleKey::new(ThemeMode::Light, Lang::English, system);
        let explicit_dark = StyleKey::new(ThemeMode::Dark, Lang::English, system);

        assert_ne!(following, explicit_light, "Light must trigger a rebuild");
        assert_eq!(
            following, explicit_dark,
            "Dark already matches the system here, so nothing needs rebuilding"
        );
    }

    #[test]
    fn the_style_key_tracks_the_language() {
        let system = Some(egui::Theme::Dark);
        assert_ne!(
            StyleKey::new(ThemeMode::Dark, Lang::English, system),
            StyleKey::new(ThemeMode::Dark, Lang::Chinese, system),
        );
    }

    #[test]
    fn the_style_key_follows_a_system_theme_change() {
        assert_ne!(
            StyleKey::new(ThemeMode::System, Lang::English, Some(egui::Theme::Light)),
            StyleKey::new(ThemeMode::System, Lang::English, Some(egui::Theme::Dark)),
        );
    }

    /// Switching between two settings that resolve the same way should not
    /// churn the style - the key is about the *result*, not the setting.
    #[test]
    fn an_equivalent_setting_does_not_force_a_rebuild() {
        let system = Some(egui::Theme::Light);
        assert_eq!(
            StyleKey::new(ThemeMode::System, Lang::English, system),
            StyleKey::new(ThemeMode::Light, Lang::English, system),
        );
    }

    #[test]
    fn appearance_follows_the_configured_mode() {
        assert_eq!(
            Appearance::resolve(ThemeMode::Light, Some(egui::Theme::Dark)),
            Appearance::Light
        );
        assert_eq!(
            Appearance::resolve(ThemeMode::Dark, Some(egui::Theme::Light)),
            Appearance::Dark
        );
        assert_eq!(
            Appearance::resolve(ThemeMode::System, Some(egui::Theme::Light)),
            Appearance::Light
        );
        assert_eq!(
            Appearance::resolve(ThemeMode::System, None),
            Appearance::Dark,
            "no system signal should fall back to dark"
        );
    }
}
