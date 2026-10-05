//! 标注样式：每工具独立的样式状态、预设、最近使用颜色、配置持久化与到引擎参数的映射。
//!
//! 本模块不依赖 GPUI，全部逻辑可离屏单测。样式以逻辑像素表示，真正下发引擎时才乘设备像素比。
//!
//! 持久化沿用旧版 schema 的键（`drawing/*_style`），值是 JSON 对象：
//! `{"color":"#RRGGBBAA","width":3,"font_size":20,"fill":false,"arrowhead":"arrow"}`。
//! 最近使用颜色没有对应的旧键，放在 `drawing/shape_style` 对象的 `recent_colors` 字段里。

use serde_json::{Map, Value, json};
use snow_draw_engine::{Arrowhead, ColorRgba8, FillStyle, ShapeKind, ShapeStyle, ShapeStylePatch};
use snow_draw_engine_editor::{
    SHAPE_STYLE_PROPERTY_END_ARROWHEAD, SHAPE_STYLE_PROPERTY_FILL, SHAPE_STYLE_PROPERTY_FILL_STYLE,
    SHAPE_STYLE_PROPERTY_START_ARROWHEAD, SHAPE_STYLE_PROPERTY_STROKE,
    SHAPE_STYLE_PROPERTY_STROKE_WIDTH,
};
use snow_ui::widgets::AnnotationTool;
use std::collections::HashMap;

/// RGBA 颜色（每通道 0..=255）。
pub type Rgba = [u8; 4];

/// 预设色板（RGBA）。
pub const PALETTE: [Rgba; 11] = [
    [0xFF, 0x30, 0x30, 0xFF],
    [0xFF, 0x8A, 0x00, 0xFF],
    [0xFF, 0xD6, 0x0A, 0xFF],
    [0x30, 0xC0, 0x48, 0xFF],
    [0x00, 0xBC, 0xD4, 0xFF],
    [0x16, 0x77, 0xFF, 0xFF],
    [0x8E, 0x44, 0xFF, 0xFF],
    [0xFF, 0x4D, 0xA6, 0xFF],
    [0x00, 0x00, 0x00, 0xFF],
    [0x8C, 0x8C, 0x8C, 0xFF],
    [0xFF, 0xFF, 0xFF, 0xFF],
];
/// 线宽档位（逻辑像素）。
pub const WIDTH_PRESETS: [u32; 10] = [1, 2, 3, 4, 6, 8, 12, 16, 20, 30];
/// 字号档位（逻辑像素）。
pub const FONT_PRESETS: [u32; 11] = [12, 14, 16, 18, 20, 24, 28, 32, 40, 48, 64];
/// 最近使用颜色的最大保留数量。
pub const RECENT_LIMIT: usize = 8;
/// 线宽允许范围（逻辑像素）。
const WIDTH_RANGE: (u32, u32) = (1, 64);
/// 字号允许范围（逻辑像素）。
const FONT_RANGE: (u32, u32) = (8, 128);
/// 形状填充色相对描边色的透明度（0..=255）。
pub const FILL_ALPHA: u8 = 0x55;
/// 荧光笔默认颜色（半透明黄）。
const HIGHLIGHT_COLOR: Rgba = [0xFF, 0xD6, 0x0A, 0x80];
/// 序号球数字的颜色（白）。
pub const COUNTER_DIGIT_COLOR: Rgba = [0xFF, 0xFF, 0xFF, 0xFF];
/// 文字样式对象里最近使用颜色的字段名。
const RECENT_FIELD: &str = "recent_colors";
/// 承载最近使用颜色的配置键（旧版形状样式键）。
const RECENT_HOST_KEY: &str = "drawing/shape_style";

/// 箭头头型选项（含“无”）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowheadChoice {
    /// 无箭头。
    None,
    /// 标准箭头。
    Arrow,
    /// 实心三角。
    Triangle,
    /// 实心圆点。
    Dot,
    /// 竖线。
    Bar,
    /// 实心菱形。
    Diamond,
}

impl ArrowheadChoice {
    /// 全部选项（下拉顺序）。
    pub const ALL: [ArrowheadChoice; 6] = [
        Self::None,
        Self::Arrow,
        Self::Triangle,
        Self::Dot,
        Self::Bar,
        Self::Diamond,
    ];

    /// 配置里的标识（与引擎序列化名一致，`none` 表示无）。
    pub const fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Arrow => "arrow",
            Self::Triangle => "triangle",
            Self::Dot => "dot",
            Self::Bar => "bar",
            Self::Diamond => "diamond",
        }
    }

    /// 由配置标识还原；未知标识返回 `None`。
    ///
    /// # 参数
    /// - `id`：配置里的标识。
    ///
    /// ```ignore
    /// assert_eq!(ArrowheadChoice::from_id("dot"), Some(ArrowheadChoice::Dot));
    /// ```
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// 本地化文案的 id。
    pub const fn text_id(self) -> &'static str {
        match self {
            Self::None => "annot-arrowhead-none",
            Self::Arrow => "annot-arrowhead-arrow",
            Self::Triangle => "annot-arrowhead-triangle",
            Self::Dot => "annot-arrowhead-dot",
            Self::Bar => "annot-arrowhead-bar",
            Self::Diamond => "annot-arrowhead-diamond",
        }
    }

    /// 对应的引擎箭头头型。
    pub const fn engine(self) -> Option<Arrowhead> {
        match self {
            Self::None => None,
            Self::Arrow => Some(Arrowhead::Arrow),
            Self::Triangle => Some(Arrowhead::Triangle),
            Self::Dot => Some(Arrowhead::Dot),
            Self::Bar => Some(Arrowhead::Bar),
            Self::Diamond => Some(Arrowhead::Diamond),
        }
    }
}

/// 某个工具样式面板上需要展示的控件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleFields {
    /// 颜色。
    pub color: bool,
    /// 线宽。
    pub width: bool,
    /// 字号。
    pub font_size: bool,
    /// 填充开关。
    pub fill: bool,
    /// 箭头头型。
    pub arrowhead: bool,
}

impl StyleFields {
    /// 是否一个控件都没有（此时不弹面板）。
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// 单个工具的样式（逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolStyle {
    /// 描边 / 文字颜色。
    pub color: Rgba,
    /// 线宽（逻辑像素）。
    pub width: u32,
    /// 字号（逻辑像素）。
    pub font_size: u32,
    /// 形状是否填充。
    pub fill: bool,
    /// 箭头头型。
    pub arrowhead: ArrowheadChoice,
}

/// 把工具归并到样式槽：椭圆与矩形共用引擎的形状样式，因此共用一份。
///
/// # 参数
/// - `tool`：工具栏工具。
///
/// ```ignore
/// assert_eq!(style_slot(AnnotationTool::Ellipse), AnnotationTool::Rectangle);
/// ```
pub fn style_slot(tool: AnnotationTool) -> AnnotationTool {
    match tool {
        AnnotationTool::Ellipse => AnnotationTool::Rectangle,
        other => other,
    }
}

/// 工具对应的配置键（沿用旧版 `drawing/*_style`）；没有样式的工具返回 `None`。
///
/// # 参数
/// - `tool`：工具栏工具。
///
/// ```ignore
/// assert_eq!(config_key(AnnotationTool::Arrow), Some("drawing/arrow_style"));
/// ```
pub fn config_key(tool: AnnotationTool) -> Option<&'static str> {
    match style_slot(tool) {
        AnnotationTool::Rectangle => Some("drawing/shape_style"),
        AnnotationTool::Arrow => Some("drawing/arrow_style"),
        AnnotationTool::Line => Some("drawing/line_style"),
        AnnotationTool::Pencil => Some("drawing/free_draw_style"),
        AnnotationTool::Text => Some("drawing/text_style"),
        AnnotationTool::Highlighter => Some("drawing/pen_highlight_style"),
        AnnotationTool::Counter => Some("drawing/serial_number_style"),
        _ => None,
    }
}

/// 工具样式面板要展示哪些控件。
///
/// # 参数
/// - `tool`：工具栏工具。
///
/// ```ignore
/// assert!(style_fields(AnnotationTool::Text).font_size);
/// assert!(style_fields(AnnotationTool::Mosaic).is_empty());
/// ```
pub fn style_fields(tool: AnnotationTool) -> StyleFields {
    let base = StyleFields::default();
    match style_slot(tool) {
        AnnotationTool::Rectangle => StyleFields {
            color: true,
            width: true,
            fill: true,
            ..base
        },
        AnnotationTool::Arrow => StyleFields {
            color: true,
            width: true,
            arrowhead: true,
            ..base
        },
        AnnotationTool::Line | AnnotationTool::Pencil | AnnotationTool::Highlighter => {
            StyleFields {
                color: true,
                width: true,
                ..base
            }
        }
        AnnotationTool::Text | AnnotationTool::Counter => StyleFields {
            color: true,
            font_size: true,
            ..base
        },
        _ => base,
    }
}

/// 工具的出厂默认样式。
///
/// # 参数
/// - `tool`：工具栏工具。
///
/// ```ignore
/// assert_eq!(default_style(AnnotationTool::Line).width, 3);
/// ```
pub fn default_style(tool: AnnotationTool) -> ToolStyle {
    let base = ToolStyle {
        color: PALETTE[0],
        width: 3,
        font_size: 20,
        fill: false,
        arrowhead: ArrowheadChoice::Arrow,
    };
    match style_slot(tool) {
        AnnotationTool::Highlighter => ToolStyle {
            color: HIGHLIGHT_COLOR,
            width: 20,
            ..base
        },
        AnnotationTool::Counter => ToolStyle {
            font_size: 18,
            ..base
        },
        _ => base,
    }
}

/// 在档位表里找最接近 `value` 的下标。
///
/// # 参数
/// - `presets`：升序档位表（非空）。
/// - `value`：当前值。
///
/// ```ignore
/// assert_eq!(nearest_index(&[1, 2, 3, 5], 4), 2);
/// ```
pub fn nearest_index(presets: &[u32], value: u32) -> usize {
    presets
        .iter()
        .enumerate()
        .min_by_key(|(_, p)| p.abs_diff(value))
        .map_or(0, |(i, _)| i)
}

/// 颜色格式化为 `#RRGGBBAA`（与旧版配置一致）。
///
/// # 参数
/// - `color`：RGBA 颜色。
///
/// ```ignore
/// assert_eq!(format_hex([255, 0, 0, 255]), "#FF0000FF");
/// ```
pub fn format_hex(color: Rgba) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        color[0], color[1], color[2], color[3]
    )
}

/// 解析 `#RRGGBB` 或 `#RRGGBBAA`；非法返回 `None`。
///
/// # 参数
/// - `text`：颜色文本。
///
/// ```ignore
/// assert_eq!(parse_hex("#FF000080"), Some([255, 0, 0, 0x80]));
/// assert_eq!(parse_hex("red"), None);
/// ```
pub fn parse_hex(text: &str) -> Option<Rgba> {
    let digits = text.strip_prefix('#')?;
    if !digits.is_ascii() || !(digits.len() == 6 || digits.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if digits.len() == 8 { byte(6)? } else { 0xFF },
    ])
}

impl ToolStyle {
    /// 序列化为配置对象。
    pub fn to_json(&self) -> Value {
        json!({
            "color": format_hex(self.color),
            "width": self.width,
            "font_size": self.font_size,
            "fill": self.fill,
            "arrowhead": self.arrowhead.id(),
        })
    }

    /// 从配置对象还原；缺字段或非法字段取 `base` 的值，数值夹到允许范围。
    ///
    /// # 参数
    /// - `value`：配置里的 JSON（非对象时整体取 `base`）。
    /// - `base`：缺省值。
    ///
    /// ```ignore
    /// let s = ToolStyle::from_json(&json!({"width": 999}), default_style(AnnotationTool::Line));
    /// assert_eq!(s.width, 64);
    /// ```
    pub fn from_json(value: &Value, base: ToolStyle) -> ToolStyle {
        let Some(obj) = value.as_object() else {
            return base;
        };
        let number = |name: &str, range: (u32, u32), fallback: u32| {
            obj.get(name).and_then(Value::as_u64).map_or(fallback, |v| {
                v.clamp(u64::from(range.0), u64::from(range.1)) as u32
            })
        };
        ToolStyle {
            color: obj
                .get("color")
                .and_then(Value::as_str)
                .and_then(parse_hex)
                .unwrap_or(base.color),
            width: number("width", WIDTH_RANGE, base.width),
            font_size: number("font_size", FONT_RANGE, base.font_size),
            fill: obj
                .get("fill")
                .and_then(Value::as_bool)
                .unwrap_or(base.fill),
            arrowhead: obj
                .get("arrowhead")
                .and_then(Value::as_str)
                .and_then(ArrowheadChoice::from_id)
                .unwrap_or(base.arrowhead),
        }
    }
}

/// 逻辑像素换算为物理像素（至少 1）。
///
/// # 参数
/// - `logical`：逻辑像素值。
/// - `dpr`：设备像素比，非法值按 1.0。
///
/// ```ignore
/// assert_eq!(physical_px(3, 2.0), 6.0);
/// ```
pub fn physical_px(logical: u32, dpr: f64) -> f64 {
    let dpr = if dpr.is_finite() && dpr > 0.0 {
        dpr
    } else {
        1.0
    };
    (f64::from(logical) * dpr).round().max(1.0)
}

/// 颜色转引擎颜色。
pub fn engine_color(color: Rgba) -> ColorRgba8 {
    ColorRgba8 {
        r: color[0],
        g: color[1],
        b: color[2],
        a: color[3],
    }
}

/// 形状填充色：开启时取描边色并降低透明度，关闭为全透明。
///
/// # 参数
/// - `style`：工具样式。
///
/// ```ignore
/// assert_eq!(fill_color(&default_style(AnnotationTool::Rectangle)).a, 0);
/// ```
pub fn fill_color(style: &ToolStyle) -> ColorRgba8 {
    if style.fill {
        ColorRgba8 {
            a: FILL_ALPHA.min(style.color[3]),
            ..engine_color(style.color)
        }
    } else {
        ColorRgba8 {
            a: 0,
            ..engine_color(style.color)
        }
    }
}

/// 把工具样式映射成引擎的形状样式补丁（只改本工具关心的属性，因此会同时作用于
/// 引擎默认值与当前选中的同类对象）。非形状类工具（文字 / 序号 / 滤镜）返回 `None`。
///
/// # 参数
/// - `tool`：工具栏工具。
/// - `style`：该工具的样式。
/// - `dpr`：设备像素比。
/// - `base`：补丁里其余字段的基底（不会被应用）。
///
/// ```ignore
/// let patch = shape_patch(AnnotationTool::Line, &default_style(AnnotationTool::Line), 2.0, base).unwrap();
/// assert_eq!(patch.style.stroke_width, 6.0);
/// ```
pub fn shape_patch(
    tool: AnnotationTool,
    style: &ToolStyle,
    dpr: f64,
    base: ShapeStyle,
) -> Option<ShapeStylePatch> {
    let stroke = SHAPE_STYLE_PROPERTY_STROKE | SHAPE_STYLE_PROPERTY_STROKE_WIDTH;
    let width = physical_px(style.width, dpr);
    let mut shape = ShapeStyle {
        stroke: engine_color(style.color),
        stroke_width: width,
        ..base
    };
    let (kind, properties) = match style_slot(tool) {
        AnnotationTool::Rectangle => {
            shape.fill = fill_color(style);
            shape.fill_style = FillStyle::Solid;
            (
                ShapeKind::Rectangle,
                stroke | SHAPE_STYLE_PROPERTY_FILL | SHAPE_STYLE_PROPERTY_FILL_STYLE,
            )
        }
        AnnotationTool::Arrow => {
            shape.start_arrowhead = None;
            shape.end_arrowhead = style.arrowhead.engine();
            (
                ShapeKind::Arrow,
                stroke | SHAPE_STYLE_PROPERTY_START_ARROWHEAD | SHAPE_STYLE_PROPERTY_END_ARROWHEAD,
            )
        }
        AnnotationTool::Line => (ShapeKind::Line, stroke),
        AnnotationTool::Pencil => (ShapeKind::FreeDraw, stroke),
        AnnotationTool::Highlighter => (ShapeKind::PenHighlight, stroke),
        _ => return None,
    };
    Some(ShapeStylePatch {
        kind,
        style: shape,
        properties,
    })
}

/// 计算样式面板的左上角（逻辑像素）：优先贴在工具栏下方，放不下就放到工具栏上方，
/// 水平方向与工具栏左对齐并夹进屏幕。
///
/// # 参数
/// - `toolbar`：工具栏左上角。
/// - `toolbar_h`：工具栏高度。
/// - `panel`：面板 `(宽, 高)`。
/// - `screen`：屏幕 `(宽, 高)`。
/// - `gap`：面板与工具栏的间距。
///
/// # 返回
/// 面板左上角 `(x, y)`。
///
/// ```ignore
/// assert_eq!(panel_placement((100, 50), 36, (400, 80), (1920, 1080), 6), (100, 92));
/// ```
pub fn panel_placement(
    toolbar: (i32, i32),
    toolbar_h: i32,
    panel: (i32, i32),
    screen: (i32, i32),
    gap: i32,
) -> (i32, i32) {
    let x = toolbar.0.min(screen.0 - panel.0).max(0);
    let below = toolbar.1 + toolbar_h + gap;
    let y = if below + panel.1 <= screen.1 {
        below
    } else {
        toolbar.1 - gap - panel.1
    };
    (x, y.max(0))
}

/// 每工具独立的样式仓库（含最近使用颜色）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolStyleStore {
    /// 已被用户或配置改过的样式槽；缺省槽取出厂默认。
    slots: HashMap<AnnotationTool, ToolStyle>,
    /// 最近使用颜色（最新在前）。
    recent: Vec<Rgba>,
}

impl ToolStyleStore {
    /// 创建全部取出厂默认的仓库。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从配置读取各工具样式与最近颜色。
    ///
    /// # 参数
    /// - `read`：按键读取配置值的函数（未知键应返回 `Null`）。
    ///
    /// ```ignore
    /// let store = ToolStyleStore::load(|key| doc.value(key));
    /// ```
    pub fn load(read: impl Fn(&str) -> Value) -> Self {
        let mut store = Self::new();
        for tool in [
            AnnotationTool::Rectangle,
            AnnotationTool::Arrow,
            AnnotationTool::Line,
            AnnotationTool::Pencil,
            AnnotationTool::Text,
            AnnotationTool::Highlighter,
            AnnotationTool::Counter,
        ] {
            let Some(key) = config_key(tool) else {
                continue;
            };
            let value = read(key);
            if value.as_object().is_some_and(|o| !o.is_empty()) {
                store
                    .slots
                    .insert(tool, ToolStyle::from_json(&value, default_style(tool)));
            }
        }
        if let Some(list) = read(RECENT_HOST_KEY)
            .get(RECENT_FIELD)
            .and_then(Value::as_array)
        {
            for color in list
                .iter()
                .rev()
                .filter_map(Value::as_str)
                .filter_map(parse_hex)
            {
                store.push_recent(color);
            }
        }
        store
    }

    /// 取工具当前样式（没改过则为出厂默认）。
    ///
    /// # 参数
    /// - `tool`：工具栏工具；椭圆与矩形共用一份。
    pub fn style(&self, tool: AnnotationTool) -> ToolStyle {
        let slot = style_slot(tool);
        self.slots
            .get(&slot)
            .copied()
            .unwrap_or_else(|| default_style(slot))
    }

    /// 设置工具样式。
    ///
    /// # 参数
    /// - `tool`：工具栏工具。
    /// - `style`：新样式。
    ///
    /// # 返回
    /// 样式是否真的发生了变化。
    pub fn set_style(&mut self, tool: AnnotationTool, style: ToolStyle) -> bool {
        if self.style(tool) == style {
            return false;
        }
        self.slots.insert(style_slot(tool), style);
        true
    }

    /// 最近使用颜色（最新在前）。
    pub fn recent(&self) -> &[Rgba] {
        &self.recent
    }

    /// 记录一次选色：去重后置顶，超过 [`RECENT_LIMIT`] 丢最旧的。
    ///
    /// # 参数
    /// - `color`：被选中的颜色。
    pub fn push_recent(&mut self, color: Rgba) {
        self.recent.retain(|c| *c != color);
        self.recent.insert(0, color);
        self.recent.truncate(RECENT_LIMIT);
    }

    /// 需要写回配置的键值：工具自己的键，以及承载最近颜色的形状样式键。
    ///
    /// # 参数
    /// - `tool`：刚被修改的工具。
    ///
    /// # 返回
    /// `(配置键, 值)` 列表；没有样式的工具返回空。
    pub fn persist_entries(&self, tool: AnnotationTool) -> Vec<(&'static str, Value)> {
        let Some(key) = config_key(tool) else {
            return Vec::new();
        };
        let with_recent = |style: ToolStyle| {
            let mut obj: Map<String, Value> =
                style.to_json().as_object().cloned().unwrap_or_default();
            obj.insert(
                RECENT_FIELD.into(),
                Value::Array(
                    self.recent
                        .iter()
                        .map(|c| Value::String(format_hex(*c)))
                        .collect(),
                ),
            );
            Value::Object(obj)
        };
        let mut entries = Vec::new();
        if key == RECENT_HOST_KEY {
            entries.push((key, with_recent(self.style(tool))));
        } else {
            entries.push((key, self.style(tool).to_json()));
            entries.push((
                RECENT_HOST_KEY,
                with_recent(self.style(AnnotationTool::Rectangle)),
            ));
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_draw_engine::EditorStyleDefaults;

    /// 基底形状样式（取引擎默认的直线样式）。
    fn base() -> ShapeStyle {
        EditorStyleDefaults::default().line
    }

    #[test]
    fn hex_roundtrip_and_rejects_garbage() {
        assert_eq!(parse_hex(&format_hex([1, 2, 3, 4])), Some([1, 2, 3, 4]));
        assert_eq!(parse_hex("#FF0000"), Some([255, 0, 0, 255]));
        assert_eq!(parse_hex("FF0000"), None);
        assert_eq!(parse_hex("#GG0000"), None);
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#é12345"), None);
    }

    #[test]
    fn ellipse_shares_rectangle_slot_and_key() {
        assert_eq!(
            style_slot(AnnotationTool::Ellipse),
            AnnotationTool::Rectangle
        );
        assert_eq!(
            config_key(AnnotationTool::Ellipse),
            Some("drawing/shape_style")
        );
        let mut store = ToolStyleStore::new();
        let mut s = store.style(AnnotationTool::Rectangle);
        s.width = 8;
        assert!(store.set_style(AnnotationTool::Ellipse, s));
        assert_eq!(store.style(AnnotationTool::Rectangle).width, 8);
        assert!(
            !store.set_style(AnnotationTool::Rectangle, s),
            "相同样式不算变化"
        );
    }

    #[test]
    fn config_keys_follow_legacy_names() {
        assert_eq!(
            config_key(AnnotationTool::Arrow),
            Some("drawing/arrow_style")
        );
        assert_eq!(config_key(AnnotationTool::Line), Some("drawing/line_style"));
        assert_eq!(
            config_key(AnnotationTool::Pencil),
            Some("drawing/free_draw_style")
        );
        assert_eq!(config_key(AnnotationTool::Text), Some("drawing/text_style"));
        assert_eq!(
            config_key(AnnotationTool::Highlighter),
            Some("drawing/pen_highlight_style")
        );
        assert_eq!(
            config_key(AnnotationTool::Counter),
            Some("drawing/serial_number_style")
        );
        assert_eq!(config_key(AnnotationTool::Mosaic), None);
        assert_eq!(config_key(AnnotationTool::None), None);
        // 键必须真实存在于 schema，且默认值是对象
        for tool in [
            AnnotationTool::Rectangle,
            AnnotationTool::Arrow,
            AnnotationTool::Line,
            AnnotationTool::Pencil,
            AnnotationTool::Text,
            AnnotationTool::Highlighter,
            AnnotationTool::Counter,
        ] {
            let key = config_key(tool).unwrap();
            assert!(snow_config::schema::default_value(key).is_object(), "{key}");
        }
    }

    #[test]
    fn fields_per_tool() {
        assert!(style_fields(AnnotationTool::Rectangle).fill);
        assert!(style_fields(AnnotationTool::Arrow).arrowhead);
        assert!(style_fields(AnnotationTool::Text).font_size);
        assert!(!style_fields(AnnotationTool::Text).width);
        assert!(style_fields(AnnotationTool::Highlighter).width);
        assert!(style_fields(AnnotationTool::Counter).font_size);
        assert!(style_fields(AnnotationTool::Mosaic).is_empty());
        assert!(style_fields(AnnotationTool::None).is_empty());
    }

    #[test]
    fn recent_colors_dedup_and_cap() {
        let mut store = ToolStyleStore::new();
        for i in 0..12u8 {
            store.push_recent([i, 0, 0, 255]);
        }
        assert_eq!(store.recent().len(), RECENT_LIMIT);
        assert_eq!(store.recent()[0], [11, 0, 0, 255]);
        store.push_recent([8, 0, 0, 255]);
        assert_eq!(store.recent()[0], [8, 0, 0, 255]);
        assert_eq!(
            store
                .recent()
                .iter()
                .filter(|c| **c == [8, 0, 0, 255])
                .count(),
            1
        );
    }

    #[test]
    fn each_tool_remembers_its_own_style() {
        let mut store = ToolStyleStore::new();
        let mut line = store.style(AnnotationTool::Line);
        line.color = PALETTE[5];
        store.set_style(AnnotationTool::Line, line);
        assert_eq!(store.style(AnnotationTool::Line).color, PALETTE[5]);
        assert_eq!(store.style(AnnotationTool::Arrow).color, PALETTE[0]);
        assert_eq!(
            store.style(AnnotationTool::Highlighter),
            default_style(AnnotationTool::Highlighter)
        );
    }

    #[test]
    fn json_roundtrip_through_persist_entries() {
        let mut store = ToolStyleStore::new();
        let mut arrow = store.style(AnnotationTool::Arrow);
        arrow.width = 8;
        arrow.arrowhead = ArrowheadChoice::Dot;
        arrow.color = [1, 2, 3, 4];
        store.set_style(AnnotationTool::Arrow, arrow);
        store.push_recent([1, 2, 3, 4]);
        let entries = store.persist_entries(AnnotationTool::Arrow);
        assert_eq!(entries[0].0, "drawing/arrow_style");
        assert_eq!(entries[1].0, "drawing/shape_style");
        let map: HashMap<_, _> = entries.into_iter().collect();
        let loaded = ToolStyleStore::load(|key| map.get(key).cloned().unwrap_or(Value::Null));
        assert_eq!(loaded.style(AnnotationTool::Arrow), arrow);
        assert_eq!(loaded.recent(), &[[1, 2, 3, 4]]);
        assert_eq!(
            loaded.style(AnnotationTool::Line),
            default_style(AnnotationTool::Line)
        );
    }

    #[test]
    fn from_json_clamps_and_falls_back() {
        let base = default_style(AnnotationTool::Line);
        let s = ToolStyle::from_json(
            &json!({"width": 9999, "font_size": 0, "color": "bad", "arrowhead": "x"}),
            base,
        );
        assert_eq!(s.width, 64);
        assert_eq!(s.font_size, 8);
        assert_eq!(s.color, base.color);
        assert_eq!(s.arrowhead, base.arrowhead);
        assert_eq!(ToolStyle::from_json(&json!(3), base), base);
        assert_eq!(ToolStyle::from_json(&json!({}), base), base);
    }

    #[test]
    fn nearest_index_picks_closest() {
        assert_eq!(nearest_index(&WIDTH_PRESETS, 3), 2);
        assert_eq!(nearest_index(&WIDTH_PRESETS, 5), 3);
        assert_eq!(nearest_index(&WIDTH_PRESETS, 1000), WIDTH_PRESETS.len() - 1);
        assert_eq!(nearest_index(&FONT_PRESETS, 20), 4);
    }

    #[test]
    fn rectangle_patch_maps_stroke_and_fill() {
        let mut style = default_style(AnnotationTool::Rectangle);
        style.width = 4;
        style.color = [10, 20, 30, 255];
        let off = shape_patch(AnnotationTool::Rectangle, &style, 2.0, base()).unwrap();
        assert_eq!(off.kind, ShapeKind::Rectangle);
        assert_eq!(
            off.style.stroke,
            ColorRgba8 {
                r: 10,
                g: 20,
                b: 30,
                a: 255
            }
        );
        assert_eq!(off.style.stroke_width, 8.0);
        assert_eq!(off.style.fill.a, 0);
        style.fill = true;
        let on = shape_patch(AnnotationTool::Ellipse, &style, 1.0, base()).unwrap();
        assert_eq!(on.kind, ShapeKind::Rectangle);
        assert_eq!(
            on.style.fill,
            ColorRgba8 {
                r: 10,
                g: 20,
                b: 30,
                a: FILL_ALPHA
            }
        );
        assert_ne!(on.properties & SHAPE_STYLE_PROPERTY_FILL, 0);
    }

    #[test]
    fn arrow_patch_maps_arrowhead() {
        let mut style = default_style(AnnotationTool::Arrow);
        style.arrowhead = ArrowheadChoice::Triangle;
        let p = shape_patch(AnnotationTool::Arrow, &style, 1.0, base()).unwrap();
        assert_eq!(p.kind, ShapeKind::Arrow);
        assert_eq!(p.style.end_arrowhead, Some(Arrowhead::Triangle));
        assert_eq!(p.style.start_arrowhead, None);
        style.arrowhead = ArrowheadChoice::None;
        let p = shape_patch(AnnotationTool::Arrow, &style, 1.0, base()).unwrap();
        assert_eq!(p.style.end_arrowhead, None);
        assert_ne!(p.properties & SHAPE_STYLE_PROPERTY_END_ARROWHEAD, 0);
    }

    #[test]
    fn line_pencil_highlighter_patch_kinds() {
        for (tool, kind) in [
            (AnnotationTool::Line, ShapeKind::Line),
            (AnnotationTool::Pencil, ShapeKind::FreeDraw),
            (AnnotationTool::Highlighter, ShapeKind::PenHighlight),
        ] {
            let style = default_style(tool);
            let p = shape_patch(tool, &style, 1.5, base()).unwrap();
            assert_eq!(p.kind, kind);
            assert_eq!(p.style.stroke_width, physical_px(style.width, 1.5));
            assert_eq!(p.properties & SHAPE_STYLE_PROPERTY_FILL, 0);
        }
        assert!(
            shape_patch(
                AnnotationTool::Text,
                &default_style(AnnotationTool::Text),
                1.0,
                base()
            )
            .is_none()
        );
        assert!(
            shape_patch(
                AnnotationTool::Counter,
                &default_style(AnnotationTool::Counter),
                1.0,
                base()
            )
            .is_none()
        );
        assert!(
            shape_patch(
                AnnotationTool::Mosaic,
                &default_style(AnnotationTool::Line),
                1.0,
                base()
            )
            .is_none()
        );
    }

    #[test]
    fn highlighter_default_is_translucent_and_thick() {
        let s = default_style(AnnotationTool::Highlighter);
        assert!(s.color[3] < 0xFF);
        assert!(s.width > default_style(AnnotationTool::Line).width);
    }

    #[test]
    fn physical_px_rounds_and_guards() {
        assert_eq!(physical_px(3, 1.25), 4.0);
        assert_eq!(physical_px(1, 0.1), 1.0);
        assert_eq!(physical_px(3, f64::NAN), 3.0);
    }

    #[test]
    fn panel_goes_below_then_above_and_stays_on_screen() {
        let screen = (1920, 1080);
        assert_eq!(
            panel_placement((100, 50), 36, (400, 80), screen, 6),
            (100, 92)
        );
        assert_eq!(
            panel_placement((100, 1000), 36, (400, 80), screen, 6),
            (100, 914)
        );
        assert_eq!(
            panel_placement((1800, 50), 36, (400, 80), screen, 6),
            (1520, 92)
        );
        assert_eq!(panel_placement((-20, 0), 36, (400, 80), screen, 6), (0, 42));
        assert_eq!(
            panel_placement((10, 10), 36, (400, 2000), (800, 600), 6).1,
            0
        );
    }

    #[test]
    fn arrowhead_ids_roundtrip() {
        for c in ArrowheadChoice::ALL {
            assert_eq!(ArrowheadChoice::from_id(c.id()), Some(c));
        }
        assert_eq!(ArrowheadChoice::from_id("nope"), None);
    }

    #[test]
    fn config_document_accepts_style_objects() {
        use snow_config::document::ConfigDocument;
        let mut store = ToolStyleStore::new();
        store.push_recent(PALETTE[2]);
        let mut doc = ConfigDocument::from_bytes(None);
        for (key, value) in store.persist_entries(AnnotationTool::Rectangle) {
            doc.set_value(key, value).unwrap();
        }
        let loaded = ToolStyleStore::load(|key| doc.value(key));
        assert_eq!(loaded.recent(), &[PALETTE[2]]);
        assert_eq!(
            loaded.style(AnnotationTool::Rectangle),
            store.style(AnnotationTool::Rectangle)
        );
    }
}
