//! 聚光灯 / 水印样式：从配置键 `drawing/spotlight_style`、`drawing/watermark_style` 读取并夹取范围。
//!
//! 键名、字段名与取值范围沿用旧版 `screenshotcanvastoolstyles.cpp`：颜色是 `#rrggbbaa`，
//! 水印文本与模板属于编辑会话，不进样式，所以这里只读外观字段。

use serde_json::Value;
use snow_draw_engine::{ColorRgba8, SpotlightConfig, WatermarkConfig};

use crate::annotation_style::parse_hex;

/// 水印样式配置键。
pub const WATERMARK_STYLE_KEY: &str = "drawing/watermark_style";
/// 聚光灯样式配置键。
pub const SPOTLIGHT_STYLE_KEY: &str = "drawing/spotlight_style";
/// 水印字号范围（画布像素）。
const WATERMARK_FONT_RANGE: (f64, f64) = (6.0, 512.0);
/// 水印角度范围（度）。
const WATERMARK_ANGLE_RANGE: (f64, f64) = (-90.0, 90.0);
/// 水印间距范围（画布像素）。
const WATERMARK_GAP_RANGE: (f64, f64) = (10.0, 200.0);
/// 不透明度范围。
const OPACITY_RANGE: (f64, f64) = (0.0, 1.0);

/// 读配置里的颜色（`#rrggbbaa` 或 `#rrggbb`），非法返回 `None`。
fn color_field(obj: &serde_json::Map<String, Value>) -> Option<ColorRgba8> {
    let [r, g, b, a] = parse_hex(obj.get("color")?.as_str()?)?;
    Some(ColorRgba8 { r, g, b, a })
}

/// 读数值字段并夹到范围；缺失或非有限数返回 `None`。
fn number_field(
    obj: &serde_json::Map<String, Value>,
    name: &str,
    range: (f64, f64),
) -> Option<f64> {
    obj.get(name)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(range.0, range.1))
}

/// 由配置 JSON 还原水印外观，缺字段或非法字段取 `base`（文本与模板也沿用 `base`）。
///
/// # 参数
/// - `value`：`drawing/watermark_style` 的值（非对象时整体取 `base`）。
/// - `base`：缺省值，通常是引擎当前水印配置。
///
/// # 返回
/// 夹取后的水印配置：字号 6..512、角度 -90..90、间距 10..200、不透明度 0..1。
///
/// ```ignore
/// let w = watermark_from_json(&json!({"angle": 999}), &WatermarkConfig::default());
/// assert_eq!(w.angle, 90.0);
/// ```
pub fn watermark_from_json(value: &Value, base: &WatermarkConfig) -> WatermarkConfig {
    let Some(obj) = value.as_object() else {
        return base.clone();
    };
    let mut out = base.clone();
    if let Some(color) = color_field(obj) {
        out.color = color;
    }
    if let Some(v) = number_field(obj, "font_size", WATERMARK_FONT_RANGE) {
        out.font_size = v;
    }
    if let Some(family) = obj.get("font_family").and_then(Value::as_str) {
        out.font_family = family.trim().to_owned();
    }
    if let Some(v) = number_field(obj, "angle", WATERMARK_ANGLE_RANGE) {
        out.angle = v;
    }
    if let Some(v) = number_field(obj, "gap", WATERMARK_GAP_RANGE) {
        out.gap = v;
    }
    if let Some(v) = number_field(obj, "opacity", OPACITY_RANGE) {
        out.opacity = v;
    }
    out
}

/// 由配置 JSON 还原聚光灯样式，缺字段或非法字段取 `base`。
///
/// # 参数
/// - `value`：`drawing/spotlight_style` 的值（非对象时整体取 `base`）。
/// - `base`：缺省值。
///
/// # 返回
/// 夹取后的聚光灯配置（不透明度 0..1）。
///
/// ```ignore
/// let s = spotlight_from_json(&json!({"opacity": 5}), &SpotlightConfig::default());
/// assert_eq!(s.opacity, 1.0);
/// ```
pub fn spotlight_from_json(value: &Value, base: &SpotlightConfig) -> SpotlightConfig {
    let Some(obj) = value.as_object() else {
        return *base;
    };
    let mut out = *base;
    if let Some(color) = color_field(obj) {
        out.color = color;
    }
    if let Some(v) = number_field(obj, "opacity", OPACITY_RANGE) {
        out.opacity = v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 水印字段夹到旧版范围，颜色按 `#rrggbbaa` 解析，文本沿用 base。
    #[test]
    fn watermark_fields_clamped_like_legacy() {
        let base = WatermarkConfig {
            text: "keep".into(),
            ..WatermarkConfig::default()
        };
        let w = watermark_from_json(
            &json!({
                "color": "#11223380",
                "font_size": 9999,
                "font_family": " Segoe UI ",
                "angle": -200,
                "gap": 1,
                "opacity": 2.5
            }),
            &base,
        );
        assert_eq!(
            w.color,
            ColorRgba8 {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 0x80
            }
        );
        assert_eq!(
            (w.font_size, w.angle, w.gap, w.opacity),
            (512.0, -90.0, 10.0, 1.0)
        );
        assert_eq!(w.font_family, "Segoe UI");
        assert_eq!(w.text, "keep");
    }

    /// 缺字段、非法字段、非对象都回落到 base。
    #[test]
    fn invalid_values_fall_back_to_base() {
        let base = WatermarkConfig::default();
        assert_eq!(watermark_from_json(&json!(null), &base), base);
        assert_eq!(watermark_from_json(&json!({}), &base), base);
        let w = watermark_from_json(&json!({"color": "red", "angle": "x", "gap": null}), &base);
        assert_eq!(w, base);
        let s = SpotlightConfig::default();
        assert_eq!(spotlight_from_json(&json!([1]), &s), s);
    }

    /// 聚光灯颜色与不透明度。
    #[test]
    fn spotlight_fields_parse() {
        let s = spotlight_from_json(
            &json!({"color": "#ff000040", "opacity": 0.3}),
            &SpotlightConfig::default(),
        );
        assert_eq!(
            s.color,
            ColorRgba8 {
                r: 255,
                g: 0,
                b: 0,
                a: 0x40
            }
        );
        assert_eq!(s.opacity, 0.3);
        let s = spotlight_from_json(&json!({"opacity": -1}), &SpotlightConfig::default());
        assert_eq!(s.opacity, 0.0);
    }
}
