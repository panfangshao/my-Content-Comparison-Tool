//! Interface language.
//!
//! A `match` on the key rather than a hash map: the strings end up in `.rodata`,
//! lookup is a compare chain the optimizer handles well, and a missing key is a
//! visible `⟨key⟩` in the UI rather than a silent blank.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Lang {
    /// Follow the OS language, falling back to English.
    #[default]
    System,
    #[serde(rename = "zh-CN")]
    Chinese,
    #[serde(rename = "en")]
    English,
}

impl Lang {
    pub const ALL: [Self; 3] = [Self::System, Self::Chinese, Self::English];

    /// The name of the language *in* that language, which is how language
    /// pickers should always be labelled.
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::System => "System / 跟随系统",
            Self::Chinese => "简体中文",
            Self::English => "English",
        }
    }

    /// Resolve `System` against the OS locale.
    pub fn resolve(self) -> Self {
        match self {
            Self::System if system_prefers_chinese() => Self::Chinese,
            Self::System => Self::English,
            other => other,
        }
    }
}

/// Does the operating system want Chinese?
///
/// Windows does not set `LANG`/`LC_ALL`, so the environment-variable sniff that
/// works everywhere else always answered "no" there. Both the display language
/// and the regional format are consulted, because a machine can easily be set
/// to an English UI with a Chinese locale, or the other way round - and either
/// is a good reason to open in Chinese.
#[cfg(windows)]
fn system_prefers_chinese() -> bool {
    // Both live in kernel32 and take no arguments, so declaring them here is
    // cheaper than taking a dependency on a Win32 binding crate.
    unsafe extern "system" {
        /// The Windows display language.
        fn GetUserDefaultUILanguage() -> u16;
        /// The user's locale ("Region" in Settings).
        fn GetUserDefaultLangID() -> u16;
    }

    /// Low 10 bits of a LANGID are the primary language; 0x04 is Chinese,
    /// covering zh-CN, zh-TW, zh-HK and the rest.
    const LANG_CHINESE: u16 = 0x04;
    let primary = |id: u16| id & 0x03FF;

    // SAFETY: both are argument-less kernel32 calls returning a plain integer.
    let (ui, locale) = unsafe { (GetUserDefaultUILanguage(), GetUserDefaultLangID()) };
    primary(ui) == LANG_CHINESE || primary(locale) == LANG_CHINESE
}

/// Best-effort OS language sniff from the usual environment variables.
#[cfg(not(windows))]
fn system_prefers_chinese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .any(|v| v.to_ascii_lowercase().starts_with("zh"))
}

/// Look up `key` in the current language.
pub fn t(lang: Lang, key: &str) -> &'static str {
    let (zh, en) = lookup(key);
    match lang.resolve() {
        Lang::Chinese => zh,
        _ => en,
    }
}

/// `(Chinese, English)` for a key. Unknown keys render as `⟨key⟩` so a typo is
/// obvious on screen instead of showing an empty label.
fn lookup(key: &str) -> (&'static str, &'static str) {
    match key {
        // ---- Application ------------------------------------------------
        "app.title" => ("对比 - 文本比较与合并", "DuiBi - Text Compare & Merge"),
        "app.left" => ("左侧", "Left"),
        "app.right" => ("右侧", "Right"),
        "app.untitled" => ("未命名", "Untitled"),

        // ---- File -------------------------------------------------------
        "file.menu" => ("文件", "File"),
        "file.open_left" => ("打开左侧文件…", "Open Left File…"),
        "file.open_right" => ("打开右侧文件…", "Open Right File…"),
        "file.save_left" => ("保存左侧", "Save Left"),
        "file.save_right" => ("保存右侧", "Save Right"),
        "file.save_left_as" => ("左侧另存为…", "Save Left As…"),
        "file.save_right_as" => ("右侧另存为…", "Save Right As…"),
        "file.recent" => ("最近打开", "Recent Files"),
        "file.recent_empty" => ("(暂无记录)", "(nothing yet)"),
        "file.clear_recent" => ("清除历史", "Clear History"),
        "file.export_unified" => ("导出 Unified Diff…", "Export Unified Diff…"),
        "file.copy_unified" => ("复制 Unified Diff", "Copy Unified Diff"),
        "file.open" => ("打开文件…", "Open File…"),
        "file.save" => ("保存", "Save"),
        "file.save_as" => ("另存为…", "Save As…"),
        "file.exit" => ("退出", "Exit"),

        // ---- Edit -------------------------------------------------------
        "edit.menu" => ("编辑", "Edit"),
        "edit.undo" => ("撤销", "Undo"),
        "edit.redo" => ("重做", "Redo"),
        "edit.cut" => ("剪切", "Cut"),
        "edit.copy" => ("复制", "Copy"),
        "edit.paste" => ("粘贴", "Paste"),
        "edit.select_all" => ("全选", "Select All"),
        "edit.clear" => ("清空", "Clear"),
        "edit.swap" => ("交换左右", "Swap Sides"),
        "edit.find" => ("查找", "Find"),
        "edit.replace" => ("替换", "Replace"),

        // ---- View -------------------------------------------------------
        "view.menu" => ("视图", "View"),
        "view.theme" => ("主题", "Theme"),
        "view.theme_light" => ("浅色", "Light"),
        "view.theme_dark" => ("深色", "Dark"),
        "view.theme_system" => ("跟随系统", "Follow System"),
        "view.language" => ("语言", "Language"),
        "view.word_wrap" => ("自动换行", "Word Wrap"),
        "view.sync_scroll" => ("同步滚动", "Sync Scroll"),
        "view.line_numbers" => ("行号", "Line Numbers"),
        "view.whitespace" => ("显示空白字符", "Show Whitespace"),
        "view.syntax" => ("语法高亮", "Syntax Highlighting"),
        "view.minimap" => ("差异概览条", "Diff Overview"),
        "view.font_size" => ("字号", "Font Size"),
        "view.line_height" => ("行高", "Line Height"),
        "view.reset_layout" => ("重置布局", "Reset Layout"),
        "view.caret_line_end" => ("上下键移到行尾", "Up/Down go to line end"),
        "view.caret_line_end.hint" => (
            "勾选后，按上/下键会落到目标行的行尾。\n不勾选则保持列位置——穿过一个短行再出来，光标回到原来的列，这是大多数编辑器的行为。",
            "With this on, Up and Down land at the end of the line moved to.\nWith it off the column is kept, so moving through a short line and out the other side returns to the column you started in - the way most editors behave.",
        ),
        "view.single_pane" => ("单文件内容", "Single Document"),
        "view.single_pane.hint" => (
            "只保留一个文本框，当作普通文本编辑器使用；取消勾选即可回到左右对比。另一侧的内容会保留。",
            "Keep a single editor and use it as an ordinary text editor. Unchecking returns to side-by-side comparison; the other side keeps its contents.",
        ),

        // ---- Compare options --------------------------------------------
        "cmp.menu" => ("对比", "Compare"),
        "cmp.algorithm" => ("算法", "Algorithm"),
        "cmp.granularity" => ("差异高亮", "Difference highlighting"),
        "cmp.granularity.line" => ("整行", "Whole line"),
        "cmp.granularity.word" => ("按词", "By word"),
        "cmp.granularity.char" => ("按字符", "By character"),
        "cmp.granularity.short" => ("高亮", "Highlight"),
        "cmp.granularity.hint" => (
            "只决定改动怎么涂色，不影响光标移动。光标行为见 视图 → 上下键移到行尾。",
            "Only decides how a change is painted; it does not affect the caret. For that see View - Up/Down go to line end.",
        ),
        "cmp.whitespace" => ("空白处理", "Whitespace"),
        "cmp.ws.exact" => ("精确比较", "Exact"),
        "cmp.ws.trailing" => ("忽略首尾空白", "Ignore Leading/Trailing"),
        "cmp.ws.amount" => ("忽略空白数量", "Ignore Amount"),
        "cmp.ws.all" => ("忽略所有空白", "Ignore All Whitespace"),
        "cmp.ignore_case" => ("忽略大小写", "Ignore Case"),
        "cmp.ignore_blank" => ("忽略空行", "Ignore Blank Lines"),
        "cmp.smart_align" => ("智能对齐", "Smart Alignment"),
        "cmp.smart_align.hint" => (
            "在修改块内按相似度配对行，而不是简单按位置对齐",
            "Pair lines inside a changed block by similarity instead of position",
        ),

        // ---- Merge / navigation -----------------------------------------
        "merge.menu" => ("合并", "Merge"),
        "merge.to_right" => ("合并到右侧", "Merge to Right"),
        "merge.to_left" => ("合并到左侧", "Merge to Left"),
        "merge.all_to_right" => ("全部合并到右侧", "Accept All to Right"),
        "merge.all_to_left" => ("全部合并到左侧", "Accept All to Left"),
        "merge.confirm_all" => (
            "这会用一侧的内容覆盖另一侧的全部差异，确定吗？",
            "This overwrites every difference on the other side. Continue?",
        ),
        "nav.prev_diff" => ("上一处差异", "Previous Difference"),
        "nav.next_diff" => ("下一处差异", "Next Difference"),
        "nav.first_diff" => ("第一处差异", "First Difference"),
        "nav.last_diff" => ("最后一处差异", "Last Difference"),

        // ---- Tools ------------------------------------------------------
        "tools.menu" => ("工具", "Tools"),
        "tools.apply_to" => ("应用到", "Apply to"),
        "tool.remove_duplicates" => ("移除重复行", "Remove Duplicate Lines"),
        "tool.remove_empty" => ("删除空行", "Remove Empty Lines"),
        "tool.squeeze_blank" => ("合并连续空行", "Squeeze Blank Lines"),
        "tool.trim_leading" => ("去除行首空白", "Trim Leading Whitespace"),
        "tool.trim_trailing" => ("去除行尾空白", "Trim Trailing Whitespace"),
        "tool.trim_both" => ("去除首尾空白", "Trim Whitespace"),
        "tool.newlines_to_spaces" => ("换行替换为空格", "Newlines to Spaces"),
        "tool.sort_unique" => ("排序并去重", "Sort and Deduplicate"),
        "tool.normalize_ws" => ("规范化空白", "Normalize Whitespace"),

        // ---- Tool descriptions (shown on hover) --------------------------
        "tools.hint" => (
            "以下每项都会重写所选一侧的全部内容，可用 Ctrl+Z 一次撤销",
            "Each tool rewrites one whole side, and undoes in a single Ctrl+Z",
        ),
        "tool.remove_duplicates.help" => (
            "删除与前面完全相同的行，只保留第一次出现的那一行；行的先后顺序不变。\n例：a b a c  →  a b c",
            "Delete lines identical to an earlier one, keeping the first occurrence. Order is preserved.\ne.g. a b a c  →  a b c",
        ),
        "tool.sort_unique.help" => (
            "先按字典序排序，再删除重复的行。相当于「排序」+「移除重复行」。\n例：c a b a  →  a b c",
            "Sort, then drop duplicates - the two tools in one pass.\ne.g. c a b a  →  a b c",
        ),
        "tool.remove_empty.help" => (
            "删除空行，以及只含空格或制表符的行。\n注意：另一侧仍有这些行时，对比视图会用斜纹占位行保持对齐——那不是残留的空行。",
            "Delete blank lines, including ones holding only spaces or tabs.\nNote: while the other side still has them, hatched filler keeps the panes aligned - that is not a leftover blank line.",
        ),
        "tool.squeeze_blank.help" => (
            "连续的多个空行压缩成一个；单独的空行保持不变。\n例：a、空、空、空、b  →  a、空、b",
            "Collapse runs of two or more blank lines into one; a lone blank line is left alone.",
        ),
        "tool.trim_both.help" => (
            "删除每行开头和结尾的空格与制表符，行内的空白不动。",
            "Remove spaces and tabs from both ends of every line; whitespace inside the line is untouched.",
        ),
        "tool.trim_leading.help" => (
            "只删除每行开头的空格与制表符，也就是取消缩进。",
            "Remove spaces and tabs from the start of every line - i.e. strip indentation.",
        ),
        "tool.trim_trailing.help" => (
            "只删除每行末尾的空格与制表符。这类空白看不见，却常常是两边「看起来一样却报差异」的原因。",
            "Remove spaces and tabs from the end of every line. Invisible on screen, and the usual culprit behind two lines that look identical but still compare as different.",
        ),
        "tool.normalize_ws.help" => (
            "行内连续的空格或制表符压成一个空格，并去掉首尾空白。\n例：「  a   b  」  →  「a b」",
            "Collapse every run of spaces and tabs inside a line to a single space, and trim both ends.\ne.g. \"  a   b  \"  →  \"a b\"",
        ),
        "tool.newlines_to_spaces.help" => (
            "把所有行接成一行，用单个空格连接；空行会被跳过。适合把硬换行的段落还原成整段文字。",
            "Join every line into one, separated by single spaces; blank lines are skipped. Useful for un-wrapping a hard-wrapped paragraph.",
        ),

        // ---- Find bar ---------------------------------------------------
        "find.placeholder" => ("查找…", "Find…"),
        "find.replace_placeholder" => ("替换为…", "Replace with…"),
        "find.case" => ("区分大小写", "Match Case"),
        "find.word" => ("全字匹配", "Whole Word"),
        "find.regex" => ("正则表达式", "Regular Expression"),
        "find.in_left" => ("左侧", "Left"),
        "find.in_right" => ("右侧", "Right"),
        "find.replace_one" => ("替换", "Replace"),
        "find.replace_all" => ("全部替换", "Replace All"),
        "find.no_results" => ("无结果", "No results"),
        "find.bad_regex" => ("正则表达式无效", "Invalid regular expression"),
        "find.results" => ("{n} 个匹配", "{n} matches"),
        "find.result_of" => ("第 {i} / {n} 个", "{i} of {n}"),
        "find.close" => ("关闭", "Close"),

        // ---- Status bar --------------------------------------------------
        "status.added" => ("新增", "Added"),
        "status.removed" => ("删除", "Removed"),
        "status.modified" => ("修改", "Modified"),
        "status.similarity" => ("相似度", "Similarity"),
        "status.identical" => ("两侧内容完全相同", "The two sides are identical"),
        "status.lines" => ("行", "lines"),
        "status.chars" => ("字符", "chars"),
        "status.ln_col" => ("行 {l}，列 {c}", "Ln {l}, Col {c}"),
        "status.selection" => ("已选 {n} 字符", "{n} selected"),
        "status.diff_of" => ("差异 {i}/{n}", "Diff {i}/{n}"),
        "status.diff_list" => (
            "点击列出全部差异并跳转",
            "Click to list every difference and jump to one",
        ),
        "status.diff_list.title" => ("全部差异", "All differences"),
        "status.blank_line" => ("(空行)", "(blank line)"),
        "status.modified_flag" => ("未保存", "Unsaved"),
        "status.comparing" => ("正在对比…", "Comparing…"),
        "status.truncated" => (
            "文本过大，已使用快速对比模式",
            "Text is very large - used fast comparison mode",
        ),
        "status.encoding" => ("编码", "Encoding"),

        // ---- Messages ---------------------------------------------------
        "msg.saved" => ("已保存 {name}", "Saved {name}"),
        "msg.opened" => ("已打开 {name}", "Opened {name}"),
        "msg.copied" => ("已复制到剪贴板", "Copied to clipboard"),
        "msg.open_failed" => ("打开失败：{err}", "Could not open: {err}"),
        "msg.save_failed" => ("保存失败：{err}", "Could not save: {err}"),
        "msg.no_diff_to_export" => ("没有差异可导出", "No differences to export"),
        "msg.replaced" => ("已替换 {n} 处", "Replaced {n} occurrences"),
        "msg.merged" => ("已合并 {n} 处差异", "Merged {n} differences"),
        "msg.lossy_encoding" => (
            "该文件的编码无法完全识别，部分字符可能显示为 �",
            "This file's encoding could not be fully determined; some characters may show as \u{fffd}",
        ),
        "msg.unsaved_changes" => (
            "有未保存的修改，确定要放弃吗？",
            "There are unsaved changes. Discard them?",
        ),
        "msg.confirm" => ("确定", "OK"),
        "msg.cancel" => ("取消", "Cancel"),
        "msg.discard" => ("放弃修改", "Discard"),

        // ---- Onboarding --------------------------------------------------
        "empty.hint" => (
            "在左右两侧粘贴或输入文本，对比会实时进行。\n也可以直接把文件拖进窗口。",
            "Paste or type into either side - the comparison runs as you type.\nYou can also drop files onto the window.",
        ),
        "shortcuts.title" => ("快捷键", "Keyboard Shortcuts"),

        _ => ("", ""),
    }
}

/// Substitute `{name}` placeholders in a translated string.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Translate and substitute in one step.
pub fn tf(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    fill(t(lang, key), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_resolve() {
        assert_eq!(t(Lang::Chinese, "app.left"), "左侧");
        assert_eq!(t(Lang::English, "app.left"), "Left");
    }

    #[test]
    fn system_resolves_to_a_concrete_language() {
        assert_ne!(Lang::System.resolve(), Lang::System);
        // Resolving is idempotent: an already-resolved language stays put.
        let once = Lang::System.resolve();
        assert_eq!(once.resolve(), once);
        assert_eq!(Lang::Chinese.resolve(), Lang::Chinese);
        assert_eq!(Lang::English.resolve(), Lang::English);
    }

    /// The Windows detection path calls into kernel32; make sure it is sound
    /// and answers consistently rather than, say, tripping over a null pointer.
    #[test]
    fn system_detection_is_stable() {
        let first = system_prefers_chinese();
        for _ in 0..8 {
            assert_eq!(system_prefers_chinese(), first);
        }
    }

    #[test]
    fn an_unknown_key_is_empty_not_a_panic() {
        assert_eq!(t(Lang::English, "does.not.exist"), "");
    }

    #[test]
    fn placeholders_are_substituted() {
        assert_eq!(
            tf(Lang::English, "status.ln_col", &[("l", "12"), ("c", "5")]),
            "Ln 12, Col 5"
        );
        assert_eq!(
            tf(Lang::Chinese, "msg.replaced", &[("n", "3")]),
            "已替换 3 处"
        );
    }

    /// Every cleanup tool must have a label in both languages, or the Tools
    /// menu shows blanks.
    #[test]
    fn every_cleanup_tool_is_translated() {
        for op in crate::core::text::CleanupOp::ALL {
            let key = op.i18n_key();
            let (zh, en) = lookup(key);
            assert!(!zh.is_empty(), "missing Chinese for {key}");
            assert!(!en.is_empty(), "missing English for {key}");
        }
    }

    /// A key that has one language but not the other would silently fall back
    /// to an empty label, so check the whole table.
    #[test]
    fn no_key_is_half_translated() {
        for key in ALL_KEYS {
            let (zh, en) = lookup(key);
            assert!(!zh.is_empty(), "missing Chinese for {key}");
            assert!(!en.is_empty(), "missing English for {key}");
        }
    }

    fn has_cjk(s: &str) -> bool {
        s.chars().any(|c| matches!(c as u32,
            0x4E00..=0x9FFF     // CJK unified ideographs
            | 0x3400..=0x4DBF   // extension A
            | 0x3000..=0x303F   // CJK punctuation
            | 0xFF00..=0xFFEF)) // fullwidth forms
    }

    /// The English column must not contain Chinese, and vice versa.
    ///
    /// These tables are two wide columns of quoted strings; it is genuinely
    /// easy to leave a stray character behind while editing one of them, and
    /// the result is a sentence that reads as a typo to whichever half of the
    /// audience hits it.
    #[test]
    fn the_two_columns_do_not_bleed_into_each_other() {
        let keys: Vec<&str> = ALL_KEYS
            .iter()
            .copied()
            .chain(crate::core::text::CleanupOp::ALL.iter().map(|o| o.help_key()))
            .chain(crate::core::text::CleanupOp::ALL.iter().map(|o| o.i18n_key()))
            .collect();

        for key in keys {
            let (zh, en) = lookup(key);
            assert!(!has_cjk(en), "English text for {key} contains CJK: {en:?}");
            assert!(
                has_cjk(zh) || zh == en,
                "Chinese text for {key} looks untranslated: {zh:?}"
            );
        }
    }

    /// Keys used by the UI. Kept here so the test above can sweep the table;
    /// adding a key without listing it simply means it is not swept.
    const ALL_KEYS: &[&str] = &[
        "app.title",
        "app.left",
        "app.right",
        "app.untitled",
        "file.menu",
        "file.open_left",
        "file.open_right",
        "file.save_left",
        "file.save_right",
        "file.recent",
        "file.export_unified",
        "file.copy_unified",
        "file.exit",
        "file.open",
        "file.save",
        "file.save_as",
        "edit.menu",
        "edit.undo",
        "edit.redo",
        "edit.copy",
        "edit.paste",
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
        "view.font_size",
        "view.reset_layout",
        "view.single_pane",
        "view.single_pane.hint",
        "cmp.menu",
        "cmp.algorithm",
        "cmp.granularity",
        "cmp.granularity.short",
        "cmp.granularity.hint",
        "view.caret_line_end",
        "view.caret_line_end.hint",
        "cmp.whitespace",
        "cmp.ignore_case",
        "cmp.ignore_blank",
        "cmp.smart_align",
        "merge.to_right",
        "merge.to_left",
        "merge.all_to_right",
        "merge.all_to_left",
        "nav.prev_diff",
        "nav.next_diff",
        "tools.menu",
        "tools.apply_to",
        "find.placeholder",
        "find.regex",
        "find.replace_all",
        "status.added",
        "status.removed",
        "status.modified",
        "status.similarity",
        "status.identical",
        "status.diff_list",
        "status.diff_list.title",
        "status.blank_line",
        "msg.copied",
        "msg.saved",
        "empty.hint",
    ];
}
