//! 语言发现：由 `locales/<代码>/locale.toml` 在构建期生成的语言元数据与匹配。

use crate::embedded::LOCALES;

/// 回退语言：任何语言缺消息时回退到它。
pub const FALLBACK_LOCALE: &str = "en-US";

/// 一种内置界面语言的元数据（来自 `locale.toml`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleInfo {
    /// 语言代码，等于目录名，如 `zh-CN`。
    pub code: &'static str,
    /// 写入配置的取值，如 `zh_CN`（代码里的 `-` 换成 `_`）。
    pub config_value: &'static str,
    /// 该语言的自称，如 `简体中文`。
    pub native_name: &'static str,
    /// 别名（精确匹配，忽略大小写与 `-`/`_` 差异），如 `zh-Hans`。
    pub aliases: &'static [&'static str],
    /// 系统语言前缀：标记等于它或以它加 `-` 开头即命中，如 `zh-cn`。
    pub system_prefixes: &'static [&'static str],
}

/// 规范化语言标记：去空白、小写、`_` 换成 `-`。
fn normalize(tag: &str) -> String {
    tag.trim().to_lowercase().replace('_', "-")
}

/// 全部内置语言，按代码排序。
///
/// # 示例
/// ```
/// assert!(snow_i18n::locales().iter().any(|l| l.code == "en-US"));
/// ```
pub fn locales() -> &'static [LocaleInfo] {
    LOCALES
}

impl LocaleInfo {
    /// 规范化后的标记是否命中本语言：代码或别名精确相等，或命中某个系统语言前缀。
    fn matches(&self, norm: &str) -> bool {
        let same = |s: &str| normalize(s) == norm;
        same(self.code)
            || self.aliases.iter().any(|a| same(a))
            || self.system_prefixes.iter().any(|p| {
                let p = normalize(p);
                norm == p
                    || norm
                        .strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('-'))
            })
    }
}

/// 把语言标记（配置值或系统语言）匹配到内置语言；空串或不支持返回 `None`。
///
/// # 参数
/// - `tag`：如 `zh_CN`、`zh-Hans-CN`、`en-GB`。
///
/// # 示例
/// ```
/// assert_eq!(snow_i18n::match_locale("zh-Hans-CN").map(|l| l.code), Some("zh-CN"));
/// assert!(snow_i18n::match_locale("ja-JP").is_none());
/// ```
pub fn match_locale(tag: &str) -> Option<&'static LocaleInfo> {
    let norm = normalize(tag);
    if norm.is_empty() {
        return None;
    }
    LOCALES.iter().find(|l| l.matches(&norm))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回退语言必须存在，配置值用下划线，自称非空。
    #[test]
    fn discovered_locales_are_sane() {
        assert!(locales().iter().any(|l| l.code == FALLBACK_LOCALE));
        for l in locales() {
            assert_eq!(l.config_value, l.code.replace('-', "_"));
            assert!(!l.native_name.is_empty());
        }
    }

    /// 别名、系统前缀与不支持语言的匹配。
    #[test]
    fn matching_rules() {
        let code = |t: &str| match_locale(t).map(|l| l.code);
        for t in ["zh_CN", "zh-Hans", "zh-Hans-CN", "zh", "zh-SG", "ZH-cn"] {
            assert_eq!(code(t), Some("zh-CN"), "{t}");
        }
        for t in ["en", "en_US", "en-GB"] {
            assert_eq!(code(t), Some("en-US"), "{t}");
        }
        for t in [
            "zh-TW",
            "zh_Hant",
            "zh-Hant-TW",
            "zh-HK",
            "ja-JP",
            "",
            "system",
        ] {
            assert_eq!(code(t), None, "{t:?}");
        }
    }

    /// 缺消息时回退到 en-US，不报错。
    #[test]
    fn missing_message_falls_back_to_english() {
        let zh = crate::I18n::from_resources(
            "zh-CN",
            FALLBACK_LOCALE,
            "Cisox",
            &[("zh-CN", "a = 甲\n"), ("en-US", "a = A\nb = B\n")],
        )
        .unwrap();
        assert_eq!(zh.tr("a"), "甲");
        assert_eq!(zh.tr("b"), "B");
    }
}
