//! 光栅化集成测试共用夹具：帧视图、64px 分块光栅化器、按偏移取像素。
#![allow(dead_code)]

use snow_canvas_raster::{RasterConfig, TinySkiaRasterizer};
use snow_draw_engine_core::{Camera, ColorRgba8, Point, SurfaceSize};
use snow_draw_engine_display::FrameView;

/// 构造帧视图：相机居中于 surface 中心时，画布坐标等于视图坐标。
pub fn frame(w: u32, h: u32, zoom: f64, center: (f64, f64)) -> FrameView {
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
pub fn identity_frame(w: u32, h: u32) -> FrameView {
    frame(w, h, 1.0, (f64::from(w) / 2.0, f64::from(h) / 2.0))
}

/// 用 64px 分块、1x 的光栅化器。
pub fn raster64() -> TinySkiaRasterizer {
    TinySkiaRasterizer::new(RasterConfig {
        tile_size: 64,
        device_pixel_ratio: 1.0,
    })
}

/// 取宽为 `w` 的 RGBA 缓冲里 `(x, y)` 像素。
pub fn rgba_at(buf: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
    let o = (y * w + x) * 4;
    [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
}
