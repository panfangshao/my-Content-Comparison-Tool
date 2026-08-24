//! Menu bar and toolbar.
//!
//! Everything the app can be told to do arrives back as an [`Action`], so the
//! menus stay declarative and the application logic lives in one place rather
//! than being scattered through closures.

use egui::{RichText, Ui};

use crate::config::{Config, MAX_FONT_SIZE, MIN_FONT_SIZE, ThemeMode};
use crate::core::diff::{DiffAlgorithm, Granularity, Side, WhitespaceMode};
use crate::core::merge::Direction;
use crate::core::text::CleanupOp;
use crate::i18n::{Lang, t};

/// A request from the user. Collected during the frame, applied after it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Open(Side),
    OpenRecent(std::path::PathBuf, Side),
    ClearRecent,
    Save(Side),
    SaveAs(Side),
    ExportUnified,
    CopyUnified,
    CopyText(Side),
    Undo(Side),
    Redo(Side),
    SelectAll(Side),
    Clear(Side),
    Swap,
    Find,
    Replace,
    Cleanup(CleanupOp, Side),
    MergeAll(Direction),
    /// Emitted by the confirmation dialog, never directly by a menu.
    MergeAllConfirmed(Direction),
    NextDiff,
    PrevDiff,
    FirstDiff,
    LastDiff,
    ToggleShortcuts,
    ResetLayout,
    Quit,
}

/// Read-only facts the menus need in order to enable or disable items.
pub struct MenuFacts {
    pub can_undo: [bool; 2],
    pub can_redo: [bool; 2],
    pub has_diffs: bool,
    pub focus_side: Side,
    pub recent: Vec<std::path::PathBuf>,
    pub single_pane: bool,
}

/// Draw the menu bar and the toolbar row beneath it.
pub fn show_toolbar(
    ui: &mut Ui,
    config: &mut Config,
    facts: &MenuFacts,
    lang: Lang,
    actions: &mut Vec<Action>,
) {
    // In single-document mode the comparison simply does not exist, so the
    // menus that act on it are hidden rather than disabled - a menu full of
    // greyed-out entries is worse than one that is honestly shorter.
    let single = config.single_pane;

    egui::MenuBar::new().ui(ui, |ui| {
        file_menu(ui, config, facts, lang, actions);
        edit_menu(ui, facts, lang, actions);
        view_menu(ui, config, lang, actions);
        if !single {
            compare_menu(ui, config, lang);
            merge_menu(ui, facts, lang, actions);
        }
        tools_menu(ui, config, lang, actions);
    });

    ui.add_space(2.0);
    quick_row(ui, config, facts, lang, actions);
}

fn file_menu(
    ui: &mut Ui,
    config: &mut Config,
    facts: &MenuFacts,
    lang: Lang,
    actions: &mut Vec<Action>,
) {
    let single = config.single_pane;

    ui.menu_button(t(lang, "file.menu"), |ui| {
        if single {
            if shortcut_item(ui, t(lang, "file.open"), "Ctrl+O").clicked() {
                actions.push(Action::Open(Side::Left));
                ui.close();
            }
        } else {
            if ui.button(t(lang, "file.open_left")).clicked() {
                actions.push(Action::Open(Side::Left));
                ui.close();
            }
            if ui.button(t(lang, "file.open_right")).clicked() {
                actions.push(Action::Open(Side::Right));
                ui.close();
            }
        }

        ui.separator();
        ui.menu_button(t(lang, "file.recent"), |ui| {
            if facts.recent.is_empty() {
                ui.add_enabled(false, egui::Button::new(t(lang, "file.recent_empty")));
                return;
            }
            for path in &facts.recent {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_owned();

                if single {
                    // One pane: opening is a single click, not a side to pick.
                    if ui
                        .button(name)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        actions.push(Action::OpenRecent(path.clone(), Side::Left));
                        ui.close();
                    }
                    continue;
                }

                ui.menu_button(name, |ui| {
                    ui.label(
                        RichText::new(path.display().to_string())
                            .small()
                            .weak(),
                    );
                    ui.separator();
                    if ui.button(t(lang, "app.left")).clicked() {
                        actions.push(Action::OpenRecent(path.clone(), Side::Left));
                        ui.close();
                    }
                    if ui.button(t(lang, "app.right")).clicked() {
                        actions.push(Action::OpenRecent(path.clone(), Side::Right));
                        ui.close();
                    }
                });
            }
            ui.separator();
            if ui.button(t(lang, "file.clear_recent")).clicked() {
                actions.push(Action::ClearRecent);
                ui.close();
            }
        });

        ui.separator();
        if single {
            if shortcut_item(ui, t(lang, "file.save"), "Ctrl+S").clicked() {
                actions.push(Action::Save(Side::Left));
                ui.close();
            }
            if ui.button(t(lang, "file.save_as")).clicked() {
                actions.push(Action::SaveAs(Side::Left));
                ui.close();
            }
        } else {
            if shortcut_item(ui, t(lang, "file.save_left"), "Ctrl+S").clicked() {
                actions.push(Action::Save(Side::Left));
                ui.close();
            }
            if ui.button(t(lang, "file.save_right")).clicked() {
                actions.push(Action::Save(Side::Right));
                ui.close();
            }
            if ui.button(t(lang, "file.save_left_as")).clicked() {
                actions.push(Action::SaveAs(Side::Left));
                ui.close();
            }
            if ui.button(t(lang, "file.save_right_as")).clicked() {
                actions.push(Action::SaveAs(Side::Right));
                ui.close();
            }
        }

        ui.separator();
        let enabled = facts.has_diffs && !single;
        if ui
            .add_enabled(enabled, egui::Button::new(t(lang, "file.copy_unified")))
            .clicked()
        {
            actions.push(Action::CopyUnified);
            ui.close();
        }
        if ui
            .add_enabled(enabled, egui::Button::new(t(lang, "file.export_unified")))
            .clicked()
        {
            actions.push(Action::ExportUnified);
            ui.close();
        }

        ui.separator();
        if ui.button(t(lang, "file.exit")).clicked() {
            actions.push(Action::Quit);
            ui.close();
        }
    });
}

fn edit_menu(ui: &mut Ui, facts: &MenuFacts, lang: Lang, actions: &mut Vec<Action>) {
    let side = facts.focus_side;
    let i = usize::from(side == Side::Right);

    ui.menu_button(t(lang, "edit.menu"), |ui| {
        if ui
            .add_enabled(
                facts.can_undo[i],
                egui::Button::new(t(lang, "edit.undo")).shortcut_text("Ctrl+Z"),
            )
            .clicked()
        {
            actions.push(Action::Undo(side));
            ui.close();
        }
        if ui
            .add_enabled(
                facts.can_redo[i],
                egui::Button::new(t(lang, "edit.redo")).shortcut_text("Ctrl+Shift+Z"),
            )
            .clicked()
        {
            actions.push(Action::Redo(side));
            ui.close();
        }

        ui.separator();
        if shortcut_item(ui, t(lang, "edit.select_all"), "Ctrl+A").clicked() {
            actions.push(Action::SelectAll(side));
            ui.close();
        }
        if ui.button(t(lang, "edit.copy")).clicked() {
            actions.push(Action::CopyText(side));
            ui.close();
        }
        if ui.button(t(lang, "edit.clear")).clicked() {
            actions.push(Action::Clear(side));
            ui.close();
        }

        if !facts.single_pane {
            ui.separator();
            if shortcut_item(ui, t(lang, "edit.swap"), "Ctrl+Shift+X").clicked() {
                actions.push(Action::Swap);
                ui.close();
            }
        }

        ui.separator();
        if shortcut_item(ui, t(lang, "edit.find"), "Ctrl+F").clicked() {
            actions.push(Action::Find);
            ui.close();
        }
        if shortcut_item(ui, t(lang, "edit.replace"), "Ctrl+H").clicked() {
            actions.push(Action::Replace);
            ui.close();
        }
    });
}

fn view_menu(ui: &mut Ui, config: &mut Config, lang: Lang, actions: &mut Vec<Action>) {
    ui.menu_button(t(lang, "view.menu"), |ui| {
        ui.menu_button(t(lang, "view.theme"), |ui| {
            for mode in ThemeMode::ALL {
                ui.radio_value(&mut config.theme, mode, t(lang, mode.i18n_key()));
            }
        });
        ui.menu_button(t(lang, "view.language"), |ui| {
            for l in Lang::ALL {
                ui.radio_value(&mut config.lang, l, l.native_name());
            }
        });

        ui.separator();
        ui.checkbox(&mut config.single_pane, t(lang, "view.single_pane"))
            .on_hover_text(t(lang, "view.single_pane.hint"));

        ui.separator();
        ui.checkbox(&mut config.word_wrap, t(lang, "view.word_wrap"));
        ui.checkbox(&mut config.show_line_numbers, t(lang, "view.line_numbers"));
        ui.checkbox(&mut config.show_whitespace, t(lang, "view.whitespace"));
        ui.checkbox(&mut config.syntax_highlighting, t(lang, "view.syntax"));
        // Both of these describe the relationship between two panes.
        ui.add_enabled_ui(!config.single_pane, |ui| {
            ui.checkbox(&mut config.sync_scroll, t(lang, "view.sync_scroll"));
            ui.checkbox(&mut config.show_overview, t(lang, "view.minimap"));
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(t(lang, "view.font_size"));
            ui.add(
                egui::DragValue::new(&mut config.font_size)
                    .range(MIN_FONT_SIZE..=MAX_FONT_SIZE)
                    .speed(0.25)
                    .fixed_decimals(0),
            );
        });
        ui.horizontal(|ui| {
            ui.label(t(lang, "view.line_height"));
            ui.add(
                egui::DragValue::new(&mut config.line_height)
                    .range(1.0..=2.4)
                    .speed(0.02)
                    .fixed_decimals(2),
            );
        });

        ui.separator();
        if ui.button(t(lang, "view.reset_layout")).clicked() {
            actions.push(Action::ResetLayout);
            ui.close();
        }
        if ui.button(t(lang, "shortcuts.title")).clicked() {
            actions.push(Action::ToggleShortcuts);
            ui.close();
        }
    });
}

fn compare_menu(ui: &mut Ui, config: &mut Config, lang: Lang) {
    ui.menu_button(t(lang, "cmp.menu"), |ui| {
        ui.menu_button(t(lang, "cmp.algorithm"), |ui| {
            for alg in DiffAlgorithm::ALL {
                ui.radio_value(&mut config.diff.algorithm, alg, alg.label());
            }
        });
        ui.menu_button(t(lang, "cmp.granularity"), |ui| {
            for g in Granularity::ALL {
                let key = match g {
                    Granularity::Line => "cmp.granularity.line",
                    Granularity::Word => "cmp.granularity.word",
                    Granularity::Char => "cmp.granularity.char",
                };
                ui.radio_value(&mut config.diff.granularity, g, t(lang, key));
            }
        });
        ui.menu_button(t(lang, "cmp.whitespace"), |ui| {
            for w in WhitespaceMode::ALL {
                let key = match w {
                    WhitespaceMode::Exact => "cmp.ws.exact",
                    WhitespaceMode::IgnoreTrailing => "cmp.ws.trailing",
                    WhitespaceMode::IgnoreAmount => "cmp.ws.amount",
                    WhitespaceMode::IgnoreAll => "cmp.ws.all",
                };
                ui.radio_value(&mut config.diff.whitespace, w, t(lang, key));
            }
        });

        ui.separator();
        ui.checkbox(&mut config.diff.ignore_case, t(lang, "cmp.ignore_case"));
        ui.checkbox(&mut config.diff.ignore_blank_lines, t(lang, "cmp.ignore_blank"));
        ui.checkbox(&mut config.diff.smart_align, t(lang, "cmp.smart_align"))
            .on_hover_text(t(lang, "cmp.smart_align.hint"));
    });
}

fn merge_menu(ui: &mut Ui, facts: &MenuFacts, lang: Lang, actions: &mut Vec<Action>) {
    ui.menu_button(t(lang, "merge.menu"), |ui| {
        let on = facts.has_diffs;
        if ui
            .add_enabled(on, egui::Button::new(t(lang, "merge.all_to_right")))
            .clicked()
        {
            actions.push(Action::MergeAll(Direction::ToRight));
            ui.close();
        }
        if ui
            .add_enabled(on, egui::Button::new(t(lang, "merge.all_to_left")))
            .clicked()
        {
            actions.push(Action::MergeAll(Direction::ToLeft));
            ui.close();
        }

        ui.separator();
        if ui
            .add_enabled(
                on,
                egui::Button::new(t(lang, "nav.next_diff")).shortcut_text("F3"),
            )
            .clicked()
        {
            actions.push(Action::NextDiff);
            ui.close();
        }
        if ui
            .add_enabled(
                on,
                egui::Button::new(t(lang, "nav.prev_diff")).shortcut_text("Shift+F3"),
            )
            .clicked()
        {
            actions.push(Action::PrevDiff);
            ui.close();
        }
    });
}

/// How long to hover a tool before its description appears.
///
/// Long enough that running down the list to find a familiar entry does not
/// throw a wall of text at you, short enough to feel like an answer when you
/// actually stop on something.
const TOOL_HELP_DELAY: f32 = 2.0;

/// The Tools menu.
///
/// # Why the tools are flat buttons rather than submenus
///
/// Each tool used to open a submenu holding "Left" / "Right". That cannot carry
/// a hover description: egui refuses to draw a tooltip for any widget whose
/// layer has an open popup, and hovering a submenu button *is* opening a popup,
/// so the tip would never appear. Hoisting the side into a single choice at the
/// top makes every tool a plain button - which can be described on hover, and
/// which also cuts applying one from two clicks to one.
fn tools_menu(ui: &mut Ui, config: &mut Config, lang: Lang, actions: &mut Vec<Action>) {
    ui.menu_button(t(lang, "tools.menu"), |ui| {
        // Tooltip timing is read from the *global* style, so `ui.style_mut()`
        // would have no effect here. Set it and put it back: only this menu
        // wants a long delay, since the icon-only toolbar buttons depend on
        // their tips appearing promptly.
        let previous_delay = ui.ctx().global_style().interaction.tooltip_delay;
        ui.ctx()
            .global_style_mut(|s| s.interaction.tooltip_delay = TOOL_HELP_DELAY);

        ui.label(RichText::new(t(lang, "tools.hint")).small().weak());
        if !config.single_pane {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(t(lang, "tools.apply_to"));
                ui.selectable_value(&mut config.tools_side, Side::Left, t(lang, "app.left"));
                ui.selectable_value(&mut config.tools_side, Side::Right, t(lang, "app.right"));
            });
        }
        ui.separator();

        for op in CleanupOp::ALL {
            let clicked = ui
                .button(t(lang, op.i18n_key()))
                .on_hover_ui(|ui| {
                    // Bounded, or a long description becomes one very wide line.
                    ui.set_max_width(360.0);
                    ui.label(RichText::new(t(lang, op.i18n_key())).strong());
                    ui.add_space(2.0);
                    ui.label(t(lang, op.help_key()));
                })
                .clicked();
            if clicked {
                let side = if config.single_pane {
                    Side::Left
                } else {
                    config.tools_side
                };
                actions.push(Action::Cleanup(op, side));
                ui.close();
            }
        }

        ui.ctx()
            .global_style_mut(|s| s.interaction.tooltip_delay = previous_delay);
    });
}

/// The always-visible row of frequently used controls.
///
/// Glyphs are restricted to characters the bundled font actually carries.
/// Anything more exotic (dingbat arrows, emoji) renders as a tofu box on a
/// default install, so arrows are built by repetition instead.
fn quick_row(
    ui: &mut Ui,
    config: &mut Config,
    facts: &MenuFacts,
    lang: Lang,
    actions: &mut Vec<Action>,
) {
    if config.single_pane {
        ui.horizontal(|ui| {
            if ui
                .button(t(lang, "edit.find"))
                .on_hover_text(format!("{} (Ctrl+F)", t(lang, "edit.find")))
                .clicked()
            {
                actions.push(Action::Find);
            }
            ui.separator();
            ui.checkbox(&mut config.word_wrap, t(lang, "view.word_wrap"));
            ui.checkbox(&mut config.show_line_numbers, t(lang, "view.line_numbers"));
        });
        return;
    }

    ui.horizontal(|ui| {
        let on = facts.has_diffs;
        if ui
            .add_enabled(on, egui::Button::new("\u{2191}\u{2191}").small())
            .on_hover_text(t(lang, "nav.first_diff"))
            .clicked()
        {
            actions.push(Action::FirstDiff);
        }
        if ui
            .add_enabled(on, egui::Button::new("\u{2191}").small())
            .on_hover_text(format!("{} (Shift+F3)", t(lang, "nav.prev_diff")))
            .clicked()
        {
            actions.push(Action::PrevDiff);
        }
        if ui
            .add_enabled(on, egui::Button::new("\u{2193}").small())
            .on_hover_text(format!("{} (F3)", t(lang, "nav.next_diff")))
            .clicked()
        {
            actions.push(Action::NextDiff);
        }
        if ui
            .add_enabled(on, egui::Button::new("\u{2193}\u{2193}").small())
            .on_hover_text(t(lang, "nav.last_diff"))
            .clicked()
        {
            actions.push(Action::LastDiff);
        }

        ui.separator();

        if ui
            .button("\u{2194}")
            .on_hover_text(format!("{} (Ctrl+Shift+X)", t(lang, "edit.swap")))
            .clicked()
        {
            actions.push(Action::Swap);
        }
        if ui
            .button(t(lang, "edit.find"))
            .on_hover_text(format!("{} (Ctrl+F)", t(lang, "edit.find")))
            .clicked()
        {
            actions.push(Action::Find);
        }

        ui.separator();

        // Granularity is the control people reach for most, so it gets a
        // permanent home rather than living two menus deep.
        for g in Granularity::ALL {
            let key = match g {
                Granularity::Line => "cmp.granularity.line",
                Granularity::Word => "cmp.granularity.word",
                Granularity::Char => "cmp.granularity.char",
            };
            if ui
                .selectable_label(config.diff.granularity == g, t(lang, key))
                .clicked()
            {
                config.diff.granularity = g;
            }
        }

        ui.separator();
        ui.checkbox(&mut config.diff.ignore_case, t(lang, "cmp.ignore_case"));
        ui.checkbox(&mut config.word_wrap, t(lang, "view.word_wrap"));
        ui.checkbox(&mut config.sync_scroll, t(lang, "view.sync_scroll"));
    });
}

/// A menu item with its shortcut right-aligned.
fn shortcut_item(ui: &mut Ui, label: &str, shortcut: &str) -> egui::Response {
    ui.add(egui::Button::new(label).shortcut_text(shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_carry_the_side_they_apply_to() {
        assert_ne!(Action::Undo(Side::Left), Action::Undo(Side::Right));
        assert_eq!(Action::Save(Side::Left), Action::Save(Side::Left));
    }

    #[test]
    fn merge_actions_distinguish_direction() {
        assert_ne!(
            Action::MergeAll(Direction::ToLeft),
            Action::MergeAll(Direction::ToRight)
        );
    }

    /// Every menu entry the toolbar renders must resolve to a real label in
    /// both languages, or the UI shows blanks.
    #[test]
    fn every_menu_label_is_translated() {
        let keys = [
            "file.menu",
            "file.open_left",
            "file.open_right",
            "file.save_left",
            "file.save_right",
            "file.save_left_as",
            "file.save_right_as",
            "file.recent",
            "file.recent_empty",
            "file.clear_recent",
            "file.copy_unified",
            "file.export_unified",
            "file.exit",
            "edit.menu",
            "edit.undo",
            "edit.redo",
            "edit.copy",
            "edit.select_all",
            "edit.clear",
            "edit.swap",
            "edit.find",
            "edit.replace",
            "view.menu",
            "view.theme",
            "view.language",
            "view.word_wrap",
            "view.sync_scroll",
            "view.line_numbers",
            "view.whitespace",
            "view.syntax",
            "view.minimap",
            "view.font_size",
            "view.line_height",
            "view.reset_layout",
            "cmp.menu",
            "cmp.algorithm",
            "cmp.granularity",
            "cmp.granularity.line",
            "cmp.granularity.word",
            "cmp.granularity.char",
            "cmp.whitespace",
            "cmp.ws.exact",
            "cmp.ws.trailing",
            "cmp.ws.amount",
            "cmp.ws.all",
            "cmp.ignore_case",
            "cmp.ignore_blank",
            "cmp.smart_align",
            "cmp.smart_align.hint",
            "merge.menu",
            "merge.all_to_right",
            "merge.all_to_left",
            "nav.next_diff",
            "nav.prev_diff",
            "nav.first_diff",
            "nav.last_diff",
            "tools.menu",
            "shortcuts.title",
            "app.left",
            "app.right",
        ];
        for key in keys {
            for lang in [Lang::Chinese, Lang::English] {
                assert!(!t(lang, key).is_empty(), "{key} is blank in {lang:?}");
            }
        }
    }

    /// Every tool shows a description on hover, so a missing one would leave a
    /// blank tooltip - worse than none at all.
    #[test]
    fn every_tool_has_a_description_in_both_languages() {
        for op in CleanupOp::ALL {
            for lang in [Lang::Chinese, Lang::English] {
                let help = t(lang, op.help_key());
                assert!(!help.is_empty(), "{op:?} has no help in {lang:?}");
                assert!(
                    help.chars().count() > 12,
                    "{op:?} help in {lang:?} is too thin to be useful: {help:?}"
                );
                assert_ne!(
                    help,
                    t(lang, op.i18n_key()),
                    "{op:?} help just repeats its label"
                );
            }
        }
        for lang in [Lang::Chinese, Lang::English] {
            assert!(!t(lang, "tools.hint").is_empty());
        }
    }

    /// Theme modes are rendered from `ALL`, so each needs a label too.
    #[test]
    fn every_theme_mode_is_translated() {
        for mode in ThemeMode::ALL {
            for lang in [Lang::Chinese, Lang::English] {
                assert!(!t(lang, mode.i18n_key()).is_empty(), "{mode:?}");
            }
        }
    }
}
