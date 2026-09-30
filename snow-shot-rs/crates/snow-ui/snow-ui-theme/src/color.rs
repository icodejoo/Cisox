//! 令牌用颜色类型：复刻 Qt6 `QColor`(Rgb) 的 16 位通道语义，以便与 C++ 令牌逐值一致。
//!
//! 8 位读取 = 16 位值除以 257 四舍五入（对拍真 Qt 6.11.1）；浮点读取 = `通道 / 65535`（f32）；浮点写入 = `round(值 * 65535)`（f32）。

use crate::fast_color::FastColor;

/// 16 位满量程。
const CHANNEL_MAX: f32 = 65535.0;
/// 8 位扩展到 16 位的倍数（0x101）。
const EXPAND_8_TO_16: u16 = 0x101;
/// `compositeOn` 中视为完全不透明的阈值。
const OPAQUE_THRESHOLD: f32 = 0.999;

/// 复刻 `qRound(float)`。
fn q_round(value: f32) -> i32 {
    if value >= 0.0 {
        (value + 0.5) as i32
    } else {
        (value - 0.5) as i32
    }
}

/// 浮点 0..1 转 16 位通道（同 `QColor::setXxxF`）。
fn f_to_u16(value: f32) -> u16 {
    q_round(value * CHANNEL_MAX).clamp(0, 65535) as u16
}

/// 16 位通道的颜色（含透明度），对应 Qt 的有效 `QColor`。
///
/// `Default` 为全透明黑，仅作为令牌结构“尚未赋值”的占位（C++ 中为无效色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    r: u16,
    g: u16,
    b: u16,
    a: u16,
}

impl Color {
    /// 不透明白色（`Qt::white`）。
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    /// 不透明黑色（`Qt::black`）。
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    /// 全透明黑色（`Qt::transparent`）。
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// 由 8 位 RGB 构造不透明色。
    ///
    /// ```rust
    /// use snow_ui_theme::color::Color;
    /// assert_eq!(Color::rgb(22, 119, 255).name(), "#1677ff");
    /// ```
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as u16 * EXPAND_8_TO_16,
            g: g as u16 * EXPAND_8_TO_16,
            b: b as u16 * EXPAND_8_TO_16,
            a: 65535,
        }
    }

    /// 按 Qt 规则解析 `#RGB` / `#RRGGBB` / `#AARRGGBB`（注意 8 位是 ARGB 顺序）。
    ///
    /// 参数：`text` 颜色文本。返回：成功为 `Some`，否则 `None`。
    ///
    /// ```rust
    /// use snow_ui_theme::color::Color;
    /// assert_eq!(Color::from_hex("#141414").unwrap().name(), "#141414");
    /// assert!(Color::from_hex("nope").is_none());
    /// ```
    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let nibble = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        match hex.len() {
            3 => Some(Self::rgb(nibble(0)? * 17, nibble(1)? * 17, nibble(2)? * 17)),
            6 => Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => {
                let mut c = Self::rgb(byte(2)?, byte(4)?, byte(6)?);
                c.a = byte(0)? as u16 * EXPAND_8_TO_16;
                Some(c)
            }
            _ => None,
        }
    }

    /// 红色通道（8 位）。
    pub fn red(&self) -> u8 {
        narrow_to_u8(self.r)
    }

    /// 绿色通道（8 位）。
    pub fn green(&self) -> u8 {
        narrow_to_u8(self.g)
    }

    /// 蓝色通道（8 位）。
    pub fn blue(&self) -> u8 {
        narrow_to_u8(self.b)
    }

    /// 透明度（8 位，16 位值除以 257 四舍五入）。
    pub fn alpha(&self) -> u8 {
        narrow_to_u8(self.a)
    }

    /// 16 位原始通道 `[r, g, b, a]`，用于精确对拍。
    pub fn raw16(&self) -> [u16; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// 红色通道（浮点 0..1）。
    pub fn red_f(&self) -> f32 {
        self.r as f32 / CHANNEL_MAX
    }

    /// 绿色通道（浮点 0..1）。
    pub fn green_f(&self) -> f32 {
        self.g as f32 / CHANNEL_MAX
    }

    /// 蓝色通道（浮点 0..1）。
    pub fn blue_f(&self) -> f32 {
        self.b as f32 / CHANNEL_MAX
    }

    /// 透明度（浮点 0..1）。
    pub fn alpha_f(&self) -> f32 {
        self.a as f32 / CHANNEL_MAX
    }

    /// 返回替换透明度后的颜色（同 `setAlphaF`，浮点写入 16 位）。
    ///
    /// ```rust
    /// use snow_ui_theme::color::Color;
    /// assert_eq!(Color::BLACK.with_alpha_f(0.88).alpha(), 224);
    /// ```
    pub fn with_alpha_f(&self, alpha: f32) -> Self {
        Self {
            a: f_to_u16(alpha),
            ..*self
        }
    }

    /// 返回强制不透明（`setAlpha(255)`）的颜色。
    pub fn opaque(&self) -> Self {
        Self { a: 65535, ..*self }
    }

    /// 输出 `#rrggbb`（同 `name(HexRgb)`，忽略透明度）。
    ///
    /// ```rust
    /// use snow_ui_theme::color::Color;
    /// assert_eq!(Color::WHITE.name(), "#ffffff");
    /// ```
    pub fn name(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red(), self.green(), self.blue())
    }

    /// 转为 `FastColor`（对应 C++ `toFastColor`）。
    pub fn to_fast_color(&self) -> FastColor {
        FastColor::from_rgba(
            self.red() as i32,
            self.green() as i32,
            self.blue() as i32,
            self.alpha_f() as f64,
        )
    }
}

/// 给颜色设定透明度（对应 `alphaColor`，alpha 先钳制到 0..1）。
///
/// 参数：`base` 基色；`alpha` 透明度。返回：带透明度的颜色。
///
/// ```rust
/// use snow_ui_theme::color::{alpha_color, Color};
/// assert_eq!(alpha_color(Color::BLACK, 0.45).alpha(), 115);
/// ```
pub fn alpha_color(base: Color, alpha: f64) -> Color {
    base.with_alpha_f(alpha.clamp(0.0, 1.0) as f32)
}

/// 把前景叠加到背景上得到不透明色（对应 `compositeOn`）。
///
/// 参数：`foreground` 前景；`background` 背景；`opacity_scale` 前景透明度缩放。
/// 返回：叠加后的不透明色。
///
/// ```rust
/// use snow_ui_theme::color::{composite_on, Color};
/// let c = composite_on(Color::WHITE, Color::BLACK, 1.0);
/// assert_eq!(c.name(), "#ffffff");
/// ```
pub fn composite_on(foreground: Color, background: Color, opacity_scale: f64) -> Color {
    let alpha = ((foreground.alpha_f() as f64 * opacity_scale) as f32).clamp(0.0, 1.0);
    if alpha >= OPAQUE_THRESHOLD {
        return foreground.opaque();
    }
    let mix = |f: f32, b: f32| f_to_u16(f * alpha + b * (1.0 - alpha));
    Color {
        r: mix(foreground.red_f(), background.red_f()),
        g: mix(foreground.green_f(), background.green_f()),
        b: mix(foreground.blue_f(), background.blue_f()),
        a: 65535,
    }
}

/// 以 `FastColor` 算法调暗/调亮（对应 `solidColor`）。
///
/// 参数：`base` 基色；`amount_percent` 百分点；`raise_lightness` 为真时调亮。
/// 返回：结果色（经十六进制字符串回读，与 C++ 一致）。
///
/// ```rust
/// use snow_ui_theme::color::{solid_color, Color};
/// assert_eq!(solid_color(Color::WHITE, 4.0, false).name(), "#f5f5f5");
/// ```
pub fn solid_color(base: Color, amount_percent: f64, raise_lightness: bool) -> Color {
    let fast = base.to_fast_color();
    let adjusted = if raise_lightness {
        fast.lighten(amount_percent)
    } else {
        fast.darken(amount_percent)
    };
    Color::from_hex(&adjusted.to_hex_string()).unwrap_or(base)
}

/// 混合两色（对应 `mixColor`，两色均有效）。
///
/// 参数：`first` 起点色；`second` 目标色；`amount_percent` 目标权重。返回：混合色。
///
/// ```rust
/// use snow_ui_theme::color::{mix_color, Color};
/// assert_eq!(mix_color(Color::BLACK, Color::WHITE, 50.0).name(), "#808080");
/// ```
pub fn mix_color(first: Color, second: Color, amount_percent: f64) -> Color {
    let mixed = first
        .to_fast_color()
        .mix(&second.to_fast_color(), amount_percent);
    Color::from_hex(&mixed.to_hex_string()).unwrap_or(first)
}

/// 16 位通道转 8 位：除以 257 并四舍五入（与真 Qt 输出一致）。
fn narrow_to_u8(value: u16) -> u8 {
    ((value as u32 + EXPAND_8_TO_16 as u32 / 2) / EXPAND_8_TO_16 as u32) as u8
}

#[cfg(test)]
mod narrow_tests {
    use super::*;

    /// 8 位读取对拍真 Qt：0.88 -> 224、0.95 -> 242，且 8 位往返不失真。
    #[test]
    fn narrowing_matches_real_qt() {
        assert_eq!(Color::BLACK.with_alpha_f(0.88).alpha(), 224);
        assert_eq!(Color::BLACK.with_alpha_f(0.95).alpha(), 242);
        for byte in 0..=255u16 {
            assert_eq!(narrow_to_u8(byte * EXPAND_8_TO_16) as u16, byte);
        }
        assert_eq!(narrow_to_u8(u16::MAX), 255);
    }
}
