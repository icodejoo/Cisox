//! 高层入口：按滤镜类型分派到各内核，语义对应 C++ `apply / applyMasked / applyRect / applyRegion`。
//!
//! 源与目标须是不同缓冲（C++ 中源目标同址时由调用方先拷贝）。

use crate::blur::{blur, blur_masked, make_gaussian_blur_plan};
use crate::effects::{
    color_effect, color_effect_rect, color_masked, emboss_masked_rect, emboss_rect,
    emboss_sampling_radius, mosaic, mosaic_masked,
};
use crate::image::{AlphaRef, ImageMut, ImageRef, Rect};
use crate::params::{
    ExecutionOptions, FILTER_BLUR, FILTER_EMBOSS, FILTER_GRAYSCALE, FILTER_INVERT, FILTER_MOSAIC,
    Parameters,
};
use crate::pixel::{combine_coverage, normalized_strength_mix, q_round};

/// SIMD 后端选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdBackend {
    /// 纯标量。
    Scalar,
    /// x86_64 AVX2。
    Avx2,
}

/// 返回当前运行环境选用的 SIMD 后端（运行时检测）。
///
/// 示例：`let backend = selected_simd_backend();`
pub fn selected_simd_backend() -> SimdBackend {
    if crate::avx2::is_avx2_available() {
        SimdBackend::Avx2
    } else {
        SimdBackend::Scalar
    }
}

/// 滤镜需要额外读取的源像素半径（物理像素）。
///
/// 示例：`let r = sampling_radius_pixels(&params);`
pub fn sampling_radius_pixels(parameters: &Parameters) -> i32 {
    if parameters.filter_type == FILTER_BLUR {
        return make_gaussian_blur_plan(parameters).physical_support_radius;
    }
    if parameters.filter_type == FILTER_EMBOSS {
        return emboss_sampling_radius(parameters);
    }
    0.max((parameters.logical_sampling_radius * parameters.device_pixel_ratio).ceil() as i32)
}

/// 整图原地应用滤镜。
///
/// 参数：`image` 待处理图像（ARGB32 预乘，非空）；`parameters` 滤镜参数；`options` 执行选项。
///
/// 示例：`apply(&mut img.as_mut(), &params, &ExecutionOptions::default());`
pub fn apply(image: &mut ImageMut, parameters: &Parameters, options: &ExecutionOptions) {
    let t = parameters.filter_type;
    if (t == FILTER_GRAYSCALE || t == FILTER_INVERT)
        && normalized_strength_mix(parameters.strength) == 0
    {
        return;
    }
    match t {
        FILTER_MOSAIC => mosaic(image, parameters),
        FILTER_BLUR => {
            blur(image, parameters, options.force_scalar);
        }
        FILTER_GRAYSCALE | FILTER_INVERT => {
            color_effect(
                image,
                t,
                normalized_strength_mix(parameters.strength),
                options.force_scalar,
            );
        }
        FILTER_EMBOSS => {
            // 浮雕需读取邻域，先拷贝出独立的源缓冲
            let copy = image.data.to_vec();
            let source = ImageRef {
                data: &copy,
                width: image.width,
                height: image.height,
                stride: image.stride,
            };
            let rect = Rect::new(0, 0, image.width, image.height);
            emboss_rect(source, image, rect, parameters, 255);
        }
        _ => {}
    }
}

/// 源与目标尺寸是否一致。
fn same_size(source: &ImageRef, destination: &ImageMut) -> bool {
    source.width == destination.width && source.height == destination.height
}

/// 遮罩合成：把 `source` 按遮罩权重应用滤镜写入 `destination`。
///
/// 参数：`mask` 为 8 位遮罩，原点 `(mask_origin_x, mask_origin_y)`，须覆盖有效矩形；
/// `destination_pixels` 为目标矩形（会与目标求交）。
/// 返回：尺寸不符或遮罩未覆盖时为 `false`。
///
/// 示例：`apply_masked(src, &mut dst, mask, 0, 0, Rect::new(0, 0, 64, 64), &params, &opts);`
#[allow(clippy::too_many_arguments)]
pub fn apply_masked(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    destination_pixels: Rect,
    parameters: &Parameters,
    options: &ExecutionOptions,
) -> bool {
    if !same_size(&source, destination) {
        return false;
    }
    let pixels =
        destination_pixels.intersected(&Rect::new(0, 0, destination.width, destination.height));
    if pixels.is_empty() {
        return true;
    }
    if !Rect::new(mask_origin_x, mask_origin_y, mask.width, mask.height).contains(&pixels) {
        return false;
    }
    let t = parameters.filter_type;
    let color_effect_type = t == FILTER_GRAYSCALE || t == FILTER_INVERT;
    let strength_mix = if color_effect_type {
        normalized_strength_mix(parameters.strength)
    } else {
        255
    };
    if color_effect_type && strength_mix == 0 {
        return true;
    }
    match t {
        FILTER_BLUR => blur_masked(
            source,
            destination,
            Some(mask),
            mask_origin_x,
            mask_origin_y,
            &[pixels],
            -1,
            parameters,
            options.force_scalar,
        ),
        FILTER_EMBOSS => {
            emboss_masked_rect(
                source,
                destination,
                mask,
                mask_origin_x,
                mask_origin_y,
                pixels,
                parameters,
            );
            true
        }
        FILTER_MOSAIC => {
            mosaic_masked(
                source,
                destination,
                mask,
                mask_origin_x,
                mask_origin_y,
                pixels,
                parameters,
            );
            true
        }
        _ => {
            color_masked(
                source,
                destination,
                mask,
                mask_origin_x,
                mask_origin_y,
                pixels,
                t,
                strength_mix,
                options.force_scalar,
            );
            true
        }
    }
}

/// 矩形应用（模糊/灰度/反相/浮雕），按不透明度混合。
///
/// 参数：`opacity` 0..=1；`destination_pixels` 会与目标求交。马赛克不支持，返回 `false`。
/// 返回：类型不支持或尺寸不符为 `false`。
///
/// 示例：`apply_rect(src, &mut dst, Rect::new(0, 0, 64, 64), 1.0, &params, &opts);`
pub fn apply_rect(
    source: ImageRef,
    destination: &mut ImageMut,
    destination_pixels: Rect,
    opacity: f64,
    parameters: &Parameters,
    options: &ExecutionOptions,
) -> bool {
    let t = parameters.filter_type;
    let supported = matches!(
        t,
        FILTER_BLUR | FILTER_GRAYSCALE | FILTER_INVERT | FILTER_EMBOSS
    );
    if !supported || !same_size(&source, destination) {
        return false;
    }
    let pixels =
        destination_pixels.intersected(&Rect::new(0, 0, destination.width, destination.height));
    let mut mix = q_round(opacity * 255.0).clamp(0, 255);
    if t == FILTER_GRAYSCALE || t == FILTER_INVERT {
        mix = combine_coverage(mix, normalized_strength_mix(parameters.strength));
    }
    if pixels.is_empty() || mix == 0 {
        return true;
    }
    match t {
        FILTER_BLUR => blur_masked(
            source,
            destination,
            None,
            0,
            0,
            &[pixels],
            mix,
            parameters,
            options.force_scalar,
        ),
        FILTER_EMBOSS => {
            emboss_rect(source, destination, pixels, parameters, mix);
            true
        }
        _ => {
            color_effect_rect(source, destination, pixels, t, mix, options.force_scalar);
            true
        }
    }
}

/// 区域应用（模糊/灰度/反相/浮雕），权重恒为 255（颜色效果为强度权重）。
///
/// 参数：`destination_region` 为互不重叠矩形集合（逐个与目标求交，空矩形丢弃）。
/// 返回：类型不支持或尺寸不符为 `false`。
///
/// 示例：`apply_region(src, &mut dst, &[Rect::new(0, 0, 8, 8)], &params, &opts);`
pub fn apply_region(
    source: ImageRef,
    destination: &mut ImageMut,
    destination_region: &[Rect],
    parameters: &Parameters,
    options: &ExecutionOptions,
) -> bool {
    let t = parameters.filter_type;
    let supported = matches!(
        t,
        FILTER_BLUR | FILTER_GRAYSCALE | FILTER_INVERT | FILTER_EMBOSS
    );
    if !supported || !same_size(&source, destination) {
        return false;
    }
    let bounds = Rect::new(0, 0, destination.width, destination.height);
    let pixels: Vec<Rect> = destination_region
        .iter()
        .map(|r| r.intersected(&bounds))
        .filter(|r| !r.is_empty())
        .collect();
    if pixels.is_empty() {
        return true;
    }
    let color_mix = if t == FILTER_GRAYSCALE || t == FILTER_INVERT {
        normalized_strength_mix(parameters.strength)
    } else {
        255
    };
    if color_mix == 0 {
        return true;
    }
    match t {
        FILTER_BLUR => blur_masked(
            source,
            destination,
            None,
            0,
            0,
            &pixels,
            255,
            parameters,
            options.force_scalar,
        ),
        FILTER_EMBOSS => {
            for rect in &pixels {
                emboss_rect(source, destination, *rect, parameters, 255);
            }
            true
        }
        _ => {
            for rect in &pixels {
                color_effect_rect(
                    source,
                    destination,
                    *rect,
                    t,
                    color_mix,
                    options.force_scalar,
                );
            }
            true
        }
    }
}
