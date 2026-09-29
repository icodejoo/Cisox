//! 工具栏布局规范化：常量、默认布局与 `normalizeToolbarLayout` 的移植。
//!
//! 对应 C++ `configurationschema.cpp` 中的 `normalizeToolbarLayout` 及相关常量。
//! 初稿由 antigravity 产出，已逐行对照 C++ 原文复审。

use crate::value::{Normalization, json_eq};
use serde_json::{Value, json};
use std::collections::HashSet;

/// 绘图工具的内部标识符列表（`kDrawingToolIds`）。
pub const DRAWING_TOOL_IDS: &[&str] = &[
    "shape",
    "arrow",
    "line",
    "free-draw",
    "highlighter",
    "spotlight",
    "text",
    "serial-number",
    "filter",
    "eraser",
    "watermark",
];

/// 绘图工具栏可用项（`kDrawingToolbarItemIds`，工具 + separator/undo/redo）。
pub const DRAWING_TOOLBAR_ITEM_IDS: &[&str] = &[
    "shape",
    "arrow",
    "line",
    "free-draw",
    "highlighter",
    "spotlight",
    "text",
    "serial-number",
    "filter",
    "eraser",
    "watermark",
    "separator",
    "undo",
    "redo",
];

/// 最近使用的绘图工具取值集合（`kLastDrawingToolIds`，首项为空串）。
pub const LAST_DRAWING_TOOL_IDS: &[&str] = &[
    "",
    "shape",
    "arrow",
    "line",
    "free-draw",
    "highlighter",
    "spotlight",
    "text",
    "serial-number",
    "filter",
    "eraser",
    "watermark",
];

/// 动作工具栏可用项（`kActionToolbarItemIds`）。
pub const ACTION_TOOLBAR_ITEM_IDS: &[&str] = &[
    "barcode-recognition",
    "table-recognition",
    "convert-to-markdown",
    "latex-recognition",
    "convert-to-html",
    "record-screen",
    "pin-to-screen",
    "text-recognition",
    "text-translation",
    "scrolling-screenshot",
    "quick-save",
    "save-as-file",
];

/// 贴图动作工具栏可用项（`kPinnedActionToolbarItemIds`）。
pub const PINNED_ACTION_TOOLBAR_ITEM_IDS: &[&str] = &[
    "barcode-recognition",
    "table-recognition",
    "convert-to-markdown",
    "latex-recognition",
    "convert-to-html",
    "text-recognition",
    "text-translation",
];

/// 绘图工具栏默认分组（`defaultDrawingToolbarPositions`）。
pub const DRAWING_DEFAULT_POSITIONS: &[&[&str]] = &[
    &["shape"],
    &["line", "arrow"],
    &["free-draw"],
    &["spotlight", "highlighter"],
    &["text"],
    &["serial-number"],
    &["filter"],
    &["eraser"],
    &["watermark"],
    &["separator"],
    &["undo"],
    &["redo"],
];

/// 动作工具栏默认分组（`defaultActionToolbarPositions`）。
pub const ACTION_DEFAULT_POSITIONS: &[&[&str]] = &[
    &[
        "convert-to-html",
        "convert-to-markdown",
        "latex-recognition",
        "barcode-recognition",
        "table-recognition",
    ],
    &["record-screen"],
    &["pin-to-screen"],
    &["text-recognition"],
    &["text-translation"],
    &["scrolling-screenshot"],
    &["quick-save", "save-as-file"],
];

/// 贴图动作工具栏默认分组（`defaultPinnedActionToolbarPositions`）。
pub const PINNED_DEFAULT_POSITIONS: &[&[&str]] = &[
    &[
        "convert-to-html",
        "convert-to-markdown",
        "latex-recognition",
        "barcode-recognition",
        "table-recognition",
    ],
    &["text-recognition"],
    &["text-translation"],
];

/// 生成默认工具栏布局对象 `{"positions": ..., "hidden": []}`。
///
/// # 参数
/// - `positions`：分组列表
///
/// # 返回
/// 布局 JSON 对象。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::toolbar::default_toolbar_layout;
///
/// let layout = default_toolbar_layout(&[&["shape"], &["line"]]);
/// assert_eq!(layout, json!({"positions": [["shape"], ["line"]], "hidden": []}));
/// ```
pub fn default_toolbar_layout(positions: &[&[&str]]) -> Value {
    json!({
        "positions": positions,
        "hidden": []
    })
}

/// 把 `&[&[&str]]` 转成 `Vec<Vec<String>>`。
fn to_owned_positions(positions: &[&[&str]]) -> Vec<Vec<String>> {
    positions
        .iter()
        .map(|group| group.iter().map(|id| (*id).to_string()).collect())
        .collect()
}

/// 追加一个分组：过滤未知/重复/隐藏项，`separator` 独占一组。
fn append_position(
    known: &HashSet<&str>,
    ids: &[String],
    positions: &mut Vec<Vec<String>>,
    positioned: &mut HashSet<String>,
    hidden_set: &HashSet<String>,
) {
    let mut position: Vec<String> = Vec::new();
    for id in ids {
        if known.contains(id.as_str()) && !positioned.contains(id) && !hidden_set.contains(id) {
            if id == "separator" {
                if !position.is_empty() {
                    positions.push(std::mem::take(&mut position));
                }
                positions.push(vec![id.clone()]);
                positioned.insert(id.clone());
                continue;
            }
            position.push(id.clone());
            positioned.insert(id.clone());
        }
    }
    if !position.is_empty() {
        positions.push(position);
    }
}

/// 规范化工具栏布局，等价于 C++ `normalizeToolbarLayout`。
///
/// 丢弃未知/重复项、按可见优先处理隐藏项、按默认布局补齐缺失项，
/// 并对历史默认布局做升级（快速保存、Markdown/HTML/LaTeX 识别）。
///
/// # 参数
/// - `value`：待规范化的 JSON（须为含 `positions` 数组的对象）
/// - `item_ids`：该工具栏允许的项 ID
/// - `default_positions`：默认分组，用于补齐缺失项
/// - `migrate_screenshot_layout`：是否启用截图动作栏的历史布局迁移
///
/// # 返回
/// 非对象或缺少 `positions` 数组时为非法（调用方回退默认值）。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::toolbar::{
///     DRAWING_DEFAULT_POSITIONS, DRAWING_TOOLBAR_ITEM_IDS, normalize_toolbar_layout,
/// };
///
/// let input = json!({"positions": [["shape"]], "hidden": []});
/// let norm = normalize_toolbar_layout(
///     &input,
///     DRAWING_TOOLBAR_ITEM_IDS,
///     DRAWING_DEFAULT_POSITIONS,
///     false,
/// );
/// assert!(norm.valid);
/// ```
pub fn normalize_toolbar_layout(
    value: &Value,
    item_ids: &[&str],
    default_positions: &[&[&str]],
    migrate_screenshot_layout: bool,
) -> Normalization {
    let Some(object) = value.as_object() else {
        return Normalization::invalid();
    };

    let known: HashSet<&str> = item_ids.iter().copied().collect();
    let mut positions: Vec<Vec<String>> = Vec::new();
    let mut positioned: HashSet<String> = HashSet::new();
    let mut hidden: Vec<String> = Vec::new();
    let mut hidden_set: HashSet<String> = HashSet::new();

    let Some(position_groups) = object.get("positions").and_then(Value::as_array) else {
        return Normalization::invalid();
    };
    for group in position_groups {
        let Some(items) = group.as_array() else {
            continue;
        };
        let ids: Vec<String> = items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        append_position(&known, &ids, &mut positions, &mut positioned, &hidden_set);
    }
    if let Some(hidden_items) = object.get("hidden").and_then(Value::as_array) {
        for id in hidden_items.iter().filter_map(Value::as_str) {
            if known.contains(id) && !positioned.contains(id) && !hidden_set.contains(id) {
                hidden.push(id.to_string());
                hidden_set.insert(id.to_string());
            }
        }
    }

    if known.contains("quick-save")
        && !positioned.contains("quick-save")
        && !hidden_set.contains("quick-save")
    {
        for position in &mut positions {
            if let Some(save_index) = position.iter().position(|id| id == "save-as-file") {
                position.insert(save_index, "quick-save".to_string());
                positioned.insert("quick-save".to_string());
                break;
            }
        }
        if !positioned.contains("quick-save") && hidden_set.contains("save-as-file") {
            hidden.push("quick-save".to_string());
            hidden_set.insert("quick-save".to_string());
        }
    }

    if migrate_screenshot_layout && !positions.is_empty() && known.contains("convert-to-markdown") {
        // 升级早期默认布局，不动用户自定义位置
        let mut previous_default = to_owned_positions(default_positions);
        previous_default[0] = vec![
            "barcode-recognition".to_string(),
            "table-recognition".into(),
        ];
        previous_default.insert(1, vec!["convert-to-markdown".to_string()]);
        previous_default.insert(2, vec!["convert-to-html".to_string()]);
        let mut previous_grouped_default = to_owned_positions(default_positions);
        previous_grouped_default[0] = ["table-recognition", "barcode-recognition"]
            .iter()
            .chain(["convert-to-markdown", "convert-to-html"].iter())
            .map(|id| (*id).to_string())
            .collect();
        if hidden.is_empty()
            && (positions == previous_default || positions == previous_grouped_default)
        {
            positions = to_owned_positions(default_positions);
        }
        let mut recognition_position: Option<usize> = None;
        for anchor in ["barcode-recognition", "table-recognition"] {
            if let Some(index) = positions
                .iter()
                .position(|group| group.iter().any(|id| id == anchor))
            {
                recognition_position = Some(index);
                break;
            }
        }
        for id in ["convert-to-markdown", "convert-to-html"] {
            if let Some(index) = recognition_position
                && !positioned.contains(id)
                && !hidden_set.contains(id)
            {
                positions[index].push(id.to_string());
                positioned.insert(id.to_string());
            }
        }
    }

    // 升级上一版默认识别分组，保留用户自定义排列
    let previous_recognition_group: Vec<String> = [
        "convert-to-html",
        "latex-recognition",
        "convert-to-markdown",
        "barcode-recognition",
        "table-recognition",
    ]
    .iter()
    .map(|id| (*id).to_string())
    .collect();
    for position in &mut positions {
        if *position == previous_recognition_group {
            position.swap(1, 2);
        }
    }
    if known.contains("latex-recognition")
        && !positioned.contains("latex-recognition")
        && !hidden_set.contains("latex-recognition")
    {
        for position in &mut positions {
            if position.iter().any(|id| id == "latex-recognition") {
                positioned.insert("latex-recognition".to_string());
                break;
            }
            if let Some(index) = position.iter().position(|id| id == "convert-to-markdown") {
                // 弹出按钮会反转保存的栈：插在 Markdown 之后才会显示在其左侧
                position.insert(index + 1, "latex-recognition".to_string());
                positioned.insert("latex-recognition".to_string());
                break;
            }
        }
        if !positioned.contains("latex-recognition") && hidden_set.contains("convert-to-markdown") {
            hidden.push("latex-recognition".to_string());
            hidden_set.insert("latex-recognition".to_string());
        }
    }
    for default_position in default_positions {
        let missing: Vec<String> = default_position
            .iter()
            .filter(|id| !positioned.contains(**id) && !hidden_set.contains(**id))
            .map(|id| (*id).to_string())
            .collect();
        append_position(
            &known,
            &missing,
            &mut positions,
            &mut positioned,
            &hidden_set,
        );
    }

    let normalized = json!({ "positions": positions, "hidden": hidden });
    let changed = !json_eq(&normalized, value);
    Normalization::ok(normalized, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 绘图栏：重复/未知/隐藏冲突项的处理（C++ screenshotUiSchemaRepairsStructuredValues）。
    #[test]
    fn drawing_layout_repair_matches_cpp() {
        let input = json!({
            "positions": [
                ["watermark", "shape", "unknown", "watermark"],
                ["line", "shape"],
                "not-a-position",
                ["unknown-highlight", "unknown-pen"]
            ],
            "hidden": ["shape", "arrow", "free-draw", "unknown-highlight", "arrow"]
        });
        let out = normalize_toolbar_layout(
            &input,
            DRAWING_TOOLBAR_ITEM_IDS,
            DRAWING_DEFAULT_POSITIONS,
            false,
        );
        assert!(out.valid && out.changed);
        assert_eq!(
            out.value,
            json!({
                "positions": [
                    ["watermark", "shape"], ["line"], ["spotlight", "highlighter"], ["text"],
                    ["serial-number"], ["filter"], ["eraser"], ["separator"], ["undo"], ["redo"]
                ],
                "hidden": ["arrow", "free-draw"]
            })
        );
    }

    /// 绘图栏：separator 独占一组。
    #[test]
    fn separator_gets_own_position() {
        let input = json!({
            "positions": [["shape", "separator", "undo", "redo"], []],
            "hidden": []
        });
        let out = normalize_toolbar_layout(
            &input,
            DRAWING_TOOLBAR_ITEM_IDS,
            DRAWING_DEFAULT_POSITIONS,
            false,
        );
        let positions = out.value["positions"].as_array().unwrap();
        assert!(out.valid && out.changed && positions.len() >= 3);
        assert_eq!(positions[0], json!(["shape"]));
        assert_eq!(positions[1], json!(["separator"]));
        assert_eq!(positions[2], json!(["undo", "redo"]));
    }

    /// 隐藏的 separator 保持隐藏。
    #[test]
    fn hidden_separator_stays_hidden() {
        let input = json!({ "positions": [["shape"], []], "hidden": ["separator"] });
        let out = normalize_toolbar_layout(
            &input,
            DRAWING_TOOLBAR_ITEM_IDS,
            DRAWING_DEFAULT_POSITIONS,
            false,
        );
        assert!(out.valid);
        assert_eq!(out.value["hidden"], json!(["separator"]));
        assert!(
            !out.value["positions"]
                .as_array()
                .unwrap()
                .contains(&json!(["separator"]))
        );
    }

    /// 动作栏：丢弃无效项、可见优先、按默认位置补齐缺失项。
    #[test]
    fn action_layout_repair_matches_cpp() {
        let input = json!({
            "positions": [
                ["save-as-file", "table-recognition", "save-as-file", "unknown"],
                "not-a-position",
                ["record-screen", "table-recognition"]
            ],
            "hidden": [
                "table-recognition", "barcode-recognition", "text-recognition",
                "barcode-recognition", "unknown"
            ]
        });
        let out = normalize_toolbar_layout(
            &input,
            ACTION_TOOLBAR_ITEM_IDS,
            ACTION_DEFAULT_POSITIONS,
            true,
        );
        assert!(out.valid && out.changed);
        assert_eq!(
            out.value["positions"],
            json!([
                [
                    "quick-save",
                    "save-as-file",
                    "table-recognition",
                    "convert-to-markdown",
                    "latex-recognition",
                    "convert-to-html"
                ],
                ["record-screen"],
                ["pin-to-screen"],
                ["text-translation"],
                ["scrolling-screenshot"]
            ])
        );
        assert_eq!(
            out.value["hidden"],
            json!(["barcode-recognition", "text-recognition"])
        );
    }

    /// 动作栏全部隐藏：合法且不变。
    #[test]
    fn all_hidden_action_layout_is_stable() {
        let input = json!({
            "positions": [],
            "hidden": [
                "barcode-recognition", "table-recognition", "convert-to-markdown",
                "convert-to-html", "latex-recognition", "record-screen", "pin-to-screen",
                "text-recognition", "text-translation", "scrolling-screenshot", "quick-save",
                "save-as-file"
            ]
        });
        let out = normalize_toolbar_layout(
            &input,
            ACTION_TOOLBAR_ITEM_IDS,
            ACTION_DEFAULT_POSITIONS,
            true,
        );
        assert!(out.valid && !out.changed);
        assert_eq!(out.value, input);
    }

    /// 默认布局自身规范化后不变；非对象/缺 positions 为非法。
    #[test]
    fn defaults_are_fixed_points_and_bad_shapes_invalid() {
        for (ids, defaults, migrate) in [
            (DRAWING_TOOLBAR_ITEM_IDS, DRAWING_DEFAULT_POSITIONS, false),
            (ACTION_TOOLBAR_ITEM_IDS, ACTION_DEFAULT_POSITIONS, true),
            (
                PINNED_ACTION_TOOLBAR_ITEM_IDS,
                PINNED_DEFAULT_POSITIONS,
                false,
            ),
        ] {
            let layout = default_toolbar_layout(defaults);
            let out = normalize_toolbar_layout(&layout, ids, defaults, migrate);
            assert!(out.valid && !out.changed, "默认布局应为不动点");
        }
        assert!(!normalize_toolbar_layout(&json!([]), &[], &[], false).valid);
        assert!(
            !normalize_toolbar_layout(&json!({"hidden": []}), DRAWING_TOOLBAR_ITEM_IDS, &[], false)
                .valid
        );
    }
}
