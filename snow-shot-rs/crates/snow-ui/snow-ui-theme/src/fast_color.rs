//! 快速色彩模块：移植自 `fast_color_lite.cpp`，行为与 C++ 逐值一致。

/// HSV 色彩：色相 0..360，饱和度/明度/透明度 0..1。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HsvColor {
    /// 色相（度）
    pub h: f64,
    /// 饱和度
    pub s: f64,
    /// 明度
    pub v: f64,
    /// 透明度
    pub a: f64,
}

/// 8 位整数通道 + 浮点透明度的颜色，对应 C++ 的 `FastColorLite`。
#[derive(Clone, Debug, PartialEq)]
pub struct FastColor {
    r: i32,
    g: i32,
    b: i32,
    a: f64,
    valid: bool,
}

impl Default for FastColor {
    /// 等同 [`FastColor::new`]。
    fn default() -> Self {
        Self::new()
    }
}

/// 保留两位小数（四舍五入）。
fn round_to_two(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

impl FastColor {
    /// 默认构造：不透明黑色，状态有效。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::new().to_hex_string(), "#000000");
    /// ```
    pub fn new() -> Self {
        Self {
            r: 0,
            g: 0,
            b: 0,
            a: 1.0,
            valid: true,
        }
    }

    /// 解析颜色字符串，支持 `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa` 与 `rgb()`/`rgba()`。
    ///
    /// 参数：`input` 颜色字符串。返回：解析结果，失败时 `is_valid()` 为 false。
    /// 与 Qt 版唯一偏差：带正负号的十六进制片段视为无效。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// let c = FastColor::parse("#1677ff");
    /// assert!(c.is_valid());
    /// assert_eq!((c.red(), c.green(), c.blue()), (0x16, 0x77, 0xff));
    /// ```
    pub fn parse(input: &str) -> Self {
        let mut color = Self::new();
        let trimmed = input.trim();
        if trimmed.is_empty() {
            color.valid = false;
            return color;
        }
        color.valid = color.parse_hex(trimmed) || color.parse_rgb(trimmed);
        color
    }

    /// 由通道值构造，通道自动钳制到合法范围。
    ///
    /// 参数：`r/g/b` 0..255，`a` 0..1。返回：有效颜色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::from_rgba(300, -5, 0, 1.0).to_hex_string(), "#ff0000");
    /// ```
    pub fn from_rgba(r: i32, g: i32, b: i32, a: f64) -> Self {
        Self {
            r: Self::clamp_channel(r),
            g: Self::clamp_channel(g),
            b: Self::clamp_channel(b),
            a: Self::clamp_unit(a),
            valid: true,
        }
    }

    /// 由 HSV 构造。
    ///
    /// 参数：`hsv` HSV 颜色。返回：对应 RGB 颜色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::{FastColor, HsvColor};
    /// let c = FastColor::from_hsv(&HsvColor { h: 120.0, s: 1.0, v: 1.0, a: 1.0 });
    /// assert_eq!(c.to_hex_string(), "#00ff00");
    /// ```
    pub fn from_hsv(hsv: &HsvColor) -> Self {
        let mut h = hsv.h % 360.0;
        if h < 0.0 {
            h += 360.0;
        }
        let s = Self::clamp_unit(hsv.s);
        let v = Self::clamp_unit(hsv.v);

        let mut r = (v * 255.0).round() as i32;
        let mut g = r;
        let mut b = r;

        if s > 0.0 {
            let hh = h / 60.0;
            let i = hh.floor() as i32;
            let ff = hh - i as f64;
            let p = (v * (1.0 - s) * 255.0).round() as i32;
            let q = (v * (1.0 - (s * ff)) * 255.0).round() as i32;
            let t = (v * (1.0 - (s * (1.0 - ff))) * 255.0).round() as i32;
            match i {
                0 => {
                    g = t;
                    b = p;
                }
                1 => {
                    r = q;
                    b = p;
                }
                2 => {
                    r = p;
                    b = t;
                }
                3 => {
                    r = p;
                    g = q;
                }
                4 => {
                    r = t;
                    g = p;
                }
                _ => {
                    g = p;
                    b = q;
                }
            }
        }
        Self::from_rgba(r, g, b, hsv.a)
    }

    /// 颜色是否有效（解析成功）。
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// 红色通道 0..255。
    pub fn red(&self) -> i32 {
        self.r
    }

    /// 绿色通道 0..255。
    pub fn green(&self) -> i32 {
        self.g
    }

    /// 蓝色通道 0..255。
    pub fn blue(&self) -> i32 {
        self.b
    }

    /// 透明度 0..1。
    pub fn alpha(&self) -> f64 {
        self.a
    }

    /// 返回替换透明度后的新颜色。
    ///
    /// 参数：`alpha` 新透明度。返回：新颜色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::new().set_alpha(0.5).alpha(), 0.5);
    /// ```
    pub fn set_alpha(&self, alpha: f64) -> Self {
        Self::from_rgba(self.r, self.g, self.b, Self::clamp_unit(alpha))
    }

    /// 转为 HSV（色相会四舍五入为整数）。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::parse("#ff0000").to_hsv().h, 0.0);
    /// ```
    pub fn to_hsv(&self) -> HsvColor {
        let max_channel = self.r.max(self.g).max(self.b);
        let min_channel = self.r.min(self.g).min(self.b);
        let delta = max_channel - min_channel;

        let mut h = 0.0;
        if delta != 0 {
            let d = delta as f64;
            if self.r == max_channel {
                h = 60.0
                    * (((self.g - self.b) as f64 / d) + if self.g < self.b { 6.0 } else { 0.0 });
            } else if self.g == max_channel {
                h = 60.0 * (((self.b - self.r) as f64 / d) + 2.0);
            } else {
                h = 60.0 * (((self.r - self.g) as f64 / d) + 4.0);
            }
        }
        h = h.round();

        let mut s = 0.0;
        if max_channel != 0 {
            s = delta as f64 / max_channel as f64;
        }
        let v = max_channel as f64 / 255.0;

        HsvColor { h, s, v, a: self.a }
    }

    /// 调暗（沿用 C++ 的实现：以 HSV 饱和度参与 HSL 转换）。
    ///
    /// 参数：`amount_percent` 亮度降低的百分点。返回：新颜色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// let c = FastColor::parse("#ffffff").darken(4.0);
    /// assert_eq!(c.to_hex_string(), "#f5f5f5");
    /// ```
    pub fn darken(&self, amount_percent: f64) -> Self {
        let hsv = self.to_hsv();
        let max_channel = self.r.max(self.g).max(self.b);
        let min_channel = self.r.min(self.g).min(self.b);

        let mut lightness = (max_channel + min_channel) as f64 / 510.0;
        lightness -= amount_percent / 100.0;
        lightness = Self::clamp_unit(lightness);

        Self::from_hsl(hsv.h, hsv.s, lightness, self.a)
    }

    /// 调亮（实现同 [`FastColor::darken`]，方向相反）。
    ///
    /// 参数：`amount_percent` 亮度提升的百分点。返回：新颜色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// let c = FastColor::parse("#000000").lighten(8.0);
    /// assert_eq!(c.to_hex_string(), "#141414");
    /// ```
    pub fn lighten(&self, amount_percent: f64) -> Self {
        let hsv = self.to_hsv();
        let max_channel = self.r.max(self.g).max(self.b);
        let min_channel = self.r.min(self.g).min(self.b);

        let mut lightness = (max_channel + min_channel) as f64 / 510.0;
        lightness += amount_percent / 100.0;
        lightness = Self::clamp_unit(lightness);

        Self::from_hsl(hsv.h, hsv.s, lightness, self.a)
    }

    /// 与另一颜色线性混合。
    ///
    /// 参数：`other` 目标色；`amount_percent` 目标色权重 0..100。返回：混合色。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// let m = FastColor::parse("#000000").mix(&FastColor::parse("#ffffff"), 50.0);
    /// assert_eq!(m.to_hex_string(), "#808080");
    /// ```
    pub fn mix(&self, other: &Self, amount_percent: f64) -> Self {
        let p = Self::clamp(amount_percent / 100.0, 0.0, 1.0);
        let r = ((other.r - self.r) as f64 * p + self.r as f64).round() as i32;
        let g = ((other.g - self.g) as f64 * p + self.g as f64).round() as i32;
        let b = ((other.b - self.b) as f64 * p + self.b as f64).round() as i32;
        let a = round_to_two((other.a - self.a) * p + self.a);
        Self::from_rgba(r, g, b, a)
    }

    /// 输出小写十六进制；`0 <= a < 1` 时追加两位 alpha。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::from_rgba(255, 0, 0, 0.5).to_hex_string(), "#ff000080");
    /// ```
    pub fn to_hex_string(&self) -> String {
        let mut hex = format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b);
        if self.a >= 0.0 && self.a < 1.0 {
            let alpha = (self.a * 255.0).round() as i32;
            hex.push_str(&format!("{alpha:02x}"));
        }
        hex
    }

    /// 输出 `rgb(r,g,b)` 或 `rgba(r,g,b,a)`。
    ///
    /// ```rust
    /// use snow_ui_theme::fast_color::FastColor;
    /// assert_eq!(FastColor::from_rgba(1, 2, 3, 0.5).to_rgb_string(), "rgba(1,2,3,0.5)");
    /// ```
    pub fn to_rgb_string(&self) -> String {
        if self.a >= 1.0 {
            format!("rgb({},{},{})", self.r, self.g, self.b)
        } else {
            format!(
                "rgba({},{},{},{})",
                self.r,
                self.g,
                self.b,
                Self::format_alpha(self.a)
            )
        }
    }

    /// 解析十六进制写法，成功则写入自身。
    fn parse_hex(&mut self, input: &str) -> bool {
        let hex = input.strip_prefix('#').unwrap_or(input);
        let chars: Vec<char> = hex.chars().collect();

        // 仅接受全 ASCII 十六进制数字的片段
        let to_int = |s: String| -> Option<i32> {
            if s.chars().all(|c| c.is_ascii_hexdigit()) {
                i32::from_str_radix(&s, 16).ok()
            } else {
                None
            }
        };
        let single = |i: usize| to_int(format!("{}{}", chars[i], chars[i]));
        let pair = |i: usize| to_int(format!("{}{}", chars[i], chars[i + 1]));

        let (r, g, b, alpha) = match chars.len() {
            3 | 4 => {
                let alpha = if chars.len() == 4 {
                    Some(single(3))
                } else {
                    None
                };
                (single(0), single(1), single(2), alpha)
            }
            6 | 8 => {
                let alpha = if chars.len() == 8 {
                    Some(pair(6))
                } else {
                    None
                };
                (pair(0), pair(2), pair(4), alpha)
            }
            _ => return false,
        };
        let (Some(r), Some(g), Some(b)) = (r, g, b) else {
            return false;
        };
        let a = match alpha {
            None => 1.0,
            Some(Some(v)) => v as f64 / 255.0,
            Some(None) => return false,
        };
        self.r = r;
        self.g = g;
        self.b = b;
        self.a = Self::clamp_unit(a);
        true
    }

    /// 解析 `rgb()`/`rgba()` 写法（手写等价于原正则），成功则写入自身。
    fn parse_rgb(&mut self, input: &str) -> bool {
        let s = input.trim();
        let lower = s.to_ascii_lowercase();
        let inside = if lower.starts_with("rgba(") && lower.ends_with(')') {
            &s[5..s.len() - 1]
        } else if lower.starts_with("rgb(") && lower.ends_with(')') {
            &s[4..s.len() - 1]
        } else {
            return false;
        };

        let values = Self::scan_numbers(inside);
        if values.len() < 3 {
            return false;
        }

        let to_channel = |v: &str| -> i32 {
            match v.strip_suffix('%') {
                Some(p) => (p.parse::<f64>().unwrap_or(0.0) / 100.0 * 255.0).round() as i32,
                None => v.parse::<f64>().unwrap_or(0.0).round() as i32,
            }
        };
        let to_alpha = |v: &str| -> f64 {
            match v.strip_suffix('%') {
                Some(p) => p.parse::<f64>().unwrap_or(0.0) / 100.0,
                None => v.parse::<f64>().unwrap_or(0.0),
            }
        };

        self.r = Self::clamp_channel(to_channel(&values[0]));
        self.g = Self::clamp_channel(to_channel(&values[1]));
        self.b = Self::clamp_channel(to_channel(&values[2]));
        self.a = if values.len() >= 4 {
            Self::clamp_unit(to_alpha(&values[3]))
        } else {
            1.0
        };
        true
    }

    /// 等价于全局匹配正则 `\d*\.?\d+%?`，返回全部匹配片段。
    fn scan_numbers(inside: &str) -> Vec<String> {
        let chars: Vec<char> = inside.chars().collect();
        let digits_at = |from: usize| {
            chars[from.min(chars.len())..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count()
        };
        let mut values = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let a = digits_at(i);
            let mut len = 0;
            if i + a < chars.len() && chars[i + a] == '.' {
                let b = digits_at(i + a + 1);
                if b >= 1 {
                    len = a + 1 + b;
                } else if a >= 1 {
                    len = a;
                }
            } else if a >= 1 {
                len = a;
            }
            if len == 0 {
                i += 1;
                continue;
            }
            if i + len < chars.len() && chars[i + len] == '%' {
                len += 1;
            }
            values.push(chars[i..i + len].iter().collect());
            i += len;
        }
        values
    }

    /// 通道钳制到 0..255。
    fn clamp_channel(value: i32) -> i32 {
        value.clamp(0, 255)
    }

    /// 数值钳制到 0..1。
    fn clamp_unit(value: f64) -> f64 {
        Self::clamp(value, 0.0, 1.0)
    }

    /// 与 C++ 一致的钳制（不使用 f64::clamp，避免 NaN 时 panic 差异）。
    fn clamp(value: f64, min_value: f64, max_value: f64) -> f64 {
        if value < min_value {
            return min_value;
        }
        if value > max_value {
            return max_value;
        }
        value
    }

    /// 格式化 alpha：保留两位并去掉多余的 0 与小数点。
    fn format_alpha(alpha: f64) -> String {
        let mut result = format!("{:.2}", round_to_two(alpha));
        while result.ends_with('0') {
            result.pop();
        }
        if result.ends_with('.') {
            result.pop();
        }
        result
    }

    /// HSL 转 RGB（与 C++ 分段公式一致）。
    fn from_hsl(h: f64, s: f64, l: f64, a: f64) -> Self {
        let mut h = h % 360.0;
        if h < 0.0 {
            h += 360.0;
        }
        let s = Self::clamp_unit(s);
        let l = Self::clamp_unit(l);

        if s <= 0.0 {
            let rgb = (l * 255.0).round() as i32;
            return Self::from_rgba(rgb, rgb, rgb, a);
        }

        let hue_prime = h / 60.0;
        let chroma = (1.0 - ((2.0 * l) - 1.0).abs()) * s;
        let second = chroma * (1.0 - (hue_prime % 2.0 - 1.0).abs());

        let (r, g, b) = if (0.0..1.0).contains(&hue_prime) {
            (chroma, second, 0.0)
        } else if (1.0..2.0).contains(&hue_prime) {
            (second, chroma, 0.0)
        } else if (2.0..3.0).contains(&hue_prime) {
            (0.0, chroma, second)
        } else if (3.0..4.0).contains(&hue_prime) {
            (0.0, second, chroma)
        } else if (4.0..5.0).contains(&hue_prime) {
            (second, 0.0, chroma)
        } else {
            (chroma, 0.0, second)
        };

        let m = l - chroma / 2.0;
        Self::from_rgba(
            ((r + m) * 255.0).round() as i32,
            ((g + m) * 255.0).round() as i32,
            ((b + m) * 255.0).round() as i32,
            a,
        )
    }
}
