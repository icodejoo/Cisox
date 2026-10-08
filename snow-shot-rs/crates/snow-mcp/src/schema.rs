//! 极简 JSON Schema 子集校验：`type` / `properties` / `required` / `additionalProperties` / `enum` / `items`。
//!
//! 只覆盖 tool 入参用到的子集，不引入第三方 schema 库。

use serde_json::Value;

/// 按 schema 校验 `value`。
///
/// # 参数
/// - `schema`：schema 对象（缺失的关键字视为不限制）。
/// - `value`：待校验的值。
///
/// # 返回
/// 通过返回 `Ok(())`；否则返回 `路径: 原因`（路径以 `$` 起头）。
///
/// ```
/// use serde_json::json;
/// let schema = json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]});
/// assert!(snow_mcp::schema::validate(&schema, &json!({"a":"x"})).is_ok());
/// assert!(snow_mcp::schema::validate(&schema, &json!({})).is_err());
/// ```
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check(schema, value, "$")
}

/// 判断值是否符合 JSON Schema 的类型名。
fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

/// 递归校验。
fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if let Some(ty) = schema.get("type").and_then(Value::as_str)
        && !type_matches(ty, value)
    {
        return Err(format!("{path}: expected {ty}"));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        return Err(format!("{path}: not one of the allowed values"));
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{path}.{key}: required"));
                }
            }
        }
        for (key, item) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => check(sub, item, &format!("{path}.{key}"))?,
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path}.{key}: unknown property"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(sub)) = (value.as_array(), schema.get("items")) {
        for (index, item) in items.iter().enumerate() {
            check(sub, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 类型、必填、未知字段、枚举、数组元素。
    #[test]
    fn validates_subset() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "n": {"type": "integer"},
                "mode": {"enum": ["a", "b"]},
                "list": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["name"],
            "additionalProperties": false
        });
        assert!(
            validate(
                &schema,
                &json!({"name": "x", "n": 1, "mode": "a", "list": ["q"]})
            )
            .is_ok()
        );
        assert!(
            validate(&schema, &json!({"n": 1}))
                .unwrap_err()
                .contains("required")
        );
        assert!(
            validate(&schema, &json!({"name": 1}))
                .unwrap_err()
                .contains("expected string")
        );
        assert!(validate(&schema, &json!({"name": "x", "n": 1.5})).is_err());
        assert!(validate(&schema, &json!({"name": "x", "mode": "z"})).is_err());
        assert!(
            validate(&schema, &json!({"name": "x", "extra": 1}))
                .unwrap_err()
                .contains("unknown")
        );
        assert!(
            validate(&schema, &json!({"name": "x", "list": [1]}))
                .unwrap_err()
                .contains("[0]")
        );
        assert!(validate(&schema, &json!("str")).is_err());
    }

    /// 缺省关键字不限制。
    #[test]
    fn open_schema_accepts_anything_object() {
        let schema = json!({"type": "object"});
        assert!(validate(&schema, &json!({"any": [1, 2]})).is_ok());
    }
}
