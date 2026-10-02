//! 设置页界面文案：全部走 snow-i18n 的 Fluent 语料（`settings_ui.ftl`、`settings_items.ftl`、
//! `settings_options.ftl`），这里只保留 message id 的规则与语言选择，没有按语言写死的文案表。

use crate::ocr_backend::i18n_for;
use snow_i18n::Args;

/// 界面语言：内置语言之一，持有语言代码（如 `zh-CN`），按代码到语料取文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang {
    /// 内置语言代码，必为 `snow_i18n::locales()` 中的一项。
    locale: &'static str,
}

impl Lang {
    /// 由语言标记得到界面语言；标记不支持时回退 en-US。
    ///
    /// # 参数
    /// - `tag`：语言代码或别名，如 `zh-CN`、`zh_CN`、`en`。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(Lang::new("zh_CN").locale(), "zh-CN");
    /// assert_eq!(Lang::new("fr").locale(), "en-US");
    /// ```
    pub fn new(tag: &str) -> Lang {
        let locale = snow_i18n::match_locale(tag).map_or(snow_i18n::FALLBACK_LOCALE, |l| l.code);
        Lang { locale }
    }

    /// 由配置值与系统语言得到界面语言：已保存值优先，没有则取系统语言，都不支持回退英文。
    ///
    /// # 参数
    /// - `value`：`interface/language` 已保存的值，空串表示没有
    /// - `system_language`：系统界面语言标记
    pub fn from_config(value: &str, system_language: &str) -> Lang {
        Lang::new(crate::translate_service::effective_interface_language(
            Some(value),
            system_language,
        ))
    }

    /// snow-i18n 语料使用的语言代码。
    pub fn locale(self) -> &'static str {
        self.locale
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
    /// 快捷键数量已达上限。
    ListFull,
    /// 不支持该按键。
    UnsupportedKey,
    /// 只读原因：内部固定值。
    ReadOnlyInternal,
    /// 只读原因：含密钥。
    ReadOnlySecret,
    /// 只读原因：结构较大。
    ReadOnlyTooLarge,
    /// 输入错误：不是整数。
    ErrNotInteger,
    /// 输入错误：不是合法 JSON。
    ErrBadJson,
    /// 输入错误：该项不可编辑。
    ErrNotEditable,
    /// 语言选项“自动识别”。
    LanguageAuto,
}

impl Text {
    /// 全部文案变体（供完整性测试遍历）。
    pub const ALL: [Text; 27] = [
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
        Text::ListFull,
        Text::UnsupportedKey,
        Text::ReadOnlyInternal,
        Text::ReadOnlySecret,
        Text::ReadOnlyTooLarge,
        Text::ErrNotInteger,
        Text::ErrBadJson,
        Text::ErrNotEditable,
        Text::LanguageAuto,
    ];

    /// 对应的 message id（`settings_ui.ftl`）。
    pub fn id(self) -> &'static str {
        match self {
            Text::Title => "settings-ui-title",
            Text::SearchPlaceholder => "settings-ui-search-placeholder",
            Text::ResetItem => "settings-ui-reset-item",
            Text::ResetGroup => "settings-ui-reset-group",
            Text::ItemsCount => "settings-ui-items-count",
            Text::SearchResults => "settings-ui-search-results",
            Text::Saved => "settings-ui-saved",
            Text::InvalidValue => "settings-ui-invalid-value",
            Text::SaveFailed => "settings-ui-save-failed",
            Text::AddShortcut => "settings-ui-add-shortcut",
            Text::PressShortcut => "settings-ui-press-shortcut",
            Text::ShortcutConflict => "settings-ui-shortcut-conflict",
            Text::HotkeyRegisterFailed => "settings-ui-hotkey-register-failed",
            Text::ReadOnly => "settings-ui-read-only",
            Text::ThemeLiveNote => "settings-ui-theme-live-note",
            Text::LanguageLiveNote => "settings-ui-language-live-note",
            Text::NoResults => "settings-ui-no-results",
            Text::Restored => "settings-ui-restored",
            Text::ListFull => "settings-ui-list-full",
            Text::UnsupportedKey => "settings-ui-unsupported-key",
            Text::ReadOnlyInternal => "settings-ui-read-only-internal",
            Text::ReadOnlySecret => "settings-ui-read-only-secret",
            Text::ReadOnlyTooLarge => "settings-ui-read-only-too-large",
            Text::ErrNotInteger => "settings-ui-err-not-integer",
            Text::ErrBadJson => "settings-ui-err-bad-json",
            Text::ErrNotEditable => "settings-ui-err-not-editable",
            Text::LanguageAuto => "settings-ui-language-auto",
        }
    }
}

/// 获取指定语言的界面文案；缺消息时按回退链落到 en-US。
///
/// # 参数
/// - `lang`：界面语言
/// - `text`：文案变体
///
/// # 返回
/// 对应语言的文案。
pub fn t(lang: Lang, text: Text) -> String {
    i18n_for(lang.locale()).tr_with(text.id(), &Args::new())
}

/// 分组 id 列表，顺序即侧栏顺序；标题在 `settings_ui.ftl` 的 `settings-group-<id>`。
pub const GROUP_IDS: [&str; 27] = [
    "interface",
    "system",
    "tray",
    "updates",
    "network",
    "mcp",
    "global_shortcuts",
    "global_mouse",
    "screenshot",
    "screenshot_ui",
    "screenshot_selection",
    "screenshot_toolbar",
    "screenshot_translation",
    "screenshot_conversion",
    "screenshot_shortcuts",
    "drawing",
    "drawing_shortcuts",
    "pin_to_screen",
    "pin_to_screen_shortcuts",
    "pinned_history",
    "capture_history",
    "text_recognition",
    "screen_recording",
    "screen_recording_shortcuts",
    "api_configuration",
    "extended_features",
    "storage",
];

/// 分组标题的 message id：`pin_to_screen` 变为 `settings-group-pin-to-screen`。
///
/// # 参数
/// - `group_id`：分组标识符
pub fn group_message_id(group_id: &str) -> String {
    format!("settings-group-{}", group_id.replace('_', "-"))
}

/// 获取分组标题。
///
/// # 参数
/// - `lang`：界面语言
/// - `group_id`：分组标识符
///
/// # 返回
/// 对应语言的分组标题，找不到时返回空字符串。
pub fn group_title(lang: Lang, group_id: &str) -> String {
    let id = group_message_id(group_id);
    let i18n = i18n_for(lang.locale());
    if i18n.has(&id) {
        i18n.tr(&id)
    } else {
        String::new()
    }
}

/// 配置项名称/说明的 message id 前缀。
const ITEM_ID_PREFIX: &str = "setting-";
/// 说明 message id 后缀。
const ITEM_DESC_SUFFIX: &str = "-desc";

/// 配置键对应的名称 message id：`screenshot/image_quality` 变为 `setting-screenshot-image-quality`。
///
/// # 参数
/// - `key`：`"组/名"` 配置键
pub fn item_message_id(key: &str) -> String {
    format!("{ITEM_ID_PREFIX}{}", key.replace(['/', '_'], "-"))
}

/// 取配置项的本地化名称；语料缺失时退回英文化的键名。
///
/// # 参数
/// - `lang`：界面语言
/// - `key`：配置键
///
/// # 返回
/// 该语言下行标题文案。
pub fn item_label(lang: Lang, key: &str) -> String {
    let id = item_message_id(key);
    let i18n = i18n_for(lang.locale());
    if i18n.has(&id) {
        i18n.tr(&id)
    } else {
        crate::settings_model::humanize(key)
    }
}

/// 取配置项的本地化说明；没有说明时返回 `None`。
///
/// # 参数
/// - `lang`：界面语言
/// - `key`：配置键
pub fn item_desc(lang: Lang, key: &str) -> Option<String> {
    let id = format!("{}{ITEM_DESC_SUFFIX}", item_message_id(key));
    let i18n = i18n_for(lang.locale());
    i18n.has(&id).then(|| i18n.tr(&id))
}

/// 原样显示的选项值白名单：文件/编码格式名，各语言写法一致，不需要翻译。
/// 其余选项值（含 PNG / JPEG / WebP 等带固定写法的标签）在 `settings_options.ftl` 里逐项登记。
pub const RAW_OPTION_VALUES: &[&str] = &["mp4", "gif", "apng", "jxl"];

/// 下拉选项显示文字的 message id 前缀。
const OPTION_ID_PREFIX: &str = "setting-option-";

/// 把配置键或选项值规范成 message id 片段：非字母数字一律换成 `-`。
fn id_part(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        out.push(if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        });
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "empty".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 下拉选项的 message id：`tray/icon` + `snow-dark` 变为 `setting-option-tray-icon-snow-dark`。
///
/// # 参数
/// - `key`：配置键
/// - `value`：选项配置值
pub fn option_message_id(key: &str, value: &str) -> String {
    format!("{OPTION_ID_PREFIX}{}-{}", id_part(key), id_part(value))
}

/// 取下拉选项的本地化文字；语料里没有时返回 `None`（调用方回退原值）。
///
/// # 参数
/// - `locale`：界面语言代码（不支持的值回退 en-US）
/// - `key`：配置键
/// - `value`：选项配置值
///
/// # 示例
/// ```ignore
/// assert_eq!(option_text("zh-CN", "tray/icon", "dark").as_deref(), Some("深色"));
/// ```
pub fn option_text(locale: &str, key: &str, value: &str) -> Option<String> {
    let id = option_message_id(key, value);
    let i18n = i18n_for(Lang::new(locale).locale());
    i18n.has(&id).then(|| i18n.tr(&id))
}

/// 设置窗口标题（随界面语言）。
///
/// # 参数
/// - `lang`：界面语言
pub fn window_title(lang: Lang) -> String {
    t(lang, Text::Title)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部内置语言对应的界面语言。
    fn all_langs() -> Vec<Lang> {
        snow_i18n::locales()
            .iter()
            .map(|l| Lang::new(l.code))
            .collect()
    }

    /// 已保存值优先，没有时取系统语言，不支持回退英文；旧 system / 繁体按没有保存处理。
    #[test]
    fn lang_from_config() {
        assert_eq!(Lang::from_config("", "zh-CN").locale(), "zh-CN");
        assert_eq!(Lang::from_config("", "en-US").locale(), "en-US");
        assert_eq!(Lang::from_config("", "ja-JP").locale(), "en-US");
        assert_eq!(Lang::from_config("", "zh-Hant-TW").locale(), "en-US");
        assert_eq!(Lang::from_config("zh_CN", "en-US").locale(), "zh-CN");
        assert_eq!(Lang::from_config("en_US", "zh-CN").locale(), "en-US");
        for old in ["system", "zh_TW", "zh-Hant"] {
            assert_eq!(Lang::from_config(old, "zh-CN").locale(), "zh-CN", "{old}");
            assert_eq!(Lang::from_config(old, "en-US").locale(), "en-US", "{old}");
        }
        assert_eq!(Lang::new("fr").locale(), "en-US");
        assert_eq!(Lang::new("zh_CN").locale(), "zh-CN");
    }

    /// 所有 Text 变体在每种内置语言下都能解析，且不是缺失标记。
    #[test]
    fn every_text_resolves_in_all_locales() {
        for lang in all_langs() {
            let i18n = i18n_for(lang.locale());
            for text in Text::ALL.iter() {
                assert!(i18n.has(text.id()), "{} 缺少 {}", lang.locale(), text.id());
                assert!(
                    !t(lang, *text).is_empty(),
                    "{} {:?} 为空",
                    lang.locale(),
                    text
                );
            }
        }
        assert_eq!(t(Lang::new("zh-CN"), Text::Title), "设置");
        assert_eq!(t(Lang::new("en-US"), Text::Title), "Settings");
    }

    /// 分组表 27 项、id 唯一，每组在每种语言下都有标题。
    #[test]
    fn group_titles_complete() {
        let mut seen = std::collections::HashSet::new();
        for id in GROUP_IDS {
            assert!(seen.insert(id), "重复分组 id：{id}");
            for lang in all_langs() {
                assert!(
                    !group_title(lang, id).is_empty(),
                    "{} 缺少分组标题 {id}",
                    lang.locale()
                );
            }
        }
        assert_eq!(group_title(Lang::new("zh-CN"), "screenshot"), "截图");
        assert_eq!(group_title(Lang::new("en-US"), "nope"), "");
        assert_eq!(
            group_message_id("pin_to_screen"),
            "settings-group-pin-to-screen"
        );
    }

    /// schema 中每个键在每种内置语言下都必须有名称，缺一条就失败。
    #[test]
    fn every_schema_key_has_label_in_all_locales() {
        for lang in all_langs() {
            let i18n = i18n_for(lang.locale());
            let missing: Vec<&str> = snow_config::schema::entries()
                .iter()
                .map(|e| e.key)
                .filter(|key| !i18n.has(&item_message_id(key)))
                .collect();
            assert!(
                missing.is_empty(),
                "{} 缺少名称：{missing:?}",
                lang.locale()
            );
        }
    }

    /// 名称与说明按语言取值；无说明的键返回 None。
    #[test]
    fn item_labels_are_localized() {
        let (zh, en) = (Lang::new("zh-CN"), Lang::new("en-US"));
        assert_eq!(
            item_label(zh, "screenshot_translation/source_language"),
            "源语言"
        );
        assert_eq!(
            item_label(en, "screenshot_translation/source_language"),
            "Source language"
        );
        assert!(item_desc(zh, "screenshot/image_quality").is_none());
        for lang in [en, zh] {
            assert!(item_desc(lang, "screenshot/delay_seconds").is_some());
        }
        assert_eq!(window_title(zh), "设置");
        assert_eq!(window_title(en), "Settings");
    }

    /// 每个下拉枚举键的每个选项值，每种语言下都要有文字，或在白名单内；语言类选项另有自称表。
    #[test]
    fn every_enum_option_is_localized_or_whitelisted() {
        let mut missing: Vec<String> = Vec::new();
        for entry in snow_config::schema::entries() {
            // 语言类选项走各语言自称，OCR 后端与路由模式有专用标签，这些不查通用选项文案。
            let dedicated = [
                snow_config::extensions::KEY_OCR_BACKEND,
                snow_config::extensions::KEY_LOCAL_ROUTE_MODE,
            ];
            if crate::language_names::is_language_key(entry.key) || dedicated.contains(&entry.key) {
                continue;
            }
            let options = match crate::settings_model::control_for(entry) {
                crate::settings_model::Control::Choice(o) => o,
                _ => continue,
            };
            for value in options {
                if RAW_OPTION_VALUES.contains(value) {
                    continue;
                }
                for info in snow_i18n::locales() {
                    if option_text(info.code, entry.key, value).is_none() {
                        missing.push(format!("{} {} = {value:?}", info.code, entry.key));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "缺少选项文字：{missing:#?}");
        assert_eq!(
            option_text("zh-CN", "tray/icon", "dark").as_deref(),
            Some("深色")
        );
        assert_eq!(
            option_text("en-US", "tray/icon", "dark").as_deref(),
            Some("Dark")
        );
        assert_eq!(
            option_message_id("tray/icon", "snow-dark"),
            "setting-option-tray-icon-snow-dark"
        );
    }
}
