//! 自定义 AI 模型配置（`api_configuration/custom_models`）的规范化。
//!
//! 移植自 C++ `customaimodelconfiguration.h`。C++ 用 `QUuid`/`QUrl(StrictMode)` 校验，
//! 这里用受限子集复刻（见各函数说明）。初稿由 antigravity 产出，复审后修正了 IPv6 端口解析
//! 等问题。

use crate::value::{Normalization, case_folded, json_eq, trimmed};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// UUID 文本长度（8-4-4-4-12 含连字符）。
const UUID_TEXT_LEN: usize = 36;
/// UUID 中连字符所在下标。
const UUID_DASH_POSITIONS: [usize; 4] = [8, 13, 18, 23];
/// 合法端口上限。
const PORT_MAX: u32 = 65535;
/// 完整聊天端点后缀（出现即视为“填了完整端点”）。
const FULL_ENDPOINT_SUFFIX: &str = "/chat/completions";

/// 一条自定义 AI 模型配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAiModel {
    /// 模型记录 ID（小写 UUID）。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 服务基础地址（不含 `/chat/completions`）。
    pub base_url: String,
    /// API 密钥，不允许含换行。
    pub api_key: String,
    /// 服务端模型名。
    pub model: String,
    /// 是否支持视觉输入。
    pub supports_vision: bool,
    /// 是否支持推理。
    pub supports_reasoning: bool,
}

/// 基础地址校验结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomAiModelUrlError {
    /// 合法。
    None,
    /// 不是合法的 http/https 基础地址。
    InvalidBaseUrl,
    /// 填成了完整端点（以 `/chat/completions` 结尾）。
    FullEndpoint,
}

/// 规范化模型：各字符串去首尾空白，`base_url` 再去掉尾部所有 `/`。
///
/// # 参数
/// - `value`：待处理的模型
///
/// # 示例
/// ```
/// use snow_config::custom_models::{CustomAiModel, normalize_custom_ai_model};
///
/// let model = CustomAiModel {
///     id: " a ".into(),
///     name: " n ".into(),
///     base_url: " http://url/// ".into(),
///     api_key: " k ".into(),
///     model: " m ".into(),
///     supports_vision: true,
///     supports_reasoning: false,
/// };
/// assert_eq!(normalize_custom_ai_model(model).base_url, "http://url");
/// ```
pub fn normalize_custom_ai_model(mut value: CustomAiModel) -> CustomAiModel {
    value.id = trimmed(&value.id).to_string();
    value.name = trimmed(&value.name).to_string();
    value.base_url = trimmed(&value.base_url).trim_end_matches('/').to_string();
    value.api_key = trimmed(&value.api_key).to_string();
    value.model = trimmed(&value.model).to_string();
    value
}

/// URL 中 StrictMode 不允许的 ASCII 字符（空白/控制字符与 `"<>\^`{|}`）。
fn is_forbidden_url_byte(byte: u8) -> bool {
    byte <= 0x20
        || byte == 0x7f
        || matches!(
            byte,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        )
}

/// 校验 authority（不含 userinfo）：拆出主机与端口并检查合法性。
fn authority_is_valid(authority: &str) -> bool {
    let host = if authority.starts_with('[') {
        // IPv6 字面量：`[...]` 后可跟 `:port`
        let Some(end) = authority.find(']') else {
            return false;
        };
        let (literal, tail) = authority.split_at(end + 1);
        if literal.len() <= 2 {
            return false;
        }
        if let Some(port) = tail.strip_prefix(':') {
            if !port_is_valid(port) {
                return false;
            }
        } else if !tail.is_empty() {
            return false;
        }
        return true;
    } else if let Some(index) = authority.rfind(':') {
        if !port_is_valid(&authority[index + 1..]) {
            return false;
        }
        &authority[..index]
    } else {
        authority
    };
    !host.is_empty()
        && host
            .chars()
            .all(|c| !c.is_ascii() || c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// 端口须为非空数字且不超过 65535。
fn port_is_valid(port: &str) -> bool {
    !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|value| value <= PORT_MAX)
}

/// 校验基础地址（`QUrl::StrictMode` 的受限子集）。
///
/// 要求：仅含允许字符、`%` 后跟两位十六进制、scheme 为 http/https、无 userinfo、
/// 主机非空、端口合法、无 query 与 fragment；path 以 `/chat/completions` 结尾则判为完整端点。
/// 与 Qt 的差异：不实现 IDN/百分号编码等更细的严格模式规则。
///
/// # 参数
/// - `value`：已去空白与尾部斜杠的地址
///
/// # 示例
/// ```
/// use snow_config::custom_models::{CustomAiModelUrlError, custom_ai_model_url_error};
///
/// assert_eq!(custom_ai_model_url_error("http://localhost:1234/v1"), CustomAiModelUrlError::None);
/// assert_eq!(
///     custom_ai_model_url_error("https://a.com/v1/chat/completions"),
///     CustomAiModelUrlError::FullEndpoint
/// );
/// ```
pub fn custom_ai_model_url_error(value: &str) -> CustomAiModelUrlError {
    use CustomAiModelUrlError::{FullEndpoint, InvalidBaseUrl};
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if is_forbidden_url_byte(*byte) {
            return InvalidBaseUrl;
        }
        if *byte == b'%'
            && !(index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit())
        {
            return InvalidBaseUrl;
        }
    }
    let Some(scheme_end) = value.find("://") else {
        return InvalidBaseUrl;
    };
    let scheme = &value[..scheme_end];
    let scheme_ok = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !scheme_ok || !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
    {
        return InvalidBaseUrl;
    }
    let after_scheme = &value[scheme_end + 3..];
    let split = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let (authority, rest) = after_scheme.split_at(split);
    if authority.contains('@') || !authority_is_valid(authority) {
        return InvalidBaseUrl;
    }
    if rest.contains('?') || rest.contains('#') {
        return InvalidBaseUrl;
    }
    if rest.to_ascii_lowercase().ends_with(FULL_ENDPOINT_SUFFIX) {
        return FullEndpoint;
    }
    CustomAiModelUrlError::None
}

/// ID 是否为小写、无花括号、非全零的 UUID（与 `QUuid::toString(WithoutBraces) == id` 等价）。
fn is_canonical_uuid(id: &str) -> bool {
    if id.len() != UUID_TEXT_LEN {
        return false;
    }
    let mut all_zero = true;
    for (index, byte) in id.bytes().enumerate() {
        if UUID_DASH_POSITIONS.contains(&index) {
            if byte != b'-' {
                return false;
            }
        } else if byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) {
            all_zero = all_zero && byte == b'0';
        } else {
            return false;
        }
    }
    !all_zero
}

/// 校验一条（已规范化的）模型是否合法。
///
/// # 参数
/// - `value`：模型
///
/// # 示例
/// ```
/// use snow_config::custom_models::{CustomAiModel, valid_custom_ai_model};
///
/// let model = CustomAiModel {
///     id: "12345678-1234-1234-1234-1234567890ab".into(),
///     name: "n".into(),
///     base_url: "http://test".into(),
///     api_key: String::new(),
///     model: "m".into(),
///     supports_vision: false,
///     supports_reasoning: false,
/// };
/// assert!(valid_custom_ai_model(&model));
/// ```
pub fn valid_custom_ai_model(value: &CustomAiModel) -> bool {
    is_canonical_uuid(&value.id)
        && !value.name.is_empty()
        && !value.model.is_empty()
        && !value.api_key.contains(['\r', '\n'])
        && custom_ai_model_url_error(&value.base_url) == CustomAiModelUrlError::None
}

/// 模型列表转 JSON 数组（每项含 7 个键）。
///
/// # 示例
/// ```
/// use snow_config::custom_models::custom_ai_models_to_json;
///
/// assert!(custom_ai_models_to_json(&[]).is_array());
/// ```
pub fn custom_ai_models_to_json(models: &[CustomAiModel]) -> Value {
    Value::Array(
        models
            .iter()
            .map(|model| {
                let mut map = Map::new();
                map.insert("id".into(), Value::String(model.id.clone()));
                map.insert("name".into(), Value::String(model.name.clone()));
                map.insert("base_url".into(), Value::String(model.base_url.clone()));
                map.insert("api_key".into(), Value::String(model.api_key.clone()));
                map.insert("model".into(), Value::String(model.model.clone()));
                map.insert("supports_vision".into(), Value::Bool(model.supports_vision));
                map.insert(
                    "supports_reasoning".into(),
                    Value::Bool(model.supports_reasoning),
                );
                Value::Object(map)
            })
            .collect(),
    )
}

/// 读取字符串字段；缺失或类型不符返回 `None`。
fn string_field<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    object?.get(key)?.as_str()
}

/// 从 JSON 读取模型列表：保留合法记录（读取时“挽救”），并报告是否全部合法。
///
/// # 返回
/// `(模型列表, 是否所有输入项都合法)`。重复 ID、重复名称（忽略大小写）、类型错误、字段非法的项被丢弃。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::custom_models::custom_ai_models_from_json;
///
/// let (models, all_valid) = custom_ai_models_from_json(&json!([]));
/// assert!(models.is_empty() && all_valid);
/// ```
pub fn custom_ai_models_from_json(value: &Value) -> (Vec<CustomAiModel>, bool) {
    let mut all_valid = value.is_array();
    let mut ids: HashSet<String> = HashSet::new();
    let mut names: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    for item in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let object = item.as_object();
        let text_keys = ["id", "name", "base_url", "api_key", "model"];
        let mut types_valid = text_keys
            .iter()
            .all(|key| string_field(object, key).is_some());
        let vision = object.and_then(|o| o.get("supports_vision"));
        types_valid = types_valid && vision.is_some_and(Value::is_boolean);
        let reasoning = object.and_then(|o| o.get("supports_reasoning"));
        types_valid = types_valid && reasoning.is_none_or(Value::is_boolean);
        let text = |key: &str| string_field(object, key).unwrap_or_default().to_string();
        let model = normalize_custom_ai_model(CustomAiModel {
            id: text("id"),
            name: text("name"),
            base_url: text("base_url"),
            api_key: text("api_key"),
            model: text("model"),
            supports_vision: vision.and_then(Value::as_bool).unwrap_or(false),
            supports_reasoning: reasoning.and_then(Value::as_bool).unwrap_or(false),
        });
        let folded_name = case_folded(&model.name);
        if !types_valid
            || !valid_custom_ai_model(&model)
            || ids.contains(&model.id)
            || names.contains(&folded_name)
        {
            all_valid = false;
            continue;
        }
        ids.insert(model.id.clone());
        names.insert(folded_name);
        result.push(model);
    }
    (result, all_valid)
}

/// 规范化 `api_configuration/custom_models`：结果为挽救后的规范列表，`valid` 表示输入是否全部合法。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::custom_models::normalize_custom_models;
///
/// assert!(normalize_custom_models(&json!([])).valid);
/// ```
pub fn normalize_custom_models(value: &Value) -> Normalization {
    let (models, valid) = custom_ai_models_from_json(value);
    let normalized = custom_ai_models_to_json(&models);
    let changed = !json_eq(&normalized, value);
    Normalization {
        value: normalized,
        valid,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "12345678-1234-4234-8234-1234567890ab";

    /// 生成合法模型 JSON。
    fn sample() -> Value {
        json!({"id": ID, "name": "My Model", "base_url": "http://localhost:1234/v1",
               "api_key": "", "model": "model-id", "supports_vision": true})
    }

    /// URL 黄金用例（C++ custom_ai_models_tests）。
    #[test]
    fn url_cases_match_cpp() {
        use CustomAiModelUrlError::*;
        assert_eq!(custom_ai_model_url_error("http://localhost:1234/v1"), None);
        assert_eq!(custom_ai_model_url_error("https://example.com"), None);
        assert_eq!(custom_ai_model_url_error("http://[::1]:8080/v1"), None);
        for bad in [
            "ftp://example.com",
            "https://user:password@example.com/v1",
            "https://example.com/v1?key=x",
            "https://example.com/v1#x",
            "/v1",
            "https://example.com:99999",
            "http://",
            "http://exa mple.com",
        ] {
            assert_eq!(custom_ai_model_url_error(bad), InvalidBaseUrl, "{bad}");
        }
        assert_eq!(
            custom_ai_model_url_error("https://example.com/v1/chat/completions"),
            FullEndpoint
        );
    }

    /// 缺失 supports_reasoning 视为 false；旧记录经规范化后补全字段。
    #[test]
    fn legacy_record_defaults_reasoning_off() {
        let (models, valid) = custom_ai_models_from_json(&json!([sample()]));
        assert!(valid && models.len() == 1 && !models[0].supports_reasoning);
        let out = normalize_custom_models(&json!([sample()]));
        assert!(out.valid && out.changed);
        assert_eq!(out.value[0]["supports_reasoning"], json!(false));
    }

    /// supports_reasoning 非 bool 则整体不合法（记录被丢弃）。
    #[test]
    fn reasoning_must_be_boolean() {
        let mut record = sample();
        record["supports_reasoning"] = json!("yes");
        let out = normalize_custom_models(&json!([record]));
        assert!(!out.valid);
        assert_eq!(out.value, json!([]));
    }

    /// 规范化：URL 尾部斜杠与名称空白被清理；重名（忽略大小写）与重复 ID 被丢弃。
    #[test]
    fn values_trimmed_and_duplicates_dropped() {
        let mut first = sample();
        first["base_url"] = json!("http://localhost:1234/v1/// ");
        first["name"] = json!("My Model ");
        let mut dup_name = sample();
        dup_name["id"] = json!("87654321-1234-4234-8234-1234567890ab");
        dup_name["name"] = json!("MY MODEL");
        let out = normalize_custom_models(&json!([first, dup_name]));
        assert!(!out.valid && out.changed);
        assert_eq!(out.value.as_array().unwrap().len(), 1);
        assert_eq!(out.value[0]["base_url"], json!("http://localhost:1234/v1"));
        assert_eq!(out.value[0]["name"], json!("My Model"));
    }

    /// 已规范的记录再规范化不变；非数组非法；非对象项被挽救丢弃。
    #[test]
    fn canonical_is_stable() {
        let mut record = sample();
        record["supports_reasoning"] = json!(false);
        let out = normalize_custom_models(&json!([record.clone()]));
        assert!(out.valid && !out.changed && out.value == json!([record]));
        assert!(!normalize_custom_models(&json!({})).valid);
        let out = normalize_custom_models(&json!([1, {"api_key": "never-log-this"}]));
        assert!(!out.valid && out.value == json!([]));
    }

    /// UUID 必须小写、无花括号、非全零。
    #[test]
    fn uuid_rules() {
        assert!(is_canonical_uuid(ID));
        assert!(!is_canonical_uuid(&ID.to_uppercase()));
        assert!(!is_canonical_uuid(&format!("{{{ID}}}")));
        assert!(!is_canonical_uuid("00000000-0000-0000-0000-000000000000"));
        assert!(!is_canonical_uuid("xyz"));
    }
}
