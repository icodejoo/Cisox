//! 双线性上采样（含遮罩/常量权重合成）：直译自 C++ `snow_canvas_filter_render.cpp`。
//!
//! 行并行在 Rust 侧串行；C++ 的模板参数 `Factor` 改为运行时参数，且保留“非 2/4/8/16/32 一律按 64”的语义。

// 直译 C++ 下标循环，保持与原文逐行对应
#![allow(clippy::needless_range_loop)]

use crate::avx2::{interpolate_and_blend_constant_avx2, interpolate_and_blend_masked_avx2};
use crate::image::{AlphaRef, ImageMut, ImageRef, Rect};
use crate::pixel::blend_premultiplied;

/// 轴向采样参数。
#[derive(Clone, Copy, Default)]
struct AxisSample {
    /// 第一个参与插值的源索引。
    first: i32,
    /// 第二个参与插值的源索引。
    second: i32,
    /// 插值权重（0..=256）。
    weight: i32,
}

/// 计算单个目标坐标的轴向采样（要求 `extent > 0`）。
fn axis_sample(coordinate: i32, extent: i32, factor: i32) -> AxisSample {
    let denominator = factor * 2;
    let numerator = coordinate * 2 + 1 - factor;
    let base = if numerator >= 0 {
        numerator / denominator
    } else {
        -((-numerator + denominator - 1) / denominator)
    };
    let remainder = numerator - base * denominator;
    AxisSample {
        first: base.clamp(0, extent - 1),
        second: (base + 1).clamp(0, extent - 1),
        weight: (remainder * 256 + factor) / denominator,
    }
}

/// 两像素按权重（0..=256）插值，双通道并行。
fn interpolate_pixel(first: u32, second: u32, weight: i32) -> u32 {
    if weight <= 0 || first == second {
        return first;
    }
    if weight >= 256 {
        return second;
    }
    let lanes = 0x00ff_00ffu32;
    let rounding = 0x0080_0080u32;
    let inverse = (256 - weight) as u32;
    let mix = weight as u32;
    let red_blue = (((first & lanes)
        .wrapping_mul(inverse)
        .wrapping_add((second & lanes).wrapping_mul(mix))
        .wrapping_add(rounding))
        >> 8)
        & lanes;
    let alpha_green = ((((first >> 8) & lanes)
        .wrapping_mul(inverse)
        .wrapping_add(((second >> 8) & lanes).wrapping_mul(mix))
        .wrapping_add(rounding))
        >> 8)
        & lanes;
    red_blue | (alpha_green << 8)
}

/// 预计算一个轴向上 `count` 个坐标的采样参数。
fn prepare_axis_samples(
    first_coordinate: i32,
    count: i32,
    extent: i32,
    factor: i32,
) -> Vec<AxisSample> {
    (0..count)
        .map(|index| axis_sample(first_coordinate + index, extent, factor))
        .collect()
}

/// 把 C++ switch 的因子分发映射为实际使用的 Factor：非 2/4/8/16/32（及允许时的 1）一律为 64。
fn normalize_factor(factor: i32, allow_one: bool) -> i32 {
    if factor == 1 && allow_one {
        return 1;
    }
    match factor {
        2 | 4 | 8 | 16 | 32 => factor,
        _ => 64,
    }
}

/// 用水平采样把源行展开为目标宽度的一行。
fn expand_row(line: &[u32], samples: &[AxisSample], expanded: &mut [u32]) {
    for (slot, horizontal) in expanded.iter_mut().zip(samples) {
        *slot = interpolate_pixel(
            line[horizontal.first as usize],
            line[horizontal.second as usize],
            horizontal.weight,
        );
    }
}

/// 整图双线性上采样核心（常量权重 255）。
fn upsample_bilinear_impl(
    source: ImageRef,
    destination: &mut ImageMut,
    factor: i32,
    use_avx2: bool,
) -> bool {
    let x_samples = prepare_axis_samples(0, destination.width, source.width, factor);
    let y_samples = prepare_axis_samples(0, destination.height, source.height, factor);
    let width = destination.width as usize;
    let mut executed = false;
    let mut first_expanded = vec![0u32; width];
    let mut second_expanded = vec![0u32; width];
    let mut first_source_row = -1;
    let mut second_source_row = -1;
    let row_of = |row: i32| {
        let offset = (row as usize) * source.stride;
        &source.data[offset..offset + source.width as usize]
    };
    for y in 0..destination.height {
        let vertical = y_samples[y as usize];
        if vertical.first == second_source_row {
            std::mem::swap(&mut first_expanded, &mut second_expanded);
            first_source_row = second_source_row;
            second_source_row = -1;
        }
        if vertical.first != first_source_row {
            expand_row(row_of(vertical.first), &x_samples, &mut first_expanded);
            first_source_row = vertical.first;
        }
        if vertical.second != first_source_row && vertical.second != second_source_row {
            expand_row(row_of(vertical.second), &x_samples, &mut second_expanded);
            second_source_row = vertical.second;
        }
        let line0 = &first_expanded;
        let line1 = if vertical.second == first_source_row {
            &first_expanded
        } else {
            &second_expanded
        };
        let offset = (y as usize) * destination.stride;
        let output = &mut destination.data[offset..offset + width];
        let mut x = 0;
        if use_avx2 {
            x = interpolate_and_blend_constant_avx2(
                line0,
                line1,
                output,
                destination.width,
                vertical.weight,
                255,
            );
            if x > 0 {
                executed = true;
            }
        }
        for i in x as usize..width {
            output[i] = interpolate_pixel(line0[i], line1[i], vertical.weight);
        }
    }
    executed
}

/// 双线性上采样（整图）。
///
/// 参数：`source` 为缩小图；`destination` 为整图；`factor` 为缩小倍数（`<=1` 为逐行拷贝）；
/// `use_avx2` 为是否尝试 AVX2。要求非空图像。
/// 返回：是否执行了 AVX2 路径。
///
/// 示例：`let simd = upsample_bilinear(small.as_ref(), &mut big.as_mut(), 4, true);`
pub(crate) fn upsample_bilinear(
    source: ImageRef,
    destination: &mut ImageMut,
    factor: i32,
    use_avx2: bool,
) -> bool {
    if factor <= 1 {
        let width = destination.width as usize;
        for y in 0..destination.height {
            let src = (y as usize) * source.stride;
            let dst = (y as usize) * destination.stride;
            destination.data[dst..dst + width].copy_from_slice(&source.data[src..src + width]);
        }
        return false;
    }
    upsample_bilinear_impl(
        source,
        destination,
        normalize_factor(factor, false),
        use_avx2,
    )
}

/// 带遮罩/常量权重合成的上采样核心。
#[allow(clippy::too_many_arguments)]
fn upsample_bilinear_composited_impl(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: Option<AlphaRef>,
    mask_origin_x: i32,
    mask_origin_y: i32,
    destination_pixels: Rect,
    source_pixels: Rect,
    factor: i32,
    constant_mix: i32,
    use_avx2: bool,
) -> bool {
    let x_samples = prepare_axis_samples(
        destination_pixels.x - source_pixels.x,
        destination_pixels.w,
        source.width,
        factor,
    );
    let y_samples = prepare_axis_samples(
        destination_pixels.y - source_pixels.y,
        destination_pixels.h,
        source.height,
        factor,
    );
    let width = destination_pixels.w as usize;
    let mut executed = false;
    let mut first_expanded = vec![0u32; width];
    let mut second_expanded = vec![0u32; width];
    let mut first_source_row = -1;
    let mut second_source_row = -1;
    let row_of = |row: i32| {
        let offset = (row as usize) * source.stride;
        &source.data[offset..offset + source.width as usize]
    };
    for local_y in 0..destination_pixels.h {
        let y = destination_pixels.y + local_y;
        let vertical = y_samples[local_y as usize];
        if vertical.first == second_source_row {
            std::mem::swap(&mut first_expanded, &mut second_expanded);
            first_source_row = second_source_row;
            second_source_row = -1;
        }
        if vertical.first != first_source_row {
            expand_row(row_of(vertical.first), &x_samples, &mut first_expanded);
            first_source_row = vertical.first;
        }
        if vertical.second != first_source_row && vertical.second != second_source_row {
            expand_row(row_of(vertical.second), &x_samples, &mut second_expanded);
            second_source_row = vertical.second;
        }
        let line0 = &first_expanded;
        let line1 = if vertical.second == first_source_row {
            &first_expanded
        } else {
            &second_expanded
        };
        let output_start = (y as usize) * destination.stride + destination_pixels.x as usize;
        let output = &mut destination.data[output_start..output_start + width];
        let alpha_line = mask.map(|m| {
            let start = ((y - mask_origin_y) as usize) * m.stride
                + (destination_pixels.x - mask_origin_x) as usize;
            &m.data[start..start + width]
        });
        let mut local_x = 0;
        if use_avx2 {
            local_x = match alpha_line {
                None => interpolate_and_blend_constant_avx2(
                    line0,
                    line1,
                    output,
                    destination_pixels.w,
                    vertical.weight,
                    constant_mix,
                ),
                Some(alpha) => interpolate_and_blend_masked_avx2(
                    line0,
                    line1,
                    output,
                    alpha,
                    destination_pixels.w,
                    vertical.weight,
                ),
            };
            if local_x > 0 {
                executed = true;
            }
        }
        for i in local_x as usize..width {
            let mix = alpha_line.map_or(constant_mix, |alpha| alpha[i] as i32);
            if mix == 0 {
                continue;
            }
            let effect = interpolate_pixel(line0[i], line1[i], vertical.weight);
            output[i] = if mix == 255 {
                effect
            } else {
                blend_premultiplied(output[i], effect, mix)
            };
        }
    }
    executed
}

/// 带遮罩/常量权重合成的双线性上采样。
///
/// 参数：`source` 为缩小图；`mask` 为 `None` 时使用 `constant_mix`；`mask_origin_*` 为遮罩原点；
/// `destination_pixels` 为写入矩形；`source_pixels` 为对应源区域；`factor` 为缩小倍数。要求非空图像。
/// 返回：是否执行了 AVX2 路径。
///
/// 示例：`upsample_bilinear_composited(a, &mut dst, None, 0, 0, rect, src_rect, 4, 255, true);`
#[allow(clippy::too_many_arguments)]
pub(crate) fn upsample_bilinear_composited(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: Option<AlphaRef>,
    mask_origin_x: i32,
    mask_origin_y: i32,
    destination_pixels: Rect,
    source_pixels: Rect,
    factor: i32,
    constant_mix: i32,
    use_avx2: bool,
) -> bool {
    upsample_bilinear_composited_impl(
        source,
        destination,
        mask,
        mask_origin_x,
        mask_origin_y,
        destination_pixels,
        source_pixels,
        normalize_factor(factor, true),
        constant_mix,
        use_avx2,
    )
}
