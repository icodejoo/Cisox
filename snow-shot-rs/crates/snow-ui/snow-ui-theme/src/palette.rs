//! 色板生成：移植自 `palette_generate.cpp` 与 `theme_color_utils.cpp` 的色阶映射。

use crate::fast_color::{FastColor, HsvColor};

/// 色相步长。
const HUE_STEP: i32 = 2;
/// 浅色系饱和度步长。
const SATURATION_STEP: f64 = 0.16;
/// 深色系饱和度步长。
const SATURATION_STEP2: f64 = 0.05;
/// 浅色系明度步长。
const BRIGHTNESS_STEP1: f64 = 0.05;
/// 深色系明度步长。
const BRIGHTNESS_STEP2: f64 = 0.15;
/// 主色之前的浅色个数。
const LIGHT_COLOR_COUNT: i32 = 5;
/// 主色之后的深色个数。
const DARK_COLOR_COUNT: i32 = 4;
/// 默认主色（输入无效时回退）。
const FALLBACK_PRIMARY: &str = "#1677ff";
/// 暗色主题默认混合背景。
const DEFAULT_DARK_BACKGROUND: &str = "#141414";

/// 暗色映射表：`(pattern 索引, 混合百分比)`。
const DARK_COLOR_MAP: [(usize, f64); 10] = [
    (7, 15.0),
    (6, 25.0),
    (5, 30.0),
    (5, 45.0),
    (5, 65.0),
    (5, 85.0),
    (4, 90.0),
    (3, 95.0),
    (2, 97.0),
    (1, 98.0),
];

/// 钳制到 `[min, max]`。
fn clamp(value: f64, min_value: f64, max_value: f64) -> f64 {
    if value < min_value {
        return min_value;
    }
    if value > max_value {
        return max_value;
    }
    value
}

/// 第 `i` 阶的色相。
fn get_hue(hsv: &HsvColor, i: i32, light: bool) -> f64 {
    let rounded = hsv.h.round();
    let step = (HUE_STEP * i) as f64;
    let mut hue = if (60.0..=240.0).contains(&rounded) {
        if light {
            rounded - step
        } else {
            rounded + step
        }
    } else if light {
        rounded + step
    } else {
        rounded - step
    };

    if hue < 0.0 {
        hue += 360.0;
    } else if hue >= 360.0 {
        hue -= 360.0;
    }
    hue
}

/// 第 `i` 阶的饱和度。
fn get_saturation(hsv: &HsvColor, i: i32, light: bool) -> f64 {
    if hsv.h == 0.0 && hsv.s == 0.0 {
        return hsv.s;
    }

    let mut saturation = if light {
        hsv.s - (SATURATION_STEP * i as f64)
    } else if i == DARK_COLOR_COUNT {
        hsv.s + SATURATION_STEP
    } else {
        hsv.s + (SATURATION_STEP2 * i as f64)
    };

    if saturation > 1.0 {
        saturation = 1.0;
    }
    if light && i == LIGHT_COLOR_COUNT && saturation > 0.1 {
        saturation = 0.1;
    }
    if saturation < 0.06 {
        saturation = 0.06;
    }
    (saturation * 100.0).round() / 100.0
}

/// 第 `i` 阶的明度。
fn get_value(hsv: &HsvColor, i: i32, light: bool) -> f64 {
    let value = if light {
        hsv.v + (BRIGHTNESS_STEP1 * i as f64)
    } else {
        hsv.v - (BRIGHTNESS_STEP2 * i as f64)
    };
    (clamp(value, 0.0, 1.0) * 100.0).round() / 100.0
}

/// 生成 Ant Design 10 阶色板（十六进制字符串，小写）。
///
/// 参数：`color` 主色（无效时回退 `#1677ff`）；`dark_theme` 是否暗色主题；
/// `background_color` 暗色混合背景（空串或无效时用 `#141414`）。
/// 返回：长度为 10 的色板，第 6 项（下标 5）在亮色下即主色。
///
/// ```rust
/// use snow_ui_theme::palette::generate_palette;
/// let light = generate_palette("#1677ff", false, "");
/// assert_eq!(light[0], "#e6f4ff");
/// let dark = generate_palette("#1677ff", true, "#141414");
/// assert_eq!(dark[5], "#1668dc");
/// ```
pub fn generate_palette(color: &str, dark_theme: bool, background_color: &str) -> Vec<String> {
    let mut primary = FastColor::parse(color);
    if !primary.is_valid() {
        primary = FastColor::parse(FALLBACK_PRIMARY);
    }
    let hsv = primary.to_hsv();

    let mut patterns: Vec<FastColor> = Vec::with_capacity(10);
    for i in (1..=LIGHT_COLOR_COUNT).rev() {
        patterns.push(FastColor::from_hsv(&HsvColor {
            h: get_hue(&hsv, i, true),
            s: get_saturation(&hsv, i, true),
            v: get_value(&hsv, i, true),
            a: hsv.a,
        }));
    }
    patterns.push(primary);
    for i in 1..=DARK_COLOR_COUNT {
        patterns.push(FastColor::from_hsv(&HsvColor {
            h: get_hue(&hsv, i, false),
            s: get_saturation(&hsv, i, false),
            v: get_value(&hsv, i, false),
            a: hsv.a,
        }));
    }

    if dark_theme {
        let bg_input = if background_color.is_empty() {
            DEFAULT_DARK_BACKGROUND
        } else {
            background_color
        };
        let mut bg = FastColor::parse(bg_input);
        if !bg.is_valid() {
            bg = FastColor::parse(DEFAULT_DARK_BACKGROUND);
        }
        return DARK_COLOR_MAP
            .iter()
            .map(|&(index, amount)| bg.mix(&patterns[index], amount).to_hex_string())
            .collect();
    }

    patterns.iter().map(FastColor::to_hex_string).collect()
}

/// 把 10 阶色板映射为 1..=10 号色阶（下标 0 留空串，表示无效）。
///
/// 参数：`colors` 为 [`generate_palette`] 的结果；`dark_theme` 选择映射表。
/// 返回：长度 11 的数组；`colors` 少于 7 项时全部为空串。
///
/// ```rust
/// use snow_ui_theme::palette::{generate_palette, map_palette};
/// let mapped = map_palette(&generate_palette("#1677ff", false, ""), false);
/// assert_eq!(mapped[6], "#1677ff");
/// ```
pub fn map_palette(colors: &[String], dark_theme: bool) -> [String; 11] {
    // 目标色阶 1..=10 依次取自 colors 的下标
    const DEFAULT_MAP: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 4, 5, 6];
    const DARK_MAP: [usize; 10] = [0, 1, 2, 3, 6, 5, 4, 6, 5, 4];

    let mut mapped: [String; 11] = std::array::from_fn(|_| String::new());
    if colors.len() < 7 {
        return mapped;
    }
    let table = if dark_theme { &DARK_MAP } else { &DEFAULT_MAP };
    for (slot, &src) in table.iter().enumerate() {
        mapped[slot + 1] = colors[src].clone();
    }
    mapped
}
