//! Persisted preferences.
//!
//! Stored as JSON next to the OS's other per-user config, so it survives
//! upgrades and can be inspected or deleted by hand. Nothing here leaves the
//! machine, and the file deliberately contains no document text - only the
//! *paths* of recently opened files.
//!
//! Loading is forgiving: a field added in a later version, or a file corrupted
//! by a half-finished write, falls back to defaults rather than refusing to
//! start.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::diff::{DiffOptions, Side};
use crate::i18n::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn i18n_key(self) -> &'static str {
        match self {
            Self::System => "view.theme_system",
            Self::Light => "view.theme_light",
            Self::Dark => "view.theme_dark",
        }
    }
}

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;
pub const MIN_LINE_HEIGHT: f32 = 1.0;
pub const MAX_LINE_HEIGHT: f32 = 2.4;

/// How many recent paths to keep per side.
const MAX_RECENT: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeMode,
    pub lang: Lang,

    pub font_size: f32,
    /// Multiplier on the font's natural height.
    pub line_height: f32,

    pub word_wrap: bool,
    pub sync_scroll: bool,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub syntax_highlighting: bool,
    pub show_overview: bool,

    /// Show one editor instead of two, turning the app into a plain text
    /// editor. The hidden side keeps its contents and comes back untouched.
    pub single_pane: bool,

    pub diff: DiffOptions,

    /// Left pane's share of the width, `0.15..=0.85`.
    pub split: f32,

    pub window_size: [f32; 2],
    pub window_maximized: bool,

    pub recent_files: Vec<PathBuf>,

    /// Which pane the Tools menu rewrites. Persisted because it is a working
    /// preference, not a per-document choice.
    pub tools_side: Side,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            lang: Lang::default(),
            font_size: 14.0,
            line_height: 1.45,
            word_wrap: false,
            sync_scroll: true,
            show_line_numbers: true,
            show_whitespace: false,
            syntax_highlighting: true,
            show_overview: true,
            single_pane: false,
            diff: DiffOptions::default(),
            split: 0.5,
            window_size: [1400.0, 900.0],
            window_maximized: false,
            recent_files: Vec::new(),
            tools_side: Side::Left,
        }
    }
}

impl Config {
    /// `%APPDATA%\DuiBi\config\config.json` on Windows, the XDG equivalent
    /// elsewhere.
    ///
    /// `DUIBI_CONFIG` overrides it entirely. That exists so automated runs can
    /// use a scratch file instead of overwriting the settings of whoever is
    /// actually using the machine.
    pub fn path() -> Option<PathBuf> {
        if let Some(over) = std::env::var_os("DUIBI_CONFIG") {
            return Some(PathBuf::from(over));
        }
        directories::ProjectDirs::from("", "", "DuiBi")
            .map(|d| d.config_dir().join("config.json"))
    }

    /// Load, falling back to defaults on any problem.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(cfg) => cfg.sanitized(),
            Err(e) => {
                // Do not delete the file - the user may want to look at it.
                eprintln!("config: ignoring {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write to a sibling then rename, so a crash mid-write cannot leave a
        // truncated config behind.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Clamp anything a hand-edited file could have put out of range.
    ///
    /// A NaN split or a zero font size would otherwise produce an unusable
    /// window with no way back short of deleting the file.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();
        self.font_size = clamp_or(self.font_size, MIN_FONT_SIZE, MAX_FONT_SIZE, d.font_size);
        self.line_height = clamp_or(
            self.line_height,
            MIN_LINE_HEIGHT,
            MAX_LINE_HEIGHT,
            d.line_height,
        );
        self.split = clamp_or(self.split, 0.15, 0.85, d.split);
        self.window_size[0] = clamp_or(self.window_size[0], 480.0, 16384.0, d.window_size[0]);
        self.window_size[1] = clamp_or(self.window_size[1], 320.0, 16384.0, d.window_size[1]);
        self.diff.align_threshold = clamp_or(self.diff.align_threshold, 0.0, 1.0, 0.35);
        self.recent_files.truncate(MAX_RECENT);
        self
    }

    /// Record a freshly opened file, most recent first, without duplicates.
    pub fn push_recent(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(MAX_RECENT);
    }

    /// Directory to start the next file dialog in.
    pub fn last_dir(&self) -> Option<PathBuf> {
        self.recent_files
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
    }
}

/// `clamp`, but NaN falls back to `fallback` instead of propagating.
fn clamp_or(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.font_size, Config::default().font_size);
        assert_eq!(back.diff, Config::default().diff);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Simulates a config written by an older build.
        let cfg: Config = serde_json::from_str(r#"{"font_size": 18.0}"#).unwrap();
        assert_eq!(cfg.font_size, 18.0);
        assert_eq!(cfg.line_height, Config::default().line_height);
        assert!(cfg.sync_scroll);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let cfg: Config =
            serde_json::from_str(r#"{"font_size": 12.0, "from_the_future": true}"#).unwrap();
        assert_eq!(cfg.font_size, 12.0);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let cfg = Config {
            font_size: 900.0,
            line_height: 0.01,
            split: 2.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(cfg.font_size, MAX_FONT_SIZE);
        assert_eq!(cfg.line_height, MIN_LINE_HEIGHT);
        assert_eq!(cfg.split, 0.85);
    }

    #[test]
    fn nan_falls_back_instead_of_poisoning_the_layout() {
        let cfg = Config {
            split: f32::NAN,
            font_size: f32::INFINITY,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(cfg.split, 0.5);
        assert_eq!(cfg.font_size, 14.0);
    }

    #[test]
    fn recent_files_are_deduped_and_capped() {
        let mut cfg = Config::default();
        for i in 0..20 {
            cfg.push_recent(Path::new(&format!("/tmp/f{i}.txt")));
        }
        assert_eq!(cfg.recent_files.len(), MAX_RECENT);
        assert_eq!(cfg.recent_files[0], PathBuf::from("/tmp/f19.txt"));

        cfg.push_recent(Path::new("/tmp/f19.txt"));
        assert_eq!(cfg.recent_files[0], PathBuf::from("/tmp/f19.txt"));
        assert_eq!(
            cfg.recent_files.iter().filter(|p| p.ends_with("f19.txt")).count(),
            1,
            "re-opening a file must not duplicate it"
        );
    }

    #[test]
    fn last_dir_comes_from_the_most_recent_file() {
        let mut cfg = Config::default();
        assert!(cfg.last_dir().is_none());
        cfg.push_recent(Path::new("/projects/a/notes.txt"));
        assert_eq!(cfg.last_dir(), Some(PathBuf::from("/projects/a")));
    }

    #[test]
    fn language_tags_are_stable_in_json() {
        // The serialized names are part of the on-disk format.
        assert_eq!(serde_json::to_string(&Lang::Chinese).unwrap(), "\"zh-CN\"");
        assert_eq!(serde_json::to_string(&Lang::English).unwrap(), "\"en\"");
    }
}
