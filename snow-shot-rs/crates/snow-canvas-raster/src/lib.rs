//! 标注画布光栅化器（`ViewportPatch` 消费者）。
//!
//! 所属阶段：P2。消费引擎的 `ViewportPatch` / `DirtyRegion` 协议，用 tiny-skia 在 CPU 上
//! 只重绘脏区，输出脏块的预乘 RGBA 像素；不依赖 gpui，纹理上传由 `snow-ui-shell` 负责。
//!
//! # 用法
//! ```
//! use snow_canvas_raster::{CanvasRasterizer, RasterConfig, TinySkiaRasterizer};
//! use snow_draw_engine_display::ViewportPatch;
//!
//! let mut raster = TinySkiaRasterizer::new(RasterConfig::default());
//! // 引擎侧：patch = engine.acquire_patch(viewport, raster.cursor())
//! let out = raster.apply_patch(&ViewportPatch::default()).unwrap();
//! for tile in &out.tiles { /* 上传 tile.rgba 到 (tile.x, tile.y) */ }
//! for key in &out.released { /* 释放旧纹理 */ }
//! ```
//!
//! # 关键约定（P0-V2 spike 结论）
//! - 分块 256px，只处理脏区；`cursor()` 必须回传给引擎才能得到真增量。
//! - 脏区蒙版常驻全 0、每帧只处理脏区行。
//! - 被替换/清空的旧块由上层显式释放，否则泄漏图集。

pub mod decoration;
mod draw;
mod rasterizer;
pub mod region;
mod types;

pub use rasterizer::{CanvasRasterizer, TinySkiaRasterizer};
pub use types::{
    DEFAULT_TILE_SIZE, DeferredItem, DeferredKind, MAX_DEVICE_PIXEL_RATIO, MAX_SURFACE_PIXELS,
    MAX_TILE_SIZE, MIN_DEVICE_PIXEL_RATIO, MIN_TILE_SIZE, RasterConfig, RasterError, RasterOutput,
    RasterTile, TileKey,
};

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P2";
