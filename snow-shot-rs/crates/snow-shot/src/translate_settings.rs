//! 翻译设置页的路由文案与说明区行拆分：纯数据与文案，不依赖 GPUI，可离屏单测。

use crate::ocr_backend::i18n_for;
use snow_config::extensions::{
    KEY_LOCAL_MAX_RESIDENT, KEY_LOCAL_ROUTE_MODE, ROUTE_MIXED_SPLIT, ROUTE_SINGLE,
    ROUTE_SPECIALIZED_FIRST,
};

/// 路由模式取值对应的标签消息 ID 与说明消息 ID。
const ROUTE_MODE_MESSAGES: &[(&str, &str, &str)] = &[
    (
        ROUTE_SINGLE,
        "translate-route-mode-single",
        "translate-route-mode-single-hint",
    ),
    (
        ROUTE_SPECIALIZED_FIRST,
        "translate-route-mode-specialized-first",
        "translate-route-mode-specialized-first-hint",
    ),
    (
        ROUTE_MIXED_SPLIT,
        "translate-route-mode-mixed-split",
        "translate-route-mode-mixed-split-hint",
    ),
];

/// 取路由模式取值的显示名。
///
/// # 参数
/// - `mode`：配置取值（`single` / `specialized_first` / `mixed_split`）。
/// - `locale`：界面语言（内置语言代码，如 `en-US` / `zh-CN`）。
///
/// # 返回
/// 显示名；未知取值返回 `None`。
///
/// # 示例
/// ```ignore
/// assert_eq!(route_mode_label("single", "zh-CN").as_deref(), Some("仅用指定模型"));
/// ```
pub fn route_mode_label(mode: &str, locale: &str) -> Option<String> {
    let (_, label, _) = ROUTE_MODE_MESSAGES
        .iter()
        .find(|(value, _, _)| *value == mode)?;
    Some(i18n_for(locale).tr(label))
}

/// 取路由相关配置项的一行说明（显示在设置行的副标题位置）。
///
/// # 参数
/// - `key`：配置键。
/// - `value`：该项当前值（路由模式时用来挑说明）。
/// - `locale`：界面语言。
///
/// # 返回
/// 说明文本；不属于路由设置的键返回 `None`。
///
/// # 示例
/// ```ignore
/// let hint = route_hint("screenshot_translation/local_max_resident_models", &json!(1), "en-US");
/// ```
pub fn route_hint(key: &str, value: &serde_json::Value, locale: &str) -> Option<String> {
    if key == KEY_LOCAL_MAX_RESIDENT {
        return Some(i18n_for(locale).tr("translate-route-resident-hint"));
    }
    if key == KEY_LOCAL_ROUTE_MODE {
        let mode = value.as_str()?;
        let (_, _, hint) = ROUTE_MODE_MESSAGES.iter().find(|(v, _, _)| *v == mode)?;
        return Some(i18n_for(locale).tr(hint));
    }
    None
}

/// 每个定高行容纳的文本行数（标题算一行）。
const HYMT2_LINES_PER_ROW: usize = 3;

/// 说明区拆成的定高行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hymt2Row {
    /// 文本行：`title` 为真时首行是标题，`lines` 是其后紧跟的说明行区间。
    Text {
        /// 本行是否以标题开头。
        title: bool,
        /// 说明行下标区间。
        lines: std::ops::Range<usize>,
    },
    /// 按钮行（各说明区自己放按钮 / 提示）。
    Actions,
}

/// 把说明区拆成若干定高行，便于混入定高虚拟列表随之滚动。
///
/// # 参数
/// - `line_count`：说明行数。
///
/// # 返回
/// 依次为文本行（标题 + 说明行，每行最多 3 个文本行）与末尾的按钮行。
///
/// # 示例
/// ```ignore
/// assert_eq!(hymt2_rows(7).len(), 4);
/// ```
pub fn hymt2_rows(line_count: usize) -> Vec<Hymt2Row> {
    let mut rows = vec![Hymt2Row::Text {
        title: true,
        lines: 0..line_count.min(HYMT2_LINES_PER_ROW - 1),
    }];
    let mut next = line_count.min(HYMT2_LINES_PER_ROW - 1);
    while next < line_count {
        let end = (next + HYMT2_LINES_PER_ROW).min(line_count);
        rows.push(Hymt2Row::Text {
            title: false,
            lines: next..end,
        });
        next = end;
    }
    rows.push(Hymt2Row::Actions);
    rows
}

/// 把列表下标换算成（说明区行下标 / 普通行位置）。
///
/// # 参数
/// - `index`：虚拟列表下标。
/// - `header_len`：说明区占的行数。
///
/// # 返回
/// `Err(i)` 表示第 `i` 个说明区行，`Ok(p)` 表示第 `p` 个普通可见行。
pub fn split_list_index(index: usize, header_len: usize) -> Result<usize, usize> {
    if index < header_len {
        Err(index)
    } else {
        Ok(index - header_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 全部语言的占位符都不应残留为缺失消息。
    const LOCALES: [&str; 2] = ["en-US", "zh-CN"];

    #[test]
    fn route_labels_cover_all_modes_and_locales() {
        for locale in LOCALES {
            for mode in [ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST, ROUTE_MIXED_SPLIT] {
                let label = route_mode_label(mode, locale).expect("已知模式");
                assert!(
                    !label.is_empty() && !label.starts_with("translate-"),
                    "{locale} {mode}: {label}"
                );
                let hint = route_hint(KEY_LOCAL_ROUTE_MODE, &json!(mode), locale).expect("说明");
                assert!(!hint.starts_with("translate-"), "{locale} {mode}");
            }
        }
        assert!(route_mode_label("auto", "en-US").is_none());
        assert_eq!(
            route_mode_label(ROUTE_SINGLE, "zh-CN").as_deref(),
            Some("仅用指定模型")
        );
    }

    #[test]
    fn route_hint_only_for_route_keys() {
        assert!(route_hint(KEY_LOCAL_MAX_RESIDENT, &json!(1), "en-US").is_some());
        assert!(route_hint("screenshot_translation/local_model_id", &json!(""), "en-US").is_none());
        assert!(route_hint(KEY_LOCAL_ROUTE_MODE, &json!("auto"), "en-US").is_none());
    }

    #[test]
    fn route_hints_fit_two_lines() {
        // 说明在 300px 宽、11px 字号下最多两行：拉丁约 55 字符/行，中文约 27 字/行。
        for locale in LOCALES {
            let budget = if locale == "en-US" { 100 } else { 50 };
            let mut hints = vec![route_hint(KEY_LOCAL_MAX_RESIDENT, &json!(1), locale).unwrap()];
            for mode in [ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST, ROUTE_MIXED_SPLIT] {
                hints.push(route_hint(KEY_LOCAL_ROUTE_MODE, &json!(mode), locale).unwrap());
            }
            for hint in hints {
                assert!(hint.chars().count() <= budget, "{locale}: {hint}");
            }
        }
    }

    /// 拆行覆盖全部文本行且末行是按钮行。
    #[test]
    fn rows_cover_all_lines() {
        for n in [0, 1, 2, 3, 7, 8] {
            let rows = hymt2_rows(n);
            assert_eq!(rows.last(), Some(&Hymt2Row::Actions));
            let mut next = 0;
            for row in &rows[..rows.len() - 1] {
                let Hymt2Row::Text { title, lines } = row else {
                    panic!("中间应为文本行")
                };
                assert_eq!(lines.start, next);
                assert!(lines.len() + usize::from(*title) <= 3);
                next = lines.end;
            }
            assert_eq!(next, n);
        }
        assert_eq!(hymt2_rows(7).len(), 4);
    }

    /// 列表下标：前 header_len 个属于说明区，其后按普通行偏移。
    #[test]
    fn list_index_split_includes_header() {
        assert_eq!(split_list_index(0, 4), Err(0));
        assert_eq!(split_list_index(3, 4), Err(3));
        assert_eq!(split_list_index(4, 4), Ok(0));
        assert_eq!(split_list_index(9, 4), Ok(5));
        assert_eq!(split_list_index(2, 0), Ok(2));
    }
}
