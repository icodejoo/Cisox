//! 标注文本样式模型与属性定义。
//!
//! 包含字体、字号步进调节、文本颜色、排版对齐与样式增量修补（Patch）机制。

use serde::{Deserialize, Serialize};

/// 文本对齐方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextAlignment {
    /// 左对齐（默认）。
    #[default]
    Left,
    /// 居中对齐。
    Center,
    /// 右对齐。
    Right,
}

/// 样式属性位掩码常数，用于选择性修补样式。
pub mod style_mask {
    /// 字体名称属性。
    pub const FONT_FAMILY: u32 = 1 << 0;
    /// 字体字号属性。
    pub const FONT_SIZE: u32 = 1 << 1;
    /// 字体颜色属性。
    pub const COLOR: u32 = 1 << 2;
    /// 粗体属性。
    pub const BOLD: u32 = 1 << 3;
    /// 斜体属性。
    pub const ITALIC: u32 = 1 << 4;
    /// 下划线属性。
    pub const UNDERLINE: u32 = 1 << 5;
    /// 删除线属性。
    pub const STRIKE_THROUGH: u32 = 1 << 6;
    /// 对齐方式属性。
    pub const ALIGNMENT: u32 = 1 << 7;
    /// 行高比例属性。
    pub const LINE_HEIGHT: u32 = 1 << 8;
    /// 自动调整大小属性。
    pub const AUTO_RESIZE: u32 = 1 << 9;
    /// 全部属性掩码。
    pub const ALL: u32 = (1 << 10) - 1;
}

/// 标准字号梯度阶梯。
pub const FONT_SIZE_STEPS: &[f32] = &[
    9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0, 48.0, 64.0, 72.0,
    96.0, 144.0,
];

/// 标注文本样式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasTextStyle {
    /// 字体家族名称。
    pub font_family: String,
    /// 字体字号（像素）。
    pub font_size: f32,
    /// 行高倍率（如 1.25 表示 1.25 倍字号高度）。
    pub line_height: f32,
    /// 文本颜色（RGBA 字节数组）。
    pub color: [u8; 4],
    /// 是否粗体。
    pub bold: bool,
    /// 是否斜体。
    pub italic: bool,
    /// 是否带下划线。
    pub underline: bool,
    /// 是否带删除线。
    pub strike_through: bool,
    /// 文本对齐方式。
    pub alignment: TextAlignment,
    /// 是否根据内容自动伸缩宽度（否则超出换行）。
    pub auto_resize: bool,
}

impl Default for CanvasTextStyle {
    /// 创建默认标注文本样式。
    fn default() -> Self {
        Self {
            font_family: "Segoe UI".to_string(),
            font_size: Self::DEFAULT_FONT_SIZE,
            line_height: 1.25,
            color: [255, 77, 79, 255], // 默认 Ant Design 风格醒目红色
            bold: false,
            italic: false,
            underline: false,
            strike_through: false,
            alignment: TextAlignment::Left,
            auto_resize: true,
        }
    }
}

impl CanvasTextStyle {
    /// 默认字体字号。
    pub const DEFAULT_FONT_SIZE: f32 = 16.0;
    /// 最小允许字号。
    pub const MIN_FONT_SIZE: f32 = 9.0;
    /// 最大允许字号。
    pub const MAX_FONT_SIZE: f32 = 144.0;

    /// 规范化并限制字号在合法范围内。
    ///
    /// # 参数
    /// - `size`：输入的字号。
    ///
    /// # 返回
    /// 限制在 [MIN_FONT_SIZE, MAX_FONT_SIZE] 之间的合法字号。
    pub fn clamp_font_size(size: f32) -> f32 {
        if size.is_nan() {
            Self::DEFAULT_FONT_SIZE
        } else {
            size.clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
        }
    }

    /// 根据属性掩码合并并生成新样式。
    ///
    /// # 参数
    /// - `requested`：请求应用的新样式。
    /// - `mask`：要覆盖生效的属性掩码（参考 `style_mask`）。
    ///
    /// # 返回
    /// 合并补丁后的新样式。
    ///
    /// # 示例
    /// ```
    /// use snow_canvas_text::{CanvasTextStyle, style_mask};
    /// let current = CanvasTextStyle::default();
    /// let mut requested = current.clone();
    /// requested.font_size = 24.0;
    /// let patched = current.patch(&requested, style_mask::FONT_SIZE);
    /// assert_eq!(patched.font_size, 24.0);
    /// ```
    pub fn patch(&self, requested: &Self, mask: u32) -> Self {
        let mut result = self.clone();
        if mask & style_mask::FONT_FAMILY != 0 {
            result.font_family = requested.font_family.clone();
        }
        if mask & style_mask::FONT_SIZE != 0 {
            result.font_size = Self::clamp_font_size(requested.font_size);
        }
        if mask & style_mask::COLOR != 0 {
            result.color = requested.color;
        }
        if mask & style_mask::BOLD != 0 {
            result.bold = requested.bold;
        }
        if mask & style_mask::ITALIC != 0 {
            result.italic = requested.italic;
        }
        if mask & style_mask::UNDERLINE != 0 {
            result.underline = requested.underline;
        }
        if mask & style_mask::STRIKE_THROUGH != 0 {
            result.strike_through = requested.strike_through;
        }
        if mask & style_mask::ALIGNMENT != 0 {
            result.alignment = requested.alignment;
        }
        if mask & style_mask::LINE_HEIGHT != 0 {
            result.line_height = requested.line_height.max(1.0);
        }
        if mask & style_mask::AUTO_RESIZE != 0 {
            result.auto_resize = requested.auto_resize;
        }
        result
    }
}

/// 计算阶梯调整后的字号（放大或缩小一档）。
///
/// # 参数
/// - `current`：当前字号。
/// - `increase`：`true` 表示调大，`false` 表示调小。
///
/// # 返回
/// 调整后的下一个阶梯字号。
///
/// # 示例
/// ```
/// use snow_canvas_text::stepped_font_size;
/// assert_eq!(stepped_font_size(16.0, true), 18.0);
/// assert_eq!(stepped_font_size(16.0, false), 14.0);
/// ```
pub fn stepped_font_size(current: f32, increase: bool) -> f32 {
    let size = CanvasTextStyle::clamp_font_size(current);
    if increase {
        for &step in FONT_SIZE_STEPS {
            if step > size + 0.1 {
                return step;
            }
        }
        CanvasTextStyle::MAX_FONT_SIZE
    } else {
        for &step in FONT_SIZE_STEPS.iter().rev() {
            if step < size - 0.1 {
                return step;
            }
        }
        CanvasTextStyle::MIN_FONT_SIZE
    }
}
