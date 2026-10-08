//! 装饰层（聚光灯 / 水印）的确定性离屏测试：洞内外像素、标注保留、增量与整屏一致、水印几何与覆盖量。

use snow_canvas_raster::decoration::{CoverageBitmap, DecorationLayer, RegionRect};
use snow_canvas_raster::{CanvasRasterizer, RasterConfig, RasterError, TinySkiaRasterizer};
use snow_draw_engine_core::{Camera, ColorRgba8, Point, SurfaceSize};
use snow_draw_engine_display::{
    DecorationPatch, DecorationView, DirtyRegion, DisplaySpotlightConfig, DisplaySpotlightCutout,
    DisplayWatermarkConfig, FrameView, ReplaceRangeOp, ViewportPatch,
};

/// 测试用分块边长。
const TILE: i32 = 32;
/// 墨迹块宽（测试用假文字位图）。
const INK_W: u32 = 12;
/// 墨迹块高。
const INK_H: u32 = 6;
/// 假文字位图四周留白（验证墨迹包围盒裁剪）。
const INK_PAD: u32 = 2;
/// 白色不透明。
const WHITE: ColorRgba8 = ColorRgba8 {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

/// 构造相机居中的帧视图（画布坐标等于视图坐标）。
fn frame(w: u32, h: u32) -> FrameView {
    FrameView {
        surface: SurfaceSize {
            width: w,
            height: h,
        },
        camera: Camera {
            center: Point {
                x: f64::from(w) / 2.0,
                y: f64::from(h) / 2.0,
            },
            zoom: 1.0,
        },
        clear_color: ColorRgba8::default(),
    }
}

/// 假文字光栅化：固定实心块 + 四周留白，与文本内容无关，保证确定性。
fn fake_text(_: &str, _: &str, _: f32) -> Option<CoverageBitmap> {
    let (w, h) = (INK_W + INK_PAD * 2, INK_H + INK_PAD * 2);
    let mut coverage = vec![0u8; (w * h) as usize];
    for y in INK_PAD..INK_PAD + INK_H {
        for x in INK_PAD..INK_PAD + INK_W {
            coverage[(y * w + x) as usize] = 255;
        }
    }
    Some(CoverageBitmap {
        width: w,
        height: h,
        coverage,
    })
}

/// 构造聚光灯配置。
fn spotlight(color: ColorRgba8, opacity: f64, active: bool) -> DisplaySpotlightConfig {
    DisplaySpotlightConfig {
        color,
        opacity,
        active,
    }
}

/// 构造水印配置。
fn watermark(text: &str, angle: f64, gap: f64, opacity: f64) -> DisplayWatermarkConfig {
    let mut w = DisplayWatermarkConfig {
        color: WHITE,
        angle,
        gap,
        opacity,
        ..DisplayWatermarkConfig::default()
    };
    w.text[..text.len()].copy_from_slice(text.as_bytes());
    w.text_len = text.len() as u16;
    w
}

/// 构造矩形洞。
fn cutout(cx: f64, cy: f64, w: f64, h: f64, rotation: f64) -> DisplaySpotlightCutout {
    DisplaySpotlightCutout {
        center_x: cx,
        center_y: cy,
        width: w,
        height: h,
        rotation,
    }
}

/// 构造整屏 reset 装饰补丁。
fn reset_patch(
    revision: u64,
    view: DecorationView,
    cutouts: Vec<DisplaySpotlightCutout>,
) -> DecorationPatch {
    DecorationPatch {
        base_revision: 0,
        revision,
        reset: true,
        view,
        spotlight_ops: vec![ReplaceRangeOp {
            start: 0,
            delete_count: 0,
            insert_items: cutouts,
        }],
        dirty_regions: Vec::new(),
    }
}

/// 创建并应用 reset 补丁的装饰层。
fn layer_with(view: DecorationView, cutouts: Vec<DisplaySpotlightCutout>) -> DecorationLayer {
    let mut layer = DecorationLayer::default();
    layer.apply_patch(&reset_patch(1, view, cutouts)).unwrap();
    layer
}

/// 取 RGBA 缓冲里 `(x, y)` 像素（缓冲左上角为 `origin`，宽为 `w`）。
fn px(buf: &[u8], w: i32, origin: (i32, i32), x: i32, y: i32) -> [u8; 4] {
    let o = (((y - origin.1) * w + (x - origin.0)) * 4) as usize;
    [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
}

/// 渲染整个 `w x h` 画布（单个区域）。
fn render_all(layer: &mut DecorationLayer, w: u32, h: u32, dpr: f64) -> Option<Vec<u8>> {
    layer.render_region(
        &frame(w, h),
        dpr,
        [
            0,
            0,
            (f64::from(w) * dpr).round() as i32,
            (f64::from(h) * dpr).round() as i32,
        ],
        &mut fake_text,
    )
}

/// 按 `TILE` 分块渲染 `rects`（只渲染给定块），写进整屏缓冲 `canvas`（宽 `cw`）。
fn render_tiles_into(
    layer: &mut DecorationLayer,
    frame: &FrameView,
    dpr: f64,
    canvas: &mut [u8],
    cw: i32,
    ch: i32,
    tiles: &[(i32, i32)],
) {
    for &(col, row) in tiles {
        let rect: RegionRect = [
            col * TILE,
            row * TILE,
            ((col + 1) * TILE).min(cw),
            ((row + 1) * TILE).min(ch),
        ];
        let tile = layer.render_region(frame, dpr, rect, &mut fake_text);
        for y in rect[1]..rect[3] {
            for x in rect[0]..rect[2] {
                let o = ((y * cw + x) * 4) as usize;
                let px = match &tile {
                    Some(t) => {
                        let to =
                            (((y - rect[1]) * (rect[2] - rect[0]) + (x - rect[0])) * 4) as usize;
                        [t[to], t[to + 1], t[to + 2], t[to + 3]]
                    }
                    None => [0; 4],
                };
                canvas[o..o + 4].copy_from_slice(&px);
            }
        }
    }
}

/// 全部分块坐标。
fn all_tiles(cw: i32, ch: i32) -> Vec<(i32, i32)> {
    let (cols, rows) = ((cw + TILE - 1) / TILE, (ch + TILE - 1) / TILE);
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (c, r)))
        .collect()
}

/// 预乘 source-over：`dst = src + dst * (1 - sa)`。
fn over(dst: &mut [u8; 4], src: [u8; 4]) {
    let inv = 255 - u32::from(src[3]);
    for i in 0..4 {
        dst[i] = (u32::from(src[i]) + (u32::from(dst[i]) * inv + 127) / 255).min(255) as u8;
    }
}

/// 无洞时整块纯色快路径：每个像素都是 `color * opacity`（预乘）。
#[test]
fn spotlight_without_cutouts_fills_solid() {
    let view = DecorationView {
        spotlight: spotlight(WHITE, 0.5, true),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![]);
    let buf = render_all(&mut layer, 64, 48, 1.0).unwrap();
    let first = px(&buf, 64, (0, 0), 0, 0);
    assert_eq!(first, [128, 128, 128, 128]);
    assert!(buf.chunks_exact(4).all(|p| p == first));
}

/// 洞内全透明、洞外压暗，不旋转的矩形洞边界落在整数像素上，无抗锯齿过渡。
#[test]
fn spotlight_hole_inside_and_outside_pixels() {
    let view = DecorationView {
        spotlight: spotlight(WHITE, 1.0, true),
        ..DecorationView::default()
    };
    // 洞 [44,84) x [36,60)
    let mut layer = layer_with(view, vec![cutout(64.0, 48.0, 40.0, 24.0, 0.0)]);
    let buf = render_all(&mut layer, 128, 96, 1.0).unwrap();
    assert_eq!(px(&buf, 128, (0, 0), 64, 48), [0, 0, 0, 0], "洞心");
    assert_eq!(px(&buf, 128, (0, 0), 44, 36), [0, 0, 0, 0], "洞左上角内侧");
    assert_eq!(px(&buf, 128, (0, 0), 83, 59), [0, 0, 0, 0], "洞右下角内侧");
    assert_eq!(
        px(&buf, 128, (0, 0), 43, 48),
        WHITE.into_array(),
        "洞左外侧"
    );
    assert_eq!(
        px(&buf, 128, (0, 0), 84, 48),
        WHITE.into_array(),
        "洞右外侧"
    );
    assert_eq!(px(&buf, 128, (0, 0), 5, 5), WHITE.into_array(), "远处");
}

/// 旋转 90 度的洞：宽高对调。
#[test]
fn spotlight_rotated_hole() {
    let view = DecorationView {
        spotlight: spotlight(WHITE, 1.0, true),
        ..DecorationView::default()
    };
    let mut layer = layer_with(
        view,
        vec![cutout(64.0, 48.0, 40.0, 10.0, std::f64::consts::FRAC_PI_2)],
    );
    let buf = render_all(&mut layer, 128, 96, 1.0).unwrap();
    assert_eq!(px(&buf, 128, (0, 0), 64, 48 - 15)[3], 0, "竖向在洞内");
    assert_eq!(px(&buf, 128, (0, 0), 64 + 15, 48)[3], 255, "横向在洞外");
}

/// 多个重叠洞取并集：重叠区仍是透明，且边缘处不会比单洞更不透明。
#[test]
fn spotlight_overlapping_holes_are_union() {
    let view = DecorationView {
        spotlight: spotlight(WHITE, 1.0, true),
        ..DecorationView::default()
    };
    let mut layer = layer_with(
        view,
        vec![
            cutout(50.0, 48.0, 40.0, 24.0, 0.0),
            cutout(70.0, 48.0, 40.0, 24.0, 0.0),
        ],
    );
    let buf = render_all(&mut layer, 128, 96, 1.0).unwrap();
    assert_eq!(px(&buf, 128, (0, 0), 60, 48)[3], 0, "重叠区");
    assert_eq!(px(&buf, 128, (0, 0), 35, 48)[3], 0, "左洞独占区");
    assert_eq!(px(&buf, 128, (0, 0), 85, 48)[3], 0, "右洞独占区");
    assert_eq!(px(&buf, 128, (0, 0), 20, 48)[3], 255, "两洞之外");
}

/// 停用、不透明度为 0、颜色全透明时，聚光灯与整个装饰层都不出像素。
#[test]
fn inactive_spotlight_renders_nothing() {
    for s in [
        spotlight(WHITE, 0.5, false),
        spotlight(WHITE, 0.0, true),
        spotlight(ColorRgba8 { a: 0, ..WHITE }, 0.5, true),
    ] {
        let view = DecorationView {
            spotlight: s,
            ..DecorationView::default()
        };
        let mut layer = layer_with(view, vec![cutout(10.0, 10.0, 5.0, 5.0, 0.0)]);
        assert!(!layer.is_active());
        assert!(render_all(&mut layer, 64, 48, 1.0).is_none());
    }
}

/// 洞内的标注不被擦掉，洞外的标注被压暗：聚光灯层叠在标注层之上而不是擦它。
#[test]
fn annotation_inside_hole_is_preserved() {
    let view = DecorationView {
        spotlight: spotlight(
            ColorRgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            0.5,
            true,
        ),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![cutout(64.0, 48.0, 40.0, 24.0, 0.0)]);
    let deco = render_all(&mut layer, 128, 96, 1.0).unwrap();
    let red = [255u8, 0, 0, 255];
    let mut inside = red;
    over(&mut inside, px(&deco, 128, (0, 0), 64, 48));
    let mut outside = red;
    over(&mut outside, px(&deco, 128, (0, 0), 10, 10));
    assert_eq!(inside, red, "洞内标注原样保留");
    // 50% alpha 在 8 位预乘里取整为 127 或 128
    assert!(
        outside[0].abs_diff(128) <= 1 && outside[1] == 0 && outside[2] == 0 && outside[3] == 255,
        "洞外标注被 50% 黑压暗: {outside:?}"
    );
}

/// 渲染某个区域得到的像素与整屏渲染同一区域逐字节一致（区域对齐整数像素）。
fn assert_tiles_equal_whole(dpr: f64, tolerance: u8) {
    for (name, spot, mark) in [
        ("聚光灯", true, false),
        ("水印", false, true),
        ("两者", true, true),
    ] {
        assert_tiles_equal_whole_with(dpr, tolerance, name, spot, mark);
    }
}

/// 同上，可选只开聚光灯或只开水印，便于定位差异来源。
fn assert_tiles_equal_whole_with(dpr: f64, tolerance: u8, name: &str, spot: bool, mark: bool) {
    let (w, h) = (300_u32, 270_u32);
    let f = frame(w, h);
    let view = DecorationView {
        spotlight: spotlight(WHITE, 0.6, spot),
        watermark: watermark(if mark { "W" } else { "" }, 30.0, 20.0, 0.5),
    };
    let mut layer = layer_with(
        view,
        vec![
            cutout(40.0, 40.0, 30.5, 18.25, 0.4),
            cutout(252.0, 130.0, 40.0, 22.0, 1.1),
        ],
    );
    let (pw, ph) = (
        (f64::from(w) * dpr).round() as i32,
        (f64::from(h) * dpr).round() as i32,
    );
    let mut tiled = vec![0u8; (pw * ph * 4) as usize];
    render_tiles_into(&mut layer, &f, dpr, &mut tiled, pw, ph, &all_tiles(pw, ph));
    let whole = layer
        .render_region(&f, dpr, [0, 0, pw, ph], &mut fake_text)
        .unwrap();
    let at = tiled.iter().zip(&whole).position(|(a, b)| a != b);
    let worst = tiled
        .iter()
        .zip(&whole)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(
        worst <= tolerance,
        "{name} dpr {dpr}: 分块与整屏最大差 {worst}，首个差异字节 {at:?}（像素 {:?}）",
        at.map(|i| ((i / 4) as i32 % pw, (i / 4) as i32 / pw))
    );
}

/// 分块渲染与整屏单区域渲染逐字节一致（内部按固定网格渲染），含 125% / 150% DPR（跨网格接缝）。
#[test]
fn tiled_render_matches_whole_render() {
    assert_tiles_equal_whole(1.0, 0);
    assert_tiles_equal_whole(1.25, 0);
    assert_tiles_equal_whole(1.5, 0);
}

/// 增量脏区重画与整屏重画逐字节一致（`ViewportPatch` 协议核心不变量）。
#[test]
fn incremental_dirty_redraw_equals_full_redraw() {
    let (w, h) = (300_i32, 270_i32);
    let f = frame(w as u32, h as u32);
    let view = DecorationView {
        spotlight: spotlight(WHITE, 0.6, true),
        watermark: watermark("W", 30.0, 20.0, 0.5),
    };
    let first = vec![
        cutout(40.0, 40.0, 30.5, 18.25, 0.4),
        cutout(252.0, 130.0, 40.0, 22.0, 1.1),
    ];
    let mut layer = layer_with(view, first.clone());
    let mut canvas = vec![0u8; (w * h * 4) as usize];
    render_tiles_into(&mut layer, &f, 1.0, &mut canvas, w, h, &all_tiles(w, h));

    // 把第二个洞换成新位置：增量补丁只带这一处区间替换和新旧包围盒脏区
    let moved = cutout(270.0, 200.0, 40.0, 22.0, 1.1);
    let patch = DecorationPatch {
        base_revision: 1,
        revision: 2,
        reset: false,
        view,
        spotlight_ops: vec![ReplaceRangeOp {
            start: 1,
            delete_count: 1,
            insert_items: vec![moved],
        }],
        dirty_regions: vec![
            DirtyRegion::new(225.0, 100.0, 280.0, 160.0),
            DirtyRegion::new(245.0, 170.0, 300.0, 230.0),
        ],
    };
    layer.apply_patch(&patch).unwrap();
    // 只重画被脏区触及的块
    let dirty_tiles: Vec<(i32, i32)> = all_tiles(w, h)
        .into_iter()
        .filter(|&(c, r)| {
            let (x0, y0, x1, y1) = (c * TILE, r * TILE, (c + 1) * TILE, (r + 1) * TILE);
            patch.dirty_regions.iter().any(|d| {
                d.max_x > f64::from(x0)
                    && d.min_x < f64::from(x1)
                    && d.max_y > f64::from(y0)
                    && d.min_y < f64::from(y1)
            })
        })
        .collect();
    assert!(
        dirty_tiles.len() < all_tiles(w, h).len(),
        "增量必须只触及部分块"
    );
    render_tiles_into(&mut layer, &f, 1.0, &mut canvas, w, h, &dirty_tiles);

    // 对照：全新装饰层按最终状态整屏重画
    let mut fresh = layer_with(view, vec![first[0], moved]);
    let mut expected = vec![0u8; (w * h * 4) as usize];
    render_tiles_into(&mut fresh, &f, 1.0, &mut expected, w, h, &all_tiles(w, h));
    assert_eq!(canvas, expected);
}

/// 水印几何：墨迹包围盒裁掉留白，步长 = 墨迹尺寸 + 间距，单元为两倍步长。
#[test]
fn watermark_geometry_and_cell() {
    let view = DecorationView {
        watermark: watermark("W", 0.0, 20.0, 1.0),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![]);
    assert!(render_all(&mut layer, 200, 200, 1.0).is_some());
    let g = layer.watermark_geometry().unwrap();
    assert_eq!((g.ink_width, g.ink_height), (INK_W, INK_H));
    assert_eq!(g.step_x, f64::from(INK_W) + 20.0);
    assert_eq!(g.step_y, f64::from(INK_H) + 20.0);
    assert_eq!((g.cell_width, g.cell_height), (64, 52));
    // 缩放 2 倍（DPR）：间距与墨迹都按 2 倍，假文字不随字号变，所以只有间距翻倍
    let mut layer2 = layer_with(view, vec![]);
    assert!(render_all(&mut layer2, 100, 100, 2.0).is_some());
    let g2 = layer2.watermark_geometry().unwrap();
    assert_eq!(g2.step_x, f64::from(INK_W) + 40.0);
}

/// 逻辑像素换算：字号与间距按 DPR 放大成物理像素（125% / 150%），非法系数按 1。
#[test]
fn watermark_logical_scale_converts_font_and_gap() {
    // 默认字号 16、间距 56（逻辑像素）
    let view = DecorationView {
        watermark: {
            let mut w = watermark("W", 0.0, 56.0, 1.0);
            w.font_size = 16.0;
            w
        },
        ..DecorationView::default()
    };
    for (scale, want_px, want_gap) in [(1.0, 16.0, 56.0), (1.25, 20.0, 70.0), (1.5, 24.0, 84.0)] {
        let mut layer = layer_with(view, vec![]);
        layer.set_logical_scale(scale);
        let mut seen: Vec<f32> = Vec::new();
        let mut text = |t: &str, f: &str, px: f32| {
            seen.push(px);
            fake_text(t, f, px)
        };
        let out = layer.render_region(&frame(300, 300), 1.0, [0, 0, 300, 300], &mut text);
        assert!(out.is_some());
        assert_eq!(seen, vec![want_px as f32], "scale {scale}");
        let g = layer.watermark_geometry().unwrap();
        assert_eq!(g.step_x, f64::from(INK_W) + want_gap, "scale {scale}");
        assert_eq!(g.step_y, f64::from(INK_H) + want_gap, "scale {scale}");
        assert_eq!(
            snow_canvas_raster::decoration::watermark_physical(16.0, 56.0, scale),
            (want_px, want_gap)
        );
    }
    let mut layer = layer_with(view, vec![]);
    layer.set_logical_scale(f64::NAN);
    assert_eq!(layer.logical_scale(), 1.0);
    layer.set_logical_scale(-2.0);
    assert_eq!(layer.logical_scale(), 1.0);
}

/// 不旋转时水印位置：锚点（渲染区中心）处是第一个单元的墨迹左上角，奇数行错开半个步长。
#[test]
fn watermark_anchor_and_stagger_unrotated() {
    let view = DecorationView {
        watermark: watermark("W", 0.0, 20.0, 1.0),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![]);
    let buf = render_all(&mut layer, 200, 200, 1.0).unwrap();
    // 锚点 (100, 100)：墨迹块 [100,112) x [100,106)
    assert_eq!(px(&buf, 200, (0, 0), 100, 100), [255, 255, 255, 255]);
    assert_eq!(px(&buf, 200, (0, 0), 111, 105), [255, 255, 255, 255]);
    assert_eq!(px(&buf, 200, (0, 0), 112, 100)[3], 0);
    assert_eq!(px(&buf, 200, (0, 0), 99, 100)[3], 0);
    // 同一行下一个单元：+step_x (32)
    assert_eq!(px(&buf, 200, (0, 0), 132, 100)[3], 255);
    // 下一行（+26）错开半步长 16
    assert_eq!(px(&buf, 200, (0, 0), 116, 126)[3], 255);
    assert_eq!(px(&buf, 200, (0, 0), 100, 126)[3], 0);
}

/// 旋转中心为渲染区中心：旋转 90 度（顺时针）后局部 (6, 3) 落在锚点左下方。
#[test]
fn watermark_rotation_about_center() {
    let view = DecorationView {
        watermark: watermark("W", 90.0, 20.0, 1.0),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![]);
    let buf = render_all(&mut layer, 200, 200, 1.0).unwrap();
    // 局部 (x, y) -> 设备 (100 - y, 100 + x)
    assert_eq!(px(&buf, 200, (0, 0), 97, 106)[3], 255, "局部 (6,3)");
    assert_eq!(
        px(&buf, 200, (0, 0), 100 - 1, 100 + 11)[3],
        255,
        "局部 (11,1)"
    );
    assert_eq!(px(&buf, 200, (0, 0), 103, 106)[3], 0, "锚点右侧不是墨迹");
}

/// 水印覆盖量：总 alpha 约等于「面积 x 墨迹占单元比例 x 有效 alpha」，旋转后守恒。
#[test]
fn watermark_total_coverage_matches_density() {
    for angle in [0.0, 30.0, -45.0] {
        let view = DecorationView {
            watermark: watermark("W", angle, 20.0, 0.5),
            ..DecorationView::default()
        };
        let mut layer = layer_with(view, vec![]);
        // 取画布中央 160x160（旋转后边角仍被平铺覆盖）
        let (w, h) = (400_u32, 400_u32);
        let buf = layer
            .render_region(&frame(w, h), 1.0, [120, 120, 280, 280], &mut fake_text)
            .unwrap();
        let sum: f64 = buf.chunks_exact(4).map(|p| f64::from(p[3]) / 255.0).sum();
        let (sx, sy) = (f64::from(INK_W) + 20.0, f64::from(INK_H) + 20.0);
        let expected = 160.0 * 160.0 * f64::from(INK_W * INK_H) / (sx * sy) * 0.5;
        let rel = (sum - expected).abs() / expected;
        assert!(
            rel < 0.05,
            "angle {angle}: 覆盖量 {sum} vs {expected}（相对误差 {rel}）"
        );
    }
}

/// 空文本、有效 alpha 低于可见下限、不透明度 0 时水印不出像素；水印叠在聚光灯之上。
#[test]
fn watermark_early_exit_and_layering() {
    for wm in [
        watermark("   ", 30.0, 20.0, 1.0),
        watermark("W", 30.0, 20.0, 0.003),
        watermark("W", 30.0, 20.0, 0.0),
    ] {
        let view = DecorationView {
            watermark: wm,
            ..DecorationView::default()
        };
        let mut layer = layer_with(view, vec![]);
        assert!(!layer.is_active());
        assert!(render_all(&mut layer, 100, 100, 1.0).is_none());
    }
    // 聚光灯洞内仍能看到水印
    let view = DecorationView {
        spotlight: spotlight(WHITE, 1.0, true),
        watermark: watermark("W", 0.0, 20.0, 1.0),
    };
    let mut layer = layer_with(view, vec![cutout(100.0, 100.0, 80.0, 80.0, 0.0)]);
    let buf = render_all(&mut layer, 200, 200, 1.0).unwrap();
    assert_eq!(
        px(&buf, 200, (0, 0), 100, 100),
        [255, 255, 255, 255],
        "洞内水印墨迹"
    );
    assert_eq!(
        px(&buf, 200, (0, 0), 100, 112)[3],
        0,
        "洞内无墨迹处保持透明"
    );
}

/// 水印单元超限：不 panic，降级为不画并标记 degraded；缩小间距后恢复。
#[test]
fn oversized_watermark_cell_degrades_without_panic() {
    let view = DecorationView {
        watermark: watermark("W", 0.0, 200.0, 1.0),
        ..DecorationView::default()
    };
    let mut layer = layer_with(view, vec![]);
    // DPR 8 时间距 1600 像素，单元边长 > 4096
    let out = layer.render_region(&frame(64, 64), 8.0, [0, 0, 64, 64], &mut fake_text);
    assert!(out.is_none());
    assert!(layer.watermark_degraded());
    assert!(layer.watermark_geometry().is_none());
}

/// 补丁协议：增量补丁需要先 reset；基线版本不一致报错；reset 后旧洞被清掉。
#[test]
fn decoration_patch_protocol() {
    let view = DecorationView::default();
    let mut layer = DecorationLayer::default();
    let incremental = DecorationPatch {
        reset: false,
        ..reset_patch(2, view, vec![])
    };
    assert_eq!(
        layer.apply_patch(&incremental),
        Err(RasterError::NotInitialized)
    );

    layer
        .apply_patch(&reset_patch(1, view, vec![cutout(1.0, 1.0, 2.0, 2.0, 0.0)]))
        .unwrap();
    assert_eq!(layer.cutouts().len(), 1);
    let stale = DecorationPatch {
        reset: false,
        base_revision: 7,
        revision: 8,
        ..reset_patch(0, view, vec![])
    };
    assert_eq!(
        layer.apply_patch(&stale),
        Err(RasterError::RevisionMismatch {
            expected: 1,
            got: 7
        })
    );
    // 引擎的 force_reset 补丁会带 delete_count = 旧洞数：reset 后应按空列表夹取，不报越界
    let mut forced = reset_patch(3, view, vec![cutout(5.0, 5.0, 2.0, 2.0, 0.0)]);
    forced.spotlight_ops[0].delete_count = 9;
    layer.apply_patch(&forced).unwrap();
    assert_eq!(layer.cutouts().len(), 1);
    // 越界的增量区间报错
    let bad = DecorationPatch {
        reset: false,
        base_revision: 3,
        revision: 4,
        spotlight_ops: vec![ReplaceRangeOp {
            start: 5,
            delete_count: 0,
            insert_items: vec![],
        }],
        ..reset_patch(0, view, vec![])
    };
    assert_eq!(layer.apply_patch(&bad), Err(RasterError::InvalidOp));
}

/// 光栅化器消费装饰补丁：整屏时全部块进入 `touched_tiles`，增量时只有脏区的块；矢量画布不被改写。
#[test]
fn rasterizer_reports_decoration_dirty_tiles() {
    let view = DecorationView {
        spotlight: spotlight(WHITE, 0.5, true),
        ..DecorationView::default()
    };
    let (w, h) = (256_u32, 256_u32);
    let mut full = ViewportPatch {
        frame_view: frame(w, h),
        ..ViewportPatch::default()
    };
    full.decoration = reset_patch(1, view, vec![cutout(128.0, 128.0, 40.0, 40.0, 0.0)]);
    let mut raster = TinySkiaRasterizer::new(RasterConfig {
        tile_size: 64,
        device_pixel_ratio: 1.0,
    });
    let out = raster.apply_patch(&full).unwrap();
    assert!(out.full_redraw);
    assert_eq!(out.touched_tiles.len(), 16);
    assert!(out.tiles.is_empty(), "矢量层没有内容，装饰不进矢量块");
    assert!(raster.decoration().is_active());
    let deco = raster
        .render_decoration([0, 0, 64, 64], &mut fake_text)
        .unwrap();
    assert_eq!(&deco[..4], &[128, 128, 128, 128]);

    // 增量：只动一个脏区
    let mut inc = ViewportPatch {
        frame_view: frame(w, h),
        ..ViewportPatch::default()
    };
    inc.scene = snow_draw_engine_display::LayerPatch {
        base_revision: full.scene.revision,
        revision: full.scene.revision,
        reset: false,
        ops: vec![],
        dirty_regions: vec![],
    };
    inc.decoration = DecorationPatch {
        base_revision: 1,
        revision: 2,
        reset: false,
        view,
        spotlight_ops: vec![ReplaceRangeOp {
            start: 0,
            delete_count: 1,
            insert_items: vec![cutout(40.0, 40.0, 20.0, 20.0, 0.0)],
        }],
        dirty_regions: vec![
            DirtyRegion::new(100.0, 100.0, 150.0, 150.0),
            DirtyRegion::new(30.0, 30.0, 50.0, 50.0),
        ],
    };
    let out = raster.apply_patch(&inc).unwrap();
    assert!(!out.full_redraw);
    assert_eq!(
        out.touched_tiles.len(),
        5,
        "脏区 (100..150)^2 触及 4 块，(30..50)^2 触及 1 块"
    );
    // 版本不匹配：作废状态，装饰层也一起清空
    inc.decoration.base_revision = 99;
    assert!(raster.apply_patch(&inc).is_err());
    assert!(!raster.decoration().is_active());
}

/// 取 `ColorRgba8` 的预乘外字节数组（测试辅助，不透明颜色即原值）。
trait IntoArray {
    /// 转成 `[r, g, b, a]`。
    fn into_array(self) -> [u8; 4];
}

impl IntoArray for ColorRgba8 {
    fn into_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}
