//! 翻译设置页的路由文案与可选包（Hy-MT2）说明：纯数据与文案，不依赖 GPUI，可离屏单测。

use crate::ocr_backend::i18n_for;
use snow_config::extensions::{
    KEY_LOCAL_MAX_RESIDENT, KEY_LOCAL_MODEL_ID, KEY_LOCAL_ROUTE_MODE, ROUTE_MIXED_SPLIT,
    ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST,
};
use snow_i18n::Args;

/// Hy-MT2 可选包的模型 ID（与包清单一致）。
pub const HYMT2_MODEL_ID: &str = "hymt2-1.8b-int4";

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

/// Hy-MT2 可选包的说明区内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hymt2Panel {
    /// 标题。
    pub title: String,
    /// 说明行（模型 ID、体积、内存、速度、适用、短板、许可）。
    pub lines: Vec<String>,
    /// 「使用该模型」按钮文案。
    pub use_label: String,
    /// 「使用中」文案。
    pub in_use_label: String,
    /// 「下载」按钮文案。
    pub download_label: String,
}

/// 生成 Hy-MT2 说明区文案。
///
/// # 参数
/// - `locale`：界面语言。
///
/// # 返回
/// 已本地化的说明区内容。
///
/// # 示例
/// ```ignore
/// let panel = hymt2_panel("en-US");
/// assert_eq!(panel.lines.len(), 7);
/// ```
pub fn hymt2_panel(locale: &str) -> Hymt2Panel {
    let i18n = i18n_for(locale);
    let id_line = i18n.tr_with(
        "translate-hymt2-id",
        &Args::new().named("id", HYMT2_MODEL_ID),
    );
    Hymt2Panel {
        title: i18n.tr("translate-hymt2-title"),
        lines: vec![
            id_line,
            i18n.tr("translate-hymt2-size"),
            i18n.tr("translate-hymt2-memory"),
            i18n.tr("translate-hymt2-latency"),
            i18n.tr("translate-hymt2-fit"),
            i18n.tr("translate-hymt2-weak"),
            i18n.tr("translate-hymt2-license"),
        ],
        use_label: i18n.tr("translate-hymt2-use"),
        in_use_label: i18n.tr("translate-hymt2-in-use"),
        download_label: i18n.tr("translate-hymt2-download"),
    }
}

/// 下载入口的占位行为：目前没有发布地址，只给出手动放置的指引，不发起任何网络请求。
///
/// # 参数
/// - `locale`：界面语言。
///
/// # 返回
/// 要展示给用户的提示文本。
///
/// # 示例
/// ```ignore
/// let note = hymt2_download_notice("zh-CN");
/// ```
pub fn hymt2_download_notice(locale: &str) -> String {
    i18n_for(locale).tr("translate-hymt2-download-unavailable")
}

/// 当前模型 ID 配置是否已指向 Hy-MT2。
///
/// # 参数
/// - `model_id`：`local_model_id` 配置值。
pub fn hymt2_selected(model_id: &str) -> bool {
    model_id == HYMT2_MODEL_ID
}

/// 说明区里可点击的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hymt2Button {
    /// 「使用该模型」。
    Use,
    /// 「下载」（占位）。
    Download,
}

/// 点击按钮后要做的事（由视图层执行，本模块不碰配置与网络）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hymt2Click {
    /// 把某个配置项改成某个字符串值。
    SetConfig {
        /// 配置键。
        key: &'static str,
        /// 新值。
        value: String,
    },
    /// 展示一条提示文本（不发起任何网络请求）。
    ShowNotice(String),
}

/// 说明区当前的展示状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hymt2View {
    /// 文案。
    pub panel: Hymt2Panel,
    /// 是否已选中（选中时「使用该模型」显示为不可点的「使用中」）。
    pub in_use: bool,
}

/// 由当前配置值生成说明区状态。
///
/// # 参数
/// - `locale`：界面语言。
/// - `model_id`：`local_model_id` 当前值。
///
/// # 示例
/// ```ignore
/// assert!(hymt2_view("en-US", HYMT2_MODEL_ID).in_use);
/// ```
pub fn hymt2_view(locale: &str, model_id: &str) -> Hymt2View {
    Hymt2View {
        panel: hymt2_panel(locale),
        in_use: hymt2_selected(model_id),
    }
}

/// 说明区文本行数（不含标题），与 [`hymt2_panel`] 的 `lines` 一致。
pub const HYMT2_LINE_COUNT: usize = 7;

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
    /// 按钮行（使用 / 下载与下载提示）。
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

/// 处理按钮点击：使用 → 把 `local_model_id` 设为 Hy-MT2；下载 → 只给提示。
///
/// # 参数
/// - `button`：被点的按钮。
/// - `locale`：界面语言。
///
/// # 示例
/// ```ignore
/// let click = hymt2_click(Hymt2Button::Use, "zh-CN");
/// ```
pub fn hymt2_click(button: Hymt2Button, locale: &str) -> Hymt2Click {
    match button {
        Hymt2Button::Use => Hymt2Click::SetConfig {
            key: KEY_LOCAL_MODEL_ID,
            value: HYMT2_MODEL_ID.to_string(),
        },
        Hymt2Button::Download => Hymt2Click::ShowNotice(hymt2_download_notice(locale)),
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

    #[test]
    fn hymt2_panel_lists_facts_without_product_name() {
        for locale in LOCALES {
            let panel = hymt2_panel(locale);
            assert_eq!(panel.lines.len(), 7, "{locale}");
            let all = format!("{} {}", panel.title, panel.lines.join(" "));
            assert!(all.contains(HYMT2_MODEL_ID), "{locale}");
            assert!(
                all.contains("1.3 GiB") && all.contains("Apache-2.0"),
                "{locale}"
            );
            assert!(!all.contains(snow_app_core::PRODUCT_NAME), "{locale}");
            assert!(!all.contains("translate-"), "{locale}: 缺消息");
        }
    }

    #[test]
    fn download_placeholder_is_text_only() {
        for locale in LOCALES {
            let note = hymt2_download_notice(locale);
            assert!(
                !note.is_empty() && !note.starts_with("translate-"),
                "{locale}"
            );
            assert!(!note.contains("http"), "{locale}");
        }
    }

    /// 「使用该模型」：设置 local_model_id 为 Hy-MT2 的包 ID。
    #[test]
    fn use_click_sets_local_model_id() {
        assert_eq!(
            hymt2_click(Hymt2Button::Use, "zh-CN"),
            Hymt2Click::SetConfig {
                key: "screenshot_translation/local_model_id",
                value: "hymt2-1.8b-int4".to_string()
            }
        );
    }

    /// 下载占位：只返回提示文本，不含网络地址，也不是配置修改。
    #[test]
    fn download_click_only_shows_notice() {
        for locale in LOCALES {
            let Hymt2Click::ShowNotice(text) = hymt2_click(Hymt2Button::Download, locale) else {
                panic!("下载占位应只提示");
            };
            assert_eq!(text, hymt2_download_notice(locale));
            assert!(!text.contains("http"));
        }
    }

    /// 状态：选中 Hy-MT2 显示“使用中”，其它值（含空）显示可点的“使用该模型”；点击后再渲染即转为使用中。
    #[test]
    fn view_reflects_selection() {
        for locale in LOCALES {
            let idle = hymt2_view(locale, "opus");
            assert!(!idle.in_use && !idle.panel.in_use_label.is_empty());
            assert_ne!(idle.panel.use_label, idle.panel.in_use_label);
            assert!(!hymt2_view(locale, "").in_use);
            assert!(hymt2_view(locale, HYMT2_MODEL_ID).in_use);
        }
        let Hymt2Click::SetConfig { value, .. } = hymt2_click(Hymt2Button::Use, "en-US") else {
            panic!("应为配置修改");
        };
        assert!(hymt2_view("en-US", &value).in_use);
    }

    /// 说明行数常量与文案一致；拆行覆盖全部文本行且末行是按钮行。
    #[test]
    fn rows_cover_all_lines() {
        for locale in LOCALES {
            assert_eq!(hymt2_panel(locale).lines.len(), HYMT2_LINE_COUNT);
        }
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
        assert_eq!(hymt2_rows(HYMT2_LINE_COUNT).len(), 4);
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

    #[test]
    fn selected_matches_exact_id() {
        assert!(hymt2_selected(HYMT2_MODEL_ID));
        assert!(!hymt2_selected(""));
        assert!(!hymt2_selected("opus"));
    }
}
