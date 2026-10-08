//! 单个连接的会话状态机：鉴权 → `initialize` → `tools/list` / `tools/call`。
//!
//! 与传输无关：输入一行文本，输出 [`Outcome`]，因此可完全离屏测试。

use crate::auth::{AuthGate, Token};
use crate::protocol::{
    ERR_FORBIDDEN, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_NOT_INITIALIZED, ERR_UNAUTHORIZED,
    PROTOCOL_VERSION, Request, error_response, ok_response, parse_request,
};
use crate::registry::{Registry, Scope, ToolDef};
use crate::schema;
use crate::tools::{AppBackend, ServerInfo, ToolContext, ToolError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 连接首条消息必须是的鉴权方法名。
pub const AUTH_METHOD: &str = "cisox/auth";

/// 所有连接共享的服务状态。
pub struct Shared {
    /// 本次启用周期的令牌。
    pub token: Token,
    /// tool 注册表。
    pub registry: Registry,
    /// 应用数据来源。
    pub backend: Arc<dyn AppBackend>,
    /// 服务信息。
    pub server: ServerInfo,
    /// 已授权的权限域。
    pub granted: Vec<Scope>,
    /// 鉴权失败计数（跨连接，用于退避）。
    pub gate: Mutex<AuthGate>,
}

/// 处理一行输入的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// 要回写的一行；`None` 表示不回（通知）。
    pub reply: Option<String>,
    /// 回写后是否断开连接。
    pub close: bool,
    /// 回写前应等待的时长（鉴权失败退避）。
    pub delay: Duration,
}

/// 会话阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// 等待鉴权。
    Unauthenticated,
    /// 已鉴权，等待 `initialize`。
    Authenticated,
    /// 已完成 `initialize`。
    Initialized,
}

/// 一个连接的会话。
pub struct Session {
    /// 共享状态。
    shared: Arc<Shared>,
    /// 当前阶段。
    stage: Stage,
}

impl Session {
    /// 为新连接创建会话。
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            stage: Stage::Unauthenticated,
        }
    }

    /// 处理一行输入。
    ///
    /// # 参数
    /// - `line`：一行 JSON-RPC 文本。
    ///
    /// # 返回
    /// 回应、是否断开与退避时长。
    pub fn handle_line(&mut self, line: &str) -> Outcome {
        if self.stage == Stage::Unauthenticated {
            return self.authenticate(line);
        }
        let request = match parse_request(line) {
            Ok(r) => r,
            Err(f) => return reply(error_response(&f.id, f.code, f.message, None)),
        };
        let Some(id) = request.id.clone() else {
            // 通知：不回应；`initialized` 之外的通知直接忽略
            return Outcome::default();
        };
        reply(self.dispatch(&request, &id))
    }

    /// 首条消息：必须是携带正确令牌的鉴权请求；任何偏差都只回统一的未授权错误并断开。
    fn authenticate(&mut self, line: &str) -> Outcome {
        let parsed = parse_request(line).ok();
        let id = parsed
            .as_ref()
            .and_then(|r| r.id.clone())
            .unwrap_or(Value::Null);
        let presented = parsed
            .as_ref()
            .filter(|r| r.method == AUTH_METHOD)
            .and_then(|r| r.params.get("token"))
            .and_then(Value::as_str);
        let accepted = presented.is_some_and(|t| self.shared.token.matches(t));
        let mut gate = self.shared.gate.lock().unwrap_or_else(|e| e.into_inner());
        if accepted {
            gate.record_success();
            self.stage = Stage::Authenticated;
            return reply(ok_response(&id, json!({"authenticated": true})));
        }
        let delay = gate.record_failure();
        Outcome {
            reply: Some(error_response(&id, ERR_UNAUTHORIZED, "unauthorized", None)),
            close: true,
            delay,
        }
    }

    /// 已鉴权后的方法分发。
    fn dispatch(&mut self, request: &Request, id: &Value) -> String {
        match request.method.as_str() {
            "initialize" => self.initialize(request, id),
            "ping" => ok_response(id, json!({})),
            "tools/list" | "tools/call" if self.stage != Stage::Initialized => {
                error_response(id, ERR_NOT_INITIALIZED, "not initialized", None)
            }
            "tools/list" => self.tools_list(id),
            "tools/call" => self.tools_call(request, id),
            _ => error_response(id, ERR_METHOD_NOT_FOUND, "method not found", None),
        }
    }

    /// 握手：协议版本必须一致。
    fn initialize(&mut self, request: &Request, id: &Value) -> String {
        let requested = request
            .params
            .get("protocolVersion")
            .and_then(Value::as_str);
        if requested != Some(PROTOCOL_VERSION) {
            return error_response(
                id,
                ERR_INVALID_PARAMS,
                "unsupported protocol version",
                Some(json!({"supported": [PROTOCOL_VERSION], "requested": requested})),
            );
        }
        self.stage = Stage::Initialized;
        let server = &self.shared.server;
        ok_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": server.name, "version": server.version},
            }),
        )
    }

    /// 列出全部 tool（含尚未实现的，描述里注明）。
    fn tools_list(&self, id: &Value) -> String {
        let tools: Vec<Value> = self
            .shared
            .registry
            .all()
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": describe(t),
                    "inputSchema": t.schema,
                })
            })
            .collect();
        ok_response(id, json!({"tools": tools}))
    }

    /// 调用 tool：未知名 → 参数错误；越权 → 禁止；schema 不符 → 参数错误；未实现 → 结构化错误结果。
    fn tools_call(&self, request: &Request, id: &Value) -> String {
        let Some(name) = request.params.get("name").and_then(Value::as_str) else {
            return error_response(id, ERR_INVALID_PARAMS, "missing tool name", None);
        };
        let Some(tool) = self.shared.registry.get(name) else {
            return error_response(
                id,
                ERR_INVALID_PARAMS,
                "unknown tool",
                Some(json!({"tool": name})),
            );
        };
        if !self.shared.granted.contains(&tool.scope) {
            return error_response(
                id,
                ERR_FORBIDDEN,
                "scope not granted",
                Some(json!({"tool": name, "scope": tool.scope.name()})),
            );
        }
        let args = request
            .params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Err(reason) = schema::validate(&tool.schema, &args) {
            return error_response(
                id,
                ERR_INVALID_PARAMS,
                "invalid arguments",
                Some(json!({"tool": name, "reason": reason})),
            );
        }
        let Some(handler) = tool.handler else {
            return ok_response(id, tool_error_result(&not_implemented(tool)));
        };
        let ctx = ToolContext {
            backend: self.shared.backend.as_ref(),
            server: &self.shared.server,
            registry: &self.shared.registry,
        };
        match handler(&ctx, &args) {
            Ok(value) => ok_response(id, tool_ok_result(&value)),
            Err(e) => ok_response(id, tool_error_result(&e_to_json(&e))),
        }
    }
}

/// 构造只含一行回应的结果。
fn reply(text: String) -> Outcome {
    Outcome {
        reply: Some(text),
        close: false,
        delay: Duration::ZERO,
    }
}

/// `tools/list` 里的描述文本。
fn describe(tool: &ToolDef) -> String {
    let status = if tool.handler.is_some() {
        "available"
    } else {
        "not implemented yet"
    };
    format!("{} domain tool ({status})", tool.domain.legacy_key())
}

/// 未实现错误的结构化内容。
fn not_implemented(tool: &ToolDef) -> Value {
    json!({
        "code": "not_implemented",
        "tool": tool.name,
        "domain": tool.domain.legacy_key(),
        "milestone": tool.domain.milestone(),
    })
}

/// 把处理函数的错误转成结构化内容。
fn e_to_json(e: &ToolError) -> Value {
    json!({"code": e.code, "message": e.message})
}

/// 成功的 tool 结果（文本 + 结构化内容）。
fn tool_ok_result(value: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
        "isError": false,
    })
}

/// 失败的 tool 结果（`isError` 为真，错误放进 `structuredContent.error`）。
fn tool_error_result(error: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": error.to_string()}],
        "structuredContent": {"error": error},
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::FakeBackend;

    /// 固定令牌。
    const TOKEN: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    /// 构造共享状态。
    fn shared(granted: &[Scope]) -> Arc<Shared> {
        Arc::new(Shared {
            token: Token::generate(|b| {
                b.fill(1);
                Ok(())
            })
            .unwrap(),
            registry: Registry::build(),
            backend: Arc::new(FakeBackend),
            server: ServerInfo {
                name: "t".into(),
                version: "1".into(),
            },
            granted: granted.to_vec(),
            gate: Mutex::new(AuthGate::default()),
        })
    }

    /// 发送一行并解析回应。
    fn send(s: &mut Session, msg: Value) -> (Value, Outcome) {
        let out = s.handle_line(&msg.to_string());
        let v = out
            .reply
            .as_deref()
            .map(|r| serde_json::from_str(r).unwrap())
            .unwrap_or(Value::Null);
        (v, out)
    }

    /// 已鉴权并完成握手的会话。
    fn ready(shared: Arc<Shared>) -> Session {
        let mut s = Session::new(shared);
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":AUTH_METHOD,"params":{"token":TOKEN}}),
        );
        assert_eq!(v["result"]["authenticated"], true);
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}),
        );
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
        s
    }

    /// 调用 tool 的便捷封装。
    fn call(s: &mut Session, name: &str, args: Value) -> Value {
        send(
            s,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":args}}),
        )
        .0
    }

    /// 鉴权失败：错误令牌、缺令牌、非鉴权首条、乱码都断开，且回应一致不泄漏细节。
    #[test]
    fn auth_failures_close_with_uniform_error() {
        let sh = shared(Scope::DEFAULT_GRANTED);
        let cases = [
            json!({"jsonrpc":"2.0","id":1,"method":AUTH_METHOD,"params":{"token":"wrong"}}),
            json!({"jsonrpc":"2.0","id":1,"method":AUTH_METHOD,"params":{}}),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION}}),
        ];
        for case in cases {
            let mut s = Session::new(Arc::clone(&sh));
            let (v, out) = send(&mut s, case);
            assert!(out.close);
            assert_eq!(v["error"]["code"], ERR_UNAUTHORIZED);
            assert!(v["error"].get("data").is_none());
            assert!(!out.reply.unwrap().contains(TOKEN));
        }
        let mut s = Session::new(Arc::clone(&sh));
        let out = s.handle_line("not json");
        assert!(out.close);
        assert_eq!(sh.gate.lock().unwrap().failures(), 4);
        // 退避随失败次数增长；成功后清零
        assert!(out.delay >= Duration::from_millis(200));
        ready(Arc::clone(&sh));
        assert_eq!(sh.gate.lock().unwrap().failures(), 0);
    }

    /// 握手：版本不一致被拒并给出支持的版本；握手前不能调 tools。
    #[test]
    fn handshake_rules() {
        let sh = shared(Scope::DEFAULT_GRANTED);
        let mut s = Session::new(Arc::clone(&sh));
        send(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":AUTH_METHOD,"params":{"token":TOKEN}}),
        );
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        assert_eq!(v["error"]["code"], ERR_NOT_INITIALIZED);
        let (v, out) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
        );
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
        assert_eq!(v["error"]["data"]["supported"][0], PROTOCOL_VERSION);
        assert!(!out.close);
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":{}}),
        );
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
    }

    /// 通知不回应；未知方法、批量、坏 JSON 各有对应错误；ping 可用。
    #[test]
    fn protocol_edges() {
        let mut s = ready(shared(Scope::DEFAULT_GRANTED));
        let out = s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(out, Outcome::default());
        let (v, _) = send(&mut s, json!({"jsonrpc":"2.0","id":5,"method":"nope"}));
        assert_eq!(v["error"]["code"], ERR_METHOD_NOT_FOUND);
        let out = s.handle_line("[1]");
        assert!(out.reply.unwrap().contains("-32600"));
        assert!(!out.close);
        let (v, _) = send(&mut s, json!({"jsonrpc":"2.0","id":6,"method":"ping"}));
        assert_eq!(v["result"], json!({}));
    }

    /// tools/list 列出 101 个，带名称与 schema。
    #[test]
    fn lists_all_tools() {
        let mut s = ready(shared(Scope::DEFAULT_GRANTED));
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}),
        );
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 101);
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
        assert!(tools.iter().any(|t| t["name"] == "snow_shot_app_status"));
    }

    /// tools/call：已实现成功；未知 tool、缺 name、schema 不符走协议错误。
    #[test]
    fn call_success_unknown_and_schema() {
        let mut s = ready(shared(Scope::DEFAULT_GRANTED));
        let v = call(&mut s, "snow_shot_app_status", json!({}));
        assert_eq!(v["result"]["isError"], false);
        assert_eq!(v["result"]["structuredContent"]["version"], "9.9.9");
        let v = call(&mut s, "snow_shot_does_not_exist", json!({}));
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
        assert_eq!(v["error"]["message"], "unknown tool");
        let (v, _) = send(
            &mut s,
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{}}),
        );
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
        let v = call(&mut s, "snow_shot_settings_get", json!({"section": 5}));
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
        assert!(
            v["error"]["data"]["reason"]
                .as_str()
                .unwrap()
                .contains("section")
        );
        let v = call(&mut s, "snow_shot_app_status", json!({"extra": 1}));
        assert_eq!(v["error"]["code"], ERR_INVALID_PARAMS);
        let v = call(&mut s, "snow_shot_settings_get", json!({"key": "nope/x"}));
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(
            v["result"]["structuredContent"]["error"]["code"],
            "unknown_setting"
        );
    }

    /// 未实现的 tool 返回结构化“未实现”错误结果，并带域与里程碑。
    #[test]
    fn unimplemented_tool_returns_structured_error() {
        let mut s = ready(shared(Scope::DEFAULT_GRANTED));
        let v = call(
            &mut s,
            "snow_shot_screenshot_begin",
            json!({"anything": true}),
        );
        assert_eq!(v["result"]["isError"], true);
        let e = &v["result"]["structuredContent"]["error"];
        assert_eq!(e["code"], "not_implemented");
        assert_eq!(e["tool"], "snow_shot_screenshot_begin");
        assert_eq!(e["domain"], "screenshot");
        assert_eq!(e["milestone"], "M1");
    }

    /// 未授权的权限域：控制类 tool 被拒（在 schema 校验与执行之前）。
    #[test]
    fn ungranted_scope_is_forbidden() {
        let mut s = ready(shared(Scope::DEFAULT_GRANTED));
        let v = call(&mut s, "snow_shot_settings_update", json!({}));
        assert_eq!(v["error"]["code"], ERR_FORBIDDEN);
        assert_eq!(v["error"]["data"]["scope"], "control");
        let mut s = ready(shared(&[Scope::ReadOnly]));
        let v = call(&mut s, "snow_shot_screenshot_begin", json!({}));
        assert_eq!(v["error"]["code"], ERR_FORBIDDEN);
    }
}
