//! 装饰层 pass：聚光灯（暗化并挖洞）与水印（旋转平铺文字）。
//!
//! 装饰层在标注之上、水印在聚光灯之上，不进文档图元。本模块只产出“装饰图层”的预乘 RGBA
//! 区块，由调用方用 source-over 叠在标注层上；预览与导出走同一个 [`DecorationLayer::render_region`]，
//! 因此像素一致。只重画调用方给出的矩形，天然符合 `ViewportPatch` 的脏区协议。
//!
//! 行为对照旧版 `snow_canvas_spotlight_renderer.cpp` / `snow_canvas_watermark_renderer.cpp`：
//! 洞内不变暗（并集、抗锯齿）；水印按墨迹包围盒 + 间距平铺，奇数行错开半个步长，绕渲染区中心旋转。

use snow_draw_engine_display::{
    DecorationPatch, DecorationView, DisplaySpotlightConfig, DisplaySpotlightCutout,
    DisplayWatermarkConfig, FrameView,
};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, Rect,
    SpreadMode, Transform,
};

use crate::draw::View;
use crate::types::{MAX_SURFACE_PIXELS, RasterError};

/// 整数矩形 `[x0, y0, x1, y1]`（画布物理像素，右下开区间）。
pub type RegionRect = [i32; 4];

/// 装饰渲染的固定网格边长（物理像素）：任何请求区域都按这个网格切块渲染再裁剪，
/// 因此同一像素无论预览分块还是导出裁切，字节都一致（tiny-skia 的抗锯齿对裁剪区域敏感）。
pub const DECORATION_GRID: i32 = 256;
/// 水印有效 alpha 的可见下限（对照旧版 `kMinimumVisibleAlpha`），低于它直接不画。
const MIN_VISIBLE_ALPHA: f64 = 0.004;
/// 水印间距下限（画布像素）。
const WATERMARK_GAP_MIN: f64 = 10.0;
/// 水印间距上限（画布像素）。
const WATERMARK_GAP_MAX: f64 = 200.0;
/// 字号量化精度：像素字号按 1/64 取整，避免缩放时反复重建单元。
const FONT_PX_QUANTIZATION: f64 = 64.0;
/// 重复单元单边像素上限（对照旧版 `kRepeatCellDimensionLimit`）。
const MAX_CELL_SIDE: u32 = 4096;
/// 重复单元字节上限（对照旧版 `kRepeatCellByteLimit`）。
const MAX_CELL_BYTES: usize = 4 * 1024 * 1024;
/// 每像素字节数（预乘 RGBA8）。
const BPP: usize = 4;
/// 单元内向左右/上下补画的行列范围，保证跨单元边界的墨迹能回绕（对照旧版 -1..=2 行、-2..=2 列）。
const CELL_ROWS: std::ops::RangeInclusive<i32> = -1..=2;
/// 同上，列范围。
const CELL_COLS: std::ops::RangeInclusive<i32> = -2..=2;
/// 一个单元包含的行/列周期数（奇偶行错位，所以横纵都取 2 倍步长）。
const CELL_PERIODS: f64 = 2.0;

/// 单通道覆盖率位图（0 无墨迹，255 完全覆盖），由调用方的文字光栅化器提供。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageBitmap {
    /// 宽度（像素）。
    pub width: u32,
    /// 高度（像素）。
    pub height: u32,
    /// 行紧密排列的覆盖率，长度 `width * height`。
    pub coverage: Vec<u8>,
}

/// 文字光栅化回调：`(文本, 字体族, 像素字号) -> 覆盖率位图`，失败返回 `None`。
///
/// 字体族为空串表示由调用方取默认字体。
pub type TextRasterizer<'a> = dyn FnMut(&str, &str, f32) -> Option<CoverageBitmap> + 'a;

/// 水印平铺几何（物理像素），用于测试与诊断。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WatermarkGeometry {
    /// 墨迹包围盒宽。
    pub ink_width: u32,
    /// 墨迹包围盒高。
    pub ink_height: u32,
    /// 横向步长 = 墨迹宽 + 间距。
    pub step_x: f64,
    /// 纵向步长 = 墨迹高 + 间距。
    pub step_y: f64,
    /// 重复单元宽（像素）。
    pub cell_width: u32,
    /// 重复单元高（像素）。
    pub cell_height: u32,
}

/// 水印重复单元的缓存键：任何一项变化都要重建。
#[derive(Clone, Debug, PartialEq)]
struct CellKey {
    /// 水印文本。
    text: String,
    /// 字体族。
    family: String,
    /// 量化后的像素字号（乘 64 取整）。
    px64: u32,
    /// 物理间距（乘 64 取整）。
    gap64: i64,
    /// 着色后的预乘前 RGBA（alpha 已含不透明度）。
    tint: [u8; 4],
}

/// 已构建的水印重复单元。
struct WatermarkCell {
    /// 缓存键。
    key: CellKey,
    /// 单元像素（预乘 RGBA）。
    pixmap: Pixmap,
    /// 平铺几何。
    geometry: WatermarkGeometry,
}

/// 装饰层状态与渲染器：持有聚光灯洞列表、装饰配置、水印单元缓存与草稿块。
///
/// # 示例
/// ```
/// use snow_canvas_raster::decoration::DecorationLayer;
/// let layer = DecorationLayer::default();
/// assert!(!layer.is_active());
/// ```
#[derive(Default)]
pub struct DecorationLayer {
    /// 当前装饰配置（水印 + 聚光灯）。
    view: DecorationView,
    /// 当前聚光灯洞列表。
    cutouts: Vec<DisplaySpotlightCutout>,
    /// 已应用的装饰修订号；`None` 表示尚未收到 reset 补丁。
    revision: Option<u64>,
    /// 最近一份水印重复单元。
    cell: Option<WatermarkCell>,
    /// 单元构建失败（或超限）时的键，避免每个区块重复尝试。
    failed_key: Option<CellKey>,
    /// 复用的草稿块。
    scratch: Option<Pixmap>,
}

impl DecorationLayer {
    /// 丢弃全部状态（出错或重置时调用）。
    pub fn reset(&mut self) {
        self.view = DecorationView::default();
        self.cutouts.clear();
        self.revision = None;
        self.cell = None;
        self.failed_key = None;
    }

    /// 应用一个装饰补丁：reset 清空洞列表，再按 `spotlight_ops` 做区间替换。
    ///
    /// # 参数
    /// - `patch`：引擎补丁里的装饰层部分；增量补丁的基线版本必须与本地一致。
    ///
    /// # 返回
    /// 版本不匹配返回 `RevisionMismatch` / `NotInitialized`，区间越界返回 `InvalidOp`。
    pub fn apply_patch(&mut self, patch: &DecorationPatch) -> Result<(), RasterError> {
        if patch.reset {
            self.cutouts.clear();
        } else {
            let Some(rev) = self.revision else {
                return Err(RasterError::NotInitialized);
            };
            if patch.base_revision != rev {
                return Err(RasterError::RevisionMismatch {
                    expected: rev,
                    got: patch.base_revision,
                });
            }
        }
        for op in &patch.spotlight_ops {
            let start = op.start as usize;
            let mut end = start.saturating_add(op.delete_count as usize);
            if patch.reset {
                // reset 补丁的删除数针对旧列表，已清空，按空列表夹取
                end = end.min(self.cutouts.len());
            }
            if start > self.cutouts.len() || end > self.cutouts.len() {
                return Err(RasterError::InvalidOp);
            }
            self.cutouts
                .splice(start..end, op.insert_items.iter().copied());
        }
        self.view = patch.view;
        self.revision = Some(patch.revision);
        Ok(())
    }

    /// 已应用的装饰修订号；尚未收到 reset 补丁时为 `None`。
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// 当前装饰配置。
    pub fn view(&self) -> &DecorationView {
        &self.view
    }

    /// 当前聚光灯洞列表。
    pub fn cutouts(&self) -> &[DisplaySpotlightCutout] {
        &self.cutouts
    }

    /// 聚光灯是否会画出东西（启用、颜色与不透明度都非零）。
    pub fn spotlight_visible(&self) -> bool {
        let s = &self.view.spotlight;
        s.active && s.color.a > 0 && s.opacity > 0.0 && s.opacity.is_finite()
    }

    /// 水印是否会画出东西（文本非空白、有效 alpha 达到可见下限、字号合法）。
    pub fn watermark_visible(&self) -> bool {
        let w = &self.view.watermark;
        !watermark_text(w).trim().is_empty()
            && watermark_alpha(w) >= MIN_VISIBLE_ALPHA
            && w.font_size.is_finite()
            && w.font_size > 0.0
    }

    /// 装饰层是否有任何可见内容。
    pub fn is_active(&self) -> bool {
        self.spotlight_visible() || self.watermark_visible()
    }

    /// 水印是否因构建失败或单元超限而降级为不绘制（调用方可据此记录告警）。
    pub fn watermark_degraded(&self) -> bool {
        self.failed_key.is_some()
    }

    /// 最近一次渲染使用的水印平铺几何；尚未渲染过水印时为 `None`。
    pub fn watermark_geometry(&self) -> Option<WatermarkGeometry> {
        self.cell.as_ref().map(|c| c.geometry)
    }

    /// 渲染一个矩形区域的装饰图层。
    ///
    /// # 参数
    /// - `frame`：当前帧视图（决定洞的画布到物理像素变换与水印锚点）。
    /// - `dpr`：设备像素比。
    /// - `rect`：要渲染的区域（画布物理像素，会被夹进画布）。
    /// - `text`：水印文字光栅化回调；仅在需要（重建单元）时调用。
    ///
    /// # 返回
    /// 区域大小的预乘 RGBA（行紧密排列），装饰层在该区域无可见内容时为 `None`。
    /// 调用方用 source-over 叠到标注层之上，同一函数同时服务预览与导出；
    /// 内部按 [`DECORATION_GRID`] 网格切块，所以同一像素在任意请求区域下字节一致。
    pub fn render_region(
        &mut self,
        frame: &FrameView,
        dpr: f64,
        rect: RegionRect,
        text: &mut TextRasterizer<'_>,
    ) -> Option<Vec<u8>> {
        if !self.is_active() {
            return None;
        }
        let surface_w = (f64::from(frame.surface.width) * dpr).round();
        let surface_h = (f64::from(frame.surface.height) * dpr).round();
        let limit = f64::from(MAX_SURFACE_PIXELS);
        if !(surface_w > 0.0 && surface_h > 0.0 && surface_w <= limit && surface_h <= limit) {
            return None;
        }
        let (sw, sh) = (surface_w as i32, surface_h as i32);
        let rect = [
            rect[0].max(0),
            rect[1].max(0),
            rect[2].min(sw),
            rect[3].min(sh),
        ];
        if rect[0] >= rect[2] || rect[1] >= rect[3] {
            return None;
        }
        let view = View::new(frame, dpr);
        let (out_w, out_h) = ((rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize);
        let mut out: Option<Vec<u8>> = None;
        for gy in rect[1].div_euclid(DECORATION_GRID)..=(rect[3] - 1).div_euclid(DECORATION_GRID) {
            for gx in
                rect[0].div_euclid(DECORATION_GRID)..=(rect[2] - 1).div_euclid(DECORATION_GRID)
            {
                let tile = [
                    gx * DECORATION_GRID,
                    gy * DECORATION_GRID,
                    ((gx + 1) * DECORATION_GRID).min(sw),
                    ((gy + 1) * DECORATION_GRID).min(sh),
                ];
                let Some(pixels) = self.render_grid_tile(&view, (surface_w, surface_h), tile, text)
                else {
                    continue;
                };
                // 请求区域与网格块的交集
                let (x0, y0) = (rect[0].max(tile[0]), rect[1].max(tile[1]));
                let (x1, y1) = (rect[2].min(tile[2]), rect[3].min(tile[3]));
                let tile_w = (tile[2] - tile[0]) as usize;
                let dst = out.get_or_insert_with(|| vec![0u8; out_w * out_h * BPP]);
                for y in y0..y1 {
                    let src_off = ((y - tile[1]) as usize * tile_w + (x0 - tile[0]) as usize) * BPP;
                    let dst_off = ((y - rect[1]) as usize * out_w + (x0 - rect[0]) as usize) * BPP;
                    let len = (x1 - x0) as usize * BPP;
                    dst[dst_off..dst_off + len].copy_from_slice(&pixels[src_off..src_off + len]);
                }
            }
        }
        out.filter(|buf| buf.iter().any(|&b| b != 0))
    }

    /// 渲染一个网格块（`rect` 已夹进画布）；无可见内容返回 `None`。
    ///
    /// # 参数
    /// - `view`：画布到物理像素的变换。
    /// - `surface`：画布物理尺寸（水印锚点取其中心）。
    /// - `rect`：网格块（画布物理像素）。
    /// - `text`：水印文字光栅化回调。
    fn render_grid_tile(
        &mut self,
        view: &View,
        surface: (f64, f64),
        rect: RegionRect,
        text: &mut TextRasterizer<'_>,
    ) -> Option<Vec<u8>> {
        let (w, h) = ((rect[2] - rect[0]) as u32, (rect[3] - rect[1]) as u32);
        let origin = (f64::from(rect[0]), f64::from(rect[1]));
        let anchor = (surface.0 / 2.0 - origin.0, surface.1 / 2.0 - origin.1);

        // 水印单元先准备好（可能调用回调），再借用草稿块作画
        let watermark = self.watermark_visible();
        if watermark {
            self.ensure_cell(view, text);
        }
        let spotlight = self.spotlight_visible();
        if !matches!(&self.scratch, Some(p) if p.width() == w && p.height() == h) {
            self.scratch = Some(Pixmap::new(w, h)?);
        }
        let scratch = self.scratch.as_mut()?;
        scratch.fill(Color::TRANSPARENT);
        if spotlight {
            draw_spotlight(scratch, &self.view.spotlight, &self.cutouts, view, origin);
        }
        if watermark && let Some(cell) = &self.cell {
            draw_watermark(scratch, cell, &self.view.watermark, anchor);
        }
        (!scratch.data().iter().all(|&b| b == 0)).then(|| scratch.data().to_vec())
    }

    /// 按当前水印配置与缩放确保 `self.cell` 是对应的重复单元；失败时 `self.cell` 为 `None`。
    fn ensure_cell(&mut self, view: &View, text: &mut TextRasterizer<'_>) {
        let w = &self.view.watermark;
        let scale = view.scale;
        let content = watermark_text(w);
        let px = (w.font_size * scale * FONT_PX_QUANTIZATION).round() / FONT_PX_QUANTIZATION;
        let gap_px = w.gap.clamp(WATERMARK_GAP_MIN, WATERMARK_GAP_MAX) * scale;
        let alpha = watermark_alpha(w);
        let key = CellKey {
            text: content.clone(),
            family: watermark_family(w),
            px64: (px * FONT_PX_QUANTIZATION).round().max(0.0) as u32,
            gap64: (gap_px * FONT_PX_QUANTIZATION).round() as i64,
            tint: [
                w.color.r,
                w.color.g,
                w.color.b,
                (alpha * 255.0).round() as u8,
            ],
        };
        if self.cell.as_ref().is_some_and(|c| c.key == key) {
            return;
        }
        self.cell = None;
        if self.failed_key.as_ref() == Some(&key) {
            return;
        }
        let built = text(&key.text, &key.family, px as f32)
            .and_then(|bitmap| build_cell(key.clone(), &bitmap, gap_px, alpha, w));
        match built {
            Some(cell) => {
                self.failed_key = None;
                self.cell = Some(cell);
            }
            None => self.failed_key = Some(key),
        }
    }
}

/// 水印文本（UTF-8，按 `text_len` 截断；非法字节丢弃）。
fn watermark_text(w: &DisplayWatermarkConfig) -> String {
    let len = usize::from(w.text_len).min(w.text.len());
    String::from_utf8_lossy(&w.text[..len]).into_owned()
}

/// 水印字体族（空串表示默认字体）。
fn watermark_family(w: &DisplayWatermarkConfig) -> String {
    let len = usize::from(w.font_family_len).min(w.font_family.len());
    String::from_utf8_lossy(&w.font_family[..len]).into_owned()
}

/// 水印有效 alpha（0..=1）：`color.a * clamp(opacity) / 255`。
fn watermark_alpha(w: &DisplayWatermarkConfig) -> f64 {
    let opacity = if w.opacity.is_finite() {
        w.opacity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    f64::from(w.color.a) * opacity / 255.0
}

/// 由覆盖率位图构建水印重复单元；单元超限或位图无墨迹返回 `None`。
///
/// # 参数
/// - `key`：缓存键。
/// - `bitmap`：文字覆盖率位图（含留白，会裁到墨迹包围盒）。
/// - `gap_px`：物理间距。
/// - `alpha`：有效 alpha（0..=1）。
/// - `w`：水印配置（取颜色）。
fn build_cell(
    key: CellKey,
    bitmap: &CoverageBitmap,
    gap_px: f64,
    alpha: f64,
    w: &DisplayWatermarkConfig,
) -> Option<WatermarkCell> {
    let (bw, bh) = (bitmap.width as usize, bitmap.height as usize);
    if bw == 0 || bh == 0 || bitmap.coverage.len() < bw * bh {
        return None;
    }
    // 墨迹包围盒
    let (mut x0, mut y0, mut x1, mut y1) = (bw, bh, 0, 0);
    for y in 0..bh {
        for x in 0..bw {
            if bitmap.coverage[y * bw + x] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (iw, ih) = (x1 - x0, y1 - y0);
    let step_x = (iw as f64 + gap_px).max(1.0);
    let step_y = (ih as f64 + gap_px).max(1.0);
    let cell_w = (step_x * CELL_PERIODS).ceil();
    let cell_h = (step_y * CELL_PERIODS).ceil();
    let side_limit = f64::from(MAX_CELL_SIDE);
    if cell_w > side_limit
        || cell_h > side_limit
        || (cell_w as usize) * (cell_h as usize) * BPP > MAX_CELL_BYTES
    {
        return None;
    }
    let (cw, ch) = (cell_w as u32, cell_h as u32);
    let mut pixmap = Pixmap::new(cw, ch)?;
    // 把覆盖率裁成预乘 RGBA 墨迹块
    let tint_a = alpha * 255.0;
    let mut ink = vec![[0u8; 4]; iw * ih];
    for y in 0..ih {
        for x in 0..iw {
            let cov = f64::from(bitmap.coverage[(y0 + y) * bw + x0 + x]);
            if cov == 0.0 {
                continue;
            }
            let a = (cov * tint_a / 255.0).round().min(255.0);
            let pm = |c: u8| (f64::from(c) * a / 255.0).round() as u8;
            ink[y * iw + x] = [pm(w.color.r), pm(w.color.g), pm(w.color.b), a as u8];
        }
    }
    let data = pixmap.data_mut();
    for row in CELL_ROWS {
        let offset = if row & 1 != 0 { step_x / 2.0 } else { 0.0 };
        for col in CELL_COLS {
            let ux = (f64::from(col) * step_x + offset).round() as i64;
            let uy = (f64::from(row) * step_y).round() as i64;
            blit_over(
                data, cw as i64, ch as i64, &ink, iw as i64, ih as i64, ux, uy,
            );
        }
    }
    Some(WatermarkCell {
        key,
        pixmap,
        geometry: WatermarkGeometry {
            ink_width: iw as u32,
            ink_height: ih as u32,
            step_x,
            step_y,
            cell_width: cw,
            cell_height: ch,
        },
    })
}

/// 把墨迹块以 source-over 画进单元（预乘 RGBA），越界部分裁掉。
#[allow(clippy::too_many_arguments)]
fn blit_over(
    dst: &mut [u8],
    dw: i64,
    dh: i64,
    ink: &[[u8; 4]],
    iw: i64,
    ih: i64,
    ux: i64,
    uy: i64,
) {
    for y in 0..ih {
        let dy = uy + y;
        if dy < 0 || dy >= dh {
            continue;
        }
        for x in 0..iw {
            let dx = ux + x;
            if dx < 0 || dx >= dw {
                continue;
            }
            let src = ink[(y * iw + x) as usize];
            if src[3] == 0 {
                continue;
            }
            let o = ((dy * dw + dx) as usize) * BPP;
            let inv = 255 - u32::from(src[3]);
            for c in 0..BPP {
                let v = u32::from(src[c]) + (u32::from(dst[o + c]) * inv + 127) / 255;
                dst[o + c] = v.min(255) as u8;
            }
        }
    }
}

/// 聚光灯：整块填 `color * opacity`，再用 `DestinationOut` 一次性挖掉所有洞的并集（抗锯齿）。
///
/// # 参数
/// - `target`：草稿块（已清空）。
/// - `config`：聚光灯配置。
/// - `cutouts`：洞列表（画布坐标，旋转矩形，弧度）。
/// - `view`：画布到物理像素的变换。
/// - `origin`：草稿块左上角的物理像素坐标。
fn draw_spotlight(
    target: &mut Pixmap,
    config: &DisplaySpotlightConfig,
    cutouts: &[DisplaySpotlightCutout],
    view: &View,
    origin: (f64, f64),
) {
    let alpha = f64::from(config.color.a) / 255.0 * config.opacity.clamp(0.0, 1.0);
    let fill = Color::from_rgba(
        f32::from(config.color.r) / 255.0,
        f32::from(config.color.g) / 255.0,
        f32::from(config.color.b) / 255.0,
        alpha as f32,
    )
    .unwrap_or(Color::TRANSPARENT);
    target.fill(fill);

    let (w, h) = (f64::from(target.width()), f64::from(target.height()));
    let mut builder = PathBuilder::new();
    let mut any = false;
    for cut in cutouts {
        let valid = [
            cut.center_x,
            cut.center_y,
            cut.width,
            cut.height,
            cut.rotation,
        ]
        .iter()
        .all(|v| v.is_finite())
            && cut.width > 0.0
            && cut.height > 0.0;
        if !valid {
            continue;
        }
        let (sin, cos) = cut.rotation.sin_cos();
        let (hw, hh) = (cut.width / 2.0, cut.height / 2.0);
        // 固定的角点顺序保证各洞绕向一致，Winding 填充即并集
        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(lx, ly)| {
            let canvas = [
                cut.center_x + lx * cos - ly * sin,
                cut.center_y + lx * sin + ly * cos,
            ];
            let (px, py) = view.pt(canvas);
            (f64::from(px) - origin.0, f64::from(py) - origin.1)
        });
        // 包围盒剔除
        let (min_x, max_x) = corners
            .iter()
            .fold((f64::MAX, f64::MIN), |a, c| (a.0.min(c.0), a.1.max(c.0)));
        let (min_y, max_y) = corners
            .iter()
            .fold((f64::MAX, f64::MIN), |a, c| (a.0.min(c.1), a.1.max(c.1)));
        if max_x < 0.0 || max_y < 0.0 || min_x > w || min_y > h {
            continue;
        }
        builder.move_to(corners[0].0 as f32, corners[0].1 as f32);
        for c in &corners[1..] {
            builder.line_to(c.0 as f32, c.1 as f32);
        }
        builder.close();
        any = true;
    }
    if !any {
        return;
    }
    if let Some(path) = builder.finish() {
        let mut paint = Paint::default();
        paint.set_color(Color::BLACK);
        paint.anti_alias = true;
        paint.blend_mode = BlendMode::DestinationOut;
        target.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// 水印：对单元做 Repeat 的 Pattern 填充整个草稿块，变换为 `translate(锚点) · rotate(角度) · scale(单元→步长)`。
///
/// # 参数
/// - `target`：草稿块（可能已有聚光灯内容，水印叠在其上）。
/// - `cell`：重复单元。
/// - `config`：水印配置（取角度）。
/// - `anchor`：锚点（渲染区中心）在草稿块坐标里的位置。
fn draw_watermark(
    target: &mut Pixmap,
    cell: &WatermarkCell,
    config: &DisplayWatermarkConfig,
    anchor: (f64, f64),
) {
    let g = cell.geometry;
    let sx = (g.step_x * CELL_PERIODS / f64::from(g.cell_width)) as f32;
    let sy = (g.step_y * CELL_PERIODS / f64::from(g.cell_height)) as f32;
    let angle = if config.angle.is_finite() {
        config.angle as f32
    } else {
        0.0
    };
    let transform = Transform::from_translate(anchor.0 as f32, anchor.1 as f32)
        .pre_rotate(angle)
        .pre_scale(sx, sy);
    let paint = Paint {
        shader: Pattern::new(
            cell.pixmap.as_ref(),
            SpreadMode::Repeat,
            FilterQuality::Bilinear,
            1.0,
            transform,
        ),
        anti_alias: false,
        ..Paint::default()
    };
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, target.width() as f32, target.height() as f32) {
        target.fill_rect(rect, &paint, Transform::identity(), None);
    }
}
