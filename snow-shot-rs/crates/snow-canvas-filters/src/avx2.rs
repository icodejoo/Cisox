//! AVX2 内核：直译自 C++ `snow_canvas_filter_avx2.cpp`（拷贝/四抽头降采样/插值混合/灰度/反相）。
//!
//! 所有公共函数先做运行时 AVX2 检测，不支持时返回 `false`/`0`（等价 C++ 的“未执行”），
//! 由调用方回退到标量实现。

use crate::image::{AlphaRef, ImageMut, ImageRef};
use crate::pixel::{blend_premultiplied, q_alpha, q_blue, q_green, q_red, q_rgba};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// 当前 CPU 是否支持 AVX2。
///
/// 返回：支持返回 `true`，非 x86_64 恒为 `false`。
///
/// 示例：`if snow_canvas_filters::avx2::is_avx2_available() { /* ... */ }`
pub fn is_avx2_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// AVX2 整行拷贝。
///
/// 参数：`source`/`destination` 宽度须相同且不小于 8；`begin_row..end_row` 为行范围。
/// 返回：是否执行（宽度不足、行范围为空或无 AVX2 时为 `false`）。
///
/// 示例：`copy_rows_avx2(src, &mut dst, 0, 10);`
pub fn copy_rows_avx2(
    source: ImageRef,
    destination: &mut ImageMut,
    begin_row: i32,
    end_row: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if source.width != destination.width || source.width < 8 {
                return false;
            }
            if begin_row < end_row {
                assert!(begin_row >= 0);
                // 越界探测：先用带检查的索引确认最远访问点在缓冲内
                let _ = &source.data
                    [((end_row - 1) as usize) * source.stride + (source.width as usize) - 1];
                let _ = &destination.data[((end_row - 1) as usize) * destination.stride
                    + (destination.width as usize)
                    - 1];
                // SAFETY: 已确认 AVX2 可用，且行范围内最远像素已通过索引检查。
                return unsafe { copy_rows_impl(source, destination, begin_row, end_row) };
            }
        }
    }
    let _ = (source, destination, begin_row, end_row);
    false
}

/// 拷贝核心实现。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn copy_rows_impl(
    source: ImageRef,
    destination: &mut ImageMut,
    begin_row: i32,
    end_row: i32,
) -> bool {
    // SAFETY: 调用方已保证行范围与宽度在缓冲内；loadu/storeu 允许非对齐。
    unsafe {
        for y in begin_row..end_row {
            let input = source.data.as_ptr().add((y as usize) * source.stride);
            let output = destination
                .data
                .as_mut_ptr()
                .add((y as usize) * destination.stride);
            let mut x = 0;
            while x + 8 <= source.width {
                let v = _mm256_loadu_si256(input.add(x as usize) as *const __m256i);
                _mm256_storeu_si256(output.add(x as usize) as *mut __m256i, v);
                x += 8;
            }
            while x < source.width {
                *output.add(x as usize) = *input.add(x as usize);
                x += 1;
            }
        }
    }
    begin_row < end_row
}

/// AVX2 四抽头降采样。
///
/// 参数：`source_right`/`source_bottom` 为开区间末端；`factor >= 2`，目标宽度不小于 8。
/// 返回：是否至少执行了一个向量块（与 C++ 一致）。
///
/// 示例：`downsample_four_tap_avx2(src, 0, 0, 100, 100, &mut dst, 2, 0, 50);`
#[allow(clippy::too_many_arguments)]
pub fn downsample_four_tap_avx2(
    source: ImageRef,
    source_left: i32,
    source_top: i32,
    source_right: i32,
    source_bottom: i32,
    destination: &mut ImageMut,
    factor: i32,
    begin_row: i32,
    end_row: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if factor < 2 || destination.width < 8 {
                return false;
            }
            if begin_row < end_row {
                assert!(begin_row >= 0);
                // 最远采样点：最后一行、最后一列的第二个抽头
                let max_top = source_top + (end_row - 1) * factor;
                let max_bottom = source_bottom.min(max_top + factor);
                let max_y1 = max_top + ((max_bottom - max_top) * 3) / 4;
                let max_left = source_left + (destination.width - 1) * factor;
                let max_right = source_right.min(max_left + factor);
                let max_x1 = max_left + ((max_right - max_left) * 3) / 4;
                assert!(max_y1 >= 0 && max_x1 >= 0 && source_left >= 0 && source_top >= 0);
                let _ = &source.data[(max_y1 as usize) * source.stride + (max_x1 as usize)];
                let _ = &destination.data[((end_row - 1) as usize) * destination.stride
                    + (destination.width as usize)
                    - 1];
                // SAFETY: 已确认 AVX2 可用，最远读写点已通过索引检查。
                return unsafe {
                    downsample_impl(
                        source,
                        source_left,
                        source_right,
                        source_top,
                        source_bottom,
                        destination,
                        factor,
                        begin_row,
                        end_row,
                    )
                };
            }
        }
    }
    let _ = (
        source,
        source_left,
        source_top,
        source_right,
        source_bottom,
        destination,
        factor,
        begin_row,
        end_row,
    );
    false
}

/// 32 字节对齐的 8 个 i32（gather 索引用）。
#[cfg(target_arch = "x86_64")]
#[repr(C, align(32))]
struct AlignedI32x8([i32; 8]);

/// 降采样核心实现。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn downsample_impl(
    source: ImageRef,
    source_left: i32,
    source_right: i32,
    source_top: i32,
    source_bottom: i32,
    destination: &mut ImageMut,
    factor: i32,
    begin_row: i32,
    end_row: i32,
) -> bool {
    let mut executed = false;
    // SAFETY: 行指针与 gather 索引均不超过调用方检查过的最远采样点。
    unsafe {
        let zero = _mm256_setzero_si256();
        for y in begin_row..end_row {
            let top = source_top + y * factor;
            let bottom = source_bottom.min(top + factor);
            let y0 = top + (bottom - top) / 4;
            let y1 = top + ((bottom - top) * 3) / 4;
            let line0 = source.data.as_ptr().add((y0 as usize) * source.stride);
            let line1 = source.data.as_ptr().add((y1 as usize) * source.stride);
            let output = destination
                .data
                .as_mut_ptr()
                .add((y as usize) * destination.stride);
            let mut x = 0;
            let mut first_indices = AlignedI32x8([0; 8]);
            let mut second_indices = AlignedI32x8([0; 8]);
            while x + 8 <= destination.width {
                let mut full = y0 != y1;
                for lane in 0..8 {
                    let left = source_left + (x + lane) * factor;
                    let right = source_right.min(left + factor);
                    first_indices.0[lane as usize] = left + (right - left) / 4;
                    second_indices.0[lane as usize] = left + ((right - left) * 3) / 4;
                    full =
                        full && first_indices.0[lane as usize] != second_indices.0[lane as usize];
                }
                if !full {
                    break;
                }
                let first = _mm256_load_si256(first_indices.0.as_ptr() as *const __m256i);
                let second = _mm256_load_si256(second_indices.0.as_ptr() as *const __m256i);
                let a = _mm256_i32gather_epi32::<4>(line0 as *const i32, first);
                let b = _mm256_i32gather_epi32::<4>(line0 as *const i32, second);
                let c = _mm256_i32gather_epi32::<4>(line1 as *const i32, first);
                let d = _mm256_i32gather_epi32::<4>(line1 as *const i32, second);
                let low = _mm256_srli_epi16(
                    _mm256_add_epi16(
                        _mm256_add_epi16(
                            _mm256_unpacklo_epi8(a, zero),
                            _mm256_unpacklo_epi8(b, zero),
                        ),
                        _mm256_add_epi16(
                            _mm256_unpacklo_epi8(c, zero),
                            _mm256_unpacklo_epi8(d, zero),
                        ),
                    ),
                    2,
                );
                let high = _mm256_srli_epi16(
                    _mm256_add_epi16(
                        _mm256_add_epi16(
                            _mm256_unpackhi_epi8(a, zero),
                            _mm256_unpackhi_epi8(b, zero),
                        ),
                        _mm256_add_epi16(
                            _mm256_unpackhi_epi8(c, zero),
                            _mm256_unpackhi_epi8(d, zero),
                        ),
                    ),
                    2,
                );
                let result = _mm256_packus_epi16(low, high);
                _mm256_storeu_si256(output.add(x as usize) as *mut __m256i, result);
                executed = true;
                x += 8;
            }
            while x < destination.width {
                let left = source_left + x * factor;
                let right = source_right.min(left + factor);
                let x0 = left + (right - left) / 4;
                let x1 = left + ((right - left) * 3) / 4;
                let samples = [
                    *line0.add(x0 as usize),
                    *line0.add(x1 as usize),
                    *line1.add(x0 as usize),
                    *line1.add(x1 as usize),
                ];
                let x_count = if x0 == x1 { 1 } else { 2 };
                let y_count = if y0 == y1 { 1 } else { 2 };
                let (mut a, mut r, mut g, mut b) = (0, 0, 0, 0);
                for sy in 0..y_count {
                    for sx in 0..x_count {
                        let pixel = samples[(sy * 2 + sx) as usize];
                        a += q_alpha(pixel);
                        r += q_red(pixel);
                        g += q_green(pixel);
                        b += q_blue(pixel);
                    }
                }
                let count = x_count * y_count;
                *output.add(x as usize) = q_rgba(r / count, g / count, b / count, a / count);
                x += 1;
            }
        }
    }
    executed
}

/// 展开的插值半边：`(from*(256-w) + to*w + 128) >> 8`（16 位通道）。
#[cfg(target_arch = "x86_64")]
macro_rules! interpolate_half {
    ($from:expr, $to:expr, $inverse:expr, $weight:expr, $rounding:expr) => {
        _mm256_srli_epi16(
            _mm256_add_epi16(
                _mm256_add_epi16(
                    _mm256_mullo_epi16($from, $inverse),
                    _mm256_mullo_epi16($to, $weight),
                ),
                $rounding,
            ),
            8,
        )
    };
}

/// 展开的 /255 近似：`(v + 1 + (v >> 8)) >> 8`。
#[cfg(target_arch = "x86_64")]
macro_rules! divide_by_255 {
    ($value:expr, $one:expr) => {
        _mm256_srli_epi16(
            _mm256_add_epi16(_mm256_add_epi16($value, $one), _mm256_srli_epi16($value, 8)),
            8,
        )
    };
}

/// AVX2 双行插值并按常量权重混合到目标。
///
/// 参数：`count >= 8`，`0 <= weight <= 256`，`0 < mix <= 255`。
/// 返回：已处理的像素数（8 的倍数），条件不满足或无 AVX2 时为 0。
///
/// 示例：`interpolate_and_blend_constant_avx2(&a, &b, &mut dst, 64, 128, 255);`
pub fn interpolate_and_blend_constant_avx2(
    first: &[u32],
    second: &[u32],
    destination: &mut [u32],
    count: i32,
    weight: i32,
    mix: i32,
) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if count < 8 || !(0..=256).contains(&weight) || mix <= 0 || mix > 255 {
                return 0;
            }
            let c = count as usize;
            let _ = &first[c - 1];
            let _ = &second[c - 1];
            let _ = &destination[c - 1];
            // SAFETY: 已确认 AVX2 可用，三个缓冲的前 count 个元素已通过索引检查。
            return unsafe {
                interpolate_constant_impl(first, second, destination, count, weight, mix)
            };
        }
    }
    let _ = (first, second, destination, count, weight, mix);
    0
}

/// 常量权重插值混合核心。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn interpolate_constant_impl(
    first: &[u32],
    second: &[u32],
    destination: &mut [u32],
    count: i32,
    weight: i32,
    mix: i32,
) -> i32 {
    // SAFETY: 调用方保证前 count 个元素可访问；loadu/storeu 允许非对齐。
    unsafe {
        let zero = _mm256_setzero_si256();
        let interpolation_weight = _mm256_set1_epi16(weight as i16);
        let interpolation_inverse = _mm256_set1_epi16((256 - weight) as i16);
        let interpolation_rounding = _mm256_set1_epi16(128);
        let blend_weight = _mm256_set1_epi16(mix as i16);
        let blend_inverse = _mm256_set1_epi16((255 - mix) as i16);
        let blend_rounding = _mm256_set1_epi16(127);
        let one = _mm256_set1_epi16(1);
        let spread_byte = _mm256_set1_epi32(0x0101_0101);
        let first_ptr = first.as_ptr();
        let second_ptr = second.as_ptr();
        let dest_ptr = destination.as_mut_ptr();
        let mut processed = 0;
        while processed + 8 <= count {
            let p = processed as usize;
            let a = _mm256_loadu_si256(first_ptr.add(p) as *const __m256i);
            let b = _mm256_loadu_si256(second_ptr.add(p) as *const __m256i);
            let effect_low = interpolate_half!(
                _mm256_unpacklo_epi8(a, zero),
                _mm256_unpacklo_epi8(b, zero),
                interpolation_inverse,
                interpolation_weight,
                interpolation_rounding
            );
            let effect_high = interpolate_half!(
                _mm256_unpackhi_epi8(a, zero),
                _mm256_unpackhi_epi8(b, zero),
                interpolation_inverse,
                interpolation_weight,
                interpolation_rounding
            );
            let mut result = _mm256_packus_epi16(effect_low, effect_high);
            if mix < 255 {
                let current = _mm256_loadu_si256(dest_ptr.add(p) as *const __m256i);
                let blend = |cur: __m256i, eff: __m256i| {
                    divide_by_255!(
                        _mm256_add_epi16(
                            _mm256_add_epi16(
                                _mm256_mullo_epi16(cur, blend_inverse),
                                _mm256_mullo_epi16(eff, blend_weight),
                            ),
                            blend_rounding,
                        ),
                        one
                    )
                };
                result = _mm256_packus_epi16(
                    blend(_mm256_unpacklo_epi8(current, zero), effect_low),
                    blend(_mm256_unpackhi_epi8(current, zero), effect_high),
                );
            }
            let replicated_alpha = _mm256_mullo_epi32(_mm256_srli_epi32(result, 24), spread_byte);
            result = _mm256_min_epu8(result, replicated_alpha);
            _mm256_storeu_si256(dest_ptr.add(p) as *mut __m256i, result);
            processed += 8;
        }
        processed
    }
}

/// AVX2 双行插值并按逐像素遮罩混合到目标。
///
/// 参数：`count >= 8`，`0 <= weight <= 256`，`mask` 至少 `count` 字节。
/// 返回：已处理的像素数（8 的倍数），条件不满足或无 AVX2 时为 0。
///
/// 示例：`interpolate_and_blend_masked_avx2(&a, &b, &mut dst, &mask, 64, 128);`
pub fn interpolate_and_blend_masked_avx2(
    first: &[u32],
    second: &[u32],
    destination: &mut [u32],
    mask: &[u8],
    count: i32,
    weight: i32,
) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if count < 8 || !(0..=256).contains(&weight) {
                return 0;
            }
            let c = count as usize;
            let _ = &first[c - 1];
            let _ = &second[c - 1];
            let _ = &destination[c - 1];
            let _ = &mask[c - 1];
            // SAFETY: 已确认 AVX2 可用，四个缓冲的前 count 个元素已通过索引检查。
            return unsafe {
                interpolate_masked_impl(first, second, destination, mask, count, weight)
            };
        }
    }
    let _ = (first, second, destination, mask, count, weight);
    0
}

/// 遮罩插值混合核心。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn interpolate_masked_impl(
    first: &[u32],
    second: &[u32],
    destination: &mut [u32],
    mask: &[u8],
    count: i32,
    weight: i32,
) -> i32 {
    // SAFETY: 调用方保证前 count 个元素可访问；mask 每次读 8 字节且 p+8<=count。
    unsafe {
        let zero = _mm256_setzero_si256();
        let full = _mm256_set1_epi16(255);
        let interpolation_weight = _mm256_set1_epi16(weight as i16);
        let interpolation_inverse = _mm256_set1_epi16((256 - weight) as i16);
        let interpolation_rounding = _mm256_set1_epi16(128);
        let blend_rounding = _mm256_set1_epi16(127);
        let one = _mm256_set1_epi16(1);
        let spread_byte = _mm256_set1_epi32(0x0101_0101);
        let first_ptr = first.as_ptr();
        let second_ptr = second.as_ptr();
        let dest_ptr = destination.as_mut_ptr();
        let mask_ptr = mask.as_ptr();
        let mut processed = 0;
        while processed + 8 <= count {
            let p = processed as usize;
            let a = _mm256_loadu_si256(first_ptr.add(p) as *const __m256i);
            let b = _mm256_loadu_si256(second_ptr.add(p) as *const __m256i);
            let effect_low = interpolate_half!(
                _mm256_unpacklo_epi8(a, zero),
                _mm256_unpacklo_epi8(b, zero),
                interpolation_inverse,
                interpolation_weight,
                interpolation_rounding
            );
            let effect_high = interpolate_half!(
                _mm256_unpackhi_epi8(a, zero),
                _mm256_unpackhi_epi8(b, zero),
                interpolation_inverse,
                interpolation_weight,
                interpolation_rounding
            );
            let current = _mm256_loadu_si256(dest_ptr.add(p) as *const __m256i);
            let mask_bytes = _mm_loadl_epi64(mask_ptr.add(p) as *const __m128i);
            let packed_mix = _mm256_mullo_epi32(_mm256_cvtepu8_epi32(mask_bytes), spread_byte);
            let mix_low = _mm256_unpacklo_epi8(packed_mix, zero);
            let mix_high = _mm256_unpackhi_epi8(packed_mix, zero);
            let blend = |cur: __m256i, eff: __m256i, mix: __m256i| {
                divide_by_255!(
                    _mm256_add_epi16(
                        _mm256_add_epi16(
                            _mm256_mullo_epi16(cur, _mm256_sub_epi16(full, mix)),
                            _mm256_mullo_epi16(eff, mix),
                        ),
                        blend_rounding,
                    ),
                    one
                )
            };
            let mut result = _mm256_packus_epi16(
                blend(_mm256_unpacklo_epi8(current, zero), effect_low, mix_low),
                blend(_mm256_unpackhi_epi8(current, zero), effect_high, mix_high),
            );
            let replicated_alpha = _mm256_mullo_epi32(_mm256_srli_epi32(result, 24), spread_byte);
            result = _mm256_min_epu8(result, replicated_alpha);
            _mm256_storeu_si256(dest_ptr.add(p) as *mut __m256i, result);
            processed += 8;
        }
        processed
    }
}

/// 灰度标量像素：亮度 = min(alpha, (54R+183G+19B+128)>>8)。
#[allow(dead_code)]
#[inline]
fn grayscale_pixel(pixel: u32) -> u32 {
    let alpha = q_alpha(pixel);
    let luminance =
        alpha.min((q_red(pixel) * 54 + q_green(pixel) * 183 + q_blue(pixel) * 19 + 128) >> 8);
    q_rgba(luminance, luminance, luminance, alpha)
}

/// 反相标量像素：各通道 = alpha - 通道。
#[allow(dead_code)]
#[inline]
fn inversion_pixel(pixel: u32) -> u32 {
    let alpha = q_alpha(pixel);
    q_rgba(
        alpha - q_red(pixel),
        alpha - q_green(pixel),
        alpha - q_blue(pixel),
        alpha,
    )
}

/// 灰度向量版（8 像素）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn grayscale_pixels(pixels: __m256i) -> __m256i {
    let byte_mask = _mm256_set1_epi32(0xff);
    let red = _mm256_and_si256(_mm256_srli_epi32(pixels, 16), byte_mask);
    let green = _mm256_and_si256(_mm256_srli_epi32(pixels, 8), byte_mask);
    let blue = _mm256_and_si256(pixels, byte_mask);
    let mut luminance = _mm256_add_epi32(
        _mm256_add_epi32(
            _mm256_mullo_epi32(red, _mm256_set1_epi32(54)),
            _mm256_mullo_epi32(green, _mm256_set1_epi32(183)),
        ),
        _mm256_add_epi32(
            _mm256_mullo_epi32(blue, _mm256_set1_epi32(19)),
            _mm256_set1_epi32(128),
        ),
    );
    luminance = _mm256_srli_epi32(luminance, 8);
    let alpha = _mm256_srli_epi32(pixels, 24);
    luminance = _mm256_min_epu32(luminance, alpha);
    let gray = _mm256_mullo_epi32(luminance, _mm256_set1_epi32(0x0001_0101));
    _mm256_or_si256(gray, _mm256_slli_epi32(alpha, 24))
}

/// 反相向量版（8 像素）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn inversion_pixels(pixels: __m256i) -> __m256i {
    let alpha = _mm256_srli_epi32(pixels, 24);
    let replicated = _mm256_mullo_epi32(alpha, _mm256_set1_epi32(0x0101_0101));
    let inverted = _mm256_subs_epu8(replicated, pixels);
    _mm256_or_si256(
        _mm256_and_si256(inverted, _mm256_set1_epi32(0x00ff_ffff)),
        _mm256_and_si256(pixels, _mm256_set1_epi32(0xff00_0000u32 as i32)),
    )
}

/// 常量权重向量混合。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn blend_constant(current: __m256i, effect: __m256i, mix: i32) -> __m256i {
    let zero = _mm256_setzero_si256();
    let weight = _mm256_set1_epi16(mix as i16);
    let inverse = _mm256_set1_epi16((255 - mix) as i16);
    let rounding = _mm256_set1_epi16(127);
    let one = _mm256_set1_epi16(1);
    let half = |first: __m256i, second: __m256i| {
        let value = _mm256_add_epi16(
            _mm256_add_epi16(
                _mm256_mullo_epi16(first, inverse),
                _mm256_mullo_epi16(second, weight),
            ),
            rounding,
        );
        divide_by_255!(value, one)
    };
    _mm256_packus_epi16(
        half(
            _mm256_unpacklo_epi8(current, zero),
            _mm256_unpacklo_epi8(effect, zero),
        ),
        half(
            _mm256_unpackhi_epi8(current, zero),
            _mm256_unpackhi_epi8(effect, zero),
        ),
    )
}

/// 逐像素权重向量混合（`mix32` 每 32 位车道为 0..=255）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn blend_variable(current: __m256i, effect: __m256i, mix32: __m256i) -> __m256i {
    let zero = _mm256_setzero_si256();
    let full = _mm256_set1_epi16(255);
    let rounding = _mm256_set1_epi16(127);
    let one = _mm256_set1_epi16(1);
    let packed_mix = _mm256_mullo_epi32(mix32, _mm256_set1_epi32(0x0101_0101));
    let mix_low = _mm256_unpacklo_epi8(packed_mix, zero);
    let mix_high = _mm256_unpackhi_epi8(packed_mix, zero);
    let half = |first: __m256i, second: __m256i, mix: __m256i| {
        let value = _mm256_add_epi16(
            _mm256_add_epi16(
                _mm256_mullo_epi16(first, _mm256_sub_epi16(full, mix)),
                _mm256_mullo_epi16(second, mix),
            ),
            rounding,
        );
        divide_by_255!(value, one)
    };
    _mm256_packus_epi16(
        half(
            _mm256_unpacklo_epi8(current, zero),
            _mm256_unpacklo_epi8(effect, zero),
            mix_low,
        ),
        half(
            _mm256_unpackhi_epi8(current, zero),
            _mm256_unpackhi_epi8(effect, zero),
            mix_high,
        ),
    )
}

/// 遮罩与强度合成：`strength_mix >= 255` 直接取遮罩，否则 `(m*s+127)` 后 /255 近似。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn combine_mask_strength(mask32: __m256i, strength_mix: i32) -> __m256i {
    if strength_mix >= 255 {
        return mask32;
    }
    let value = _mm256_add_epi32(
        _mm256_mullo_epi32(mask32, _mm256_set1_epi32(strength_mix)),
        _mm256_set1_epi32(127),
    );
    _mm256_srli_epi32(
        _mm256_add_epi32(
            _mm256_add_epi32(value, _mm256_set1_epi32(1)),
            _mm256_srli_epi32(value, 8),
        ),
        8,
    )
}

/// 颜色矩形核心（源、目标可为同一缓冲，逐元素同下标先读后写）。
///
/// `kind`：`true` 灰度，`false` 反相。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn color_rect_impl(
    gray: bool,
    src: *const u32,
    src_stride: usize,
    dst: *mut u32,
    dst_stride: usize,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    mix: i32,
) -> bool {
    if right - left < 8 || mix <= 0 {
        return false;
    }
    // SAFETY: 调用方已确认所有行/列在缓冲内；源目标同址时每个向量先 load 后 store，标量同理。
    unsafe {
        for y in top..bottom {
            let source_line = src.add((y as usize) * src_stride);
            let destination_line = dst.add((y as usize) * dst_stride);
            let mut x = left;
            while x + 8 <= right {
                let from = _mm256_loadu_si256(source_line.add(x as usize) as *const __m256i);
                let mut result = if gray {
                    grayscale_pixels(from)
                } else {
                    inversion_pixels(from)
                };
                if mix < 255 {
                    let current =
                        _mm256_loadu_si256(destination_line.add(x as usize) as *const __m256i);
                    result = blend_constant(current, result, mix);
                }
                _mm256_storeu_si256(destination_line.add(x as usize) as *mut __m256i, result);
                x += 8;
            }
            while x < right {
                let s = *source_line.add(x as usize);
                let effect = if gray {
                    grayscale_pixel(s)
                } else {
                    inversion_pixel(s)
                };
                let d = destination_line.add(x as usize);
                *d = if mix == 255 {
                    effect
                } else {
                    blend_premultiplied(*d, effect, mix)
                };
                x += 1;
            }
        }
    }
    true
}

/// 颜色遮罩核心（`gray` 同上）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn color_masked_impl(
    gray: bool,
    src: *const u32,
    src_stride: usize,
    dst: *mut u32,
    dst_stride: usize,
    mask: *const u8,
    mask_stride: usize,
    mask_origin_x: i32,
    mask_origin_y: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    strength_mix: i32,
) -> bool {
    if right - left < 8 || strength_mix <= 0 {
        return false;
    }
    // SAFETY: 调用方已确认源/目标/遮罩的访问范围在缓冲内；遮罩每次读 8 字节且 x+8<=right。
    unsafe {
        let full32 = _mm256_set1_epi32(255);
        for y in top..bottom {
            let source_line = src.add((y as usize) * src_stride);
            let destination_line = dst.add((y as usize) * dst_stride);
            let alpha_line = mask.add(((y - mask_origin_y) as usize) * mask_stride);
            let mut x = left;
            while x + 8 <= right {
                let mask_bytes =
                    _mm_loadl_epi64(alpha_line.add((x - mask_origin_x) as usize) as *const __m128i);
                let mix32 = combine_mask_strength(_mm256_cvtepu8_epi32(mask_bytes), strength_mix);
                if _mm256_testz_si256(mix32, mix32) != 0 {
                    x += 8;
                    continue;
                }
                let from = _mm256_loadu_si256(source_line.add(x as usize) as *const __m256i);
                let effect = if gray {
                    grayscale_pixels(from)
                } else {
                    inversion_pixels(from)
                };
                let mut result = effect;
                let full_coverage = _mm256_cmpeq_epi32(mix32, full32);
                if _mm256_movemask_epi8(full_coverage) != -1 {
                    let current =
                        _mm256_loadu_si256(destination_line.add(x as usize) as *const __m256i);
                    result = blend_variable(current, effect, mix32);
                }
                _mm256_storeu_si256(destination_line.add(x as usize) as *mut __m256i, result);
                x += 8;
            }
            while x < right {
                let mix = (*alpha_line.add((x - mask_origin_x) as usize) as i32 * strength_mix
                    + 127)
                    / 255;
                if mix == 0 {
                    x += 1;
                    continue;
                }
                let s = *source_line.add(x as usize);
                let effect = if gray {
                    grayscale_pixel(s)
                } else {
                    inversion_pixel(s)
                };
                let d = destination_line.add(x as usize);
                *d = if mix == 255 {
                    effect
                } else {
                    blend_premultiplied(*d, effect, mix)
                };
                x += 1;
            }
        }
    }
    true
}

/// 矩形颜色变换的安全包装（含越界探测）。
#[allow(clippy::too_many_arguments)]
fn color_rect_checked(
    gray: bool,
    source: ImageRef,
    destination: &mut ImageMut,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    mix: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if right - left < 8 || mix <= 0 {
                return false;
            }
            if top < bottom {
                assert!(top >= 0 && left >= 0);
                let last = ((bottom - 1) as usize, (right - 1) as usize);
                let _ = &source.data[last.0 * source.stride + last.1];
                let _ = &destination.data[last.0 * destination.stride + last.1];
                // SAFETY: 已确认 AVX2 可用，最远像素已通过索引检查；源目标为不同缓冲。
                return unsafe {
                    color_rect_impl(
                        gray,
                        source.data.as_ptr(),
                        source.stride,
                        destination.data.as_mut_ptr(),
                        destination.stride,
                        left,
                        top,
                        right,
                        bottom,
                        mix,
                    )
                };
            }
            // 与 C++ 一致：空行范围也返回 true（循环不执行）
            return true;
        }
    }
    let _ = (gray, source, destination, left, top, right, bottom, mix);
    false
}

/// AVX2 灰度（矩形，`right`/`bottom` 为开区间末端）。
///
/// 返回：是否执行（宽度不足 8、`mix <= 0` 或无 AVX2 时为 `false`）。
///
/// 示例：`grayscale_rect_avx2(src, &mut dst, 0, 0, 64, 64, 255);`
pub fn grayscale_rect_avx2(
    source: ImageRef,
    destination: &mut ImageMut,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    mix: i32,
) -> bool {
    color_rect_checked(true, source, destination, left, top, right, bottom, mix)
}

/// AVX2 反相（矩形，参数同 [`grayscale_rect_avx2`]）。
///
/// 示例：`invert_rect_avx2(src, &mut dst, 0, 0, 64, 64, 255);`
pub fn invert_rect_avx2(
    source: ImageRef,
    destination: &mut ImageMut,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    mix: i32,
) -> bool {
    color_rect_checked(false, source, destination, left, top, right, bottom, mix)
}

/// 原地颜色变换的安全包装。
fn color_inplace_checked(
    gray: bool,
    image: &mut ImageMut,
    begin_row: i32,
    end_row: i32,
    mix: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if image.width < 8 || mix <= 0 {
                return false;
            }
            if begin_row < end_row {
                assert!(begin_row >= 0);
                let _ = &image.data
                    [((end_row - 1) as usize) * image.stride + (image.width as usize) - 1];
                let ptr = image.data.as_mut_ptr();
                let (stride, width) = (image.stride, image.width);
                // SAFETY: 已确认 AVX2 可用且范围合法；源与目标同址同下标，向量 load 先于 store，
                // 标量先读后写，因此原地安全。
                return unsafe {
                    color_rect_impl(
                        gray,
                        ptr as *const u32,
                        stride,
                        ptr,
                        stride,
                        0,
                        begin_row,
                        width,
                        end_row,
                        mix,
                    )
                };
            }
            return true;
        }
    }
    let _ = (gray, image, begin_row, end_row, mix);
    false
}

/// AVX2 原地灰度（`begin_row..end_row`）。
///
/// 返回：是否执行（宽度不足 8、`mix <= 0` 或无 AVX2 时为 `false`）。
///
/// 示例：`grayscale_avx2(&mut img, 0, img.height, 255);`
pub fn grayscale_avx2(image: &mut ImageMut, begin_row: i32, end_row: i32, mix: i32) -> bool {
    color_inplace_checked(true, image, begin_row, end_row, mix)
}

/// AVX2 原地反相（参数同 [`grayscale_avx2`]）。
///
/// 示例：`invert_avx2(&mut img, 0, img.height, 255);`
pub fn invert_avx2(image: &mut ImageMut, begin_row: i32, end_row: i32, mix: i32) -> bool {
    color_inplace_checked(false, image, begin_row, end_row, mix)
}

/// 遮罩颜色变换的安全包装。
#[allow(clippy::too_many_arguments)]
fn color_masked_checked(
    gray: bool,
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    strength_mix: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            if right - left < 8 || strength_mix <= 0 {
                return false;
            }
            if top < bottom {
                assert!(top >= 0 && left >= 0);
                assert!(top >= mask_origin_y && left >= mask_origin_x);
                let last = ((bottom - 1) as usize, (right - 1) as usize);
                let _ = &source.data[last.0 * source.stride + last.1];
                let _ = &destination.data[last.0 * destination.stride + last.1];
                let _ = &mask.data[((bottom - 1 - mask_origin_y) as usize) * mask.stride
                    + (right - 1 - mask_origin_x) as usize];
                // SAFETY: 已确认 AVX2 可用，源/目标/遮罩的最远访问点已通过索引检查。
                return unsafe {
                    color_masked_impl(
                        gray,
                        source.data.as_ptr(),
                        source.stride,
                        destination.data.as_mut_ptr(),
                        destination.stride,
                        mask.data.as_ptr(),
                        mask.stride,
                        mask_origin_x,
                        mask_origin_y,
                        left,
                        top,
                        right,
                        bottom,
                        strength_mix,
                    )
                };
            }
            return true;
        }
    }
    let _ = (
        gray,
        source,
        destination,
        mask,
        mask_origin_x,
        mask_origin_y,
        left,
        top,
        right,
        bottom,
        strength_mix,
    );
    false
}

/// AVX2 遮罩灰度（矩形，`right`/`bottom` 为开区间末端）。
///
/// 返回：是否执行（宽度不足 8、`strength_mix <= 0` 或无 AVX2 时为 `false`）。
///
/// 示例：`grayscale_masked_avx2(src, &mut dst, mask, 0, 0, 0, 0, 64, 64, 255);`
#[allow(clippy::too_many_arguments)]
pub fn grayscale_masked_avx2(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    strength_mix: i32,
) -> bool {
    color_masked_checked(
        true,
        source,
        destination,
        mask,
        mask_origin_x,
        mask_origin_y,
        left,
        top,
        right,
        bottom,
        strength_mix,
    )
}

/// AVX2 遮罩反相（参数同 [`grayscale_masked_avx2`]）。
///
/// 示例：`invert_masked_avx2(src, &mut dst, mask, 0, 0, 0, 0, 64, 64, 255);`
#[allow(clippy::too_many_arguments)]
pub fn invert_masked_avx2(
    source: ImageRef,
    destination: &mut ImageMut,
    mask: AlphaRef,
    mask_origin_x: i32,
    mask_origin_y: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    strength_mix: i32,
) -> bool {
    color_masked_checked(
        false,
        source,
        destination,
        mask,
        mask_origin_x,
        mask_origin_y,
        left,
        top,
        right,
        bottom,
        strength_mix,
    )
}
