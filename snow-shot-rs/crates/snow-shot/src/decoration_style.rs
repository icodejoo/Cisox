//! 聚光灯 / 水印样式：从配置键 `drawing/spotlight_style`、`drawing/watermark_style` 读取并夹取范围。
//!
//! 键名、字段名与取值范围沿用旧版 `screenshotcanvastoolstyles.cpp`：颜色是 `#rrggbbaa`，
//! 水印文本与模板属于编辑会话，不进样式，所以这里只读外观字段。

use serde_json::{Value, json};
use snow_draw_engine::{ColorRgba8, SpotlightConfig, WatermarkConfig};

use crate::annotation_style::{format_hex, parse_hex};

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

/// 水印文本最大字节数（旧版上限）。
pub const WATERMARK_TEXT_MAX_BYTES: usize = 256;
/// 水印字号档位（画布像素，含旧版的 12 / 16 / 24 / 30）。
pub const WATERMARK_FONT_PRESETS: [u32; 8] = [12, 16, 24, 30, 48, 72, 96, 128];
/// 水印不透明度档位（百分比）。
pub const WATERMARK_OPACITY_PRESETS: [u32; 10] = [4, 8, 12, 16, 24, 32, 48, 64, 80, 100];
/// 聚光灯不透明度档位（百分比，旧版默认 64）。
pub const SPOTLIGHT_OPACITY_PRESETS: [u32; 8] = [16, 32, 48, 64, 72, 80, 88, 96];
/// 水印旋转角度档位（度）。
pub const WATERMARK_ANGLE_PRESETS: [i32; 9] = [-60, -45, -30, -15, 0, 15, 30, 45, 60];
/// 水印间距档位（画布像素）。
pub const WATERMARK_GAP_PRESETS: [u32; 8] = [10, 20, 32, 56, 80, 120, 160, 200];

/// 对水印配置的一次编辑（面板控件产生）。
#[derive(Debug, Clone, PartialEq)]
pub enum WatermarkEdit {
    /// 换颜色的 RGB，保留原透明度通道。
    ColorRgb([u8; 3]),
    /// 水印文本（超长按字节上限截断，首尾空白去掉）。
    Text(String),
    /// 字号（画布像素）。
    FontSize(f64),
    /// 不透明度（0..1）。
    Opacity(f64),
    /// 旋转角度（度）。
    Angle(f64),
    /// 平铺间距（画布像素）。
    Gap(f64),
}

/// 对聚光灯样式的一次编辑。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpotlightEdit {
    /// 换颜色的 RGB，保留原透明度通道。
    ColorRgb([u8; 3]),
    /// 不透明度（0..1）。
    Opacity(f64),
}

/// 按字节上限截断文本，不切断字符。
///
/// # 参数
/// - `text`：原文本。
/// - `max_bytes`：最大字节数。
fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 应用一次水印编辑并按旧版范围夹取。
///
/// # 参数
/// - `base`：当前水印配置。
/// - `edit`：控件产生的编辑。
///
/// # 返回
/// 编辑后的配置：字号 6..512、角度 -90..90、间距 10..200、不透明度 0..1，文本 ≤256 字节。
///
/// ```ignore
/// let w = apply_watermark_edit(&WatermarkConfig::default(), WatermarkEdit::Angle(500.0));
/// assert_eq!(w.angle, 90.0);
/// ```
pub fn apply_watermark_edit(base: &WatermarkConfig, edit: WatermarkEdit) -> WatermarkConfig {
    let mut out = base.clone();
    let clamp = |v: f64, range: (f64, f64)| {
        if v.is_finite() {
            v.clamp(range.0, range.1)
        } else {
            range.0
        }
    };
    match edit {
        WatermarkEdit::ColorRgb([r, g, b]) => {
            out.color = ColorRgba8 {
                r,
                g,
                b,
                a: out.color.a,
            }
        }
        WatermarkEdit::Text(text) => {
            out.text = truncate_bytes(text.trim(), WATERMARK_TEXT_MAX_BYTES).to_owned();
        }
        WatermarkEdit::FontSize(v) => out.font_size = clamp(v, WATERMARK_FONT_RANGE),
        WatermarkEdit::Opacity(v) => out.opacity = clamp(v, OPACITY_RANGE),
        WatermarkEdit::Angle(v) => out.angle = clamp(v, WATERMARK_ANGLE_RANGE),
        WatermarkEdit::Gap(v) => out.gap = clamp(v, WATERMARK_GAP_RANGE),
    }
    out
}

/// 应用一次聚光灯编辑并夹取。
///
/// # 参数
/// - `base`：当前聚光灯样式。
/// - `edit`：控件产生的编辑。
pub fn apply_spotlight_edit(base: &SpotlightConfig, edit: SpotlightEdit) -> SpotlightConfig {
    let mut out = *base;
    match edit {
        SpotlightEdit::ColorRgb([r, g, b]) => {
            out.color = ColorRgba8 {
                r,
                g,
                b,
                a: out.color.a,
            }
        }
        SpotlightEdit::Opacity(v) => {
            out.opacity = if v.is_finite() {
                v.clamp(OPACITY_RANGE.0, OPACITY_RANGE.1)
            } else {
                0.0
            };
        }
    }
    out
}

/// 水印外观序列化为 `drawing/watermark_style` 的值（文本属于编辑会话，不写入）。
///
/// # 参数
/// - `config`：水印配置。
pub fn watermark_to_json(config: &WatermarkConfig) -> Value {
    json!({
        "color": format_hex([config.color.r, config.color.g, config.color.b, config.color.a]),
        "font_size": config.font_size,
        "font_family": config.font_family,
        "angle": config.angle,
        "gap": config.gap,
        "opacity": config.opacity,
    })
}

/// 聚光灯样式序列化为 `drawing/spotlight_style` 的值。
///
/// # 参数
/// - `config`：聚光灯样式。
pub fn spotlight_to_json(config: &SpotlightConfig) -> Value {
    json!({
        "color": format_hex([config.color.r, config.color.g, config.color.b, config.color.a]),
        "opacity": config.opacity,
    })
}

/// 在整数档位表里找最接近 `value` 的下标（表非空，空表返回 0）。
///
/// # 参数
/// - `presets`：档位表。
/// - `value`：当前值。
pub fn nearest_preset(presets: &[i32], value: f64) -> usize {
    presets
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (f64::from(**a) - value)
                .abs()
                .total_cmp(&(f64::from(**b) - value).abs())
        })
        .map_or(0, |(i, _)| i)
}

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

    /// 水印编辑夹取范围、保留透明度通道、文本按字节截断。
    #[test]
    fn watermark_edits_clamp_and_keep_alpha() {
        let base = WatermarkConfig {
            color: ColorRgba8 {
                r: 1,
                g: 2,
                b: 3,
                a: 0x80,
            },
            ..WatermarkConfig::default()
        };
        let w = apply_watermark_edit(&base, WatermarkEdit::ColorRgb([9, 8, 7]));
        assert_eq!(
            w.color,
            ColorRgba8 {
                r: 9,
                g: 8,
                b: 7,
                a: 0x80
            }
        );
        assert_eq!(
            apply_watermark_edit(&base, WatermarkEdit::Angle(500.0)).angle,
            90.0
        );
        assert_eq!(
            apply_watermark_edit(&base, WatermarkEdit::Gap(1.0)).gap,
            10.0
        );
        assert_eq!(
            apply_watermark_edit(&base, WatermarkEdit::FontSize(9999.0)).font_size,
            512.0
        );
        assert_eq!(
            apply_watermark_edit(&base, WatermarkEdit::Opacity(f64::NAN)).opacity,
            0.0
        );
        let long = "水".repeat(200);
        let t = apply_watermark_edit(&base, WatermarkEdit::Text(format!("  {long} "))).text;
        assert!(t.len() <= WATERMARK_TEXT_MAX_BYTES);
        assert_eq!(t.len(), 255);
    }

    /// 聚光灯编辑与序列化往返一致。
    #[test]
    fn spotlight_edit_and_json_round_trip() {
        let s = apply_spotlight_edit(&SpotlightConfig::default(), SpotlightEdit::Opacity(2.0));
        assert_eq!(s.opacity, 1.0);
        let s = apply_spotlight_edit(&s, SpotlightEdit::ColorRgb([10, 20, 30]));
        assert_eq!(
            spotlight_from_json(&spotlight_to_json(&s), &SpotlightConfig::default()),
            s
        );
    }

    /// 水印样式序列化后不含文本，且还原一致。
    #[test]
    fn watermark_json_round_trip_without_text() {
        let w = WatermarkConfig {
            text: "secret".into(),
            angle: -15.0,
            gap: 80.0,
            font_size: 24.0,
            opacity: 0.32,
            ..WatermarkConfig::default()
        };
        let v = watermark_to_json(&w);
        assert!(v.get("text").is_none());
        let back = watermark_from_json(&v, &WatermarkConfig::default());
        assert_eq!(
            (back.angle, back.gap, back.font_size, back.opacity),
            (-15.0, 80.0, 24.0, 0.32)
        );
        assert!(back.text.is_empty());
    }

    /// 档位表最近项查找。
    #[test]
    fn nearest_preset_picks_closest() {
        assert_eq!(nearest_preset(&WATERMARK_ANGLE_PRESETS, 28.0), 6);
        assert_eq!(nearest_preset(&WATERMARK_ANGLE_PRESETS, -100.0), 0);
        assert_eq!(nearest_preset(&[], 3.0), 0);
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
