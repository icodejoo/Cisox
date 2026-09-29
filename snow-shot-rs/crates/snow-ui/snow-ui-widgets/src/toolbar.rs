//! 截图浮动工具栏与操作面板组件（Screenshot Toolbar）。
//!
//! 提供标注工具切换（矩形、椭圆、箭头、线段、画笔、文字、马赛克、高亮、序号）、
//! 撤销/重做堆栈操作以及导出动作（钉图、OCR、翻译、复制、保存、取消）。

use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::*;

/// 标注工具种类枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AnnotationTool {
    /// 默认无激活工具（纯选区模式）。
    #[default]
    None,
    /// 矩形工具。
    Rectangle,
    /// 椭圆工具。
    Ellipse,
    /// 箭头工具。
    Arrow,
    /// 直线工具。
    Line,
    /// 涂鸦画笔工具。
    Pencil,
    /// 文字标注工具。
    Text,
    /// 马赛克/模糊工具。
    Mosaic,
    /// 荧光笔高亮工具。
    Highlighter,
    /// 步骤序号标记球。
    Counter,
}

impl AnnotationTool {
    /// 工具名称（中文）。
    ///
    /// # 返回
    /// 名称字符串切片。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::AnnotationTool;
    /// assert_eq!(AnnotationTool::Rectangle.label(), "矩形");
    /// assert_eq!(AnnotationTool::Text.label(), "文字");
    /// ```
    pub const fn label(&self) -> &'static str {
        match self {
            Self::None => "选择",
            Self::Rectangle => "矩形",
            Self::Ellipse => "椭圆",
            Self::Arrow => "箭头",
            Self::Line => "直线",
            Self::Pencil => "画笔",
            Self::Text => "文字",
            Self::Mosaic => "马赛克",
            Self::Highlighter => "高亮",
            Self::Counter => "序号",
        }
    }
}

/// 截图操作动作枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarAction {
    /// 撤销上一步绘制。
    Undo,
    /// 重做上一步绘制。
    Redo,
    /// 钉在桌面上（贴图）。
    Pin,
    /// 文字识别（OCR）。
    Ocr,
    /// 截图翻译。
    Translate,
    /// 保存为图片文件。
    Save,
    /// 复制图像到剪贴板并退出。
    Copy,
    /// 取消截图并退出覆盖窗。
    Cancel,
}

/// 计算工具栏相对于当前选区和屏幕边界的摆放坐标。
///
/// 优先放置在选区下方右侧；若贴近屏幕底边则翻转至选区上方；若上下空间均不足则嵌入选区内部底端。
///
/// # 参数
/// - `selection`: 当前选区矩形。
/// - `toolbar_size`: 工具栏物理尺寸（宽高）。
/// - `screen_bounds`: 屏幕物理边界。
/// - `margin`: 边距间隔（像素）。
///
/// # 返回
/// 工具栏左上角物理坐标。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_widgets::calculate_toolbar_placement;
/// let sel = PhysicalRect::new(200, 200, 600, 400);
/// let tb = PhysicalPoint::new(360, 36);
/// let screen = PhysicalRect::new(0, 0, 1920, 1080);
/// let pos = calculate_toolbar_placement(sel, tb, screen, 8);
/// assert_eq!(pos.y, 608); // 200 + 400 + 8
/// ```
pub fn calculate_toolbar_placement(
    selection: PhysicalRect,
    toolbar_size: PhysicalPoint,
    screen_bounds: PhysicalRect,
    margin: i32,
) -> PhysicalPoint {
    // 水平右对齐选区右侧
    let mut x = selection.right() - toolbar_size.x;
    // 若左侧越过选区或屏幕，则自适应靠拢
    if x < screen_bounds.x {
        x = screen_bounds.x;
    }
    if x + toolbar_size.x > screen_bounds.right() {
        x = screen_bounds.right() - toolbar_size.x;
    }

    // 优先放置在下方
    let mut y = selection.bottom() + margin;

    // 若下方空间不足放置工具栏
    if y + toolbar_size.y > screen_bounds.bottom() {
        // 尝试翻转到选区上方
        let top_y = selection.y - margin - toolbar_size.y;
        if top_y >= screen_bounds.y {
            y = top_y;
        } else {
            // 上下均不够（选区铺满全屏），嵌入选区内部底端
            y = (selection.bottom() - margin - toolbar_size.y).max(screen_bounds.y);
        }
    }

    PhysicalPoint::new(x, y)
}

/// 截图主工具栏组件。
pub struct ScreenshotToolbar {
    id: ElementId,
    active_tool: AnnotationTool,
    can_undo: bool,
    can_redo: bool,
    on_tool_change: Option<Box<dyn Fn(AnnotationTool) + 'static>>,
    on_action: Option<Box<dyn Fn(ToolbarAction) + 'static>>,
}

impl ScreenshotToolbar {
    /// 构造工具栏组件。
    ///
    /// # 参数
    /// - `id`: 元素标识。
    ///
    /// # 返回
    /// 工具栏构建器。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::ScreenshotToolbar;
    /// let _tb = ScreenshotToolbar::new("screenshot-toolbar");
    /// ```
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            active_tool: AnnotationTool::None,
            can_undo: false,
            can_redo: false,
            on_tool_change: None,
            on_action: None,
        }
    }

    /// 设置当前激活的标注工具。
    pub fn active_tool(mut self, tool: AnnotationTool) -> Self {
        self.active_tool = tool;
        self
    }

    /// 设置撤销/重做可用状态。
    pub fn undo_redo_state(mut self, can_undo: bool, can_redo: bool) -> Self {
        self.can_undo = can_undo;
        self.can_redo = can_redo;
        self
    }

    /// 注册工具变更事件回调。
    pub fn on_tool_change(mut self, handler: impl Fn(AnnotationTool) + 'static) -> Self {
        self.on_tool_change = Some(Box::new(handler));
        self
    }

    /// 注册动作触发事件回调。
    pub fn on_action(mut self, handler: impl Fn(ToolbarAction) + 'static) -> Self {
        self.on_action = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ScreenshotToolbar {
    /// 渲染工具栏。
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let tools = [
            AnnotationTool::Rectangle,
            AnnotationTool::Ellipse,
            AnnotationTool::Arrow,
            AnnotationTool::Line,
            AnnotationTool::Pencil,
            AnnotationTool::Text,
            AnnotationTool::Mosaic,
        ];

        let mut tool_group = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1();

        for tool in tools {
            let is_active = self.active_tool == tool;
            let mut btn = div()
                .px_2()
                .py_1()
                .rounded_sm()
                .text_xs()
                .cursor_pointer();

            if is_active {
                btn = btn
                    .bg(rgba(0x1677FF33))
                    .text_color(rgb(0x1677FF))
                    .font_weight(FontWeight::SEMIBOLD);
            } else {
                btn = btn
                    .text_color(rgba(0xCCCCCCFF))
                    .hover(|s| s.bg(rgba(0xFFFFFF1A)).text_color(rgba(0xFFFFFFFF)));
            }

            btn = btn.child(tool.label());
            tool_group = tool_group.child(btn);
        }

        // 分割线
        let divider = div()
            .w(px(1.0))
            .h_4()
            .bg(rgba(0xFFFFFF33));

        // 动作按钮组
        let mut action_group = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1();

        let actions = [
            ("贴图", ToolbarAction::Pin),
            ("OCR", ToolbarAction::Ocr),
            ("翻译", ToolbarAction::Translate),
            ("保存", ToolbarAction::Save),
            ("复制", ToolbarAction::Copy),
            ("取消", ToolbarAction::Cancel),
        ];

        for (label, act) in actions {
            let mut btn = div()
                .px_2()
                .py_1()
                .rounded_sm()
                .text_xs()
                .cursor_pointer()
                .child(label);

            if act == ToolbarAction::Copy {
                // 推荐主动作高亮
                btn = btn
                    .bg(rgb(0x1677FF))
                    .text_color(rgba(0xFFFFFFFF))
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|s| s.bg(rgb(0x4096FF)));
            } else if act == ToolbarAction::Cancel {
                btn = btn
                    .text_color(rgba(0xFF4D4FFF))
                    .hover(|s| s.bg(rgba(0xFF4D4F1A)));
            } else {
                btn = btn
                    .text_color(rgba(0xCCCCCCFF))
                    .hover(|s| s.bg(rgba(0xFFFFFF1A)).text_color(rgba(0xFFFFFFFF)));
            }

            action_group = action_group.child(btn);
        }

        div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgba(0x1F1F1FE6))
            .shadow_lg()
            .border_1()
            .border_color(rgba(0x00000080))
            .child(tool_group)
            .child(divider)
            .child(action_group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证工具标签。
    #[test]
    fn tool_labels() {
        assert_eq!(AnnotationTool::Rectangle.label(), "矩形");
        assert_eq!(AnnotationTool::Arrow.label(), "箭头");
        assert_eq!(AnnotationTool::Mosaic.label(), "马赛克");
    }

    /// 验证工具栏定位算法。
    #[test]
    fn toolbar_placement() {
        let screen = PhysicalRect::new(0, 0, 1920, 1080);
        let tb = PhysicalPoint::new(400, 36);

        // 选区处于屏幕中央，放置在下方
        let sel_center = PhysicalRect::new(500, 300, 600, 400);
        let pos = calculate_toolbar_placement(sel_center, tb, screen, 8);
        assert_eq!(pos.y, 708);
        assert_eq!(pos.x, 500 + 600 - 400); // 700

        // 选区贴近屏幕底边，翻转至上方
        let sel_bottom = PhysicalRect::new(500, 800, 600, 260);
        let pos_flipped = calculate_toolbar_placement(sel_bottom, tb, screen, 8);
        assert_eq!(pos_flipped.y, 800 - 8 - 36); // 756
    }
}
