//! 引擎驱动的端到端测试：真实 `acquire_patch` 游标回传、增量正确性与 4K 单箭头拖动耗时。

use std::time::Instant;

use snow_canvas_raster::{CanvasRasterizer, RasterConfig, TinySkiaRasterizer};
use snow_draw_engine::{
    ActiveTool, Arrowhead, ColorRgba8, Engine, InputEvent, Modifiers, Point, PointerButton,
    PointerButtons, PointerDevice, PointerEvent, PointerEventType, ShapeKind, ShapeStylePatch,
    StrokeStyle, ViewportConfig, ViewportId,
};
use snow_draw_engine_editor::{
    SHAPE_STYLE_PROPERTY_END_ARROWHEAD, SHAPE_STYLE_PROPERTY_STROKE_STYLE,
};

/// 创建视口、切到箭头工具（虚线 + 三角头）并落下起点。
fn setup(width: u32, height: u32) -> (Engine, ViewportId) {
    let mut engine = Engine::default();
    let id = engine
        .create_viewport(ViewportConfig::default())
        .expect("create_viewport");
    engine
        .set_viewport_surface_size(id, width, height)
        .expect("surface");
    engine
        .set_viewport_active_tool(id, ActiveTool::Arrow)
        .expect("tool");
    let mut style = engine
        .viewport_style_toolbar_state(id)
        .expect("style")
        .shape_style;
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
            id,
            ShapeStylePatch {
                kind: ShapeKind::Arrow,
                style,
                properties: SHAPE_STYLE_PROPERTY_STROKE_STYLE | SHAPE_STYLE_PROPERTY_END_ARROWHEAD,
            },
        )
        .expect("style patch");
    pointer(&mut engine, id, PointerEventType::Down, 400.0, 400.0);
    (engine, id)
}

/// 喂一个指针事件。
fn pointer(engine: &mut Engine, id: ViewportId, kind: PointerEventType, x: f64, y: f64) {
    engine
        .process_input_with_viewport_changes(
            id,
            InputEvent::Pointer(PointerEvent {
                pointer_id: 1,
                event_type: kind,
                device: PointerDevice::Mouse,
                position: Point::new(x, y),
                button: (kind == PointerEventType::Down).then_some(PointerButton::Primary),
                buttons: PointerButtons(PointerButtons::PRIMARY),
                modifiers: Modifiers::default(),
            }),
        )
        .expect("pointer");
}

/// 第 step 步拖动的终点（对照 spike 的拖动轨迹）。
fn drag_point(step: usize) -> (f64, f64) {
    let p = step as f64;
    (400.0 + p * 8.0, 400.0 + (p * 0.35).sin() * 260.0 + p * 2.0)
}

/// 增量光栅化（回传游标）的画布必须与“无游标整幅重绘”逐字节一致，且增量帧只输出部分块。
#[test]
fn engine_drag_incremental_equals_full() {
    let (w, h) = (1280, 720);
    let (mut engine, id) = setup(w, h);
    let cfg = RasterConfig::default();
    let mut inc = TinySkiaRasterizer::new(cfg);
    let total_tiles = (w.div_ceil(256) * h.div_ceil(256)) as usize;
    let mut partial_frames = 0;

    for step in 1..=40 {
        let (x, y) = drag_point(step);
        pointer(&mut engine, id, PointerEventType::Move, x, y);
        let patch = engine.acquire_patch(id, inc.cursor()).unwrap();
        let out = inc.apply_patch(&patch).unwrap();
        if !out.full_redraw && !out.tiles.is_empty() && out.tiles.len() < total_tiles {
            partial_frames += 1;
        }
        if step % 10 == 0 {
            let mut full = TinySkiaRasterizer::new(cfg);
            let fp = engine.acquire_patch(id, None).unwrap();
            full.apply_patch(&fp).unwrap();
            assert_eq!(
                inc.canvas_rgba().unwrap().2,
                full.canvas_rgba().unwrap().2,
                "第 {step} 步增量与整幅重绘不一致"
            );
        }
    }
    assert!(partial_frames >= 20, "增量帧过少：{partial_frames}");
    // 画布上确实画出了内容。
    assert!(inc.canvas_rgba().unwrap().2.iter().any(|&b| b != 0));
}

/// 4K 全屏单箭头（虚线+三角头）拖动：记录纯光栅化 `apply_patch` 每帧耗时（不含引擎与上传）。
///
/// 目标与 spike 一致约 0.5ms/帧（release）；debug 构建不做断言。
#[test]
fn perf_4k_single_arrow_drag() {
    let (mut engine, id) = setup(3840, 2160);
    let mut raster = TinySkiaRasterizer::new(RasterConfig::default());
    // 首帧（整幅 4K，含空画布分配）单独计时。
    let first = engine.acquire_patch(id, None).unwrap();
    let t0 = Instant::now();
    raster.apply_patch(&first).unwrap();
    let first_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut times = Vec::new();
    let mut tile_counts = Vec::new();
    for step in 1..=300 {
        let (x, y) = drag_point(step);
        pointer(&mut engine, id, PointerEventType::Move, x, y);
        let patch = engine.acquire_patch(id, raster.cursor()).unwrap();
        let t = Instant::now();
        let out = raster.apply_patch(&patch).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        tile_counts.push(out.tiles.len());
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let p50 = times[times.len() / 2];
    let p99 = times[(times.len() as f64 * 0.99) as usize];
    let max = times[times.len() - 1];
    let avg_tiles = tile_counts.iter().sum::<usize>() as f64 / tile_counts.len() as f64;
    println!(
        "4K 单箭头拖动 300 帧（含拷出脏块）：首帧 {first_ms:.2} ms；每帧 平均 {avg:.3} ms, \
         P50 {p50:.3} ms, P99 {p99:.3} ms, 最大 {max:.3} ms；平均输出块 {avg_tiles:.1}"
    );
    if !cfg!(debug_assertions) {
        assert!(avg < 5.0, "release 下每帧平均应远小于 5ms，实测 {avg:.3}");
    }
}
