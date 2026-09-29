//! P0-V2 垂直切片验证：tiny-skia 光栅化的带箭头虚线描边，合成进 GPUI canvas。
//! 通过判据：4K 全屏、拖动标注时稳定 60fps。
//!
//! 数据流：Engine(process_input) -> acquire_patch(增量 ViewportPatch/DirtyRegion)
//!   -> tiny-skia 只重绘脏区 -> Pixmap(RGBA premultiplied) -> 转 BGRA -> RenderImage
//!   -> gpui img(ImageSource::Render) 合成显示。
//! 硬约束：不修改 snow_draw_engine_qt 任何既有源码；不升级 gpui/tiny-skia 版本；
//! 不绕开 acquire_patch/DirtyRegion 协议自造几何。

use gpui::*;
use image::{Frame, RgbaImage};
use snow_draw_engine::{
    ActiveTool, Arrowhead, ColorRgba8, DecorationRevision, Engine, OverlayRevision, PatchCursor, SceneRevision, InputEvent, Modifiers, Point as EnginePoint,
    PointerButton, PointerButtons, PointerDevice, PointerEvent, PointerEventType,
    SceneDisplayItem, ShapeKind, ShapeStylePatch, StrokeStyle, ViewportConfig, ViewportId,
};
use snow_draw_engine_editor::{
    SHAPE_STYLE_PROPERTY_END_ARROWHEAD, SHAPE_STYLE_PROPERTY_STROKE_STYLE,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Rect, Stroke, StrokeDash, Transform};

/// 已完成的 render 帧数，供看门狗线程观察是否停止出帧。
static FRAME_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 当前 Unix 毫秒时间戳，用于阶段耗时对表。
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 启动看门狗线程：超过 3 秒没有新帧就输出一行，用于区分“卡死”与“只是慢”。
fn spawn_watchdog() {
    std::thread::spawn(|| {
        let t0 = Instant::now();
        let mut last = usize::MAX;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let cur = FRAME_COUNTER.load(Ordering::Relaxed);
            if cur == last {
                eprintln!("看门狗: t={:.0}s 帧计数停在 {}，无新帧", t0.elapsed().as_secs_f64(), cur);
            }
            last = cur;
        }
    });
}

/// 脏区裁剪方式，由环境变量 SPIKE_MASK 选择（full/dirty/none），缺省 dirty。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MaskMode {
    /// 第二轮旧做法：每帧整体清空全画布 Mask 再填脏区（对照组）。
    Full,
    /// 第三轮：Mask 常驻全 0，只对脏区行置 255，画完只把这些行复位。
    Dirty,
    /// 不建 Mask，仅靠包围盒相交跳过（对照，边缘抗锯齿会重复叠加）。
    None,
}

/// 模拟 4K surface 尺寸（方案判据要求：4K 全屏）。
const SURFACE_WIDTH: u32 = 3840;
const SURFACE_HEIGHT: u32 = 2160;
/// 拖动模拟的总帧数：跑足够长时间以统计稳定后的帧率分布。
const TOTAL_DRAG_FRAMES: usize = 300;

/// 单帧耗时拆分统计：光栅化 vs 合成打包（转 BGRA + 构造 RenderImage）。
#[derive(Default, Clone, Copy)]
struct FrameTiming {
    total_secs: f64,
    raster_secs: f64,
    composite_secs: f64,
    /// 本帧重建的分块数。
    dirty_tiles: usize,
    /// render 函数总耗时（光栅化+合成+构建元素树）。
    render_secs: f64,
    /// 本帧脏区像素总数（整数外扩后）。
    dirty_px: u64,
}

/// 驱动“引擎拖动模拟 + tiny-skia 光栅化 + GPUI 合成”的根视图。
/// 每次 GPUI 调用 render 即视为一帧：喂一个模拟的指针移动事件、
/// 拉取增量 patch、只在脏区重绘、打包成 RenderImage 显示。
struct RasterSpikeView {
    engine: Engine,
    viewport_id: ViewportId,
    /// 整幅 4K 画布，跨帧持久化——脏区之外的旧内容原样保留。
    pixmap: Pixmap,
    /// 当前场景条目的本地镜像，通过 LayerPatch::ops 增量维护。
    scene_items: Vec<SceneDisplayItem>,
    frame_index: usize,
    drag_progress: f64,
    started_at: Instant,
    last_frame_at: Instant,
    timings: Vec<FrameTiming>,
    finished: bool,
    /// 上一次已应用 patch 的游标；回传给引擎才能拿到增量 patch。
    patch_cursor: Option<PatchCursor>,
    /// 各分块当前的 RenderImage（未画过内容的块为 None）。
    tiles: Vec<Option<Arc<RenderImage>>>,
    /// 本帧被脏区触及、待重建的分块标记。
    dirty_flags: Vec<bool>,
    /// 分块边长（像素），命令行可配（128/256/512）。
    tile_size: u32,
    /// 横向分块数。
    tiles_x: u32,
    /// 纵向分块数。
    tiles_y: u32,
    /// 复用的脏区裁剪蒙版（与画布同尺寸）。
    mask: Mask,
    /// 裁剪方式。
    mask_mode: MaskMode,
    /// 本帧脏区像素总数，供插桩记录。
    last_dirty_px: u64,
}

impl RasterSpikeView {
    /// 构造视图：创建 4K viewport，把 Arrow 工具切到“虚线 + 三角箭头”样式。
    fn new(_window: &mut Window, cx: &mut Context<Self>, tile_size: u32, mask_mode: MaskMode) -> Self {
        let mut engine = Engine::default();
        let viewport_id = engine
            .create_viewport(ViewportConfig::default())
            .expect("create_viewport 失败");
        engine
            .set_viewport_surface_size(viewport_id, SURFACE_WIDTH, SURFACE_HEIGHT)
            .expect("set_viewport_surface_size 失败");
        engine
            .set_viewport_active_tool(viewport_id, ActiveTool::Arrow)
            .expect("set_viewport_active_tool 失败");

        // 以引擎当前给出的默认 Arrow 样式为基底，只覆盖虚线+箭头两项，避免
        // 凭空构造一个可能不合法的 ShapeStyle。
        let toolbar_state = engine
            .viewport_style_toolbar_state(viewport_id)
            .expect("viewport_style_toolbar_state 失败");
        let mut style = toolbar_state.shape_style;
        style.stroke_style = StrokeStyle::Dashed;
        style.end_arrowhead = Some(Arrowhead::Triangle);
        style.stroke = ColorRgba8 {
            r: 0xff,
            g: 0x30,
            b: 0x30,
            a: 0xff,
        };
        style.stroke_width = 6.0;
        engine
            .set_viewport_shape_style_patch(
                viewport_id,
                ShapeStylePatch {
                    kind: ShapeKind::Arrow,
                    style,
                    properties: SHAPE_STYLE_PROPERTY_STROKE_STYLE
                        | SHAPE_STYLE_PROPERTY_END_ARROWHEAD,
                },
            )
            .expect("set_viewport_shape_style_patch 失败");

        // Down：在 4K 画布左上角附近落下箭头起点。
        engine
            .process_input_with_viewport_changes(
                viewport_id,
                InputEvent::Pointer(PointerEvent {
                    pointer_id: 1,
                    event_type: PointerEventType::Down,
                    device: PointerDevice::Mouse,
                    position: EnginePoint::new(400.0, 400.0),
                    button: Some(PointerButton::Primary),
                    buttons: PointerButtons(PointerButtons::PRIMARY),
                    modifiers: Modifiers::default(),
                }),
            )
            .expect("初始 Pointer Down 失败");

        let now = Instant::now();
        let tiles_x = SURFACE_WIDTH.div_ceil(tile_size);
        let tiles_y = SURFACE_HEIGHT.div_ceil(tile_size);
        let view = Self {
            engine,
            viewport_id,
            pixmap: Pixmap::new(SURFACE_WIDTH, SURFACE_HEIGHT).expect("Pixmap::new 失败"),
            scene_items: Vec::new(),
            frame_index: 0,
            drag_progress: 0.0,
            started_at: now,
            last_frame_at: now,
            timings: Vec::with_capacity(TOTAL_DRAG_FRAMES),
            finished: false,
            patch_cursor: None,
            tiles: vec![None; (tiles_x * tiles_y) as usize],
            dirty_flags: vec![false; (tiles_x * tiles_y) as usize],
            tile_size,
            tiles_x,
            tiles_y,
            mask: Mask::new(SURFACE_WIDTH, SURFACE_HEIGHT).expect("Mask::new 失败"),
            mask_mode,
            last_dirty_px: 0,
        };
        cx.notify();
        view
    }

    /// 用代码模拟“持续拖动”的一帧：终点坐标做小增量位移，
    /// 驱动 process_input_with_viewport_changes(Move)。
    fn simulate_drag_step(&mut self) {
        self.drag_progress += 1.0;
        let x = 400.0 + self.drag_progress * 8.0;
        let y = 400.0 + (self.drag_progress * 0.35).sin() * 260.0 + self.drag_progress * 2.0;
        self.engine
            .process_input_with_viewport_changes(
                self.viewport_id,
                InputEvent::Pointer(PointerEvent {
                    pointer_id: 1,
                    event_type: PointerEventType::Move,
                    device: PointerDevice::Mouse,
                    position: EnginePoint::new(x, y),
                    button: None,
                    buttons: PointerButtons(PointerButtons::PRIMARY),
                    modifiers: Modifiers::default(),
                }),
            )
            .expect("Pointer Move 失败");
    }

    /// 拉取增量 patch，只对 dirty_regions 覆盖的矩形做 tiny-skia 重绘。
    /// 返回 (光栅化耗时, 本帧脏区数量)。
    fn raster_frame(&mut self) -> f64 {
        let raster_start = Instant::now();

        let patch = self
            .engine
            .acquire_patch(self.viewport_id, self.patch_cursor)
            .expect("acquire_patch 失败");
        self.patch_cursor = Some(PatchCursor {
            scene_revision: SceneRevision(patch.scene.revision),
            decoration_revision: DecorationRevision(patch.decoration.revision),
            overlay_revision: OverlayRevision(patch.overlay.revision),
        });

        if patch.scene.reset {
            self.scene_items.clear();
        }
        for op in &patch.scene.ops {
            let start = op.start as usize;
            let end = start + op.delete_count as usize;
            self.scene_items
                .splice(start..end, op.insert_items.iter().cloned());
        }

        // reset 时脏区协议可能为空（整帧替换），保底用整幅画布当作脏区。
        let mut dirty_rects: Vec<Rect> = Vec::new();
        if patch.scene.reset && patch.scene.dirty_regions.is_empty() {
            if let Some(rect) = Rect::from_ltrb(0.0, 0.0, SURFACE_WIDTH as f32, SURFACE_HEIGHT as f32)
            {
                dirty_rects.push(rect);
            }
        }
        for region in &patch.scene.dirty_regions {
            if region.is_empty() {
                continue;
            }
            let min_x = region.min_x.max(0.0) as f32;
            let min_y = region.min_y.max(0.0) as f32;
            let max_x = (region.max_x.min(SURFACE_WIDTH as f64)) as f32;
            let max_y = (region.max_y.min(SURFACE_HEIGHT as f64)) as f32;
            if let Some(rect) = Rect::from_ltrb(min_x, min_y, max_x, max_y) {
                dirty_rects.push(rect);
            }
        }

        for rect in &dirty_rects {
            self.mark_dirty_tiles(rect);
        }

        if !dirty_rects.is_empty() {
            // 脏区外扩为整数矩形：清除与 Mask 使用同一范围，边缘不会半清半画。
            let int_rects: Vec<(u32, u32, u32, u32)> =
                dirty_rects.iter().filter_map(to_int_rect).collect();
            self.last_dirty_px = int_rects
                .iter()
                .map(|&(x0, y0, x1, y1)| (x1 - x0) as u64 * (y1 - y0) as u64)
                .sum();
            let mut clear_paint = Paint::default();
            clear_paint.set_color_rgba8(0, 0, 0, 0);
            clear_paint.blend_mode = tiny_skia::BlendMode::Source;
            for &(x0, y0, x1, y1) in &int_rects {
                if let Some(r) = Rect::from_ltrb(x0 as f32, y0 as f32, x1 as f32, y1 as f32) {
                    self.pixmap
                        .fill_rect(r, &clear_paint, Transform::identity(), None);
                }
            }

            match self.mask_mode {
                MaskMode::Full => {
                    // 旧做法：整体清空全画布 Mask 后逐脏区填充。
                    self.mask.clear();
                    for rect in &dirty_rects {
                        self.mask.fill_path(
                            &PathBuilder::from_rect(*rect),
                            FillRule::Winding,
                            false,
                            Transform::identity(),
                        );
                    }
                }
                MaskMode::Dirty => set_mask_rows(&mut self.mask, &int_rects, 255),
                MaskMode::None => {}
            }
            let mask_ref = if self.mask_mode == MaskMode::None { None } else { Some(&self.mask) };
            for item in &self.scene_items {
                if let SceneDisplayItem::Arrow(arrow) = item {
                    // 包围盒与所有脏区都不相交则整条箭头跳过。
                    if !arrow_hits_dirty(arrow, &dirty_rects) {
                        continue;
                    }
                    draw_arrow(&mut self.pixmap, arrow, mask_ref);
                }
            }
            // Dirty 模式：只把刚置位的行复位，保持 Mask 常驻全 0。
            if self.mask_mode == MaskMode::Dirty {
                set_mask_rows(&mut self.mask, &int_rects, 0);
            }
        } else {
            self.last_dirty_px = 0;
        }

        raster_start.elapsed().as_secs_f64()
    }

    /// 把脏矩形覆盖到的分块打上脏标记。
    fn mark_dirty_tiles(&mut self, rect: &Rect) {
        // 向外多扩 1 像素，覆盖抗锯齿边缘。
        let x0 = ((rect.left() - 1.0).max(0.0) as u32 / self.tile_size).min(self.tiles_x - 1);
        let y0 = ((rect.top() - 1.0).max(0.0) as u32 / self.tile_size).min(self.tiles_y - 1);
        let x1 = ((rect.right() + 1.0).max(0.0) as u32 / self.tile_size).min(self.tiles_x - 1);
        let y1 = ((rect.bottom() + 1.0).max(0.0) as u32 / self.tile_size).min(self.tiles_y - 1);
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                self.dirty_flags[(ty * self.tiles_x + tx) as usize] = true;
            }
        }
    }

    /// 仅对脏分块做 RGBA->BGRA 转换并重建 RenderImage，旧图从图集丢弃。
    /// 两者都已预乘，只需交换 R/B。返回 (耗时, 重建块数)。
    fn composite_dirty_tiles(&mut self, window: &mut Window, cx: &mut App) -> (f64, usize) {
        let start = Instant::now();
        let src = self.pixmap.data();
        let stride = SURFACE_WIDTH as usize * 4;
        let mut rebuilt = 0;
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let idx = (ty * self.tiles_x + tx) as usize;
                if !std::mem::take(&mut self.dirty_flags[idx]) {
                    continue;
                }
                let x0 = tx * self.tile_size;
                let y0 = ty * self.tile_size;
                let w = self.tile_size.min(SURFACE_WIDTH - x0);
                let h = self.tile_size.min(SURFACE_HEIGHT - y0);
                let mut buf = Vec::with_capacity((w * h * 4) as usize);
                for row in 0..h as usize {
                    let off = (y0 as usize + row) * stride + x0 as usize * 4;
                    buf.extend_from_slice(&src[off..off + w as usize * 4]);
                }
                for px in buf.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                let image = RgbaImage::from_raw(w, h, buf).expect("分块尺寸不匹配");
                let new_image = Arc::new(RenderImage::new(vec![Frame::new(image)]));
                if let Some(old) = self.tiles[idx].replace(new_image) {
                    cx.drop_image(old, Some(window));
                }
                rebuilt += 1;
            }
        }
        (start.elapsed().as_secs_f64(), rebuilt)
    }

    /// 帧率统计完成后打印平均/最低 fps 与光栅化/合成耗时占比。
    fn print_report(&self) {
        let n = self.timings.len();
        if n == 0 {
            println!("没有采集到任何帧数据。");
            return;
        }
        let total_wall = self.timings.iter().map(|t| t.total_secs).sum::<f64>();
        let avg_fps = n as f64 / total_wall;
        let min_fps = self
            .timings
            .iter()
            .map(|t| 1.0 / t.total_secs.max(1e-9))
            .fold(f64::MAX, f64::min);
        let avg_raster_ms =
            self.timings.iter().map(|t| t.raster_secs).sum::<f64>() / n as f64 * 1000.0;
        let avg_composite_ms =
            self.timings.iter().map(|t| t.composite_secs).sum::<f64>() / n as f64 * 1000.0;

        println!("==== P0-V2 tiny-skia + GPUI 光栅化基准结果 ====");
        println!("surface: {}x{} (4K)", SURFACE_WIDTH, SURFACE_HEIGHT);
        println!("采样帧数: {}", n);
        println!("平均 fps: {:.2}", avg_fps);
        println!("最低 fps(单帧瞬时): {:.2}", min_fps);
        let mut sorted: Vec<f64> = self.timings.iter().map(|t| t.total_secs).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p99_ms = sorted[((n as f64 * 0.99) as usize).min(n - 1)] * 1000.0;
        let avg_tiles =
            self.timings.iter().map(|t| t.dirty_tiles).sum::<usize>() as f64 / n as f64;
        let mut worst: Vec<(usize, f64)> =
            self.timings.iter().enumerate().map(|(i, t)| (i + 1, t.total_secs * 1000.0)).collect();
        worst.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        println!("最慢 5 帧(帧号, ms): {:?}", &worst[..5.min(worst.len())]);
        // 稳态统计：剔除第 1 帧（整幅 4K 首次上传的一次性预热帧）。
        let steady: Vec<f64> = self.timings.iter().skip(1).map(|t| t.total_secs).collect();
        let steady_avg = steady.len() as f64 / steady.iter().sum::<f64>();
        let steady_min = steady.iter().map(|t| 1.0 / t).fold(f64::MAX, f64::min);
        let mut ss = steady.clone();
        ss.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let steady_p99 = ss[((ss.len() as f64 * 0.99) as usize).min(ss.len() - 1)] * 1000.0;
        println!(
            "稳态(剔除首帧): 平均 {:.2} fps, 最低 {:.2} fps, P99 {:.2} ms, 大于 20ms 的帧数 {}",
            steady_avg, steady_min, steady_p99, steady.iter().filter(|t| **t > 0.020).count()
        );
        let steady_p50 = ss[ss.len() / 2] * 1000.0;
        let steady_max = ss[ss.len() - 1] * 1000.0;
        println!("稳态帧间隔: P50 {:.2} ms, P99 {:.2} ms, 最大 {:.2} ms", steady_p50, steady_p99, steady_max);
        println!("P99 帧间隔(稳态): {:.2} ms", steady_p99);
        println!("P99 帧间隔: {:.2} ms (约 {:.2} fps)", p99_ms, 1000.0 / p99_ms);
        println!("分块 {}px, 平均每帧重建块数: {:.1}", self.tile_size, avg_tiles);
        println!("平均单帧光栅化(tiny-skia)耗时: {:.3} ms", avg_raster_ms);
        println!("平均单帧合成(转BGRA+RenderImage)耗时: {:.3} ms", avg_composite_ms);
        // 长尾归因：帧间隔(n) = 帧n-1 的 render 耗时 + 呈现/等待 vsync 时间，故对比前一帧 render。
        let mut long_cnt = 0;
        let mut long_cpu_spike = 0;
        for i in 1..n {
            if self.timings[i].total_secs > 0.020 {
                long_cnt += 1;
                let prev_render = self.timings[i - 1].render_secs * 1000.0;
                let idle = self.timings[i].total_secs * 1000.0 - prev_render;
                if prev_render > 8.0 {
                    long_cpu_spike += 1;
                }
                println!(
                    "长尾帧#{}: 间隔 {:.2} ms, 前一帧render {:.2} ms(光栅{:.2}+合成{:.2}, 块{}, 脏区{}px), 非render时间 {:.2} ms",
                    i + 1,
                    self.timings[i].total_secs * 1000.0,
                    prev_render,
                    self.timings[i - 1].raster_secs * 1000.0,
                    self.timings[i - 1].composite_secs * 1000.0,
                    self.timings[i - 1].dirty_tiles,
                    self.timings[i - 1].dirty_px,
                    idle
                );
            }
        }
        let mut rs: Vec<f64> = self.timings.iter().map(|t| t.render_secs * 1000.0).collect();
        rs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "render耗时: P50 {:.2} ms, P99 {:.2} ms, 最大 {:.2} ms; 长尾帧 {} 个, 其中前一帧render>8ms 的 {} 个",
            rs[rs.len() / 2],
            rs[((rs.len() as f64 * 0.99) as usize).min(rs.len() - 1)],
            rs[rs.len() - 1],
            long_cnt,
            long_cpu_spike
        );
        println!("MODE mask={:?} tile={}", self.mask_mode, self.tile_size);
        // 画布内容校验和：不同裁剪方式应得到一致的像素。
        let hash = self.pixmap.data().iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
        println!("画布FNV: {:016x}", hash);
        println!(
            "判据(4K 全屏拖动稳定60fps): {}",
            if min_fps >= 55.0 { "通过" } else { "未通过" }
        );
    }
}

/// 浮点脏矩形向外取整并夹到画布内，返回 (x0,y0,x1,y1)；空矩形返回 None。
fn to_int_rect(r: &Rect) -> Option<(u32, u32, u32, u32)> {
    let x0 = r.left().floor().max(0.0) as u32;
    let y0 = r.top().floor().max(0.0) as u32;
    let x1 = (r.right().ceil().max(0.0) as u32).min(SURFACE_WIDTH);
    let y1 = (r.bottom().ceil().max(0.0) as u32).min(SURFACE_HEIGHT);
    if x0 < x1 && y0 < y1 { Some((x0, y0, x1, y1)) } else { None }
}

/// 把 Mask 中各整数矩形覆盖的像素统一置为 value（Mask 为 1 字节/像素）。
fn set_mask_rows(mask: &mut Mask, rects: &[(u32, u32, u32, u32)], value: u8) {
    let w = mask.width() as usize;
    let data = mask.data_mut();
    for &(x0, y0, x1, y1) in rects {
        for y in y0 as usize..y1 as usize {
            data[y * w + x0 as usize..y * w + x1 as usize].fill(value);
        }
    }
}

/// 判断箭头（含线宽外扩的包围盒）是否与任一脏区相交。
fn arrow_hits_dirty(arrow: &snow_draw_engine::ArrowDisplayItem, dirty: &[Rect]) -> bool {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let heads = arrow.arrowhead_primitives.iter().flat_map(|h| h.points.iter());
    for p in arrow.points.iter().chain(heads) {
        x0 = x0.min(p[0] as f32);
        y0 = y0.min(p[1] as f32);
        x1 = x1.max(p[0] as f32);
        y1 = y1.max(p[1] as f32);
    }
    let pad = arrow.stroke_width.max(1.0) as f32 * 2.0;
    dirty.iter().any(|d| {
        x1 + pad > d.left() && x0 - pad < d.right() && y1 + pad > d.top() && y0 - pad < d.bottom()
    })
}

/// 把 ArrowDisplayItem 光栅化到 pixmap：折线主体（含虚线）+ 箭头头部图元。
fn draw_arrow(pixmap: &mut Pixmap, arrow: &snow_draw_engine::ArrowDisplayItem, mask: Option<&Mask>) {
    if arrow.points.len() >= 2 {
        let mut pb = PathBuilder::new();
        pb.move_to(arrow.points[0][0] as f32, arrow.points[0][1] as f32);
        for p in &arrow.points[1..] {
            pb.line_to(p[0] as f32, p[1] as f32);
        }
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(arrow.stroke.r, arrow.stroke.g, arrow.stroke.b, arrow.stroke.a);
            paint.anti_alias = true;

            let mut stroke = Stroke::default();
            stroke.width = arrow.stroke_width.max(1.0) as f32;
            stroke.line_cap = tiny_skia::LineCap::Round;
            stroke.line_join = tiny_skia::LineJoin::Round;
            if arrow.stroke_style == StrokeStyle::Dashed {
                let dash_len = stroke.width * 3.0;
                stroke.dash = StrokeDash::new(vec![dash_len, dash_len], 0.0);
            } else if arrow.stroke_style == StrokeStyle::Dotted {
                let dot = stroke.width * 1.2;
                stroke.dash = StrokeDash::new(vec![dot, dot * 1.5], 0.0);
            }

            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), mask);
        }
    }

    // 箭头头部（三角/圆点等）图元：引擎已算好局部几何点，直接按类型光栅化。
    for head in &arrow.arrowhead_primitives {
        if head.points.len() < 2 {
            continue;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(head.points[0][0] as f32, head.points[0][1] as f32);
        for p in &head.points[1..] {
            pb.line_to(p[0] as f32, p[1] as f32);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(arrow.stroke.r, arrow.stroke.g, arrow.stroke.b, arrow.stroke.a);
            paint.anti_alias = true;
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), mask);
        }
    }
}

impl Render for RasterSpikeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = Instant::now();
        if !self.finished {
            let frame_start = render_start;

            if self.frame_index > 0 {
                self.simulate_drag_step();
            }
            let raster_secs = self.raster_frame();
            let (composite_secs, dirty_tiles) = self.composite_dirty_tiles(window, cx);

            let total_secs = frame_start.duration_since(self.last_frame_at).as_secs_f64();
            self.last_frame_at = frame_start;
            // 停顿告警：单帧间隔超过 0.5s 时输出，用于排查卡死/极慢。
            if total_secs > 0.5 {
                eprintln!("停顿: 帧#{} 间隔 {:.0} ms", self.frame_index, total_secs * 1000.0);
            }
            if self.frame_index > 0 {
                // 第一帧只做初始化，不计入统计（没有上一帧参考点）。
                self.timings.push(FrameTiming {
                    total_secs: total_secs.max(1e-9),
                    raster_secs,
                    composite_secs,
                    dirty_tiles,
                    render_secs: 0.0,
                    dirty_px: self.last_dirty_px,
                });
            }
            self.frame_index += 1;
            FRAME_COUNTER.store(self.frame_index, Ordering::Relaxed);

            if self.frame_index > TOTAL_DRAG_FRAMES {
                self.finished = true;
                self.print_report();
                eprintln!("阶段: 报告完成、调用 quit epoch_ms={}", epoch_ms());
                cx.quit();
            } else {
                window.request_animation_frame();
                cx.notify();
            }
        }

        // 每个分块一个绝对定位的 img，按比例铺满窗口。
        let mut content = div().relative().size_full();
        for (idx, tile) in self.tiles.iter().enumerate() {
            let Some(image) = tile else { continue };
            let tx = idx as u32 % self.tiles_x;
            let ty = idx as u32 / self.tiles_x;
            let x0 = tx * self.tile_size;
            let y0 = ty * self.tile_size;
            let w = self.tile_size.min(SURFACE_WIDTH - x0);
            let h = self.tile_size.min(SURFACE_HEIGHT - y0);
            content = content.child(
                img(ImageSource::Render(image.clone()))
                    .absolute()
                    .left(relative(x0 as f32 / SURFACE_WIDTH as f32))
                    .top(relative(y0 as f32 / SURFACE_HEIGHT as f32))
                    .w(relative(w as f32 / SURFACE_WIDTH as f32))
                    .h(relative(h as f32 / SURFACE_HEIGHT as f32))
                    .object_fit(ObjectFit::Fill),
            );
        }

        // 回填本帧 render 总耗时（含元素树构建），供长尾归因。
        if !self.finished {
            if let Some(t) = self.timings.last_mut() {
                t.render_secs = render_start.elapsed().as_secs_f64();
            }
        }

        div()
            .size_full()
            .bg(rgb(0x101010))
            .child(content)
    }
}

fn main() {
    // 命令行第一个参数指定分块边长，仅允许 128/256/512，缺省 256。
    let tile_size = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u32>().ok())
        .filter(|t| matches!(t, 128 | 256 | 512))
        .unwrap_or(256);
    let mask_mode = match std::env::var("SPIKE_MASK").as_deref() {
        Ok("full") => MaskMode::Full,
        Ok("none") => MaskMode::None,
        _ => MaskMode::Dirty,
    };
    spawn_watchdog();
    eprintln!("阶段: main 入口 epoch_ms={}", epoch_ms());
    gpui_platform::application().run(move |cx: &mut App| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1280.0), px(800.0)),
            })),
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| cx.new(|cx| RasterSpikeView::new(window, cx, tile_size, mask_mode)))
            .expect("open_window 失败");
        eprintln!("阶段: open_window 返回 epoch_ms={}", epoch_ms());
    });
    eprintln!("阶段: run 返回(进程即将退出) epoch_ms={}", epoch_ms());
}
