//! 键专属规范化与分派：对应 C++ `ConfigurationSchema::normalize(key, value)`。
//!
//! 规范化的语义：输入不合法时 `valid=false`（调用方回退默认值）；合法时给出规范值，
//! `changed` 表示需要回写磁盘。这修正了 V8 spike 里“越界值只返回 Err”的偏差——越界回退默认值
//! 的语义由 [`crate::document`] 统一实现。

use crate::custom_models::normalize_custom_models;
use crate::schema::{SchemaEntry, ValueKind, entry_for};
use crate::selection::{normalize_presets, normalize_selection};
use crate::shortcut::normalize_shortcuts;
use crate::templates::{
    normalize_draw_templates, normalize_manual_save_format_options, normalize_save_path_shortcuts,
    normalize_watermark_templates,
};
use crate::toolbar::{
    ACTION_DEFAULT_POSITIONS, ACTION_TOOLBAR_ITEM_IDS, DRAWING_DEFAULT_POSITIONS,
    DRAWING_TOOLBAR_ITEM_IDS, PINNED_ACTION_TOOLBAR_ITEM_IDS, PINNED_DEFAULT_POSITIONS,
    normalize_toolbar_layout,
};
use crate::value::{Normalization, as_integer, eq_ignore_case, int_value, json_eq, trimmed};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// `#AARRGGBB` 颜色键（C++ `isRgbaColorKey`）。
const RGBA_COLOR_KEYS: [&str; 13] = [
    "interface/theme_primary_color",
    "screenshot_ui/selection_border_color",
    "screenshot_ui/selection_mask_color",
    "screenshot_ui/cursor_guide_line_color",
    "screenshot_ui/monitor_center_guide_line_color",
    "screenshot_ui/color_picker_center_guide_line_color",
    "pin_to_screen/border_color",
    "pin_to_screen/border_active_color",
    "screen_recording/mouse_trail_color",
    "screen_recording/mouse_click_color",
    "screen_recording/mouse_highlight_color",
    "screen_recording/keyboard_background_color",
    "screen_recording/keyboard_foreground_color",
];
/// 文件名格式键（C++ `isFilenameFormatKey`）。
const FILENAME_FORMAT_KEYS: [&str; 3] = [
    "screenshot/manual_save_filename_format",
    "screenshot/auto_save_filename_format",
    "screen_recording/video_filename_format",
];
/// 文件名中不允许出现的字符（Windows 保留字符）。
const INVALID_FILENAME_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
/// 全局鼠标激活键（Windows 分支，C++ `globalMouseActivationKeys`）。
const GLOBAL_MOUSE_ACTIVATION_KEYS: [&str; 4] = ["windows", "ctrl", "alt", "shift"];
/// 全局鼠标按键。
const GLOBAL_MOUSE_BUTTONS: [&str; 5] = [
    "left_drag",
    "right_drag",
    "wheel_drag",
    "side_button_1_drag",
    "side_button_2_drag",
];
/// 录屏帧率允许值（含 83，与 C++ 一致）。
const VIDEO_FRAME_RATES: [i32; 8] = [5, 10, 15, 24, 30, 60, 120, 83];
/// 动图帧率允许值。
const ANIMATED_FRAME_RATES: [i32; 4] = [5, 10, 15, 24];
/// 托盘菜单旧命令名与其新名称。
const LEGACY_TRAY_COMMAND: &str = "tray.disable-shortcut-functions";
/// 托盘菜单旧命令迁移后的名称。
const MIGRATED_TRAY_COMMAND: &str = "quick.toggle-global-hotkeys";
/// 语言键的系统值。
const LANGUAGE_SYSTEM: &str = "system";
/// 语言键 `en` 的展开值。
const LANGUAGE_EN_US: &str = "en_US";
/// 语言子标签数量上限对应的长度范围。
const LANGUAGE_TAG_LEN: std::ops::RangeInclusive<usize> = 2..=3;
/// 语言后续子标签长度范围。
const LANGUAGE_SUBTAG_LEN: std::ops::RangeInclusive<usize> = 2..=8;

/// 仅当值为字符串时返回。
fn string_of(value: &Value) -> Option<&str> {
    value.as_str()
}

/// 主题：去空白、转小写后必须是 system/light/dark。
fn normalize_theme(value: &Value) -> Normalization {
    let Some(original) = string_of(value) else {
        return Normalization::invalid();
    };
    let normalized = trimmed(original).to_lowercase();
    if !["system", "light", "dark"].contains(&normalized.as_str()) {
        return Normalization::invalid();
    }
    let changed = normalized != original;
    Normalization::ok(Value::String(normalized), changed)
}

/// 语言区域默认地区表（QLocale 的默认地区，受限子集）。
fn default_territory(language: &str, script: Option<&str>) -> Option<&'static str> {
    match (language, script) {
        ("zh", Some(script)) if script.eq_ignore_ascii_case("Hant") => Some("TW"),
        ("zh", _) => Some("CN"),
        ("en", _) => Some("US"),
        ("de", _) => Some("DE"),
        ("fr", _) => Some("FR"),
        ("es", _) => Some("ES"),
        ("it", _) => Some("IT"),
        ("ja", _) => Some("JP"),
        ("ko", _) => Some("KR"),
        ("pt", _) => Some("BR"),
        ("ru", _) => Some("RU"),
        ("tr", _) => Some("TR"),
        ("ar", _) => Some("EG"),
        _ => None,
    }
}

/// 语言区域规范化为 `语言[_地区]`（QLocale::name() 的受限子集）：语言小写；显式地区大写；
/// 缺省地区按 [`default_territory`] 补全；表外语言保持原样（C++ 由 QLocale 判定，Rust 无法判定）。
fn canonical_locale(text: &str) -> String {
    let mut parts = text.split('_');
    let language = parts.next().unwrap_or_default().to_lowercase();
    let mut script: Option<String> = None;
    let mut territory: Option<String> = None;
    for part in parts {
        if part.len() == 4 && part.chars().all(|c| c.is_ascii_alphabetic()) && script.is_none() {
            script = Some(part.to_string());
        } else if territory.is_none()
            && ((part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
                || (part.len() == 3 && part.chars().all(|c| c.is_ascii_digit())))
        {
            territory = Some(part.to_ascii_uppercase());
        }
    }
    let territory =
        territory.or_else(|| default_territory(&language, script.as_deref()).map(str::to_string));
    match territory {
        Some(territory) => format!("{language}_{territory}"),
        None => language,
    }
}

/// 语言键：`system`、`en`→`en_US`，或形如 `ll[_XX...]` 的区域名（受限子集，见 [`canonical_locale`]）。
fn normalize_language(value: &Value) -> Normalization {
    let Some(original) = string_of(value) else {
        return Normalization::invalid();
    };
    let mut normalized = trimmed(original).to_string();
    if eq_ignore_case(&normalized, LANGUAGE_SYSTEM) {
        normalized = LANGUAGE_SYSTEM.to_string();
    } else {
        normalized = normalized.replace('-', "_");
        if eq_ignore_case(&normalized, "en") {
            normalized = LANGUAGE_EN_US.to_string();
        } else {
            let mut segments = normalized.split('_');
            let head = segments.next().unwrap_or_default();
            let head_ok = LANGUAGE_TAG_LEN.contains(&head.len())
                && head.chars().all(|c| c.is_ascii_alphabetic());
            let rest_ok = segments.all(|segment| {
                LANGUAGE_SUBTAG_LEN.contains(&segment.len())
                    && segment.chars().all(|c| c.is_ascii_alphanumeric())
            });
            if !head_ok || !rest_ok {
                return Normalization::invalid();
            }
            normalized = canonical_locale(&normalized);
        }
    }
    let changed = normalized != original;
    Normalization::ok(Value::String(normalized), changed)
}

/// 带白名单（大小写不敏感匹配、输出规范拼写）的字符串列表，去重并受 `max_items` 限制。
fn normalize_allowed_string_list(entry: &SchemaEntry, value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut normalized: Vec<Value> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut changed = false;
    for item in items {
        let Some(original) = item.as_str() else {
            changed = true;
            continue;
        };
        let candidate = trimmed(original);
        let canonical = entry
            .allowed
            .iter()
            .find(|allowed| eq_ignore_case(allowed, candidate));
        let over_limit = entry.max_items.is_some_and(|max| normalized.len() >= max);
        let Some(canonical) = canonical.filter(|name| !seen.contains(**name) && !over_limit) else {
            changed = true;
            continue;
        };
        seen.insert(canonical);
        normalized.push(Value::String((*canonical).to_string()));
        changed = changed || *canonical != original;
    }
    Normalization::ok(Value::Array(normalized), changed)
}

/// 托盘菜单：先把旧命令名迁移为新名称，再按白名单列表规范化；迁移本身算一次变更。
fn normalize_tray_menu_options(entry: &SchemaEntry, value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut renamed = false;
    let migrated: Vec<Value> = items
        .iter()
        .map(|item| {
            if item
                .as_str()
                .is_some_and(|text| trimmed(text) == LEGACY_TRAY_COMMAND)
            {
                renamed = true;
                Value::String(MIGRATED_TRAY_COMMAND.to_string())
            } else {
                item.clone()
            }
        })
        .collect();
    let mut normalized = normalize_allowed_string_list(entry, &Value::Array(migrated));
    normalized.changed = normalized.changed || renamed;
    normalized
}

/// 值必须是给定集合中的整数。
fn normalize_allowed_integer(value: &Value, allowed: &[i32]) -> Normalization {
    match as_integer(value) {
        Some(number) if allowed.contains(&number) => {
            Normalization::ok(int_value(i64::from(number)), false)
        }
        _ => Normalization::invalid(),
    }
}

/// `#AARRGGBB`：去空白转大写后必须恰为 `#` + 8 位十六进制。
fn normalize_rgba_color(value: &Value) -> Normalization {
    let Some(original) = string_of(value) else {
        return Normalization::invalid();
    };
    let normalized = trimmed(original).to_uppercase();
    let is_valid = normalized.len() == 9
        && normalized.starts_with('#')
        && normalized[1..]
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'A'..='F'));
    if !is_valid {
        return Normalization::invalid();
    }
    let changed = normalized != original;
    Normalization::ok(Value::String(normalized), changed)
}

/// 文件名格式：去空白后非空，且不含 `\ / : * ? " < > |`。
fn normalize_filename_format(value: &Value) -> Normalization {
    let Some(original) = string_of(value) else {
        return Normalization::invalid();
    };
    let normalized = trimmed(original);
    if normalized.is_empty() || normalized.contains(INVALID_FILENAME_CHARS) {
        return Normalization::invalid();
    }
    let changed = normalized != original;
    Normalization::ok(Value::String(normalized.to_string()), changed)
}

/// 翻译语言：去空白后大小写不敏感命中白名单，输出规范拼写。
fn normalize_translation_language(entry: &SchemaEntry, value: &Value) -> Normalization {
    let Some(original) = string_of(value) else {
        return Normalization::invalid();
    };
    let trimmed_value = trimmed(original);
    match entry
        .allowed
        .iter()
        .find(|allowed| eq_ignore_case(allowed, trimmed_value))
    {
        Some(canonical) => Normalization::ok(
            Value::String((*canonical).to_string()),
            *canonical != original,
        ),
        None => Normalization::invalid(),
    }
}

/// 全局鼠标组合：空对象表示未设置；否则须恰有 `activation_key`（字符串或字符串数组）与
/// `mouse_button` 两个键，激活键去重排序，单个时输出为字符串。
fn normalize_global_mouse_combination(value: &Value) -> Normalization {
    let Some(object) = value.as_object() else {
        return Normalization::invalid();
    };
    if object.is_empty() {
        return Normalization::ok(Value::Object(Map::new()), false);
    }
    let activation = object.get("activation_key");
    let button = object.get("mouse_button").and_then(Value::as_str);
    let activation_valid = activation.is_some_and(|v| v.is_string() || v.is_array());
    let (true, Some(button)) = (object.len() == 2 && activation_valid, button) else {
        return Normalization::invalid();
    };
    let mut keys: Vec<String> = Vec::new();
    match activation {
        Some(Value::String(text)) => keys.push(trimmed(text).to_string()),
        Some(Value::Array(items)) => {
            for item in items {
                let Some(text) = item.as_str() else {
                    return Normalization::invalid();
                };
                keys.push(trimmed(text).to_string());
            }
        }
        _ => return Normalization::invalid(),
    }
    let button = trimmed(button);
    if keys.is_empty()
        || !GLOBAL_MOUSE_BUTTONS.contains(&button)
        || !keys
            .iter()
            .all(|key| GLOBAL_MOUSE_ACTIVATION_KEYS.contains(&key.as_str()))
    {
        return Normalization::invalid();
    }
    keys.sort();
    keys.dedup();
    let activation_value = if keys.len() == 1 {
        Value::String(keys.remove(0))
    } else {
        Value::Array(keys.into_iter().map(Value::String).collect())
    };
    let mut normalized = Map::new();
    normalized.insert("activation_key".into(), activation_value);
    normalized.insert("mouse_button".into(), Value::String(button.to_string()));
    let normalized = Value::Object(normalized);
    let changed = !json_eq(&normalized, value);
    Normalization::ok(normalized, changed)
}

/// 整数范围：必须是 i32 内整数且落在 `[min, max]`。
fn normalize_integer_range(value: &Value, min: i32, max: i32) -> Normalization {
    match as_integer(value) {
        Some(number) if (min..=max).contains(&number) => {
            Normalization::ok(int_value(i64::from(number)), false)
        }
        _ => Normalization::invalid(),
    }
}

/// 类型精确匹配（布尔/对象）：类型对即合法，值不变。
fn exact_type(value: &Value, matches_type: fn(&Value) -> bool) -> Normalization {
    if matches_type(value) {
        Normalization::ok(value.clone(), false)
    } else {
        Normalization::invalid()
    }
}

/// 对指定键的值做规范化，等价于 C++ `ConfigurationSchema::normalize`。
///
/// # 参数
/// - `key`：`"组/名"` 键
/// - `value`：待规范化的值
///
/// # 返回
/// [`Normalization`]：未知键、类型/范围/白名单不符时 `valid=false`。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::normalize::normalize;
///
/// let out = normalize("screenshot_ui/cursor_guide_line_color", &json!("#abcdef80"));
/// assert!(out.valid && out.changed);
/// assert_eq!(out.value, json!("#ABCDEF80"));
/// assert!(!normalize("screen_recording/frame_rate", &json!(25)).valid);
/// ```
pub fn normalize(key: &str, value: &Value) -> Normalization {
    let Some(entry) = entry_for(key) else {
        return Normalization::invalid();
    };
    match key {
        "api_configuration/custom_models" => return normalize_custom_models(value),
        "interface/theme_mode" => return normalize_theme(value),
        "interface/language" => return normalize_language(value),
        "screenshot_selection/previous_selection" => return normalize_selection(value),
        "screenshot_selection/selection_rect_presets" => return normalize_presets(value),
        "drawing/watermark_templates" => return normalize_watermark_templates(value),
        "drawing/draw_templates" => return normalize_draw_templates(value),
        "screenshot/save_path_shortcuts" => return normalize_save_path_shortcuts(value),
        "screenshot/manual_save_format_options" => {
            return normalize_manual_save_format_options(value);
        }
        "screenshot_toolbar/layout" => {
            return normalize_toolbar_layout(
                value,
                DRAWING_TOOLBAR_ITEM_IDS,
                DRAWING_DEFAULT_POSITIONS,
                false,
            );
        }
        "pin_to_screen/action_tools_layout" => {
            return normalize_toolbar_layout(
                value,
                PINNED_ACTION_TOOLBAR_ITEM_IDS,
                PINNED_DEFAULT_POSITIONS,
                false,
            );
        }
        "screenshot_toolbar/action_tools_layout" => {
            return normalize_toolbar_layout(
                value,
                ACTION_TOOLBAR_ITEM_IDS,
                ACTION_DEFAULT_POSITIONS,
                true,
            );
        }
        _ => {}
    }
    if key.starts_with("global_mouse/") {
        return normalize_global_mouse_combination(value);
    }
    if RGBA_COLOR_KEYS.contains(&key) {
        return normalize_rgba_color(value);
    }
    if FILENAME_FORMAT_KEYS.contains(&key) {
        return normalize_filename_format(value);
    }
    match key {
        "screenshot_translation/source_language" | "screenshot_translation/target_language" => {
            return normalize_translation_language(entry, value);
        }
        "drawing/quick_selection_disabled_tools" => {
            return normalize_allowed_string_list(entry, value);
        }
        "tray/menu_options" => return normalize_tray_menu_options(entry, value),
        "screen_recording/frame_rate" => {
            return normalize_allowed_integer(value, &VIDEO_FRAME_RATES);
        }
        "screen_recording/animated_image_frame_rate" => {
            return normalize_allowed_integer(value, &ANIMATED_FRAME_RATES);
        }
        _ => {}
    }
    match entry.kind {
        ValueKind::Boolean => exact_type(value, Value::is_boolean),
        ValueKind::Integer => match entry.range {
            Some(range) => normalize_integer_range(value, range.min, range.max),
            None => match as_integer(value) {
                Some(_) => Normalization::ok(value.clone(), false),
                None => Normalization::invalid(),
            },
        },
        ValueKind::String => {
            let Some(original) = string_of(value) else {
                return Normalization::invalid();
            };
            let normalized = trimmed(original);
            if !entry.allowed.is_empty() && !entry.allowed.contains(&normalized) {
                return Normalization::invalid();
            }
            Normalization::ok(
                Value::String(normalized.to_string()),
                normalized != original,
            )
        }
        ValueKind::StringList => normalize_allowed_string_list(entry, value),
        ValueKind::ShortcutList => normalize_shortcuts(
            value,
            entry.max_items,
            key.starts_with("screenshot_shortcuts/"),
        ),
        ValueKind::Structured => exact_type(value, Value::is_object),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 对合法输入断言规范值与 changed 标志。
    fn check(key: &str, input: Value, expected: Value, changed: bool) {
        let out = normalize(key, &input);
        assert!(out.valid, "{key} 应合法：{input}");
        assert_eq!(out.value, expected, "{key}");
        assert_eq!(out.changed, changed, "{key} 的 changed");
    }

    /// 断言非法。
    fn check_invalid(key: &str, input: Value) {
        assert!(!normalize(key, &input).valid, "{key} 应非法：{input}");
    }

    /// 颜色：大小写规范化、缺 alpha 非法（C++ screenshotUiSchemaRepairsStructuredValues）。
    #[test]
    fn rgba_color_rules() {
        let key = "screenshot_ui/cursor_guide_line_color";
        check(key, json!("#abcdef80"), json!("#ABCDEF80"), true);
        check(key, json!(" #ABCDEF80 "), json!("#ABCDEF80"), true);
        check(key, json!("#ABCDEF80"), json!("#ABCDEF80"), false);
        for bad in [
            json!("#ABCDEF"),
            json!("ABCDEF80"),
            json!("#GGGGGGGG"),
            json!(1),
            json!("#ABCDEF800"),
        ] {
            check_invalid(key, bad);
        }
        for color_key in RGBA_COLOR_KEYS {
            assert!(crate::schema::contains(color_key), "{color_key}");
        }
    }

    /// 文件名格式：去空白、拒绝空与非法字符。
    #[test]
    fn filename_format_rules() {
        let key = "screenshot/manual_save_filename_format";
        check(key, json!("  Shot_{YYYY}  "), json!("Shot_{YYYY}"), true);
        for bad in [
            "", "   ", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b",
        ] {
            check_invalid(key, json!(bad));
        }
        check_invalid(key, json!(5));
        check_invalid("screen_recording/video_filename_format", json!("x|y"));
    }

    /// 帧率白名单（C++ 单测：宣告的都接受，未宣告的拒绝，非整数拒绝）。
    #[test]
    fn frame_rate_allow_lists() {
        for rate in [5, 10, 15, 24, 30, 60, 120, 83] {
            check(
                "screen_recording/frame_rate",
                json!(rate),
                json!(rate),
                false,
            );
        }
        for rate in [0, 25, 29, 84, 121] {
            check_invalid("screen_recording/frame_rate", json!(rate));
        }
        for rate in [5, 10, 15, 24] {
            check(
                "screen_recording/animated_image_frame_rate",
                json!(rate),
                json!(rate),
                false,
            );
        }
        check_invalid("screen_recording/animated_image_frame_rate", json!(30));
        check_invalid("screen_recording/frame_rate", json!(30.5));
        check("screen_recording/frame_rate", json!(30.0), json!(30), false);
    }

    /// 图片质量边界（0/1/99/100 合法，-1/101 越界回退）。
    #[test]
    fn integer_range_bounds() {
        for quality in [0, 1, 99, 100] {
            check(
                "screenshot/image_quality",
                json!(quality),
                json!(quality),
                false,
            );
        }
        check_invalid("screenshot/image_quality", json!(-1));
        check_invalid("screenshot/image_quality", json!(101));
        check_invalid("screenshot/image_quality", json!("50"));
        check_invalid("storage/schema_version", json!(4));
        check(
            "capture_history/max_disk_mib",
            json!(128),
            json!(128),
            false,
        );
        check_invalid("capture_history/max_disk_mib", json!(127));
        check_invalid("capture_history/retention_days", json!(366));
        check_invalid("capture_history/max_entries", json!(0));
    }

    /// 白名单字符串：全部通告值合法，未知值非法；String 会去空白。
    #[test]
    fn allowed_string_values() {
        for item in crate::schema::entries() {
            if item.kind != ValueKind::String || item.allowed.is_empty() {
                continue;
            }
            if item.key.starts_with("screenshot_translation/") || item.key == "interface/theme_mode"
            {
                continue;
            }
            for allowed in item.allowed {
                check(item.key, json!(*allowed), json!(*allowed), false);
            }
            check_invalid(item.key, json!("unsupported-value"));
        }
        check(
            "tray/left_click_action",
            json!("  screenshot  "),
            json!("screenshot"),
            true,
        );
        check_invalid("network/proxy", json!(5));
    }

    /// 主题：忽略大小写与空白。
    #[test]
    fn theme_mode() {
        check("interface/theme_mode", json!(" DARK "), json!("dark"), true);
        check(
            "interface/theme_mode",
            json!("system"),
            json!("system"),
            false,
        );
        check_invalid("interface/theme_mode", json!("blue"));
    }

    /// 语言：system/en 特殊处理，`-` 转 `_`，格式校验。
    #[test]
    fn language_rules() {
        let key = "interface/language";
        check(key, json!("SYSTEM"), json!("system"), true);
        check(key, json!("en"), json!("en_US"), true);
        check(key, json!("EN"), json!("en_US"), true);
        check(key, json!("zh-cn"), json!("zh_CN"), true);
        check(key, json!("zh_TW"), json!("zh_TW"), false);
        check(key, json!("zh-Hant"), json!("zh_TW"), true);
        check(key, json!("zh"), json!("zh_CN"), true);
        check(key, json!("en_US"), json!("en_US"), false);
        for bad in ["", "e", "english", "zh__CN", "zh_C", "zh_CN_!", "1234"] {
            check_invalid(key, json!(bad));
        }
        check_invalid(key, json!(1));
    }

    /// 翻译语言：忽略大小写命中白名单并输出规范拼写；目标语言不允许 auto。
    #[test]
    fn translation_languages() {
        let source = "screenshot_translation/source_language";
        let target = "screenshot_translation/target_language";
        check(source, json!(" AUTO "), json!("auto"), true);
        check(source, json!("zh-hans"), json!("zh-Hans"), true);
        check(target, json!("ZH-HANT"), json!("zh-Hant"), true);
        check(target, json!("ja"), json!("ja"), false);
        check_invalid(target, json!("auto"));
        check_invalid(target, json!(""));
        check_invalid(source, json!("xx"));
    }

    /// 抽屉工具白名单列表（C++：` FREE-DRAW `、重复、未知、非字符串）。
    #[test]
    fn drawing_tool_list() {
        let key = "drawing/quick_selection_disabled_tools";
        check(
            key,
            json!([" FREE-DRAW ", "pen-filter", "PEN-FILTER", "unknown", 42]),
            json!(["free-draw", "pen-filter"]),
            true,
        );
        check_invalid(key, json!("free-draw"));
    }

    /// 托盘菜单：旧命令原位改名且不重复；默认值稳定；restart-app 显式选择才接受。
    #[test]
    fn tray_menu_options() {
        let key = "tray/menu_options";
        check(
            key,
            json!([
                "quick.screenshot",
                "tray.disable-shortcut-functions",
                "quick.toggle-global-hotkeys",
                "tray.exit"
            ]),
            json!([
                "quick.screenshot",
                "quick.toggle-global-hotkeys",
                "tray.exit"
            ]),
            true,
        );
        check(
            key,
            json!([
                "quick.screenshot",
                "tray.window-grouping",
                "tray.disable-shortcut-functions",
                "tray.exit"
            ]),
            json!([
                "quick.screenshot",
                "tray.window-grouping",
                "quick.toggle-global-hotkeys",
                "tray.exit"
            ]),
            true,
        );
        let defaults = crate::schema::default_value(key);
        check(key, defaults.clone(), defaults, false);
        check(
            key,
            json!(["tray.show-main-window", "tray.restart-app", "tray.exit"]),
            json!(["tray.show-main-window", "tray.restart-app", "tray.exit"]),
            false,
        );
        check_invalid(key, json!("quick.screenshot"));
    }

    /// 全局鼠标组合（C++ globalMouseCombinationSchemaIsStrictAndPersistent）。
    #[test]
    fn global_mouse_rules() {
        let key = "global_mouse/screenshot_copy";
        check(key, json!({}), json!({}), false);
        for activation in ["windows", "ctrl", "alt", "shift"] {
            for button in GLOBAL_MOUSE_BUTTONS {
                let combo = json!({"activation_key": activation, "mouse_button": button});
                check(key, combo.clone(), combo, false);
            }
        }
        check(
            key,
            json!({"activation_key": ["shift", "ctrl", "ctrl"], "mouse_button": "left_drag"}),
            json!({"activation_key": ["ctrl", "shift"], "mouse_button": "left_drag"}),
            true,
        );
        check(
            key,
            json!({"activation_key": ["windows"], "mouse_button": "left_drag"}),
            json!({"activation_key": "windows", "mouse_button": "left_drag"}),
            true,
        );
        for bad in [
            json!("windows+left_drag"),
            json!(["windows", "left_drag"]),
            json!({"activation_key": "windows"}),
            json!({"mouse_button": "left_drag"}),
            json!({"activation_key": "meta", "mouse_button": "left_drag"}),
            json!({"activation_key": "windows", "mouse_button": "middle_drag"}),
            json!({"activation_key": "windows", "mouse_button": "left_drag", "extra": true}),
            json!({"activation_key": 1, "mouse_button": "left_drag"}),
            json!({"activation_key": [], "mouse_button": "left_drag"}),
            json!({"activation_key": ["ctrl", 1], "mouse_button": "left_drag"}),
            json!({"activation_key": ["ctrl", "bad"], "mouse_button": "left_drag"}),
        ] {
            check_invalid(key, bad);
        }
    }

    /// 快捷键键分派：仅 screenshot_shortcuts 允许单独 Shift；上限 2。
    #[test]
    fn shortcut_dispatch() {
        check(
            "screenshot_shortcuts/copy_color",
            json!(["Shift"]),
            json!([{"portable": "Shift"}]),
            true,
        );
        check("drawing_shortcuts/shape", json!(["Shift"]), json!([]), true);
        check(
            "drawing_shortcuts/shape",
            json!([{"portable": "1"}, {"portable": "2"}, {"portable": "3"}]),
            json!([{"portable": "1"}, {"portable": "2"}]),
            true,
        );
    }

    /// 布尔/结构化/无范围整数与未知键。
    #[test]
    fn generic_kinds_and_unknown_keys() {
        check("system/auto_start_at_boot", json!(true), json!(true), false);
        check_invalid("system/auto_start_at_boot", json!(1));
        check(
            "drawing/shape_style",
            json!({"a": 1}),
            json!({"a": 1}),
            false,
        );
        check_invalid("drawing/shape_style", json!([]));
        check_invalid("no/such_key", json!(1));
        check_invalid("screenshot_toolbar/last_drawing_tool", json!("undo"));
        check(
            "screenshot_toolbar/last_drawing_tool",
            json!(""),
            json!(""),
            false,
        );
        check_invalid("screenshot_translation/target_language", json!(null));
    }
}
