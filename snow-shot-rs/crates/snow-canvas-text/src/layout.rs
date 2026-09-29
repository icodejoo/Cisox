//! 标注文本排版、测量、坐标命中测试与几何计算。
//!
//! 负责多行断行折行、对齐排布、光标矩形生成、选区高亮多边形矩形集合计算，
//! 以及将画布坐标点反向映射为字符字节偏移。

use crate::style::{CanvasTextStyle, TextAlignment};
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// 二维点坐标（浮点像素）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextPoint {
    /// 横坐标。
    pub x: f32,
    /// 纵坐标。
    pub y: f32,
}

impl TextPoint {
    /// 构造点。
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 二维尺寸（浮点像素）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextSize {
    /// 宽度。
    pub width: f32,
    /// 高度。
    pub height: f32,
}

impl TextSize {
    /// 构造尺寸。
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// 二维矩形区域。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextRect {
    /// 矩形原点（左上角）。
    pub origin: TextPoint,
    /// 矩形尺寸。
    pub size: TextSize,
}

impl TextRect {
    /// 构造矩形。
    pub const fn new(origin: TextPoint, size: TextSize) -> Self {
        Self { origin, size }
    }

    /// 构造矩形（由分量）。
    pub const fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: TextPoint::new(x, y),
            size: TextSize::new(width, height),
        }
    }

    /// 左边界。
    pub fn left(&self) -> f32 {
        self.origin.x
    }

    /// 上边界。
    pub fn top(&self) -> f32 {
        self.origin.y
    }

    /// 右边界。
    pub fn right(&self) -> f32 {
        self.origin.x + self.size.width
    }

    /// 下边界。
    pub fn bottom(&self) -> f32 {
        self.origin.y + self.size.height
    }

    /// 判定点是否在矩形内。
    pub fn contains(&self, p: TextPoint) -> bool {
        p.x >= self.left() && p.x <= self.right() && p.y >= self.top() && p.y <= self.bottom()
    }
}

/// 单个字符在排版中的几何度量。
#[derive(Debug, Clone, PartialEq)]
pub struct TextCharMetric {
    /// 该字符在原文本中的起始字节偏移。
    pub byte_offset: usize,
    /// 该字符的字节长度。
    pub byte_len: usize,
    /// 字符占据的矩形区域。
    pub bounds: TextRect,
}

/// 单行排版结果。
#[derive(Debug, Clone, PartialEq)]
pub struct TextLineLayout {
    /// 行索引（从 0 开始）。
    pub line_index: usize,
    /// 在全文中的起始字节偏移。
    pub start_byte: usize,
    /// 在全文中的结束字节偏移。
    pub end_byte: usize,
    /// 这一行的包围矩形。
    pub bounds: TextRect,
    /// 行文本内容。
    pub text: String,
    /// 行内各个字符的几何度量。
    pub chars: Vec<TextCharMetric>,
}

/// 完整文本排版结果。
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayoutResult {
    /// 总布局区域尺寸（含自动伸缩外边距安全缓冲）。
    pub layout_size: TextSize,
    /// 墨水文本内容实际占据的包围盒尺寸。
    pub content_size: TextSize,
    /// 各行排版度量。
    pub lines: Vec<TextLineLayout>,
    /// 排版所用的样式。
    pub style: CanvasTextStyle,
}

impl TextLayoutResult {
    /// 根据画布局部坐标命中测试最近的字符字节偏移。
    ///
    /// # 参数
    /// - `point`：局部坐标点。
    ///
    /// # 返回
    /// 最贴合的字节偏移位置（0..=text.len()）。
    ///
    /// # 示例
    /// ```
    /// use snow_canvas_text::{CanvasTextStyle, TextLayoutResult, TextPoint};
    /// let style = CanvasTextStyle::default();
    /// let layout = TextLayoutResult::layout_text("Hello", &style, None);
    /// let pos = layout.hit_test(TextPoint::new(0.0, 5.0));
    /// assert_eq!(pos, 0);
    /// ```
    pub fn hit_test(&self, point: TextPoint) -> usize {
        if self.lines.is_empty() {
            return 0;
        }

        // 垂直落点高于第一行，归于开头
        if point.y < self.lines[0].bounds.top() {
            return self.lines[0].start_byte;
        }

        // 垂直落点低于最后一行，归于末尾
        let last_line = &self.lines[self.lines.len() - 1];
        if point.y > last_line.bounds.bottom() {
            return last_line.end_byte;
        }

        // 寻找命中的行
        let matched_line = self
            .lines
            .iter()
            .find(|line| point.y >= line.bounds.top() && point.y <= line.bounds.bottom())
            .unwrap_or(last_line);

        // 水平落点在行左侧
        if point.x <= matched_line.bounds.left() {
            return matched_line.start_byte;
        }
        // 水平落点在行右侧
        if point.x >= matched_line.bounds.right() {
            return matched_line.end_byte;
        }

        // 行内各字符水平判定
        for c in &matched_line.chars {
            let mid_x = c.bounds.left() + c.bounds.size.width * 0.5;
            if point.x <= mid_x {
                return c.byte_offset;
            } else if point.x <= c.bounds.right() {
                return c.byte_offset + c.byte_len;
            }
        }

        matched_line.end_byte
    }

    /// 计算给定字节偏移处的光标闪烁矩形。
    ///
    /// # 参数
    /// - `byte_offset`：目标光标字节偏移。
    /// - `cursor_width`：光标线宽（通常 2.0 像素）。
    ///
    /// # 返回
    /// 光标线占据的包围矩形。
    pub fn cursor_rect(&self, byte_offset: usize, cursor_width: f32) -> TextRect {
        let line_height = self.style.font_size * self.style.line_height;
        if self.lines.is_empty() {
            return TextRect::from_xywh(0.0, 0.0, cursor_width, line_height);
        }

        // 查找对应字节属于哪一行
        for line in &self.lines {
            if byte_offset >= line.start_byte && byte_offset <= line.end_byte {
                let x = if byte_offset == line.start_byte {
                    line.bounds.left()
                } else if byte_offset == line.end_byte {
                    line.bounds.right()
                } else {
                    let mut found_x = line.bounds.left();
                    for c in &line.chars {
                        if byte_offset == c.byte_offset {
                            found_x = c.bounds.left();
                            break;
                        } else if byte_offset == c.byte_offset + c.byte_len {
                            found_x = c.bounds.right();
                            break;
                        }
                    }
                    found_x
                };
                return TextRect::from_xywh(x, line.bounds.top(), cursor_width, line.bounds.size.height);
            }
        }

        let last_line = &self.lines[self.lines.len() - 1];
        TextRect::from_xywh(
            last_line.bounds.right(),
            last_line.bounds.top(),
            cursor_width,
            last_line.bounds.size.height,
        )
    }

    /// 计算指定字节区间的选区高亮矩形集合。
    ///
    /// # 参数
    /// - `range`：选区的字节区间。
    ///
    /// # 返回
    /// 覆盖选区的各行高亮矩形切片。
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<TextRect> {
        let mut rects = Vec::new();
        if range.is_empty() || self.lines.is_empty() {
            return rects;
        }

        for line in &self.lines {
            if range.end <= line.start_byte || range.start >= line.end_byte {
                continue;
            }

            let sel_start = range.start.max(line.start_byte);
            let sel_end = range.end.min(line.end_byte);

            let start_x = if sel_start == line.start_byte {
                line.bounds.left()
            } else {
                line.chars
                    .iter()
                    .find(|c| c.byte_offset == sel_start)
                    .map(|c| c.bounds.left())
                    .unwrap_or(line.bounds.left())
            };

            let end_x = if sel_end == line.end_byte {
                line.bounds.right()
            } else {
                line.chars
                    .iter()
                    .find(|c| c.byte_offset + c.byte_len == sel_end)
                    .map(|c| c.bounds.right())
                    .unwrap_or(line.bounds.right())
            };

            let width = (end_x - start_x).max(1.0);
            rects.push(TextRect::from_xywh(
                start_x,
                line.bounds.top(),
                width,
                line.bounds.size.height,
            ));
        }

        rects
    }

    /// 排版指定文本内容。
    ///
    /// # 参数
    /// - `text`：待排版文本。
    /// - `style`：排版样式。
    /// - `max_width`：可选的限制最大换行宽度。
    ///
    /// # 返回
    /// 排版度量与行布局结果。
    pub fn layout_text(text: &str, style: &CanvasTextStyle, max_width: Option<f32>) -> Self {
        let font_size = style.font_size;
        let line_height = (font_size * style.line_height).max(font_size);

        if text.is_empty() {
            let empty_line = TextLineLayout {
                line_index: 0,
                start_byte: 0,
                end_byte: 0,
                bounds: TextRect::from_xywh(0.0, 0.0, 0.0, line_height),
                text: String::new(),
                chars: Vec::new(),
            };
            return Self {
                layout_size: TextSize::new(4.0, line_height),
                content_size: TextSize::new(0.0, line_height),
                lines: vec![empty_line],
                style: style.clone(),
            };
        }

        // 1. 按显式换行符切分为段落
        let mut raw_lines = Vec::new();
        let mut line_start = 0;
        for (idx, ch) in text.char_indices() {
            if ch == '\n' {
                raw_lines.push((line_start, idx));
                line_start = idx + 1;
            }
        }
        raw_lines.push((line_start, text.len()));

        // 2. 处理自动换行或保持原行
        let mut laid_lines = Vec::new();
        let wrap_limit = if !style.auto_resize { max_width } else { None };

        let mut current_y = 0.0;
        let mut max_content_width: f32 = 0.0;

        for (start_byte, end_byte) in raw_lines {
            let paragraph = &text[start_byte..end_byte];
            let mut sub_start = 0;
            let mut sub_start_byte = start_byte;

            let mut char_metrics = Vec::new();
            let mut current_line_width = 0.0;

            for (ch_idx, ch) in paragraph.char_indices() {
                let ch_width = estimate_char_width(ch, font_size);
                let byte_len = ch.len_utf8();
                let actual_byte_offset = start_byte + ch_idx;

                if let Some(limit) = wrap_limit
                    && current_line_width + ch_width > limit
                    && !char_metrics.is_empty()
                {
                    // 触发软折行
                    let line_text = paragraph[sub_start..ch_idx].to_string();
                    let line_w = current_line_width;
                    max_content_width = max_content_width.max(line_w);

                    laid_lines.push(TextLineLayout {
                        line_index: laid_lines.len(),
                        start_byte: sub_start_byte,
                        end_byte: actual_byte_offset,
                        bounds: TextRect::from_xywh(0.0, current_y, line_w, line_height),
                        text: line_text,
                        chars: std::mem::take(&mut char_metrics),
                    });
                    current_y += line_height;
                    sub_start = ch_idx;
                    sub_start_byte = actual_byte_offset;
                    current_line_width = 0.0;
                }

                char_metrics.push(TextCharMetric {
                    byte_offset: actual_byte_offset,
                    byte_len,
                    bounds: TextRect::from_xywh(
                        current_line_width,
                        current_y,
                        ch_width,
                        line_height,
                    ),
                });
                current_line_width += ch_width;
            }

            let line_text = paragraph[sub_start..].to_string();
            let line_w = current_line_width;
            max_content_width = max_content_width.max(line_w);

            laid_lines.push(TextLineLayout {
                line_index: laid_lines.len(),
                start_byte: sub_start_byte,
                end_byte,
                bounds: TextRect::from_xywh(0.0, current_y, line_w, line_height),
                text: line_text,
                chars: char_metrics,
            });
            current_y += line_height;
        }

        // 3. 对齐调整（Left, Center, Right）
        let total_box_width = wrap_limit.unwrap_or(max_content_width);
        for line in &mut laid_lines {
            let offset_x = match style.alignment {
                TextAlignment::Left => 0.0,
                TextAlignment::Center => ((total_box_width - line.bounds.size.width) * 0.5).max(0.0),
                TextAlignment::Right => (total_box_width - line.bounds.size.width).max(0.0),
            };
            if offset_x > 0.0 {
                line.bounds.origin.x += offset_x;
                for c in &mut line.chars {
                    c.bounds.origin.x += offset_x;
                }
            }
        }

        let content_size = TextSize::new(max_content_width, current_y);
        let layout_size = if style.auto_resize {
            // 自动伸缩模式保留安全内边距 4px
            TextSize::new(max_content_width + 4.0, current_y)
        } else {
            TextSize::new(total_box_width, current_y)
        };

        Self {
            layout_size,
            content_size,
            lines: laid_lines,
            style: style.clone(),
        }
    }
}

/// 估算字符的排版显示宽度（无头测试与布局初筛使用）。
fn estimate_char_width(ch: char, font_size: f32) -> f32 {
    if ch == '\t' {
        return font_size * 1.5;
    }
    if ch == ' ' {
        return font_size * 0.35;
    }
    let uwidth = ch.width().unwrap_or(1);
    if uwidth >= 2 {
        // 全角字符 / CJK / 表情符号
        font_size * 1.0
    } else {
        // 半角英文字符比例约 0.55
        font_size * 0.55
    }
}
