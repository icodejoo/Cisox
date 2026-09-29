//! 画布滤镜内核（马赛克/模糊/浮雕/灰度/反相/画笔遮罩）。
//!
//! 直译自 C++ `snow_canvas_filter_avx2.cpp` / `snow_canvas_filter_render.cpp` /
//! `snow_canvas_pen_mask_avx2.cpp`（ADR-10）：x86_64 上运行时检测 AVX2，其余走标量回退；
//! 标量与 AVX2 路径均以 C++ 黄金样本逐字节对拍。像素格式为 ARGB32 预乘（`u32` = 0xAARRGGBB）。
//!
//! 智能擦除依赖 OpenCV 与 Qt，此处仅保留接口说明，见 [`smart_erase`]。
//!
//! # 示例
//! ```
//! use snow_canvas_filters::{apply, ExecutionOptions, OwnedImage, Parameters, FILTER_INVERT};
//! let mut img = OwnedImage::new(16, 4);
//! img.data.fill(0xff10_2030);
//! let params = Parameters { filter_type: FILTER_INVERT, ..Parameters::default() };
//! apply(&mut img.as_mut(), &params, &ExecutionOptions::default());
//! assert_eq!(img.data[0], 0xffef_dfcf);
//! ```

pub mod apply;
pub mod avx2;
pub mod blur;
pub mod effects;
pub mod image;
pub mod params;
pub mod pen_mask;
pub mod pixel;
pub mod smart_erase;
mod upsample;

pub use apply::{
    SimdBackend, apply, apply_masked, apply_rect, apply_region, sampling_radius_pixels,
    selected_simd_backend,
};
pub use effects::blend_over_source;
pub use image::{AlphaRef, ImageMut, ImageRef, OwnedImage, Rect};
pub use params::{
    ExecutionOptions, FILTER_BLUR, FILTER_EMBOSS, FILTER_GRAYSCALE, FILTER_INVERT, FILTER_MOSAIC,
    GaussianBlurPlan, Parameters,
};

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P2";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }
}
