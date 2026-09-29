//! 快捷键绑定规范化：`canonicalPortableText` 与 binding JSON 读写的移植。
//!
//! C++ 依赖 `QKeySequence::fromString(PortableText)` 解析按键；这里复刻其受限子集
//! （见 [`canonical_portable_text`]），仅覆盖非 macOS 分支。初稿由 antigravity 产出并经复审。

use crate::value::{Normalization, eq_ignore_case, json_eq, trimmed};
use serde_json::{Map, Value, json};

/// macOS 物理键码上限（含）。
const MACOS_KEY_MAX: u32 = 127;

/// 单个快捷键绑定：可移植文本 + 可选 macOS 物理键码。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortcutBinding {
    /// 可移植快捷键文本，例如 `Ctrl+C`；空串表示无效绑定。
    pub portable_text: String,
    /// 可选的 macOS 物理键码（0..=127）。
    pub macos_key: Option<u32>,
}

/// 受支持的具名按键（Qt PortableText 拼写）。
const NAMED_KEYS: &[&str] = &[
    "Space",
    "Esc",
    "Tab",
    "Backtab",
    "Backspace",
    "Return",
    "Enter",
    "Ins",
    "Del",
    "Pause",
    "Print",
    "SysReq",
    "Home",
    "End",
    "Left",
    "Up",
    "Right",
    "Down",
    "PgUp",
    "PgDown",
    "CapsLock",
    "NumLock",
    "ScrollLock",
    "Menu",
    "Help",
];

/// 功能键编号上限（F1..F35）。
const FUNCTION_KEY_MAX: u32 = 35;

/// 是否“词字符”（字母数字或下划线），用于 `\b` 词边界判定。
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 在词边界上大小写不敏感地替换，从左到右、非重叠，等价于 C++ 的 `\b(?:a|b)\b` 替换。
fn replace_words(text: &str, targets: &[&str], replacement: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut matched = false;
        for target in targets {
            let target_chars: Vec<char> = target.chars().collect();
            let end = i + target_chars.len();
            if end > chars.len() {
                continue;
            }
            let same = chars[i..end]
                .iter()
                .zip(&target_chars)
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
            let left_ok = i == 0 || !is_word_char(chars[i - 1]);
            let right_ok = end == chars.len() || !is_word_char(chars[end]);
            if same && left_ok && right_ok {
                result.push_str(replacement);
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

/// 把按键词规范化为 Qt PortableText 拼写；无法识别返回 `None`。
fn canonical_key(key: &str) -> Option<String> {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() == 1 {
        let c = chars[0];
        let mut upper = c.to_uppercase();
        return Some(match (upper.next(), upper.next()) {
            (Some(u), None) => u.to_string(),
            _ => c.to_string(),
        });
    }
    if let Some(number) = key.strip_prefix(['F', 'f'])
        && let Ok(number) = number.parse::<u32>()
        && (1..=FUNCTION_KEY_MAX).contains(&number)
    {
        let canonical = format!("F{number}");
        return eq_ignore_case(key, &canonical).then_some(canonical);
    }
    NAMED_KEYS
        .iter()
        .find(|name| eq_ignore_case(key, name))
        .map(|name| (*name).to_string())
}

/// 把快捷键文本规范化为 Qt PortableText（单键序列）。
///
/// 步骤：去空白 → 修饰键别名整词替换（control/command/option/windows 等）→
/// 可选的“仅 Shift” → 解析 `修饰键+按键` → 按 `Meta+Ctrl+Alt+Shift+Num+` 顺序输出。
/// 多段序列（含 `", "`）、仅修饰键、未知按键均为非法。
///
/// # 参数
/// - `text`：原始文本，例如 `" control+c "`
/// - `allow_modifier_only_shift`：是否允许单独的 `Shift`
///
/// # 返回
/// 规范文本；非法返回空串。
///
/// # 示例
/// ```
/// use snow_config::shortcut::canonical_portable_text;
///
/// assert_eq!(canonical_portable_text("cTrL+c", false), "Ctrl+C");
/// assert_eq!(canonical_portable_text("Ctrl+K, Ctrl+C", false), "");
/// ```
pub fn canonical_portable_text(text: &str, allow_modifier_only_shift: bool) -> String {
    let text = trimmed(text);
    if text.is_empty() {
        return String::new();
    }
    let mut replaced = replace_words(text, &["control", "ctrl"], "Ctrl");
    replaced = replace_words(&replaced, &["command", "cmd"], "Ctrl");
    replaced = replace_words(&replaced, &["option"], "Alt");
    replaced = replace_words(&replaced, &["alt"], "Alt");
    replaced = replace_words(&replaced, &["shift"], "Shift");
    replaced = replace_words(&replaced, &["windows", "win", "super", "meta"], "Meta");
    replaced = replace_words(&replaced, &["num"], "Num");
    if allow_modifier_only_shift && eq_ignore_case(&replaced, "Shift") {
        return "Shift".to_string();
    }
    if replaced.contains(", ") {
        return String::new();
    }

    // 以 '+' 分词；以 '+' 结尾时按键就是 '+'
    let (modifier_part, key_part) = match replaced.strip_suffix('+') {
        Some(rest) => match rest.strip_suffix('+') {
            Some(modifiers) => (modifiers, "+"),
            None if rest.is_empty() => ("", "+"),
            None => return String::new(),
        },
        None => match replaced.rfind('+') {
            Some(index) => (&replaced[..index], &replaced[index + 1..]),
            None => ("", replaced.as_str()),
        },
    };

    let (mut meta, mut ctrl, mut alt, mut shift, mut num) = (false, false, false, false, false);
    if !modifier_part.is_empty() {
        for token in modifier_part.split('+') {
            let token = token.trim();
            if eq_ignore_case(token, "Ctrl") {
                ctrl = true;
            } else if eq_ignore_case(token, "Alt") {
                alt = true;
            } else if eq_ignore_case(token, "Shift") {
                shift = true;
            } else if eq_ignore_case(token, "Meta") {
                meta = true;
            } else if eq_ignore_case(token, "Num") {
                num = true;
            } else {
                return String::new();
            }
        }
    }
    let Some(key) = canonical_key(key_part.trim()) else {
        return String::new();
    };

    let mut result = String::new();
    for (flag, name) in [
        (meta, "Meta+"),
        (ctrl, "Ctrl+"),
        (alt, "Alt+"),
        (shift, "Shift+"),
        (num, "Num+"),
    ] {
        if flag {
            result.push_str(name);
        }
    }
    result.push_str(&key);
    result
}

/// 规范化绑定：规范化文本并只保留合法的 macOS 物理键码；文本非法则返回默认（空）绑定。
///
/// # 参数
/// - `binding`：原始绑定
/// - `allow_modifier_only_shift`：是否允许单独的 `Shift`
///
/// # 示例
/// ```
/// use snow_config::shortcut::{ShortcutBinding, canonical_binding};
///
/// let binding = ShortcutBinding { portable_text: "cmd+a".into(), macos_key: Some(8) };
/// let canonical = canonical_binding(&binding, false);
/// assert_eq!(canonical.portable_text, "Ctrl+A");
/// assert_eq!(canonical.macos_key, Some(8));
/// ```
pub fn canonical_binding(
    binding: &ShortcutBinding,
    allow_modifier_only_shift: bool,
) -> ShortcutBinding {
    let portable_text = canonical_portable_text(&binding.portable_text, allow_modifier_only_shift);
    if portable_text.is_empty() {
        return ShortcutBinding::default();
    }
    let macos_key = binding
        .macos_key
        .filter(|key| *key <= MACOS_KEY_MAX && portable_text != "Shift");
    ShortcutBinding {
        portable_text,
        macos_key,
    }
}

/// 绑定转 JSON：`{"portable": ..., "physical_keys": {"macos": n}}`（物理键仅在合法时输出）。
///
/// # 示例
/// ```
/// use snow_config::shortcut::{ShortcutBinding, shortcut_binding_to_json};
///
/// let binding = ShortcutBinding { portable_text: "Ctrl+C".into(), macos_key: None };
/// assert_eq!(shortcut_binding_to_json(&binding)["portable"], "Ctrl+C");
/// ```
pub fn shortcut_binding_to_json(binding: &ShortcutBinding) -> Value {
    let mut object = Map::new();
    object.insert(
        "portable".to_string(),
        Value::String(binding.portable_text.clone()),
    );
    if let Some(key) = binding.macos_key.filter(|key| *key <= MACOS_KEY_MAX) {
        let mut physical = Map::new();
        physical.insert("macos".to_string(), json!(key));
        object.insert("physical_keys".to_string(), Value::Object(physical));
    }
    Value::Object(object)
}

/// 从 JSON 解析单个绑定（接受旧版字符串与新版对象）。
///
/// # 返回
/// `(绑定, valid, changed)`：`valid` 为假表示应丢弃；`changed` 表示规范化结果与输入不同。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::shortcut::shortcut_binding_from_json;
///
/// let (binding, valid, changed) = shortcut_binding_from_json(&json!("ctrl+c"), false);
/// assert_eq!(binding.portable_text, "Ctrl+C");
/// assert!(valid && changed);
/// ```
pub fn shortcut_binding_from_json(
    value: &Value,
    allow_modifier_only_shift: bool,
) -> (ShortcutBinding, bool, bool) {
    let mut changed: bool;
    let mut candidate = ShortcutBinding::default();
    if let Some(text) = value.as_str() {
        candidate.portable_text = text.to_string();
        changed = true;
    } else if let Some(object) = value.as_object() {
        let Some(portable) = object.get("portable").and_then(Value::as_str) else {
            return (ShortcutBinding::default(), false, true);
        };
        candidate.portable_text = portable.to_string();
        changed = object.len()
            > if object.contains_key("physical_keys") {
                2
            } else {
                1
            };
        if let Some(physical_value) = object.get("physical_keys") {
            if let Some(physical) = physical_value.as_object() {
                let mac_value = physical.get("macos");
                if let Some(mac_value) = mac_value {
                    match mac_value.as_f64() {
                        Some(number)
                            if (0.0..=f64::from(MACOS_KEY_MAX)).contains(&number)
                                && number.floor() == number =>
                        {
                            candidate.macos_key = Some(number as u32);
                        }
                        _ => changed = true,
                    }
                }
                if physical.len() != usize::from(mac_value.is_some()) {
                    changed = true;
                }
            } else {
                changed = true;
            }
        }
    } else {
        return (ShortcutBinding::default(), false, true);
    }

    let result = canonical_binding(&candidate, allow_modifier_only_shift);
    if result.portable_text.is_empty() {
        return (ShortcutBinding::default(), false, true);
    }
    changed = changed || result != candidate || !json_eq(&shortcut_binding_to_json(&result), value);
    (result, true, changed)
}

/// 绑定列表转 JSON 数组（跳过空文本项）。
///
/// # 示例
/// ```
/// use snow_config::shortcut::{ShortcutBinding, shortcut_bindings_to_json};
///
/// let list = [ShortcutBinding { portable_text: "Ctrl+C".into(), macos_key: None }];
/// assert!(shortcut_bindings_to_json(&list).is_array());
/// ```
pub fn shortcut_bindings_to_json(bindings: &[ShortcutBinding]) -> Value {
    Value::Array(
        bindings
            .iter()
            .filter(|binding| !binding.portable_text.is_empty())
            .map(shortcut_binding_to_json)
            .collect(),
    )
}

/// 从 JSON 数组解析绑定列表：丢弃非法项、重复（冲突）项与超出上限的项。
///
/// # 参数
/// - `value`：JSON 数组
/// - `allow_modifier_only_shift`：是否允许单独的 `Shift`
/// - `maximum_items`：条目上限，`None` 表示不限
///
/// # 返回
/// `(绑定列表, valid, changed)`；输入不是数组时 `valid=false`。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::shortcut::shortcut_bindings_from_json;
///
/// let (list, valid, _) = shortcut_bindings_from_json(&json!(["ctrl+c"]), false, Some(1));
/// assert!(valid && list.len() == 1);
/// ```
pub fn shortcut_bindings_from_json(
    value: &Value,
    allow_modifier_only_shift: bool,
    maximum_items: Option<usize>,
) -> (Vec<ShortcutBinding>, bool, bool) {
    let Some(items) = value.as_array() else {
        return (Vec::new(), false, false);
    };
    let mut result: Vec<ShortcutBinding> = Vec::new();
    let mut changed = false;
    for item in items {
        let (binding, item_valid, item_changed) =
            shortcut_binding_from_json(item, allow_modifier_only_shift);
        let duplicate = result
            .iter()
            .any(|existing| bindings_conflict(existing, &binding));
        let over_limit = maximum_items.is_some_and(|max| result.len() >= max);
        if !item_valid || duplicate || over_limit {
            changed = true;
            continue;
        }
        result.push(binding);
        changed = changed || item_changed;
    }
    changed = changed || !json_eq(&shortcut_bindings_to_json(&result), value);
    (result, true, changed)
}

/// 非 macOS 的绑定冲突判定：两者规范文本相同且非空即冲突（物理键码不参与）。
///
/// # 示例
/// ```
/// use snow_config::shortcut::{ShortcutBinding, bindings_conflict};
///
/// let a = ShortcutBinding { portable_text: "Ctrl+C".into(), macos_key: None };
/// let b = ShortcutBinding { portable_text: "control+c".into(), macos_key: None };
/// assert!(bindings_conflict(&a, &b));
/// ```
pub fn bindings_conflict(a: &ShortcutBinding, b: &ShortcutBinding) -> bool {
    let first = canonical_portable_text(&a.portable_text, true);
    let second = canonical_portable_text(&b.portable_text, true);
    !first.is_empty() && first == second
}

/// 快捷键列表键的规范化（C++ `normalizeShortcuts`）。
///
/// # 参数
/// - `value`：待规范化的 JSON
/// - `maximum_items`：条目上限
/// - `allow_modifier_only_shift`：是否允许单独的 `Shift`（仅 `screenshot_shortcuts/*`）
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::shortcut::normalize_shortcuts;
///
/// let norm = normalize_shortcuts(&json!(["NotARealKey"]), Some(2), false);
/// assert!(norm.valid && norm.changed);
/// ```
pub fn normalize_shortcuts(
    value: &Value,
    maximum_items: Option<usize>,
    allow_modifier_only_shift: bool,
) -> Normalization {
    let (bindings, valid, changed) =
        shortcut_bindings_from_json(value, allow_modifier_only_shift, maximum_items);
    Normalization {
        value: shortcut_bindings_to_json(&bindings),
        valid,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 别名与规范化（C++ window_shortcut_manager_tests 的黄金用例），重复两遍验证无状态。
    #[test]
    fn canonical_text_matches_cpp_aliases() {
        let cases = [
            (" control+c ", "Ctrl+C"),
            ("cTrL+c", "Ctrl+C"),
            ("COMMAND+c", "Ctrl+C"),
            ("cmd+c", "Ctrl+C"),
            ("oPtIoN+a", "Alt+A"),
            ("ALT+a", "Alt+A"),
            ("sHiFt+a", "Shift+A"),
            ("WINDOWS+a", "Meta+A"),
            ("win+a", "Meta+A"),
            ("SuPeR+a", "Meta+A"),
            ("meta+a", "Meta+A"),
            ("nUm+1", "Num+1"),
            ("command+option+shift+a", "Ctrl+Alt+Shift+A"),
            ("Ctrl++", "Ctrl++"),
            ("Ctrl+Num+1", "Ctrl+Num+1"),
        ];
        for _ in 0..2 {
            for (input, expected) in cases {
                assert_eq!(
                    canonical_portable_text(input, false),
                    expected,
                    "输入 {input:?}"
                );
            }
        }
    }

    /// 默认快捷键、单段限制与仅 Shift 策略。
    #[test]
    fn canonical_text_defaults_and_rejections() {
        for same in [
            "Shift+Esc",
            "Esc",
            ",",
            ".",
            "Shift+=",
            "Ctrl+Shift+S",
            "Up",
            "Space",
            "Alt+Shift+Tab",
            "F1",
            "Ctrl+F1",
            "Ctrl+Esc",
            "Alt+R",
            "1",
            "V",
        ] {
            assert_eq!(canonical_portable_text(same, false), same);
        }
        for bad in [
            "Ctrl+K, Ctrl+C",
            "Ctrl",
            "NotARealKey",
            "Shift",
            "Ctrl+",
            "",
            "  ",
            "Ctrl+Shift",
        ] {
            assert_eq!(canonical_portable_text(bad, false), "", "输入 {bad:?}");
        }
        assert_eq!(canonical_portable_text("Shift", true), "Shift");
        assert_eq!(canonical_portable_text("Ctrl+Shift", true), "");
    }

    /// 非法/越界项被丢弃：畸形快捷键规范化后为空数组且 changed。
    #[test]
    fn malformed_shortcuts_are_removed() {
        for malformed in ["Ctrl+K, Ctrl+C", "Ctrl", "NotARealKey"] {
            let out = normalize_shortcuts(&json!([malformed]), Some(2), false);
            assert!(out.valid && out.changed);
            assert_eq!(out.value, json!([]));
        }
        assert!(!normalize_shortcuts(&json!("Ctrl+C"), Some(2), false).valid);
    }

    /// v2 修复：保留可移植回退，丢弃畸形元数据与条目（C++ shortcutSchemaMigration...）。
    #[test]
    fn physical_metadata_is_repaired() {
        let input = json!([
            {"portable": "Ctrl+C", "physical_keys": {"macos": 8, "future": 99}},
            {"portable": "Alt+X", "physical_keys": {"macos": 128}},
            {"portable": "Ctrl+K, Ctrl+C"}
        ]);
        let out = normalize_shortcuts(&input, Some(2), false);
        assert!(out.valid && out.changed);
        let expected = json!([
            {"portable": "Ctrl+C", "physical_keys": {"macos": 8}},
            {"portable": "Alt+X"}
        ]);
        assert_eq!(out.value, expected);
        let again = normalize_shortcuts(&out.value, Some(2), false);
        assert!(again.valid && !again.changed && again.value == expected);
    }

    /// 旧版字符串格式升级为对象，超出上限的项被截断。
    #[test]
    fn legacy_strings_upgrade_and_limit() {
        let out = normalize_shortcuts(&json!(["Ctrl+Alt+K"]), Some(2), false);
        assert!(out.valid && out.changed);
        assert_eq!(out.value, json!([{"portable": "Ctrl+Alt+K"}]));
        let out = normalize_shortcuts(&json!(["A", "B", "C"]), Some(2), false);
        assert_eq!(out.value, json!([{"portable": "A"}, {"portable": "B"}]));
        let dup = normalize_shortcuts(&json!(["ctrl+c", "Ctrl+C"]), Some(2), false);
        assert_eq!(dup.value, json!([{"portable": "Ctrl+C"}]));
    }

    /// 绑定冲突：物理键码不参与，空/非法项不冲突。
    #[test]
    fn conflict_rules() {
        let a = ShortcutBinding {
            portable_text: "Ctrl+C".into(),
            macos_key: Some(8),
        };
        let b = ShortcutBinding {
            portable_text: "Ctrl+C".into(),
            macos_key: None,
        };
        let c = ShortcutBinding {
            portable_text: "Ctrl+V".into(),
            macos_key: None,
        };
        assert!(bindings_conflict(&a, &b));
        assert!(!bindings_conflict(&a, &c));
        assert!(!bindings_conflict(
            &ShortcutBinding::default(),
            &ShortcutBinding::default()
        ));
    }
}
