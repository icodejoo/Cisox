//! 配置文档：加载修复（越界/非法值回退默认）、历史迁移、未知字段保留、Qt 风格序列化。
//!
//! 对应 C++ `ConfigurationStore` 的 `materializeConfiguration` 与 `load` 的纯逻辑部分，
//! 不含文件 IO（见 [`crate::store`]）。磁盘格式与 Qt `QJsonDocument::toJson(Indented)` 字节一致：
//! 4 空格缩进、键按字典序、空数组/空对象写成“开括号-换行-缩进-闭括号”、末尾带换行。

use crate::custom_models::{custom_ai_models_from_json, custom_ai_models_to_json};
use crate::normalize::normalize;
use crate::schema::{
    SCHEMA_VERSION_KEY, complete_default_document, current_version, default_value, entries,
    insert_path, parse_integer_version, value_at_path,
};
use crate::value::{int_value, json_eq};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write;

/// 自定义模型键。
const CUSTOM_MODELS_KEY: &str = "api_configuration/custom_models";
/// 托盘菜单键。
const TRAY_MENU_KEY: &str = "tray/menu_options";
/// 贴图销毁快捷键键（v3 迁移对象）。
const DESTROY_SHORTCUT_KEY: &str = "pin_to_screen_shortcuts/destroy_window";
/// 恢复最近关闭窗口的快捷键键（用于判断托盘菜单是否为旧默认）。
const RESTORE_SHORTCUT_KEY: &str = "global_shortcuts/restore_last_closed_windows";
/// 托盘菜单里“恢复最近关闭窗口”项。
const RESTORE_TRAY_ITEM: &str = "quick.restore-last-closed-windows";
/// v3 之前贴图销毁快捷键的默认值。
const PREVIOUS_DESTROY_SHORTCUT: &str = "Ctrl+Esc";
/// 该版本起才使用当前销毁快捷键默认值。
const DESTROY_SHORTCUT_MIGRATION_VERSION: i32 = 3;
/// JSON 缩进（Qt Indented 格式）。
const INDENT: &str = "    ";

/// 文档兼容性状态（C++ `ConfigurationCompatibility`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    /// 正常。
    Current,
    /// 版本高于当前：整库只读。
    FutureVersion,
    /// 文件损坏或版本非法：已用默认值恢复。
    RecoveredDefaults,
    /// 存储不可读。
    Unavailable,
}

/// 覆盖策略：合并磁盘值，或全部替换（导入快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayPolicy {
    /// 用磁盘上已有的键覆盖默认值。
    MergeFromDisk,
    /// 以传入值整体替换。
    ReplaceAll,
}

/// 物化结果（C++ `MaterializedConfiguration`）。
struct Materialized {
    /// 扁平键值（含规范化后的值）。
    values: BTreeMap<String, Value>,
    /// 回写用的两级文档（保留未知字段）。
    document: Map<String, Value>,
    /// 是否需要回写。
    dirty: bool,
    /// 自定义模型是否有被丢弃的非法项。
    custom_models_repaired: bool,
}

/// 设置值失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// schema 中没有该键。
    UnknownKey(String),
    /// 值未通过键专属规范化。
    InvalidValue(String),
    /// 文档只读（未来版本或存储不可用）。
    ReadOnly,
}

impl std::fmt::Display for SetError {
    /// 输出面向日志的英文简述（不含配置内容，避免泄露密钥）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(key) => write!(f, "The configuration key is not supported: {key}"),
            Self::InvalidValue(key) => write!(f, "The configuration value is invalid: {key}"),
            Self::ReadOnly => write!(f, "Configuration storage is read-only"),
        }
    }
}

impl std::error::Error for SetError {}

/// 按 C++ `materializeConfiguration` 把覆盖值物化：缺失键补默认、非法值回退默认、执行迁移。
fn materialize(
    overlay: &BTreeMap<String, Value>,
    document: Map<String, Value>,
    schema_version: i32,
    compatibility: Compatibility,
    policy: OverlayPolicy,
) -> Materialized {
    let mut result = Materialized {
        values: BTreeMap::new(),
        document,
        dirty: false,
        custom_models_repaired: false,
    };
    let mutate_document = compatibility != Compatibility::FutureVersion;
    let replace_all = policy == OverlayPolicy::ReplaceAll;

    for item in entries() {
        if item.key == SCHEMA_VERSION_KEY {
            continue;
        }
        let Some(stored) = overlay.get(item.key) else {
            result
                .values
                .insert(item.key.to_string(), item.default.clone());
            if mutate_document {
                insert_path(&mut result.document, item.key, item.default.clone());
                result.dirty = result.dirty || !replace_all;
            }
            continue;
        };
        let mut raw = stored.clone();
        // 仅升级“未改动的旧默认托盘菜单”，用户自定义顺序保持不变
        if item.key == TRAY_MENU_KEY
            && mutate_document
            && !overlay.contains_key(RESTORE_SHORTCUT_KEY)
        {
            let previous_default: Vec<Value> = item
                .default
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter(|entry| entry.as_str() != Some(RESTORE_TRAY_ITEM))
                .cloned()
                .collect();
            if raw.as_array().is_some_and(|array| {
                json_eq(
                    &Value::Array(previous_default.clone()),
                    &Value::Array(array.clone()),
                )
            }) {
                raw = item.default.clone();
                insert_path(&mut result.document, item.key, raw.clone());
                result.dirty = true;
            }
        }
        let mut migrated_destroy_shortcut = false;
        if item.key == DESTROY_SHORTCUT_KEY
            && mutate_document
            && schema_version < DESTROY_SHORTCUT_MIGRATION_VERSION
        {
            let previous = normalize(
                item.key,
                &Value::Array(vec![Value::String(PREVIOUS_DESTROY_SHORTCUT.into())]),
            );
            let stored_normalized = normalize(item.key, &raw);
            if stored_normalized.valid && json_eq(&stored_normalized.value, &previous.value) {
                raw = item.default.clone();
                migrated_destroy_shortcut = true;
            }
        }
        if item.key == CUSTOM_MODELS_KEY {
            let (models, valid) = custom_ai_models_from_json(&raw);
            let canonical = custom_ai_models_to_json(&models);
            result
                .values
                .insert(item.key.to_string(), canonical.clone());
            result.custom_models_repaired = result.custom_models_repaired || !valid;
            if replace_all {
                insert_path(&mut result.document, item.key, canonical);
            }
            continue;
        }

        let normalized = normalize(item.key, &raw);
        if !normalized.valid {
            result
                .values
                .insert(item.key.to_string(), item.default.clone());
            if mutate_document {
                insert_path(&mut result.document, item.key, item.default.clone());
                result.dirty = result.dirty || !replace_all;
            }
            continue;
        }
        result
            .values
            .insert(item.key.to_string(), normalized.value.clone());
        if mutate_document && (replace_all || normalized.changed || migrated_destroy_shortcut) {
            insert_path(&mut result.document, item.key, normalized.value);
            if (normalized.changed || migrated_destroy_shortcut) && !replace_all {
                result.dirty = true;
            }
        }
    }

    let mut resolved_version = schema_version;
    let current = current_version();
    if mutate_document && resolved_version < current {
        resolved_version = current;
        result.dirty = result.dirty || !replace_all;
    }
    result.values.insert(
        SCHEMA_VERSION_KEY.to_string(),
        int_value(i64::from(resolved_version)),
    );
    if mutate_document && (replace_all || resolved_version != schema_version) {
        insert_path(
            &mut result.document,
            SCHEMA_VERSION_KEY,
            int_value(i64::from(resolved_version)),
        );
    }
    result
}

/// 从两级文档提取 schema 已知键的覆盖值（跳过版本键）。
fn overlay_from_document(document: &Map<String, Value>) -> BTreeMap<String, Value> {
    entries()
        .iter()
        .filter(|item| item.key != SCHEMA_VERSION_KEY)
        .filter_map(|item| {
            value_at_path(document, item.key).map(|value| (item.key.to_string(), value.clone()))
        })
        .collect()
}

/// 一份已加载的配置：扁平值 + 保留未知字段的回写文档。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::document::ConfigDocument;
///
/// let mut doc = ConfigDocument::from_bytes(None);
/// assert_eq!(doc.value("capture_history/retention_days"), json!(7));
/// doc.set_value("capture_history/retention_days", json!(30)).unwrap();
/// assert!(doc.set_value("capture_history/retention_days", json!(9999)).is_err());
/// ```
#[derive(Debug, Clone)]
pub struct ConfigDocument {
    /// 扁平键值。
    values: BTreeMap<String, Value>,
    /// 回写文档（含未知字段）。
    document: Map<String, Value>,
    /// 兼容性状态。
    compatibility: Compatibility,
    /// 是否有未落盘的修改。
    dirty: bool,
    /// 最近一次加载/修复产生的提示（不含配置内容）。
    last_error: String,
}

impl ConfigDocument {
    /// 从文件字节加载并修复。
    ///
    /// - `None`（文件不存在）：使用默认值，标记需要写出。
    /// - JSON 损坏、顶层非对象、版本非法：`RecoveredDefaults`，使用默认值（调用方负责保留损坏副本）。
    /// - 版本高于当前：`FutureVersion`，整库只读，不改写文档。
    /// - 其余：逐键规范化，非法/越界值回退默认，缺失键补默认，低版本执行迁移并升到当前版本。
    ///
    /// # 参数
    /// - `bytes`：文件内容
    pub fn from_bytes(bytes: Option<&[u8]>) -> Self {
        let mut values: BTreeMap<String, Value> = entries()
            .iter()
            .map(|item| (item.key.to_string(), item.default.clone()))
            .collect();
        let mut document = match complete_default_document() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let mut compatibility = Compatibility::Current;
        let mut dirty = bytes.is_none();
        let mut last_error = String::new();

        match bytes {
            None => {}
            Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
                Ok(Value::Object(parsed)) => {
                    let version =
                        value_at_path(&parsed, SCHEMA_VERSION_KEY).and_then(parse_integer_version);
                    match version {
                        None => {
                            compatibility = Compatibility::RecoveredDefaults;
                            dirty = true;
                            last_error =
                                "config.json has an invalid schema version; defaults were loaded"
                                    .to_string();
                        }
                        Some(version) => {
                            if version > current_version() {
                                compatibility = Compatibility::FutureVersion;
                                last_error = "Configuration schema is newer than this application; storage is read-only".to_string();
                            }
                            let materialized = materialize(
                                &overlay_from_document(&parsed),
                                parsed,
                                version,
                                compatibility,
                                OverlayPolicy::MergeFromDisk,
                            );
                            values = materialized.values;
                            document = materialized.document;
                            dirty = materialized.dirty;
                            if materialized.custom_models_repaired {
                                last_error = "Some custom AI model configurations are invalid and were ignored".to_string();
                            }
                        }
                    }
                }
                _ => {
                    compatibility = Compatibility::RecoveredDefaults;
                    dirty = true;
                    last_error = "config.json is malformed; defaults were loaded".to_string();
                }
            },
        }
        let dirty = dirty && compatibility != Compatibility::FutureVersion;
        Self {
            values,
            document,
            compatibility,
            dirty,
            last_error,
        }
    }

    /// 读取键的当前值；未知键返回 schema 默认值（即 `Null`）。
    ///
    /// # 参数
    /// - `key`：`"组/名"` 键
    pub fn value(&self, key: &str) -> Value {
        self.values
            .get(key)
            .cloned()
            .unwrap_or_else(|| default_value(key))
    }

    /// 全部扁平键值的只读视图。
    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// 兼容性状态。
    pub fn compatibility(&self) -> Compatibility {
        self.compatibility
    }

    /// 是否有未落盘的修改。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 是否可写（未来版本与不可用状态只读）。
    pub fn is_writable(&self) -> bool {
        !matches!(
            self.compatibility,
            Compatibility::FutureVersion | Compatibility::Unavailable
        )
    }

    /// 最近一次加载产生的提示，无则为空。
    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    /// 设置单个键：值先经键专属规范化，非法则拒绝且不改动任何状态。
    ///
    /// # 参数
    /// - `key`：`"组/名"` 键
    /// - `value`：新值
    ///
    /// # 返回
    /// 成功返回 `Ok(())`（值未变化也视为成功）。
    pub fn set_value(&mut self, key: &str, value: Value) -> Result<(), SetError> {
        self.set_values(std::iter::once((key.to_string(), value)))
    }

    /// 批量设置：全部通过规范化才会落地，否则整批拒绝（C++ `setValues` 语义）。
    ///
    /// # 参数
    /// - `updates`：`(键, 值)` 序列
    pub fn set_values(
        &mut self,
        updates: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), SetError> {
        let mut normalized_values: Vec<(String, Value)> = Vec::new();
        for (key, value) in updates {
            if !crate::schema::contains(&key) {
                return Err(SetError::UnknownKey(key));
            }
            let normalized = normalize(&key, &value);
            if !normalized.valid {
                return Err(SetError::InvalidValue(key));
            }
            normalized_values.push((key, normalized.value));
        }
        if !self.is_writable() {
            return Err(SetError::ReadOnly);
        }
        for (key, value) in normalized_values {
            if self
                .values
                .get(&key)
                .is_some_and(|current| json_eq(current, &value))
            {
                continue;
            }
            insert_path(&mut self.document, &key, value.clone());
            self.values.insert(key, value);
            self.dirty = true;
        }
        Ok(())
    }

    /// 以快照整体替换（配置导入）：`schema_version <= 0` 视为当前版本，高于当前则拒绝。
    ///
    /// # 参数
    /// - `values`：扁平键值（缺失键取默认，非法值回退默认）
    /// - `schema_version`：快照的 schema 版本
    ///
    /// # 返回
    /// 版本过新或文档只读时返回 `Err`。
    pub fn apply_snapshot(
        &mut self,
        values: &BTreeMap<String, Value>,
        schema_version: i32,
    ) -> Result<(), SetError> {
        let current = current_version();
        let version = match schema_version {
            v if v <= 0 => current,
            v if v > current => return Err(SetError::ReadOnly),
            v => v,
        };
        if !self.is_writable() {
            return Err(SetError::ReadOnly);
        }
        let base = match complete_default_document() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let materialized = materialize(
            values,
            base,
            version,
            Compatibility::Current,
            OverlayPolicy::ReplaceAll,
        );
        self.last_error = if materialized.custom_models_repaired {
            "Some custom AI model configurations are invalid and were ignored".to_string()
        } else {
            String::new()
        };
        if materialized.values != self.values {
            self.values = materialized.values;
            self.document = materialized.document;
            self.dirty = true;
        }
        Ok(())
    }

    /// 序列化为磁盘字节（Qt 风格缩进 JSON，末尾换行）。
    ///
    /// # 示例
    /// ```
    /// use snow_config::document::ConfigDocument;
    ///
    /// let text = String::from_utf8(ConfigDocument::from_bytes(None).to_bytes()).unwrap();
    /// assert!(text.starts_with("{\n    \"api_configuration\": {"));
    /// assert!(text.ends_with("}\n"));
    /// ```
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut text = String::new();
        write_qt_json(&Value::Object(self.document.clone()), 0, &mut text);
        text.push('\n');
        text.into_bytes()
    }

    /// 标记已落盘（由存储层在写入成功后调用）。
    pub(crate) fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// 设置提示信息（由存储层使用）。
    pub(crate) fn set_last_error(&mut self, message: &str) {
        self.last_error = message.to_string();
    }
}

/// 写出 JSON 字符串字面量（转义规则与 Qt 一致：`"`、`\`、控制字符；不转义 `/` 与非 ASCII）。
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// 以 Qt `QJsonDocument::toJson(Indented)` 的排版写出 JSON。
fn write_qt_json(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            // Qt 把所有数字当 double 写出：整数值不带小数点
            match number.as_f64() {
                Some(double) if double.fract() == 0.0 && double.abs() < 1e15 => {
                    let _ = write!(out, "{}", double as i64);
                }
                _ => {
                    let _ = write!(out, "{number}");
                }
            }
        }
        Value::String(text) => write_json_string(text, out),
        Value::Array(items) => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&INDENT.repeat(depth + 1));
                write_qt_json(item, depth + 1, out);
                out.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&INDENT.repeat(depth));
            out.push(']');
        }
        Value::Object(map) => {
            out.push_str("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                out.push_str(&INDENT.repeat(depth + 1));
                write_json_string(key, out);
                out.push_str(": ");
                write_qt_json(item, depth + 1, out);
                out.push_str(if index + 1 < map.len() { ",\n" } else { "\n" });
            }
            out.push_str(&INDENT.repeat(depth));
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// V8 spike 的脱敏真实样本（换行统一为 LF 以免受 git 换行转换影响）。
    fn sample() -> String {
        include_str!("../tests/fixtures/config.json").replace("\r\n", "\n")
    }

    /// 从 JSON 文本加载。
    fn load(text: &str) -> ConfigDocument {
        ConfigDocument::from_bytes(Some(text.as_bytes()))
    }

    /// 序列化器对真实样本逐字节往返一致（含空数组/空对象排版、键序、缩进）。
    #[test]
    fn writer_is_byte_identical_to_qt_on_real_sample() {
        let text = sample();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let mut out = String::new();
        write_qt_json(&parsed, 0, &mut out);
        out.push('\n');
        assert_eq!(out, text);
    }

    /// 真实样本（旧版本产出）加载后：仅 C++ 同款修复项改变，其余每个键与磁盘原值一致。
    ///
    /// 预期改变的键：样本缺失的两个新键补默认；两个动作工具栏布局补 latex-recognition；
    /// 目标翻译语言默认值 `""` 不在白名单内，C++ 每次加载都会判为非法并回写默认（同为空串）。
    #[test]
    fn real_sample_loads_with_only_cpp_repairs() {
        let text = sample();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let map = parsed.as_object().unwrap();
        let doc = load(&text);
        assert_eq!(doc.compatibility(), Compatibility::Current);
        assert!(doc.is_dirty());
        let mut changed_keys = Vec::new();
        for item in entries() {
            let loaded = doc.value(item.key);
            match value_at_path(map, item.key) {
                None => {
                    assert_eq!(loaded, item.default, "缺失键应补默认：{}", item.key);
                    changed_keys.push(item.key);
                }
                Some(raw) if !json_eq(raw, &loaded) => changed_keys.push(item.key),
                Some(_) => {}
            }
        }
        assert_eq!(
            changed_keys,
            [
                "text_recognition/detector_resize_policy",
                "pin_to_screen/action_tools_layout",
                "screenshot_toolbar/action_tools_layout",
                "screenshot/auto_recognize_qr_code",
            ]
        );
        for key in [
            "pin_to_screen/action_tools_layout",
            "screenshot_toolbar/action_tools_layout",
        ] {
            let layout = doc.value(key);
            assert!(
                layout["positions"][0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id == "latex-recognition")
            );
        }
        // 回写文档再加载应稳定（除去 target_language 的每次回写）
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        assert_eq!(reloaded.values(), doc.values());
    }

    /// 文件缺失：全默认并标记需写出；损坏 JSON 与非法版本：恢复默认。
    #[test]
    fn missing_or_malformed_falls_back_to_defaults() {
        let missing = ConfigDocument::from_bytes(None);
        assert!(missing.is_dirty() && missing.compatibility() == Compatibility::Current);
        assert_eq!(missing.values().len(), 238);
        for bad in [
            "{not json",
            "[]",
            "5",
            r#"{"storage":{"schema_version":"3"}}"#,
            "{}",
        ] {
            let doc = load(bad);
            assert_eq!(
                doc.compatibility(),
                Compatibility::RecoveredDefaults,
                "{bad}"
            );
            assert!(doc.is_dirty());
            assert_eq!(doc.value("capture_history/retention_days"), json!(7));
        }
    }

    /// 未来版本：整库只读、不改写、拒绝写入。
    #[test]
    fn future_version_is_read_only() {
        let mut doc =
            load(r#"{"storage":{"schema_version":4},"tray":{"left_click_action":"invalid"}}"#);
        assert_eq!(doc.compatibility(), Compatibility::FutureVersion);
        assert!(!doc.is_dirty() && !doc.is_writable());
        assert_eq!(
            doc.set_value("system/auto_start_at_boot", json!(false)),
            Err(SetError::ReadOnly)
        );
        let out = String::from_utf8(doc.to_bytes()).unwrap();
        assert!(out.contains("\"schema_version\": 4"));
    }

    /// 越界/非法值回退默认（V8 spike 只返回 Err 的偏差在此修正为 C++ 语义）。
    #[test]
    fn out_of_range_values_fall_back_to_defaults() {
        let doc = load(
            r#"{"storage":{"schema_version":3},
                "capture_history":{"retention_days":0,"max_entries":1001,"max_disk_mib":127,"compression_level":"ultra"},
                "tray":{"left_click_action":"invalid","middle_click_action":"invalid"}}"#,
        );
        assert!(doc.is_dirty());
        assert_eq!(doc.value("capture_history/retention_days"), json!(7));
        assert_eq!(doc.value("capture_history/max_entries"), json!(100));
        assert_eq!(doc.value("capture_history/max_disk_mib"), json!(1024));
        assert_eq!(
            doc.value("capture_history/compression_level"),
            json!("medium")
        );
        assert_eq!(doc.value("tray/left_click_action"), json!("screenshot"));
        assert_eq!(
            doc.value("tray/middle_click_action"),
            json!("screenshot_fixed")
        );
    }

    /// 未知字段与未知分组原样保留；已废弃键被忽略但保留在磁盘（C++ obsoleteClickThroughShortcutIsIgnored）。
    #[test]
    fn unknown_fields_are_preserved() {
        let doc = load(
            r#"{"storage":{"schema_version":2,"future":1},"future_group":{"x":[1,2]},
                "pin_to_screen_shortcuts":{"click_through":["Alt+M"]}}"#,
        );
        let out: Value = serde_json::from_slice(&doc.to_bytes()).unwrap();
        assert_eq!(out["future_group"], json!({"x": [1, 2]}));
        assert_eq!(out["storage"]["future"], json!(1));
        assert_eq!(
            out["pin_to_screen_shortcuts"]["click_through"],
            json!(["Alt+M"])
        );
        assert_eq!(out["storage"]["schema_version"], json!(3));
        assert_eq!(
            doc.value("pin_to_screen_shortcuts/click_through"),
            Value::Null
        );
        assert_eq!(
            doc.value("pin_to_screen_shortcuts/toggle_click_through"),
            json!([{"portable": "Ctrl+M"}])
        );
    }

    /// v1 快捷键迁移：显式绑定升级为结构化格式，其余键补默认。
    #[test]
    fn v1_shortcut_migration() {
        let doc = load(
            r#"{"storage":{"schema_version":1},"global_shortcuts":{"screenshot":["Ctrl+Alt+K"]},"screenshot_shortcuts":{"copy_color":["Alt+C"]}}"#,
        );
        assert_eq!(doc.value("storage/schema_version"), json!(3));
        assert_eq!(
            doc.value("global_shortcuts/screenshot"),
            json!([{"portable": "Ctrl+Alt+K"}])
        );
        assert_eq!(
            doc.value("screenshot_shortcuts/copy_color"),
            json!([{"portable": "Alt+C"}])
        );
        assert!(
            !doc.value("global_shortcuts/screenshot_copy")
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(doc.is_dirty());
    }

    /// v2 贴图销毁快捷键：旧默认 Ctrl+Esc 迁到 Shift+Esc；自定义保留；v3 上的 Ctrl+Esc 保留。
    #[test]
    fn destroy_shortcut_migration() {
        let key = "pin_to_screen_shortcuts/destroy_window";
        let write = |version: i32, shortcut: Value| {
            let doc = json!({"storage": {"schema_version": version},
                             "pin_to_screen_shortcuts": {"destroy_window": shortcut}});
            load(&doc.to_string())
        };
        let migrated = json!([{"portable": "Shift+Esc"}]);
        assert_eq!(write(2, json!(["Ctrl+Esc"])).value(key), migrated);
        assert_eq!(
            write(2, json!([{"portable": "Ctrl+Esc"}])).value(key),
            migrated
        );
        assert_eq!(
            write(2, json!(["Alt+X"])).value(key),
            json!([{"portable": "Alt+X"}])
        );
        assert_eq!(
            write(3, json!(["Ctrl+Esc"])).value(key),
            json!([{"portable": "Ctrl+Esc"}])
        );
    }

    /// 托盘菜单：旧默认（缺 restore 项）升级；自定义顺序保持；旧命令名迁移并回写。
    #[test]
    fn tray_menu_migrations() {
        let defaults = default_value("tray/menu_options");
        let previous: Vec<Value> = defaults
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item.as_str() != Some("quick.restore-last-closed-windows"))
            .cloned()
            .collect();
        let doc = load(
            &json!({"storage": {"schema_version": 2}, "tray": {"menu_options": previous}})
                .to_string(),
        );
        assert_eq!(doc.value("tray/menu_options"), defaults);
        let custom = json!(["tray.exit", "quick.screenshot"]);
        let doc = load(
            &json!({"storage": {"schema_version": 2}, "tray": {"menu_options": custom}})
                .to_string(),
        );
        assert_eq!(doc.value("tray/menu_options"), custom);
        let legacy = json!([
            "quick.screenshot",
            "tray.window-grouping",
            "tray.disable-shortcut-functions",
            "tray.exit"
        ]);
        let doc = load(
            &json!({"storage": {"schema_version": 2}, "tray": {"menu_options": legacy}})
                .to_string(),
        );
        assert_eq!(
            doc.value("tray/menu_options"),
            json!([
                "quick.screenshot",
                "tray.window-grouping",
                "quick.toggle-global-hotkeys",
                "tray.exit"
            ])
        );
        let out: Value = serde_json::from_slice(&doc.to_bytes()).unwrap();
        assert_eq!(out["tray"]["menu_options"], doc.value("tray/menu_options"));
    }

    /// 自定义模型：读取时挽救合法项并提示；原始记录在未写入前不被改动。
    #[test]
    fn custom_models_salvage() {
        let good = json!({"id": "12345678-1234-4234-8234-1234567890ab", "name": "M",
            "base_url": "http://localhost:1234/v1", "api_key": "never-log-this",
            "model": "m", "supports_vision": true, "supports_reasoning": false});
        let doc = load(
            &json!({"storage": {"schema_version": 3},
                    "api_configuration": {"custom_models": [good, {"api_key": "never-log-this"}]}})
            .to_string(),
        );
        assert_eq!(doc.value("api_configuration/custom_models"), json!([good]));
        assert!(!doc.last_error().is_empty() && !doc.last_error().contains("never-log-this"));
        let out: Value = serde_json::from_slice(&doc.to_bytes()).unwrap();
        assert_eq!(
            out["api_configuration"]["custom_models"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    /// 设置：规范化后写入；非法拒绝且状态不变；批量全有或全无。
    #[test]
    fn set_values_semantics() {
        let mut doc = ConfigDocument::from_bytes(Some(sample().as_bytes()));
        doc.mark_clean();
        assert!(!doc.is_dirty());
        doc.set_value("screenshot_ui/shortcut_hint_opacity", json!(42))
            .unwrap();
        assert!(doc.is_dirty());
        assert_eq!(doc.value("screenshot_ui/shortcut_hint_opacity"), json!(42));
        assert_eq!(
            doc.set_value("screenshot_ui/shortcut_hint_opacity", json!(101)),
            Err(SetError::InvalidValue(
                "screenshot_ui/shortcut_hint_opacity".into()
            ))
        );
        assert_eq!(doc.value("screenshot_ui/shortcut_hint_opacity"), json!(42));
        assert!(matches!(
            doc.set_value("nope/nope", json!(1)),
            Err(SetError::UnknownKey(_))
        ));
        let before = doc.value("mcp/enabled");
        let batch = vec![
            ("mcp/enabled".to_string(), json!(true)),
            ("screenshot/image_quality".to_string(), json!(500)),
        ];
        assert!(doc.set_values(batch).is_err());
        assert_eq!(doc.value("mcp/enabled"), before);
        doc.set_value("screenshot_ui/cursor_guide_line_color", json!("#abcdef80"))
            .unwrap();
        assert_eq!(
            doc.value("screenshot_ui/cursor_guide_line_color"),
            json!("#ABCDEF80")
        );
    }

    /// 快照导入：缺失键补默认、非法值回退、版本过新拒绝。
    #[test]
    fn apply_snapshot_semantics() {
        let mut doc = ConfigDocument::from_bytes(None);
        let mut values = BTreeMap::new();
        values.insert("capture_history/retention_days".to_string(), json!(30));
        values.insert("capture_history/max_entries".to_string(), json!(-5));
        doc.apply_snapshot(&values, 0).unwrap();
        assert_eq!(doc.value("capture_history/retention_days"), json!(30));
        assert_eq!(doc.value("capture_history/max_entries"), json!(100));
        assert!(doc.apply_snapshot(&values, 4).is_err());
    }

    /// 序列化：数字整数值不带小数点，字符串转义与 Qt 一致，非 ASCII 原样输出。
    #[test]
    fn writer_formats_scalars_like_qt() {
        let mut out = String::new();
        write_qt_json(
            &json!({"a": [1, 2.5, -3.0, true, null], "b": "x\"\\\n\u{1}/é"}),
            0,
            &mut out,
        );
        assert_eq!(
            out,
            "{\n    \"a\": [\n        1,\n        2.5,\n        -3,\n        true,\n        null\n    ],\n    \"b\": \"x\\\"\\\\\\n\\u0001/é\"\n}"
        );
    }

    /// 样本中的 `SnowShot_...` 文件名格式属合法用户数据，加载后必须原样保留。
    #[test]
    fn user_snowshot_filename_formats_are_preserved() {
        let doc = load(&sample());
        for (key, expected) in [
            (
                "screenshot/manual_save_filename_format",
                "SnowShot_{YYYY-MM-DD_HH-mm-ss}",
            ),
            (
                "screenshot/auto_save_filename_format",
                "SnowShot_{YYYY-MM-DD_HH-mm-ss}",
            ),
            (
                "screen_recording/video_filename_format",
                "SnowShot_Video_{YYYY-MM-DD_HH-mm-ss}",
            ),
        ] {
            assert_eq!(doc.value(key), json!(expected), "{key}");
        }
    }
}
