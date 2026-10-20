//! 截图浮动工具栏与操作面板组件（Screenshot Toolbar）。
//!
//! 提供标注工具切换（矩形、椭圆、箭头、线段、画笔、文字、马赛克、高亮、序号）、
//! 撤销/重做堆栈操作以及导出动作（钉图、OCR、翻译、复制、保存、取消）。
//!
//! 按钮全部是图标，文案作 tooltip；同类按钮折成「当前项图标 + 小箭头」合一的分组按钮，
//! 分组表、悬停状态机与尺寸计算见 [`crate::toolbar_groups`]。

use crate::menu_button::MenuEntry;
use crate::submenu_placement::{SubmenuInput, SubmenuSide, place_submenu};
use crate::text_measure::{LABEL_FONT_PX, ROW_ICON, max_label_width, menu_outer_width};
use crate::toolbar_groups::{
    BAR_BORDER, BAR_PADDING_X, BAR_PADDING_Y, BUTTON_SIZE, CHECK_ICON, CHECK_ICON_SIZE,
    COLOR_CHECK_GREEN, GroupMemory, ITEM_GAP, MENU_PADDING, MENU_ROW_HEIGHT, POPUP_OFFSET,
    PopupEffect, PopupMachine, SECTION_GAP, ToolbarGroup, ToolbarItem, ToolbarSlot, group_width,
    menu_outer_height, toolbar_logical_size, toolbar_slots,
};
use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use snow_ui_shell::ui::component::tooltip::Tooltip;
use snow_ui_shell::ui::component::{Disableable, Icon, Sizable, Size as ComponentSize};
use snow_ui_shell::ui::*;
use std::cell::Cell;
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
type ItemRunner = ToolbarItemRunner;
/// 选中翻译目标语言的回调，参数是语言项序号。
type LanguageHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// 主菜单某一行的悬停事件对二级菜单的影响：移入「翻译」行展开，移入其他行收起，移出不改变
/// （移向二级菜单时靠外层悬停保持，不在这里收起）。
///
/// # 参数
/// - `hovered`: 鼠标是否移入该行。
/// - `is_translate`: 该行是否是「翻译」行。
///
/// # 返回
/// `Some(open)` 表示二级菜单应变成该状态；`None` 表示保持不变。
fn translate_menu_after_hover(hovered: bool, is_translate: bool) -> Option<bool> {
    hovered.then_some(is_translate)
}

/// 二级菜单与窗口边缘保持的距离（与 `Popup` 的窗口边距一致）。
const SUBMENU_WINDOW_MARGIN: f32 = 8.0;

/// 右箭头图标（Lucide），翻译行末尾的语言二级菜单入口。
const SUBMENU_ARROW_ICON: &str = "icons/chevron-right.svg";

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
pub(crate) const COLOR_BAR_BG: u32 = 0x1F1F1FE6;
/// 工具栏与弹出菜单的边框色（RGBA）。
pub(crate) const COLOR_BAR_BORDER: u32 = 0x00000080;
/// 分割线颜色（RGBA）。
pub(crate) const COLOR_DIVIDER: u32 = 0xFFFFFF33;
/// 选中态背景透明度（叠在主色上）。
const ALPHA_ACTIVE_BG: u32 = 0x33;
/// 选中态悬停背景透明度。
const ALPHA_ACTIVE_HOVER: u32 = 0x4D;
/// 悬停提示（tooltip）出现前的延迟。
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(350);

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

/// 给元素套一层原生悬停提示（组件库的托管提示需要 Root，覆盖窗没有）。
///
/// # 参数
/// - `id`：被包元素的 id，提示层 id 为 `{id}-tip`。
/// - `text`：提示文字。
/// - `child`：被包元素。
pub(crate) fn with_tooltip(id: String, text: String, child: impl IntoElement) -> impl IntoElement {
    let text = SharedString::from(text);
    div()
        .id(SharedString::from(format!("{id}-tip")))
        .tooltip_show_delay(TOOLTIP_DELAY)
        .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
        .child(child)
}

/// 弹出菜单一行的内容：图标 + 文字（当前项提亮）+ 末尾选中标记槽。
///
/// # 参数
/// - `icon`：图标资源路径。
/// - `label`：已本地化的文字。
/// - `checked`：是否当前项。
pub(crate) fn menu_row_content(icon: &'static str, label: String, checked: bool) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .px_2()
        .text_sm()
        .child(
            Icon::default()
                .path(icon)
                .with_size(ComponentSize::Size(px(ROW_ICON))),
        )
        .child(
            div()
                .flex_1()
                .when(checked, |l| l.text_color(rgb(COLOR_CURRENT_TEXT)))
                .child(label),
        )
        .child(menu_check_slot(checked))
}

/// 按钮外观：普通 / 选中 / 主操作 / 危险。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
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
    pub fn variant(self, cx: &App) -> ButtonCustomVariant {
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
    /// 翻译目标语言二级菜单是否展开（随分组菜单收起而复位）。
    translate_menu_open: bool,
    /// 主菜单左上角在窗口中的位置（上一帧量到的），供二级菜单换边 / 夹边用；菜单收起后清空。
    menu_origin: Rc<Cell<Option<Point<Pixels>>>>,
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
            translate_menu_open: false,
            menu_origin: Rc::new(Cell::new(None)),
        }
    }

    /// 主菜单左上角位置的共享记录（弹出层定位后写入，渲染二级菜单时读取）。
    fn menu_origin(&self) -> Rc<Cell<Option<Point<Pixels>>>> {
        self.menu_origin.clone()
    }

    /// 翻译目标语言二级菜单是否展开。
    pub fn translate_menu_open(&self) -> bool {
        self.translate_menu_open
    }

    /// 展开 / 收起翻译目标语言二级菜单（悬停「翻译」行展开，悬停其他行收起）；状态没变不重绘。
    pub fn set_translate_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.translate_menu_open != open {
            self.translate_menu_open = open;
            cx.notify();
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
        // 分组菜单收起后，二级菜单一并复位
        if self.machine.open().is_none() {
            self.translate_menu_open = false;
            self.menu_origin.set(None);
        }
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

    /// 鼠标进出翻译二级菜单。
    pub fn submenu_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let effect = self.machine.submenu_hover(hovered);
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

/// 按钮业务回调；由宿主共用操作栏安装事件隔离。
pub type ToolbarItemRunner = Rc<dyn Fn(&mut Window, &mut App)>;

/// 截图工具栏的可离屏按钮输入，不携带具体的按钮渲染实现。
pub struct ToolbarButtonSpec {
    /// 稳定元素标识。
    pub id: String,
    /// 工具或业务动作。
    pub item: ToolbarItem,
    /// 本地化提示。
    pub tooltip: String,
    /// 展开菜单时主按钮暂停提示。
    pub tooltip_enabled: bool,
    /// 当前项高亮。
    pub selected: bool,
    /// 禁用态。
    pub disabled: bool,
    /// 配色语义。
    pub look: Look,
    /// 原有逻辑尺寸。
    pub size: (f32, f32),
    /// 主栏分组提示图标。
    pub indicator: Option<&'static str>,
    /// 主动作分发。
    pub on_click: Option<ToolbarItemRunner>,
    /// 分组右键展开。
    pub on_right_down: Option<ToolbarItemRunner>,
}

impl ToolbarButtonSpec {
    /// 离屏与实际点击共用分发入口，禁用输入不调用宿主回调。
    pub fn dispatch(&self, callback: impl FnOnce(ToolbarItem)) -> bool {
        if self.disabled {
            return false;
        }
        callback(self.item);
        true
    }
}

/// 宿主必须显式提供共用操作栏渲染器；工具栏只负责菜单与布局模型。
pub trait ToolbarRenderer {
    /// 渲染一个图标动作。
    fn button(&self, spec: ToolbarButtonSpec, cx: &App) -> AnyElement;
    /// 渲染组间分隔符。
    fn separator(&self) -> AnyElement;
    /// 可选的首部控件，由宿主共用组件提供。
    fn leading(&self) -> Option<AnyElement> {
        None
    }
    /// 首部控件的完整占用宽度（含间距）。
    fn leading_width(&self) -> f32 {
        0.0
    }
    /// 渲染主栏横向按钮行。
    fn row(&self, gap: f32) -> Div;
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
    /// 翻译目标语言菜单项；为空则翻译行没有二级菜单入口。
    translate_languages: Vec<MenuEntry>,
    /// 选中语言的回调。
    on_translate_language: Option<LanguageHandler>,
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
            translate_languages: Vec::new(),
            on_translate_language: None,
        }
    }

    /// 绑定宿主操作栏渲染器后才成为可绘制元素，禁止回退到旧的手写按钮。
    pub fn with_renderer(self, renderer: Rc<dyn ToolbarRenderer>) -> RenderedToolbar {
        RenderedToolbar {
            toolbar: self,
            renderer,
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

    /// 给分组菜单里的「翻译」行加目标语言二级菜单：行末出现右箭头提示，悬停该行即展开。
    ///
    /// # 参数
    /// - `entries`: 语言项（当前项 `checked`）；为空则不显示二级菜单。
    pub fn translate_languages(mut self, entries: Vec<MenuEntry>) -> Self {
        self.translate_languages = entries;
        self
    }

    /// 选中二级菜单里某个语言的回调，参数是语言项序号；调用前菜单已收起。
    pub fn on_translate_language(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_translate_language = Some(Rc::new(handler));
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

    /// 条目被点击时的完整行为：先记为所属组当前项并收起菜单，再调用回调。
    ///
    /// 条目置灰或对应回调没注册时返回 `None`（按钮不挂点击）。
    fn item_click(&self, item: ToolbarItem) -> Option<ItemRunner> {
        if self.item_disabled(item) {
            return None;
        }
        let run = self.runner(item)?;
        let groups = self.groups.clone();
        Some(Rc::new(move |window, cx| {
            if let Some(groups) = &groups {
                groups.update(cx, |state, gcx| state.choose(item, gcx));
            }
            run(window, cx);
        }))
    }

    /// 条目按钮的外观：激活中的工具为选中色。
    fn item_look(&self, item: ToolbarItem) -> Look {
        match item {
            ToolbarItem::Tool(tool) if tool == self.active_tool => Look::Active,
            _ => Look::Normal,
        }
    }

    /// 按钮定义交给宿主的共用操作栏渲染，尺寸与分组行为仍由工具栏提供。
    pub fn button_spec(
        &self,
        id: String,
        item: ToolbarItem,
        look: Look,
        size: (f32, f32),
    ) -> ToolbarButtonSpec {
        ToolbarButtonSpec {
            id,
            item,
            tooltip: self.label_text(item.label()),
            tooltip_enabled: true,
            selected: matches!(item, ToolbarItem::Tool(tool) if tool == self.active_tool),
            disabled: self.item_disabled(item)
                || matches!(item, ToolbarItem::Action(ToolbarAction::Undo) if !self.can_undo)
                || matches!(item, ToolbarItem::Action(ToolbarAction::Redo) if !self.can_redo),
            look,
            size,
            indicator: None,
            on_click: if ToolbarGroup::of(item).is_some() {
                self.item_click(item)
            } else {
                self.runner(item)
            },
            on_right_down: None,
        }
    }

    /// 独立动作使用同一个渲染入口与禁用判定。
    fn solo_button(
        &self,
        action: ToolbarAction,
        look: Look,
        renderer: &dyn ToolbarRenderer,
        cx: &App,
    ) -> AnyElement {
        let spec = self.button_spec(
            format!("tb-solo-{action:?}"),
            ToolbarItem::Action(action),
            look,
            (BUTTON_SIZE, BUTTON_SIZE),
        );
        renderer.button(spec, cx)
    }

    /// 一个分组的复合按钮：图标 + 小箭头合成同一个按钮，悬停弹出同组菜单。
    fn group_view(
        &self,
        window: &Window,
        group: ToolbarGroup,
        memory: &GroupMemory,
        open: Option<ToolbarGroup>,
        renderer: &dyn ToolbarRenderer,
        cx: &App,
    ) -> AnyElement {
        let current = memory.current(group);
        let is_open = open == Some(group);
        let key = group.key();
        let mut spec = self.button_spec(
            format!("tb-{key}-main"),
            current,
            self.item_look(current),
            (group_width(group), BUTTON_SIZE),
        );
        if !group.has_menu() {
            return renderer.button(spec, cx);
        }
        spec.tooltip = self.label_text(ToolbarLabel::GroupTip(group, current));
        spec.indicator = Some("icons/chevron-down.svg");
        spec.tooltip_enabled = !is_open;
        if let Some(groups) = self.groups.clone() {
            spec.on_right_down = Some(Rc::new(move |_, cx| {
                groups.update(cx, |state, gcx| state.click_arrow(group, gcx));
            }));
        }
        let row = renderer.button(spec, cx);

        let mut popup = Popup::new(SharedString::from(format!("tb-group-{key}")), row)
            .anchor(if self.popup_up {
                Anchor::BottomLeft
            } else {
                Anchor::TopLeft
            })
            .offset(px(POPUP_OFFSET));
        if let Some(groups) = self.groups.clone() {
            // 记下主菜单定位后的左上角，二级菜单据此换边 / 夹边
            let origin = groups.read(cx).menu_origin();
            popup = popup
                .on_position(move |position, _| origin.set(Some(position.bounds.origin)))
                .on_hover(move |hovered, _, cx| {
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
        let has_languages = !self.translate_languages.is_empty()
            && group
                .items()
                .contains(&ToolbarItem::Action(ToolbarAction::Translate));
        // 翻译行末尾的箭头提示占一个图标宽
        let arrow_extra = if has_languages { ROW_ICON } else { 0.0 };
        let width = menu_outer_width(max_label_width(window, &labels, LABEL_FONT_PX)) + arrow_extra;
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
        let translate_open = self
            .groups
            .as_ref()
            .is_some_and(|g| g.read(cx).translate_menu_open());
        for item in group.items().iter().copied() {
            let disabled = self.item_disabled(item);
            let is_current = item == current;
            let is_translate =
                has_languages && item == ToolbarItem::Action(ToolbarAction::Translate);
            let mut content =
                menu_row_content(item.icon_path(), self.label_text(item.label()), is_current);
            if is_translate {
                // 箭头只是“有子菜单”的提示，和文字同属一个点击 / 悬停区域
                content = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .child(content.flex_1())
                    .child(
                        Icon::default()
                            .path(SUBMENU_ARROW_ICON)
                            .with_size(ComponentSize::Size(px(ROW_ICON))),
                    );
            }
            let mut row = Button::new(SharedString::from(format!("tb-{key}-item-{item:?}")))
                .custom(self.item_look(item).variant(cx))
                .with_size(ComponentSize::Size(px(ROW_ICON)))
                .w_full()
                .h(px(MENU_ROW_HEIGHT))
                .disabled(disabled)
                .child(content);
            if let Some(act) = self.item_click(item) {
                row = row.on_click(move |_, window, cx| act(window, cx));
            }
            let mut cell = div()
                .id(SharedString::from(format!("tb-{key}-cell-{item:?}")))
                .w_full()
                .child(row);
            if has_languages && let Some(groups) = self.groups.clone() {
                // 悬停“翻译”行展开二级菜单，悬停其他行收起；移向二级菜单靠外层 wrap 的悬停保持
                cell = cell.on_hover(move |hovered, _, cx| {
                    if let Some(open) = translate_menu_after_hover(*hovered, is_translate) {
                        groups.update(cx, |state, gcx| state.set_translate_menu(open, gcx));
                    }
                });
            }
            menu = menu.child(cell);
        }
        // wrap 里只有主菜单参与布局，大小与位置只由主菜单决定
        let mut wrap = div()
            .id(SharedString::from(format!("tb-menu-wrap-{key}")))
            .relative()
            .flex()
            .flex_row()
            .items_start()
            .child(menu);
        if has_languages && translate_open {
            let menu_size = (width, menu_outer_height(group.items().len()));
            wrap = wrap.child(self.language_submenu(window, menu_size, cx));
        }
        // 悬停判定覆盖主菜单；二级菜单脱离文档流，自带悬停容器保持菜单不收起
        if let Some(groups) = self.groups.clone() {
            wrap = wrap.on_hover(move |hovered, _, cx| {
                groups.update(cx, |state, gcx| state.menu_hover(*hovered, gcx));
            });
        }
        wrap.into_any_element()
    }

    /// 目标语言二级菜单（含悬停容器）：绝对定位在主菜单旁，不影响主菜单布局。
    ///
    /// # 参数
    /// - `menu_size`: 主菜单外框尺寸（宽, 高）。
    fn language_submenu(&self, window: &Window, menu_size: (f32, f32), cx: &App) -> AnyElement {
        let labels: Vec<String> = self
            .translate_languages
            .iter()
            .map(|e| e.label.clone())
            .collect();
        let width = menu_outer_width(max_label_width(window, &labels, LABEL_FONT_PX));
        let viewport = window.viewport_size();
        let placement = place_submenu(&SubmenuInput {
            menu_origin: self
                .groups
                .as_ref()
                .and_then(|g| g.read(cx).menu_origin.get())
                .map(|origin| (f32::from(origin.x), f32::from(origin.y))),
            menu_size,
            sub_size: (width, menu_outer_height(self.translate_languages.len())),
            viewport: (f32::from(viewport.width), f32::from(viewport.height)),
            margin: SUBMENU_WINDOW_MARGIN,
            gap: ITEM_GAP,
            bottom_aligned: self.popup_up,
        });
        let mut holder = div()
            .id("tb-translate-lang-holder")
            .absolute()
            .left(px(placement.left))
            .top(px(placement.top))
            .occlude();
        // 间隔也属于悬停容器，鼠标从翻译行移向二级菜单途中不会收起
        holder = match placement.side {
            SubmenuSide::Right => holder.pl(px(ITEM_GAP)),
            SubmenuSide::Left => holder.pr(px(ITEM_GAP)),
        };
        if let Some(groups) = self.groups.clone() {
            holder = holder.on_hover(move |hovered, _, cx| {
                groups.update(cx, |state, gcx| state.submenu_hover(*hovered, gcx));
            });
        }
        holder
            .child(self.language_menu(width, cx))
            .into_any_element()
    }

    /// 目标语言二级菜单本体：选中后先收起菜单，再回调语言序号。
    fn language_menu(&self, width: f32, cx: &App) -> AnyElement {
        let mut menu = div()
            .id("tb-translate-lang-menu")
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
        for (index, entry) in self.translate_languages.iter().enumerate() {
            let content = menu_row_content(entry.icon, entry.label.clone(), entry.checked);
            let mut row = Button::new(SharedString::from(format!(
                "tb-translate-lang-{}",
                entry.id
            )))
            .custom(Look::Normal.variant(cx))
            .with_size(ComponentSize::Size(px(ROW_ICON)))
            .w_full()
            .h(px(MENU_ROW_HEIGHT))
            .child(content);
            if let Some(handler) = self.on_translate_language.clone() {
                let groups = self.groups.clone();
                row = row.on_click(move |_, window, cx| {
                    if let Some(groups) = &groups {
                        groups.update(cx, |state, gcx| state.dismiss(gcx));
                    }
                    handler(index, window, cx);
                });
            }
            menu = menu.child(row);
        }
        menu.into_any_element()
    }
}

/// 显式注入渲染器后的工具栏元素，避免 widgets 反向依赖应用层。
pub struct RenderedToolbar {
    toolbar: ScreenshotToolbar,
    renderer: Rc<dyn ToolbarRenderer>,
}

impl RenderOnce for RenderedToolbar {
    /// 渲染工具栏。
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let renderer = self.renderer.as_ref();
        let this = self.toolbar;
        // 当前激活的工具（可能由快捷键切换）要成为它所在组的当前项；静默同步，不触发重绘
        if let Some(groups) = &this.groups {
            let active = ToolbarItem::Tool(this.active_tool);
            groups.update(cx, |state, _| state.remember(active));
        }
        let (memory, open) = match &this.groups {
            Some(groups) => {
                let state = groups.read(cx);
                (state.memory.clone(), state.open_group())
            }
            None => (GroupMemory::default(), None),
        };

        let (width, height) = toolbar_logical_size(this.show_tools);
        let mut bar = div()
            .id(this.id.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SECTION_GAP))
            .px(px(BAR_PADDING_X))
            .py(px(BAR_PADDING_Y))
            .w(px(width as f32 + renderer.leading_width()))
            .h(px(height as f32))
            .rounded_md()
            .bg(rgba(COLOR_BAR_BG))
            .shadow_lg()
            .border(px(BAR_BORDER))
            .border_color(rgba(COLOR_BAR_BORDER))
            // 点击工具栏不应穿透到下层选区，否则会误触发重新框选
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation());
        if let Some(leading) = renderer.leading() {
            bar = bar.child(div().flex_none().mr(px(-SECTION_GAP)).child(leading));
        }
        let mut section = renderer.row(ITEM_GAP);
        for slot in toolbar_slots(this.show_tools) {
            match *slot {
                ToolbarSlot::Group(group) => {
                    section =
                        section.child(this.group_view(window, group, &memory, open, renderer, cx));
                }
                ToolbarSlot::Action(action) => {
                    let look = match action {
                        ToolbarAction::Copy => Look::Primary,
                        ToolbarAction::Cancel => Look::Danger,
                        _ => Look::Normal,
                    };
                    section = section.child(this.solo_button(action, look, renderer, cx));
                }
                ToolbarSlot::Separator => {
                    bar = bar.child(section).child(renderer.separator());
                    section = renderer.row(ITEM_GAP);
                }
            }
        }
        bar.child(section)
    }
}

impl IntoElement for RenderedToolbar {
    type Element = ViewElement<Self>;

    /// 将已绑定渲染器的工具栏转换为元素。
    #[track_caller]
    fn into_element(self) -> Self::Element {
        ViewElement::new(self)
    }

    /// 擦除元素类型供宿主组合布局。
    #[track_caller]
    fn into_any_element(self) -> AnyElement {
        Element::into_any(self.into_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 七组菜单实际渲染入口必须保留图标文字、选中槽、原尺寸与点击路径。
    #[test]
    fn toolbar_popup_options_keep_icon_and_text() {
        let source = include_str!("toolbar.rs");
        let menu = source
            .split("    fn menu_view(")
            .nth(1)
            .unwrap()
            .split("    fn language_submenu(")
            .next()
            .unwrap();
        assert!(menu.contains("for item in group.items().iter().copied()"));
        assert!(menu.contains(
            "menu_row_content(item.icon_path(), self.label_text(item.label()), is_current)"
        ));
        for required in [
            ".custom(self.item_look(item).variant(cx))",
            ".w_full()",
            ".h(px(MENU_ROW_HEIGHT))",
            ".disabled(disabled)",
            "self.item_click(item)",
            "row.on_click(",
            "cell.on_hover(",
            "self.language_submenu(window, menu_size, cx)",
        ] {
            assert!(menu.contains(required), "缺少原菜单行为：{required}");
        }
        assert!(!menu.contains("renderer"));
        assert!(!menu.contains("button_spec"));
        let content = source
            .split("pub(crate) fn menu_row_content(")
            .nth(1)
            .unwrap()
            .split("/// 按钮外观")
            .next()
            .unwrap();
        for required in [
            ".path(icon)",
            ".child(label)",
            ".child(menu_check_slot(checked))",
        ] {
            assert!(content.contains(required));
        }
        let toolbar = ScreenshotToolbar::new("popup-test");
        for group in ToolbarGroup::ALL {
            for item in group.items() {
                assert!(!item.icon_path().is_empty());
                assert!(!toolbar.label_text(item.label()).is_empty());
                let _ = menu_row_content(item.icon_path(), toolbar.label_text(item.label()), true)
                    .into_any_element();
            }
        }
    }

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

    /// 悬停「翻译」行展开二级菜单，悬停其他行收起，移出保持不变。
    #[test]
    fn translate_submenu_follows_row_hover() {
        assert_eq!(translate_menu_after_hover(true, true), Some(true));
        assert_eq!(translate_menu_after_hover(true, false), Some(false));
        assert_eq!(translate_menu_after_hover(false, true), None);
        assert_eq!(translate_menu_after_hover(false, false), None);
    }

    /// 条目点击行为：置灰或没注册回调时不挂点击，有回调才返回。
    #[test]
    fn item_click_requires_enabled_and_handler() {
        let copy = ToolbarItem::Action(ToolbarAction::Copy);
        let tool = ToolbarItem::Tool(AnnotationTool::Arrow);
        let bare = ScreenshotToolbar::new("t");
        assert!(bare.item_click(copy).is_none());
        assert!(bare.item_click(tool).is_none());
        let wired = ScreenshotToolbar::new("t")
            .on_action(|_, _, _| {})
            .on_tool_change(|_, _, _| {})
            .disabled_actions(&[ToolbarAction::Copy]);
        assert!(wired.item_click(copy).is_none());
        assert!(wired.item_click(tool).is_some());
        assert!(
            wired
                .item_click(ToolbarItem::Action(ToolbarAction::Save))
                .is_some()
        );
    }
}
