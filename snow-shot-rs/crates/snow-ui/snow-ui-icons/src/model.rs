//! 图标模型：主题、颜色、引用与调色板解析。

use crate::assets::find_template;

/// 图标主题，对应 Ant Design 的三套风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IconTheme {
    /// 线框风格（单色）。
    Outlined,
    /// 实心风格（单色）。
    Filled,
    /// 双色风格（主色 + 次色）。
    TwoTone,
}

impl IconTheme {
    /// 全部主题。
    pub const ALL: [IconTheme; 3] = [IconTheme::Outlined, IconTheme::Filled, IconTheme::TwoTone];

    /// 资源目录名（同时是路径前缀）。
    pub fn dir(self) -> &'static str {
        match self {
            IconTheme::Outlined => "outlined",
            IconTheme::Filled => "filled",
            IconTheme::TwoTone => "twotone",
        }
    }

    /// 由目录名解析主题；未知名称返回 `None`。
    pub fn from_dir(dir: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.dir() == dir)
    }

    /// 是否为双色主题。
    pub fn is_two_tone(self) -> bool {
        self == IconTheme::TwoTone
    }
}

/// 8 位 RGBA 颜色（非预乘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// 红。
    pub r: u8,
    /// 绿。
    pub g: u8,
    /// 蓝。
    pub b: u8,
    /// 透明度，255 为不透明。
    pub a: u8,
}

impl Rgba {
    /// 构造不透明颜色。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// 构造带透明度的颜色。
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// 解析 `#RRGGBB` 或 `#RRGGBBAA`。
    ///
    /// # 返回
    /// 格式非法时为 `None`。
    ///
    /// # 示例
    /// ```
    /// use snow_ui_icons::Rgba;
    /// assert_eq!(Rgba::from_hex("#1677FF"), Some(Rgba::rgb(0x16, 0x77, 0xFF)));
    /// ```
    pub fn from_hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        let a = if h.len() == 8 { byte(6)? } else { 255 };
        Some(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a,
        })
    }

    /// 输出 `#rrggbb`（忽略透明度，与 C++ `QColor::name(HexRgb)` 一致）。
    pub fn hex_rgb(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// 打包为 u32，供缓存键使用。
    pub(crate) fn packed(self) -> u32 {
        u32::from_be_bytes([self.a, self.r, self.g, self.b])
    }
}

/// 图标着色：主色/副色均可缺省，缺省时由 [`IconPalette`] 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct IconColors {
    /// 主色覆盖。
    pub primary: Option<Rgba>,
    /// 副色覆盖（仅双色图标使用）。
    pub secondary: Option<Rgba>,
}

impl IconColors {
    /// 仅指定主色。
    ///
    /// # 示例
    /// ```
    /// use snow_ui_icons::{IconColors, Rgba};
    /// let c = IconColors::primary(Rgba::rgb(255, 0, 0));
    /// assert!(c.secondary.is_none());
    /// ```
    pub const fn primary(color: Rgba) -> Self {
        Self {
            primary: Some(color),
            secondary: None,
        }
    }

    /// 同时指定主色与副色。
    pub const fn two_tone(primary: Rgba, secondary: Rgba) -> Self {
        Self {
            primary: Some(primary),
            secondary: Some(secondary),
        }
    }

    /// 返回替换主色后的副本。
    pub const fn with_primary(mut self, color: Rgba) -> Self {
        self.primary = Some(color);
        self
    }

    /// 返回替换副色后的副本。
    pub const fn with_secondary(mut self, color: Rgba) -> Self {
        self.secondary = Some(color);
        self
    }

    /// 是否没有任何覆盖。
    pub const fn is_empty(&self) -> bool {
        self.primary.is_none() && self.secondary.is_none()
    }
}

/// 应用级调色板，提供未覆盖时的默认色（默认值与 C++ `IconPalette` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconPalette {
    /// 单色图标默认色。
    pub text: Rgba,
    /// 禁用态图标色。
    pub text_disabled: Rgba,
    /// 双色图标默认主色。
    pub primary: Rgba,
    /// 双色图标默认副色。
    pub two_tone_secondary: Rgba,
}

impl Default for IconPalette {
    fn default() -> Self {
        Self {
            text: Rgba::rgb(0x1F, 0x1F, 0x1F),
            text_disabled: Rgba::rgb(0xBF, 0xBF, 0xBF),
            primary: Rgba::rgb(0x16, 0x77, 0xFF),
            two_tone_secondary: Rgba::rgb(0xE6, 0xF4, 0xFF),
        }
    }
}

/// 解析后的最终颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedColors {
    /// 主色。
    pub primary: Rgba,
    /// 副色（单色图标不使用）。
    pub secondary: Rgba,
}

impl IconPalette {
    /// 按 C++ 规则解析颜色：覆盖 > 调色板默认；双色图标只覆盖主色时副色由主色派生。
    ///
    /// # 参数
    /// - `theme`: 图标主题。
    /// - `colors`: 调用方覆盖。
    /// - `disabled`: 是否禁用态。
    pub fn resolve(&self, theme: IconTheme, colors: &IconColors, disabled: bool) -> ResolvedColors {
        let base_primary = match (theme.is_two_tone(), disabled) {
            (_, true) => self.text_disabled,
            (true, false) => self.primary,
            (false, false) => self.text,
        };
        let base_secondary = if disabled {
            derive_secondary(self.text_disabled)
        } else {
            self.two_tone_secondary
        };
        let primary = colors.primary.unwrap_or(base_primary);
        let mut secondary = colors.secondary.unwrap_or(base_secondary);
        if theme.is_two_tone() && colors.primary.is_some() && colors.secondary.is_none() {
            secondary = derive_secondary(primary);
        }
        ResolvedColors { primary, secondary }
    }
}

/// 由主色派生浅色副色：饱和度 x0.22（下限 8），亮度向白色靠近 82%（上限 245）。
/// 数值按 Qt 的 0~255 整型 HSL 口径计算，与 C++ `deriveSecondary` 同式。
pub(crate) fn derive_secondary(primary: Rgba) -> Rgba {
    let (h, s, l) = rgb_to_hsl(primary);
    let s255 = (s * 255.0).round();
    let l255 = (l * 255.0).round();
    let s2 = (s255 * 0.22).round().max(8.0) / 255.0;
    let l2 = (l255 + (255.0 - l255) * 0.82).round().min(245.0) / 255.0;
    let (r, g, b) = hsl_to_rgb(h, s2, l2);
    Rgba {
        r,
        g,
        b,
        a: primary.a,
    }
}

/// RGB 转 HSL，h 为角度，s/l 为 0~1。
fn rgb_to_hsl(c: Rgba) -> (f64, f64, f64) {
    let (r, g, b) = (c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

/// HSL 转 RGB（8 位）。
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (q(r), q(g), q(b))
}

/// 缩放适配方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IconFit {
    /// 保持宽高比并居中（默认）。
    #[default]
    Contain,
    /// 拉伸铺满。
    Stretch,
}

/// 图标引用：主题 + 名称 + 着色覆盖。名称不存在时渲染会降级为占位图标。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IconRef {
    /// 主题。
    pub theme: IconTheme,
    /// kebab-case 名称，如 `account-book`。
    pub name: String,
    /// 着色覆盖。
    pub colors: IconColors,
}

impl IconRef {
    /// 构造引用（不校验是否存在，见 [`IconRef::exists`]）。
    ///
    /// # 示例
    /// ```
    /// use snow_ui_icons::{IconRef, IconTheme};
    /// assert!(IconRef::new(IconTheme::Outlined, "setting").exists());
    /// assert!(!IconRef::new(IconTheme::Outlined, "no-such-icon").exists());
    /// ```
    pub fn new(theme: IconTheme, name: impl Into<String>) -> Self {
        Self {
            theme,
            name: name.into(),
            colors: IconColors::default(),
        }
    }

    /// 由 `theme/name` 路径解析，如 `outlined/setting`；格式非法返回 `None`。
    pub fn from_path(path: &str) -> Option<Self> {
        let (dir, name) = path.split_once('/')?;
        Some(Self::new(IconTheme::from_dir(dir)?, name))
    }

    /// 返回替换着色后的副本。
    pub fn with_colors(mut self, colors: IconColors) -> Self {
        self.colors = colors;
        self
    }

    /// 内置资源里是否存在该图标。
    pub fn exists(&self) -> bool {
        find_template(self.theme, &self.name).is_some()
    }
}
