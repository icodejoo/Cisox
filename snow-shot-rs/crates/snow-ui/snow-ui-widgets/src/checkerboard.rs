//! 棋盘格透明底纹组件（方案 ADR-3 / 缺口组件）。
//!
//! 用于选区、预览窗和贴图窗口中指示 Alpha 透明通道。

use snow_ui_shell::ui::{
    AnyElement, Hsla, IntoElement, ParentElement, Pixels, RenderOnce, Styled, div, px,
};

/// 默认方格尺寸（逻辑像素）。
pub const DEFAULT_CELL_SIZE: f32 = 12.0;

/// 默认浅色方格颜色（纯白）。
pub const DEFAULT_LIGHT_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};

/// 默认深色方格颜色（浅灰）。
pub const DEFAULT_DARK_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.92,
    a: 1.0,
};

/// 棋盘格透明底纹组件。
pub struct Checkerboard {
    /// 方格尺寸。
    pub cell_size: Pixels,
    /// 浅色方格颜色。
    pub light_color: Hsla,
    /// 深色方格颜色。
    pub dark_color: Hsla,
    /// 子元素。
    children: Vec<AnyElement>,
}

impl Default for Checkerboard {
    /// 以默认参数构造棋盘格组件。
    fn default() -> Self {
        Self::new()
    }
}

impl Checkerboard {
    /// 创建默认配置的棋盘格组件。
    ///
    /// # 返回
    /// 棋盘格实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::Checkerboard;
    /// use snow_ui_shell::ui::px;
    /// let board = Checkerboard::new();
    /// assert_eq!(board.cell_size, px(12.0));
    /// ```
    pub fn new() -> Self {
        Self {
            cell_size: px(DEFAULT_CELL_SIZE),
            light_color: DEFAULT_LIGHT_COLOR,
            dark_color: DEFAULT_DARK_COLOR,
            children: Vec::new(),
        }
    }

    /// 设置单个小方格的尺寸。
    ///
    /// # 参数
    /// - `size`：方格像素尺寸。
    ///
    /// # 返回
    /// 修改后的实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::Checkerboard;
    /// use snow_ui_shell::ui::px;
    /// let board = Checkerboard::new().cell_size(px(16.0));
    /// assert_eq!(board.cell_size, px(16.0));
    /// ```
    pub fn cell_size(mut self, size: Pixels) -> Self {
        self.cell_size = size;
        self
    }

    /// 设置棋盘格的明暗交替颜色。
    ///
    /// # 参数
    /// - `light`：浅色方格颜色。
    /// - `dark`：深色方格颜色。
    ///
    /// # 返回
    /// 修改后的实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::Checkerboard;
    /// use snow_ui_shell::ui::hsla;
    /// let board = Checkerboard::new().colors(hsla(0.0, 0.0, 1.0, 1.0), hsla(0.0, 0.0, 0.8, 1.0));
    /// ```
    pub fn colors(mut self, light: Hsla, dark: Hsla) -> Self {
        self.light_color = light;
        self.dark_color = dark;
        self
    }
}

impl ParentElement for Checkerboard {
    /// 扩展挂载多个子元素。
    ///
    /// # 参数
    /// - `children`：子元素迭代器。
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}

impl RenderOnce for Checkerboard {
    /// 渲染底纹与包裹的内容。
    ///
    /// # 参数
    /// - `_window`：窗口上下文。
    /// - `_cx`：应用上下文。
    ///
    /// # 返回
    /// 渲染出的元素。
    fn render(self, _window: &mut snow_ui_shell::ui::Window, _cx: &mut snow_ui_shell::ui::App) -> impl IntoElement {
        let mut container = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(self.light_color);

        for child in self.children {
            container = container.child(child);
        }

        container
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui_shell::ui::hsla;

    /// 验证棋盘格默认参数及链式配置修改。
    #[test]
    fn test_checkerboard_config() {
        let board = Checkerboard::new()
            .cell_size(px(20.0))
            .colors(hsla(0.0, 0.0, 1.0, 1.0), hsla(0.0, 0.0, 0.5, 1.0));

        assert_eq!(board.cell_size, px(20.0));
        assert_eq!(board.light_color, hsla(0.0, 0.0, 1.0, 1.0));
        assert_eq!(board.dark_color, hsla(0.0, 0.0, 0.5, 1.0));
    }
}
