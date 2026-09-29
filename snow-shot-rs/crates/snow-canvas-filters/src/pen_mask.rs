//! 画笔胶囊遮罩光栅化：直译自 C++ `snow_canvas_pen_mask_avx2.cpp`。
//!
//! AVX2 主体按 4 像素一组用“乘以 1/长度²”计算投影；尾部像素与纯标量版本用“除以长度²”，
//! 这是 C++ 原实现自身的差异，此处如实保留。

use crate::pixel::q_round;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// 32 字节对齐的 4 个 f64（存放向量结果）。
#[cfg(target_arch = "x86_64")]
#[repr(C, align(32))]
struct AlignedF64x4([f64; 4]);

/// 校验写入范围（越界 panic），空范围返回 `true` 表示无事可做。
fn check_range(
    alpha_len: usize,
    stride: usize,
    begin_x: i32,
    end_x: i32,
    begin_y: i32,
    end_y: i32,
) -> bool {
    if end_x <= begin_x || end_y <= begin_y {
        return true;
    }
    assert!(begin_x >= 0 && begin_y >= 0, "起点坐标必须非负");
    let required = ((end_y - 1) as usize) * stride + (end_x as usize);
    assert!(alpha_len >= required, "alpha 缓冲不足");
    false
}

/// 标量像素：与 C++ 尾部循环体一致，写入 `max(原值, round(coverage*255))`。
#[allow(clippy::too_many_arguments)]
#[inline]
fn scalar_pixel(
    row: &mut [u8],
    x: i32,
    y: i32,
    tile_left: i32,
    tile_top: i32,
    a: (f64, f64),
    d: (f64, f64),
    length_squared: f64,
    transition_outer: f64,
) {
    let px = (tile_left + x) as f64 + 0.5;
    let py = (tile_top + y) as f64 + 0.5;
    let mut projection = 0.0;
    if length_squared > 0.0 {
        projection = (((px - a.0) * d.0 + (py - a.1) * d.1) / length_squared).clamp(0.0, 1.0);
    }
    let distance_x = px - (a.0 + projection * d.0);
    let distance_y = py - (a.1 + projection * d.1);
    let coverage = (transition_outer - (distance_x * distance_x + distance_y * distance_y).sqrt())
        .clamp(0.0, 1.0);
    let slot = &mut row[x as usize];
    *slot = (*slot as i32).max(q_round(coverage * 255.0)) as u8;
}

/// AVX2 核心实现（无 FMA，乘加分开以保持逐位一致）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
fn capsule_avx2(
    alpha: &mut [u8],
    stride: usize,
    tile_left: i32,
    tile_top: i32,
    begin_x: i32,
    end_x: i32,
    begin_y: i32,
    end_y: i32,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    transition_outer: f64,
) {
    let dx = bx - ax;
    let dy = by - ay;
    let length_squared = dx * dx + dy * dy;
    let zero = _mm256_setzero_pd();
    let one = _mm256_set1_pd(1.0);
    let vector_ax = _mm256_set1_pd(ax);
    let vector_ay = _mm256_set1_pd(ay);
    let vector_dx = _mm256_set1_pd(dx);
    let vector_dy = _mm256_set1_pd(dy);
    let inverse_length = _mm256_set1_pd(if length_squared > 0.0 {
        1.0 / length_squared
    } else {
        0.0
    });
    let outer = _mm256_set1_pd(transition_outer);
    let mut coverages = AlignedF64x4([0.0; 4]);
    for y in begin_y..end_y {
        let row = &mut alpha[(y as usize) * stride..];
        let py = _mm256_set1_pd((tile_top + y) as f64 + 0.5);
        let mut x = begin_x;
        while x + 4 <= end_x {
            let base = (tile_left + x) as f64;
            let px = _mm256_set_pd(base + 3.5, base + 2.5, base + 1.5, base + 0.5);
            let mut closest_x = vector_ax;
            let mut closest_y = vector_ay;
            if length_squared > 0.0 {
                let mut projection = _mm256_mul_pd(
                    _mm256_add_pd(
                        _mm256_mul_pd(_mm256_sub_pd(px, vector_ax), vector_dx),
                        _mm256_mul_pd(_mm256_sub_pd(py, vector_ay), vector_dy),
                    ),
                    inverse_length,
                );
                projection = _mm256_max_pd(zero, _mm256_min_pd(one, projection));
                closest_x = _mm256_add_pd(vector_ax, _mm256_mul_pd(projection, vector_dx));
                closest_y = _mm256_add_pd(vector_ay, _mm256_mul_pd(projection, vector_dy));
            }
            let distance_x = _mm256_sub_pd(px, closest_x);
            let distance_y = _mm256_sub_pd(py, closest_y);
            let distance = _mm256_sqrt_pd(_mm256_add_pd(
                _mm256_mul_pd(distance_x, distance_x),
                _mm256_mul_pd(distance_y, distance_y),
            ));
            let coverage = _mm256_max_pd(zero, _mm256_min_pd(one, _mm256_sub_pd(outer, distance)));
            // SAFETY: coverages 为 32 字节对齐的 4 个 f64，满足 store_pd 的对齐与长度要求。
            unsafe { _mm256_store_pd(coverages.0.as_mut_ptr(), coverage) };
            for lane in 0..4 {
                let slot = &mut row[(x as usize) + lane];
                *slot = (*slot as i32).max(q_round(coverages.0[lane] * 255.0)) as u8;
            }
            x += 4;
        }
        while x < end_x {
            scalar_pixel(
                row,
                x,
                y,
                tile_left,
                tile_top,
                (ax, ay),
                (dx, dy),
                length_squared,
                transition_outer,
            );
            x += 1;
        }
    }
}

/// AVX2 胶囊线段遮罩光栅化（覆盖度取最大值写入 8 位遮罩）。
///
/// 参数：`alpha` 为遮罩缓冲，`stride` 为行距（字节），`tile_left/tile_top` 为瓦片物理原点，
/// `begin_x..end_x`、`begin_y..end_y` 为瓦片内写入范围，`(ax,ay)-(bx,by)` 为线段，
/// `transition_outer` 为外半径（半径 + 0.5）。
/// 返回：`true` 表示已处理（含空范围），无 AVX2 时返回 `false` 且不写入。
///
/// 示例：`rasterize_capsule_segment(&mut buf, 64, 0, 0, 0, 64, 0, 64, 8.0, 8.0, 40.0, 40.0, 3.5);`
#[allow(clippy::too_many_arguments)]
pub fn rasterize_capsule_segment(
    alpha: &mut [u8],
    stride: usize,
    tile_left: i32,
    tile_top: i32,
    begin_x: i32,
    end_x: i32,
    begin_y: i32,
    end_y: i32,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    transition_outer: f64,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if !crate::avx2::is_avx2_available() {
            return false;
        }
        if check_range(alpha.len(), stride, begin_x, end_x, begin_y, end_y) {
            return true;
        }
        // SAFETY: 已确认 AVX2 可用；写入范围已由 check_range 校验，内部均为带检查的切片索引。
        unsafe {
            capsule_avx2(
                alpha,
                stride,
                tile_left,
                tile_top,
                begin_x,
                end_x,
                begin_y,
                end_y,
                ax,
                ay,
                bx,
                by,
                transition_outer,
            );
        }
        true
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (
            alpha,
            stride,
            tile_left,
            tile_top,
            begin_x,
            end_x,
            begin_y,
            end_y,
            ax,
            ay,
            bx,
            by,
            transition_outer,
        );
        false
    }
}

/// 纯标量胶囊线段遮罩光栅化（与 C++ AVX2 内核尾部循环同式）。
///
/// 参数同 [`rasterize_capsule_segment`]，返回恒为 `true`。
///
/// 示例：`rasterize_capsule_segment_scalar(&mut buf, 64, 0, 0, 0, 64, 0, 64, 8.0, 8.0, 40.0, 40.0, 3.5);`
#[allow(clippy::too_many_arguments)]
pub fn rasterize_capsule_segment_scalar(
    alpha: &mut [u8],
    stride: usize,
    tile_left: i32,
    tile_top: i32,
    begin_x: i32,
    end_x: i32,
    begin_y: i32,
    end_y: i32,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    transition_outer: f64,
) -> bool {
    if check_range(alpha.len(), stride, begin_x, end_x, begin_y, end_y) {
        return true;
    }
    let dx = bx - ax;
    let dy = by - ay;
    let length_squared = dx * dx + dy * dy;
    for y in begin_y..end_y {
        let row = &mut alpha[(y as usize) * stride..];
        for x in begin_x..end_x {
            scalar_pixel(
                row,
                x,
                y,
                tile_left,
                tile_top,
                (ax, ay),
                (dx, dy),
                length_squared,
                transition_outer,
            );
        }
    }
    true
}
