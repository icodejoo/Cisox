//! 模板与列表类键的规范化：水印模板、绘图模板、保存路径快捷项、手动保存格式选项。
//!
//! 移植自 C++ `configurationschema.cpp`。C++ 用 `QByteArray::fromBase64` 后再 `toBase64` 比对，
//! 这里用严格的标准 base64 解码等价实现。初稿由 antigravity 产出，复审后修正了
//! “填充仅允许出现在末组”的缺陷。

use crate::value::{
    Normalization, as_integer, case_folded, int_value, json_eq, to_int_or, trimmed,
};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// 绘图模板载荷解码后的字节上限（16 MiB）。
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// 图片质量上限。
const QUALITY_MAX: i32 = 100;

/// 严格标准 base64 解码（带 `=` 填充、规范填充位）；任何偏差返回 `None`。
fn decode_base64_strict(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let chunk_count = bytes.len() / 4;
    let mut out = Vec::with_capacity(chunk_count * 3);
    for (chunk_index, chunk) in bytes.chunks_exact(4).enumerate() {
        let mut values = [0u8; 4];
        let mut padding = 0;
        for (slot, byte) in chunk.iter().enumerate() {
            values[slot] = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    padding += 1;
                    0
                }
                _ => return None,
            };
            // 填充只能连续出现在末组末尾
            if padding > 0 && *byte != b'=' {
                return None;
            }
        }
        if padding > 0 && chunk_index + 1 != chunk_count {
            return None;
        }
        out.push((values[0] << 2) | (values[1] >> 4));
        match padding {
            0 => {
                out.push((values[1] << 4) | (values[2] >> 2));
                out.push((values[2] << 6) | values[3]);
            }
            1 => {
                if values[2] & 0b11 != 0 {
                    return None;
                }
                out.push((values[1] << 4) | (values[2] >> 2));
            }
            2 => {
                if values[1] & 0b1111 != 0 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (out.len() <= MAX_PAYLOAD_BYTES).then_some(out)
}

/// 读取字符串字段，缺失或类型不符返回空串（等价 `QJsonValue::toString()`）。
fn string_or_empty<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// 构造 `{name, <key>: text}` 对象。
fn pair_object(name: &str, key: &str, text: &str) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), Value::String(name.to_string()));
    map.insert(key.into(), Value::String(text.to_string()));
    Value::Object(map)
}

/// 规范化 `drawing/watermark_templates`：需要字符串 `name`（去空白后非空）与 `value`（去空白后非空）。
/// 保留重复项与 `value` 原文。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::templates::normalize_watermark_templates;
///
/// assert!(normalize_watermark_templates(&json!([])).valid);
/// ```
pub fn normalize_watermark_templates(value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut result = Vec::new();
    let mut changed = false;
    for item in items {
        let Some(object) = item.as_object() else {
            changed = true;
            continue;
        };
        let (Some(name), Some(template)) = (
            object.get("name").and_then(Value::as_str),
            object.get("value").and_then(Value::as_str),
        ) else {
            changed = true;
            continue;
        };
        let name = trimmed(name);
        if name.is_empty() || trimmed(template).is_empty() {
            changed = true;
            continue;
        }
        let normalized = pair_object(name, "value", template);
        changed = changed || !json_eq(&normalized, item);
        result.push(normalized);
    }
    Normalization::ok(Value::Array(result), changed)
}

/// 判断绘图模板载荷（base64 的 JSON）是否合法。
fn draw_payload_is_valid(encoded: &str) -> bool {
    let Some(bytes) = decode_base64_strict(encoded) else {
        return false;
    };
    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let non_empty_array = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    };
    payload
        .get("schemaVersion")
        .is_some_and(|v| to_int_or(v, -1) == 1)
        && non_empty_array("elements")
        && non_empty_array("selectedIds")
}

/// 规范化 `drawing/draw_templates`：需要非空 `name` 与合法 base64 JSON `payload`
/// （`schemaVersion == 1`，`elements`/`selectedIds` 非空）。保留重复名称。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::templates::normalize_draw_templates;
///
/// assert!(normalize_draw_templates(&json!([])).valid);
/// ```
pub fn normalize_draw_templates(value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut result = Vec::new();
    let mut changed = false;
    for item in items {
        let Some(object) = item.as_object() else {
            changed = true;
            continue;
        };
        let name = trimmed(string_or_empty(object, "name"));
        let encoded = string_or_empty(object, "payload");
        if name.is_empty() || !draw_payload_is_valid(encoded) {
            changed = true;
            continue;
        }
        let normalized = pair_object(name, "payload", encoded);
        changed = changed || !json_eq(&normalized, item);
        result.push(normalized);
    }
    Normalization::ok(Value::Array(result), changed)
}

/// 规范化 `screenshot/save_path_shortcuts`：名称与路径去空白后均非空，名称按大小写折叠去重。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::templates::normalize_save_path_shortcuts;
///
/// assert!(normalize_save_path_shortcuts(&json!([])).valid);
/// ```
pub fn normalize_save_path_shortcuts(value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut result = Vec::new();
    let mut names: HashSet<String> = HashSet::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let name = trimmed(string_or_empty(object, "name"));
        let path = trimmed(string_or_empty(object, "path"));
        if name.is_empty() || path.is_empty() || !names.insert(case_folded(name)) {
            continue;
        }
        result.push(pair_object(name, "path", path));
    }
    let normalized = Value::Array(result);
    let changed = !json_eq(&normalized, value);
    Normalization::ok(normalized, changed)
}

/// 规范化 `screenshot/manual_save_format_options`：仅保留已知格式的合法 `quality`（截断到 0..=100）
/// 与 `compression_level`。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::templates::normalize_manual_save_format_options;
///
/// assert!(normalize_manual_save_format_options(&json!({})).valid);
/// ```
pub fn normalize_manual_save_format_options(value: &Value) -> Normalization {
    const QUALITY_FORMATS: [&str; 5] = ["jpeg", "webp", "jxl", "avif", "pdf"];
    const COMPRESSION_FORMATS: [&str; 4] = ["png", "webp", "jxl", "avif"];
    const COMPRESSION_VALUES: [&str; 3] = ["low", "medium", "high"];
    let Some(source) = value.as_object() else {
        return Normalization::invalid();
    };
    let mut result = Map::new();
    for (format, entry) in source {
        let has_quality = QUALITY_FORMATS.contains(&format.as_str());
        let has_compression = COMPRESSION_FORMATS.contains(&format.as_str());
        let Some(options) = entry.as_object().filter(|_| has_quality || has_compression) else {
            continue;
        };
        let mut normalized = Map::new();
        if has_quality && let Some(quality) = options.get("quality").and_then(as_integer) {
            normalized.insert(
                "quality".into(),
                int_value(i64::from(quality.clamp(0, QUALITY_MAX))),
            );
        }
        let compression = trimmed(string_or_empty(options, "compression_level"));
        if has_compression && COMPRESSION_VALUES.contains(&compression) {
            normalized.insert(
                "compression_level".into(),
                Value::String(compression.to_string()),
            );
        }
        if !normalized.is_empty() {
            result.insert(format.clone(), Value::Object(normalized));
        }
    }
    let normalized = Value::Object(result);
    let changed = !json_eq(&normalized, value);
    Normalization::ok(normalized, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `{"schemaVersion":1,"selectedIds":[1],"elements":[1]}` 的 base64（标准字母表，带填充）。
    const PAYLOAD_B64: &str =
        "eyJzY2hlbWFWZXJzaW9uIjoxLCJzZWxlY3RlZElkcyI6WzFdLCJlbGVtZW50cyI6WzFdfQ==";

    /// base64 解码严格性：往返一致才接受。
    #[test]
    fn base64_is_strict() {
        assert_eq!(decode_base64_strict("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64_strict("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64_strict("TQ==").unwrap(), b"M");
        for bad in [
            "", "TWF", "TWE", "TWF=u", "TQ=A", "TR==", "TWF!", "TQ==TQ==", "TW F",
        ] {
            assert!(decode_base64_strict(bad).is_none(), "{bad:?}");
        }
        assert!(decode_base64_strict(PAYLOAD_B64).is_some());
    }

    /// 水印模板：保留有效顺序、重复项与 value 原文（C++ watermarkTemplateSettingsRepair...）。
    #[test]
    fn watermark_repair_matches_cpp() {
        let input = json!([
            {"name": "  Release  ", "value": "  {text} {YYYY}  ", "extra": true},
            7,
            {"name": "   ", "value": "x"},
            {"name": "Bad", "value": 42},
            {"name": "Whitespace", "value": "   "},
            {"name": "Release", "value": "  {text} {YYYY}  "},
            {"name": "Release", "value": "{DD}"}
        ]);
        let out = normalize_watermark_templates(&input);
        assert!(out.valid && out.changed);
        assert_eq!(
            out.value,
            json!([
                {"name": "Release", "value": "  {text} {YYYY}  "},
                {"name": "Release", "value": "  {text} {YYYY}  "},
                {"name": "Release", "value": "{DD}"}
            ])
        );
        assert!(!normalize_watermark_templates(&json!({})).valid);
    }

    /// 绘图模板：丢弃非法载荷/空名称与多余字段，保留重名（C++ drawTemplateSettingsRepair...）。
    #[test]
    fn draw_templates_repair_matches_cpp() {
        let input = json!([
            {"name": "  Mark  ", "payload": PAYLOAD_B64, "extra": 1},
            {"name": "Mark", "payload": PAYLOAD_B64},
            {"name": "Bad", "payload": "!invalid!"},
            {"name": "  ", "payload": PAYLOAD_B64}
        ]);
        let out = normalize_draw_templates(&input);
        assert!(out.valid && out.changed);
        assert_eq!(
            out.value,
            json!([
                {"name": "Mark", "payload": PAYLOAD_B64},
                {"name": "Mark", "payload": PAYLOAD_B64}
            ])
        );
        let stable = normalize_draw_templates(&out.value);
        assert!(stable.valid && !stable.changed);
    }

    /// 载荷内容规则：schemaVersion 必须为 1，elements/selectedIds 必须非空，顶层必须是对象。
    #[test]
    fn draw_payload_rules() {
        let b64 = |json_text: &str| {
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in json_text.as_bytes().chunks(3) {
                let n = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
                for i in 0..4 {
                    if i <= chunk.len() {
                        out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
                    } else {
                        out.push('=');
                    }
                }
            }
            out
        };
        assert!(draw_payload_is_valid(&b64(
            r#"{"schemaVersion":1,"selectedIds":[1],"elements":[1]}"#
        )));
        assert!(draw_payload_is_valid(&b64(
            r#"{"schemaVersion":1.0,"selectedIds":[1],"elements":[1]}"#
        )));
        assert!(!draw_payload_is_valid(&b64(
            r#"{"schemaVersion":2,"selectedIds":[1],"elements":[1]}"#
        )));
        assert!(!draw_payload_is_valid(&b64(
            r#"{"schemaVersion":1,"selectedIds":[],"elements":[1]}"#
        )));
        assert!(!draw_payload_is_valid(&b64(
            r#"{"schemaVersion":1,"selectedIds":[1]}"#
        )));
        assert!(!draw_payload_is_valid(&b64(r#"[1]"#)));
        assert!(!draw_payload_is_valid(&b64("not json")));
    }

    /// 保存路径快捷项：空项与大小写折叠后的重名被丢弃。
    #[test]
    fn save_path_shortcuts_dedupe() {
        let input = json!([
            {"name": " Docs ", "path": " C:/Docs "},
            {"name": "docs", "path": "D:/Other"},
            {"name": "", "path": "E:/x"},
            {"name": "NoPath", "path": "  "},
            5
        ]);
        let out = normalize_save_path_shortcuts(&input);
        assert!(out.valid && out.changed);
        assert_eq!(out.value, json!([{"name": "Docs", "path": "C:/Docs"}]));
        assert!(!normalize_save_path_shortcuts(&json!({})).valid);
    }

    /// 手动保存格式选项：截断质量、剔除畸形字段（C++ settingsSchemaDefaultsAndValidation...）。
    #[test]
    fn manual_save_format_options_match_cpp() {
        let input = json!({
            "png": {"quality": 41, "compression_level": "medium", "unknown": true},
            "jpeg": {"quality": -4, "compression_level": "high"},
            "webp": {"quality": 140, "compression_level": "invalid"},
            "jxl": "malformed",
            "avif": {"quality": 55.5, "compression_level": "high"},
            "pdf": {"quality": 0},
            "unsupported": {"quality": 75}
        });
        let out = normalize_manual_save_format_options(&input);
        assert!(out.valid && out.changed);
        assert_eq!(
            out.value,
            json!({
                "png": {"compression_level": "medium"},
                "jpeg": {"quality": 0},
                "webp": {"quality": 100},
                "avif": {"compression_level": "high"},
                "pdf": {"quality": 0}
            })
        );
        assert!(!normalize_manual_save_format_options(&json!([])).valid);
    }
}
