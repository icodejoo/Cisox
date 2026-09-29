//! 各元素的 tiny-skia 绘制实现。
//!
//! 样式常量与绘制顺序对照 Qt 版 `snow_canvas_renderer.cpp`：
//! 箭头/画笔/矩形/椭圆/菱形/序号/序号连线；文字与滤镜只登记不绘制。
//! 所有路径在物理像素空间构建，画到 identity 变换上，便于包围盒裁剪。

use std::collections::HashMap;
use std::f64::consts::SQRT_2;

use snow_draw_engine_core::{
    ColorRgba8, CornerRadii, PathCommand,
    arrow::{ArrowShaftType, StrokeStyle},
};
use snow_draw_engine_display::{
    ArrowDisplayItem, ArrowheadDisplayDashMode, ArrowheadDisplayFillMode,
    ArrowheadDisplayPrimitive, ArrowheadDisplayPrimitiveKind, DisplayBlendMode, DisplayFillStyle,
    DisplayRectangleShape, DisplaySerialNumberType, FrameView, RectangleDisplayItem,
    SceneDisplayItem, SerialNumberConnectorDisplayItem, SerialNumberDisplayItem,
};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, Path, PathBuilder,
    Pattern, Pixmap, Rect, SpreadMode, Stroke, StrokeDash, Transform,
};

/// 圆弧的三次贝塞尔近似系数。
const ARC_KAPPA: f64 = 0.552_284_75;
/// Qt 默认斜接限制。
const MITER_LIMIT: f32 = 2.0;
/// 影线纹理的超采样倍数（对照 Qt `kHatchTextureSupersampling`）。
const HATCH_SUPERSAMPLING: f64 = 4.0;
/// 影线纹理缓存容量。
const HATCH_CACHE_CAPACITY: usize = 64;
/// 序号背景填充的参考字号（对照 Qt `kTextFillReferenceFontSize`）。
const TEXT_FILL_REFERENCE_FONT_SIZE: f64 = 24.0;

/// 画布坐标到物理像素的变换：`phys = (canvas - center) * zoom * dpr + surface/2 * dpr`。
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    /// 总缩放（相机缩放乘设备像素比）。
    pub scale: f64,
    tx: f64,
    ty: f64,
}

impl View {
    /// 由帧视图与设备像素比构建。
    pub fn new(frame: &FrameView, dpr: f64) -> Self {
        let zoom = frame.camera.zoom;
        Self {
            scale: zoom * dpr,
            tx: (f64::from(frame.surface.width) * 0.5 - frame.camera.center.x * zoom) * dpr,
            ty: (f64::from(frame.surface.height) * 0.5 - frame.camera.center.y * zoom) * dpr,
        }
    }

    /// 画布点转物理像素点。
    pub fn pt(&self, p: [f64; 2]) -> (f32, f32) {
        (
            (p[0] * self.scale + self.tx) as f32,
            (p[1] * self.scale + self.ty) as f32,
        )
    }

    /// 画布到物理像素的整体变换。
    pub fn transform(&self) -> Transform {
        Transform::from_row(
            self.scale as f32,
            0.0,
            0.0,
            self.scale as f32,
            self.tx as f32,
            self.ty as f32,
        )
    }

    /// 元素局部坐标（原点在元素中心、按弧度旋转、单位为画布单位）到物理像素的变换。
    pub fn item_transform(&self, cx: f64, cy: f64, rotation: f64) -> Transform {
        let (sin, cos) = rotation.sin_cos();
        let (tx, ty) = self.pt([cx, cy]);
        let s = self.scale;
        Transform::from_row(
            (s * cos) as f32,
            (s * sin) as f32,
            (-s * sin) as f32,
            (s * cos) as f32,
            tx,
            ty,
        )
    }

    /// 画布包围盒转物理像素包围盒。
    pub fn rect_to_phys(&self, b: [f64; 4]) -> [f32; 4] {
        let (x0, y0) = self.pt([b[0], b[1]]);
        let (x1, y1) = self.pt([b[2], b[3]]);
        [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
    }
}

/// 影线纹理（对照 Qt `HatchTexture`）。
struct HatchTexture {
    /// 超采样后的纹理。
    pixmap: Pixmap,
    /// 纹理到局部坐标的缩放。
    brush_scale: f64,
}

/// 影线纹理缓存，键为 (RGBA, 线宽位模式)。
#[derive(Default)]
pub(crate) struct HatchCache {
    entries: HashMap<(u32, u64), HatchTexture>,
}

impl HatchCache {
    /// 取（必要时创建）纹理。
    fn get(&mut self, color: ColorRgba8, line_width: f64) -> Option<&HatchTexture> {
        let key = (
            u32::from_le_bytes([color.r, color.g, color.b, color.a]),
            line_width.to_bits(),
        );
        if !self.entries.contains_key(&key) {
            if self.entries.len() >= HATCH_CACHE_CAPACITY {
                self.entries.clear();
            }
            let texture = make_hatch(color, line_width)?;
            self.entries.insert(key, texture);
        }
        self.entries.get(&key)
    }
}

/// 生成 BDiag 影线纹理：斜线间距与线宽对照 Qt `createHatchTexture`。
fn make_hatch(color: ColorRgba8, line_width: f64) -> Option<HatchTexture> {
    let spacing = (line_width * 4.0).clamp(3.0, 21.0);
    let desired = spacing * SQRT_2;
    let size = ((desired * HATCH_SUPERSAMPLING).ceil() as u32).max(1);
    let source = f64::from(size) / HATCH_SUPERSAMPLING;
    let brush_scale = desired / source;
    let mut pixmap = Pixmap::new(size, size)?;
    let paint = solid_paint(color, 1.0, false);
    let stroke = Stroke {
        width: (line_width / brush_scale * HATCH_SUPERSAMPLING) as f32,
        line_cap: LineCap::Butt,
        ..Stroke::default()
    };
    let p = size as f32;
    for d in -1..=2_i32 {
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, d as f32 * p);
        pb.line_to(p, (d - 1) as f32 * p);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
    Some(HatchTexture {
        pixmap,
        brush_scale,
    })
}

/// 绘制上下文：目标像素、脏区蒙版与视图变换。
pub(crate) struct Ctx<'a> {
    /// 目标画布。
    pub pixmap: &'a mut Pixmap,
    /// 脏区蒙版（脏区内为 255）。
    pub mask: &'a Mask,
    /// 视图变换。
    pub view: &'a View,
    /// 画布清除色（“背景填充”箭头头部使用）。
    pub clear: ColorRgba8,
    /// 影线纹理缓存。
    pub hatch: &'a mut HatchCache,
}

/// 把不透明度规整到 0..=1（非有限视为 1）。
fn norm_opacity(opacity: f64) -> f32 {
    if opacity.is_finite() {
        opacity.clamp(0.0, 1.0) as f32
    } else {
        1.0
    }
}

/// 构造纯色画笔，`opacity` 与颜色 alpha 相乘（对照 Qt `setOpacity` 逐次绘制生效）。
fn solid_paint(color: ColorRgba8, opacity: f64, multiply: bool) -> Paint<'static> {
    let alpha = f32::from(color.a) / 255.0 * norm_opacity(opacity);
    let mut paint = Paint::default();
    paint.set_color(
        Color::from_rgba(
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            alpha,
        )
        .unwrap_or(Color::TRANSPARENT),
    );
    paint.anti_alias = true;
    paint.blend_mode = if multiply {
        BlendMode::Multiply
    } else {
        BlendMode::SourceOver
    };
    paint
}

/// 虚线图案族（各元素在 Qt 中的图案不同）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DashKind {
    /// 箭头/序号：Qt `DashLine` {4,2}、`DotLine` {1,2}。
    Arrow,
    /// 自由画笔：{2,2.4} 与 {0.0001,1.9999}。
    FreeDraw,
    /// 矩形/椭圆：{4,4} 与 {0.01,3}。
    Rect,
}

/// 生成虚线；图案单位为线宽倍数，`phase` 为物理像素。
fn dash_pattern(kind: DashKind, style: StrokeStyle, width: f32, phase: f32) -> Option<StrokeDash> {
    let (on, off) = match (kind, style) {
        (_, StrokeStyle::Solid) => return None,
        (DashKind::Arrow, StrokeStyle::Dashed) => (4.0, 2.0),
        (DashKind::Arrow, StrokeStyle::Dotted) => (1.0, 2.0),
        (DashKind::FreeDraw, StrokeStyle::Dashed) => (2.0, 2.4),
        (DashKind::FreeDraw, StrokeStyle::Dotted) => (0.0001, 1.9999),
        (DashKind::Rect, StrokeStyle::Dashed) => (4.0, 4.0),
        (DashKind::Rect, StrokeStyle::Dotted) => (0.01, 3.0),
    };
    StrokeDash::new(vec![on * width, off * width], phase)
}

/// 构造描边参数。
fn stroke_of(width: f32, cap: LineCap, join: LineJoin, dash: Option<StrokeDash>) -> Stroke {
    Stroke {
        width,
        miter_limit: MITER_LIMIT,
        line_cap: cap,
        line_join: join,
        dash,
    }
}

/// 描边；宽度非正或非有限时跳过。
fn stroke_path(ctx: &mut Ctx, path: &Path, paint: &Paint, stroke: &Stroke) {
    if stroke.width.is_finite() && stroke.width > 0.0 {
        ctx.pixmap
            .stroke_path(path, paint, stroke, Transform::identity(), Some(ctx.mask));
    }
}

/// 纯色填充。
fn fill_solid(ctx: &mut Ctx, path: &Path, paint: &Paint) {
    ctx.pixmap.fill_path(
        path,
        paint,
        FillRule::Winding,
        Transform::identity(),
        Some(ctx.mask),
    );
}

/// 按填充样式填充：实心直接填，斜线/交叉线用影线纹理（对照 Qt `drawStyledFill`）。
///
/// `local` 为图案所在局部坐标到物理像素的变换。
#[allow(clippy::too_many_arguments)]
fn fill_styled(
    ctx: &mut Ctx,
    path: &Path,
    fill: ColorRgba8,
    style: DisplayFillStyle,
    reference_stroke_width: f64,
    local: Transform,
    opacity: f64,
    multiply: bool,
) {
    if fill.a == 0 {
        return;
    }
    if style == DisplayFillStyle::Solid {
        fill_solid(ctx, path, &solid_paint(fill, opacity, multiply));
        return;
    }
    if !reference_stroke_width.is_finite() {
        return;
    }
    let line_width = (1.0 + (reference_stroke_width - 1.0) * 0.6).clamp(0.5, 6.0);
    let passes: &[bool] = if style == DisplayFillStyle::CrossLine {
        &[false, true]
    } else {
        &[false]
    };
    for &mirrored in passes {
        let Some(texture) = ctx.hatch.get(fill, line_width) else {
            return;
        };
        let k = (texture.brush_scale / HATCH_SUPERSAMPLING) as f32;
        let pattern_ts = local.pre_concat(Transform::from_scale(k, if mirrored { -k } else { k }));
        let mut paint = Paint {
            shader: Pattern::new(
                texture.pixmap.as_ref(),
                SpreadMode::Repeat,
                FilterQuality::Bilinear,
                norm_opacity(opacity),
                pattern_ts,
            ),
            anti_alias: true,
            ..Paint::default()
        };
        if multiply {
            paint.blend_mode = BlendMode::Multiply;
        }
        ctx.pixmap.fill_path(
            path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            Some(ctx.mask),
        );
    }
}

/// 把路径命令追加到路径构建器（画布坐标转物理像素）。
fn push_commands(pb: &mut PathBuilder, view: &View, commands: &[PathCommand]) {
    for command in commands {
        match *command {
            PathCommand::MoveTo { point } => {
                let (x, y) = view.pt(point);
                pb.move_to(x, y);
            }
            PathCommand::LineTo { point } => {
                let (x, y) = view.pt(point);
                pb.line_to(x, y);
            }
            PathCommand::QuadTo { control, end } => {
                let (cx, cy) = view.pt(control);
                let (x, y) = view.pt(end);
                pb.quad_to(cx, cy, x, y);
            }
            PathCommand::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                let (c1x, c1y) = view.pt(control_1);
                let (c2x, c2y) = view.pt(control_2);
                let (x, y) = view.pt(end);
                pb.cubic_to(c1x, c1y, c2x, c2y, x, y);
            }
        }
    }
}

/// 折线点集转路径。
fn polyline(view: &View, points: &[[f64; 2]], close: bool) -> Option<Path> {
    let (first, rest) = points.split_first()?;
    let mut pb = PathBuilder::new();
    let (x, y) = view.pt(*first);
    pb.move_to(x, y);
    for p in rest {
        let (x, y) = view.pt(*p);
        pb.line_to(x, y);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

/// 单个元素是否为箭头头部形状之外的简单可见性判断：alpha 与线宽都有效。
fn stroke_visible(color: ColorRgba8, width: f64) -> bool {
    color.a != 0 && width.is_finite() && width > 0.0
}

/// 绘制箭头/画笔元素。
fn draw_arrow(ctx: &mut Ctx, arrow: &ArrowDisplayItem) {
    if !stroke_visible(arrow.stroke, arrow.stroke_width) {
        return;
    }
    let multiply = arrow.blend_mode == DisplayBlendMode::Multiply;
    let width = (arrow.stroke_width * ctx.view.scale) as f32;
    let paint = solid_paint(arrow.stroke, arrow.opacity, multiply);
    let view = *ctx.view;
    let dash_kind = if arrow.is_free_draw {
        DashKind::FreeDraw
    } else {
        DashKind::Arrow
    };
    let chunks = &arrow.geometry.chunks;
    let has_chunks = !chunks.is_empty();

    if arrow.arrow_shaft_type == ArrowShaftType::Tapered && !arrow.path_commands.is_empty() {
        // 锥形箭杆：path_commands 是轮廓，直接填充。
        let mut pb = PathBuilder::new();
        push_commands(&mut pb, &view, &arrow.path_commands);
        if let Some(path) = pb.finish() {
            fill_solid(ctx, &path, &paint);
        }
    } else if has_chunks {
        // 闭合路径的内部填充。
        if arrow.geometry.closed && arrow.fill.a != 0 {
            let mut pb = PathBuilder::new();
            for chunk in chunks.iter() {
                push_commands(&mut pb, &view, &chunk.commands);
            }
            pb.close();
            if let Some(path) = pb.finish() {
                fill_styled(
                    ctx,
                    &path,
                    arrow.fill,
                    arrow.fill_style,
                    arrow.stroke_width,
                    view.transform(),
                    arrow.opacity,
                    multiply,
                );
            }
        }
        // 各块独立描边，虚线相位取块起点累计长度以保持跨块连续。
        for chunk in chunks.iter() {
            let mut pb = PathBuilder::new();
            let (sx, sy) = view.pt(chunk.start_point);
            pb.move_to(sx, sy);
            push_commands(&mut pb, &view, &chunk.commands);
            let Some(path) = pb.finish() else {
                continue;
            };
            let phase = (chunk.cumulative_start_length * view.scale) as f32;
            let dash = dash_pattern(dash_kind, arrow.stroke_style, width, phase);
            let stroke = stroke_of(width, LineCap::Round, LineJoin::Round, dash);
            stroke_path(ctx, &path, &paint, &stroke);
        }
    } else {
        // 回退：无分块几何时用 path_commands，再退到 points 折线（曲线/折角箭头会退化为折线）。
        let path = if arrow.path_commands.is_empty() {
            polyline(&view, &arrow.points, false)
        } else {
            let mut pb = PathBuilder::new();
            push_commands(&mut pb, &view, &arrow.path_commands);
            pb.finish()
        };
        if let Some(path) = path {
            let closed = arrow.points.len() >= 3 && arrow.points.first() == arrow.points.last();
            if closed && arrow.fill.a != 0 {
                fill_styled(
                    ctx,
                    &path,
                    arrow.fill,
                    arrow.fill_style,
                    arrow.stroke_width,
                    view.transform(),
                    arrow.opacity,
                    multiply,
                );
            }
            let dash = dash_pattern(dash_kind, arrow.stroke_style, width, 0.0);
            let stroke = stroke_of(width, LineCap::Round, LineJoin::Round, dash);
            stroke_path(ctx, &path, &paint, &stroke);
        }
    }

    for primitive in &arrow.arrowhead_primitives {
        draw_arrowhead(ctx, arrow, primitive, width, &paint, multiply);
    }
}

/// 绘制单个箭头头部图元（引擎已算好画布坐标几何）。
fn draw_arrowhead(
    ctx: &mut Ctx,
    arrow: &ArrowDisplayItem,
    primitive: &ArrowheadDisplayPrimitive,
    width: f32,
    stroke_paint: &Paint,
    multiply: bool,
) {
    let view = *ctx.view;
    let style = match primitive.dash_mode {
        ArrowheadDisplayDashMode::Inherit => arrow.stroke_style,
        ArrowheadDisplayDashMode::Solid => StrokeStyle::Solid,
        ArrowheadDisplayDashMode::DottedCap => StrokeStyle::Dotted,
    };
    let stroke = stroke_of(
        width,
        LineCap::Round,
        LineJoin::Round,
        dash_pattern(DashKind::Arrow, style, width, 0.0),
    );
    let fill_paint = if primitive.fill_mode == ArrowheadDisplayFillMode::Background {
        solid_paint(ctx.clear, arrow.opacity, multiply)
    } else {
        stroke_paint.clone()
    };
    match primitive.kind {
        ArrowheadDisplayPrimitiveKind::Line => {
            if primitive.points.len() >= 2
                && let Some(path) = polyline(&view, &primitive.points[..2], false)
            {
                stroke_path(ctx, &path, stroke_paint, &stroke);
            }
        }
        ArrowheadDisplayPrimitiveKind::Polygon => {
            if primitive.points.len() < 2 {
                return;
            }
            // Qt 版填充隐式闭合、描边不闭合，这里保持一致。
            if let Some(closed) = polyline(&view, &primitive.points, true) {
                fill_solid(ctx, &closed, &fill_paint);
            }
            if let Some(open) = polyline(&view, &primitive.points, false) {
                stroke_path(ctx, &open, stroke_paint, &stroke);
            }
        }
        ArrowheadDisplayPrimitiveKind::Circle => {
            let diameter = (primitive.diameter * view.scale) as f32;
            if diameter <= 0.0 || !diameter.is_finite() {
                return;
            }
            let (cx, cy) = view.pt(primitive.center);
            if let Some(oval) =
                Rect::from_xywh(cx - diameter / 2.0, cy - diameter / 2.0, diameter, diameter)
                    .and_then(PathBuilder::from_oval)
            {
                fill_solid(ctx, &oval, &fill_paint);
                stroke_path(ctx, &oval, stroke_paint, &stroke);
            }
        }
    }
}

/// 按 Qt `toViewCornerRadii` 约束圆角：相邻圆角之和不超过边长。
fn clamp_radii(radii: &CornerRadii, w: f64, h: f64) -> [f64; 4] {
    let r = [
        radii.top_left.max(0.0),
        radii.top_right.max(0.0),
        radii.bottom_right.max(0.0),
        radii.bottom_left.max(0.0),
    ];
    let constraint = |len: f64, sum: f64| if sum > len { len / sum } else { 1.0 };
    let factor = constraint(w, r[0] + r[1])
        .min(constraint(w, r[3] + r[2]))
        .min(constraint(h, r[0] + r[3]))
        .min(constraint(h, r[1] + r[2]))
        .clamp(0.0, 1.0);
    r.map(|v| v * factor)
}

/// 局部坐标圆角矩形路径（中心在原点，radii 顺序 左上/右上/右下/左下）。
fn rounded_rect_path(w: f64, h: f64, radii: [f64; 4]) -> Option<Path> {
    let (l, t, r, b) = (-w / 2.0, -h / 2.0, w / 2.0, h / 2.0);
    let [tl, tr, br, bl] = radii;
    let mut pb = PathBuilder::new();
    let f = |v: f64| v as f32;
    let corner =
        |pb: &mut PathBuilder, c1: (f64, f64), c2: (f64, f64), end: (f64, f64), rad: f64| {
            if rad > 0.0 {
                pb.cubic_to(f(c1.0), f(c1.1), f(c2.0), f(c2.1), f(end.0), f(end.1));
            } else {
                pb.line_to(f(end.0), f(end.1));
            }
        };
    pb.move_to(f(l + tl), f(t));
    pb.line_to(f(r - tr), f(t));
    corner(
        &mut pb,
        (r - tr + ARC_KAPPA * tr, t),
        (r, t + tr - ARC_KAPPA * tr),
        (r, t + tr),
        tr,
    );
    pb.line_to(f(r), f(b - br));
    corner(
        &mut pb,
        (r, b - br + ARC_KAPPA * br),
        (r - br + ARC_KAPPA * br, b),
        (r - br, b),
        br,
    );
    pb.line_to(f(l + bl), f(b));
    corner(
        &mut pb,
        (l + bl - ARC_KAPPA * bl, b),
        (l, b - bl + ARC_KAPPA * bl),
        (l, b - bl),
        bl,
    );
    pb.line_to(f(l), f(t + tl));
    corner(
        &mut pb,
        (l, t + tl - ARC_KAPPA * tl),
        (l + tl - ARC_KAPPA * tl, t),
        (l + tl, t),
        tl,
    );
    pb.close();
    pb.finish()
}

/// 局部坐标菱形路径。
fn diamond_path(w: f64, h: f64) -> Option<Path> {
    let (hw, hh) = ((w / 2.0) as f32, (h / 2.0) as f32);
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, -hh);
    pb.line_to(hw, 0.0);
    pb.line_to(0.0, hh);
    pb.line_to(-hw, 0.0);
    pb.close();
    pb.finish()
}

/// 局部坐标椭圆路径。
fn ellipse_path(w: f64, h: f64) -> Option<Path> {
    Rect::from_xywh(-(w / 2.0) as f32, -(h / 2.0) as f32, w as f32, h as f32)
        .and_then(PathBuilder::from_oval)
}

/// 绘制矩形/椭圆/菱形元素。
fn draw_rectangle(ctx: &mut Ctx, item: &RectangleDisplayItem) {
    let scale = ctx.view.scale;
    if !(item.width.is_finite() && item.height.is_finite())
        || item.width <= 0.0
        || item.height <= 0.0
        || !(scale.is_finite() && scale > 0.0)
    {
        return;
    }
    let has_fill = item.fill.a != 0;
    let has_stroke = stroke_visible(item.stroke, item.stroke_width);
    if !has_fill && !has_stroke {
        return;
    }
    let local = match item.shape {
        DisplayRectangleShape::Ellipse => ellipse_path(item.width, item.height),
        DisplayRectangleShape::Diamond => diamond_path(item.width, item.height),
        DisplayRectangleShape::Rectangle => rounded_rect_path(
            item.width,
            item.height,
            clamp_radii(&item.corner_radii, item.width, item.height),
        ),
    };
    let matrix = ctx
        .view
        .item_transform(item.center_x, item.center_y, item.rotation);
    let Some(path) = local.and_then(|p| p.transform(matrix)) else {
        return;
    };
    let multiply = item.blend_mode == DisplayBlendMode::Multiply;
    if has_fill {
        fill_styled(
            ctx,
            &path,
            item.fill,
            item.fill_style,
            item.stroke_width,
            matrix,
            item.opacity,
            multiply,
        );
    }
    if has_stroke {
        let width = (item.stroke_width * scale) as f32;
        let dashed = item.stroke_style != StrokeStyle::Solid;
        let cap = if dashed {
            LineCap::Round
        } else {
            LineCap::Butt
        };
        let dash = dash_pattern(DashKind::Rect, item.stroke_style, width, 0.0);
        let stroke = stroke_of(width, cap, LineJoin::Miter, dash);
        stroke_path(
            ctx,
            &path,
            &solid_paint(item.stroke, item.opacity, multiply),
            &stroke,
        );
    }
}

/// 绘制序号标记的形状部分（数字文字由 `snow-canvas-text` 负责，此处不画）。
fn draw_serial_number(ctx: &mut Ctx, item: &SerialNumberDisplayItem) {
    let scale = ctx.view.scale;
    let diameter = item.diameter;
    if !(diameter.is_finite() && diameter > 0.0 && scale.is_finite() && scale > 0.0) {
        return;
    }
    let solid = matches!(
        item.serial_number_type,
        DisplaySerialNumberType::SolidCircle | DisplaySerialNumberType::SolidSquare
    );
    let square = matches!(
        item.serial_number_type,
        DisplaySerialNumberType::OutlinedSquare | DisplaySerialNumberType::SolidSquare
    );
    let stroke_width = if item.stroke_width.is_finite() {
        item.stroke_width.max(0.0)
    } else {
        0.0
    };
    // 序号的描边色即 `color`（Qt FFI 转换中 stroke = color）。
    let has_stroke = item.color.a != 0 && stroke_width > 0.0;
    if !solid && item.fill.a == 0 && !has_stroke {
        return;
    }
    let matrix = ctx
        .view
        .item_transform(item.center_x, item.center_y, item.rotation);
    let radius = item.corner_radii.top_left.max(0.0);
    if solid {
        let half = stroke_width / 2.0;
        let side = diameter + stroke_width;
        let local = if square {
            let r = radius + half;
            rounded_rect_path(side, side, [r; 4])
        } else {
            ellipse_path(side, side)
        };
        if let Some(path) = local.and_then(|p| p.transform(matrix)) {
            fill_solid(ctx, &path, &solid_paint(item.color, item.opacity, false));
        }
        return;
    }
    let local = if square {
        rounded_rect_path(diameter, diameter, [radius; 4])
    } else {
        ellipse_path(diameter, diameter)
    };
    let Some(path) = local.and_then(|p| p.transform(matrix)) else {
        return;
    };
    fill_styled(
        ctx,
        &path,
        item.fill,
        item.fill_style,
        item.font_size / TEXT_FILL_REFERENCE_FONT_SIZE,
        matrix,
        item.opacity,
        false,
    );
    if has_stroke {
        let width = (stroke_width * scale) as f32;
        let dash = dash_pattern(DashKind::Arrow, item.stroke_style, width, 0.0);
        let stroke = stroke_of(width, LineCap::Round, LineJoin::Round, dash);
        stroke_path(
            ctx,
            &path,
            &solid_paint(item.color, item.opacity, false),
            &stroke,
        );
    }
}

/// 绘制序号连线（含可选基线）。
fn draw_serial_connector(ctx: &mut Ctx, item: &SerialNumberConnectorDisplayItem) {
    if !stroke_visible(item.stroke, item.stroke_width) {
        return;
    }
    let view = *ctx.view;
    let width = (item.stroke_width * view.scale) as f32;
    let paint = solid_paint(item.stroke, item.opacity, false);
    let stroke = stroke_of(width, LineCap::Round, LineJoin::Round, None);
    if item.has_baseline
        && let Some(path) = polyline(
            &view,
            &[
                [item.baseline_start_x, item.baseline_start_y],
                [item.baseline_end_x, item.baseline_end_y],
            ],
            false,
        )
    {
        stroke_path(ctx, &path, &paint, &stroke);
    }
    if let Some(path) = polyline(
        &view,
        &[[item.start_x, item.start_y], [item.end_x, item.end_y]],
        false,
    ) {
        stroke_path(ctx, &path, &paint, &stroke);
    }
}

/// 绘制一个场景元素。文字、滤镜、图片、旧版 Stroke 不在此绘制。
pub(crate) fn draw_item(ctx: &mut Ctx, item: &SceneDisplayItem) {
    match item {
        SceneDisplayItem::Rectangle(rect) => draw_rectangle(ctx, rect),
        SceneDisplayItem::Arrow(arrow) => draw_arrow(ctx, arrow),
        SceneDisplayItem::SerialNumber(serial) => draw_serial_number(ctx, serial),
        SceneDisplayItem::SerialNumberConnector(conn) => draw_serial_connector(ctx, conn),
        SceneDisplayItem::Text(_)
        | SceneDisplayItem::Filter(_)
        | SceneDisplayItem::Stroke
        | SceneDisplayItem::Image => {}
    }
}

/// 累计点集包围盒。
fn extend(b: &mut Option<[f64; 4]>, p: [f64; 2]) {
    match b {
        Some(v) => {
            v[0] = v[0].min(p[0]);
            v[1] = v[1].min(p[1]);
            v[2] = v[2].max(p[0]);
            v[3] = v[3].max(p[1]);
        }
        None => *b = Some([p[0], p[1], p[0], p[1]]),
    }
}

/// 按旋转矩形求外接包围盒。
fn rotated_rect_bounds(cx: f64, cy: f64, w: f64, h: f64, rotation: f64, pad: f64) -> [f64; 4] {
    let (sin, cos) = rotation.sin_cos();
    let hx = (w * cos.abs() + h * sin.abs()) / 2.0 + pad;
    let hy = (w * sin.abs() + h * cos.abs()) / 2.0 + pad;
    [cx - hx, cy - hy, cx + hx, cy + hy]
}

/// 元素的保守画布包围盒 `[min_x, min_y, max_x, max_y]`（含描边外扩）。
/// 无可绘制内容的元素返回 `None`。
pub(crate) fn item_canvas_bounds(item: &SceneDisplayItem) -> Option<[f64; 4]> {
    match item {
        SceneDisplayItem::Rectangle(r) => Some(rotated_rect_bounds(
            r.center_x,
            r.center_y,
            r.width,
            r.height,
            r.rotation,
            r.stroke_width.max(0.0) * 1.5 + 1.0,
        )),
        SceneDisplayItem::Text(t) => Some(rotated_rect_bounds(
            t.center_x, t.center_y, t.width, t.height, t.rotation, 1.0,
        )),
        SceneDisplayItem::Filter(f) => {
            if f.is_pen_filter {
                let mut b = None;
                for p in f.points.iter() {
                    extend(&mut b, *p);
                }
                let pad = f.stroke_width.max(0.0) + 1.0;
                b.map(|v| [v[0] - pad, v[1] - pad, v[2] + pad, v[3] + pad])
            } else {
                Some(rotated_rect_bounds(
                    f.center_x, f.center_y, f.width, f.height, f.rotation, 1.0,
                ))
            }
        }
        SceneDisplayItem::Arrow(a) => {
            let mut b = None;
            for p in &a.points {
                extend(&mut b, *p);
            }
            for c in &a.path_commands {
                match *c {
                    PathCommand::MoveTo { point } | PathCommand::LineTo { point } => {
                        extend(&mut b, point);
                    }
                    PathCommand::QuadTo { control, end } => {
                        extend(&mut b, control);
                        extend(&mut b, end);
                    }
                    PathCommand::CubicTo {
                        control_1,
                        control_2,
                        end,
                    } => {
                        extend(&mut b, control_1);
                        extend(&mut b, control_2);
                        extend(&mut b, end);
                    }
                }
            }
            if !a.geometry.chunks.is_empty() {
                let g = a.geometry.canvas_bounds;
                extend(&mut b, [g[0], g[1]]);
                extend(&mut b, [g[2], g[3]]);
            }
            let mut reach = 0.0_f64;
            for prim in &a.arrowhead_primitives {
                for p in &prim.points {
                    extend(&mut b, *p);
                }
                if prim.kind == ArrowheadDisplayPrimitiveKind::Circle {
                    extend(&mut b, prim.center);
                    reach = reach.max(prim.diameter.max(0.0) / 2.0);
                }
            }
            let pad = a.stroke_width.max(0.0) * 1.5 + reach + 1.0;
            b.map(|v| [v[0] - pad, v[1] - pad, v[2] + pad, v[3] + pad])
        }
        SceneDisplayItem::SerialNumber(s) => {
            let side = s.diameter + s.stroke_width.max(0.0) * 2.0;
            Some(rotated_rect_bounds(
                s.center_x, s.center_y, side, side, s.rotation, 1.0,
            ))
        }
        SceneDisplayItem::SerialNumberConnector(c) => {
            let mut b = None;
            extend(&mut b, [c.start_x, c.start_y]);
            extend(&mut b, [c.end_x, c.end_y]);
            if c.has_baseline {
                extend(&mut b, [c.baseline_start_x, c.baseline_start_y]);
                extend(&mut b, [c.baseline_end_x, c.baseline_end_y]);
            }
            let pad = c.stroke_width.max(0.0) + 1.0;
            b.map(|v| [v[0] - pad, v[1] - pad, v[2] + pad, v[3] + pad])
        }
        SceneDisplayItem::Stroke | SceneDisplayItem::Image => None,
    }
}
