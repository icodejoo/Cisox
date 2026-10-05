//! 截图标注层：标注引擎 + tiny-skia 光栅化 + 文字 / 滤镜合成。
//!
//! 引擎（`snow-draw-engine`）负责工具状态机、撤销重做与补丁协议，`snow-canvas-raster`
//! 只重绘脏区并输出 256px 脏块；文字（系统 GDI 光栅化）与马赛克 / 模糊滤镜由本模块
//! 在块内补画。屏幕预览的脏块与最终导出走同一套合成函数，因此两者像素一致。
//!
//! 坐标约定：世界坐标 = 覆盖窗底图物理像素坐标（相机恒等、缩放 1）。
//!
//! 层序（自下而上）：滤镜（取自冻结底图）-> 矢量图形 -> 文字。滤镜只采样冻结底图，
//! 不会对其下方的其它标注再做模糊。

use snow_canvas_filters::{
    ExecutionOptions, FILTER_BLUR, FILTER_MOSAIC, OwnedImage, Parameters, apply,
    sampling_radius_pixels,
};
use snow_canvas_raster::{
    CanvasRasterizer, RasterConfig, RasterOutput, RasterTile, TileKey, TinySkiaRasterizer,
};
use snow_draw_engine::{
    ActiveTool, ArrowStyle, ArrowType, Camera, Arrowhead, ColorRgba8, DisplayFilterType,
    DisplayTextHorizontalAlign, DisplayTextVerticalAlign, EditorStyleDefaults, Engine,
    FILTER_STYLE_PROPERTY_ALL, FilterDisplayItem, FilterStyle, InputEvent, Modifiers, Point,
    PointerButton, PointerButtons, PointerDevice, PointerEvent, PointerEventType,
    RectangleShapeStyle, RuntimeConfig, SceneDisplayItem, StrokeStyle, StyleDefaults,
    TextDisplayItem, TextLayoutSize, ViewportConfig, ViewportId,
};
use snow_draw_engine::{DisplayFillStyle, DisplaySerialNumberType, SerialNumberDisplayItem, SerialNumberType};
use snow_draw_engine::{HighlightShape, ShapeStyle, TextStyle};
use snow_draw_engine_editor::{TEXT_STYLE_MIXED_COLOR, TEXT_STYLE_MIXED_FONT_SIZE};
use crate::annotation_style::{
    COUNTER_DIGIT_COLOR, ToolStyle, engine_color, physical_px, shape_patch,
};
use snow_platform::text_raster::{self, DEFAULT_FONT_FAMILY, TextBitmap};
use snow_ui::widgets::AnnotationTool;
use std::collections::HashMap;
use std::sync::Arc;

/// 光栅分块边长（像素）。
pub const TILE_SIZE: u32 = 256;
/// 每像素字节数（RGBA / BGRA）。
const BPP: usize = 4;
/// 默认画笔颜色（红）。
const DEFAULT_COLOR: ColorRgba8 = ColorRgba8 {
    r: 0xFF,
    g: 0x30,
    b: 0x30,
    a: 0xFF,
};
/// 逻辑像素下的默认线宽。
const DEFAULT_STROKE_LOGICAL: f64 = 3.0;
/// 逻辑像素下的默认文字字号。
const DEFAULT_FONT_LOGICAL: f64 = 20.0;
/// 序号球外圈宽度（逻辑像素）。
const COUNTER_RING_LOGICAL: u32 = 2;
/// 马赛克强度（0..=1，越大块越粗）。
const MOSAIC_STRENGTH: f64 = 1.0;
/// 模糊强度（0..=1）。
const BLUR_STRENGTH: f64 = 0.6;
/// 指针编号（鼠标固定为 1）。
const POINTER_ID: u32 = 1;
/// 滤镜最大外扩采样半径（像素），防止异常参数造成巨型拷贝。
const MAX_FILTER_MARGIN: i32 = 256;
/// 文字缓存最多保留条数。
const TEXT_CACHE_LIMIT: usize = 64;

/// 整数矩形 `[x0, y0, x1, y1]`（右下开区间）。
pub type IntRect = [i32; 4];

/// 冻结底图的只读视图（BGRA，不透明）。
#[derive(Clone, Copy)]
pub struct BaseView<'a> {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// BGRA 像素，长度 `width * height * 4`。
    pub bgra: &'a [u8],
}

/// 一个待上传的预览脏块（预乘 BGRA，与 GPUI 图像格式一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileImage {
    /// 分块键。
    pub key: TileKey,
    /// 块左上角 x（物理像素）。
    pub x: u32,
    /// 块左上角 y（物理像素）。
    pub y: u32,
    /// 块宽。
    pub w: u32,
    /// 块高。
    pub h: u32,
    /// 预乘 BGRA 像素，长度 `w * h * 4`。
    pub bgra: Vec<u8>,
}

/// 一次操作后预览层需要做的增量更新。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerUpdate {
    /// 内容变化的块（同一键的旧图应被替换）。
    pub tiles: Vec<TileImage>,
    /// 现在已全透明、应释放的块。
    pub released: Vec<TileKey>,
    /// 本次更新覆盖的像素数（脏块面积和，用于性能统计）。
    pub touched_pixels: u64,
}

impl LayerUpdate {
    /// 是否没有任何变化。
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty() && self.released.is_empty()
    }
}

/// 标注样式（物理像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnnotationStyle {
    /// 描边 / 文字颜色。
    pub color: ColorRgba8,
    /// 线宽（物理像素）。
    pub stroke_width: f64,
    /// 文字字号（物理像素）。
    pub font_px: f64,
}

impl AnnotationStyle {
    /// 按设备像素比生成默认样式。
    ///
    /// # 参数
    /// - `dpr`：设备像素比（物理 / 逻辑），非法值按 1.0。
    ///
    /// ```
    /// use snow_shot::annotation::AnnotationStyle;
    /// assert_eq!(AnnotationStyle::for_dpr(2.0).stroke_width, 6.0);
    /// ```
    pub fn for_dpr(dpr: f32) -> Self {
        let dpr = if dpr.is_finite() && dpr > 0.0 { f64::from(dpr) } else { 1.0 };
        Self {
            color: DEFAULT_COLOR,
            stroke_width: (DEFAULT_STROKE_LOGICAL * dpr).round().max(1.0),
            font_px: (DEFAULT_FONT_LOGICAL * dpr).round().max(8.0),
        }
    }
}

/// 把工具栏工具映射为引擎工具；`None` / 不支持的工具返回 `None`。
///
/// # 参数
/// - `tool`：工具栏工具。
///
/// ```
/// use snow_shot::annotation::engine_tool;
/// use snow_ui::widgets::AnnotationTool;
/// assert!(engine_tool(AnnotationTool::None).is_none());
/// assert!(engine_tool(AnnotationTool::Arrow).is_some());
/// ```
pub fn engine_tool(tool: AnnotationTool) -> Option<ActiveTool> {
    match tool {
        AnnotationTool::Rectangle | AnnotationTool::Ellipse => Some(ActiveTool::Shape),
        AnnotationTool::Arrow => Some(ActiveTool::Arrow),
        AnnotationTool::Line => Some(ActiveTool::Line),
        AnnotationTool::Pencil => Some(ActiveTool::FreeDraw),
        AnnotationTool::Text => Some(ActiveTool::Text),
        AnnotationTool::Mosaic | AnnotationTool::Blur => Some(ActiveTool::RectangleFilter),
        AnnotationTool::Highlighter => Some(ActiveTool::PenHighlight),
        AnnotationTool::Counter => Some(ActiveTool::SerialNumber),
        AnnotationTool::None => None,
    }
}

/// 求两个整数矩形的交集；不相交返回 `None`。
fn intersect(a: IntRect, b: IntRect) -> Option<IntRect> {
    let r = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (r[0] < r[2] && r[1] < r[3]).then_some(r)
}

/// 矩形宽高。
fn rect_size(r: IntRect) -> (usize, usize) {
    ((r[2] - r[0]) as usize, (r[3] - r[1]) as usize)
}

/// 除以 255 并四舍五入（用于 alpha 混合）。
fn div255(x: u32) -> u32 {
    (x + 128 + ((x + 128) >> 8)) >> 8
}

/// 预乘 RGBA 的 source-over：把 `src` 叠到 `dst` 上（两者等尺寸）。
///
/// # 参数
/// - `dst`：目标（原地修改），预乘 RGBA。
/// - `src`：源，预乘 RGBA。
///
/// ```
/// use snow_shot::annotation::source_over;
/// let mut dst = [0, 0, 255, 255];
/// source_over(&mut dst, &[255, 0, 0, 255]);
/// assert_eq!(dst, [255, 0, 0, 255]);
/// ```
pub fn source_over(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(BPP).zip(src.chunks_exact(BPP)) {
        let sa = u32::from(s[3]);
        if sa == 0 {
            continue;
        }
        if sa == 255 {
            d.copy_from_slice(s);
            continue;
        }
        let inv = 255 - sa;
        for c in 0..BPP {
            d[c] = (u32::from(s[c]) + div255(u32::from(d[c]) * inv)).min(255) as u8;
        }
    }
}

/// 判断 RGBA 缓冲是否全透明（alpha 全 0）。
fn all_transparent(rgba: &[u8]) -> bool {
    rgba.chunks_exact(BPP).all(|p| p[3] == 0)
}

/// 像素缓冲的 64 位哈希（按 8 字节字混合，用于判断分块内容是否变化）。
///
/// # 参数
/// - `bytes`：像素字节。
///
/// ```
/// use snow_shot::annotation::hash_pixels;
/// assert_eq!(hash_pixels(&[1, 2, 3, 4, 5, 6, 7, 8, 9]), hash_pixels(&[1, 2, 3, 4, 5, 6, 7, 8, 9]));
/// assert_ne!(hash_pixels(&[0; 16]), hash_pixels(&[0; 17]));
/// ```
pub fn hash_pixels(bytes: &[u8]) -> u64 {
    const SEED: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = SEED ^ bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(word);
        hash = (hash ^ u64::from_le_bytes(buf)).wrapping_mul(PRIME).rotate_left(29);
    }
    for &b in words.remainder() {
        hash = (hash ^ u64::from(b)).wrapping_mul(PRIME);
    }
    hash
}

/// 把预乘 RGBA 就地交换 R / B 成预乘 BGRA。
fn swap_rb(buf: &mut [u8]) {
    for p in buf.chunks_exact_mut(BPP) {
        p.swap(0, 2);
    }
}

/// 元素旋转后的轴对齐包围盒（物理像素，向外取整）。
fn aabb(cx: f64, cy: f64, w: f64, h: f64, rotation: f64) -> IntRect {
    let (s, c) = rotation.sin_cos();
    let hw = (w.abs() * c.abs() + h.abs() * s.abs()) / 2.0;
    let hh = (w.abs() * s.abs() + h.abs() * c.abs()) / 2.0;
    [
        (cx - hw).floor() as i32,
        (cy - hh).floor() as i32,
        (cx + hw).ceil() as i32,
        (cy + hh).ceil() as i32,
    ]
}

/// 已算好的滤镜结果：覆盖矩形与不透明 BGRA 像素。
struct FilterRender {
    /// 结果覆盖的矩形（已裁进屏幕）。
    rect: IntRect,
    /// 不透明 BGRA 像素。
    bgra: Vec<u8>,
    /// 元素不透明度。
    opacity: f64,
}

/// 计算滤镜元素对应的滤镜参数。
fn filter_parameters(item: &FilterDisplayItem, dpr: f64) -> Option<Parameters> {
    let spec = item.filter;
    let base = Parameters {
        strength: spec.strength,
        logical_block_size: spec.mosaic_block_size,
        logical_sigma: spec.blur_sigma,
        logical_sampling_radius: spec.sampling_radius,
        device_pixel_ratio: dpr,
        ..Parameters::default()
    };
    match spec.filter_type {
        DisplayFilterType::Mosaic => Some(Parameters { filter_type: FILTER_MOSAIC, ..base }),
        DisplayFilterType::GaussianBlur => Some(Parameters { filter_type: FILTER_BLUR, ..base }),
        _ => None,
    }
}

/// 对底图的滤镜区域做滤镜，返回该区域的结果；不支持或区域为空返回 `None`。
///
/// # 参数
/// - `item`：滤镜元素。
/// - `base`：冻结底图。
/// - `dpr`：设备像素比（放大马赛克块与模糊半径）。
fn render_filter(item: &FilterDisplayItem, base: BaseView, dpr: f64) -> Option<FilterRender> {
    if item.is_pen_filter {
        return None;
    }
    let mut params = filter_parameters(item, dpr)?;
    let screen = [0, 0, base.width as i32, base.height as i32];
    let region = intersect(
        aabb(item.center_x, item.center_y, item.width, item.height, item.rotation),
        screen,
    )?;
    let margin = sampling_radius_pixels(&params).clamp(0, MAX_FILTER_MARGIN);
    let sub = intersect(
        [region[0] - margin, region[1] - margin, region[2] + margin, region[3] + margin],
        screen,
    )?;
    let (sw, sh) = rect_size(sub);
    let mut image = OwnedImage::new(sw as i32, sh as i32);
    for row in 0..sh {
        let src_off = ((sub[1] as usize + row) * base.width as usize + sub[0] as usize) * BPP;
        let src = base.bgra.get(src_off..src_off + sw * BPP)?;
        for (col, px) in src.chunks_exact(BPP).enumerate() {
            image.data[row * sw + col] = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
        }
    }
    params.grid_origin_x = f64::from(region[0] - sub[0]);
    params.grid_origin_y = f64::from(region[1] - sub[1]);
    apply(&mut image.as_mut(), &params, &ExecutionOptions::default());
    let (rw, rh) = rect_size(region);
    let mut bgra = Vec::with_capacity(rw * rh * BPP);
    for row in 0..rh {
        let y = (region[1] - sub[1]) as usize + row;
        let x = (region[0] - sub[0]) as usize;
        for value in &image.data[y * sw + x..y * sw + x + rw] {
            bgra.extend_from_slice(&value.to_le_bytes());
        }
    }
    Some(FilterRender {
        rect: region,
        bgra,
        opacity: item.opacity.clamp(0.0, 1.0),
    })
}

/// 把滤镜结果中与 `rect` 相交的部分写进图层（预乘 RGBA，按元素不透明度与底图混合）。
fn draw_filter(layer: &mut [u8], rect: IntRect, render: &FilterRender, base: BaseView) {
    let Some(hit) = intersect(rect, render.rect) else {
        return;
    };
    let (lw, _) = rect_size(rect);
    let (rw, _) = rect_size(render.rect);
    let (hw, hh) = rect_size(hit);
    for row in 0..hh {
        let y = hit[1] as usize + row;
        for col in 0..hw {
            let x = hit[0] as usize + col;
            let ro = ((y - render.rect[1] as usize) * rw + (x - render.rect[0] as usize)) * BPP;
            let lo = ((y - rect[1] as usize) * lw + (x - rect[0] as usize)) * BPP;
            let f = &render.bgra[ro..ro + BPP];
            let mut out = [f[2], f[1], f[0]];
            if render.opacity < 0.999 {
                let bo = (y * base.width as usize + x) * BPP;
                let b = &base.bgra[bo..bo + BPP];
                let t = render.opacity;
                let mix = |src: u8, dst: u8| (f64::from(dst) + (f64::from(src) - f64::from(dst)) * t).round() as u8;
                out = [mix(f[2], b[2]), mix(f[1], b[1]), mix(f[0], b[0])];
            }
            layer[lo..lo + BPP].copy_from_slice(&[out[0], out[1], out[2], 255]);
        }
    }
}

/// 文字缓存键：`(文本, 字号位模式, 字体族)`。
type TextKey = (String, u32, String);

/// 把一个文字元素与 `rect` 相交的部分画进图层（预乘 RGBA）。
fn draw_text(
    layer: &mut [u8],
    rect: IntRect,
    item: &TextDisplayItem,
    cache: &mut HashMap<TextKey, Option<Arc<TextBitmap>>>,
) {
    if item.text.trim().is_empty() {
        return;
    }
    let family = item
        .font_family
        .clone()
        .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string());
    let key: TextKey = (item.text.clone(), (item.font_size as f32).to_bits(), family.clone());
    if cache.len() >= TEXT_CACHE_LIMIT && !cache.contains_key(&key) {
        cache.clear();
    }
    let bitmap = cache
        .entry(key)
        .or_insert_with(|| {
            match text_raster::rasterize_text(&item.text, &family, item.font_size as f32, false) {
                Ok(b) => Some(Arc::new(b)),
                Err(e) => {
                    tracing::warn!(error = %e, "文字光栅化失败，该文字元素不会绘制");
                    None
                }
            }
        })
        .clone();
    let Some(bitmap) = bitmap else {
        return;
    };
    let (bw, bh) = (f64::from(bitmap.width), f64::from(bitmap.height));
    let left = item.center_x - item.width / 2.0;
    let top = item.center_y - item.height / 2.0;
    let dx = match item.horizontal_align {
        DisplayTextHorizontalAlign::Left => 0.0,
        DisplayTextHorizontalAlign::Center => (item.width - bw) / 2.0,
        DisplayTextHorizontalAlign::Right => item.width - bw,
    };
    let dy = match item.vertical_align {
        DisplayTextVerticalAlign::Top => 0.0,
        DisplayTextVerticalAlign::Center => (item.height - bh) / 2.0,
        DisplayTextVerticalAlign::Bottom => item.height - bh,
    };
    let (ox, oy) = ((left + dx).round() as i32, (top + dy).round() as i32);
    let bmp_rect = [ox, oy, ox + bitmap.width as i32, oy + bitmap.height as i32];
    let Some(hit) = intersect(rect, bmp_rect) else {
        return;
    };
    let (lw, _) = rect_size(rect);
    let (hw, hh) = rect_size(hit);
    // 文字整体不透明度（0..=255）：颜色 alpha 与元素不透明度相乘
    let base_alpha = (f64::from(item.color.a) * item.opacity.clamp(0.0, 1.0)).round() as u32;
    for row in 0..hh {
        let y = hit[1] as usize + row;
        for col in 0..hw {
            let x = hit[0] as usize + col;
            let cov = u32::from(bitmap.coverage[(y - oy as usize) * bitmap.width as usize + (x - ox as usize)]);
            if cov == 0 {
                continue;
            }
            let a = div255(cov * base_alpha).min(255);
            let src = [
                div255(u32::from(item.color.r) * a) as u8,
                div255(u32::from(item.color.g) * a) as u8,
                div255(u32::from(item.color.b) * a) as u8,
                a as u8,
            ];
            let lo = ((y - rect[1] as usize) * lw + (x - rect[0] as usize)) * BPP;
            source_over(&mut layer[lo..lo + BPP], &src);
        }
    }
}

/// 把序号球的数字转成居中的文字元素（实心球用白字，描边球用球本身的颜色）。
fn serial_digit_item(item: &SerialNumberDisplayItem) -> TextDisplayItem {
    let solid = matches!(
        item.serial_number_type,
        DisplaySerialNumberType::SolidCircle | DisplaySerialNumberType::SolidSquare
    );
    TextDisplayItem {
        id: item.id,
        center_x: item.center_x,
        center_y: item.center_y,
        width: item.diameter,
        height: item.diameter,
        rotation: item.rotation,
        content_width: item.diameter,
        content_height: item.diameter,
        text: item.number.to_string(),
        color: if solid { engine_color(COUNTER_DIGIT_COLOR) } else { item.color },
        font_size: item.font_size,
        font_family: item.font_family.clone(),
        fill: ColorRgba8::default(),
        fill_style: DisplayFillStyle::Solid,
        stroke: item.color,
        stroke_width: 0.0,
        corner_radii: item.corner_radii,
        horizontal_align: DisplayTextHorizontalAlign::Center,
        vertical_align: DisplayTextVerticalAlign::Center,
        opacity: item.opacity,
    }
}

/// 一个场景元素是否为需要外部绘制的（文字 / 滤镜）且包围盒与 `rect` 相交。
fn deferred_hits(item: &SceneDisplayItem, rect: IntRect) -> bool {
    let bounds = match item {
        SceneDisplayItem::Filter(f) if !f.is_pen_filter => {
            aabb(f.center_x, f.center_y, f.width, f.height, f.rotation)
        }
        SceneDisplayItem::Text(t) => aabb(t.center_x, t.center_y, t.content_width.max(t.width), t.content_height.max(t.height), t.rotation),
        SceneDisplayItem::SerialNumber(n) if n.serial_number_type != DisplaySerialNumberType::Circle => {
            aabb(n.center_x, n.center_y, n.diameter, n.diameter, n.rotation)
        }
        _ => return false,
    };
    intersect(rect, bounds).is_some()
}

/// 标注层。
pub struct AnnotationLayer {
    /// 标注引擎。
    engine: Engine,
    /// 引擎视口。
    viewport: ViewportId,
    /// 矢量光栅化器。
    raster: TinySkiaRasterizer,
    /// 画布宽（物理像素）。
    width: u32,
    /// 画布高（物理像素）。
    height: u32,
    /// 设备像素比。
    dpr: f64,
    /// 当前工具栏工具。
    tool: AnnotationTool,
    /// 当前样式。
    style: AnnotationStyle,
    /// 预览层已经输出过的块及其像素哈希（用于判断释放与跳过内容未变的块）。
    emitted: HashMap<TileKey, u64>,
    /// 文字光栅缓存。
    text_cache: HashMap<TextKey, Option<Arc<TextBitmap>>>,
    /// 指针是否处于按下（正在绘制）状态。
    drawing: bool,
}

/// 由标注样式生成引擎运行时配置。
fn runtime_config(style: AnnotationStyle) -> RuntimeConfig {
    RuntimeConfig {
        style_defaults: StyleDefaults {
            editor: engine_defaults(style),
            ..StyleDefaults::default()
        },
    }
}

/// 生成引擎初始样式默认值。
fn engine_defaults(style: AnnotationStyle) -> EditorStyleDefaults {
    let mut d = EditorStyleDefaults::default();
    d.rectangle = RectangleShapeStyle {
        stroke: style.color,
        stroke_width: style.stroke_width,
        corner_radii: snow_draw_engine::CornerRadii::default(),
        ..d.rectangle
    };
    d.arrow = ArrowStyle {
        stroke: style.color,
        stroke_width: style.stroke_width,
        end_arrowhead: Some(Arrowhead::Arrow),
        arrow_type: ArrowType::Straight,
        stroke_style: StrokeStyle::Solid,
        ..d.arrow
    };
    let line = |base: ShapeStyle| ShapeStyle {
        stroke: style.color,
        stroke_width: style.stroke_width,
        ..base
    };
    d.line = line(d.line);
    d.free_draw = line(d.free_draw);
    d.text = TextStyle {
        color: style.color,
        font_size: style.font_px,
        ..d.text
    };
    d
}

impl AnnotationLayer {
    /// 创建标注层。
    ///
    /// # 参数
    /// - `width` / `height`：画布物理尺寸（等于冻结底图尺寸）。
    /// - `dpr`：设备像素比（用于线宽、字号与滤镜强度）。
    ///
    /// # 返回
    /// 标注层；引擎初始化失败返回错误说明。
    ///
    /// ```
    /// use snow_shot::annotation::AnnotationLayer;
    /// let layer = AnnotationLayer::new(640, 480, 1.0).unwrap();
    /// assert!(!layer.can_undo());
    /// ```
    pub fn new(width: u32, height: u32, dpr: f32) -> Result<Self, String> {
        let style = AnnotationStyle::for_dpr(dpr);
        let engine = Engine::try_new(runtime_config(style)).map_err(|e| format!("初始化标注引擎失败: {e:?}"))?;
        Self::from_engine(engine, width, height, dpr, style)
    }

    /// 从引擎会话字节恢复标注层（含撤销 / 重做历史），元素仍可继续编辑。
    ///
    /// 恢复后调用 [`AnnotationLayer::refresh`] 取得需要显示的全部预览块。
    ///
    /// # 参数
    /// - `width` / `height`：画布物理尺寸（必须与保存时的底图一致）。
    /// - `dpr`：设备像素比（只影响之后新画的图形的默认线宽与字号）。
    /// - `session`：[`AnnotationLayer::serialize_session`] 产出的字节。
    ///
    /// # 返回
    /// 标注层；会话损坏或不兼容返回错误说明。
    ///
    /// ```
    /// use snow_shot::annotation::AnnotationLayer;
    /// let layer = AnnotationLayer::new(64, 64, 1.0).unwrap();
    /// let bytes = layer.serialize_session().unwrap();
    /// assert!(AnnotationLayer::from_session(64, 64, 1.0, &bytes).is_ok());
    /// assert!(AnnotationLayer::from_session(64, 64, 1.0, b"junk").is_err());
    /// ```
    pub fn from_session(width: u32, height: u32, dpr: f32, session: &[u8]) -> Result<Self, String> {
        let style = AnnotationStyle::for_dpr(dpr);
        let engine = Engine::from_serialized_document_session_with_config(session, runtime_config(style))
            .map_err(|e| format!("恢复标注会话失败: {e:?}"))?;
        Self::from_engine(engine, width, height, dpr, style)
    }

    /// 序列化当前标注会话（元素 + 撤销 / 重做历史），供贴图持久化。
    ///
    /// # 返回
    /// 会话字节；超出引擎上限等失败返回错误说明。
    pub fn serialize_session(&self) -> Result<Vec<u8>, String> {
        self.engine
            .serialize_document_session()
            .map_err(|e| format!("序列化标注会话失败: {e:?}"))
    }

    /// 按已有引擎构造标注层：建视口、设画布尺寸与恒等相机。
    fn from_engine(
        mut engine: Engine,
        width: u32,
        height: u32,
        dpr: f32,
        style: AnnotationStyle,
    ) -> Result<Self, String> {
        let viewport = engine
            .create_viewport(ViewportConfig::default())
            .map_err(|e| format!("创建视口失败: {e:?}"))?;
        engine
            .set_viewport_surface_size(viewport, width, height)
            .map_err(|e| format!("设置画布尺寸失败: {e:?}"))?;
        // 相机居中于画布中心、缩放 1：世界坐标 == 底图物理像素坐标
        engine
            .set_viewport_camera(
                viewport,
                Camera {
                    center: Point::new(f64::from(width) / 2.0, f64::from(height) / 2.0),
                    zoom: 1.0,
                },
            )
            .map_err(|e| format!("设置相机失败: {e:?}"))?;
        Ok(Self {
            engine,
            viewport,
            raster: TinySkiaRasterizer::new(RasterConfig {
                tile_size: TILE_SIZE,
                device_pixel_ratio: 1.0,
            }),
            width,
            height,
            dpr: if dpr.is_finite() && dpr > 0.0 { f64::from(dpr) } else { 1.0 },
            tool: AnnotationTool::None,
            style,
            emitted: HashMap::new(),
            text_cache: HashMap::new(),
            drawing: false,
        })
    }

    /// 当前工具。
    pub fn tool(&self) -> AnnotationTool {
        self.tool
    }

    /// 当前样式。
    pub fn style(&self) -> AnnotationStyle {
        self.style
    }

    /// 是否处于按下绘制中。
    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// 是否可撤销。
    pub fn can_undo(&self) -> bool {
        self.engine.history_state().can_undo
    }

    /// 是否可重做。
    pub fn can_redo(&self) -> bool {
        self.engine.history_state().can_redo
    }

    /// 场景内元素数量（含文字与滤镜）。
    pub fn item_count(&self) -> usize {
        self.raster.item_count()
    }

    /// 切换工具；矩形 / 椭圆共用引擎的形状工具，滤镜工具按马赛克 / 模糊切换类型。
    ///
    /// # 参数
    /// - `tool`：新工具；`None` 表示不再接收标注输入。
    ///
    /// # 返回
    /// 失败时返回引擎错误说明。
    pub fn set_tool(&mut self, tool: AnnotationTool) -> Result<(), String> {
        self.tool = tool;
        let Some(active) = engine_tool(tool) else {
            return Ok(());
        };
        let err = |e| format!("切换标注工具失败: {e:?}");
        self.engine
            .set_viewport_active_tool(self.viewport, active)
            .map_err(err)?;
        match tool {
            AnnotationTool::Rectangle | AnnotationTool::Ellipse => {
                let mut style = self.engine.viewport_rectangle_shape_style(self.viewport).map_err(err)?;
                style.shape = if tool == AnnotationTool::Ellipse {
                    HighlightShape::Ellipse
                } else {
                    HighlightShape::Rectangle
                };
                self.engine
                    .set_viewport_rectangle_shape_style(self.viewport, style)
                    .map_err(err)?;
            }
            AnnotationTool::Mosaic | AnnotationTool::Blur => {
                let (filter_type, strength) = if tool == AnnotationTool::Blur {
                    (snow_draw_engine::CanvasFilterType::GaussianBlur, BLUR_STRENGTH)
                } else {
                    (snow_draw_engine::CanvasFilterType::Mosaic, MOSAIC_STRENGTH)
                };
                self.engine
                    .set_viewport_filter_style(
                        self.viewport,
                        FilterStyle {
                            filter_type,
                            strength,
                            ..FilterStyle::default()
                        },
                        FILTER_STYLE_PROPERTY_ALL,
                    )
                    .map_err(err)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// 把工具样式下发给引擎：作用于该工具后续的绘制，也作用于当前选中的同类对象。
    ///
    /// # 参数
    /// - `tool`：样式所属工具；没有样式的工具直接返回空更新。
    /// - `style`：该工具的样式（逻辑像素，内部乘设备像素比）。
    /// - `base`：冻结底图。
    ///
    /// # 返回
    /// 预览层增量更新（作用到已有对象时才会有内容）。
    ///
    /// ```ignore
    /// let update = layer.apply_style(AnnotationTool::Line, &default_style(AnnotationTool::Line), base)?;
    /// ```
    pub fn apply_style(
        &mut self,
        tool: AnnotationTool,
        style: &ToolStyle,
        base: BaseView,
    ) -> Result<LayerUpdate, String> {
        let err = |e| format!("应用标注样式失败: {e:?}");
        let state = self.engine.viewport_style_toolbar_state(self.viewport).map_err(err)?;
        if let Some(patch) = shape_patch(tool, style, self.dpr, state.shape_style) {
            self.engine
                .set_viewport_shape_style_patch(self.viewport, patch)
                .map_err(err)?;
        } else if tool == AnnotationTool::Text {
            let font_px = physical_px(style.font_size, self.dpr);
            self.style.color = engine_color(style.color);
            self.style.font_px = font_px;
            let text = TextStyle {
                color: engine_color(style.color),
                font_size: font_px,
                ..state.text_style
            };
            self.engine
                .set_viewport_text_style_patch(
                    self.viewport,
                    text,
                    TEXT_STYLE_MIXED_COLOR | TEXT_STYLE_MIXED_FONT_SIZE,
                    &[],
                )
                .map_err(err)?;
        } else if tool == AnnotationTool::Counter {
            let serial = snow_draw_engine::SerialNumberStyle {
                serial_number_type: SerialNumberType::SolidCircle,
                color: engine_color(style.color),
                font_size: physical_px(style.font_size, self.dpr),
                stroke_width: physical_px(COUNTER_RING_LOGICAL, self.dpr),
                ..state.serial_number_style
            };
            self.engine
                .set_viewport_serial_number_style(self.viewport, serial)
                .map_err(err)?;
        } else {
            return Ok(LayerUpdate::default());
        }
        self.sync(base)
    }

    /// 底图视图转合成用参数。
    fn base_ok(&self, base: BaseView) -> bool {
        base.width == self.width
            && base.height == self.height
            && base.bgra.len() == self.width as usize * self.height as usize * BPP
    }

    /// 向引擎喂一个指针事件。
    fn feed(&mut self, kind: PointerEventType, x: f64, y: f64) -> Result<(), String> {
        let pressed = kind != PointerEventType::Up;
        let event = PointerEvent {
            pointer_id: POINTER_ID,
            event_type: kind,
            device: PointerDevice::Mouse,
            position: Point::new(x, y),
            button: (kind != PointerEventType::Move).then_some(PointerButton::Primary),
            buttons: PointerButtons(if pressed { PointerButtons::PRIMARY } else { 0 }),
            modifiers: Modifiers::default(),
        };
        self.engine
            .process_input_with_viewport_changes(self.viewport, InputEvent::Pointer(event))
            .map(|_| ())
            .map_err(|e| format!("标注输入处理失败: {e:?}"))
    }

    /// 左键按下：开始绘制（工具为空或文字工具时忽略）。
    ///
    /// # 参数
    /// - `x` / `y`：画布物理坐标。
    /// - `base`：冻结底图（滤镜合成用）。
    ///
    /// # 返回
    /// 预览层增量更新。
    pub fn pointer_down(&mut self, x: f64, y: f64, base: BaseView) -> Result<LayerUpdate, String> {
        if !self.accepts_pointer() {
            return Ok(LayerUpdate::default());
        }
        self.drawing = true;
        self.feed(PointerEventType::Down, x, y)?;
        self.sync(base)
    }

    /// 指针移动：绘制中才更新。
    ///
    /// # 参数
    /// - `x` / `y`：画布物理坐标。
    /// - `base`：冻结底图。
    pub fn pointer_move(&mut self, x: f64, y: f64, base: BaseView) -> Result<LayerUpdate, String> {
        if !self.drawing {
            return Ok(LayerUpdate::default());
        }
        self.feed(PointerEventType::Move, x, y)?;
        self.sync(base)
    }

    /// 左键松开：结束一次绘制。
    ///
    /// # 参数
    /// - `x` / `y`：画布物理坐标。
    /// - `base`：冻结底图。
    pub fn pointer_up(&mut self, x: f64, y: f64, base: BaseView) -> Result<LayerUpdate, String> {
        if !self.drawing {
            return Ok(LayerUpdate::default());
        }
        self.drawing = false;
        self.feed(PointerEventType::Up, x, y)?;
        self.sync(base)
    }

    /// 当前工具是否通过指针拖动创建元素（文字工具走单击 + 输入框）。
    pub fn accepts_pointer(&self) -> bool {
        !matches!(
            self.tool,
            AnnotationTool::None | AnnotationTool::Text
        ) && engine_tool(self.tool).is_some()
    }

    /// 撤销上一步。
    pub fn undo(&mut self, base: BaseView) -> Result<LayerUpdate, String> {
        self.engine
            .undo_with_viewport_changes()
            .map_err(|e| format!("撤销失败: {e:?}"))?;
        self.sync(base)
    }

    /// 重做上一步。
    pub fn redo(&mut self, base: BaseView) -> Result<LayerUpdate, String> {
        self.engine
            .redo_with_viewport_changes()
            .map_err(|e| format!("重做失败: {e:?}"))?;
        self.sync(base)
    }

    /// 文字元素的字体族（默认字体）。
    fn font_family(&self) -> &'static str {
        DEFAULT_FONT_FAMILY
    }

    /// 测量文本占用尺寸（物理像素）。
    ///
    /// # 参数
    /// - `text`：文本（`\n` 分行）。
    pub fn measure_text(&self, text: &str) -> Result<(u32, u32), String> {
        text_raster::measure_text(text, self.font_family(), self.style.font_px as f32, false)
            .map(|m| (m.width, m.height))
    }

    /// 在画布上落一段文字；空白文本忽略。
    ///
    /// # 参数
    /// - `left` / `top`：文字外框左上角（画布物理坐标）。
    /// - `text`：文本内容。
    /// - `base`：冻结底图。
    ///
    /// # 返回
    /// 预览层增量更新。
    pub fn commit_text(
        &mut self,
        left: f64,
        top: f64,
        text: &str,
        base: BaseView,
    ) -> Result<LayerUpdate, String> {
        if text.trim().is_empty() {
            return Ok(LayerUpdate::default());
        }
        let (w, h) = self.measure_text(text)?;
        let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
        let layout = TextLayoutSize::new(w, h);
        self.engine
            .create_text_with_viewport_changes(
                self.viewport,
                Point::new(left + w / 2.0, top + h / 2.0),
                text,
                layout,
            )
            .map_err(|e| format!("创建文字标注失败: {e:?}"))?;
        self.sync(base)
    }

    /// 重新光栅化并输出当前应显示的预览块（恢复会话后取得全部块；平时增量更新已自动完成）。
    ///
    /// # 参数
    /// - `base`：冻结底图。
    pub fn refresh(&mut self, base: BaseView) -> Result<LayerUpdate, String> {
        self.sync(base)
    }

    /// 拉取引擎补丁、光栅化脏区，并把受影响的块合成为预览块。
    fn sync(&mut self, base: BaseView) -> Result<LayerUpdate, String> {
        if !self.base_ok(base) {
            return Err("底图尺寸与标注画布不一致".into());
        }
        let patch = self
            .engine
            .acquire_patch(self.viewport, self.raster.cursor())
            .map_err(|e| format!("获取标注补丁失败: {e:?}"))?;
        let out = match self.raster.apply_patch(&patch) {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(error = %e, "增量补丁应用失败，重置光栅化器后整幅重绘");
                // 整幅重绘会触及全部块，已显示的块会在 build_update 里被更新或释放
                self.raster.reset();
                let full = self
                    .engine
                    .acquire_patch(self.viewport, None)
                    .map_err(|e| format!("获取整幅补丁失败: {e:?}"))?;
                self.raster.apply_patch(&full).map_err(|e| e.to_string())?
            }
        };
        Ok(self.build_update(out, base))
    }

    /// 由光栅输出生成预览增量：在触及的块上叠加滤镜与文字，并跟踪释放。
    fn build_update(&mut self, out: RasterOutput, base: BaseView) -> LayerUpdate {
        let mut vectors: HashMap<TileKey, RasterTile> =
            out.tiles.into_iter().map(|t| (t.key, t)).collect();
        let mut keys: Vec<TileKey> = out.touched_tiles;
        for key in out.released {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut update = LayerUpdate::default();
        let mut filters: HashMap<usize, Option<FilterRender>> = HashMap::new();
        for key in keys {
            let rect = self.tile_rect(key);
            let vector = vectors.remove(&key).map(|t| t.rgba);
            let layer = finish_layer(
                rect,
                vector,
                self.raster.scene_items(),
                base,
                self.dpr,
                &mut filters,
                &mut self.text_cache,
            );
            match layer {
                Some(mut rgba) => {
                    swap_rb(&mut rgba);
                    // 引擎的脏区是新旧包围盒的并集，其中很多块内容并没有变：哈希相同就不再上传
                    let hash = hash_pixels(&rgba);
                    if self.emitted.insert(key, hash) == Some(hash) {
                        continue;
                    }
                    let (w, h) = rect_size(rect);
                    update.touched_pixels += (w * h) as u64;
                    update.tiles.push(TileImage {
                        key,
                        x: rect[0] as u32,
                        y: rect[1] as u32,
                        w: w as u32,
                        h: h as u32,
                        bgra: rgba,
                    });
                }
                None => {
                    if self.emitted.remove(&key).is_some() {
                        update.released.push(key);
                    }
                }
            }
        }
        update
    }

    /// 分块在画布上的矩形。
    fn tile_rect(&self, key: TileKey) -> IntRect {
        let x0 = key.col * TILE_SIZE;
        let y0 = key.row * TILE_SIZE;
        [
            x0 as i32,
            y0 as i32,
            (x0 + TILE_SIZE).min(self.width) as i32,
            (y0 + TILE_SIZE).min(self.height) as i32,
        ]
    }

    /// 导出选区：底图裁切后叠加标注，返回不透明 RGBA。
    ///
    /// 与屏幕预览使用同一套合成函数，因此像素一致。
    ///
    /// # 参数
    /// - `rect`：选区（画布物理坐标，会被裁进画布）。
    /// - `base`：冻结底图。
    ///
    /// # 返回
    /// `(宽, 高, RGBA)`；选区与画布不相交返回 `None`。
    pub fn export_rgba(&mut self, rect: IntRect, base: BaseView) -> Option<(u32, u32, Vec<u8>)> {
        if !self.base_ok(base) {
            return None;
        }
        let rect = intersect(rect, [0, 0, self.width as i32, self.height as i32])?;
        let (w, h) = rect_size(rect);
        let mut out = Vec::with_capacity(w * h * BPP);
        for row in 0..h {
            let off = ((rect[1] as usize + row) * self.width as usize + rect[0] as usize) * BPP;
            for p in base.bgra[off..off + w * BPP].chunks_exact(BPP) {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        if self.item_count() == 0 {
            return Some((w as u32, h as u32, out));
        }
        let vector = self.raster.canvas_rgba().and_then(|(cw, _, data)| {
            let mut crop = Vec::with_capacity(w * h * BPP);
            for row in 0..h {
                let off = ((rect[1] as usize + row) * cw as usize + rect[0] as usize) * BPP;
                crop.extend_from_slice(data.get(off..off + w * BPP)?);
            }
            (!all_transparent(&crop)).then_some(crop)
        });
        let mut filters = HashMap::new();
        if let Some(layer) = finish_layer(
            rect,
            vector,
            self.raster.scene_items(),
            base,
            self.dpr,
            &mut filters,
            &mut self.text_cache,
        ) {
            // 图层预乘 RGBA 叠在不透明底图上：out = layer + out * (1 - a)
            source_over(&mut out, &layer);
        }
        Some((w as u32, h as u32, out))
    }
}

/// 合成一块区域的图层（预乘 RGBA）：滤镜 -> 矢量 -> 文字；全透明返回 `None`。
///
/// # 参数
/// - `rect`：区域（画布物理坐标）。
/// - `vector`：该区域的矢量层像素（预乘 RGBA，无内容为 `None`）。
/// - `items`：场景全部元素（z 序）。
/// - `base`：冻结底图。
/// - `dpr`：设备像素比。
/// - `filters`：本帧滤镜结果缓存（键为元素序号），跨块复用。
/// - `text_cache`：文字光栅缓存。
fn finish_layer(
    rect: IntRect,
    vector: Option<Vec<u8>>,
    items: &[SceneDisplayItem],
    base: BaseView,
    dpr: f64,
    filters: &mut HashMap<usize, Option<FilterRender>>,
    text_cache: &mut HashMap<TextKey, Option<Arc<TextBitmap>>>,
) -> Option<Vec<u8>> {
    let deferred: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| deferred_hits(item, rect))
        .map(|(i, _)| i)
        .collect();
    if deferred.is_empty() {
        return vector.filter(|v| !all_transparent(v));
    }
    let (w, h) = rect_size(rect);
    let mut layer = vec![0u8; w * h * BPP];
    for &index in &deferred {
        if let SceneDisplayItem::Filter(item) = &items[index] {
            let render = filters
                .entry(index)
                .or_insert_with(|| render_filter(item, base, dpr));
            if let Some(render) = render {
                draw_filter(&mut layer, rect, render, base);
            }
        }
    }
    if let Some(vector) = &vector {
        source_over(&mut layer, vector);
    }
    for &index in &deferred {
        match &items[index] {
            SceneDisplayItem::Text(item) => draw_text(&mut layer, rect, item, text_cache),
            SceneDisplayItem::SerialNumber(item) => {
                draw_text(&mut layer, rect, &serial_digit_item(item), text_cache);
            }
            _ => {}
        }
    }
    (!all_transparent(&layer)).then_some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation_style::{ArrowheadChoice, default_style};

    /// 构造渐变底图（BGRA 不透明）。
    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255]);
            }
        }
        data
    }

    /// 取 RGBA 缓冲中某像素。
    fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
    }

    /// 用给定工具在 `(x0,y0)-(x1,y1)` 拖出一个标注，返回各步更新。
    fn drag(
        layer: &mut AnnotationLayer,
        base: BaseView,
        from: (f64, f64),
        to: (f64, f64),
    ) -> Vec<LayerUpdate> {
        let mut out = vec![layer.pointer_down(from.0, from.1, base).unwrap()];
        for i in 1..=8 {
            let t = f64::from(i) / 8.0;
            out.push(
                layer
                    .pointer_move(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, base)
                    .unwrap(),
            );
        }
        out.push(layer.pointer_up(to.0, to.1, base).unwrap());
        out
    }

    /// 工具栏工具到引擎工具的映射。
    #[test]
    fn tool_mapping() {
        assert_eq!(engine_tool(AnnotationTool::Rectangle), Some(ActiveTool::Shape));
        assert_eq!(engine_tool(AnnotationTool::Ellipse), Some(ActiveTool::Shape));
        assert_eq!(engine_tool(AnnotationTool::Pencil), Some(ActiveTool::FreeDraw));
        assert_eq!(engine_tool(AnnotationTool::Mosaic), Some(ActiveTool::RectangleFilter));
        assert_eq!(engine_tool(AnnotationTool::None), None);
    }

    /// 预乘 source-over 的边界：透明不变、不透明覆盖、半透明混合。
    #[test]
    fn source_over_rules() {
        let mut d = [10, 20, 30, 255];
        source_over(&mut d, &[0, 0, 0, 0]);
        assert_eq!(d, [10, 20, 30, 255]);
        source_over(&mut d, &[1, 2, 3, 255]);
        assert_eq!(d, [1, 2, 3, 255]);
        let mut d = [200, 200, 200, 255];
        source_over(&mut d, &[100, 0, 0, 128]);
        assert_eq!(d[3], 255);
        assert!(d[0] > 190 && d[1] < 120, "半透明红色叠在灰底上: {d:?}");
    }

    /// 非法 / 不匹配的底图被拒绝而不是 panic。
    #[test]
    fn mismatched_base_is_rejected() {
        let mut layer = AnnotationLayer::new(64, 64, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        let small = gradient(8, 8);
        let bad = BaseView { width: 8, height: 8, bgra: &small };
        assert!(layer.pointer_down(1.0, 1.0, bad).is_err());
    }

    /// 矩形：拖动后描边落在预期位置，内部透明（无填充）。
    #[test]
    fn rectangle_draws_outline() {
        let (w, h) = (400, 300);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        drag(&mut layer, base, (50.0, 40.0), (250.0, 200.0));
        assert_eq!(layer.item_count(), 1);
        let (ew, eh, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        assert_eq!((ew, eh), (w, h));
        // 左边线中部应是红色描边（BGRA 底图导出后为 RGBA）
        let edge = px(&rgba, w, 50, 120);
        assert!(edge[0] > 200 && edge[1] < 90 && edge[2] < 90, "左边线像素 {edge:?}");
        // 内部不填充：应等于底图像素
        let inner = px(&rgba, w, 150, 120);
        let b = &data[((120 * w + 150) * 4) as usize..];
        assert_eq!(inner, [b[2], b[1], b[0], 255]);
    }

    /// 椭圆：外接矩形角点无描边，四边中点有描边。
    #[test]
    fn ellipse_draws_curve_not_corners() {
        let (w, h) = (400, 300);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Ellipse).unwrap();
        drag(&mut layer, base, (100.0, 60.0), (300.0, 240.0));
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let top_mid = px(&rgba, w, 200, 60);
        assert!(top_mid[0] > 200 && top_mid[1] < 90, "上边中点 {top_mid:?}");
        let corner = px(&rgba, w, 102, 62);
        let b = &data[((62 * w + 102) * 4) as usize..];
        assert_eq!(corner, [b[2], b[1], b[0], 255], "角点不应被描边");
    }

    /// 箭头、直线、画笔都能产生像素。
    #[test]
    fn arrow_line_pencil_produce_pixels() {
        for tool in [AnnotationTool::Arrow, AnnotationTool::Line, AnnotationTool::Pencil] {
            let (w, h) = (300, 200);
            let data = gradient(w, h);
            let base = BaseView { width: w, height: h, bgra: &data };
            let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
            layer.set_tool(tool).unwrap();
            drag(&mut layer, base, (30.0, 30.0), (250.0, 150.0));
            assert_eq!(layer.item_count(), 1, "{tool:?} 应创建 1 个元素");
            let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
            let changed = (0..w * h).filter(|i| {
                let o = (*i * 4) as usize;
                let b = &data[o..o + 4];
                rgba[o..o + 3] != [b[2], b[1], b[0]]
            });
            assert!(changed.count() > 100, "{tool:?} 没有画出像素");
        }
    }

    /// 拖动的增量更新只输出局部块（不是整屏），并复用画布。
    #[test]
    fn drag_updates_are_partial_tiles() {
        let (w, h) = (1920, 1080);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Arrow).unwrap();
        let updates = drag(&mut layer, base, (100.0, 100.0), (400.0, 260.0));
        let total_tiles = (w.div_ceil(TILE_SIZE) * h.div_ceil(TILE_SIZE)) as usize;
        let max_tiles = updates.iter().map(|u| u.tiles.len()).max().unwrap();
        assert!(max_tiles > 0 && max_tiles < total_tiles / 4, "单次更新块数 {max_tiles}/{total_tiles}");
        let touched: u64 = updates.iter().map(|u| u.touched_pixels).max().unwrap();
        assert!(touched < u64::from(w * h) / 4);
    }

    /// 撤销 / 重做：元素数量与导出像素随之变化，且可再次重做。
    #[test]
    fn undo_redo_roundtrip() {
        let (w, h) = (200, 150);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        drag(&mut layer, base, (20.0, 20.0), (120.0, 100.0));
        assert!(layer.can_undo() && !layer.can_redo());
        let with = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap().2;
        let update = layer.undo(base).unwrap();
        assert!(!update.is_empty());
        assert_eq!(layer.item_count(), 0);
        assert!(layer.can_redo());
        let without = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap().2;
        assert_ne!(with, without);
        layer.redo(base).unwrap();
        let again = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap().2;
        assert_eq!(with, again);
    }

    /// 马赛克：区域内出现分块（同一块内像素相同），区域外保持原样。
    #[test]
    fn mosaic_pixelates_region_only() {
        let (w, h) = (300, 200);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Mosaic).unwrap();
        drag(&mut layer, base, (50.0, 40.0), (200.0, 150.0));
        assert_eq!(layer.item_count(), 1);
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        // 区域外不变
        let outside = px(&rgba, w, 10, 10);
        let b = &data[((10 * w + 10) * 4) as usize..];
        assert_eq!(outside, [b[2], b[1], b[0], 255]);
        // 区域内：相邻像素成块相同，且整体与底图不同
        let mut same_neighbors = 0;
        let mut differs = 0;
        for y in 60..140 {
            for x in 70..180 {
                let p = px(&rgba, w, x, y);
                if p == px(&rgba, w, x + 1, y) {
                    same_neighbors += 1;
                }
                let o = ((y * w + x) * 4) as usize;
                if p[..3] != [data[o + 2], data[o + 1], data[o]] {
                    differs += 1;
                }
            }
        }
        assert!(same_neighbors > 80 * 110 * 3 / 4, "马赛克块内应相同: {same_neighbors}");
        assert!(differs > 1000, "马赛克应改变像素: {differs}");
    }

    /// 模糊：区域内高频渐变被平滑（相邻像素差变小）。
    #[test]
    fn blur_smooths_region() {
        let (w, h) = (300, 200);
        // 棋盘底图：高频
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 2 + y / 2) % 2 == 0 { 250 } else { 10 };
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Blur).unwrap();
        drag(&mut layer, base, (40.0, 30.0), (240.0, 170.0));
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let mut diff_in = 0u32;
        let mut diff_orig = 0u32;
        for y in 60..140u32 {
            for x in 80..200u32 {
                diff_in += u32::from(px(&rgba, w, x, y)[0].abs_diff(px(&rgba, w, x + 1, y)[0]));
                let o = ((y * w + x) * 4) as usize;
                diff_orig += u32::from(data[o].abs_diff(data[o + 4]));
            }
        }
        assert!(diff_in * 3 < diff_orig, "模糊后相邻差 {diff_in} 应远小于原图 {diff_orig}");
    }

    /// 文字：提交后导出含有文字颜色像素；空白文本不创建元素；撤销可移除。
    #[test]
    fn text_commit_and_export() {
        let (w, h) = (400, 200);
        let data = vec![255u8; (w * h * 4) as usize];
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Text).unwrap();
        assert!(layer.commit_text(10.0, 10.0, "   ", base).unwrap().is_empty());
        assert_eq!(layer.item_count(), 0);
        let update = layer.commit_text(20.0, 30.0, "你好 Snow", base).unwrap();
        assert!(!update.tiles.is_empty());
        assert_eq!(layer.item_count(), 1);
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let red = rgba
            .chunks_exact(4)
            .filter(|p| p[0] > 200 && p[1] < 100 && p[2] < 100)
            .count();
        assert!(red > 80, "文字应画出红色像素，实际 {red}");
        // 红色像素的包围盒应落在提交位置附近（左上角 20,30），而不是画布别处
        let (mut min_x, mut min_y) = (w, h);
        for (i, p) in rgba.chunks_exact(4).enumerate() {
            if p[0] > 200 && p[1] < 100 && p[2] < 100 {
                min_x = min_x.min(i as u32 % w);
                min_y = min_y.min(i as u32 / w);
            }
        }
        assert!((18..=40).contains(&min_x) && (28..=50).contains(&min_y), "文字位置偏离: ({min_x},{min_y})");
        layer.undo(base).unwrap();
        assert_eq!(layer.item_count(), 0);
        let (_, _, blank) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        assert!(blank.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    }

    /// 导出选区只包含选区内的像素，且与整幅导出裁切一致（合成正确性对拍）。
    #[test]
    fn export_crop_matches_full_composite() {
        let (w, h) = (500, 400);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        drag(&mut layer, base, (60.0, 50.0), (300.0, 260.0));
        layer.set_tool(AnnotationTool::Mosaic).unwrap();
        drag(&mut layer, base, (200.0, 150.0), (420.0, 330.0));
        layer.set_tool(AnnotationTool::Text).unwrap();
        layer.commit_text(80.0, 70.0, "Crop", base).unwrap();
        let full = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap().2;
        // 选区跨越多个 256 分块边界
        let sel = [180, 120, 470, 380];
        let (cw, ch, crop) = layer.export_rgba(sel, base).unwrap();
        assert_eq!((cw, ch), (290, 260));
        for y in 0..ch {
            for x in 0..cw {
                assert_eq!(
                    px(&crop, cw, x, y),
                    px(&full, w, x + 180, y + 120),
                    "裁切与整幅合成在 ({x},{y}) 不一致"
                );
            }
        }
    }

    /// 预览块合成 == 导出：把预览块按位置铺回、叠到底图，与导出逐像素一致。
    #[test]
    fn preview_tiles_equal_export() {
        let (w, h) = (700, 500);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        let mut canvas: HashMap<TileKey, TileImage> = HashMap::new();
        let apply = |u: LayerUpdate, canvas: &mut HashMap<TileKey, TileImage>| {
            for t in u.tiles {
                canvas.insert(t.key, t);
            }
            for k in u.released {
                canvas.remove(&k);
            }
        };
        layer.set_tool(AnnotationTool::Arrow).unwrap();
        for u in drag(&mut layer, base, (60.0, 50.0), (600.0, 420.0)) {
            apply(u, &mut canvas);
        }
        layer.set_tool(AnnotationTool::Mosaic).unwrap();
        for u in drag(&mut layer, base, (100.0, 200.0), (350.0, 330.0)) {
            apply(u, &mut canvas);
        }
        layer.set_tool(AnnotationTool::Text).unwrap();
        apply(layer.commit_text(300.0, 100.0, "Preview", base).unwrap(), &mut canvas);
        layer.set_tool(AnnotationTool::Pencil).unwrap();
        for u in drag(&mut layer, base, (400.0, 300.0), (650.0, 100.0)) {
            apply(u, &mut canvas);
        }
        apply(layer.undo(base).unwrap(), &mut canvas);
        // 用预览块重建整幅
        let mut rebuilt = Vec::with_capacity(data.len());
        for p in data.chunks_exact(4) {
            rebuilt.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        for t in canvas.values() {
            for row in 0..t.h {
                for col in 0..t.w {
                    let s = ((row * t.w + col) * 4) as usize;
                    let d = (((t.y + row) * w + t.x + col) * 4) as usize;
                    // 预览块是预乘 BGRA -> 先转 RGBA 再叠加
                    let src = [t.bgra[s + 2], t.bgra[s + 1], t.bgra[s], t.bgra[s + 3]];
                    source_over(&mut rebuilt[d..d + 4], &src);
                }
            }
        }
        let exported = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap().2;
        assert_eq!(rebuilt, exported, "预览块合成结果应与导出逐像素一致");
    }

    /// 内容没变的分块不重复输出：原地不动的移动事件产生空更新。
    #[test]
    fn unchanged_tiles_are_not_reemitted() {
        let (w, h) = (800, 600);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Arrow).unwrap();
        layer.pointer_down(100.0, 100.0, base).unwrap();
        let first = layer.pointer_move(500.0, 400.0, base).unwrap();
        assert!(!first.tiles.is_empty());
        let again = layer.pointer_move(500.0, 400.0, base).unwrap();
        assert!(again.is_empty(), "原地不动不应再上传分块: {} 块", again.tiles.len());
    }

    /// 像素哈希：相同内容相同、单字节变化不同、长度不同不同。
    #[test]
    fn pixel_hash_detects_changes() {
        let a = vec![7u8; 1024];
        let mut b = a.clone();
        assert_eq!(hash_pixels(&a), hash_pixels(&b));
        b[1000] = 8;
        assert_ne!(hash_pixels(&a), hash_pixels(&b));
        assert_ne!(hash_pixels(&a), hash_pixels(&a[..1023]));
    }

    /// 未选工具 / 文字工具下指针事件被忽略。
    #[test]
    fn pointer_ignored_without_drawing_tool() {
        let (w, h) = (100, 100);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        assert!(layer.pointer_down(10.0, 10.0, base).unwrap().is_empty());
        assert!(!layer.is_drawing());
        layer.set_tool(AnnotationTool::Text).unwrap();
        assert!(!layer.accepts_pointer());
        assert!(layer.pointer_down(10.0, 10.0, base).unwrap().is_empty());
    }

    /// 白底画布像素缓冲（BGRA 全 255）。
    fn white(w: u32, h: u32) -> Vec<u8> {
        vec![255u8; (w * h * 4) as usize]
    }

    /// 荧光笔与序号球映射到引擎的对应工具。
    #[test]
    fn highlighter_and_counter_map_to_engine_tools() {
        assert_eq!(engine_tool(AnnotationTool::Highlighter), Some(ActiveTool::PenHighlight));
        assert_eq!(engine_tool(AnnotationTool::Counter), Some(ActiveTool::SerialNumber));
    }

    /// 改色与线宽后，后续绘制按新样式出图（颜色为蓝，线宽约 12）。
    #[test]
    fn style_color_and_width_apply_to_next_drawing() {
        let (w, h) = (300, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Line).unwrap();
        let mut style = default_style(AnnotationTool::Line);
        style.color = [0x16, 0x77, 0xFF, 0xFF];
        style.width = 12;
        layer.apply_style(AnnotationTool::Line, &style, base).unwrap();
        drag(&mut layer, base, (40.0, 100.0), (240.0, 100.0));
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let mid = px(&rgba, w, 140, 100);
        assert!(mid[2] > 200 && mid[0] < 60, "应为蓝色: {mid:?}");
        let thickness = (0..h).filter(|&y| px(&rgba, w, 140, y)[0] < 128).count();
        assert!((10..=14).contains(&thickness), "线宽应约 12px，实际 {thickness}");
    }

    /// 设备像素比 2 时，逻辑线宽乘 2 下发引擎。
    #[test]
    fn style_width_scales_with_dpr() {
        let (w, h) = (300, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 2.0).unwrap();
        layer.set_tool(AnnotationTool::Line).unwrap();
        let mut style = default_style(AnnotationTool::Line);
        style.width = 4;
        layer.apply_style(AnnotationTool::Line, &style, base).unwrap();
        drag(&mut layer, base, (40.0, 100.0), (240.0, 100.0));
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let thickness = (0..h).filter(|&y| px(&rgba, w, 140, y)[1] < 128).count();
        assert!((7..=9).contains(&thickness), "4 逻辑像素 @2x 应约 8 物理像素，实际 {thickness}");
    }

    /// 形状填充开关：开启后内部被半透明描边色覆盖。
    #[test]
    fn rectangle_fill_toggle() {
        let (w, h) = (300, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        let mut style = default_style(AnnotationTool::Rectangle);
        style.fill = true;
        layer.apply_style(AnnotationTool::Rectangle, &style, base).unwrap();
        drag(&mut layer, base, (40.0, 40.0), (240.0, 160.0));
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let inner = px(&rgba, w, 140, 100);
        assert!(inner[0] > 200 && inner[1] < 245 && inner[1] > 100, "内部应是淡红填充: {inner:?}");
    }

    /// 箭头头型：选“无”时末端没有头部，比标准箭头少很多像素。
    #[test]
    fn arrowhead_none_draws_fewer_pixels() {
        let (w, h) = (300, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let count = |head: ArrowheadChoice| {
            let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
            layer.set_tool(AnnotationTool::Arrow).unwrap();
            let mut style = default_style(AnnotationTool::Arrow);
            style.arrowhead = head;
            style.width = 4;
            layer.apply_style(AnnotationTool::Arrow, &style, base).unwrap();
            drag(&mut layer, base, (40.0, 100.0), (240.0, 100.0));
            let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
            rgba.chunks_exact(4).filter(|p| p[1] < 128).count()
        };
        let none = count(ArrowheadChoice::None);
        let arrow = count(ArrowheadChoice::Arrow);
        assert!(none > 0 && arrow > none + 20, "无头 {none} 像素，标准箭头 {arrow} 像素");
    }

    /// 荧光笔：画出半透明粗线，底图仍能透出来。
    #[test]
    fn highlighter_draws_translucent_stroke() {
        let (w, h) = (300, 200);
        let data = gradient(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Highlighter).unwrap();
        layer
            .apply_style(AnnotationTool::Highlighter, &default_style(AnnotationTool::Highlighter), base)
            .unwrap();
        drag(&mut layer, base, (40.0, 100.0), (240.0, 100.0));
        assert_eq!(layer.item_count(), 1);
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let hit = px(&rgba, w, 140, 100);
        let b = &data[((100 * w + 140) * 4) as usize..];
        assert_ne!(hit, [b[2], b[1], b[0], 255], "荧光笔应改变像素");
        assert_ne!(hit, [0xFF, 0xD6, 0x0A, 255], "不应完全覆盖底图");
        let rows = (0..h)
            .filter(|&y| {
                let o = ((y * w + 140) * 4) as usize;
                px(&rgba, w, 140, y) != [data[o + 2], data[o + 1], data[o], 255]
            })
            .count();
        assert!((16..=24).contains(&rows), "荧光笔线宽应约 20，实际 {rows}");
    }

    /// 序号球：每次点击落一个球，数字依次递增，球体与数字都画出来。
    #[test]
    fn counter_places_numbered_balls() {
        let (w, h) = (300, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        layer.set_tool(AnnotationTool::Counter).unwrap();
        let mut style = default_style(AnnotationTool::Counter);
        style.color = [0x16, 0x77, 0xFF, 0xFF];
        layer.apply_style(AnnotationTool::Counter, &style, base).unwrap();
        for x in [80.0, 200.0] {
            layer.pointer_down(x, 100.0, base).unwrap();
            layer.pointer_up(x, 100.0, base).unwrap();
        }
        assert_eq!(layer.item_count(), 2);
        let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
        let blue = |x0: u32, x1: u32| {
            (60..140)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let p = px(&rgba, w, x, y);
                    p[2] > 200 && p[0] < 80
                })
                .count()
        };
        assert!(blue(50, 110) > 100, "第一个球应是蓝色实心");
        assert!(blue(170, 230) > 100, "第二个球应是蓝色实心");
        let rgba_ref = &rgba;
        let patch = |x0: u32| {
            (x0..x0 + 30)
                .flat_map(|x| (85..116).map(move |y| px(rgba_ref, w, x, y)))
                .collect::<Vec<_>>()
        };
        let white_dots = patch(65).iter().filter(|p| p[..3] == [255, 255, 255]).count();
        assert!(white_dots > 5, "球内应有白色数字，实际白点 {white_dots}");
        assert_ne!(patch(65), patch(185), "两个球的数字应不同");
    }

    /// 文字样式：字号与颜色作用于之后提交的文字。
    #[test]
    fn text_style_affects_committed_text() {
        let (w, h) = (400, 200);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let measure = |font: u32| {
            let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
            layer.set_tool(AnnotationTool::Text).unwrap();
            let mut style = default_style(AnnotationTool::Text);
            style.font_size = font;
            style.color = [0x16, 0x77, 0xFF, 0xFF];
            layer.apply_style(AnnotationTool::Text, &style, base).unwrap();
            layer.commit_text(20.0, 30.0, "Snow", base).unwrap();
            let (_, _, rgba) = layer.export_rgba([0, 0, w as i32, h as i32], base).unwrap();
            let blue = rgba.chunks_exact(4).filter(|p| p[2] > 200 && p[0] < 80).count();
            (layer.measure_text("Snow").unwrap().0, blue)
        };
        let (small_w, small_blue) = measure(16);
        let (big_w, big_blue) = measure(48);
        assert!(big_w > small_w * 2, "字号变大文字应更宽: {small_w} -> {big_w}");
        assert!(small_blue > 20 && big_blue > small_blue, "文字应是蓝色: {small_blue} / {big_blue}");
    }

    /// 没有样式的工具（马赛克 / 无）不产生更新也不报错。
    #[test]
    fn style_for_filter_tools_is_noop() {
        let (w, h) = (100, 100);
        let data = white(w, h);
        let base = BaseView { width: w, height: h, bgra: &data };
        let mut layer = AnnotationLayer::new(w, h, 1.0).unwrap();
        let update = layer
            .apply_style(AnnotationTool::Mosaic, &default_style(AnnotationTool::Line), base)
            .unwrap();
        assert!(update.is_empty());
    }
}
