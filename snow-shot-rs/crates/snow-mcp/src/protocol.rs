//! JSON-RPC 2.0 消息解析与应答构造（只管线格式，不含业务）。

use serde_json::{Value, json};

/// MCP 协议版本（沿用旧版能力清单）。
pub const PROTOCOL_VERSION: &str = "2026-07-28";
/// 解析错误。
pub const ERR_PARSE: i64 = -32700;
/// 非法请求。
pub const ERR_INVALID_REQUEST: i64 = -32600;
/// 方法不存在。
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
/// 参数非法（含未知 tool、schema 校验失败、协议版本不一致）。
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// 未鉴权（不回显细节）。
pub const ERR_UNAUTHORIZED: i64 = -32001;
/// 尚未完成 `initialize`。
pub const ERR_NOT_INITIALIZED: i64 = -32002;
/// 权限域未授权。
pub const ERR_FORBIDDEN: i64 = -32003;

/// 一条解析后的 JSON-RPC 请求或通知。
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// 请求 id；`None` 表示通知（不回应）。
    pub id: Option<Value>,
    /// 方法名。
    pub method: String,
    /// 参数（缺省为 `null`）。
    pub params: Value,
}

/// 解析失败：带应回的错误码与消息，以及（若能取到）请求 id。
#[derive(Debug, Clone, PartialEq)]
pub struct ParseFailure {
    /// 错误码。
    pub code: i64,
    /// 错误消息。
    pub message: &'static str,
    /// 已取到的 id，取不到为 `Value::Null`。
    pub id: Value,
}

/// 解析一行文本为请求。
///
/// # 参数
/// - `line`：一行 JSON。
///
/// # 返回
/// 请求；格式不合法返回 [`ParseFailure`]（批量请求一律视为非法）。
///
/// ```
/// let r = snow_mcp::protocol::parse_request(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
/// assert_eq!(r.method, "ping");
/// ```
pub fn parse_request(line: &str) -> Result<Request, ParseFailure> {
    let value: Value = serde_json::from_str(line).map_err(|_| ParseFailure {
        code: ERR_PARSE,
        message: "parse error",
        id: Value::Null,
    })?;
    let invalid = |id: Value| ParseFailure {
        code: ERR_INVALID_REQUEST,
        message: "invalid request",
        id,
    };
    let Value::Object(mut map) = value else {
        return Err(invalid(Value::Null));
    };
    let id = map.remove("id");
    let id_for_error = id.clone().unwrap_or(Value::Null);
    let id_ok = matches!(&id, None | Some(Value::String(_)) | Some(Value::Number(_)));
    if map.get("jsonrpc") != Some(&json!("2.0")) || !id_ok {
        return Err(invalid(id_for_error));
    }
    let Some(Value::String(method)) = map.remove("method") else {
        return Err(invalid(id_for_error));
    };
    Ok(Request {
        id,
        method,
        params: map.remove("params").unwrap_or(Value::Null),
    })
}

/// 构造成功应答文本。
///
/// # 参数
/// - `id`：请求 id。
/// - `result`：结果对象。
pub fn ok_response(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// 构造错误应答文本。
///
/// # 参数
/// - `id`：请求 id（取不到传 `Value::Null`）。
/// - `code`：错误码。
/// - `message`：简短英文消息（面向协议对端，不含内部细节）。
/// - `data`：可选附加数据。
pub fn error_response(id: &Value, code: i64, message: &str, data: Option<Value>) -> String {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法请求与通知。
    #[test]
    fn parses_request_and_notification() {
        let r =
            parse_request(r#"{"jsonrpc":"2.0","id":"a","method":"x","params":{"k":1}}"#).unwrap();
        assert_eq!(r.id, Some(json!("a")));
        assert_eq!(r.params, json!({"k": 1}));
        let n = parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert_eq!(n.id, None);
        assert_eq!(n.params, Value::Null);
    }

    /// 非 JSON、缺版本、批量、缺方法、非法 id 都被拒。
    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_request("{oops").unwrap_err().code, ERR_PARSE);
        for bad in [
            r#"{"id":1,"method":"x"}"#,
            r#"[{"jsonrpc":"2.0","id":1,"method":"x"}]"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":{"a":1},"method":"x"}"#,
            r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
        ] {
            assert_eq!(
                parse_request(bad).unwrap_err().code,
                ERR_INVALID_REQUEST,
                "{bad}"
            );
        }
        // 能取到 id 时随错误回带
        assert_eq!(
            parse_request(r#"{"jsonrpc":"2.0","id":7}"#).unwrap_err().id,
            json!(7)
        );
    }

    /// 应答构造含固定字段。
    #[test]
    fn builds_responses() {
        let ok: Value = serde_json::from_str(&ok_response(&json!(1), json!({"a": 1}))).unwrap();
        assert_eq!(ok["result"]["a"], 1);
        let err: Value =
            serde_json::from_str(&error_response(&Value::Null, -1, "m", Some(json!(2)))).unwrap();
        assert_eq!(err["error"]["data"], 2);
        assert_eq!(err["id"], Value::Null);
    }
}
