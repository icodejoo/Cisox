//! 马赛克、浮雕、灰度/反相与源图混合：直译自 C++ `snow_canvas_filter_render.cpp` 的标量内核。
//!
//! 行并行在 Rust 侧串行执行（行间相互独立，结果与并行一致）。源与目标必须是不同缓冲。

// 直译 C++ 下标循环，保持与原文逐行对应
#![allow(clippy::needless_range_loop)]

use crate::avx2::{
    grayscale_avx2, grayscale_masked_avx2, grayscale_rect_avx2, invert_avx2, invert_masked_avx2,
    invert_rect_avx2, is_avx2_available,
};
use crate::image::{AlphaRef, ImageMut, ImageRef, Rect};
use crate::params::{FILTER_GRAYSCALE, Parameters};
use crate::pixel::{
    blend_premultiplied, combine_coverage, normalized_strength, q_alpha, q_blue, q_green, q_red,
    q_rgba, q_round,
};
use std::cmp::{max, min};

/// 浮雕强度缩放系数。
const EMBOSS_STRENGTH_SCALE: f64 = 10.0;

/// 马赛克网格布局。
struct MosaicGrid {
    block: i32,
    first_x: i32,
    first_y: i32,
    column_count: i32,
    row_count: i32,
}

/// 计算马赛克网格（块大小、首列/首行偏移、列数/行数）。
fn mosaic_grid(width: i32, height: i32, parameters: &Parameters) -> MosaicGrid {
    let block = max(
        1,
        q_round(parameters.logical_block_size * parameters.device_pixel_ratio),
    );
    let origin_x = q_round(parameters.grid_origin_x);
    let origin_y = q_round(parameters.grid_origin_y);
    let first_x = origin_x + ((-origin_x) as f64 / block as f64).floor() as i32 * block;
    let first_y = origin_y + ((-origin_y) as f64 / block as f64).floor() as i32 * block;
    let column_count = (width - first_x + block - 1) / block;
    let row_count = (height - first_y + block - 1) / block;
    MosaicGrid {
        block,
        first_x,
        first_y,
        column_count,
        row_count,
    }
}

/// 采集每个马赛克块中心的样本像素（坐标钳位到图像内）。
fn collect_mosaic_samples(
    source: ImageRef,
    grid: &MosaicGrid,
    first_column: i32,
    column_count: i32,
    first_row: i32,
    row_count: i32,
    samples: &mut [u32],
) {
    for local_row in 0..row_count {
        let row = first_row + local_row;
        let sample_y =
            (grid.first_y + row * grid.block + grid.block / 2).clamp(0, source.height - 1);
        let sample_line = &source.data[(sample_y as usize * source.stride)..];
        for local_column in 0..column_count {
            let column = first_column + local_column;
            let sample_x =
                (grid.first_x + column * grid.block + grid.block / 2).clamp(0, source.width - 1);
            samples[(local_row as usize) * (column_count as usize) + (local_column as usize)] =
                sample_line[sample_x as usize];
        }
    }
}

/// 灰度/反相的单像素变换（其余类型原样返回）。
#[inline]
fn transformed_color(pixel: u32, filter_type: u32) -> u32 {
    let alpha = q_alpha(pixel);
    if filter_type == 2 {
        let luminance = min(
            alpha,
            (q_red(pixel) * 54 + q_green(pixel) * 183 + q_blue(pixel) * 19 + 128) >> 8,
        );
        return q_rgba(luminance, luminance, luminance, alpha);
    }
    if filter_type == 3 {
        return q_rgba(
            alpha - q_red(pixel),
            alpha - q_green(pixel),
            alpha - q_blue(pixel),
            alpha,
        );
    }
    pixel
}

/// 浮雕采样半径（像素）。
///
/// 参数：`parameters` 滤镜参数。返回：`max(1, ceil(max(0, r) * max(1, dpr)))`。
///
/// 示例：`let r = emboss_sampling_radius(&params);`
pub fn emboss_sampling_radius(parameters: &Parameters) -> i32 {
    max(
        1,
        (parameters.logical_sampling_radius.max(0.0) * parameters.device_pixel_ratio.max(1.0))
            .ceil() as i32,
    )
}

/// 浮雕响应查表：RGB 和差（-765..=765）到灰度系数。
struct EmbossResponse {
    gray: [f64; 1531],
}

impl EmbossResponse {
    /// 按强度构建查表，浮点运算顺序与着色器一致。
    fn new(strength: f64) -> Self {
        let mut gray = [0.0; 1531];
        for delta in -765..=765 {
            gray[(delta + 765) as usize] =
                (0.5 + strength * delta as f64 / (3.0 * 255.0)).clamp(0.0, 1.0);
        }
        Self { gray }
    }

    /// 由负/正方向采样像素与中心 alpha 计算浮雕像素。
    fn pixel(&self, negative: u32, positive: u32, alpha: i32) -> u32 {
        let delta = q_red(positive) + q_green(positive) + q_blue(positive)
            - q_red(negative)
            - q_green(negative)
            - q_blue(negative);
        let gray = q_round(self.gray[(delta + 765) as usize] * alpha as f64);
        q_rgba(gray, gray, gray, alpha)
    }
}

/// 浮雕单行采样上下文。
struct EmbossRow<'a> {
    negative: &'a [u32],
    positive: &'a [u32],
    center: &'a [u32],
    last_x: i32,
    radius: i32,
}

impl<'a> EmbossRow<'a> {
    /// 取第 `y` 行及其上下 `sampling_radius` 行（钳位）。
    fn new(source: ImageRef<'a>, y: i32, sampling_radius: i32) -> Self {
        let data = source.data;
        let stride = source.stride;
        let line = |row: i32| -> &'a [u32] { &data[(row as usize * stride)..] };
        Self {
            negative: line(max(0, y - sampling_radius)),
            positive: line(min(source.height - 1, y + sampling_radius)),
            center: line(y),
            last_x: source.width - 1,
            radius: sampling_radius,
        }
    }

    /// 计算第 `x` 列的浮雕像素（左右采样钳位）。
    fn pixel(&self, x: i32, response: &EmbossResponse) -> u32 {
        response.pixel(
            self.negative[max(0, x - self.radius) as usize],
            self.positive[min(self.last_x, x + self.radius) as usize],
            q_alpha(self.center[x as usize]),
        )
    }
}

/// 浮雕（矩形，常量混合权重）。
///
/// 参数：`source` 源图；`destination` 目标图（不得与源同一缓冲）；`pixels` 处理矩形；
/// `parameters` 参数；`constant_mix` 0..=255，`>=255` 直接覆盖。
///
/// 示例：`emboss_rect(src, &mut dst, Rect::new(0, 0, 64, 64), &params, 255);`
pub fn emboss_rect(
    source: ImageRef,
    destination: &mut ImageMut,
    pixels: Rect,
    parameters: &Parameters,
    constant_mix: i32,
) {
    if pixels.is_empty() || constant_mix <= 0 {
        return;
    }
    let sampling_radius = emboss_sampling_radius(parameters);
    let response =
        EmbossResponse::new(normalized_strength(parameters.strength) * EMBOSS_STRENGTH_SCALE);
    for local_y in 0..pixels.h {
        let y = pixels.top() + local_y;
        let row = EmbossRow::new(source, y, sampling_radius);
        let stride = destination.stride;
        let line = &mut destination.data[(y as usize * stride)..];
        for x in pixels.left()..=pixels.right() {
            let embossed = row.pixel(x, &response);
            line[x as usize] = if constant_mix >= 255 {
                embossed
            } else {
                blend_premultiplied(line[x as usize], embossed, constant_mix)
            };
        }
    }
}

/// 浮雕（矩形，逐像素遮罩权重）。
///
/// 参数：`mask` 及其原点 `(mask_origin_x, mask_origin_y)`（遮罩须覆盖 `pixels`），其余同
/// [`emboss_rect`]。
///
/// 示例：`emboss_masked_rect(src, &mut dst, mask, 0, 0, Rect::new(0, 0, 64, 64), &params);`
pub fn emboss_masked_rect(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    pixels: Rect,
    parameters: &Parameters,
) {
    if pixels.is_empty() {
        return;
    }
    let sampling_radius = emboss_sampling_radius(parameters);
    let response =
        EmbossResponse::new(normalized_strength(parameters.strength) * EMBOSS_STRENGTH_SCALE);
    for local_y in 0..pixels.h {
        let y = pixels.top() + local_y;
        let row = EmbossRow::new(source, y, sampling_radius);
        let stride = destination.stride;
        let line = &mut destination.data[(y as usize * stride)..];
        let alpha_line = &mask.data[((y - mask_origin_y) as usize * mask.stride)..];
        for x in pixels.left()..=pixels.right() {
            let mix = alpha_line[(x - mask_origin_x) as usize] as i32;
            if mix == 0 {
                continue;
            }
            let embossed = row.pixel(x, &response);
            line[x as usize] = if mix == 255 {
                embossed
            } else {
                blend_premultiplied(line[x as usize], embossed, mix)
            };
        }
    }
}

/// 整图马赛克（原地）。
///
/// 参数：`image` 待处理图像（须非空）；`parameters` 取块大小、设备像素比与网格原点。
///
/// 示例：`mosaic(&mut img.as_mut(), &params);`
pub fn mosaic(image: &mut ImageMut, parameters: &Parameters) {
    let grid = mosaic_grid(image.width, image.height, parameters);
    let mut samples = vec![0u32; (grid.column_count * grid.row_count) as usize];
    let source_ref = ImageRef {
        data: &*image.data,
        width: image.width,
        height: image.height,
        stride: image.stride,
    };
    collect_mosaic_samples(
        source_ref,
        &grid,
        0,
        grid.column_count,
        0,
        grid.row_count,
        &mut samples,
    );
    let (height, width, stride) = (image.height, image.width, image.stride);
    for py in 0..height {
        let line = &mut image.data[(py as usize * stride)..];
        let row = (py - grid.first_y) / grid.block;
        let sample_offset = (row as usize) * (grid.column_count as usize);
        for column in 0..grid.column_count {
            let left = max(0, grid.first_x + column * grid.block);
            let right = min(width, grid.first_x + (column + 1) * grid.block);
            if left < right {
                line[(left as usize)..(right as usize)]
                    .fill(samples[sample_offset + (column as usize)]);
            }
        }
    }
}

/// 马赛克（矩形 + 遮罩），对应 C++ `applyMasked` 的 type 0 分支。
///
/// 参数：`pixels` 须已与目标求交、非空且被遮罩覆盖；`mask_origin_*` 为遮罩原点。
///
/// 示例：`mosaic_masked(src, &mut dst, mask, 0, 0, Rect::new(0, 0, 64, 64), &params);`
pub fn mosaic_masked(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    pixels: Rect,
    parameters: &Parameters,
) {
    if pixels.is_empty() {
        return;
    }
    let grid = mosaic_grid(source.width, source.height, parameters);
    let first_column = (pixels.left() - grid.first_x) / grid.block;
    let last_column = (pixels.right() - grid.first_x) / grid.block;
    let first_row = (pixels.top() - grid.first_y) / grid.block;
    let last_row = (pixels.bottom() - grid.first_y) / grid.block;
    let sample_column_count = last_column - first_column + 1;
    let sample_row_count = last_row - first_row + 1;
    let mut samples = vec![0u32; (sample_column_count * sample_row_count) as usize];
    collect_mosaic_samples(
        source,
        &grid,
        first_column,
        sample_column_count,
        first_row,
        sample_row_count,
        &mut samples,
    );
    for local_y in 0..pixels.h {
        let y = pixels.top() + local_y;
        let line = &mut destination.data[(y as usize * destination.stride)..];
        let alpha_line = &mask.data[((y - mask_origin_y) as usize * mask.stride)..];
        let sample_row = (y - grid.first_y) / grid.block - first_row;
        let sample_offset = (sample_row as usize) * (sample_column_count as usize);
        let mut column = first_column;
        let mut x = pixels.left();
        while x <= pixels.right() {
            let span_end = min(pixels.right() + 1, grid.first_x + (column + 1) * grid.block);
            let sample = samples[sample_offset + (column - first_column) as usize];
            while x < span_end {
                let mask_x = (x - mask_origin_x) as usize;
                if alpha_line[mask_x] == 0 {
                    x += 1;
                    continue;
                }
                if alpha_line[mask_x] == 255 {
                    let opaque_begin = x;
                    loop {
                        x += 1;
                        if x >= span_end || alpha_line[(x - mask_origin_x) as usize] != 255 {
                            break;
                        }
                    }
                    line[(opaque_begin as usize)..(x as usize)].fill(sample);
                    continue;
                }
                line[x as usize] =
                    blend_premultiplied(line[x as usize], sample, alpha_line[mask_x] as i32);
                x += 1;
            }
            column += 1;
        }
    }
}

/// 整图灰度/反相（原地）。
///
/// 参数：`filter_type` 为 2 灰度、其余按反相；`mix` 0..=255；`force_scalar` 禁用 AVX2。
/// 返回：是否使用了 SIMD。
///
/// 示例：`let simd = color_effect(&mut img.as_mut(), FILTER_GRAYSCALE, 255, false);`
pub fn color_effect(image: &mut ImageMut, filter_type: u32, mix: i32, force_scalar: bool) -> bool {
    if mix <= 0 {
        return false;
    }
    let height = image.height;
    if !force_scalar && is_avx2_available() {
        let executed = if filter_type == FILTER_GRAYSCALE {
            grayscale_avx2(image, 0, height, mix)
        } else {
            invert_avx2(image, 0, height, mix)
        };
        if executed {
            return true;
        }
    }
    let (width, stride) = (image.width, image.stride);
    for y in 0..height {
        let line = &mut image.data[(y as usize * stride)..];
        for x in 0..width as usize {
            let pixel = line[x];
            let transformed = transformed_color(pixel, filter_type);
            line[x] = if mix == 255 {
                transformed
            } else {
                blend_premultiplied(pixel, transformed, mix)
            };
        }
    }
    false
}

/// 矩形灰度/反相（源 -> 目标，按常量权重混入目标）。
///
/// 返回：是否使用了 SIMD。参数含义同 [`color_effect`]，`pixels` 为处理矩形。
///
/// 示例：`color_effect_rect(src, &mut dst, Rect::new(0, 0, 64, 64), 3, 255, false);`
pub fn color_effect_rect(
    source: ImageRef,
    destination: &mut ImageMut,
    pixels: Rect,
    filter_type: u32,
    mix: i32,
    force_scalar: bool,
) -> bool {
    if pixels.is_empty() || mix <= 0 {
        return false;
    }
    if !force_scalar && is_avx2_available() {
        let (l, t, r, b) = (
            pixels.left(),
            pixels.top(),
            pixels.right() + 1,
            pixels.bottom() + 1,
        );
        let executed = if filter_type == FILTER_GRAYSCALE {
            grayscale_rect_avx2(source, destination, l, t, r, b, mix)
        } else {
            invert_rect_avx2(source, destination, l, t, r, b, mix)
        };
        if executed {
            return true;
        }
    }
    for local_y in 0..pixels.h {
        let y = pixels.top() + local_y;
        let source_line = &source.data[(y as usize * source.stride)..];
        let line = &mut destination.data[(y as usize * destination.stride)..];
        for x in pixels.left()..=pixels.right() {
            let transformed = transformed_color(source_line[x as usize], filter_type);
            line[x as usize] = if mix == 255 {
                transformed
            } else {
                blend_premultiplied(line[x as usize], transformed, mix)
            };
        }
    }
    false
}

/// 遮罩灰度/反相，对应 C++ `applyMasked` 中的颜色效果分支。
///
/// 参数：`strength_mix` 为强度权重（0..=255）；`pixels` 须被遮罩覆盖。返回：是否使用了 SIMD。
///
/// 示例：`color_masked(src, &mut dst, mask, 0, 0, Rect::new(0, 0, 64, 64), 2, 255, false);`
#[allow(clippy::too_many_arguments)]
pub fn color_masked(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    pixels: Rect,
    filter_type: u32,
    strength_mix: i32,
    force_scalar: bool,
) -> bool {
    if pixels.is_empty() {
        return false;
    }
    if !force_scalar && is_avx2_available() {
        let (l, t, r, b) = (
            pixels.left(),
            pixels.top(),
            pixels.right() + 1,
            pixels.bottom() + 1,
        );
        let executed = if filter_type == FILTER_GRAYSCALE {
            grayscale_masked_avx2(
                source,
                destination,
                mask,
                mask_origin_x,
                mask_origin_y,
                l,
                t,
                r,
                b,
                strength_mix,
            )
        } else {
            invert_masked_avx2(
                source,
                destination,
                mask,
                mask_origin_x,
                mask_origin_y,
                l,
                t,
                r,
                b,
                strength_mix,
            )
        };
        if executed {
            return true;
        }
    }
    for local_y in 0..pixels.h {
        let y = pixels.top() + local_y;
        let source_line = &source.data[(y as usize * source.stride)..];
        let line = &mut destination.data[(y as usize * destination.stride)..];
        let alpha_line = &mask.data[((y - mask_origin_y) as usize * mask.stride)..];
        for x in pixels.left()..=pixels.right() {
            let mix = combine_coverage(
                alpha_line[(x - mask_origin_x) as usize] as i32,
                strength_mix,
            );
            if mix == 0 {
                continue;
            }
            let to = transformed_color(source_line[x as usize], filter_type);
            line[x as usize] = if mix == 255 {
                to
            } else {
                blend_premultiplied(line[x as usize], to, mix)
            };
        }
    }
    false
}

/// 将已滤镜图与源图按不透明度插值（结果各通道钳到 alpha）。
///
/// 参数：`filtered` 原地更新；`source` 未滤镜源图（尺寸须与 `filtered` 相同）；
/// `opacity` 0..=1。
///
/// 示例：`blend_over_source(&mut filtered.as_mut(), src, 0.5);`
pub fn blend_over_source(filtered: &mut ImageMut, source: ImageRef, opacity: f64) {
    let mix = q_round(opacity * 256.0).clamp(0, 256);
    let interpolate = |first: i32, second: i32| -> i32 {
        let scaled = (second - first) * mix;
        first + (scaled + if scaled >= 0 { 128 } else { -128 }) / 256
    };
    let (height, width) = (filtered.height, filtered.width);
    for y in 0..height {
        let destination = &mut filtered.data[(y as usize * filtered.stride)..];
        let base = &source.data[(y as usize * source.stride)..];
        for x in 0..width as usize {
            let from = base[x];
            let to = destination[x];
            let alpha = interpolate(q_alpha(from), q_alpha(to));
            destination[x] = q_rgba(
                min(alpha, interpolate(q_red(from), q_red(to))),
                min(alpha, interpolate(q_green(from), q_green(to))),
                min(alpha, interpolate(q_blue(from), q_blue(to))),
                alpha,
            );
        }
    }
}
