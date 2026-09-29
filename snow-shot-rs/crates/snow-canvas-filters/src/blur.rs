//! 高斯模糊（三次盒式近似 + 自适应降采样 + 双线性重建）：直译自 C++ `snow_canvas_filter_render.cpp`。
//!
//! 行并行在 Rust 侧串行；C++ 的 scratch 图像池改为局部缓冲；诊断计数省略。要求非空图像。

// 直译 C++ 下标循环，保持与原文逐行对应
#![allow(clippy::needless_range_loop)]

use crate::image::{AlphaRef, ImageMut, ImageRef, OwnedImage, Rect};
use crate::params::{GaussianBlurPlan, Parameters};
use crate::pixel::{q_alpha, q_blue, q_green, q_red, q_rgba, q_round};
use crate::upsample::{upsample_bilinear, upsample_bilinear_composited};

/// 由 sigma 计算三次盒式模糊的半径。
///
/// 参数：`sigma` 物理 sigma；非正或 NaN 返回全 0。返回：三遍盒式半径。
///
/// 示例：`let radii = gaussian_box_radii(2.0);`
pub fn gaussian_box_radii(sigma: f64) -> [i32; 3] {
    if sigma.is_nan() || sigma <= 0.0 {
        return [0, 0, 0];
    }
    let pass_count = 3;
    let ideal_width = ((12.0 * sigma * sigma / pass_count as f64) + 1.0).sqrt();
    let mut lower_width = ideal_width.floor() as i32;
    if (lower_width & 1) == 0 {
        lower_width -= 1;
    }
    lower_width = 1.max(lower_width);
    let upper_width = lower_width + 2;
    let numerator = 12.0 * sigma * sigma
        - (pass_count * lower_width * lower_width) as f64
        - 4.0 * pass_count as f64 * lower_width as f64
        - 3.0 * pass_count as f64;
    let denominator = -4.0 * lower_width as f64 - 4.0;
    let lower_passes = q_round(numerator / denominator).clamp(0, pass_count);
    let mut radii = [0; 3];
    for index in 0..pass_count {
        radii[index as usize] = (if index < lower_passes {
            lower_width
        } else {
            upper_width
        }) / 2;
    }
    radii
}

/// 盒式平均的定点倒数。
#[derive(Clone, Copy)]
pub(crate) struct BoxAverage {
    /// 2^24 / count 的四舍五入值。
    pub reciprocal: u32,
}

impl BoxAverage {
    /// 求和转平均并钳到 255。
    pub fn apply(&self, sum: i32) -> i32 {
        255.min(((sum as u64 * self.reciprocal as u64 + (1u64 << 23)) >> 24) as i32)
    }
}

/// 构造窗口大小为 `count` 的盒式平均。
pub(crate) fn box_average(count: i32) -> BoxAverage {
    if count <= 1 {
        return BoxAverage {
            reciprocal: 1 << 24,
        };
    }
    let scale: u32 = 1 << 24;
    BoxAverage {
        reciprocal: ((scale as u64 + (count / 2) as u64) / count as u64) as u32,
    }
}

/// 四通道求和转像素。
#[inline]
fn average_pixel(alpha: i32, red: i32, green: i32, blue: i32, average: &BoxAverage) -> u32 {
    q_rgba(
        average.apply(red),
        average.apply(green),
        average.apply(blue),
        average.apply(alpha),
    )
}

/// 逐行拷贝（radius <= 0 分支）。
fn copy_image(source: ImageRef, destination: &mut ImageMut) {
    let width = source.width as usize;
    for y in 0..source.height {
        let src = (y as usize) * source.stride;
        let dst = (y as usize) * destination.stride;
        destination.data[dst..dst + width].copy_from_slice(&source.data[src..src + width]);
    }
}

/// 水平盒式模糊（边缘钳位）。
///
/// 参数：`radius` 半径（`<=0` 为拷贝）；`average` 为 [`box_average`] 结果。
///
/// 示例：`horizontal_box_blur(src, &mut dst, 3, &box_average(7));`
pub(crate) fn horizontal_box_blur(
    source: ImageRef,
    destination: &mut ImageMut,
    radius: i32,
    average: &BoxAverage,
) {
    let width = source.width;
    if radius <= 0 {
        copy_image(source, destination);
        return;
    }
    for y in 0..source.height {
        let input_offset = (y as usize) * source.stride;
        let input = &source.data[input_offset..input_offset + width as usize];
        let output_offset = (y as usize) * destination.stride;
        let output = &mut destination.data[output_offset..output_offset + width as usize];
        let (mut alpha, mut red, mut green, mut blue) = (0, 0, 0, 0);
        for offset in -radius..=radius {
            let pixel = input[offset.clamp(0, width - 1) as usize];
            alpha += q_alpha(pixel);
            red += q_red(pixel);
            green += q_green(pixel);
            blue += q_blue(pixel);
        }
        for x in 0..width {
            output[x as usize] = average_pixel(alpha, red, green, blue, average);
            let removed = input[(x - radius).clamp(0, width - 1) as usize];
            let added = input[(x + radius + 1).clamp(0, width - 1) as usize];
            alpha += q_alpha(added) - q_alpha(removed);
            red += q_red(added) - q_red(removed);
            green += q_green(added) - q_green(removed);
            blue += q_blue(added) - q_blue(removed);
        }
    }
}

/// 垂直盒式模糊（边缘钳位）。参数同 [`horizontal_box_blur`]。
///
/// 示例：`vertical_box_blur(src, &mut dst, 3, &box_average(7));`
pub(crate) fn vertical_box_blur(
    source: ImageRef,
    destination: &mut ImageMut,
    radius: i32,
    average: &BoxAverage,
) {
    let width = source.width;
    let height = source.height;
    if radius <= 0 {
        copy_image(source, destination);
        return;
    }
    let w = width as usize;
    let mut sums = vec![0i32; w * 4];
    let (alpha, rest) = sums.split_at_mut(w);
    let (red, rest) = rest.split_at_mut(w);
    let (green, blue) = rest.split_at_mut(w);
    let row_of = |y: i32| {
        let offset = (y as usize) * source.stride;
        &source.data[offset..offset + w]
    };
    for offset in -radius..=radius {
        let line = row_of(offset.clamp(0, height - 1));
        for x in 0..w {
            alpha[x] += q_alpha(line[x]);
            red[x] += q_red(line[x]);
            green[x] += q_green(line[x]);
            blue[x] += q_blue(line[x]);
        }
    }
    for y in 0..height {
        let output_offset = (y as usize) * destination.stride;
        let output = &mut destination.data[output_offset..output_offset + w];
        for x in 0..w {
            output[x] = average_pixel(alpha[x], red[x], green[x], blue[x], average);
        }
        let removed = row_of((y - radius).clamp(0, height - 1));
        let added = row_of((y + radius + 1).clamp(0, height - 1));
        for x in 0..w {
            alpha[x] += q_alpha(added[x]) - q_alpha(removed[x]);
            red[x] += q_red(added[x]) - q_red(removed[x]);
            green[x] += q_green(added[x]) - q_green(removed[x]);
            blue[x] += q_blue(added[x]) - q_blue(removed[x]);
        }
    }
}

/// 四抽头分层降采样（`factor <= 1` 为区域拷贝）。
///
/// 参数：`source_pixels` 为源区域；`destination` 尺寸为区域按 `factor` 向上取整缩小；
/// `use_avx2` 尝试 AVX2。返回：是否执行了 AVX2 路径。
///
/// 示例：`let simd = downsample(src, Rect::new(0, 0, w, h), &mut small.as_mut(), 4, true);`
pub(crate) fn downsample(
    source: ImageRef,
    source_pixels: Rect,
    destination: &mut ImageMut,
    factor: i32,
    use_avx2: bool,
) -> bool {
    let begin = 0;
    let end = destination.height;
    if factor <= 1 {
        if use_avx2
            && source_pixels == Rect::new(0, 0, source.width, source.height)
            && crate::avx2::copy_rows_avx2(source, destination, begin, end)
        {
            return true;
        }
        let width = destination.width as usize;
        for y in begin..end {
            let dst = (y as usize) * destination.stride;
            let src = ((source_pixels.y + y) as usize) * source.stride + source_pixels.x as usize;
            destination.data[dst..dst + width].copy_from_slice(&source.data[src..src + width]);
        }
        return false;
    }
    let source_right = source_pixels.right() + 1;
    let source_bottom = source_pixels.bottom() + 1;
    if use_avx2
        && crate::avx2::downsample_four_tap_avx2(
            source,
            source_pixels.left(),
            source_pixels.top(),
            source_right,
            source_bottom,
            destination,
            factor,
            begin,
            end,
        )
    {
        return true;
    }
    let width = destination.width;
    for y in begin..end {
        let output_offset = (y as usize) * destination.stride;
        let output = &mut destination.data[output_offset..output_offset + width as usize];
        let top = source_pixels.top() + y * factor;
        let bottom = source_bottom.min(top + factor);
        for x in 0..width {
            let left = source_pixels.left() + x * factor;
            let right = source_right.min(left + factor);
            let sample_x = [left + (right - left) / 4, left + ((right - left) * 3) / 4];
            let sample_y = [top + (bottom - top) / 4, top + ((bottom - top) * 3) / 4];
            let x_count = if sample_x[0] == sample_x[1] { 1 } else { 2 };
            let y_count = if sample_y[0] == sample_y[1] { 1 } else { 2 };
            let (mut alpha, mut red, mut green, mut blue) = (0, 0, 0, 0);
            for &row in sample_y.iter().take(y_count) {
                let input = &source.data[(row as usize) * source.stride..];
                for &column in sample_x.iter().take(x_count) {
                    let pixel = input[column as usize];
                    alpha += q_alpha(pixel);
                    red += q_red(pixel);
                    green += q_green(pixel);
                    blue += q_blue(pixel);
                }
            }
            let count = x_count * y_count;
            output[x as usize] = if count == 4 {
                q_rgba(red >> 2, green >> 2, blue >> 2, alpha >> 2)
            } else if count == 2 {
                q_rgba(red >> 1, green >> 1, blue >> 1, alpha >> 1)
            } else {
                q_rgba(red, green, blue, alpha)
            };
        }
    }
    false
}

/// 生成高斯模糊计划（降采样倍数、三遍半径、物理支撑半径）。
///
/// 参数：`parameters` 取 `logical_sigma` 与 `device_pixel_ratio`。
///
/// 示例：`let plan = make_gaussian_blur_plan(&params);`
pub fn make_gaussian_blur_plan(parameters: &Parameters) -> GaussianBlurPlan {
    let raw_sigma = parameters.logical_sigma * parameters.device_pixel_ratio;
    // 与 C++ `std::max(0.0, x)` 一致：NaN 时取 0.0
    let sigma = if 0.0 < raw_sigma { raw_sigma } else { 0.0 };
    let bands = [
        (2.0, 1),
        (4.0, 2),
        (8.0, 4),
        (16.0, 8),
        (32.0, 16),
        (128.0, 32),
        (f64::INFINITY, 64),
    ];
    let mut factor = 1;
    for &(upper_sigma, reduction) in &bands {
        if sigma < upper_sigma {
            factor = reduction;
            break;
        }
    }
    let radii = gaussian_box_radii(sigma / factor as f64);
    let reduced_support = radii[0] + radii[1] + radii[2];
    GaussianBlurPlan {
        reduction_factor: factor,
        pass_count: 3,
        radii,
        physical_support_radius: reduced_support * factor + if factor > 1 { 2 * factor } else { 0 },
    }
}

/// 对 `a`/`b` 两块缓冲执行各遍“水平 a->b、垂直 b->a”的盒式模糊。
fn run_box_passes(plan: &GaussianBlurPlan, a: &mut OwnedImage, b: &mut OwnedImage) {
    for index in 0..plan.pass_count {
        let radius = plan.radii[index as usize];
        if radius <= 0 {
            continue;
        }
        let average = box_average(radius * 2 + 1);
        horizontal_box_blur(a.as_ref(), &mut b.as_mut(), radius, &average);
        vertical_box_blur(b.as_ref(), &mut a.as_mut(), radius, &average);
    }
}

/// 整图高斯模糊（原地）。
///
/// 参数：`image` 待处理图像（须非空）；`parameters` 取 sigma 与设备像素比；`force_scalar` 禁用 AVX2。
/// 返回：恒为 `true`（C++ 中仅 scratch 分配失败时为 `false`）。
///
/// 示例：`blur(&mut img.as_mut(), &params, false);`
pub fn blur(image: &mut ImageMut, parameters: &Parameters, force_scalar: bool) -> bool {
    let plan = make_gaussian_blur_plan(parameters);
    let factor = plan.reduction_factor;
    let use_avx2 = !force_scalar && crate::avx2::is_avx2_available();
    if factor == 1 {
        let mut scratch = OwnedImage::new(image.width, image.height);
        for index in 0..plan.pass_count {
            let radius = plan.radii[index as usize];
            if radius <= 0 {
                continue;
            }
            let average = box_average(radius * 2 + 1);
            horizontal_box_blur(image.as_ref(), &mut scratch.as_mut(), radius, &average);
            vertical_box_blur(scratch.as_ref(), image, radius, &average);
        }
        return true;
    }
    let reduced_width = (image.width + factor - 1) / factor;
    let reduced_height = (image.height + factor - 1) / factor;
    let mut a = OwnedImage::new(reduced_width, reduced_height);
    let mut b = OwnedImage::new(reduced_width, reduced_height);
    let full = Rect::new(0, 0, image.width, image.height);
    downsample(image.as_ref(), full, &mut a.as_mut(), factor, use_avx2);
    run_box_passes(&plan, &mut a, &mut b);
    upsample_bilinear(a.as_ref(), image, factor, use_avx2);
    true
}

/// 外接矩形（`QRegion::boundingRect` 等价，空输入返回空矩形）。
fn bounding_rect(rects: &[Rect]) -> Rect {
    let mut iter = rects.iter().filter(|r| !r.is_empty());
    let Some(first) = iter.next() else {
        return Rect::default();
    };
    let (mut l, mut t, mut r, mut b) = (first.left(), first.top(), first.right(), first.bottom());
    for rect in iter {
        l = l.min(rect.left());
        t = t.min(rect.top());
        r = r.max(rect.right());
        b = b.max(rect.bottom());
    }
    Rect::new(l, t, r - l + 1, b - t + 1)
}

/// 高斯模糊到目标区域（可选遮罩 / 常量权重合成）。
///
/// 参数：`source`/`destination` 为不同缓冲；`mask` 为 `None` 时使用 `constant_mix`；
/// `destination_region` 为互不重叠的矩形集合；`parameters` 另取网格原点做对齐。
/// 返回：恒为 `true`。
///
/// 示例：`blur_masked(src, &mut dst, None, 0, 0, &[Rect::new(0, 0, 64, 64)], 255, &params, false);`
#[allow(clippy::too_many_arguments)]
pub fn blur_masked(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: Option<AlphaRef>,
    mask_origin_x: i32,
    mask_origin_y: i32,
    destination_region: &[Rect],
    constant_mix: i32,
    parameters: &Parameters,
    force_scalar: bool,
) -> bool {
    let destination_pixels = bounding_rect(destination_region);
    let plan = make_gaussian_blur_plan(parameters);
    let support = plan.physical_support_radius;
    let factor = plan.reduction_factor;
    let source_rect = Rect::new(0, 0, source.width, source.height);
    let requested = destination_pixels
        .adjusted(-support, -support, support, support)
        .intersected(&source_rect);
    if requested.is_empty() {
        return true;
    }
    let align_down = |value: i32, origin: i32| {
        let mut remainder = (value - origin) % factor;
        if remainder < 0 {
            remainder += factor;
        }
        value - remainder
    };
    let align_up = |value: i32, origin: i32| {
        let aligned = align_down(value, origin);
        if aligned == value {
            value
        } else {
            aligned + factor
        }
    };
    let origin_x = q_round(parameters.grid_origin_x);
    let origin_y = q_round(parameters.grid_origin_y);
    let left = align_up(requested.left(), origin_x);
    let top = align_up(requested.top(), origin_y);
    let right_exclusive = align_down(requested.right() + 1, origin_x);
    let bottom_exclusive = align_down(requested.bottom() + 1, origin_y);
    let aligned = Rect::new(left, top, right_exclusive - left, bottom_exclusive - top);
    let source_pixels = if aligned.is_empty() {
        requested
    } else {
        aligned
    };
    let reduced_width = (source_pixels.w + factor - 1) / factor;
    let reduced_height = (source_pixels.h + factor - 1) / factor;
    let mut a = OwnedImage::new(reduced_width, reduced_height);
    let mut b = OwnedImage::new(reduced_width, reduced_height);
    let use_avx2 = !force_scalar && crate::avx2::is_avx2_available();
    downsample(source, source_pixels, &mut a.as_mut(), factor, use_avx2);
    run_box_passes(&plan, &mut a, &mut b);
    for rect in destination_region {
        upsample_bilinear_composited(
            a.as_ref(),
            destination,
            mask,
            mask_origin_x,
            mask_origin_y,
            *rect,
            source_pixels,
            factor,
            constant_mix,
            use_avx2,
        );
    }
    true
}
