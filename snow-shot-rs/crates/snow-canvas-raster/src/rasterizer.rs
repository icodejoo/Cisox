//! `CanvasRasterizer` 抽象与基于 tiny-skia 的 CPU 实现。

use std::collections::HashSet;

use snow_draw_engine_display::{
    DecorationRevision, OverlayRevision, PatchCursor, SceneDisplayItem, SceneRevision,
    ViewportPatch,
};
use tiny_skia::{Mask, Pixmap};

use crate::draw::{Ctx, HatchCache, View, draw_item, item_canvas_bounds};
use crate::types::{
    DeferredItem, DeferredKind, MAX_SURFACE_PIXELS, RasterConfig, RasterError, RasterOutput,
    RasterTile, TileKey,
};
use snow_draw_engine_display::FrameView;

/// 物理像素整数矩形 `[x0, y0, x1, y1]`（右下开区间）。
type IntRect = [u32; 4];

/// 画布光栅化器抽象：消费引擎 `ViewportPatch`，产出脏块像素。
///
/// 除 `TinySkiaRasterizer` 外，后续可增加 GPU 实现（如 Vello）而不影响调用方。
pub trait CanvasRasterizer {
    /// 分块边长（像素）。
    fn tile_size(&self) -> u32;

    /// 应用一个 patch，返回本帧需要上传/释放的块。
    ///
    /// # 参数
    /// - `patch`：引擎 `acquire_patch` 返回的补丁；增量补丁的基线版本必须与上次应用的一致。
    ///
    /// # 返回
    /// 成功时返回脏块、释放列表与待外部绘制的元素；失败时内部状态作废，
    /// 调用方应 `reset()` 后以 `None` 游标重新拉取 patch。
    fn apply_patch(&mut self, patch: &ViewportPatch) -> Result<RasterOutput, RasterError>;

    /// 当前已应用补丁对应的游标；回传给引擎 `acquire_patch` 才能得到真正的增量补丁。
    fn cursor(&self) -> Option<PatchCursor>;

    /// 丢弃全部状态，返回此前已提交、现在需要释放的块。
    fn reset(&mut self) -> Vec<TileKey>;
}

/// 常驻画布：整幅像素与全 0 脏区蒙版。
struct Canvas {
    pixmap: Pixmap,
    mask: Mask,
}

/// tiny-skia CPU 光栅化器（P0 实现）。
///
/// 持有场景镜像、整幅画布与已提交块集合；每帧只清除并重绘脏区，
/// 脏区蒙版常驻全 0、每帧只处理脏区行（spike 结论：整幅 Mask 每帧 clear 是负优化）。
///
/// # 说明
/// - 支持元素：矩形/椭圆/菱形、箭头（含虚线、线帽、头部图元、锥形）、画笔、序号形状、序号连线。
/// - 文字与滤镜不绘制，登记在 `RasterOutput::deferred`（降级为空白）。
/// - 覆盖层（选择框/手柄）与装饰层（水印/聚光灯）不在本光栅化器范围。
///
/// # 示例
/// ```
/// use snow_canvas_raster::{CanvasRasterizer, RasterConfig, TinySkiaRasterizer};
/// use snow_draw_engine_display::ViewportPatch;
///
/// let mut r = TinySkiaRasterizer::new(RasterConfig::default());
/// let out = r.apply_patch(&ViewportPatch::default()).unwrap();
/// assert!(out.tiles.is_empty());
/// ```
pub struct TinySkiaRasterizer {
    config: RasterConfig,
    items: Vec<SceneDisplayItem>,
    scene_revision: Option<u64>,
    frame: Option<FrameView>,
    cursor: Option<PatchCursor>,
    canvas: Option<Canvas>,
    emitted: HashSet<TileKey>,
    hatch: HatchCache,
}

impl TinySkiaRasterizer {
    /// 创建光栅化器。
    ///
    /// # 参数
    /// - `config`：分块与设备像素比配置，越界值会被夹到合法范围。
    pub fn new(config: RasterConfig) -> Self {
        Self {
            config: config.normalized(),
            items: Vec::new(),
            scene_revision: None,
            frame: None,
            cursor: None,
            canvas: None,
            emitted: HashSet::new(),
            hatch: HatchCache::default(),
        }
    }

    /// 当前整幅画布的预乘 RGBA 像素（宽、高、数据），未初始化或零尺寸时为 `None`。
    /// 主要供测试与整幅导出使用。
    pub fn canvas_rgba(&self) -> Option<(u32, u32, &[u8])> {
        self.canvas
            .as_ref()
            .map(|c| (c.pixmap.width(), c.pixmap.height(), c.pixmap.data()))
    }

    /// 当前场景镜像中的元素数量。
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// 出错后作废场景状态（保留已提交块记录，供 `reset` 返回释放列表）。
    fn invalidate(&mut self) {
        self.items.clear();
        self.scene_revision = None;
        self.frame = None;
        self.cursor = None;
    }

    /// 校验并把 patch 的场景操作应用到本地镜像。
    fn apply_scene_ops(&mut self, patch: &ViewportPatch) -> Result<(), RasterError> {
        let scene = &patch.scene;
        if scene.reset {
            self.items.clear();
        } else {
            let Some(rev) = self.scene_revision else {
                return Err(RasterError::NotInitialized);
            };
            if scene.base_revision != rev {
                return Err(RasterError::RevisionMismatch {
                    expected: rev,
                    got: scene.base_revision,
                });
            }
        }
        for op in &scene.ops {
            let start = op.start as usize;
            let end = start + op.delete_count as usize;
            if end > self.items.len() {
                return Err(RasterError::InvalidOp);
            }
            self.items
                .splice(start..end, op.insert_items.iter().cloned());
        }
        self.scene_revision = Some(scene.revision);
        Ok(())
    }

    /// `apply_patch` 的实现体，错误由外层统一作废状态。
    fn apply_inner(&mut self, patch: &ViewportPatch) -> Result<RasterOutput, RasterError> {
        self.apply_scene_ops(patch)?;

        let dpr = f64::from(self.config.device_pixel_ratio);
        let frame = patch.frame_view;
        let width = (f64::from(frame.surface.width) * dpr).round() as u32;
        let height = (f64::from(frame.surface.height) * dpr).round() as u32;
        if width > MAX_SURFACE_PIXELS || height > MAX_SURFACE_PIXELS {
            return Err(RasterError::SurfaceTooLarge { width, height });
        }

        let mut output = RasterOutput {
            surface_size: (width, height),
            ..RasterOutput::default()
        };
        let frame_changed = self.frame != Some(frame);
        self.frame = Some(frame);
        self.cursor = Some(PatchCursor {
            scene_revision: SceneRevision(patch.scene.revision),
            decoration_revision: DecorationRevision(patch.decoration.revision),
            overlay_revision: OverlayRevision(patch.overlay.revision),
        });

        // 尺寸变化：重建画布，并释放落在新网格之外的旧块。
        let size_changed = self
            .canvas
            .as_ref()
            .map(|c| (c.pixmap.width(), c.pixmap.height()))
            != (width > 0 && height > 0).then_some((width, height));
        if size_changed {
            self.canvas = None;
            if width > 0 && height > 0 {
                let pixmap = Pixmap::new(width, height)
                    .ok_or(RasterError::SurfaceTooLarge { width, height })?;
                let mask = Mask::new(width, height)
                    .ok_or(RasterError::SurfaceTooLarge { width, height })?;
                self.canvas = Some(Canvas { pixmap, mask });
            }
            let tile = self.config.tile_size;
            let (cols, rows) = (width.div_ceil(tile), height.div_ceil(tile));
            let mut stale: Vec<TileKey> = self
                .emitted
                .iter()
                .copied()
                .filter(|k| k.col >= cols || k.row >= rows)
                .collect();
            stale.sort();
            for key in stale {
                self.emitted.remove(&key);
                output.released.push(key);
            }
        }
        let Some(canvas) = self.canvas.as_mut() else {
            return Ok(output);
        };

        let full = patch.scene.reset || frame_changed || size_changed;
        let rects: Vec<IntRect> = if full {
            vec![[0, 0, width, height]]
        } else {
            patch
                .scene
                .dirty_regions
                .iter()
                .filter_map(|r| to_int_rect(r.min_x, r.min_y, r.max_x, r.max_y, dpr, width, height))
                .collect()
        };
        output.full_redraw = full;
        if rects.is_empty() {
            return Ok(output);
        }

        clear_rows(canvas.pixmap.data_mut(), width, &rects);
        set_mask_rows(&mut canvas.mask, width, &rects, 255);

        let view = View::new(&frame, dpr);
        let mut ctx = Ctx {
            pixmap: &mut canvas.pixmap,
            mask: &canvas.mask,
            view: &view,
            clear: frame.clear_color,
            hatch: &mut self.hatch,
        };
        for item in &self.items {
            let Some(bounds) = item_canvas_bounds(item) else {
                continue;
            };
            let phys = view.rect_to_phys(bounds);
            if !hits_any(&phys, &rects) {
                continue;
            }
            match item {
                SceneDisplayItem::Text(t) => output.deferred.push(DeferredItem {
                    id: t.id,
                    kind: DeferredKind::Text,
                    bounds: phys,
                }),
                SceneDisplayItem::Filter(f) => output.deferred.push(DeferredItem {
                    id: f.id,
                    kind: DeferredKind::Filter,
                    bounds: phys,
                }),
                _ => draw_item(&mut ctx, item),
            }
        }
        set_mask_rows(&mut canvas.mask, width, &rects, 0);

        self.collect_tiles(&rects, &mut output);
        Ok(output)
    }

    /// 把被脏区触及的分块拷出；全透明块转为释放。
    fn collect_tiles(&mut self, rects: &[IntRect], output: &mut RasterOutput) {
        let Some(canvas) = self.canvas.as_ref() else {
            return;
        };
        let ts = self.config.tile_size;
        let (width, height) = (canvas.pixmap.width(), canvas.pixmap.height());
        let cols = width.div_ceil(ts);
        let rows = height.div_ceil(ts);
        let mut touched = vec![false; (cols * rows) as usize];
        for r in rects {
            for row in r[1] / ts..=(r[3] - 1) / ts {
                for col in r[0] / ts..=(r[2] - 1) / ts {
                    touched[(row * cols + col) as usize] = true;
                }
            }
        }
        let data = canvas.pixmap.data();
        let stride = width as usize * 4;
        for (index, _) in touched.iter().enumerate().filter(|(_, t)| **t) {
            let key = TileKey {
                col: index as u32 % cols,
                row: index as u32 / cols,
            };
            let (x, y) = (key.col * ts, key.row * ts);
            let (w, h) = (ts.min(width - x), ts.min(height - y));
            let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
            for line in 0..h as usize {
                let off = (y as usize + line) * stride + x as usize * 4;
                rgba.extend_from_slice(&data[off..off + w as usize * 4]);
            }
            if all_zero(&rgba) {
                if self.emitted.remove(&key) {
                    output.released.push(key);
                }
            } else {
                self.emitted.insert(key);
                output.tiles.push(RasterTile {
                    key,
                    x,
                    y,
                    w,
                    h,
                    rgba,
                });
            }
        }
    }
}

impl CanvasRasterizer for TinySkiaRasterizer {
    fn tile_size(&self) -> u32 {
        self.config.tile_size
    }

    fn apply_patch(&mut self, patch: &ViewportPatch) -> Result<RasterOutput, RasterError> {
        let result = self.apply_inner(patch);
        if result.is_err() {
            self.invalidate();
        }
        result
    }

    fn cursor(&self) -> Option<PatchCursor> {
        self.cursor
    }

    fn reset(&mut self) -> Vec<TileKey> {
        self.invalidate();
        self.canvas = None;
        let mut released: Vec<TileKey> = self.emitted.drain().collect();
        released.sort();
        released
    }
}

/// 逻辑脏区转物理整数矩形：外扩 1 像素覆盖抗锯齿边缘并夹到画布内；空矩形返回 `None`。
fn to_int_rect(
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    dpr: f64,
    width: u32,
    height: u32,
) -> Option<IntRect> {
    if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
        return None;
    }
    let lo = |v: f64, limit: u32| ((v * dpr).floor() - 1.0).clamp(0.0, f64::from(limit)) as u32;
    let hi = |v: f64, limit: u32| ((v * dpr).ceil() + 1.0).clamp(0.0, f64::from(limit)) as u32;
    let rect = [
        lo(min_x, width),
        lo(min_y, height),
        hi(max_x, width),
        hi(max_y, height),
    ];
    (rect[0] < rect[2] && rect[1] < rect[3]).then_some(rect)
}

/// 把各整数矩形覆盖的像素清为全透明。
fn clear_rows(data: &mut [u8], width: u32, rects: &[IntRect]) {
    let stride = width as usize * 4;
    for r in rects {
        for y in r[1] as usize..r[3] as usize {
            data[y * stride + r[0] as usize * 4..y * stride + r[2] as usize * 4].fill(0);
        }
    }
}

/// 把蒙版中各整数矩形覆盖的像素统一置为 `value`。
fn set_mask_rows(mask: &mut Mask, width: u32, rects: &[IntRect], value: u8) {
    let stride = width as usize;
    let data = mask.data_mut();
    for r in rects {
        for y in r[1] as usize..r[3] as usize {
            data[y * stride + r[0] as usize..y * stride + r[2] as usize].fill(value);
        }
    }
}

/// 物理包围盒是否与任一整数脏矩形相交。
fn hits_any(b: &[f32; 4], rects: &[IntRect]) -> bool {
    rects.iter().any(|r| {
        b[2] > r[0] as f32 && b[0] < r[2] as f32 && b[3] > r[1] as f32 && b[1] < r[3] as f32
    })
}

/// 判断字节切片是否全为 0（按 8 字节字累积 OR，便于自动向量化）。
fn all_zero(bytes: &[u8]) -> bool {
    let mut chunks = bytes.chunks_exact(8);
    let mut acc = 0_u64;
    for chunk in &mut chunks {
        let mut word = [0_u8; 8];
        word.copy_from_slice(chunk);
        acc |= u64::from_ne_bytes(word);
    }
    acc == 0 && chunks.remainder().iter().all(|&b| b == 0)
}
