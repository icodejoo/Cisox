//! 录屏输入特效的配置读取：把 `screen_recording/*` 里的轨迹、点击、高亮、按键回显设置翻成协议里的
//! [`EffectsRequest`]。颜色在设置里是 `#RRGGBB` / `#RRGGBBAA` 文本，缺失或非法按「关闭」处理。

use crate::pinned_model::parse_hex_color;
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_recorder_protocol::EffectsRequest;

/// 鼠标轨迹颜色。
pub const KEY_TRAIL_COLOR: &str = "screen_recording/mouse_trail_color";
/// 点击波纹颜色。
pub const KEY_CLICK_COLOR: &str = "screen_recording/mouse_click_color";
/// 鼠标轨迹时长（毫秒）。
pub const KEY_TRAIL_DURATION: &str = "screen_recording/mouse_trail_duration_ms";
/// 按键回显键帽大小。
pub const KEY_KEYBOARD_SIZE: &str = "screen_recording/keyboard_size";
/// 按键回显背景色。
pub const KEY_KEYBOARD_BACKGROUND: &str = "screen_recording/keyboard_background_color";
/// 按键回显文字色。
pub const KEY_KEYBOARD_FOREGROUND: &str = "screen_recording/keyboard_foreground_color";
/// 是否显示按键回显。
pub const KEY_SHOW_KEYBOARD: &str = "screen_recording/show_keyboard";
/// 是否启用鼠标高亮。
pub const KEY_HIGHLIGHT_ENABLED: &str = "screen_recording/mouse_highlight_enabled";
/// 是否录制鼠标点击。
pub const KEY_RECORD_CLICKS: &str = "screen_recording/record_mouse_clicks";
/// 鼠标高亮颜色。
pub const KEY_HIGHLIGHT_COLOR: &str = "screen_recording/mouse_highlight_color";

/// 读取 `#RRGGBBAA` 颜色设置；缺失或非法返回 `None`。
fn color_of(document: &ConfigDocument, key: &str) -> Option<[u8; 4]> {
    let rgba = parse_hex_color(document.value(key).as_str()?)?;
    Some(rgba.to_be_bytes())
}

/// 读取布尔设置；类型不对按 `false`。
fn flag_of(document: &ConfigDocument, key: &str) -> bool {
    matches!(document.value(key), Value::Bool(true))
}

/// 读取整数设置并夹到范围内；缺失或类型不对取默认值。
fn int_of(document: &ConfigDocument, key: &str, default: u32, min: u32, max: u32) -> u32 {
    document
        .value(key)
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .map_or(default, |n| n.clamp(min, max))
}

/// 由配置构造录屏输入特效请求。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 特效请求；对应开关关闭或颜色全透明的特效不会被启用。
///
/// ```ignore
/// let effects = effects_request(&document);
/// assert!(!effects.enabled()); // 默认全关
/// ```
pub fn effects_request(document: &ConfigDocument) -> EffectsRequest {
    let defaults = EffectsRequest::default();
    EffectsRequest {
        trail: color_of(document, KEY_TRAIL_COLOR).unwrap_or(defaults.trail),
        trail_ms: int_of(document, KEY_TRAIL_DURATION, defaults.trail_ms, 100, 2000),
        click: color_of(document, KEY_CLICK_COLOR).unwrap_or(defaults.click),
        highlight: if flag_of(document, KEY_HIGHLIGHT_ENABLED) {
            color_of(document, KEY_HIGHLIGHT_COLOR).unwrap_or(defaults.highlight)
        } else {
            defaults.highlight
        },
        record_clicks: flag_of(document, KEY_RECORD_CLICKS),
        keyboard: flag_of(document, KEY_SHOW_KEYBOARD),
        keyboard_size: int_of(document, KEY_KEYBOARD_SIZE, defaults.keyboard_size, 32, 128),
        keyboard_background: color_of(document, KEY_KEYBOARD_BACKGROUND).unwrap_or(defaults.keyboard_background),
        keyboard_text: color_of(document, KEY_KEYBOARD_FOREGROUND).unwrap_or(defaults.keyboard_text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 默认设置下所有特效关闭，颜色与尺寸取旧版默认值。
    #[test]
    fn defaults_keep_everything_off() {
        let doc = ConfigDocument::from_bytes(None);
        let e = effects_request(&doc);
        assert!(!e.enabled());
        assert_eq!(e.trail_ms, 500);
        assert_eq!(e.keyboard_size, 64);
        assert_eq!(e.keyboard_background, [0, 0, 0, 0xCC]);
        assert_eq!(e.keyboard_text, [0xFF; 4]);
    }

    /// 设置映射到请求：开关与颜色生效，高亮需要先开启，数值被夹到范围内。
    #[test]
    fn settings_map_to_request() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_TRAIL_COLOR, json!("#FF000080")).unwrap();
        doc.set_value(KEY_CLICK_COLOR, json!("#00FF00FF")).unwrap();
        doc.set_value(KEY_HIGHLIGHT_COLOR, json!("#0000FFFF")).unwrap();
        doc.set_value(KEY_RECORD_CLICKS, json!(true)).unwrap();
        doc.set_value(KEY_SHOW_KEYBOARD, json!(true)).unwrap();
        let e = effects_request(&doc);
        assert_eq!(e.trail, [255, 0, 0, 128]);
        assert_eq!(e.click, [0, 255, 0, 255]);
        assert_eq!(e.highlight, [0; 4], "高亮开关未开，颜色不生效");
        assert!(e.record_clicks && e.keyboard && e.enabled());

        doc.set_value(KEY_HIGHLIGHT_ENABLED, json!(true)).unwrap();
        assert_eq!(effects_request(&doc).highlight, [0, 0, 255, 255]);
        assert!(doc.set_value(KEY_TRAIL_COLOR, json!("not a colour")).is_err(), "非法颜色应被配置层拒绝");
        assert_eq!(effects_request(&doc).trail, [255, 0, 0, 128], "被拒绝的写入不改变已有值");
    }
}
