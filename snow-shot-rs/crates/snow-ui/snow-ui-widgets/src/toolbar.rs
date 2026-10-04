//! 截图浮动工具栏与操作面板组件（Screenshot Toolbar）。
//!
//! 提供标注工具切换（矩形、椭圆、箭头、线段、画笔、文字、马赛克、高亮、序号）、
//! 撤销/重做堆栈操作以及导出动作（钉图、OCR、翻译、复制、保存、取消）。

use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use std::rc::Rc;
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
    /// 马赛克工具。
    Mosaic,
    /// 高斯模糊工具。
    Blur,
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
            Self::Blur => "模糊",
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
    /// 录制屏幕（以当前选区开始录屏）。
    Record,
    /// 长截图（对当前选区做滚动截屏并拼接）。
    ScrollCapture,
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


/// 工具栏动作回调：参数为被点击的动作与窗口 / 应用上下文。
type ActionHandler = Rc<dyn Fn(ToolbarAction, &mut Window, &mut App)>;

/// 工具栏工具切换回调。
type ToolHandler = Rc<dyn Fn(AnnotationTool, &mut Window, &mut App)>;

/// 工具栏动作按钮的显示顺序、文案。
const TOOLBAR_ACTIONS: [(&str, ToolbarAction); 8] = [
    ("贴图", ToolbarAction::Pin),
    ("OCR", ToolbarAction::Ocr),
    ("翻译", ToolbarAction::Translate),
    ("长图", ToolbarAction::ScrollCapture),
    ("录屏", ToolbarAction::Record),
    ("保存", ToolbarAction::Save),
    ("复制", ToolbarAction::Copy),
    ("取消", ToolbarAction::Cancel),
];

/// 可选标注工具的显示顺序。
const TOOLBAR_TOOLS: [AnnotationTool; 10] = [
    AnnotationTool::Rectangle,
    AnnotationTool::Ellipse,
    AnnotationTool::Arrow,
    AnnotationTool::Line,
    AnnotationTool::Pencil,
    AnnotationTool::Highlighter,
    AnnotationTool::Counter,
    AnnotationTool::Text,
    AnnotationTool::Mosaic,
    AnnotationTool::Blur,
];

/// 主色（选中 / 主按钮）。
const COLOR_PRIMARY: u32 = 0x1677FF;
/// 置灰按钮的文字颜色（RGBA）。
const COLOR_DISABLED_TEXT: u32 = 0x6B6B6BFF;
/// 普通按钮的文字颜色（RGBA）。
const COLOR_NORMAL_TEXT: u32 = 0xCCCCCCFF;

/// 截图主工具栏组件。
pub struct ScreenshotToolbar {
    id: ElementId,
    active_tool: AnnotationTool,
    can_undo: bool,
    can_redo: bool,
    show_tools: bool,
    disabled_actions: Vec<ToolbarAction>,
    on_tool_change: Option<ToolHandler>,
    on_action: Option<ActionHandler>,
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
            show_tools: true,
            disabled_actions: Vec::new(),
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

    /// 是否显示标注工具组；标注尚未实现的场景传 `false` 直接隐藏。
    pub fn show_tools(mut self, show: bool) -> Self {
        self.show_tools = show;
        self
    }

    /// 置灰并禁用指定动作：按钮保持可见但不响应点击。
    ///
    /// # 参数
    /// - `actions`: 需要禁用的动作列表。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::{ScreenshotToolbar, ToolbarAction};
    /// let tb = ScreenshotToolbar::new("t").disabled_actions(&[ToolbarAction::Ocr]);
    /// assert!(tb.is_action_disabled(ToolbarAction::Ocr));
    /// assert!(!tb.is_action_disabled(ToolbarAction::Copy));
    /// ```
    pub fn disabled_actions(mut self, actions: &[ToolbarAction]) -> Self {
        self.disabled_actions = actions.to_vec();
        self
    }

    /// 动作当前是否被禁用。
    pub fn is_action_disabled(&self, action: ToolbarAction) -> bool {
        self.disabled_actions.contains(&action)
    }

    /// 注册工具变更事件回调（点击工具按钮时触发）。
    pub fn on_tool_change(
        mut self,
        handler: impl Fn(AnnotationTool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_tool_change = Some(Rc::new(handler));
        self
    }

    /// 注册动作触发事件回调（点击动作按钮时触发，被禁用的动作不会触发）。
    ///
    /// # 参数
    /// - `handler`: 回调，参数为动作、窗口与应用上下文。
    pub fn on_action(
        mut self,
        handler: impl Fn(ToolbarAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ScreenshotToolbar {
    /// 渲染工具栏。
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let mut tool_group = div().flex().flex_row().items_center().gap_1();

        for tool in TOOLBAR_TOOLS {
            let is_active = self.active_tool == tool;
            let mut btn = div()
                .id(SharedString::from(format!("tb-tool-{tool:?}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .text_xs()
                .cursor_pointer();

            if is_active {
                btn = btn
                    .bg(rgba((COLOR_PRIMARY << 8) | 0x33))
                    .text_color(rgb(COLOR_PRIMARY))
                    .font_weight(FontWeight::SEMIBOLD);
            } else {
                btn = btn
                    .text_color(rgba(COLOR_NORMAL_TEXT))
                    .hover(|s| s.bg(rgba(0xFFFFFF1A)).text_color(rgba(0xFFFFFFFF)));
            }
            if let Some(handler) = self.on_tool_change.clone() {
                btn = btn.on_click(move |_, window, cx| handler(tool, window, cx));
            }
            tool_group = tool_group.child(btn.child(tool.label()));
        }

        // 撤销 / 重做：不可用时置灰且不注册点击
        let mut history_group = div().flex().flex_row().items_center().gap_1();
        for (label, act, enabled) in [
            ("撤销", ToolbarAction::Undo, self.can_undo),
            ("重做", ToolbarAction::Redo, self.can_redo),
        ] {
            let mut btn = div()
                .id(SharedString::from(format!("tb-history-{act:?}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .text_xs()
                .child(label);
            if enabled {
                btn = btn
                    .cursor_pointer()
                    .text_color(rgba(COLOR_NORMAL_TEXT))
                    .hover(|s| s.bg(rgba(0xFFFFFF1A)).text_color(rgba(0xFFFFFFFF)));
                if let Some(handler) = self.on_action.clone() {
                    btn = btn.on_click(move |_, window, cx| handler(act, window, cx));
                }
            } else {
                btn = btn.text_color(rgba(COLOR_DISABLED_TEXT));
            }
            history_group = history_group.child(btn);
        }

        // 分割线
        let divider = || div().w(px(1.0)).h_4().bg(rgba(0xFFFFFF33));

        // 动作按钮组
        let mut action_group = div().flex().flex_row().items_center().gap_1();

        for (label, act) in TOOLBAR_ACTIONS {
            let disabled = self.is_action_disabled(act);
            let mut btn = div()
                .id(SharedString::from(format!("tb-action-{act:?}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .text_xs()
                .child(label);

            if disabled {
                // 置灰：不显示手型光标，不注册点击
                btn = btn.text_color(rgba(COLOR_DISABLED_TEXT));
            } else {
                btn = btn.cursor_pointer();
                if act == ToolbarAction::Copy {
                    // 推荐主动作高亮
                    btn = btn
                        .bg(rgb(COLOR_PRIMARY))
                        .text_color(rgba(0xFFFFFFFF))
                        .font_weight(FontWeight::SEMIBOLD)
                        .hover(|s| s.bg(rgb(0x4096FF)));
                } else if act == ToolbarAction::Cancel {
                    btn = btn
                        .text_color(rgba(0xFF4D4FFF))
                        .hover(|s| s.bg(rgba(0xFF4D4F1A)));
                } else {
                    btn = btn
                        .text_color(rgba(COLOR_NORMAL_TEXT))
                        .hover(|s| s.bg(rgba(0xFFFFFF1A)).text_color(rgba(0xFFFFFFFF)));
                }
                if let Some(handler) = self.on_action.clone() {
                    btn = btn.on_click(move |_, window, cx| handler(act, window, cx));
                }
            }

            action_group = action_group.child(btn);
        }

        let mut bar = div()
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
            // 点击工具栏不应穿透到下层选区，否则会误触发重新框选
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if self.show_tools {
            bar = bar
                .child(tool_group)
                .child(divider())
                .child(history_group)
                .child(divider());
        }
        bar.child(action_group)
    }
}

impl IntoElement for ScreenshotToolbar {
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

    /// 验证工具标签。
    #[test]
    fn tool_labels() {
        assert_eq!(AnnotationTool::Rectangle.label(), "矩形");
        assert_eq!(AnnotationTool::Arrow.label(), "箭头");
        assert_eq!(AnnotationTool::Mosaic.label(), "马赛克");
        assert_eq!(AnnotationTool::Blur.label(), "模糊");
    }

    /// 工具表无重复且包含马赛克与模糊。
    #[test]
    fn tool_table_is_unique_and_has_filters() {
        let mut seen = std::collections::HashSet::new();
        assert!(TOOLBAR_TOOLS.iter().all(|t| seen.insert(*t)));
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Mosaic));
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Blur));
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Highlighter));
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Counter));
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

    /// 禁用动作只影响指定项，且默认全部可用。
    #[test]
    fn disabled_actions_are_selective() {
        let tb = ScreenshotToolbar::new("t");
        assert!(TOOLBAR_ACTIONS.iter().all(|(_, a)| !tb.is_action_disabled(*a)));
        let tb = tb.disabled_actions(&[ToolbarAction::Pin, ToolbarAction::Ocr]);
        assert!(tb.is_action_disabled(ToolbarAction::Pin));
        assert!(tb.is_action_disabled(ToolbarAction::Ocr));
        assert!(!tb.is_action_disabled(ToolbarAction::Save));
    }

    /// 动作按钮表不含重复项，覆盖除撤销/重做外的全部动作。
    #[test]
    fn action_table_is_complete_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (_, a) in TOOLBAR_ACTIONS {
            assert!(seen.insert(a), "动作重复: {a:?}");
        }
        assert_eq!(seen.len(), 8);
        assert!(seen.contains(&ToolbarAction::Record));
        assert!(!seen.contains(&ToolbarAction::Undo) && !seen.contains(&ToolbarAction::Redo));
    }
}
