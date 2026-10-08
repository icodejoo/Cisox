//! `global_mouse/*` 配置到手势绑定的翻译，以及手势动作对应的截图模式。纯逻辑，可离屏测试；
//! 钩子本身在 `snow-platform::global_mouse`，拖动事件的驱动在 `app_runtime`。

use crate::app_runtime::CaptureMode;
use crate::overlay_view::AutoConfirm;
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_platform::global_mouse::{Binding, Modifiers, MouseKey};

/// 手势动作标识（也是配置键 `global_mouse/<标识>` 的后半段）。
pub const ACTIONS: [&str; 7] = [
    "screenshot_copy",
    "screenshot_fixed",
    "screenshot_ocr",
    "screenshot_translation",
    "screenshot_quick_save",
    "screenshot_save",
    "screen_recording",
];

/// 解析一项 `{"activation_key": [...], "mouse_button": "..."}` 配置；空对象、缺字段、
/// 未知名字或没有修饰键（会劫持普通鼠标操作）都返回 `None`。
///
/// # 参数
/// - `action`：动作标识。
/// - `value`：配置值。
///
/// ```ignore
/// let value = serde_json::json!({"activation_key": ["windows"], "mouse_button": "left_drag"});
/// assert!(binding_from_value("screenshot_copy", &value).is_some());
/// ```
pub fn binding_from_value(action: &str, value: &Value) -> Option<Binding> {
    let keys: Vec<&str> = value.get("activation_key")?.as_array()?.iter().filter_map(Value::as_str).collect();
    let modifiers = Modifiers::parse(&keys)?;
    if modifiers.is_empty() {
        return None;
    }
    let button = MouseKey::parse(value.get("mouse_button")?.as_str()?)?;
    Some(Binding { action: action.to_string(), modifiers, button })
}

/// 读取全部有效的手势绑定。
///
/// # 参数
/// - `document`：配置文档。
pub fn bindings_from_document(document: &ConfigDocument) -> Vec<Binding> {
    ACTIONS
        .iter()
        .filter_map(|action| binding_from_value(action, &document.value(&format!("global_mouse/{action}"))))
        .collect()
}

/// 手势动作对应的截图模式；未知动作返回 `None`。
///
/// # 参数
/// - `action`：动作标识。
pub fn mode_for_action(action: &str) -> Option<CaptureMode> {
    Some(match action {
        "screenshot_copy" => CaptureMode::Quick(AutoConfirm::Copy),
        "screenshot_fixed" => CaptureMode::Quick(AutoConfirm::Pin),
        "screenshot_ocr" => CaptureMode::Quick(AutoConfirm::Ocr),
        "screenshot_translation" => CaptureMode::Quick(AutoConfirm::Translate),
        "screenshot_quick_save" => CaptureMode::Quick(AutoConfirm::QuickSave),
        "screenshot_save" => CaptureMode::Quick(AutoConfirm::Save),
        "screen_recording" => CaptureMode::Record,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 默认配置：Win+左 / 中 / 右键拖动分别对应复制 / 贴图 / OCR，其余四项未绑定。
    #[test]
    fn default_bindings() {
        let bindings = bindings_from_document(&ConfigDocument::from_bytes(None));
        assert_eq!(bindings.len(), 3);
        let find = |action: &str| bindings.iter().find(|b| b.action == action).unwrap();
        assert_eq!(find("screenshot_copy").button, MouseKey::Left);
        assert_eq!(find("screenshot_fixed").button, MouseKey::Middle);
        assert_eq!(find("screenshot_ocr").button, MouseKey::Right);
        assert!(bindings.iter().all(|b| b.modifiers.win && !b.modifiers.ctrl));
    }

    /// 无效配置不产生绑定：空对象、没有修饰键、未知键名、未知按键。
    #[test]
    fn invalid_values_are_rejected() {
        assert!(binding_from_value("a", &json!({})).is_none());
        assert!(binding_from_value("a", &json!({"activation_key": [], "mouse_button": "left_drag"})).is_none());
        assert!(binding_from_value("a", &json!({"activation_key": ["hyper"], "mouse_button": "left_drag"})).is_none());
        assert!(binding_from_value("a", &json!({"activation_key": ["alt"], "mouse_button": "double"})).is_none());
        let ok = binding_from_value("a", &json!({"activation_key": ["ctrl", "alt"], "mouse_button": "side_button_1_drag"})).unwrap();
        assert!(ok.modifiers.ctrl && ok.modifiers.alt && ok.button == MouseKey::Back);
    }

    /// 每个动作都能映射到截图模式。
    #[test]
    fn every_action_has_a_mode() {
        for action in ACTIONS {
            assert!(mode_for_action(action).is_some(), "{action}");
        }
        assert!(mode_for_action("nope").is_none());
    }
}
