//! 放大镜与取色器浮层组件（Magnifier & Color Picker）。
//!
//! 在光标周围提供像素级局部放大网格、十字准星定位、RGB/HEX/HSL 色彩实时读取、
//! 屏幕物理坐标与选区几何尺寸展示。

use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::*;

/// 色彩展示格式枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorFormat {
    /// 十六进制格式（`#RRGGBB`）。
    #[default]
    Hex,
    /// RGB 格式（`rgb(r, g, b)`）。
    Rgb,
    /// HSL 格式（`hsl(h, s%, l%)`）。
    Hsl,
}

impl ColorFormat {
    /// 循环切换至下一个格式。
    ///
    /// # 返回
    /// 下一个格式。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::ColorFormat;
    /// assert_eq!(ColorFormat::Hex.cycle(), ColorFormat::Rgb);
    /// assert_eq!(ColorFormat::Rgb.cycle(), ColorFormat::Hsl);
    /// assert_eq!(ColorFormat::Hsl.cycle(), ColorFormat::Hex);
    /// ```
    pub const fn cycle(&self) -> Self {
        match self {
            Self::Hex => Self::Rgb,
            Self::Rgb => Self::Hsl,
            Self::Hsl => Self::Hex,
        }
    }

    /// 格式化 RGBA 色彩字符串。
    ///
    /// # 参数
    /// - `r`: 红色通道 (0..255)。
    /// - `g`: 绿色通道 (0..255)。
    /// - `b`: 蓝色通道 (0..255)。
    ///
    /// # 返回
    /// 格式化后的字符串。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::ColorFormat;
    /// assert_eq!(ColorFormat::Hex.format_color(22, 119, 255), "#1677FF");
    /// assert_eq!(ColorFormat::Rgb.format_color(22, 119, 255), "RGB(22, 119, 255)");
    /// ```
    pub fn format_color(&self, r: u8, g: u8, b: u8) -> String {
        match self {
            Self::Hex => format!("#{:02X}{:02X}{:02X}", r, g, b),
            Self::Rgb => format!("RGB({}, {}, {})", r, g, b),
            Self::Hsl => {
                let rf = r as f64 / 255.0;
                let gf = g as f64 / 255.0;
                let bf = b as f64 / 255.0;
                let max = rf.max(gf.max(bf));
                let min = rf.min(gf.min(bf));
                let delta = max - min;
                let l = (max + min) / 2.0;

                let (h, s) = if delta.abs() < 1e-6 {
                    (0.0, 0.0)
                } else {
                    let s = if l > 0.5 {
                        delta / (2.0 - max - min)
                    } else {
                        delta / (max + min)
                    };
                    let h = if (max - rf).abs() < 1e-6 {
                        (gf - bf) / delta + (if gf < bf { 6.0 } else { 0.0 })
                    } else if (max - gf).abs() < 1e-6 {
                        (bf - rf) / delta + 2.0
                    } else {
                        (rf - gf) / delta + 4.0
                    } * 60.0;
                    (h, s)
                };
                format!("HSL({:.0}, {:.0}%, {:.0}%)", h, s * 100.0, l * 100.0)
            }
        }
    }
}

/// 放大镜采样网格数据模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagnifierGrid {
    /// 网格边长（奇数，如 15、21）。
    pub dimension: usize,
    /// 采样像素缓存（RGBA 扁平数组，长度为 dimension * dimension * 4）。
    pub pixels: Vec<u8>,
}

impl MagnifierGrid {
    /// 创建纯色或空白网格。
    ///
    /// # 参数
    /// - `dimension`: 边长（建议为奇数）。
    /// - `fill`: 初始填充色 `(r, g, b, a)`。
    ///
    /// # 返回
    /// 采样网格。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::MagnifierGrid;
    /// let grid = MagnifierGrid::new_solid(15, (0, 0, 0, 255));
    /// assert_eq!(grid.dimension, 15);
    /// assert_eq!(grid.center_pixel(), (0, 0, 0, 255));
    /// ```
    pub fn new_solid(dimension: usize, fill: (u8, u8, u8, u8)) -> Self {
        let count = dimension * dimension;
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.push(fill.0);
            pixels.push(fill.1);
            pixels.push(fill.2);
            pixels.push(fill.3);
        }
        Self { dimension, pixels }
    }

    /// 获取中心点像素的 RGBA 值。
    ///
    /// # 返回
    /// `(r, g, b, a)`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::MagnifierGrid;
    /// let grid = MagnifierGrid::new_solid(11, (255, 128, 0, 255));
    /// assert_eq!(grid.center_pixel(), (255, 128, 0, 255));
    /// ```
    pub fn center_pixel(&self) -> (u8, u8, u8, u8) {
        if self.pixels.is_empty() || self.dimension == 0 {
            return (0, 0, 0, 255);
        }
        let center_idx = (self.dimension / 2) * self.dimension + (self.dimension / 2);
        let offset = center_idx * 4;
        if offset + 3 < self.pixels.len() {
            (
                self.pixels[offset],
                self.pixels[offset + 1],
                self.pixels[offset + 2],
                self.pixels[offset + 3],
            )
        } else {
            (0, 0, 0, 255)
        }
    }

    /// 读取指定相对坐标 `(col, row)` 的像素。
    ///
    /// # 参数
    /// - `col`: 列号 (0..dimension)。
    /// - `row`: 行号 (0..dimension)。
    ///
    /// # 返回
    /// `(r, g, b, a)`。
    pub fn pixel_at(&self, col: usize, row: usize) -> (u8, u8, u8, u8) {
        if col >= self.dimension || row >= self.dimension {
            return (0, 0, 0, 0);
        }
        let idx = (row * self.dimension + col) * 4;
        if idx + 3 < self.pixels.len() {
            (
                self.pixels[idx],
                self.pixels[idx + 1],
                self.pixels[idx + 2],
                self.pixels[idx + 3],
            )
        } else {
            (0, 0, 0, 0)
        }
    }
}

/// 计算放大镜浮窗在屏幕上的理想放置位置，防止遮挡光标或越过显示器边界。
///
/// # 参数
/// - `cursor`: 鼠标当前屏幕坐标。
/// - `window_size`: 放大镜浮窗物理尺寸。
/// - `screen_bounds`: 屏幕物理边界。
/// - `offset`: 距光标的偏移距离。
///
/// # 返回
/// 浮窗左上角坐标。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_widgets::calculate_magnifier_placement;
/// let cur = PhysicalPoint::new(100, 100);
/// let win = PhysicalPoint::new(120, 160);
/// let screen = PhysicalRect::new(0, 0, 1920, 1080);
/// let pos = calculate_magnifier_placement(cur, win, screen, 16);
/// assert_eq!(pos, PhysicalPoint::new(116, 116));
/// ```
pub fn calculate_magnifier_placement(
    cursor: PhysicalPoint,
    window_size: PhysicalPoint,
    screen_bounds: PhysicalRect,
    offset: i32,
) -> PhysicalPoint {
    let mut x = cursor.x + offset;
    let mut y = cursor.y + offset;

    // 若右侧越界，翻转至光标左侧
    if x + window_size.x > screen_bounds.right() {
        x = cursor.x - offset - window_size.x;
    }
    // 若底部越界，翻转至光标上方
    if y + window_size.y > screen_bounds.bottom() {
        y = cursor.y - offset - window_size.y;
    }

    // 严防整体超出屏幕左上边缘
    if x < screen_bounds.x {
        x = screen_bounds.x;
    }
    if y < screen_bounds.y {
        y = screen_bounds.y;
    }

    PhysicalPoint::new(x, y)
}

/// 放大镜与取色器渲染组件。
pub struct Magnifier {
    id: ElementId,
    grid: MagnifierGrid,
    cursor: PhysicalPoint,
    selection_rect: Option<PhysicalRect>,
    color_format: ColorFormat,
}

impl Magnifier {
    /// 构造放大镜组件。
    ///
    /// # 参数
    /// - `id`: 元素标识。
    /// - `grid`: 像素网格数据。
    /// - `cursor`: 当前光标物理坐标。
    ///
    /// # 返回
    /// 放大镜组件构建器。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::geometry::PhysicalPoint;
    /// use snow_ui_widgets::{Magnifier, MagnifierGrid};
    /// let grid = MagnifierGrid::new_solid(15, (0, 0, 0, 255));
    /// let _m = Magnifier::new("mag-1", grid, PhysicalPoint::new(100, 200));
    /// ```
    pub fn new(id: impl Into<ElementId>, grid: MagnifierGrid, cursor: PhysicalPoint) -> Self {
        Self {
            id: id.into(),
            grid,
            cursor,
            selection_rect: None,
            color_format: ColorFormat::default(),
        }
    }

    /// 设置关联的当前选区矩形。
    pub fn selection_rect(mut self, rect: Option<PhysicalRect>) -> Self {
        self.selection_rect = rect;
        self
    }

    /// 设置色彩展示格式。
    pub fn color_format(mut self, format: ColorFormat) -> Self {
        self.color_format = format;
        self
    }
}

impl RenderOnce for Magnifier {
    /// 渲染放大镜组件。
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (r, g, b, _a) = self.grid.center_pixel();
        let color_text = self.color_format.format_color(r, g, b);
        let center_color_css = rgb((r as u32) << 16 | (g as u32) << 8 | (b as u32));

        let dim = self.grid.dimension;
        let cell_px = 7.0;
        let grid_size_px = dim as f32 * cell_px;

        // 像素网格视图
        let mut grid_rows = div()
            .flex()
            .flex_col()
            .w(px(grid_size_px))
            .h(px(grid_size_px))
            .border_1()
            .border_color(rgba(0xFFFFFF40));

        for row in 0..dim {
            let mut row_div = div().flex().flex_row().h(px(cell_px));
            for col in 0..dim {
                let (pr, pg, pb, _) = self.grid.pixel_at(col, row);
                let pixel_color = rgb((pr as u32) << 16 | (pg as u32) << 8 | (pb as u32));
                let is_center = row == dim / 2 && col == dim / 2;

                let mut cell = div()
                    .w(px(cell_px))
                    .h(px(cell_px))
                    .bg(pixel_color);

                if is_center {
                    // 中心十字准星加亮边框
                    cell = cell.border_1().border_color(rgba(0xFF0000FF));
                }
                row_div = row_div.child(cell);
            }
            grid_rows = grid_rows.child(row_div);
        }

        // 底部色彩与坐标信息面板
        let info_panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(rgba(0x1F1F1FE6))
            .text_color(rgba(0xFFFFFFFF))
            .text_xs()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w_4()
                            .h_4()
                            .rounded_xs()
                            .border_1()
                            .border_color(rgba(0xFFFFFF60))
                            .bg(center_color_css),
                    )
                    .child(color_text),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap_2()
                    .text_color(rgba(0xCCCCCCFF))
                    .child(format!("X: {}, Y: {}", self.cursor.x, self.cursor.y))
                    .when_some(self.selection_rect, |this, sel| {
                        this.child(format!("{} × {}", sel.width.max(0), sel.height.max(0)))
                    }),
            )
            .child(
                div()
                    .text_color(rgba(0x888888FF))
                    .text_xs()
                    .child("按 C 复制颜色值"),
            );

        div()
            .id(self.id)
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .shadow_lg()
            .border_1()
            .border_color(rgba(0x00000080))
            .child(grid_rows)
            .child(info_panel)
    }
}

impl IntoElement for Magnifier {
    type Element = ViewElement<Self>;

    #[track_caller]
    fn into_element(self) -> Self::Element {
        ViewElement::new(self)
    }

    #[track_caller]
    fn into_any_element(self) -> AnyElement {
        Element::into_any(self.into_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证颜色格式切换与格式化。
    #[test]
    fn color_format_cycle_and_format() {
        let fmt = ColorFormat::Hex;
        assert_eq!(fmt.format_color(255, 0, 0), "#FF0000");

        let fmt_rgb = fmt.cycle();
        assert_eq!(fmt_rgb, ColorFormat::Rgb);
        assert_eq!(fmt_rgb.format_color(255, 0, 0), "RGB(255, 0, 0)");

        let fmt_hsl = fmt_rgb.cycle();
        assert_eq!(fmt_hsl, ColorFormat::Hsl);
        assert_eq!(fmt_hsl.format_color(255, 0, 0), "HSL(0, 100%, 50%)");

        let fmt_back = fmt_hsl.cycle();
        assert_eq!(fmt_back, ColorFormat::Hex);
    }

    /// 验证放大镜采样网格。
    #[test]
    fn magnifier_grid_sampling() {
        let grid = MagnifierGrid::new_solid(15, (10, 20, 30, 255));
        assert_eq!(grid.dimension, 15);
        assert_eq!(grid.center_pixel(), (10, 20, 30, 255));
        assert_eq!(grid.pixel_at(0, 0), (10, 20, 30, 255));
        assert_eq!(grid.pixel_at(14, 14), (10, 20, 30, 255));
        assert_eq!(grid.pixel_at(15, 15), (0, 0, 0, 0));
    }

    /// 验证浮窗位置计算与防越界。
    #[test]
    fn magnifier_placement_bounds() {
        let screen = PhysicalRect::new(0, 0, 1920, 1080);
        let win = PhysicalPoint::new(120, 160);

        // 正常放置右下
        let p1 = calculate_magnifier_placement(PhysicalPoint::new(100, 100), win, screen, 16);
        assert_eq!(p1, PhysicalPoint::new(116, 116));

        // 屏幕右下角翻转至左上
        let p2 = calculate_magnifier_placement(PhysicalPoint::new(1900, 1060), win, screen, 16);
        assert_eq!(p2, PhysicalPoint::new(1900 - 16 - 120, 1060 - 16 - 160));
    }
}
