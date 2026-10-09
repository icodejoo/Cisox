//! 截图浮动工具栏与操作面板组件（Screenshot Toolbar）。
//!
//! 提供标注工具切换（矩形、椭圆、箭头、线段、画笔、文字、马赛克、高亮、序号）、
//! 撤销/重做堆栈操作以及导出动作（钉图、OCR、翻译、复制、保存、取消）。
//!
//! 按钮全部是图标，文案作 tooltip；同类按钮折成「当前项图标 + 小箭头」合一的分组按钮，
//! 分组表、悬停状态机与尺寸计算见 [`crate::toolbar_groups`]。

use crate::text_measure::{LABEL_FONT_PX, max_label_width, menu_outer_width};
use crate::toolbar_groups::{
    ARROW_GAP, ARROW_ICON, BAR_BORDER, BAR_PADDING_X, BAR_PADDING_Y, BUTTON_SIZE, CHECK_ICON,
    CHECK_ICON_SIZE, COLOR_CHECK_GREEN, DIVIDER_WIDTH, GroupMemory, ITEM_GAP, MENU_PADDING,
    MENU_ROW_HEIGHT, POPUP_OFFSET, PopupEffect, PopupMachine, SECTION_GAP, ToolbarGroup,
    ToolbarItem, group_width, toolbar_logical_size,
};
use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use snow_ui_shell::ui::component::tooltip::Tooltip;
use snow_ui_shell::ui::component::{Disableable, Icon, Sizable, Size as ComponentSize};
use snow_ui_shell::ui::*;
use std::rc::Rc;
use std::time::Duration;

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
    /// 橡皮：拖过的标注被擦除。
    Eraser,
    /// 选择对象：点选、移动、缩放已画的标注。
    Select,
    /// 聚光灯：拖出矩形洞，洞外压暗。
    Spotlight,
    /// 水印：只打开水印设置面板，不在画布上拖拽。
    Watermark,
    /// 自动滤镜：识别选区里的文字 / 图片 / 头像等区域，点击或拖选后一键铺马赛克 / 模糊。
    AutoFilter,
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
            Self::Eraser => "橡皮",
            Self::Select => "选对象",
            Self::Spotlight => "聚光灯",
            Self::Watermark => "水印",
            Self::AutoFilter => "自动滤镜",
        }
    }

    /// 在选区内按下鼠标是否开始绘制；无工具与水印（只开面板）不绘制。
    ///
    /// # 返回
    /// 需要把指针事件交给标注层时为 `true`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::AnnotationTool;
    /// assert!(AnnotationTool::Spotlight.draws());
    /// assert!(!AnnotationTool::Watermark.draws());
    /// assert!(!AnnotationTool::None.draws());
    /// ```
    pub const fn draws(&self) -> bool {
        !matches!(self, Self::None | Self::Watermark)
    }

    /// 是否装饰层工具（聚光灯 / 水印），选中时显示各自的设置面板。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::AnnotationTool;
    /// assert!(AnnotationTool::Watermark.is_decoration());
    /// assert!(!AnnotationTool::Arrow.is_decoration());
    /// ```
    pub const fn is_decoration(&self) -> bool {
        matches!(self, Self::Spotlight | Self::Watermark)
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
    /// 表格识别（把选区里的表格转成可复制的表格数据）。
    Table,
    /// 公式识别（LaTeX）。
    Latex,
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

/// 工具栏上一个需要文案的位置：标注工具按钮、动作按钮（含撤销 / 重做）或分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarLabel {
    /// 标注工具按钮。
    Tool(AnnotationTool),
    /// 动作按钮（含撤销 / 重做）。
    Action(ToolbarAction),
    /// 分组名（如「形状」）。
    Group(ToolbarGroup),
    /// 分组箭头的提示：组名 + 当前项（由界面层用本地化模板拼接）。
    GroupTip(ToolbarGroup, ToolbarItem),
}

impl ToolbarItem {
    /// 条目对应的文案位置。
    pub const fn label(self) -> ToolbarLabel {
        match self {
            Self::Tool(tool) => ToolbarLabel::Tool(tool),
            Self::Action(action) => ToolbarLabel::Action(action),
        }
    }
}

impl ToolbarLabel {
    /// 工具栏上所有需要独立文案的位置（供调用方核对文案是否齐全）。
    ///
    /// # 返回
    /// 全部标注工具按钮、动作按钮与分组名；[`ToolbarLabel::GroupTip`] 是组合模板，不在其中。
    pub fn all() -> Vec<ToolbarLabel> {
        let tools = TOOLBAR_TOOLS.iter().map(|t| Self::Tool(*t));
        let actions = TOOLBAR_ACTIONS.iter().map(|(_, a)| Self::Action(*a));
        let history = [ToolbarAction::Undo, ToolbarAction::Redo].map(Self::Action);
        let groups = ToolbarGroup::ALL.map(Self::Group);
        tools.chain(history).chain(actions).chain(groups).collect()
    }

    /// 内置的默认文案（中文；界面应通过 [`ScreenshotToolbar::labels`] 提供本地化文案）。
    fn default_text(self) -> String {
        match self {
            Self::Tool(tool) => tool.label().to_string(),
            Self::Action(ToolbarAction::Undo) => "撤销".to_string(),
            Self::Action(ToolbarAction::Redo) => "重做".to_string(),
            Self::Action(action) => TOOLBAR_ACTIONS
                .iter()
                .find(|(_, a)| *a == action)
                .map_or("", |(text, _)| text)
                .to_string(),
            Self::Group(group) => match group {
                ToolbarGroup::Shape => "形状",
                ToolbarGroup::Pen => "画笔",
                ToolbarGroup::Mark => "标记",
                ToolbarGroup::Filter => "滤镜",
                ToolbarGroup::Edit => "编辑",
                ToolbarGroup::Recognize => "识别",
                ToolbarGroup::Output => "输出",
            }
            .to_string(),
            Self::GroupTip(group, item) => format!(
                "{}：{}",
                Self::Group(group).default_text(),
                item.label().default_text()
            ),
        }
    }
}

/// 文案提供者：按位置给出本地化文案。
type LabelProvider = Rc<dyn Fn(ToolbarLabel) -> String>;

/// 工具栏动作回调：参数为被点击的动作与窗口 / 应用上下文。
type ActionHandler = Rc<dyn Fn(ToolbarAction, &mut Window, &mut App)>;

/// 工具栏工具切换回调。
type ToolHandler = Rc<dyn Fn(AnnotationTool, &mut Window, &mut App)>;

/// 一个条目被触发（选中工具 / 执行动作）的回调。
type ItemRunner = Rc<dyn Fn(&mut Window, &mut App)>;

/// 工具栏动作按钮的显示顺序、文案。
pub(crate) const TOOLBAR_ACTIONS: [(&str, ToolbarAction); 10] = [
    ("贴图", ToolbarAction::Pin),
    ("OCR", ToolbarAction::Ocr),
    ("翻译", ToolbarAction::Translate),
    ("表格", ToolbarAction::Table),
    ("公式", ToolbarAction::Latex),
    ("长图", ToolbarAction::ScrollCapture),
    ("录屏", ToolbarAction::Record),
    ("保存", ToolbarAction::Save),
    ("复制", ToolbarAction::Copy),
    ("取消", ToolbarAction::Cancel),
];

/// 可选标注工具的显示顺序。
pub(crate) const TOOLBAR_TOOLS: [AnnotationTool; 15] = [
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
    AnnotationTool::AutoFilter,
    AnnotationTool::Eraser,
    AnnotationTool::Select,
    AnnotationTool::Spotlight,
    AnnotationTool::Watermark,
];

/// 主色（选中 / 主按钮）。
const COLOR_PRIMARY: u32 = 0x1677FF;
/// 主按钮悬停色。
const COLOR_PRIMARY_HOVER: u32 = 0x4096FF;
/// 普通按钮的图标 / 文字颜色（RGBA）。
const COLOR_NORMAL_TEXT: u32 = 0xCCCCCCFF;
/// 危险按钮（取消）的颜色。
const COLOR_DANGER: u32 = 0xFF4D4F;
/// 悬停背景（RGBA）。
const COLOR_HOVER_BG: u32 = 0xFFFFFF1A;
/// 按下背景（RGBA）。
const COLOR_PRESSED_BG: u32 = 0xFFFFFF33;
/// 工具栏与弹出菜单的背景色（RGBA）。
const COLOR_BAR_BG: u32 = 0x1F1F1FE6;
/// 工具栏与弹出菜单的边框色（RGBA）。
const COLOR_BAR_BORDER: u32 = 0x00000080;
/// 分割线颜色（RGBA）。
const COLOR_DIVIDER: u32 = 0xFFFFFF33;
/// 选中态背景透明度（叠在主色上）。
const ALPHA_ACTIVE_BG: u32 = 0x33;
/// 选中态悬停背景透明度。
const ALPHA_ACTIVE_HOVER: u32 = 0x4D;
/// 悬停提示（tooltip）出现前的延迟。
const TOOLTIP_DELAY: Duration = Duration::from_millis(350);
/// 图标按钮内图标的边长。
const ICON_SIZE: f32 = 18.0;
/// 下箭头图标边长。
const ARROW_ICON_SIZE: f32 = 10.0;
/// 菜单行内图标边长。
const MENU_ICON_SIZE: f32 = 16.0;

/// 当前项文字的提亮颜色。
pub(crate) const COLOR_CURRENT_TEXT: u32 = 0xFFFFFF;

/// 菜单行末尾的选中标记槽：当前项显示绿色对勾，其余留同宽空位保持对齐。
pub(crate) fn menu_check_slot(is_current: bool) -> Div {
    div()
        .flex_shrink_0()
        .w(px(CHECK_ICON_SIZE))
        .h(px(CHECK_ICON_SIZE))
        .when(is_current, |slot| {
            slot.child(
                Icon::default()
                    .path(CHECK_ICON)
                    .with_size(ComponentSize::Size(px(CHECK_ICON_SIZE)))
                    .text_color(rgb(COLOR_CHECK_GREEN)),
            )
        })
}

/// 按钮外观：普通 / 选中 / 主操作 / 危险。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    /// 普通。
    Normal,
    /// 选中（主色）。
    Active,
    /// 推荐主操作（实心主色）。
    Primary,
    /// 危险（取消）。
    Danger,
}

impl Look {
    /// 把外观转成组件库的自定义按钮配色；置灰由按钮的 `disabled` 状态负责。
    pub(crate) fn variant(self, cx: &App) -> ButtonCustomVariant {
        let base = ButtonCustomVariant::new(cx);
        let tint = |alpha: u32| rgba((COLOR_PRIMARY << 8) | alpha).into();
        match self {
            Self::Normal => base
                .foreground(rgba(COLOR_NORMAL_TEXT).into())
                .hover(rgba(COLOR_HOVER_BG).into())
                .active(rgba(COLOR_PRESSED_BG).into()),
            Self::Active => base
                .color(tint(ALPHA_ACTIVE_BG))
                .foreground(rgb(COLOR_PRIMARY).into())
                .hover(tint(ALPHA_ACTIVE_HOVER))
                .active(tint(ALPHA_ACTIVE_HOVER)),
            Self::Primary => base
                .color(rgb(COLOR_PRIMARY).into())
                .foreground(rgb(0xFFFFFF).into())
                .hover(rgb(COLOR_PRIMARY_HOVER).into())
                .active(rgb(COLOR_PRIMARY_HOVER).into()),
            Self::Danger => base
                .foreground(rgb(COLOR_DANGER).into())
                .hover(rgba((COLOR_DANGER << 8) | 0x1A).into())
                .active(rgba((COLOR_DANGER << 8) | 0x33).into()),
        }
    }
}

/// 分组复合按钮的共享状态：每组当前项记忆 + 悬停 / 点击弹出状态机（含计时器）。
///
/// 由使用方（覆盖窗视图）持有一个实体，经 [`ScreenshotToolbar::groups`] 交给工具栏；
/// 工具栏每帧重建，这份状态跨帧保留。
pub struct ToolbarGroups {
    /// 每组当前项记忆（只在本进程内，不落盘）。
    memory: GroupMemory,
    /// 下拉弹出状态机。
    machine: PopupMachine,
}

impl Default for ToolbarGroups {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolbarGroups {
    /// 创建初始状态：各组当前项为第一项，没有菜单弹出。
    pub fn new() -> Self {
        Self {
            memory: GroupMemory::default(),
            machine: PopupMachine::default(),
        }
    }

    /// 某组当前项。
    pub fn current(&self, group: ToolbarGroup) -> ToolbarItem {
        self.memory.current(group)
    }

    /// 当前弹出菜单的分组。
    pub fn open_group(&self) -> Option<ToolbarGroup> {
        self.machine.open()
    }

    /// 记住条目为所属组的当前项（不触发重绘，渲染前同步激活工具时用）。
    pub fn remember(&mut self, item: ToolbarItem) -> bool {
        self.memory.remember(item)
    }

    /// 执行状态机给出的副作用：重绘或启动计时器。
    fn apply(&mut self, effect: PopupEffect, cx: &mut Context<Self>) {
        match effect {
            PopupEffect::Idle => {}
            PopupEffect::Redraw => cx.notify(),
            PopupEffect::Timer { token, delay_ms } => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(delay_ms))
                        .await;
                    let _ = this.update(cx, |state, cx| {
                        let effect = state.machine.fire(token);
                        state.apply(effect, cx);
                    });
                })
                .detach();
            }
        }
    }

    /// 鼠标进出某组触发区（主按钮 + 箭头）。
    pub fn trigger_hover(&mut self, group: ToolbarGroup, hovered: bool, cx: &mut Context<Self>) {
        let effect = self.machine.trigger_hover(group, hovered);
        self.apply(effect, cx);
    }

    /// 鼠标进出弹出菜单。
    pub fn menu_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let effect = self.machine.menu_hover(hovered);
        self.apply(effect, cx);
    }

    /// 点击下箭头：立即开 / 关本组菜单。
    pub fn click_arrow(&mut self, group: ToolbarGroup, cx: &mut Context<Self>) {
        let effect = self.machine.click_arrow(group);
        self.apply(effect, cx);
    }

    /// 触发某条目（点主按钮或选菜单项）：记为所属组当前项并收起菜单。
    pub fn choose(&mut self, item: ToolbarItem, cx: &mut Context<Self>) {
        self.memory.remember(item);
        let effect = self.machine.close();
        // 当前项变了，主按钮图标要刷新
        cx.notify();
        self.apply(effect, cx);
    }

    /// 立即收起菜单（工具栏消失等场景）。
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        let effect = self.machine.close();
        self.apply(effect, cx);
    }
}

/// 截图主工具栏组件。
pub struct ScreenshotToolbar {
    id: ElementId,
    active_tool: AnnotationTool,
    can_undo: bool,
    can_redo: bool,
    show_tools: bool,
    popup_up: bool,
    disabled_actions: Vec<ToolbarAction>,
    on_tool_change: Option<ToolHandler>,
    on_action: Option<ActionHandler>,
    labels: Option<LabelProvider>,
    groups: Option<Entity<ToolbarGroups>>,
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
            popup_up: false,
            disabled_actions: Vec::new(),
            on_tool_change: None,
            on_action: None,
            labels: None,
            groups: None,
        }
    }

    /// 设置按钮文案提供者（本地化入口）；不设置时用内置中文文案。
    ///
    /// # 参数
    /// - `provider`: 按位置返回文案的闭包。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::{ScreenshotToolbar, ToolbarLabel};
    /// let tb = ScreenshotToolbar::new("t").labels(|key| format!("{key:?}"));
    /// assert_eq!(tb.label_text(ToolbarLabel::Action(snow_ui_widgets::ToolbarAction::Copy)), "Action(Copy)");
    /// ```
    pub fn labels(mut self, provider: impl Fn(ToolbarLabel) -> String + 'static) -> Self {
        self.labels = Some(Rc::new(provider));
        self
    }

    /// 某个位置当前要显示的文案（提供者优先，缺省用内置文案）。
    pub fn label_text(&self, key: ToolbarLabel) -> String {
        match &self.labels {
            Some(provider) => provider(key),
            None => key.default_text(),
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

    /// 分组下拉是否向上展开（工具栏贴近屏幕底边时用）。
    pub fn popup_up(mut self, up: bool) -> Self {
        self.popup_up = up;
        self
    }

    /// 交给工具栏一份跨帧保留的分组状态；不设置则没有下拉与当前项记忆（主按钮固定为组内第一项）。
    pub fn groups(mut self, groups: Entity<ToolbarGroups>) -> Self {
        self.groups = Some(groups);
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

    /// 条目当前是否置灰（工具不会置灰；动作看禁用表）。
    fn item_disabled(&self, item: ToolbarItem) -> bool {
        match item {
            ToolbarItem::Tool(_) => false,
            ToolbarItem::Action(action) => self.is_action_disabled(action),
        }
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

    /// 触发条目时要调用的回调；对应回调没注册返回 `None`。
    fn runner(&self, item: ToolbarItem) -> Option<ItemRunner> {
        match item {
            ToolbarItem::Tool(tool) => {
                let handler = self.on_tool_change.clone()?;
                Some(Rc::new(move |window, cx| handler(tool, window, cx)))
            }
            ToolbarItem::Action(action) => {
                let handler = self.on_action.clone()?;
                Some(Rc::new(move |window, cx| handler(action, window, cx)))
            }
        }
    }

    /// 条目按钮的外观：激活中的工具为选中色。
    fn item_look(&self, item: ToolbarItem) -> Look {
        match item {
            ToolbarItem::Tool(tool) if tool == self.active_tool => Look::Active,
            _ => Look::Normal,
        }
    }

    /// 带无障碍标签的纯图标按钮。
    fn icon_button(
        &self,
        id: String,
        item: ToolbarItem,
        look: Look,
        disabled: bool,
        cx: &App,
    ) -> Button {
        Button::new(SharedString::from(id))
            .custom(look.variant(cx))
            .with_size(ComponentSize::Size(px(BUTTON_SIZE)))
            .icon(
                Icon::default()
                    .path(item.icon_path())
                    .with_size(ComponentSize::Size(px(ICON_SIZE))),
            )
            .disabled(disabled)
            .accessibility_label(SharedString::from(self.label_text(item.label())))
    }

    /// 给元素套一层原生悬停提示（组件库的托管提示需要 Root，覆盖窗没有）。
    fn with_tooltip(id: String, text: String, child: impl IntoElement) -> impl IntoElement {
        let text = SharedString::from(text);
        div()
            .id(SharedString::from(format!("{id}-tip")))
            .tooltip_show_delay(TOOLTIP_DELAY)
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            .child(child)
    }

    /// 独立图标按钮（撤销 / 重做 / 复制 / 取消）。
    fn solo_button(
        &self,
        action: ToolbarAction,
        look: Look,
        enabled: bool,
        cx: &App,
    ) -> AnyElement {
        let item = ToolbarItem::Action(action);
        let id = format!("tb-solo-{action:?}");
        let mut btn = self.icon_button(id.clone(), item, look, !enabled, cx);
        if enabled && let Some(run) = self.runner(item) {
            btn = btn.on_click(move |_, window, cx| run(window, cx));
        }
        Self::with_tooltip(id, self.label_text(item.label()), btn).into_any_element()
    }

    /// 一个分组的复合按钮：图标 + 小箭头合成同一个按钮，悬停弹出同组菜单。
    fn group_view(
        &self,
        window: &Window,
        group: ToolbarGroup,
        memory: &GroupMemory,
        open: Option<ToolbarGroup>,
        cx: &App,
    ) -> AnyElement {
        let current = memory.current(group);
        let is_open = open == Some(group);
        let disabled = self.item_disabled(current);
        let key = group.key();
        let btn_id = format!("tb-{key}-main");
        let look = self.item_look(current);
        if !group.has_menu() {
            // 只有一项：退化为普通图标按钮
            let mut main = self.icon_button(btn_id.clone(), current, look, disabled, cx);
            if !disabled && let Some(run) = self.runner(current) {
                let groups = self.groups.clone();
                main = main.on_click(move |_, window, cx| {
                    if let Some(groups) = &groups {
                        groups.update(cx, |state, gcx| state.choose(current, gcx));
                    }
                    run(window, cx);
                });
            }
            let tip = self.label_text(current.label());
            return Self::with_tooltip(btn_id, tip, main).into_any_element();
        }

        // 整体一个按钮：当前项图标 + 小箭头；点击 = 执行当前项，右键 = 打开菜单
        let content = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(ARROW_GAP))
            .child(
                Icon::default()
                    .path(current.icon_path())
                    .with_size(ComponentSize::Size(px(ICON_SIZE))),
            )
            .child(
                Icon::default()
                    .path(ARROW_ICON)
                    .with_size(ComponentSize::Size(px(ARROW_ICON_SIZE))),
            );
        let mut btn = Button::new(SharedString::from(btn_id.clone()))
            .custom(look.variant(cx))
            .with_size(ComponentSize::Size(px(ARROW_ICON_SIZE)))
            .w(px(group_width(group)))
            .h(px(BUTTON_SIZE))
            .disabled(disabled)
            .accessibility_label(SharedString::from(
                self.label_text(ToolbarLabel::GroupTip(group, current)),
            ))
            .child(content);
        if let Some(groups) = self.groups.clone() {
            // 触屏 / 无法悬停时的补充途径：右键直接开关菜单
            btn = btn.on_mouse_down(MouseButton::Right, move |_, _, cx| {
                groups.update(cx, |state, gcx| state.click_arrow(group, gcx));
            });
        }
        if !disabled && let Some(run) = self.runner(current) {
            let groups = self.groups.clone();
            btn = btn.on_click(move |_, window, cx| {
                if let Some(groups) = &groups {
                    groups.update(cx, |state, gcx| state.choose(current, gcx));
                }
                run(window, cx);
            });
        }
        let row = if is_open {
            div().child(btn)
        } else {
            let tip = self.label_text(ToolbarLabel::GroupTip(group, current));
            div().child(Self::with_tooltip(btn_id, tip, btn))
        };

        let mut popup = Popup::new(SharedString::from(format!("tb-group-{key}")), row)
            .anchor(if self.popup_up {
                Anchor::BottomLeft
            } else {
                Anchor::TopLeft
            })
            .offset(px(POPUP_OFFSET));
        if let Some(groups) = self.groups.clone() {
            popup = popup.on_hover(move |hovered, _, cx| {
                groups.update(cx, |state, gcx| state.trigger_hover(group, *hovered, gcx));
            });
        }
        if is_open {
            popup = popup.content(self.menu_view(window, group, current, cx));
        }
        popup.into_any_element()
    }

    /// 弹出菜单：同组全部项（图标 + 文字），当前项带选中标记；仅在弹出时构造。
    fn menu_view(
        &self,
        window: &Window,
        group: ToolbarGroup,
        current: ToolbarItem,
        cx: &App,
    ) -> AnyElement {
        let key = group.key();
        // 菜单宽度按最长一项自适应（量字结果有缓存）
        let labels: Vec<String> = group
            .items()
            .iter()
            .map(|item| self.label_text(item.label()))
            .collect();
        let width = menu_outer_width(max_label_width(window, &labels, LABEL_FONT_PX));
        let mut menu = div()
            .id(SharedString::from(format!("tb-menu-{key}")))
            .flex()
            .flex_col()
            .gap(px(ITEM_GAP))
            .p(px(MENU_PADDING))
            .w(px(width))
            .rounded_md()
            .bg(rgba(COLOR_BAR_BG))
            .border_1()
            .border_color(rgba(COLOR_BAR_BORDER))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if let Some(groups) = self.groups.clone() {
            menu = menu.on_hover(move |hovered, _, cx| {
                groups.update(cx, |state, gcx| state.menu_hover(*hovered, gcx));
            });
        }
        for item in group.items().iter().copied() {
            let disabled = self.item_disabled(item);
            let is_current = item == current;
            let content = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .w_full()
                .px_2()
                .text_sm()
                .child(
                    Icon::default()
                        .path(item.icon_path())
                        .with_size(ComponentSize::Size(px(MENU_ICON_SIZE))),
                )
                .child(
                    div()
                        .flex_1()
                        .when(is_current, |label| {
                            label.text_color(rgb(COLOR_CURRENT_TEXT))
                        })
                        .child(self.label_text(item.label())),
                )
                .child(menu_check_slot(is_current));
            let mut row = Button::new(SharedString::from(format!("tb-{key}-item-{item:?}")))
                .custom(self.item_look(item).variant(cx))
                .with_size(ComponentSize::Size(px(MENU_ICON_SIZE)))
                .w_full()
                .h(px(MENU_ROW_HEIGHT))
                .disabled(disabled)
                .child(content);
            if !disabled && let Some(run) = self.runner(item) {
                let groups = self.groups.clone();
                row = row.on_click(move |_, window, cx| {
                    if let Some(groups) = &groups {
                        groups.update(cx, |state, gcx| state.choose(item, gcx));
                    }
                    run(window, cx);
                });
            }
            menu = menu.child(row);
        }
        menu.into_any_element()
    }
}

impl RenderOnce for ScreenshotToolbar {
    /// 渲染工具栏。
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // 当前激活的工具（可能由快捷键切换）要成为它所在组的当前项；静默同步，不触发重绘
        if let Some(groups) = &self.groups {
            let active = ToolbarItem::Tool(self.active_tool);
            groups.update(cx, |state, _| state.remember(active));
        }
        let (memory, open) = match &self.groups {
            Some(groups) => {
                let state = groups.read(cx);
                (state.memory.clone(), state.open_group())
            }
            None => (GroupMemory::default(), None),
        };

        let section = || div().flex().flex_row().items_center().gap(px(ITEM_GAP));
        let divider = || div().w(px(DIVIDER_WIDTH)).h_4().bg(rgba(COLOR_DIVIDER));

        let mut tool_section = section();
        for group in ToolbarGroup::TOOL_GROUPS {
            tool_section = tool_section.child(self.group_view(window, group, &memory, open, cx));
        }

        // 撤销 / 重做：不可用时置灰且不注册点击
        let history_section = section()
            .child(self.solo_button(ToolbarAction::Undo, Look::Normal, self.can_undo, cx))
            .child(self.solo_button(ToolbarAction::Redo, Look::Normal, self.can_redo, cx));

        let mut action_section = section();
        for group in ToolbarGroup::ACTION_GROUPS {
            action_section =
                action_section.child(self.group_view(window, group, &memory, open, cx));
        }
        // 复制是推荐主动作；取消用危险色
        let copy_enabled = !self.is_action_disabled(ToolbarAction::Copy);
        let cancel_enabled = !self.is_action_disabled(ToolbarAction::Cancel);
        action_section = action_section
            .child(self.solo_button(ToolbarAction::Copy, Look::Primary, copy_enabled, cx))
            .child(self.solo_button(ToolbarAction::Cancel, Look::Danger, cancel_enabled, cx));

        let (width, height) = toolbar_logical_size(self.show_tools);
        let mut bar = div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SECTION_GAP))
            .px(px(BAR_PADDING_X))
            .py(px(BAR_PADDING_Y))
            .w(px(width as f32))
            .h(px(height as f32))
            .rounded_md()
            .bg(rgba(COLOR_BAR_BG))
            .shadow_lg()
            .border(px(BAR_BORDER))
            .border_color(rgba(COLOR_BAR_BORDER))
            // 点击工具栏不应穿透到下层选区，否则会误触发重新框选
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if self.show_tools {
            bar = bar
                .child(tool_section)
                .child(divider())
                .child(history_section)
                .child(divider());
        }
        bar.child(action_section)
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
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::AutoFilter));
        assert!(AnnotationTool::AutoFilter.draws());
    }

    /// 聚光灯与水印在工具栏里；只有水印不触发拖拽绘制。
    #[test]
    fn decoration_tools_in_toolbar() {
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Spotlight));
        assert!(TOOLBAR_TOOLS.contains(&AnnotationTool::Watermark));
        assert!(AnnotationTool::Spotlight.draws());
        assert!(!AnnotationTool::Watermark.draws());
        assert!(!AnnotationTool::None.draws());
        assert!(AnnotationTool::Rectangle.draws());
        assert!(
            AnnotationTool::Spotlight.is_decoration() && AnnotationTool::Watermark.is_decoration()
        );
        assert!(!AnnotationTool::Select.is_decoration());
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
        assert!(
            TOOLBAR_ACTIONS
                .iter()
                .all(|(_, a)| !tb.is_action_disabled(*a))
        );
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
        assert_eq!(seen.len(), 10);
        assert!(seen.contains(&ToolbarAction::Record));
        assert!(!seen.contains(&ToolbarAction::Undo) && !seen.contains(&ToolbarAction::Redo));
    }
}
