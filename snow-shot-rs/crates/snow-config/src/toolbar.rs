//! 工具栏布局规范化：常量、默认布局与 `normalizeToolbarLayout` 的移植。
//!
//! 对应 C++ `configurationschema.cpp` 中的 `normalizeToolbarLayout` 及相关常量。
//! 初稿由 antigravity 产出，已逐行对照 C++ 原文复审。

use crate::value::{Normalization, json_eq};
use serde_json::{Value, json};
use std::collections::HashSet;

/// 布局对象里分组列表的键名
const POSITIONS_KEY: &str = "positions";
/// 布局对象里隐藏项列表的键名
const HIDDEN_KEY: &str = "hidden";
/// 工具栏项标识 `shape`
pub const ID_SHAPE: &str = "shape";
/// 工具栏项标识 `arrow`
pub const ID_ARROW: &str = "arrow";
/// 工具栏项标识 `line`
pub const ID_LINE: &str = "line";
/// 工具栏项标识 `free-draw`
pub const ID_FREE_DRAW: &str = "free-draw";
/// 工具栏项标识 `highlighter`
pub const ID_HIGHLIGHTER: &str = "highlighter";
/// 工具栏项标识 `spotlight`
const ID_SPOTLIGHT: &str = "spotlight";
/// 工具栏项标识 `text`
pub const ID_TEXT: &str = "text";
/// 工具栏项标识 `serial-number`
pub const ID_SERIAL_NUMBER: &str = "serial-number";
/// 工具栏项标识 `filter`
pub const ID_FILTER: &str = "filter";
/// 工具栏项标识 `eraser`
pub const ID_ERASER: &str = "eraser";
/// 工具栏项标识 `watermark`
const ID_WATERMARK: &str = "watermark";
/// 工具栏项标识 `separator`
const ID_SEPARATOR: &str = "separator";
/// 工具栏项标识 `undo`
const ID_UNDO: &str = "undo";
/// 工具栏项标识 `redo`
const ID_REDO: &str = "redo";
/// 工具栏项标识 `barcode-recognition`
const ID_BARCODE_RECOGNITION: &str = "barcode-recognition";
/// 工具栏项标识 `table-recognition`
const ID_TABLE_RECOGNITION: &str = "table-recognition";
/// 工具栏项标识 `convert-to-markdown`
const ID_CONVERT_TO_MARKDOWN: &str = "convert-to-markdown";
/// 工具栏项标识 `convert-to-html`
const ID_CONVERT_TO_HTML: &str = "convert-to-html";
/// 工具栏项标识 `record-screen`
const ID_RECORD_SCREEN: &str = "record-screen";
/// 工具栏项标识 `pin-to-screen`
const ID_PIN_TO_SCREEN: &str = "pin-to-screen";
/// 工具栏项标识 `text-recognition`
const ID_TEXT_RECOGNITION: &str = "text-recognition";
/// 工具栏项标识 `text-translation`
const ID_TEXT_TRANSLATION: &str = "text-translation";
/// 工具栏项标识 `scrolling-screenshot`
const ID_SCROLLING_SCREENSHOT: &str = "scrolling-screenshot";
/// 工具栏项标识 `save-as-file`
const ID_SAVE_AS_FILE: &str = "save-as-file";
/// 快速保存功能标识符
const QUICK_SAVE_ID: &str = "quick-save";
/// LaTeX 公式识别功能标识符
const LATEX_RECOGNITION_ID: &str = "latex-recognition";

/// 绘图工具的内部标识符列表（`kDrawingToolIds`）。
pub const DRAWING_TOOL_IDS: &[&str] = &[
    ID_SHAPE,
    ID_ARROW,
    ID_LINE,
    ID_FREE_DRAW,
    ID_HIGHLIGHTER,
    ID_SPOTLIGHT,
    ID_TEXT,
    ID_SERIAL_NUMBER,
    ID_FILTER,
    ID_ERASER,
    ID_WATERMARK,
];

/// 绘图工具栏可用项（`kDrawingToolbarItemIds`，工具 + separator/undo/redo）。
pub const DRAWING_TOOLBAR_ITEM_IDS: &[&str] = &[
    ID_SHAPE,
    ID_ARROW,
    ID_LINE,
    ID_FREE_DRAW,
    ID_HIGHLIGHTER,
    ID_SPOTLIGHT,
    ID_TEXT,
    ID_SERIAL_NUMBER,
    ID_FILTER,
    ID_ERASER,
    ID_WATERMARK,
    ID_SEPARATOR,
    ID_UNDO,
    ID_REDO,
];

/// 最近使用的绘图工具取值集合（`kLastDrawingToolIds`，首项为空串）。
pub const LAST_DRAWING_TOOL_IDS: &[&str] = &[
    "",
    ID_SHAPE,
    ID_ARROW,
    ID_LINE,
    ID_FREE_DRAW,
    ID_HIGHLIGHTER,
    ID_SPOTLIGHT,
    ID_TEXT,
    ID_SERIAL_NUMBER,
    ID_FILTER,
    ID_ERASER,
    ID_WATERMARK,
];

/// 动作工具栏可用项（`kActionToolbarItemIds`）。
pub const ACTION_TOOLBAR_ITEM_IDS: &[&str] = &[
    ID_BARCODE_RECOGNITION,
    ID_TABLE_RECOGNITION,
    ID_CONVERT_TO_MARKDOWN,
    LATEX_RECOGNITION_ID,
    ID_CONVERT_TO_HTML,
    ID_RECORD_SCREEN,
    ID_PIN_TO_SCREEN,
    ID_TEXT_RECOGNITION,
    ID_TEXT_TRANSLATION,
    ID_SCROLLING_SCREENSHOT,
    QUICK_SAVE_ID,
    ID_SAVE_AS_FILE,
];

/// 贴图动作工具栏可用项（`kPinnedActionToolbarItemIds`）。
pub const PINNED_ACTION_TOOLBAR_ITEM_IDS: &[&str] = &[
    ID_BARCODE_RECOGNITION,
    ID_TABLE_RECOGNITION,
    ID_CONVERT_TO_MARKDOWN,
    LATEX_RECOGNITION_ID,
    ID_CONVERT_TO_HTML,
    ID_TEXT_RECOGNITION,
    ID_TEXT_TRANSLATION,
];

/// 绘图工具栏默认分组（`defaultDrawingToolbarPositions`）。
pub const DRAWING_DEFAULT_POSITIONS: &[&[&str]] = &[
    &[ID_SHAPE],
    &[ID_LINE, ID_ARROW],
    &[ID_FREE_DRAW],
    &[ID_SPOTLIGHT, ID_HIGHLIGHTER],
    &[ID_TEXT],
    &[ID_SERIAL_NUMBER],
    &[ID_FILTER],
    &[ID_ERASER],
    &[ID_WATERMARK],
    &[ID_SEPARATOR],
    &[ID_UNDO],
    &[ID_REDO],
];

/// 动作工具栏默认分组（`defaultActionToolbarPositions`）。
pub const ACTION_DEFAULT_POSITIONS: &[&[&str]] = &[
    &[
        ID_CONVERT_TO_HTML,
        ID_CONVERT_TO_MARKDOWN,
        LATEX_RECOGNITION_ID,
        ID_BARCODE_RECOGNITION,
        ID_TABLE_RECOGNITION,
    ],
    &[ID_RECORD_SCREEN],
    &[ID_PIN_TO_SCREEN],
    &[ID_TEXT_RECOGNITION],
    &[ID_TEXT_TRANSLATION],
    &[ID_SCROLLING_SCREENSHOT],
    &[QUICK_SAVE_ID, ID_SAVE_AS_FILE],
];

/// 贴图动作工具栏默认分组（`defaultPinnedActionToolbarPositions`）。
pub const PINNED_DEFAULT_POSITIONS: &[&[&str]] = &[
    &[
        ID_CONVERT_TO_HTML,
        ID_CONVERT_TO_MARKDOWN,
        LATEX_RECOGNITION_ID,
        ID_BARCODE_RECOGNITION,
        ID_TABLE_RECOGNITION,
    ],
    &[ID_TEXT_RECOGNITION],
    &[ID_TEXT_TRANSLATION],
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
        POSITIONS_KEY: positions,
        HIDDEN_KEY: []
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
            if id == ID_SEPARATOR {
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

    let Some(position_groups) = object.get(POSITIONS_KEY).and_then(Value::as_array) else {
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
    if let Some(hidden_items) = object.get(HIDDEN_KEY).and_then(Value::as_array) {
        for id in hidden_items.iter().filter_map(Value::as_str) {
            if known.contains(id) && !positioned.contains(id) && !hidden_set.contains(id) {
                hidden.push(id.to_string());
                hidden_set.insert(id.to_string());
            }
        }
    }

    if known.contains(QUICK_SAVE_ID)
        && !positioned.contains(QUICK_SAVE_ID)
        && !hidden_set.contains(QUICK_SAVE_ID)
    {
        for position in &mut positions {
            if let Some(save_index) = position.iter().position(|id| id == ID_SAVE_AS_FILE) {
                position.insert(save_index, QUICK_SAVE_ID.to_string());
                positioned.insert(QUICK_SAVE_ID.to_string());
                break;
            }
        }
        if !positioned.contains(QUICK_SAVE_ID) && hidden_set.contains(ID_SAVE_AS_FILE) {
            hidden.push(QUICK_SAVE_ID.to_string());
            hidden_set.insert(QUICK_SAVE_ID.to_string());
        }
    }

    if migrate_screenshot_layout && !positions.is_empty() && known.contains(ID_CONVERT_TO_MARKDOWN)
    {
        // 升级早期默认布局，不动用户自定义位置
        let mut previous_default = to_owned_positions(default_positions);
        previous_default[0] = vec![
            ID_BARCODE_RECOGNITION.to_string(),
            ID_TABLE_RECOGNITION.into(),
        ];
        previous_default.insert(1, vec![ID_CONVERT_TO_MARKDOWN.to_string()]);
        previous_default.insert(2, vec![ID_CONVERT_TO_HTML.to_string()]);
        let mut previous_grouped_default = to_owned_positions(default_positions);
        previous_grouped_default[0] = [ID_TABLE_RECOGNITION, ID_BARCODE_RECOGNITION]
            .iter()
            .chain([ID_CONVERT_TO_MARKDOWN, ID_CONVERT_TO_HTML].iter())
            .map(|id| (*id).to_string())
            .collect();
        if hidden.is_empty()
            && (positions == previous_default || positions == previous_grouped_default)
        {
            positions = to_owned_positions(default_positions);
        }
        let mut recognition_position: Option<usize> = None;
        for anchor in [ID_BARCODE_RECOGNITION, ID_TABLE_RECOGNITION] {
            if let Some(index) = positions
                .iter()
                .position(|group| group.iter().any(|id| id == anchor))
            {
                recognition_position = Some(index);
                break;
            }
        }
        for id in [ID_CONVERT_TO_MARKDOWN, ID_CONVERT_TO_HTML] {
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
        ID_CONVERT_TO_HTML,
        LATEX_RECOGNITION_ID,
        ID_CONVERT_TO_MARKDOWN,
        ID_BARCODE_RECOGNITION,
        ID_TABLE_RECOGNITION,
    ]
    .iter()
    .map(|id| (*id).to_string())
    .collect();
    for position in &mut positions {
        if *position == previous_recognition_group {
            position.swap(1, 2);
        }
    }
    if known.contains(LATEX_RECOGNITION_ID)
        && !positioned.contains(LATEX_RECOGNITION_ID)
        && !hidden_set.contains(LATEX_RECOGNITION_ID)
    {
        for position in &mut positions {
            if position.iter().any(|id| id == LATEX_RECOGNITION_ID) {
                positioned.insert(LATEX_RECOGNITION_ID.to_string());
                break;
            }
            if let Some(index) = position.iter().position(|id| id == ID_CONVERT_TO_MARKDOWN) {
                // 弹出按钮会反转保存的栈：插在 Markdown 之后才会显示在其左侧
                position.insert(index + 1, LATEX_RECOGNITION_ID.to_string());
                positioned.insert(LATEX_RECOGNITION_ID.to_string());
                break;
            }
        }
        if !positioned.contains(LATEX_RECOGNITION_ID) && hidden_set.contains(ID_CONVERT_TO_MARKDOWN)
        {
            hidden.push(LATEX_RECOGNITION_ID.to_string());
            hidden_set.insert(LATEX_RECOGNITION_ID.to_string());
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

    let normalized = json!({ POSITIONS_KEY: positions, HIDDEN_KEY: hidden });
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
