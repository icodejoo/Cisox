//! 令牌与黄金色板测试共用夹具。

/// C++ 黄金样本全文。
pub const GOLDEN: &str = include_str!("../golden/palette_golden.txt");

/// 亮色映射：色阶号 1..=10 对应 10 阶色板下标（theme_color_utils.cpp `toMappedDefault`）。
pub const LIGHT_MAP: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 4, 5, 6];
/// 暗色映射（`toMappedDark`）。
pub const DARK_MAP: [usize; 10] = [0, 1, 2, 3, 6, 5, 4, 6, 5, 4];
