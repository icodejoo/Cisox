//! 已实现的 tool：应用域里不需要 UI 的只读类，外加服务自身状态。

use crate::registry::{Handler, Registry};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

/// 应用侧向 MCP 服务提供的只读数据（实现须 `Send + Sync`，在服务线程里被调用）。
pub trait AppBackend: Send + Sync {
    /// 应用概况：版本、平台等。
    fn app_status(&self) -> Value;
    /// 当前设置的快照（键 → 值）；调用方负责脱敏。
    fn settings_snapshot(&self) -> BTreeMap<String, Value>;
    /// 系统权限状态。
    fn permissions(&self) -> Value;
    /// 更新配置与状态（不触发网络请求）。
    fn updates_status(&self) -> Value;
    /// 显示器列表（需主线程数据，实现方带超时请求）。
    fn displays(&self) -> Result<Value, ToolError>;
    /// 批量写入设置（键已通过策略检查）；返回 `{"changed": [键...]}`。
    fn settings_update(&self, changes: Vec<(String, Value)>) -> Result<Value, ToolError>;
    /// 把若干设置恢复默认（键已通过策略检查）；返回 `{"changed": [键...]}`。
    fn settings_reset(&self, keys: Vec<String>) -> Result<Value, ToolError>;
    /// 触发一个应用动作（名字在 [`APP_ACTIONS`] 白名单内）。
    fn app_action(&self, action: &str) -> Result<Value, ToolError>;
    /// 数据目录占用概况（只读，不做清理）。
    fn storage_status(&self) -> Value;
    /// 翻译可用的语言目录。
    fn translation_catalog(&self) -> Value;
}

/// `snow_shot_app_action` 允许的动作名（只开窗口类，不含退出 / 重启等破坏性动作）。
pub const APP_ACTIONS: &[&str] = &[
    "open_settings",
    "open_main_window",
    "open_history",
    "open_pin_manage",
    "open_translate_input",
    "pin_clipboard",
];
/// 单次 `settings_update` / `settings_reset` 最多涉及的键数。
const MAX_SETTING_KEYS: usize = 64;
/// 禁止经 MCP 写入的配置键前缀（MCP 自身的授权项，防止自我提权）。
const PROTECTED_PREFIXES: &[&str] = &["mcp/"];
/// 禁止经 MCP 写入的配置键（更新端点属于外联目标，须用户自己改）。
const PROTECTED_KEYS: &[&str] = &["updates/manifest_url"];

/// 服务端自身信息。
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// 对 MCP 客户端展示的服务名。
    pub name: String,
    /// 应用版本。
    pub version: String,
}

/// tool 处理函数的运行上下文。
pub struct ToolContext<'a> {
    /// 应用数据来源。
    pub backend: &'a dyn AppBackend,
    /// 服务信息。
    pub server: &'a ServerInfo,
    /// 注册表（用于统计）。
    pub registry: &'a Registry,
}

/// tool 执行失败：带稳定的错误码，经 `isError` 结果回给客户端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// 稳定错误码（`snake_case`）。
    pub code: &'static str,
    /// 简短英文说明。
    pub message: String,
}

/// 含这些片段的设置键视为敏感，值不外传。
const SENSITIVE_KEY_PARTS: &[&str] = &["api_key", "token", "secret", "password", "credential"];
/// 脱敏后的占位值。
const REDACTED: &str = "<redacted>";

/// 判断敏感键：含密钥类片段。
fn is_sensitive(key: &str) -> bool {
    SENSITIVE_KEY_PARTS.iter().any(|part| key.contains(part))
}

/// 检查键是否允许经 MCP 写入。
///
/// # 返回
/// 允许为 `Ok`；受保护键与敏感键返回 `protected_setting`。
fn check_writable_key(key: &str) -> Result<(), ToolError> {
    if PROTECTED_PREFIXES.iter().any(|p| key.starts_with(p))
        || PROTECTED_KEYS.contains(&key)
        || is_sensitive(key)
    {
        return Err(ToolError {
            code: "protected_setting",
            message: format!("setting cannot be changed through MCP: {key}"),
        });
    }
    Ok(())
}

/// 已实现 tool 的处理表：`名 → (入参 schema, 处理函数)`。
pub fn implemented() -> HashMap<&'static str, (Value, Handler)> {
    let none = json!({"type": "object", "additionalProperties": false});
    HashMap::from([
        (
            "snow_shot_mcp_status",
            (none.clone(), mcp_status as Handler),
        ),
        (
            "snow_shot_app_status",
            (none.clone(), app_status as Handler),
        ),
        (
            "snow_shot_settings_get",
            (
                json!({
                    "type": "object",
                    "properties": {
                        "section": {"type": "string"},
                        "key": {"type": "string"}
                    },
                    "additionalProperties": false
                }),
                settings_get as Handler,
            ),
        ),
        (
            "snow_shot_permissions_get",
            (none.clone(), permissions_get as Handler),
        ),
        (
            "snow_shot_updates_status",
            (none.clone(), updates_status as Handler),
        ),
        (
            "snow_shot_app_displays",
            (none.clone(), app_displays as Handler),
        ),
        (
            "snow_shot_app_action",
            (
                json!({
                    "type": "object",
                    "properties": {"action": {"type": "string", "enum": APP_ACTIONS}},
                    "required": ["action"],
                    "additionalProperties": false
                }),
                app_action as Handler,
            ),
        ),
        (
            "snow_shot_settings_update",
            (
                json!({
                    "type": "object",
                    "properties": {"settings": {"type": "object"}},
                    "required": ["settings"],
                    "additionalProperties": false
                }),
                settings_update as Handler,
            ),
        ),
        (
            "snow_shot_settings_reset",
            (
                json!({
                    "type": "object",
                    "properties": {"keys": {"type": "array", "items": {"type": "string"}}},
                    "required": ["keys"],
                    "additionalProperties": false
                }),
                settings_reset as Handler,
            ),
        ),
        (
            "snow_shot_permissions_request",
            (
                json!({
                    "type": "object",
                    "properties": {"permission": {"type": "string"}},
                    "additionalProperties": false
                }),
                permissions_request as Handler,
            ),
        ),
        (
            "snow_shot_storage_status",
            (none.clone(), storage_status as Handler),
        ),
        (
            "snow_shot_translation_catalog",
            (none, translation_catalog as Handler),
        ),
    ])
}

/// 服务自身状态：协议版本、tool 总数与已实现数。
fn mcp_status(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    let total = ctx.registry.all().len();
    let implemented = ctx.registry.implemented_count();
    Ok(json!({
        "protocol": crate::protocol::PROTOCOL_VERSION,
        "server": {"name": ctx.server.name, "version": ctx.server.version},
        "tools_total": total,
        "tools_implemented": implemented,
        "tools_pending": total - implemented,
    }))
}

/// 应用概况。
fn app_status(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    Ok(ctx.backend.app_status())
}

/// 读取设置：可按 `key`（精确）或 `section`（键前缀，如 `screenshot`）过滤；敏感键脱敏。
fn settings_get(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, ToolError> {
    let key = args.get("key").and_then(Value::as_str);
    let section = args.get("section").and_then(Value::as_str);
    let prefix = section.map(|s| format!("{}/", s.trim_end_matches('/')));
    let mut values = Map::new();
    for (k, v) in ctx.backend.settings_snapshot() {
        let selected = match (key, &prefix) {
            (Some(want), _) => k == want,
            (None, Some(p)) => k.starts_with(p.as_str()),
            (None, None) => true,
        };
        if !selected {
            continue;
        }
        let sensitive = SENSITIVE_KEY_PARTS.iter().any(|part| k.contains(part));
        values.insert(k, if sensitive { json!(REDACTED) } else { v });
    }
    if let Some(want) = key
        && values.is_empty()
    {
        return Err(ToolError {
            code: "unknown_setting",
            message: format!("no such setting: {want}"),
        });
    }
    Ok(json!({"values": values}))
}

/// 系统权限状态。
fn permissions_get(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    Ok(ctx.backend.permissions())
}

/// 更新状态。
fn updates_status(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    Ok(ctx.backend.updates_status())
}

/// 显示器列表。
fn app_displays(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    ctx.backend.displays()
}

/// 触发应用动作（白名单由 schema 的 enum 约束，这里再兜一次底）。
fn app_action(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, ToolError> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !APP_ACTIONS.contains(&action) {
        return Err(ToolError {
            code: "unknown_action",
            message: format!("unknown action: {action}"),
        });
    }
    ctx.backend.app_action(action)
}

/// 键数必须在 1..=上限。
fn check_key_count(count: usize) -> Result<(), ToolError> {
    if count == 0 || count > MAX_SETTING_KEYS {
        return Err(ToolError {
            code: "invalid_arguments",
            message: format!("expected 1..={MAX_SETTING_KEYS} keys, got {count}"),
        });
    }
    Ok(())
}

/// 批量写设置：先整体做策略检查，任一键不允许则一个都不写。
fn settings_update(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, ToolError> {
    let changes: Vec<(String, Value)> = args
        .get("settings")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    check_key_count(changes.len())?;
    for (key, _) in &changes {
        check_writable_key(key)?;
    }
    ctx.backend.settings_update(changes)
}

/// 批量恢复默认：策略同 [`settings_update`]。
fn settings_reset(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, ToolError> {
    let keys: Vec<String> = args
        .get("keys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    check_key_count(keys.len())?;
    for key in &keys {
        check_writable_key(key)?;
    }
    ctx.backend.settings_reset(keys)
}

/// 申请系统权限：当前平台不需要弹授权，只回报状态，`prompted` 恒为 false。
fn permissions_request(ctx: &ToolContext<'_>, args: &Value) -> Result<Value, ToolError> {
    Ok(json!({
        "permissions": ctx.backend.permissions(),
        "requested": args.get("permission").cloned().unwrap_or(Value::Null),
        "prompted": false,
    }))
}

/// 存储概况。
fn storage_status(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    Ok(ctx.backend.storage_status())
}

/// 翻译语言目录。
fn translation_catalog(ctx: &ToolContext<'_>, _args: &Value) -> Result<Value, ToolError> {
    Ok(ctx.backend.translation_catalog())
}

/// 测试用的内存后端。
#[cfg(test)]
pub(crate) struct FakeBackend;

/// 测试用：记录 FakeBackend 收到的写入类调用。
#[cfg(test)]
pub(crate) static FAKE_CALLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
impl AppBackend for FakeBackend {
    fn app_status(&self) -> Value {
        json!({"version": "9.9.9", "platform": "test"})
    }

    fn settings_snapshot(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("screenshot/a".to_string(), json!(1)),
            ("screenshot/b".to_string(), json!("x")),
            ("ai/api_key".to_string(), json!("sk-secret")),
            ("mcp/enabled".to_string(), json!(true)),
        ])
    }

    fn permissions(&self) -> Value {
        json!({"required": []})
    }

    fn updates_status(&self) -> Value {
        json!({"manifest_configured": false})
    }

    fn displays(&self) -> Result<Value, ToolError> {
        Ok(json!({"displays": [{"primary": true}]}))
    }

    fn settings_update(&self, changes: Vec<(String, Value)>) -> Result<Value, ToolError> {
        let keys: Vec<String> = changes.into_iter().map(|(k, _)| k).collect();
        FAKE_CALLS
            .lock()
            .unwrap()
            .push(format!("update:{}", keys.join(",")));
        Ok(json!({"changed": keys}))
    }

    fn settings_reset(&self, keys: Vec<String>) -> Result<Value, ToolError> {
        FAKE_CALLS
            .lock()
            .unwrap()
            .push(format!("reset:{}", keys.join(",")));
        Ok(json!({"changed": keys}))
    }

    fn app_action(&self, action: &str) -> Result<Value, ToolError> {
        FAKE_CALLS.lock().unwrap().push(format!("action:{action}"));
        Ok(json!({"accepted": action}))
    }

    fn storage_status(&self) -> Value {
        json!({"total_bytes": 0})
    }

    fn translation_catalog(&self) -> Value {
        json!({"languages": []})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造上下文并调用。
    fn call(name: &str, args: Value) -> Result<Value, ToolError> {
        let registry = Registry::build();
        let info = ServerInfo {
            name: "t".into(),
            version: "1".into(),
        };
        let ctx = ToolContext {
            backend: &FakeBackend,
            server: &info,
            registry: &registry,
        };
        let handler = registry.get(name).unwrap().handler.unwrap();
        handler(&ctx, &args)
    }

    /// 状态类 tool 返回后端数据与统计。
    #[test]
    fn status_tools() {
        let s = call("snow_shot_mcp_status", json!({})).unwrap();
        assert_eq!(s["tools_total"], 101);
        assert_eq!(
            s["tools_pending"].as_u64().unwrap() + s["tools_implemented"].as_u64().unwrap(),
            101
        );
        assert_eq!(
            call("snow_shot_app_status", json!({})).unwrap()["version"],
            "9.9.9"
        );
        assert!(call("snow_shot_permissions_get", json!({})).is_ok());
        assert!(call("snow_shot_updates_status", json!({})).is_ok());
    }

    /// settings_get：全量、前缀、精确、未知键；敏感值脱敏。
    #[test]
    fn settings_get_filters_and_redacts() {
        let all = call("snow_shot_settings_get", json!({})).unwrap();
        assert_eq!(all["values"]["ai/api_key"], REDACTED);
        assert_eq!(all["values"]["mcp/enabled"], true);
        let sec = call("snow_shot_settings_get", json!({"section": "screenshot"})).unwrap();
        assert_eq!(sec["values"].as_object().unwrap().len(), 2);
        let one = call("snow_shot_settings_get", json!({"key": "screenshot/a"})).unwrap();
        assert_eq!(one["values"]["screenshot/a"], 1);
        let err = call("snow_shot_settings_get", json!({"key": "nope/x"})).unwrap_err();
        assert_eq!(err.code, "unknown_setting");
    }

    /// 参数策略：受保护键 / 敏感键 / 空键 / 未知动作被拒，且被拒的调用后端一次都没收到。
    #[test]
    fn control_tools_enforce_key_policy() {
        FAKE_CALLS.lock().unwrap().clear();
        let ok = call(
            "snow_shot_settings_update",
            json!({"settings": {"screenshot/a": 1}}),
        )
        .unwrap();
        assert_eq!(ok["changed"][0], "screenshot/a");
        for bad in ["mcp/allow_control", "ai/api_key", "updates/manifest_url"] {
            let err = call(
                "snow_shot_settings_update",
                json!({"settings": {"screenshot/a": 1, bad: true}}),
            )
            .unwrap_err();
            assert_eq!(err.code, "protected_setting", "{bad}");
            let err = call("snow_shot_settings_reset", json!({"keys": [bad]})).unwrap_err();
            assert_eq!(err.code, "protected_setting", "{bad}");
        }
        let err = call("snow_shot_settings_update", json!({"settings": {}})).unwrap_err();
        assert_eq!(err.code, "invalid_arguments");
        let err = call("snow_shot_settings_reset", json!({"keys": []})).unwrap_err();
        assert_eq!(err.code, "invalid_arguments");
        let err = call("snow_shot_app_action", json!({"action": "quit"})).unwrap_err();
        assert_eq!(err.code, "unknown_action");
        assert_eq!(
            call("snow_shot_app_action", json!({"action": "open_settings"})).unwrap()["accepted"],
            "open_settings"
        );
        let calls = FAKE_CALLS.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("mcp/") && !c.contains("api_key")),
            "{calls:?}"
        );
    }

    /// 只读 / 状态类新 tool 直接返回后端数据。
    #[test]
    fn read_tools_pass_through() {
        assert_eq!(
            call("snow_shot_app_displays", json!({})).unwrap()["displays"][0]["primary"],
            true
        );
        assert_eq!(
            call("snow_shot_storage_status", json!({})).unwrap()["total_bytes"],
            0
        );
        assert!(call("snow_shot_translation_catalog", json!({})).unwrap()["languages"].is_array());
        let p = call(
            "snow_shot_permissions_request",
            json!({"permission": "screen"}),
        )
        .unwrap();
        assert_eq!(p["prompted"], false);
        assert_eq!(p["requested"], "screen");
    }
}
