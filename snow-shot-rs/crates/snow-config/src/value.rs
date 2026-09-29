//! JSON 值辅助：规范化结果类型、Qt 语义的整数判定与相等比较、字符串辅助。
//!
//! 与 C++ 侧 `QJsonValue` 行为对齐：数字一律按 double 处理，`3` 与 `3.0` 相等。

use serde_json::{Number, Value};

/// 单个键的规范化结果，对应 C++ 的 `ConfigurationNormalization`。
#[derive(Debug, Clone, PartialEq)]
pub struct Normalization {
    /// 规范化后的值；`valid` 为假时无意义（为 `Null`）。
    pub value: Value,
    /// 输入是否合法；为假时调用方应回退默认值。
    pub valid: bool,
    /// 规范化结果与输入是否不同（需要回写磁盘）。
    pub changed: bool,
}

impl Normalization {
    /// 构造“非法”结果。
    pub fn invalid() -> Self {
        Self {
            value: Value::Null,
            valid: false,
            changed: false,
        }
    }

    /// 构造“合法”结果。
    ///
    /// # 参数
    /// - `value`：规范化后的值
    /// - `changed`：是否与输入不同
    pub fn ok(value: Value, changed: bool) -> Self {
        Self {
            value,
            valid: true,
            changed,
        }
    }
}

/// 由 i64 构造 JSON 整数。
pub fn int_value(value: i64) -> Value {
    Value::Number(Number::from(value))
}

/// 判断 JSON 值是否为落在 i32 范围内的有限整数（含 `3.0` 这种整数值浮点）。
///
/// 对应 C++ `isInteger`。
///
/// # 返回
/// 合法则返回对应的 i32，否则 `None`。
pub fn as_integer(value: &Value) -> Option<i32> {
    let number = value.as_f64()?;
    if !number.is_finite() || number.floor() != number {
        return None;
    }
    if number < f64::from(i32::MIN) || number > f64::from(i32::MAX) {
        return None;
    }
    Some(number as i32)
}

/// 对应 `QJsonValue::toInt(defaultValue)`：整数值且在 i32 范围内则返回，否则返回默认值。
pub fn to_int_or(value: &Value, default: i32) -> i32 {
    as_integer(value).unwrap_or(default)
}

/// Qt 语义的 JSON 相等：数字按 f64 比较，对象与数组递归比较。
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|other| json_eq(v, other)))
        }
        _ => a == b,
    }
}

/// 对应 `QString::trimmed()`：去除首尾空白字符。
pub fn trimmed(text: &str) -> &str {
    text.trim()
}

/// 对应 `QString::compare(..., Qt::CaseInsensitive) == 0`。
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// 对应 `QString::toCaseFolded()`（用小写近似）。
pub fn case_folded(text: &str) -> String {
    text.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 整数判定：整数值浮点合法，小数、越界、非数字非法。
    #[test]
    fn integer_detection() {
        assert_eq!(as_integer(&json!(30)), Some(30));
        assert_eq!(as_integer(&json!(30.0)), Some(30));
        assert_eq!(as_integer(&json!(30.5)), None);
        assert_eq!(as_integer(&json!(2147483648_i64)), None);
        assert_eq!(as_integer(&json!("30")), None);
        assert_eq!(to_int_or(&json!(1.5), -1), -1);
    }

    /// 数字 3 与 3.0 相等（QJsonValue 语义）。
    #[test]
    fn numeric_equality_ignores_representation() {
        assert!(json_eq(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
        assert!(!json_eq(&json!(1), &json!("1")));
    }
}
