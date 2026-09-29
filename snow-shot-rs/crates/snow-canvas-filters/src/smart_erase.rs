//! 智能擦除接口说明（未移植）。
//!
//! C++ 实现位于 `snow_canvas_smart_erase_algorithm.cpp`（约 880 行），依赖 OpenCV
//! （`opencv2/core`、`opencv2/imgproc`）与 Qt（`QPainter`、`QPainterPathStroker`），
//! 不属于 ADR-10 所述“自包含 AVX2 内核”。移植需先决策：引入 `opencv` 绑定，或用纯 Rust 重写
//! inpaint（需用户确认，见迁移方案）。此处仅约定输入输出，供后续实现对齐。

use crate::image::{AlphaRef, ImageMut};

/// 智能擦除入口：当前未实现，恒返回 `false` 且不改动图像。
///
/// 参数：`image` 预期原地修复的 ARGB32 预乘图像；`mask` 8 位遮罩（非 0 为待擦除）。
/// 返回：`false` 表示尚未实现。
///
/// 示例：`let ok = smart_erase(&mut img.as_mut(), mask); // 目前 ok == false`
pub fn smart_erase(image: &mut ImageMut, mask: AlphaRef) -> bool {
    let _ = (image, mask);
    false
}
