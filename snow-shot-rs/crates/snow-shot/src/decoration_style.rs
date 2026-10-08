//! 聚光灯 / 水印样式：从配置键 `drawing/spotlight_style`、`drawing/watermark_style` 读取并夹取范围。
//!
//! 键名、字段名与取值范围沿用旧版 `screenshotcanvastoolstyles.cpp`：颜色是 `#rrggbbaa`，
//! 水印文本与模板属于编辑会话，不进样式，所以这里只读外观字段。

use serde_json::{Value, json};
use snow_draw_engine::{
    ColorRgba8, SpotlightConfig, WatermarkConfig, WatermarkTemplateApplicationTime,
};
use snow_platform::local_time::LocalDateTime;

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
/// 水印模板配置键（旧版保存的 `[{name, value}]` 列表）。
pub const WATERMARK_TEMPLATES_KEY: &str = "drawing/watermark_templates";
/// 水印模板名最大字符数（旧版输入框上限）。
const WATERMARK_TEMPLATE_NAME_MAX_CHARS: usize = 80;
/// 水印旋转角度档位（度）。
pub const WATERMARK_ANGLE_PRESETS: [i32; 9] = [-60, -45, -30, -15, 0, 15, 30, 45, 60];
/// 水印间距档位（画布像素）。
pub const WATERMARK_GAP_PRESETS: [u32; 8] = [10, 20, 32, 56, 80, 120, 160, 200];

/// 对水印配置的一次编辑（面板控件产生）。
#[derive(Debug, Clone, PartialEq)]
pub enum WatermarkEdit {
    /// 换颜色的 RGB，保留原透明度通道。
    ColorRgb([u8; 3]),
    /// 换整个颜色（含透明度通道，取色器产生）。
    Color(ColorRgba8),
    /// 字体族（空串表示默认字体）。
    FontFamily(String),
    /// 文本模板（含 `{text}` 与 `{YYYY-MM-DD_HH-mm-ss}` 占位）与套用时间；空模板表示不用模板。
    Template {
        /// 模板文本。
        value: String,
        /// 套用时间（时间占位按它展开）。
        applied_at: Option<WatermarkTemplateApplicationTime>,
    },
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
    /// 换整个颜色（含透明度通道，取色器产生）。
    Color(ColorRgba8),
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
        WatermarkEdit::Color(color) => out.color = color,
        WatermarkEdit::FontFamily(family) => out.font_family = family.trim().to_owned(),
        WatermarkEdit::Template { value, applied_at } => {
            let value = truncate_bytes(&value, WATERMARK_TEXT_MAX_BYTES).to_owned();
            out.template_application_time = if value.is_empty() { None } else { applied_at };
            out.template_value = value;
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
        SpotlightEdit::Color(color) => out.color = color,
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

/// 水印模板（旧版 `drawing/watermark_templates` 的一项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatermarkTemplate {
    /// 模板名（下拉里显示）。
    pub name: String,
    /// 模板文本。
    pub value: String,
}

/// 从配置值读出模板列表；非数组或不合规的项（名 / 值去空白后为空）直接丢弃。
///
/// # 参数
/// - `value`：`drawing/watermark_templates` 的值。
///
/// ```ignore
/// let t = watermark_templates_from_json(&json!([{"name": "a", "value": "{text}"}]));
/// assert_eq!(t.len(), 1);
/// ```
pub fn watermark_templates_from_json(value: &Value) -> Vec<WatermarkTemplate> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.trim();
            let text = item.get("value")?.as_str()?;
            (!name.is_empty() && !text.trim().is_empty()).then(|| WatermarkTemplate {
                name: name.to_owned(),
                value: text.to_owned(),
            })
        })
        .collect()
}

/// 模板列表序列化为配置值。
///
/// # 参数
/// - `templates`：模板列表。
pub fn watermark_templates_to_json(templates: &[WatermarkTemplate]) -> Value {
    Value::Array(
        templates
            .iter()
            .map(|t| json!({"name": t.name, "value": t.value}))
            .collect(),
    )
}

/// 把当前模板文本存成一个新模板（名字取文本本身，按字符数截断）。
///
/// # 参数
/// - `templates`：现有模板列表。
/// - `value`：要保存的模板文本。
///
/// # 返回
/// 是否新增；文本为空白或已有相同文本的模板时返回 `false`。
pub fn add_watermark_template(templates: &mut Vec<WatermarkTemplate>, value: &str) -> bool {
    if value.trim().is_empty() || templates.iter().any(|t| t.value == value) {
        return false;
    }
    let name: String = value
        .trim()
        .chars()
        .take(WATERMARK_TEMPLATE_NAME_MAX_CHARS)
        .collect();
    templates.push(WatermarkTemplate {
        name,
        value: value.to_owned(),
    });
    true
}

/// 由本地时间得到模板套用时间；字段非法时返回 `None`。
///
/// # 参数
/// - `now`：本地日期时间。
pub fn template_time_from_local(now: LocalDateTime) -> Option<WatermarkTemplateApplicationTime> {
    let time = WatermarkTemplateApplicationTime {
        year: i32::from(now.year),
        month: now.month,
        day: now.day,
        hour: now.hour,
        minute: now.minute,
        second: now.second,
    };
    time.is_valid().then_some(time)
}

/// 颜色转 0..1 的 RGBA 浮点（取色器用）。
///
/// # 参数
/// - `color`：8 位颜色。
pub fn color_to_unit(color: ColorRgba8) -> [f32; 4] {
    [color.r, color.g, color.b, color.a].map(|c| f32::from(c) / 255.0)
}

/// 0..1 的 RGBA 浮点转 8 位颜色（越界与非有限值先夹取）。
///
/// # 参数
/// - `unit`：取色器给出的 RGBA。
pub fn color_from_unit(unit: [f32; 4]) -> ColorRgba8 {
    let byte = |v: f32| {
        if v.is_finite() {
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        }
    };
    ColorRgba8 {
        r: byte(unit[0]),
        g: byte(unit[1]),
        b: byte(unit[2]),
        a: byte(unit[3]),
    }
}

/// 字体族下拉的选项：系统字体名单，若当前字体不在其中则补在最前（避免已保存的字体丢失）。
///
/// # 参数
/// - `system`：系统字体名单。
/// - `current`：当前配置的字体族（空串表示默认，不补）。
pub fn font_family_options(system: Vec<String>, current: &str) -> Vec<String> {
    let current = current.trim();
    let mut out = system;
    if !current.is_empty() && !out.iter().any(|f| f == current) {
        out.insert(0, current.to_owned());
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

    /// 取色器颜色整体替换（含透明度），聚光灯同理。
    #[test]
    fn full_color_edit_replaces_alpha() {
        let c = ColorRgba8 {
            r: 1,
            g: 2,
            b: 3,
            a: 0x40,
        };
        let w = apply_watermark_edit(&WatermarkConfig::default(), WatermarkEdit::Color(c));
        assert_eq!(w.color, c);
        let s = apply_spotlight_edit(&SpotlightConfig::default(), SpotlightEdit::Color(c));
        assert_eq!(s.color, c);
    }

    /// 取色器浮点颜色与 8 位颜色互转：往返一致，越界与非有限值被夹住。
    #[test]
    fn color_unit_round_trip_and_clamp() {
        for v in [0u8, 1, 64, 128, 200, 255] {
            let c = ColorRgba8 {
                r: v,
                g: 255 - v,
                b: v / 2,
                a: v,
            };
            assert_eq!(color_from_unit(color_to_unit(c)), c);
        }
        let c = color_from_unit([2.0, -1.0, f32::NAN, 0.5]);
        assert_eq!((c.r, c.g, c.b, c.a), (255, 0, 0, 128));
    }

    /// 字体族编辑去空白；字体下拉补上不在系统名单里的当前字体。
    #[test]
    fn font_family_edit_and_options() {
        let w = apply_watermark_edit(
            &WatermarkConfig::default(),
            WatermarkEdit::FontFamily("  Segoe UI ".into()),
        );
        assert_eq!(w.font_family, "Segoe UI");
        let sys = vec!["Arial".to_owned(), "Segoe UI".to_owned()];
        assert_eq!(font_family_options(sys.clone(), "Segoe UI"), sys);
        assert_eq!(font_family_options(sys.clone(), ""), sys);
        assert_eq!(font_family_options(sys, "Gone")[0], "Gone");
    }

    /// 模板编辑：写入模板与套用时间，空模板清掉套用时间，过长按字节截断；解析结果与引擎一致。
    #[test]
    fn template_edit_sets_time_and_resolves() {
        let at = template_time_from_local(LocalDateTime {
            year: 2026,
            month: 10,
            day: 8,
            hour: 9,
            minute: 5,
            second: 7,
        });
        assert!(at.is_some());
        let w = apply_watermark_edit(
            &WatermarkConfig {
                text: "Secret".into(),
                ..WatermarkConfig::default()
            },
            WatermarkEdit::Template {
                value: "{text}-{YYYY-MM-DD_HH-mm-ss}".into(),
                applied_at: at,
            },
        );
        assert_eq!(w.resolved_text(), "Secret-2026-10-08_09-05-07");
        let cleared = apply_watermark_edit(
            &w,
            WatermarkEdit::Template {
                value: String::new(),
                applied_at: at,
            },
        );
        assert_eq!(cleared.template_application_time, None);
        assert_eq!(cleared.resolved_text(), "Secret");
        let long = apply_watermark_edit(
            &w,
            WatermarkEdit::Template {
                value: "水".repeat(200),
                applied_at: at,
            },
        );
        assert!(long.template_value.len() <= WATERMARK_TEXT_MAX_BYTES);
        // 非法本地时间不产生套用时间
        assert!(
            template_time_from_local(LocalDateTime {
                year: 2026,
                month: 13,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0
            })
            .is_none()
        );
    }

    /// 模板列表：非法项被丢弃，保存去重，往返一致。
    #[test]
    fn watermark_templates_parse_add_and_round_trip() {
        let list = watermark_templates_from_json(&json!([
            {"name": " A ", "value": "{text}"},
            {"name": "", "value": "x"},
            {"name": "B", "value": "   "},
            {"name": 3, "value": "x"},
            "junk"
        ]));
        assert_eq!(
            list,
            vec![WatermarkTemplate {
                name: "A".into(),
                value: "{text}".into()
            }]
        );
        assert!(watermark_templates_from_json(&json!({})).is_empty());
        let mut list = list;
        assert!(!add_watermark_template(&mut list, "{text}"));
        assert!(!add_watermark_template(&mut list, "  "));
        assert!(add_watermark_template(&mut list, "{text} {YYYY}"));
        assert_eq!(list.len(), 2);
        let back = watermark_templates_from_json(&watermark_templates_to_json(&list));
        assert_eq!(back, list);
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
