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
}

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
            (none, updates_status as Handler),
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

/// 测试用的内存后端。
#[cfg(test)]
pub(crate) struct FakeBackend;

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
}
