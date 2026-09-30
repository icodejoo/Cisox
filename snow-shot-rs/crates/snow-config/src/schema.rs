//! 配置 schema：238 个 `"组/名"` 键的类型、默认值、取值约束。
//!
//! 对应 C++ `ConfigurationSchema` 的 `entries()/entry()/defaultValue()/completeDefaultDocument()`。
//! 键名、默认值、范围、白名单均由 C++ 源码机械转换（见 `schema_table.rs`）。

use crate::extensions::extension_entries;
use crate::schema_table::raw_entries;
use crate::shortcut::{shortcut_bindings_from_json, shortcut_bindings_to_json};
use serde_json::{Map, Value};
use snow_app_core::PRODUCT_NAME;
use std::collections::HashMap;
use std::sync::OnceLock;

/// 与 C++ 逐项对齐的核心条目数；其后是 Cisox 扩展项（见 [`crate::extensions`]）。
pub const CORE_ENTRY_COUNT: usize = 238;

/// 使用快捷键列表语义的分组前缀（C++ `shortcutConfigurationKey`）。
const SHORTCUT_GROUP_PREFIXES: [&str; 5] = [
    "global_shortcuts/",
    "drawing_shortcuts/",
    "screenshot_shortcuts/",
    "screen_recording_shortcuts/",
    "pin_to_screen_shortcuts/",
];

/// 文件名时间戳模板（与 C++ 一致）。
const FILENAME_TIMESTAMP_TEMPLATE: &str = "{YYYY-MM-DD_HH-mm-ss}";

/// 默认截图文件名格式：`{PRODUCT_NAME}_{YYYY-MM-DD_HH-mm-ss}`。
pub(crate) fn default_screenshot_filename_format() -> String {
    format!("{PRODUCT_NAME}_{FILENAME_TIMESTAMP_TEMPLATE}")
}

/// 默认录屏文件名格式：`{PRODUCT_NAME}_Video_{YYYY-MM-DD_HH-mm-ss}`。
pub(crate) fn default_video_filename_format() -> String {
    format!("{PRODUCT_NAME}_Video_{FILENAME_TIMESTAMP_TEMPLATE}")
}

/// 版本号所在键。
pub const SCHEMA_VERSION_KEY: &str = "storage/schema_version";

/// 值类型（C++ `ConfigurationValueKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// 布尔。
    Boolean,
    /// 整数（可带范围）。
    Integer,
    /// 字符串（可带白名单）。
    String,
    /// 字符串列表。
    StringList,
    /// 快捷键列表（结构化绑定）。
    ShortcutList,
    /// 任意结构化 JSON。
    Structured,
}

/// 整数范围约束（C++ `ConfigurationIntegerRange`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntRange {
    /// 下限（含）。
    pub min: i32,
    /// 上限（含）。
    pub max: i32,
    /// UI 步长（校验不使用）。
    pub step: i32,
}

/// 一个 schema 条目（C++ `ConfigurationSchemaEntry`）。
#[derive(Debug, Clone)]
pub struct SchemaEntry {
    /// `"组/名"` 键。
    pub key: &'static str,
    /// 默认值。
    pub default: Value,
    /// 值类型。
    pub kind: ValueKind,
    /// 整数范围。
    pub range: Option<IntRange>,
    /// 字符串/字符串列表白名单；空表示不限制。
    pub allowed: &'static [&'static str],
    /// 列表最大长度；`None` 表示不限。
    pub max_items: Option<usize>,
}

/// 默认输出目录类别（对应 Qt 的 Movies/Pictures 标准位置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDirKind {
    /// 视频目录。
    Videos,
    /// 图片目录。
    Pictures,
}

/// 构造条目（供 `schema_table.rs` 使用）。
pub(crate) fn entry(
    key: &'static str,
    default: Value,
    kind: ValueKind,
    range: Option<IntRange>,
    allowed: &'static [&'static str],
    max_items: Option<usize>,
) -> SchemaEntry {
    SchemaEntry {
        key,
        default,
        kind,
        range,
        allowed,
        max_items,
    }
}

/// 计算默认输出目录：`<用户目录>/Videos|Pictures`，使用正斜杠（与 Qt 一致）；无法确定时回退 `Documents`。
///
/// 用户目录取自 `USERPROFILE`（Windows）或 `HOME`。
pub(crate) fn default_output_directory(kind: OutputDirKind) -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    if home.is_empty() {
        return String::new();
    }
    let leaf = match kind {
        OutputDirKind::Videos => "Videos",
        OutputDirKind::Pictures => "Pictures",
    };
    format!("{}/{leaf}", home.replace('\\', "/").trim_end_matches('/'))
}

/// 构建条目：把快捷键分组下的 `StringList` 升级为 `ShortcutList`，默认值转为结构化绑定
/// （C++ `buildEntries`）。
fn build_entries() -> Vec<SchemaEntry> {
    let mut entries = raw_entries();
    entries.extend(extension_entries());
    for item in &mut entries {
        let is_shortcut_group = SHORTCUT_GROUP_PREFIXES
            .iter()
            .any(|prefix| item.key.starts_with(prefix));
        if !is_shortcut_group || item.kind != ValueKind::StringList {
            continue;
        }
        item.kind = ValueKind::ShortcutList;
        item.default =
            shortcut_bindings_to_json(&shortcut_bindings_from_json(&item.default, true, None).0);
    }
    entries
}

/// 全部条目：前 238 项顺序与 C++ 一致，之后是 Cisox 扩展项。
///
/// # 示例
/// ```
/// use snow_config::schema::{CORE_ENTRY_COUNT, entries};
/// assert!(entries().len() >= CORE_ENTRY_COUNT);
/// assert_eq!(entries()[0].key, "api_configuration/custom_models");
/// ```
pub fn entries() -> &'static [SchemaEntry] {
    static ENTRIES: OnceLock<Vec<SchemaEntry>> = OnceLock::new();
    ENTRIES.get_or_init(build_entries)
}

/// 与 C++ 对齐的 238 个核心条目（不含 Cisox 扩展项）。
///
/// # 示例
/// ```
/// assert_eq!(snow_config::schema::core_entries().len(), 238);
/// ```
pub fn core_entries() -> &'static [SchemaEntry] {
    &entries()[..CORE_ENTRY_COUNT]
}

/// 键是否属于 Cisox 扩展项（不在 C++ 的 238 项里）。
///
/// # 参数
/// - `key`：`"组/名"` 键
///
/// # 示例
/// ```
/// assert!(snow_config::schema::is_extension_key("screenshot_translation/backend"));
/// assert!(!snow_config::schema::is_extension_key("screenshot_translation/model"));
/// ```
pub fn is_extension_key(key: &str) -> bool {
    entries()[CORE_ENTRY_COUNT..].iter().any(|item| item.key == key)
}

/// 按键查找条目。
///
/// # 参数
/// - `key`：`"组/名"` 键
///
/// # 返回
/// 条目引用；未知键返回 `None`。
///
/// # 示例
/// ```
/// let entry = snow_config::schema::entry_for("capture_history/retention_days").unwrap();
/// assert_eq!(entry.default, serde_json::json!(7));
/// ```
pub fn entry_for(key: &str) -> Option<&'static SchemaEntry> {
    static INDEX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    let index = INDEX.get_or_init(|| {
        entries()
            .iter()
            .enumerate()
            .map(|(position, item)| (item.key, position))
            .collect()
    });
    index.get(key).map(|position| &entries()[*position])
}

/// 键是否存在于 schema。
pub fn contains(key: &str) -> bool {
    entry_for(key).is_some()
}

/// 键的默认值；未知键返回 `Null`。
///
/// # 示例
/// ```
/// assert_eq!(snow_config::schema::default_value("mcp/enabled"), serde_json::json!(false));
/// ```
pub fn default_value(key: &str) -> Value {
    entry_for(key).map_or(Value::Null, |item| item.default.clone())
}

/// 当前 schema 版本（取自 `storage/schema_version` 的默认值，即 3）。
pub fn current_version() -> i32 {
    default_value(SCHEMA_VERSION_KEY)
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0)
}

/// 解析版本号：必须是 `[1, i32::MAX]` 内的有限整数。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::schema::parse_integer_version;
///
/// assert_eq!(parse_integer_version(&json!(3)), Some(3));
/// assert_eq!(parse_integer_version(&json!(0)), None);
/// assert_eq!(parse_integer_version(&json!(2.5)), None);
/// ```
pub fn parse_integer_version(value: &Value) -> Option<i32> {
    crate::value::as_integer(value).filter(|version| *version >= 1)
}

/// 在两级文档中按 `"组/名"` 写入值（C++ `insertPath`）。
pub fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let Some((group, name)) = path.split_once('/') else {
        return;
    };
    if name.contains('/') {
        return;
    }
    let slot = root
        .entry(group.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !slot.is_object() {
        *slot = Value::Object(Map::new());
    }
    if let Value::Object(group_map) = slot {
        group_map.insert(name.to_string(), value);
    }
}

/// 在两级文档中按 `"组/名"` 读取值（C++ `valueAtPath`）；`None` 表示不存在。
pub fn value_at_path<'a>(root: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let (group, name) = path.split_once('/')?;
    if name.contains('/') {
        return None;
    }
    root.get(group)?.as_object()?.get(name)
}

/// 由全部默认值构成的完整默认文档。
///
/// # 示例
/// ```
/// let doc = snow_config::schema::complete_default_document();
/// assert_eq!(doc["storage"]["schema_version"], 3);
/// ```
pub fn complete_default_document() -> Value {
    let mut root = Map::new();
    for item in entries() {
        insert_path(&mut root, item.key, item.default.clone());
    }
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    /// 条目数、键唯一且恰含一个 `/`。
    #[test]
    fn entry_count_and_key_shape() {
        assert_eq!(core_entries().len(), 238);
        let keys: HashSet<_> = core_entries().iter().map(|item| item.key).collect();
        assert_eq!(keys.len(), 238);
        assert!(keys.iter().all(|key| key.matches('/').count() == 1));
        let all: HashSet<_> = entries().iter().map(|item| item.key).collect();
        assert_eq!(all.len(), entries().len(), "扩展项不得与核心键重名");
        assert!(all.iter().all(|key| key.matches('/').count() == 1));
    }

    /// 27 个分组名与方案附录 B.2 一致；各组键数取自 C++ 源码（方案文档 B.2 列出的
    /// 各组键数之和超过 238，与 C++ 实际不符，以下为 C++ 实测值，总数 238 吻合）。
    #[test]
    fn groups_match_cpp() {
        let expected = [
            ("api_configuration", 1),
            ("capture_history", 6),
            ("drawing", 16),
            ("drawing_shortcuts", 10),
            ("extended_features", 3),
            ("global_mouse", 7),
            ("global_shortcuts", 21),
            ("interface", 6),
            ("mcp", 1),
            ("network", 1),
            ("pin_to_screen", 9),
            ("pin_to_screen_shortcuts", 15),
            ("pinned_history", 6),
            ("screen_recording", 25),
            ("screen_recording_shortcuts", 4),
            ("screenshot", 27),
            ("screenshot_conversion", 1),
            ("screenshot_selection", 8),
            ("screenshot_shortcuts", 27),
            ("screenshot_toolbar", 8),
            ("screenshot_translation", 5),
            ("screenshot_ui", 13),
            ("storage", 1),
            ("system", 3),
            ("text_recognition", 7),
            ("tray", 6),
            ("updates", 1),
        ];
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for item in core_entries() {
            *counts
                .entry(item.key.split('/').next().unwrap())
                .or_default() += 1;
        }
        assert_eq!(counts.len(), 27);
        for (group, count) in expected {
            assert_eq!(counts.get(group), Some(&count), "分组 {group}");
        }
        assert_eq!(counts.values().sum::<usize>(), 238);
    }

    /// 代表性默认值（方案 B.2 表 + C++ 单测断言）。
    #[test]
    fn representative_defaults() {
        assert_eq!(current_version(), 3);
        assert_eq!(
            default_value("interface/theme_primary_color"),
            json!("#1677FFFF")
        );
        assert_eq!(default_value("capture_history/max_disk_mib"), json!(1024));
        assert_eq!(
            default_value("screenshot_selection/previous_selection"),
            Value::Null
        );
        assert_eq!(default_value("screen_recording/frame_rate"), json!(30));
        assert_eq!(
            default_value("screenshot/manual_save_filename_format"),
            json!(format!("Cisox_{}", "{YYYY-MM-DD_HH-mm-ss}"))
        );
        assert_eq!(
            default_value("global_mouse/screenshot_ocr"),
            json!({"activation_key": ["windows"], "mouse_button": "right_drag"})
        );
        assert_eq!(
            default_value("global_shortcuts/screenshot"),
            json!([{"portable": "F1"}])
        );
        assert_eq!(
            default_value("drawing_shortcuts/brush"),
            json!([{"portable": "3"}, {"portable": "P"}])
        );
        assert_eq!(default_value("nope/nope"), Value::Null);
        assert_eq!(
            entry_for("global_shortcuts/screenshot").unwrap().kind,
            ValueKind::ShortcutList
        );
        assert_eq!(
            entry_for("global_shortcuts/screenshot").unwrap().max_items,
            Some(2)
        );
        assert_eq!(entry_for("tray/menu_options").unwrap().max_items, Some(22));
        assert!(entry_for("screen_recording/animated_image_format").is_none());
    }

    /// 完整默认文档含全部 27 个分组；默认值经规范化为不动点，仅有 C++ 同款的例外：
    /// 目标翻译语言默认 `""` 不在白名单（判非法），三个全局鼠标默认的单元素数组会被规范化成字符串。
    #[test]
    fn default_document_fixed_points() {
        let doc = complete_default_document();
        assert_eq!(doc.as_object().unwrap().len(), 27);
        let mut exceptions = Vec::new();
        for item in entries() {
            let out = crate::normalize::normalize(item.key, &item.default);
            if !out.valid || out.changed {
                exceptions.push(item.key);
            }
        }
        assert_eq!(
            exceptions,
            [
                "screenshot_translation/target_language",
                "global_mouse/screenshot_copy",
                "global_mouse/screenshot_fixed",
                "global_mouse/screenshot_ocr",
            ]
        );
    }

    /// 版本解析、路径读写。
    #[test]
    fn version_and_paths() {
        assert_eq!(parse_integer_version(&json!(3.0)), Some(3));
        assert_eq!(parse_integer_version(&json!(2147483648_i64)), None);
        assert_eq!(parse_integer_version(&json!("3")), None);
        let mut root = Map::new();
        insert_path(&mut root, "a/b", json!(1));
        insert_path(&mut root, "a/c", json!(2));
        insert_path(&mut root, "bad", json!(3));
        assert_eq!(value_at_path(&root, "a/c"), Some(&json!(2)));
        assert_eq!(value_at_path(&root, "a/x"), None);
        assert_eq!(root.len(), 1);
    }

    /// 默认文件名前缀由 `PRODUCT_NAME` 派生，三个键均一致。
    #[test]
    fn default_filename_formats_derive_from_product_name() {
        let shot = format!("{PRODUCT_NAME}_{{YYYY-MM-DD_HH-mm-ss}}");
        let video = format!("{PRODUCT_NAME}_Video_{{YYYY-MM-DD_HH-mm-ss}}");
        assert_eq!(default_screenshot_filename_format(), shot);
        assert_eq!(default_video_filename_format(), video);
        assert_eq!(
            default_value("screenshot/manual_save_filename_format"),
            json!(shot)
        );
        assert_eq!(
            default_value("screenshot/auto_save_filename_format"),
            json!(shot)
        );
        assert_eq!(
            default_value("screen_recording/video_filename_format"),
            json!(video)
        );
    }
}
