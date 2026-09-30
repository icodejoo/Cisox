//! 设置页界面文案表：中英双语，按当前语言取文案。

/// 语言选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 简体中文。
    ZhCn,
    /// 美式英文。
    EnUs,
}

impl Lang {
    /// 从配置值和系统语言生成语言选项。
    ///
    /// 参数：
    /// - value: 配置值，"system" 表示跟随系统
    /// - system_language: 系统语言标识
    ///
    /// 返回：
    /// 选定的语言枚举。
    pub fn from_config(value: &str, system_language: &str) -> Lang {
        let normalized = value.trim().to_lowercase();
        let check_val = if normalized == "system" {
            system_language
        } else {
            value
        };

        let check_normalized = check_val.trim().to_lowercase();
        if check_normalized.starts_with("zh") {
            Lang::ZhCn
        } else {
            Lang::EnUs
        }
    }
}

/// 界面文案文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    /// 标题。
    Title,
    /// 搜索框占位符。
    SearchPlaceholder,
    /// 重置项。
    ResetItem,
    /// 重置分组。
    ResetGroup,
    /// 项数。
    ItemsCount,
    /// 搜索结果。
    SearchResults,
    /// 已保存。
    Saved,
    /// 值无效。
    InvalidValue,
    /// 保存失败。
    SaveFailed,
    /// 添加快捷键。
    AddShortcut,
    /// 按快捷键提示。
    PressShortcut,
    /// 快捷键冲突。
    ShortcutConflict,
    /// 全局热键注册失败。
    HotkeyRegisterFailed,
    /// 只读。
    ReadOnly,
    /// 主题实时生效提示。
    ThemeLiveNote,
    /// 语言实时生效提示。
    LanguageLiveNote,
    /// 无搜索结果。
    NoResults,
    /// 已恢复默认。
    Restored,
}

/// 获取指定语言的文案。
///
/// 参数：
/// - lang: 语言选择
/// - text: 文本变体
///
/// 返回：
/// 对应语言的文案字符串。
pub fn t(lang: Lang, text: Text) -> &'static str {
    match (lang, text) {
        // Title
        (Lang::ZhCn, Text::Title) => "设置",
        (Lang::EnUs, Text::Title) => "Settings",
        // SearchPlaceholder
        (Lang::ZhCn, Text::SearchPlaceholder) => "搜索配置项…",
        (Lang::EnUs, Text::SearchPlaceholder) => "Search settings...",
        // ResetItem
        (Lang::ZhCn, Text::ResetItem) => "重置",
        (Lang::EnUs, Text::ResetItem) => "Reset",
        // ResetGroup
        (Lang::ZhCn, Text::ResetGroup) => "重置本组",
        (Lang::EnUs, Text::ResetGroup) => "Reset group",
        // ItemsCount
        (Lang::ZhCn, Text::ItemsCount) => "项",
        (Lang::EnUs, Text::ItemsCount) => "items",
        // SearchResults
        (Lang::ZhCn, Text::SearchResults) => "搜索结果",
        (Lang::EnUs, Text::SearchResults) => "Search results",
        // Saved
        (Lang::ZhCn, Text::Saved) => "已保存",
        (Lang::EnUs, Text::Saved) => "Saved",
        // InvalidValue
        (Lang::ZhCn, Text::InvalidValue) => "值无效，未保存",
        (Lang::EnUs, Text::InvalidValue) => "Invalid value, not saved",
        // SaveFailed
        (Lang::ZhCn, Text::SaveFailed) => "写入磁盘失败，已还原",
        (Lang::EnUs, Text::SaveFailed) => "Failed to write to disk, reverted",
        // AddShortcut
        (Lang::ZhCn, Text::AddShortcut) => "+ 添加",
        (Lang::EnUs, Text::AddShortcut) => "+ Add",
        // PressShortcut
        (Lang::ZhCn, Text::PressShortcut) => "请按下快捷键（Esc 取消）",
        (Lang::EnUs, Text::PressShortcut) => "Press a shortcut (Esc to cancel)",
        // ShortcutConflict
        (Lang::ZhCn, Text::ShortcutConflict) => "快捷键冲突，未保存",
        (Lang::EnUs, Text::ShortcutConflict) => "Shortcut conflict, not saved",
        // HotkeyRegisterFailed
        (Lang::ZhCn, Text::HotkeyRegisterFailed) => "全局热键注册失败（可能被占用），已还原",
        (Lang::EnUs, Text::HotkeyRegisterFailed) => "Global hotkey registration failed (may be in use), reverted",
        // ReadOnly
        (Lang::ZhCn, Text::ReadOnly) => "只读",
        (Lang::EnUs, Text::ReadOnly) => "Read-only",
        // ThemeLiveNote
        (Lang::ZhCn, Text::ThemeLiveNote) => "主题已在本窗口即时生效；其它窗口尚未接入主题",
        (Lang::EnUs, Text::ThemeLiveNote) => "Theme applied to this window instantly; other windows do not use themes yet",
        // LanguageLiveNote
        (Lang::ZhCn, Text::LanguageLiveNote) => "语言已在本窗口界面即时生效；配置项名称暂为英文",
        (Lang::EnUs, Text::LanguageLiveNote) => "Language applied to this window's UI instantly; setting names are English for now",
        // NoResults
        (Lang::ZhCn, Text::NoResults) => "没有匹配的配置项",
        (Lang::EnUs, Text::NoResults) => "No matching settings",
        // Restored
        (Lang::ZhCn, Text::Restored) => "已恢复默认",
        (Lang::EnUs, Text::Restored) => "Restored default",
    }
}

/// 分组标题表：(分组 id, 中文标题, 英文标题)。
pub const GROUP_TITLES: [(&str, &str, &str); 27] = [
    ("interface", "界面", "Interface"),
    ("system", "系统", "System"),
    ("tray", "托盘", "Tray"),
    ("updates", "更新", "Updates"),
    ("network", "网络", "Network"),
    ("mcp", "MCP", "MCP"),
    ("global_shortcuts", "全局快捷键", "Global shortcuts"),
    ("global_mouse", "全局鼠标", "Global mouse"),
    ("screenshot", "截图", "Screenshot"),
    ("screenshot_ui", "截图界面", "Screenshot UI"),
    ("screenshot_selection", "截图选区", "Selection"),
    ("screenshot_toolbar", "截图工具栏", "Screenshot toolbar"),
    ("screenshot_translation", "截图翻译", "Screenshot translation"),
    ("screenshot_conversion", "截图转换", "Screenshot conversion"),
    ("screenshot_shortcuts", "截图快捷键", "Screenshot shortcuts"),
    ("drawing", "绘图", "Drawing"),
    ("drawing_shortcuts", "绘图快捷键", "Drawing shortcuts"),
    ("pin_to_screen", "贴图", "Pin to screen"),
    ("pin_to_screen_shortcuts", "贴图快捷键", "Pin shortcuts"),
    ("pinned_history", "贴图历史", "Pinned history"),
    ("capture_history", "截图历史", "Capture history"),
    ("text_recognition", "文字识别", "Text recognition"),
    ("screen_recording", "屏幕录制", "Screen recording"),
    ("screen_recording_shortcuts", "录制快捷键", "Recording shortcuts"),
    ("api_configuration", "模型接口", "AI models"),
    ("extended_features", "扩展功能", "Extended features"),
    ("storage", "存储", "Storage"),
];

/// 获取分组标题。
///
/// 参数：
/// - lang: 语言选择
/// - group_id: 分组标识符
///
/// 返回：
/// 对应语言的分组标题，若找不到返回空字符串。
pub fn group_title(lang: Lang, group_id: &str) -> &'static str {
    for (id, zh, en) in &GROUP_TITLES {
        if *id == group_id {
            return match lang {
                Lang::ZhCn => zh,
                Lang::EnUs => en,
            };
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_lang_from_config() {
        // 测试 system 配置跟随系统语言。
        assert_eq!(Lang::from_config("system", "zh-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_config("system", "en-US"), Lang::EnUs);

        // 测试非 system 配置使用自身值。
        assert_eq!(Lang::from_config("zh_TW", "en-US"), Lang::ZhCn);
        assert_eq!(Lang::from_config("en_US", "zh-CN"), Lang::EnUs);
    }

    #[test]
    fn test_all_text_variants() {
        // 测试所有 Text 变体在两种语言下非空。
        let variants = [
            Text::Title,
            Text::SearchPlaceholder,
            Text::ResetItem,
            Text::ResetGroup,
            Text::ItemsCount,
            Text::SearchResults,
            Text::Saved,
            Text::InvalidValue,
            Text::SaveFailed,
            Text::AddShortcut,
            Text::PressShortcut,
            Text::ShortcutConflict,
            Text::HotkeyRegisterFailed,
            Text::ReadOnly,
            Text::ThemeLiveNote,
            Text::LanguageLiveNote,
            Text::NoResults,
            Text::Restored,
        ];

        for variant in &variants {
            assert!(
                !t(Lang::ZhCn, *variant).is_empty(),
                "ZhCn text for {:?} is empty",
                variant
            );
            assert!(
                !t(Lang::EnUs, *variant).is_empty(),
                "EnUs text for {:?} is empty",
                variant
            );
        }
    }

    #[test]
    fn test_group_titles() {
        // 测试 GROUP_TITLES 有 27 项且 id 唯一。
        assert_eq!(GROUP_TITLES.len(), 27, "GROUP_TITLES should have 27 items");

        let mut ids = HashSet::new();
        for (id, zh, en) in &GROUP_TITLES {
            assert!(ids.insert(*id), "Duplicate group id: {}", id);
            assert!(!zh.is_empty(), "ZhCn title for group {} is empty", id);
            assert!(!en.is_empty(), "EnUs title for group {} is empty", id);
        }

        // 测试 group_title 函数。
        assert_eq!(group_title(Lang::ZhCn, "screenshot"), "截图");
        assert_eq!(group_title(Lang::EnUs, "nope"), "");
    }
}
