//! 手工构造 `ViewportPatch` 的光栅化对照测试：单元素、重叠、脏区增量、DPI 2x、相机缩放。
//!
//! 注意：缺少可独立运行的 C++ 黄金样本，这里以“几何解析可预期的像素”和
//! “增量结果 == 整幅重绘结果”作为回归基线。

use std::sync::Arc;

use snow_canvas_raster::{
    CanvasRasterizer, DeferredKind, RasterConfig, RasterError, RasterOutput, TileKey,
    TinySkiaRasterizer,
};
use snow_draw_engine_core::{
    Camera, ColorRgba8, CornerRadii, PathCommand, PathGeometry, Point, SurfaceSize,
    arrow::StrokeStyle,
};
use snow_draw_engine_display::{
    ArrowDisplayItem, ArrowheadDisplayDashMode, ArrowheadDisplayFillMode,
    ArrowheadDisplayPrimitive, ArrowheadDisplayPrimitiveKind, DirtyRegion, DisplayFillStyle,
    DisplayItemId, DisplayRectangleShape, DisplaySerialNumberType, FilterDisplayItem, FrameView,
    LayerPatch, RectangleDisplayItem, ReplaceRangeOp, SceneDisplayItem, SerialNumberDisplayItem,
    TextDisplayItem, ViewportPatch,
};

const RED: ColorRgba8 = ColorRgba8 {
    r: 255,
    g: 0,
    b: 0,
    a: 255,
};
const BLUE: ColorRgba8 = ColorRgba8 {
    r: 0,
    g: 0,
    b: 255,
    a: 255,
};
const NONE: ColorRgba8 = ColorRgba8 {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

/// 构造帧视图：相机居中于 surface 中心时，画布坐标等于视图坐标。
fn frame(w: u32, h: u32, zoom: f64, center: (f64, f64)) -> FrameView {
    FrameView {
        surface: SurfaceSize {
            width: w,
            height: h,
        },
        camera: Camera {
            center: Point {
                x: center.0,
                y: center.1,
            },
            zoom,
        },
        clear_color: ColorRgba8::default(),
    }
}

/// 画布==视图的 1:1 帧。
fn identity_frame(w: u32, h: u32) -> FrameView {
    frame(w, h, 1.0, (f64::from(w) / 2.0, f64::from(h) / 2.0))
}

/// 构造 reset 补丁：整幅场景 + 整幅脏区。
fn reset_patch(
    frame_view: FrameView,
    revision: u64,
    items: Vec<SceneDisplayItem>,
) -> ViewportPatch {
    let full = DirtyRegion::new(
        0.0,
        0.0,
        f64::from(frame_view.surface.width),
        f64::from(frame_view.surface.height),
    );
    ViewportPatch {
        frame_view,
        scene: LayerPatch {
            base_revision: 0,
            revision,
            reset: true,
            ops: vec![ReplaceRangeOp {
                start: 0,
                delete_count: 0,
                insert_items: items,
            }],
            dirty_regions: vec![full],
        },
        scene_render_plan: None,
        ..ViewportPatch::default()
    }
}

/// 构造增量补丁：替换 `start..start+delete` 区间并声明脏区。
fn delta_patch(
    frame_view: FrameView,
    base: u64,
    revision: u64,
    op: ReplaceRangeOp<SceneDisplayItem>,
    dirty: Vec<DirtyRegion>,
) -> ViewportPatch {
    ViewportPatch {
        frame_view,
        scene: LayerPatch {
            base_revision: base,
            revision,
            reset: false,
            ops: vec![op],
            dirty_regions: dirty,
        },
        scene_render_plan: None,
        ..ViewportPatch::default()
    }
}

/// 实心矩形元素（无描边）。
fn solid_rect(cx: f64, cy: f64, w: f64, h: f64, fill: ColorRgba8) -> RectangleDisplayItem {
    RectangleDisplayItem {
        center_x: cx,
        center_y: cy,
        width: w,
        height: h,
        fill,
        ..RectangleDisplayItem::default()
    }
}

/// 读取画布像素。
fn px(r: &TinySkiaRasterizer, x: u32, y: u32) -> [u8; 4] {
    let (w, _, data) = r.canvas_rgba().expect("画布应已创建");
    let o = (y as usize * w as usize + x as usize) * 4;
    [data[o], data[o + 1], data[o + 2], data[o + 3]]
}

/// 直线箭头元素（含分块几何与命令）。
fn line_arrow(from: [f64; 2], to: [f64; 2], stroke: ColorRgba8, width: f64) -> ArrowDisplayItem {
    let commands = vec![
        PathCommand::MoveTo { point: from },
        PathCommand::LineTo { point: to },
    ];
    ArrowDisplayItem {
        points: vec![from, to],
        geometry: Arc::new(PathGeometry::from_commands(1, commands.clone(), false)),
        path_commands: commands,
        stroke,
        stroke_width: width,
        ..ArrowDisplayItem::default()
    }
}

/// 用 64px 分块、1x 的光栅化器。
fn raster64() -> TinySkiaRasterizer {
    TinySkiaRasterizer::new(RasterConfig {
        tile_size: 64,
        device_pixel_ratio: 1.0,
    })
}

/// 收集输出块的键集合。
fn keys(out: &RasterOutput) -> Vec<TileKey> {
    let mut k: Vec<TileKey> = out.tiles.iter().map(|t| t.key).collect();
    k.sort();
    k
}

/// 实心矩形：内部为填充色，外部透明；只输出被覆盖的块。
#[test]
fn solid_rectangle_fill() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(solid_rect(100.0, 100.0, 40.0, 20.0, RED));
    let out = r
        .apply_patch(&reset_patch(identity_frame(200, 200), 1, vec![item]))
        .unwrap();
    assert_eq!(px(&r, 100, 100), [255, 0, 0, 255]);
    assert_eq!(px(&r, 81, 91), [255, 0, 0, 255]);
    assert_eq!(px(&r, 118, 108), [255, 0, 0, 255]);
    assert_eq!(px(&r, 70, 100)[3], 0);
    assert_eq!(px(&r, 100, 120)[3], 0);
    // 矩形跨 80..120 × 90..110：列 1..=1，行 1..=1（64 分块），其余块全透明不输出。
    assert_eq!(keys(&out), vec![TileKey { col: 1, row: 1 }]);
    assert!(out.full_redraw);
    assert_eq!(out.surface_size, (200, 200));
}

/// 描边矩形：边线中心不透明、内部与外部透明；线宽随元素属性。
#[test]
fn stroked_rectangle_outline() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        center_x: 100.0,
        center_y: 100.0,
        width: 60.0,
        height: 40.0,
        stroke: BLUE,
        stroke_width: 4.0,
        ..RectangleDisplayItem::default()
    });
    r.apply_patch(&reset_patch(identity_frame(200, 200), 1, vec![item]))
        .unwrap();
    // 左边线中心 x=70，宽 4 => 68..72。
    assert_eq!(px(&r, 70, 100), [0, 0, 255, 255]);
    assert_eq!(px(&r, 100, 80), [0, 0, 255, 255]);
    assert_eq!(px(&r, 100, 100)[3], 0);
    assert_eq!(px(&r, 60, 100)[3], 0);
    // 斜接直角：角点 (68,80) 内侧像素应被填充。
    assert_eq!(px(&r, 69, 81), [0, 0, 255, 255]);
}

/// 圆角矩形：角上被圆弧切掉，边中点仍被填充。
#[test]
fn rounded_rectangle_corner_cut() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        corner_radii: CornerRadii::splat(20.0),
        ..solid_rect(100.0, 100.0, 80.0, 80.0, RED)
    });
    r.apply_patch(&reset_patch(identity_frame(200, 200), 1, vec![item]))
        .unwrap();
    assert_eq!(px(&r, 100, 100), [255, 0, 0, 255]);
    assert_eq!(px(&r, 100, 61), [255, 0, 0, 255]);
    assert_eq!(px(&r, 61, 61)[3], 0);
}

/// 椭圆与菱形：形状内外判定。
#[test]
fn ellipse_and_diamond() {
    let mut r = raster64();
    let ellipse = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        shape: DisplayRectangleShape::Ellipse,
        ..solid_rect(50.0, 50.0, 60.0, 40.0, RED)
    });
    let diamond = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        shape: DisplayRectangleShape::Diamond,
        ..solid_rect(150.0, 50.0, 60.0, 40.0, BLUE)
    });
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![ellipse, diamond],
    ))
    .unwrap();
    assert_eq!(px(&r, 50, 50), [255, 0, 0, 255]);
    assert_eq!(px(&r, 21, 31)[3], 0); // 椭圆外接矩形角落
    assert_eq!(px(&r, 150, 50), [0, 0, 255, 255]);
    assert_eq!(px(&r, 122, 32)[3], 0); // 菱形外接矩形角落
}

/// 旋转矩形：旋转 90 度后宽高互换。
#[test]
fn rotated_rectangle() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        rotation: std::f64::consts::FRAC_PI_2,
        ..solid_rect(100.0, 100.0, 80.0, 20.0, RED)
    });
    r.apply_patch(&reset_patch(identity_frame(200, 200), 1, vec![item]))
        .unwrap();
    assert_eq!(px(&r, 100, 65), [255, 0, 0, 255]);
    assert_eq!(px(&r, 65, 100)[3], 0);
}

/// 实线箭头：线上不透明，线外透明；圆头线帽向端点外延伸半个线宽。
#[test]
fn solid_arrow_line_and_round_cap() {
    let mut r = raster64();
    let arrow = SceneDisplayItem::Arrow(line_arrow([20.0, 50.0], [180.0, 50.0], RED, 8.0));
    r.apply_patch(&reset_patch(identity_frame(200, 100), 1, vec![arrow]))
        .unwrap();
    assert_eq!(px(&r, 100, 50), [255, 0, 0, 255]);
    assert_eq!(px(&r, 100, 53), [255, 0, 0, 255]);
    assert_eq!(px(&r, 100, 56)[3], 0);
    assert_eq!(px(&r, 17, 50), [255, 0, 0, 255]); // 线帽外延 4px
    assert_eq!(px(&r, 14, 50)[3], 0);
}

/// 虚线箭头：Qt DashLine 图案为 {4,2} 倍线宽，圆头线帽使间隙两侧各收 0.5 线宽。
#[test]
fn dashed_arrow_pattern() {
    let mut r = raster64();
    let mut a = line_arrow([10.0, 50.0], [190.0, 50.0], RED, 6.0);
    a.stroke_style = StrokeStyle::Dashed;
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![SceneDisplayItem::Arrow(a)],
    ))
    .unwrap();
    // 线宽 6：第一段 dash 覆盖路径 0..24（x=10..34），间隙 24..36（x=34..46），线帽各外延 3。
    assert_eq!(px(&r, 22, 50), [255, 0, 0, 255]);
    assert_eq!(px(&r, 40, 50)[3], 0);
    assert_eq!(px(&r, 52, 50), [255, 0, 0, 255]); // 第二段 dash 中心（路径 42..）
}

/// 点线画笔：{0.0001,1.9999}×线宽 + 圆头线帽 => 间距 2 倍线宽的圆点。
#[test]
fn freedraw_dotted_pattern() {
    let mut r = raster64();
    let mut a = line_arrow([10.0, 50.0], [190.0, 50.0], RED, 6.0);
    a.is_free_draw = true;
    a.stroke_style = StrokeStyle::Dotted;
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![SceneDisplayItem::Arrow(a)],
    ))
    .unwrap();
    // 圆点中心在 10, 22, 34...；中点 16 处应透明。
    assert_eq!(px(&r, 10, 50), [255, 0, 0, 255]);
    assert_eq!(px(&r, 22, 50), [255, 0, 0, 255]);
    assert_eq!(px(&r, 16, 50)[3], 0);
}

/// 箭头头部图元：三角形多边形填充为描边色，“背景”填充使用画布清除色。
#[test]
fn arrowhead_polygon_primitive() {
    let mut r = raster64();
    let mut a = line_arrow([20.0, 50.0], [120.0, 50.0], RED, 4.0);
    a.arrowhead_primitives = vec![ArrowheadDisplayPrimitive {
        kind: ArrowheadDisplayPrimitiveKind::Polygon,
        points: vec![[120.0, 50.0], [100.0, 40.0], [100.0, 60.0]],
        center: [0.0, 0.0],
        diameter: 0.0,
        fill_mode: ArrowheadDisplayFillMode::Stroke,
        dash_mode: ArrowheadDisplayDashMode::Solid,
    }];
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![SceneDisplayItem::Arrow(a)],
    ))
    .unwrap();
    assert_eq!(px(&r, 108, 47), [255, 0, 0, 255]); // 三角内部、离开轴线
    assert_eq!(px(&r, 105, 30)[3], 0);
}

/// 圆点头部图元：直径按缩放放大。
#[test]
fn arrowhead_circle_primitive() {
    let mut r = raster64();
    let mut a = line_arrow([20.0, 50.0], [100.0, 50.0], RED, 2.0);
    a.arrowhead_primitives = vec![ArrowheadDisplayPrimitive {
        kind: ArrowheadDisplayPrimitiveKind::Circle,
        points: Vec::new(),
        center: [100.0, 50.0],
        diameter: 20.0,
        fill_mode: ArrowheadDisplayFillMode::Stroke,
        dash_mode: ArrowheadDisplayDashMode::Solid,
    }];
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![SceneDisplayItem::Arrow(a)],
    ))
    .unwrap();
    assert_eq!(px(&r, 100, 42), [255, 0, 0, 255]);
    assert_eq!(px(&r, 100, 30)[3], 0);
}

/// 序号形状：实心圆填充；轮廓圆只有描边，内部透明。
#[test]
fn serial_number_shapes() {
    let mut r = raster64();
    let solid = SceneDisplayItem::SerialNumber(SerialNumberDisplayItem {
        center_x: 40.0,
        center_y: 40.0,
        diameter: 40.0,
        serial_number_type: DisplaySerialNumberType::SolidCircle,
        color: RED,
        number: 1,
        ..SerialNumberDisplayItem::default()
    });
    let outlined = SceneDisplayItem::SerialNumber(SerialNumberDisplayItem {
        center_x: 120.0,
        center_y: 40.0,
        diameter: 40.0,
        serial_number_type: DisplaySerialNumberType::OutlinedCircle,
        color: BLUE,
        stroke_width: 4.0,
        number: 2,
        ..SerialNumberDisplayItem::default()
    });
    r.apply_patch(&reset_patch(
        identity_frame(200, 100),
        1,
        vec![solid, outlined],
    ))
    .unwrap();
    assert_eq!(px(&r, 40, 40), [255, 0, 0, 255]);
    assert_eq!(px(&r, 120, 40)[3], 0);
    assert_eq!(px(&r, 120, 20), [0, 0, 255, 255]); // 轮廓圆顶端
}

/// 重叠：后者（列表靠后）压在前者之上；半透明按 alpha 混合。
#[test]
fn overlap_draw_order_and_opacity() {
    let mut r = raster64();
    let below = SceneDisplayItem::Rectangle(solid_rect(80.0, 80.0, 60.0, 60.0, RED));
    let above = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        opacity: 0.5,
        ..solid_rect(110.0, 110.0, 60.0, 60.0, BLUE)
    });
    r.apply_patch(&reset_patch(
        identity_frame(200, 200),
        1,
        vec![below, above],
    ))
    .unwrap();
    assert_eq!(px(&r, 60, 60), [255, 0, 0, 255]);
    let only_blue = px(&r, 130, 130); // 只有蓝色半透明（预乘）
    assert!(only_blue[0] == 0 && (i32::from(only_blue[2]) - 128).abs() <= 2);
    assert!((i32::from(only_blue[3]) - 128).abs() <= 2);
    let mixed = px(&r, 95, 95); // 红上叠 50% 蓝
    assert!((i32::from(mixed[0]) - 128).abs() <= 2 && (i32::from(mixed[2]) - 128).abs() <= 2);
    assert_eq!(mixed[3], 255);
}

/// 斜线填充：产生部分覆盖（既有透明也有不透明像素）。
#[test]
fn hatch_fill_partial_coverage() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        fill_style: DisplayFillStyle::Line,
        stroke_width: 4.0,
        ..solid_rect(50.0, 50.0, 80.0, 80.0, RED)
    });
    r.apply_patch(&reset_patch(identity_frame(100, 100), 1, vec![item]))
        .unwrap();
    let (w, _, data) = r.canvas_rgba().unwrap();
    let mut opaque = 0;
    let mut empty = 0;
    for y in 20..80 {
        for x in 20..80 {
            let a = data[(y * w as usize + x) * 4 + 3];
            opaque += usize::from(a > 200);
            empty += usize::from(a == 0);
        }
    }
    assert!(opaque > 100, "斜线应有实心像素：{opaque}");
    assert!(empty > 500, "斜线之间应留空：{empty}");
}

/// 脏区增量：两帧之间只改一个元素，输出块只含与脏区相交的块，其余远端块不重发。
#[test]
fn incremental_only_dirty_tiles() {
    let mut r = raster64();
    let f = identity_frame(256, 256);
    let far = SceneDisplayItem::Rectangle(solid_rect(224.0, 224.0, 20.0, 20.0, BLUE));
    let near = SceneDisplayItem::Rectangle(solid_rect(32.0, 32.0, 20.0, 20.0, RED));
    let first = r.apply_patch(&reset_patch(f, 1, vec![far, near])).unwrap();
    assert_eq!(
        keys(&first),
        vec![TileKey { col: 0, row: 0 }, TileKey { col: 3, row: 3 }]
    );
    // 把 near 移到 (40,32)，脏区为旧+新位置的并集。
    let moved = SceneDisplayItem::Rectangle(solid_rect(40.0, 32.0, 20.0, 20.0, RED));
    let second = r
        .apply_patch(&delta_patch(
            f,
            1,
            2,
            ReplaceRangeOp {
                start: 1,
                delete_count: 1,
                insert_items: vec![moved],
            },
            vec![DirtyRegion::new(12.0, 12.0, 60.0, 52.0)],
        ))
        .unwrap();
    assert!(!second.full_redraw);
    assert_eq!(keys(&second), vec![TileKey { col: 0, row: 0 }]);
    assert_eq!(px(&r, 48, 32), [255, 0, 0, 255]);
    assert_eq!(px(&r, 25, 32)[3], 0, "旧位置应被清除");
    assert_eq!(px(&r, 224, 224), [0, 0, 255, 255], "远端元素保持不变");
    assert_eq!(r.cursor().unwrap().scene_revision.0, 2);
}

/// 多次增量后的画布必须与一次性整幅重绘完全一致（包括抗锯齿边缘）。
#[test]
fn incremental_matches_full_render() {
    let f = identity_frame(300, 200);
    let mut inc = raster64();
    let base_items = vec![
        SceneDisplayItem::Arrow(line_arrow([20.0, 30.0], [250.0, 160.0], RED, 6.0)),
        SceneDisplayItem::Rectangle(RectangleDisplayItem {
            shape: DisplayRectangleShape::Ellipse,
            stroke: BLUE,
            stroke_width: 3.0,
            ..solid_rect(
                150.0,
                100.0,
                90.0,
                50.0,
                ColorRgba8 {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 128,
                },
            )
        }),
    ];
    inc.apply_patch(&reset_patch(f, 1, base_items.clone()))
        .unwrap();
    let mut items = base_items;
    let mut rev = 1;
    for step in 0..6 {
        let x = 30.0 + f64::from(step) * 37.0;
        let mut arrow = line_arrow([20.0, 30.0], [x, 40.0 + f64::from(step) * 20.0], RED, 6.0);
        arrow.stroke_style = StrokeStyle::Dashed;
        let old_bounds = DirtyRegion::new(0.0, 0.0, 300.0, 200.0);
        let item = SceneDisplayItem::Arrow(arrow);
        items[0] = item.clone();
        inc.apply_patch(&delta_patch(
            f,
            rev,
            rev + 1,
            ReplaceRangeOp {
                start: 0,
                delete_count: 1,
                insert_items: vec![item],
            },
            vec![old_bounds],
        ))
        .unwrap();
        rev += 1;
    }
    let mut full = raster64();
    full.apply_patch(&reset_patch(f, rev, items)).unwrap();
    assert_eq!(inc.canvas_rgba().unwrap().2, full.canvas_rgba().unwrap().2);
}

/// 元素删除后旧块变空：应出现在释放列表，而不是发送全透明块。
#[test]
fn removed_item_releases_tile() {
    let mut r = raster64();
    let f = identity_frame(128, 128);
    let item = SceneDisplayItem::Rectangle(solid_rect(32.0, 32.0, 20.0, 20.0, RED));
    r.apply_patch(&reset_patch(f, 1, vec![item])).unwrap();
    let out = r
        .apply_patch(&delta_patch(
            f,
            1,
            2,
            ReplaceRangeOp {
                start: 0,
                delete_count: 1,
                insert_items: Vec::new(),
            },
            vec![DirtyRegion::new(10.0, 10.0, 55.0, 55.0)],
        ))
        .unwrap();
    assert!(out.tiles.is_empty());
    assert_eq!(out.released, vec![TileKey { col: 0, row: 0 }]);
    let released_on_reset = r.reset();
    assert!(released_on_reset.is_empty(), "已释放的块不应重复释放");
}

/// DPI 2x：画布物理尺寸翻倍，元素几何与线宽按 2 倍缩放。
#[test]
fn device_pixel_ratio_2x() {
    let mut r = TinySkiaRasterizer::new(RasterConfig {
        tile_size: 64,
        device_pixel_ratio: 2.0,
    });
    let item = SceneDisplayItem::Rectangle(RectangleDisplayItem {
        stroke: BLUE,
        stroke_width: 2.0,
        ..solid_rect(50.0, 50.0, 40.0, 20.0, RED)
    });
    let out = r
        .apply_patch(&reset_patch(identity_frame(100, 100), 1, vec![item]))
        .unwrap();
    assert_eq!(out.surface_size, (200, 200));
    // 逻辑矩形 30..70 × 40..60 => 物理 60..140 × 80..120；描边逻辑 2 => 物理 4（58..62）。
    assert_eq!(px(&r, 100, 100), [255, 0, 0, 255]);
    assert_eq!(px(&r, 60, 100), [0, 0, 255, 255]);
    assert_eq!(px(&r, 56, 100)[3], 0);
    assert_eq!(px(&r, 143, 100)[3], 0);
    assert_eq!(px(&r, 100, 76)[3], 0);
}

/// 相机缩放与平移：视图点 = (画布点 - 中心) * zoom + surface/2。
#[test]
fn camera_zoom_and_pan() {
    let mut r = raster64();
    // 中心 (10,10)、zoom 2、surface 200x200：画布 (10,10) 落在视图 (100,100)。
    let f = frame(200, 200, 2.0, (10.0, 10.0));
    let item = SceneDisplayItem::Rectangle(solid_rect(10.0, 10.0, 10.0, 10.0, RED));
    r.apply_patch(&reset_patch(f, 1, vec![item])).unwrap();
    // 边长 10*2=20 => 视图 90..110。
    assert_eq!(px(&r, 100, 100), [255, 0, 0, 255]);
    assert_eq!(px(&r, 92, 92), [255, 0, 0, 255]);
    assert_eq!(px(&r, 85, 100)[3], 0);
}

/// 相机变化视为整幅重绘，并清掉旧内容。
#[test]
fn camera_change_triggers_full_redraw() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(solid_rect(50.0, 50.0, 20.0, 20.0, RED));
    r.apply_patch(&reset_patch(identity_frame(100, 100), 1, vec![item]))
        .unwrap();
    let shifted = frame(100, 100, 1.0, (0.0, 0.0));
    let out = r
        .apply_patch(&delta_patch(
            shifted,
            1,
            1,
            ReplaceRangeOp {
                start: 0,
                delete_count: 0,
                insert_items: Vec::new(),
            },
            Vec::new(),
        ))
        .unwrap();
    assert!(out.full_redraw);
    // 新相机下画布 (50,50) 落在视图 (100,100)，矩形只剩 90..100 的一角；旧位置被清空。
    assert_eq!(px(&r, 95, 95), [255, 0, 0, 255]);
    assert_eq!(px(&r, 50, 50)[3], 0);
}

/// 尺寸变化：旧网格之外的块被释放。
#[test]
fn surface_shrink_releases_outside_tiles() {
    let mut r = raster64();
    let item = SceneDisplayItem::Rectangle(solid_rect(200.0, 200.0, 20.0, 20.0, RED));
    r.apply_patch(&reset_patch(
        identity_frame(256, 256),
        1,
        vec![item.clone()],
    ))
    .unwrap();
    let mut small = reset_patch(identity_frame(128, 128), 2, vec![item]);
    small.scene.base_revision = 1;
    let out = r.apply_patch(&small).unwrap();
    assert!(out.tiles.is_empty());
    let mut released = out.released.clone();
    released.sort();
    let expect = [(2, 2), (2, 3), (3, 2), (3, 3)].map(|(col, row)| TileKey { col, row });
    assert_eq!(released, expect.to_vec());
    assert_eq!(out.surface_size, (128, 128));
}

/// 文字与滤镜：不绘制，登记为待外部绘制元素。
#[test]
fn text_and_filter_are_deferred() {
    let mut r = raster64();
    let text = SceneDisplayItem::Text(TextDisplayItem {
        id: DisplayItemId {
            index: 1,
            generation: 1,
        },
        center_x: 50.0,
        center_y: 50.0,
        width: 40.0,
        height: 20.0,
        fill: RED,
        ..TextDisplayItem::default()
    });
    let filter = SceneDisplayItem::Filter(FilterDisplayItem {
        id: DisplayItemId {
            index: 2,
            generation: 1,
        },
        center_x: 120.0,
        center_y: 50.0,
        width: 40.0,
        height: 20.0,
        ..FilterDisplayItem::default()
    });
    let out = r
        .apply_patch(&reset_patch(
            identity_frame(200, 100),
            1,
            vec![text, filter],
        ))
        .unwrap();
    assert!(out.tiles.is_empty(), "占位不应产生像素");
    let kinds: Vec<DeferredKind> = out.deferred.iter().map(|d| d.kind).collect();
    assert_eq!(kinds, vec![DeferredKind::Text, DeferredKind::Filter]);
    assert!(out.deferred[0].bounds[0] <= 30.0 && out.deferred[0].bounds[2] >= 70.0);
}

/// 错误路径：未初始化、基线不匹配、区间越界。
#[test]
fn error_paths() {
    let f = identity_frame(64, 64);
    let noop = |base, rev, start, del| {
        delta_patch(
            f,
            base,
            rev,
            ReplaceRangeOp {
                start,
                delete_count: del,
                insert_items: Vec::new(),
            },
            Vec::new(),
        )
    };
    let mut r = raster64();
    assert_eq!(
        r.apply_patch(&noop(0, 1, 0, 0)).unwrap_err(),
        RasterError::NotInitialized
    );
    r.apply_patch(&reset_patch(f, 5, Vec::new())).unwrap();
    assert_eq!(
        r.apply_patch(&noop(4, 6, 0, 0)).unwrap_err(),
        RasterError::RevisionMismatch {
            expected: 5,
            got: 4
        }
    );
    assert!(r.cursor().is_none(), "出错后状态作废");
    r.apply_patch(&reset_patch(f, 5, Vec::new())).unwrap();
    assert_eq!(
        r.apply_patch(&noop(5, 6, 3, 1)).unwrap_err(),
        RasterError::InvalidOp
    );
}

/// 配置越界值被夹到合法范围。
#[test]
fn config_is_normalized() {
    let r = TinySkiaRasterizer::new(RasterConfig {
        tile_size: 1,
        device_pixel_ratio: f32::NAN,
    });
    assert_eq!(r.tile_size(), 16);
    let cfg = RasterConfig {
        tile_size: 9999,
        device_pixel_ratio: 100.0,
    }
    .normalized();
    assert_eq!((cfg.tile_size, cfg.device_pixel_ratio), (1024, 8.0));
}

/// 固定复合场景（矩形+虚线箭头+序号），供哈希基线与 PNG 导出共用。
fn composite_scene() -> Vec<SceneDisplayItem> {
    let mut arrow = line_arrow([20.0, 20.0], [180.0, 100.0], RED, 6.0);
    arrow.stroke_style = StrokeStyle::Dashed;
    arrow.arrowhead_primitives = vec![ArrowheadDisplayPrimitive {
        kind: ArrowheadDisplayPrimitiveKind::Polygon,
        points: vec![[180.0, 100.0], [160.0, 96.0], [166.0, 112.0]],
        center: [0.0, 0.0],
        diameter: 0.0,
        fill_mode: ArrowheadDisplayFillMode::Stroke,
        dash_mode: ArrowheadDisplayDashMode::Solid,
    }];
    vec![
        SceneDisplayItem::Rectangle(RectangleDisplayItem {
            stroke: BLUE,
            stroke_width: 3.0,
            fill: ColorRgba8 {
                r: 255,
                g: 200,
                b: 0,
                a: 160,
            },
            corner_radii: CornerRadii::splat(8.0),
            ..solid_rect(100.0, 60.0, 120.0, 70.0, NONE)
        }),
        SceneDisplayItem::Arrow(arrow),
        SceneDisplayItem::SerialNumber(SerialNumberDisplayItem {
            center_x: 40.0,
            center_y: 110.0,
            diameter: 24.0,
            serial_number_type: DisplaySerialNumberType::SolidSquare,
            color: RED,
            corner_radii: CornerRadii::splat(4.0),
            stroke_width: 2.0,
            ..SerialNumberDisplayItem::default()
        }),
    ]
}

/// 回归基线：固定复合场景的画布 FNV-1a 校验和（缺 C++ 黄金样本时的替代基线）。
#[test]
fn golden_hash_composite_scene() {
    let mut r = raster64();
    r.apply_patch(&reset_patch(identity_frame(200, 130), 1, composite_scene()))
        .unwrap();
    let hash = r
        .canvas_rgba()
        .unwrap()
        .2
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        });
    assert_eq!(hash, GOLDEN_HASH, "光栅化输出变化：新哈希 {hash:016x}");
}

/// 人工目检辅助：把复合场景导出为 PNG（`SNOW_RASTER_DUMP_DIR` 指定目录），默认忽略。
#[test]
#[ignore = "仅用于人工目检"]
fn dump_composite_png() {
    let Ok(dir) = std::env::var("SNOW_RASTER_DUMP_DIR") else {
        return;
    };
    let mut r = raster64();
    r.apply_patch(&reset_patch(identity_frame(200, 130), 1, composite_scene()))
        .unwrap();
    let (w, h, data) = r.canvas_rgba().unwrap();
    let pixmap =
        tiny_skia::Pixmap::from_vec(data.to_vec(), tiny_skia::IntSize::from_wh(w, h).unwrap())
            .unwrap();
    pixmap
        .save_png(std::path::Path::new(&dir).join("composite.png"))
        .unwrap();
}

/// 复合场景哈希基线（由本实现首次运行生成，非 C++ 输出）。
const GOLDEN_HASH: u64 = 0x3f71_54c6_5466_d615;
