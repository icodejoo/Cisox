//! 语言选项的显示名：固定为各语言自称，不随界面语言变化；纯数据，可离屏单测。
//!
//! 内置界面语言的自称来自各自的 `locale.toml`；只作为翻译目标/源出现的语言（日语、法语等）
//! 没有界面语料，自称放在下面的 [`TRANSLATION_ONLY_ENDONYMS`]。

use crate::settings_model::LANGUAGE_KEY;
use crate::settings_text::{Lang, Text, t};

/// 选项取值是语言代码的配置键。
const LANGUAGE_VALUED_KEYS: &[&str] = &[
    LANGUAGE_KEY,
    "screenshot_translation/source_language",
    "screenshot_translation/target_language",
];

/// “自动识别”选项的取值（源语言专用，不是“跟随系统”）。
const AUTO_CODE: &str = "auto";

/// 只用于翻译的语言：键为小写、`_` 换成 `-` 的代码，值为自称。
const TRANSLATION_ONLY_ENDONYMS: &[(&str, &str)] = &[
    ("ja", "日本語"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("ru", "Русский"),
    ("pt", "Português"),
    ("it", "Italiano"),
    ("ar", "العربية"),
    ("ko", "한국어"),
    ("tr", "Türkçe"),
];

/// 该配置键的选项是否为语言代码。
///
/// # 参数
/// - `key`：配置键
pub fn is_language_key(key: &str) -> bool {
    LANGUAGE_VALUED_KEYS.contains(&key)
}

/// 语言代码对应的自称；未知代码返回 `None`。
///
/// # 参数
/// - `code`：语言代码，兼容 `zh-Hans` / `zh_CN` / `en_US` 等写法
///
/// ```ignore
/// assert_eq!(endonym("zh_CN"), Some("简体中文"));
/// assert_eq!(endonym("ja"), Some("日本語"));
/// ```
pub fn endonym(code: &str) -> Option<&'static str> {
    if let Some(info) = snow_i18n::match_locale(code) {
        return Some(info.native_name);
    }
    let norm = code.trim().replace('_', "-").to_lowercase();
    TRANSLATION_ONLY_ENDONYMS
        .iter()
        .find(|(c, _)| *c == norm)
        .map(|(_, name)| *name)
}

/// 语言选项的显示标签：`auto` 不是语言，按界面语言本地化；其余为自称，未知代码原样返回。
///
/// # 参数
/// - `code`：选项取值
/// - `locale`：界面语言代码
pub fn language_option_label(code: &str, locale: &str) -> String {
    if code == AUTO_CODE {
        return t(Lang::new(locale), Text::LanguageAuto);
    }
    endonym(code).map_or_else(|| code.to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自称不随界面语言变化，别名等价，未知代码回退原串。
    #[test]
    fn endonyms_are_locale_independent() {
        for info in snow_i18n::locales() {
            let locale = info.code;
            assert_eq!(language_option_label("ja", locale), "日本語");
            assert_eq!(language_option_label("zh-Hans", locale), "简体中文");
            assert_eq!(language_option_label("zh_CN", locale), "简体中文");
            assert_eq!(language_option_label("en_US", locale), "English");
            assert_eq!(language_option_label("xx-unknown", locale), "xx-unknown");
            assert_eq!(language_option_label("zh-Hant", locale), "zh-Hant");
        }
        assert_eq!(endonym("zh-CN"), endonym("zh-Hans"));
    }

    /// auto 随界面语言本地化；不再有“跟随系统”。
    #[test]
    fn auto_follows_locale() {
        assert_eq!(language_option_label("auto", "zh-CN"), "自动识别");
        assert_eq!(language_option_label("auto", "en-US"), "Auto detect");
        assert_eq!(language_option_label("system", "en-US"), "system");
    }

    /// schema 中语言键的全部候选都有自称；界面语言候选来自已发现的语言。
    #[test]
    fn schema_language_options_covered() {
        for key in LANGUAGE_VALUED_KEYS {
            let entry = snow_config::schema::entry_for(key).unwrap();
            let options: &[&str] = if *key == LANGUAGE_KEY {
                crate::settings_model::language_options()
            } else {
                entry.allowed
            };
            assert!(!options.is_empty(), "{key}");
            for option in options {
                assert_ne!(*option, "system", "{key} 不应再有跟随系统");
                if *option != AUTO_CODE {
                    assert!(endonym(option).is_some(), "{key}: {option}");
                }
            }
        }
    }
}
