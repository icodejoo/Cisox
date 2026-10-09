//! 截图工具栏的分组模型：分组表、图标清单、尺寸计算与悬停弹出状态机。
//!
//! 本模块只含纯逻辑（不碰 GPUI 元素），可离屏确定性测试；
//! 渲染与计时器由 [`crate::toolbar`] 里的 `ToolbarGroups` 实体承担。

use crate::toolbar::{AnnotationTool, TOOLBAR_ACTIONS, TOOLBAR_TOOLS, ToolbarAction};

/// 工具栏上的一个位置：标注工具或动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarItem {
    /// 标注工具。
    Tool(AnnotationTool),
    /// 动作（含撤销 / 重做）。
    Action(ToolbarAction),
}

/// 工具栏分组：每组一个「当前项主按钮 + 下箭头」复合按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarGroup {
    /// 形状：矩形、椭圆、箭头、直线。
    Shape,
    /// 画笔：画笔、荧光笔。
    Pen,
    /// 标记：序号、文字、水印。
    Mark,
    /// 滤镜：马赛克、模糊、自动滤镜。
    Filter,
    /// 编辑：橡皮、选对象、聚光灯。
    Edit,
    /// 识别：OCR、翻译、表格、公式。
    Recognize,
    /// 输出：贴图、长图、录屏、保存。
    Output,
}

/// 形状组成员。
const SHAPE_ITEMS: [ToolbarItem; 4] = [
    ToolbarItem::Tool(AnnotationTool::Rectangle),
    ToolbarItem::Tool(AnnotationTool::Ellipse),
    ToolbarItem::Tool(AnnotationTool::Arrow),
    ToolbarItem::Tool(AnnotationTool::Line),
];
/// 画笔组成员。
const PEN_ITEMS: [ToolbarItem; 2] = [
    ToolbarItem::Tool(AnnotationTool::Pencil),
    ToolbarItem::Tool(AnnotationTool::Highlighter),
];
/// 标记组成员。
const MARK_ITEMS: [ToolbarItem; 3] = [
    ToolbarItem::Tool(AnnotationTool::Counter),
    ToolbarItem::Tool(AnnotationTool::Text),
    ToolbarItem::Tool(AnnotationTool::Watermark),
];
/// 滤镜组成员。
const FILTER_ITEMS: [ToolbarItem; 3] = [
    ToolbarItem::Tool(AnnotationTool::Mosaic),
    ToolbarItem::Tool(AnnotationTool::Blur),
    ToolbarItem::Tool(AnnotationTool::AutoFilter),
];
/// 编辑组成员。
const EDIT_ITEMS: [ToolbarItem; 3] = [
    ToolbarItem::Tool(AnnotationTool::Eraser),
    ToolbarItem::Tool(AnnotationTool::Select),
    ToolbarItem::Tool(AnnotationTool::Spotlight),
];
/// 识别组成员。
const RECOGNIZE_ITEMS: [ToolbarItem; 4] = [
    ToolbarItem::Action(ToolbarAction::Ocr),
    ToolbarItem::Action(ToolbarAction::Translate),
    ToolbarItem::Action(ToolbarAction::Table),
    ToolbarItem::Action(ToolbarAction::Latex),
];
/// 输出组成员。
const OUTPUT_ITEMS: [ToolbarItem; 4] = [
    ToolbarItem::Action(ToolbarAction::Pin),
    ToolbarItem::Action(ToolbarAction::ScrollCapture),
    ToolbarItem::Action(ToolbarAction::Record),
    ToolbarItem::Action(ToolbarAction::Save),
];

impl ToolbarGroup {
    /// 全部分组，按工具栏上的先后顺序（先标注工具组，后动作组）。
    pub const ALL: [ToolbarGroup; 7] = [
        Self::Shape,
        Self::Pen,
        Self::Mark,
        Self::Filter,
        Self::Edit,
        Self::Recognize,
        Self::Output,
    ];

    /// 标注工具组（撤销 / 重做之前）。
    pub const TOOL_GROUPS: [ToolbarGroup; 5] =
        [Self::Shape, Self::Pen, Self::Mark, Self::Filter, Self::Edit];

    /// 动作组（撤销 / 重做之后，复制 / 取消之前）。
    pub const ACTION_GROUPS: [ToolbarGroup; 2] = [Self::Recognize, Self::Output];

    /// 分组在 [`Self::ALL`] 里的序号。
    pub const fn index(self) -> usize {
        match self {
            Self::Shape => 0,
            Self::Pen => 1,
            Self::Mark => 2,
            Self::Filter => 3,
            Self::Edit => 4,
            Self::Recognize => 5,
            Self::Output => 6,
        }
    }

    /// 分组成员（弹出菜单里的顺序，第一项是默认当前项）。
    pub const fn items(self) -> &'static [ToolbarItem] {
        match self {
            Self::Shape => &SHAPE_ITEMS,
            Self::Pen => &PEN_ITEMS,
            Self::Mark => &MARK_ITEMS,
            Self::Filter => &FILTER_ITEMS,
            Self::Edit => &EDIT_ITEMS,
            Self::Recognize => &RECOGNIZE_ITEMS,
            Self::Output => &OUTPUT_ITEMS,
        }
    }

    /// 元素 id 用的稳定短名。
    pub const fn key(self) -> &'static str {
        match self {
            Self::Shape => "shape",
            Self::Pen => "pen",
            Self::Mark => "mark",
            Self::Filter => "filter",
            Self::Edit => "edit",
            Self::Recognize => "recognize",
            Self::Output => "output",
        }
    }

    /// 条目所属分组；撤销 / 重做 / 复制 / 取消 / 无工具不属于任何组。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::{AnnotationTool, ToolbarGroup, ToolbarItem};
    /// let blur = ToolbarItem::Tool(AnnotationTool::Blur);
    /// assert_eq!(ToolbarGroup::of(blur), Some(ToolbarGroup::Filter));
    /// ```
    pub fn of(item: ToolbarItem) -> Option<ToolbarGroup> {
        Self::ALL.into_iter().find(|g| g.items().contains(&item))
    }

    /// 组内是否需要下拉：只有一项时退化为普通图标按钮。
    pub const fn has_menu(self) -> bool {
        self.items().len() > 1
    }
}

impl ToolbarItem {
    /// 条目对应的图标资源路径（`icons/antd/*`、`icons/snow/*` 或 Lucide `icons/*`）。
    ///
    /// 来源优先级：antd outlined → Lucide → 自绘；清单见各分支注释。
    pub const fn icon_path(self) -> &'static str {
        match self {
            Self::Tool(tool) => match tool {
                // antd：border
                AnnotationTool::Rectangle => "icons/antd/border.svg",
                // 自绘
                AnnotationTool::Ellipse => "icons/snow/ellipse.svg",
                AnnotationTool::Arrow => "icons/snow/arrow.svg",
                AnnotationTool::Line => "icons/snow/line.svg",
                // antd
                AnnotationTool::Pencil => "icons/antd/edit.svg",
                AnnotationTool::Highlighter => "icons/antd/highlight.svg",
                AnnotationTool::Counter => "icons/antd/number.svg",
                AnnotationTool::Text => "icons/antd/font-size.svg",
                AnnotationTool::Watermark => "icons/antd/signature.svg",
                AnnotationTool::Mosaic => "icons/antd/appstore.svg",
                // 自绘
                AnnotationTool::Blur => "icons/snow/blur.svg",
                // antd
                AnnotationTool::AutoFilter => "icons/antd/filter.svg",
                AnnotationTool::Eraser => "icons/antd/clear.svg",
                AnnotationTool::Select | AnnotationTool::None => "icons/antd/select.svg",
                AnnotationTool::Spotlight => "icons/antd/bulb.svg",
            },
            Self::Action(action) => match action {
                ToolbarAction::Undo => "icons/antd/undo.svg",
                ToolbarAction::Redo => "icons/antd/redo.svg",
                ToolbarAction::Pin => "icons/antd/pushpin.svg",
                ToolbarAction::Ocr => "icons/antd/scan.svg",
                ToolbarAction::Translate => "icons/antd/translation.svg",
                ToolbarAction::Table => "icons/antd/table.svg",
                ToolbarAction::Latex => "icons/antd/function.svg",
                ToolbarAction::Record => "icons/antd/video-camera.svg",
                // 自绘
                ToolbarAction::ScrollCapture => "icons/snow/scroll-capture.svg",
                ToolbarAction::Save => "icons/antd/save.svg",
                ToolbarAction::Copy => "icons/antd/copy.svg",
                ToolbarAction::Cancel => "icons/antd/close.svg",
            },
        }
    }
}

/// 下箭头图标（Lucide）。
pub const ARROW_ICON: &str = "icons/chevron-down.svg";
/// 菜单里当前项的选中标记图标（自绘加粗版，描边比 Lucide 的 check 重）。
pub const CHECK_ICON: &str = "icons/snow/check.svg";
/// 选中标记的颜色（绿色，深色底上够醒目；各下拉菜单共用）。
pub const COLOR_CHECK_GREEN: u32 = 0x52C41A;
/// 选中标记图标边长（逻辑像素）；非当前项也占同样宽度以保持对齐。
pub const CHECK_ICON_SIZE: f32 = 18.0;

/// 工具栏用到的全部图标路径（含箭头与选中标记），供资源可解析性测试。
pub fn all_icon_paths() -> Vec<&'static str> {
    let mut paths: Vec<&'static str> = all_items().iter().map(|i| i.icon_path()).collect();
    paths.extend([ARROW_ICON, CHECK_ICON]);
    paths
}

/// 工具栏上全部位置（工具 + 撤销 / 重做 + 动作）。
pub fn all_items() -> Vec<ToolbarItem> {
    TOOLBAR_TOOLS
        .iter()
        .map(|t| ToolbarItem::Tool(*t))
        .chain([ToolbarAction::Undo, ToolbarAction::Redo].map(ToolbarItem::Action))
        .chain(TOOLBAR_ACTIONS.iter().map(|(_, a)| ToolbarItem::Action(*a)))
        .collect()
}

/// 单个图标按钮的边长（逻辑像素）。
pub const BUTTON_SIZE: f32 = 28.0;
/// 复合按钮里图标与小箭头的间距。
pub const ARROW_GAP: f32 = 2.0;
/// 复合按钮下箭头部分占的宽度（并入同一个按钮，只影响总宽）。
pub const ARROW_WIDTH: f32 = 12.0;
/// 同一区段内相邻按钮的间距。
pub const ITEM_GAP: f32 = 2.0;
/// 区段（含分割线）之间的间距。
pub const SECTION_GAP: f32 = 6.0;
/// 分割线宽度。
pub const DIVIDER_WIDTH: f32 = 1.0;
/// 工具栏左右内边距。
pub const BAR_PADDING_X: f32 = 8.0;
/// 工具栏上下内边距。
pub const BAR_PADDING_Y: f32 = 4.0;
/// 工具栏边框宽度（单侧）。
pub const BAR_BORDER: f32 = 1.0;
/// 弹出菜单与触发按钮的间距。
pub const POPUP_OFFSET: f32 = 2.0;
/// 弹出菜单每一行的高度。
pub const MENU_ROW_HEIGHT: f32 = 28.0;
/// 弹出菜单内边距（单侧）。
pub const MENU_PADDING: f32 = 4.0;

/// 一个分组占用的宽度：主按钮 + 下箭头（只有一项时只有主按钮）。
pub fn group_width(group: ToolbarGroup) -> f32 {
    if group.has_menu() {
        BUTTON_SIZE + ARROW_WIDTH
    } else {
        BUTTON_SIZE
    }
}

/// 一行等间距元素的总宽度。
fn row_width(widths: &[f32]) -> f32 {
    widths.iter().sum::<f32>() + ITEM_GAP * widths.len().saturating_sub(1) as f32
}

/// 弹出菜单的高度（逻辑像素），用于判断向上还是向下展开。
///
/// # 参数
/// - `group`：分组。
pub fn menu_height(group: ToolbarGroup) -> f32 {
    let rows = group.items().len() as f32;
    rows * MENU_ROW_HEIGHT + ITEM_GAP * (rows - 1.0).max(0.0) + MENU_PADDING * 2.0 + 2.0
}

/// 最高的弹出菜单高度（所有分组里最大）。
pub fn max_menu_height() -> f32 {
    ToolbarGroup::ALL
        .into_iter()
        .map(menu_height)
        .fold(0.0, f32::max)
}

/// 工具栏的逻辑尺寸（宽, 高），与实际渲染的固定布局一一对应，用于定位与命中避让。
///
/// # 参数
/// - `show_tools`：是否显示标注工具组与撤销 / 重做（录屏 / 长图模式隐藏）。
///
/// # 返回
/// `(宽, 高)`，向上取整的逻辑像素。
pub fn toolbar_logical_size(show_tools: bool) -> (i32, i32) {
    let mut action_widths: Vec<f32> = ToolbarGroup::ACTION_GROUPS
        .into_iter()
        .map(group_width)
        .collect();
    // 复制、取消两个独立按钮
    action_widths.extend([BUTTON_SIZE, BUTTON_SIZE]);
    let actions = row_width(&action_widths);
    let content = if show_tools {
        let tool_widths: Vec<f32> = ToolbarGroup::TOOL_GROUPS
            .into_iter()
            .map(group_width)
            .collect();
        let tools = row_width(&tool_widths);
        let history = row_width(&[BUTTON_SIZE, BUTTON_SIZE]);
        // tools | history | actions：两条分割线、四段间距
        tools + history + actions + DIVIDER_WIDTH * 2.0 + SECTION_GAP * 4.0
    } else {
        actions
    };
    let width = content + BAR_PADDING_X * 2.0 + BAR_BORDER * 2.0;
    let height = BUTTON_SIZE + BAR_PADDING_Y * 2.0 + BAR_BORDER * 2.0;
    (width.ceil() as i32, height.ceil() as i32)
}

/// 悬停打开的延迟（毫秒），防止鼠标划过时误弹。
pub const HOVER_OPEN_DELAY_MS: u64 = 150;
/// 离开后收起的延迟（毫秒），给鼠标从按钮移向菜单留出时间。
pub const HOVER_CLOSE_DELAY_MS: u64 = 150;

/// 状态机一次事件处理后需要调用方做的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupEffect {
    /// 无需处理。
    Idle,
    /// 可见状态变了，需要重绘。
    Redraw,
    /// 启动一个计时器；到点后用同一个 `token` 调用 [`PopupMachine::fire`]。
    Timer {
        /// 计时器令牌；过期（后来又有事件）则 `fire` 不起作用。
        token: u64,
        /// 延迟毫秒数。
        delay_ms: u64,
    },
}

/// 复合按钮下拉的悬停 / 点击状态机（纯逻辑，不含计时器本身）。
#[derive(Debug, Default, Clone)]
pub struct PopupMachine {
    /// 当前弹出的分组。
    open: Option<ToolbarGroup>,
    /// 鼠标正悬在哪个分组的触发区（主按钮 + 箭头）。
    hover_trigger: Option<ToolbarGroup>,
    /// 鼠标是否悬在弹出菜单上。
    hover_menu: bool,
    /// 计时器令牌；每次事件递增使旧计时器作废。
    token: u64,
}

impl PopupMachine {
    /// 当前弹出的分组。
    pub fn open(&self) -> Option<ToolbarGroup> {
        self.open
    }

    /// 作废旧计时器并返回新令牌。
    fn bump(&mut self) -> u64 {
        self.token += 1;
        self.token
    }

    /// 鼠标已全部离开且还开着菜单时，安排一次延迟收起。
    fn close_if_idle(&mut self) -> PopupEffect {
        if self.open.is_some() && self.hover_trigger.is_none() && !self.hover_menu {
            PopupEffect::Timer {
                token: self.bump(),
                delay_ms: HOVER_CLOSE_DELAY_MS,
            }
        } else {
            PopupEffect::Idle
        }
    }

    /// 鼠标进出某分组的触发区。
    ///
    /// 进入：菜单未开则延迟打开；已开着别的组则立即切换；已是本组则保持。
    /// 离开：若菜单也没被悬停，延迟收起。
    pub fn trigger_hover(&mut self, group: ToolbarGroup, hovered: bool) -> PopupEffect {
        self.bump();
        if hovered {
            self.hover_trigger = Some(group);
            return match self.open {
                Some(g) if g == group => PopupEffect::Idle,
                Some(_) => {
                    self.open = Some(group);
                    PopupEffect::Redraw
                }
                None => PopupEffect::Timer {
                    token: self.token,
                    delay_ms: HOVER_OPEN_DELAY_MS,
                },
            };
        }
        if self.hover_trigger == Some(group) {
            self.hover_trigger = None;
        }
        self.close_if_idle()
    }

    /// 鼠标进出弹出菜单：进入保持打开，离开则（在触发区也没悬停时）延迟收起。
    pub fn menu_hover(&mut self, hovered: bool) -> PopupEffect {
        self.bump();
        self.hover_menu = hovered;
        if hovered {
            PopupEffect::Idle
        } else {
            self.close_if_idle()
        }
    }

    /// 点击下箭头：立即切换本组菜单的开 / 关。
    pub fn click_arrow(&mut self, group: ToolbarGroup) -> PopupEffect {
        self.bump();
        self.open = if self.open == Some(group) {
            None
        } else {
            Some(group)
        };
        if self.open.is_none() {
            self.hover_menu = false;
        }
        PopupEffect::Redraw
    }

    /// 立即收起（选中某项、点击主按钮、工具栏消失时调用）。
    pub fn close(&mut self) -> PopupEffect {
        self.bump();
        self.hover_menu = false;
        if self.open.take().is_some() {
            PopupEffect::Redraw
        } else {
            PopupEffect::Idle
        }
    }

    /// 计时器到点：令牌仍有效才按当前悬停情况打开 / 收起。
    pub fn fire(&mut self, token: u64) -> PopupEffect {
        if token != self.token {
            return PopupEffect::Idle;
        }
        match (self.hover_trigger, self.open) {
            (Some(g), open) if open != Some(g) => {
                self.open = Some(g);
                PopupEffect::Redraw
            }
            (None, Some(_)) if !self.hover_menu => {
                self.open = None;
                PopupEffect::Redraw
            }
            _ => PopupEffect::Idle,
        }
    }
}

/// 每组的「当前项」记忆（最近使用的一项，默认第一项）。
#[derive(Debug, Clone, Default)]
pub struct GroupMemory {
    /// 每组当前项在组内的序号，按 [`ToolbarGroup::ALL`] 排列。
    current: [usize; 7],
}

impl GroupMemory {
    /// 某组当前项。
    pub fn current(&self, group: ToolbarGroup) -> ToolbarItem {
        group.items()[self.current[group.index()].min(group.items().len() - 1)]
    }

    /// 记住条目为所属组的当前项；不属于任何组的条目忽略。
    ///
    /// # 返回
    /// 当前项发生了变化时为 `true`。
    pub fn remember(&mut self, item: ToolbarItem) -> bool {
        let Some(group) = ToolbarGroup::of(item) else {
            return false;
        };
        let Some(pos) = group.items().iter().position(|i| *i == item) else {
            return false;
        };
        let changed = self.current[group.index()] != pos;
        self.current[group.index()] = pos;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 分组表覆盖全部 TOOLBAR_TOOLS / TOOLBAR_ACTIONS，不重不漏，且不含撤销 / 重做 / 复制 / 取消。
    #[test]
    fn groups_cover_tools_and_actions_exactly() {
        let mut seen = HashSet::new();
        for group in ToolbarGroup::ALL {
            for item in group.items() {
                assert!(seen.insert(*item), "重复: {item:?}");
            }
        }
        let expected: HashSet<ToolbarItem> = TOOLBAR_TOOLS
            .iter()
            .map(|t| ToolbarItem::Tool(*t))
            .chain(
                TOOLBAR_ACTIONS
                    .iter()
                    .map(|(_, a)| *a)
                    .filter(|a| !matches!(a, ToolbarAction::Copy | ToolbarAction::Cancel))
                    .map(ToolbarItem::Action),
            )
            .collect();
        assert_eq!(seen, expected);
        for solo in [
            ToolbarAction::Undo,
            ToolbarAction::Redo,
            ToolbarAction::Copy,
            ToolbarAction::Cancel,
        ] {
            assert_eq!(ToolbarGroup::of(ToolbarItem::Action(solo)), None);
        }
    }

    /// 分组成员数量与顺序符合需求，序号与 ALL 一致。
    #[test]
    fn group_layout_matches_spec() {
        let lens: Vec<usize> = ToolbarGroup::ALL.iter().map(|g| g.items().len()).collect();
        assert_eq!(lens, [4, 2, 3, 3, 3, 4, 4]);
        for (i, g) in ToolbarGroup::ALL.iter().enumerate() {
            assert_eq!(g.index(), i);
        }
        assert_eq!(
            ToolbarGroup::TOOL_GROUPS
                .iter()
                .chain(&ToolbarGroup::ACTION_GROUPS)
                .copied()
                .collect::<Vec<_>>(),
            ToolbarGroup::ALL
        );
        assert_eq!(
            ToolbarGroup::Output.items()[1],
            ToolbarItem::Action(ToolbarAction::ScrollCapture)
        );
        assert!(ToolbarGroup::ALL.iter().all(|g| g.has_menu()));
    }

    /// 图标清单每一项都能解析到资源，且来源前缀合法。
    #[test]
    fn every_icon_resolves() {
        let paths = all_icon_paths();
        assert!(paths.len() >= 27);
        for path in paths {
            assert!(
                path.starts_with("icons/antd/")
                    || path.starts_with("icons/snow/")
                    || path.starts_with("icons/"),
                "{path}"
            );
            assert!(
                snow_ui_shell::ui::icon_asset_exists(path),
                "资源缺失: {path}"
            );
        }
    }

    /// 选中标记：绿色常量存在、图标可解析、占位宽度为正。
    #[test]
    fn check_mark_is_green_and_resolves() {
        assert_eq!(COLOR_CHECK_GREEN, 0x52C41A);
        const { assert!(CHECK_ICON_SIZE > 0.0) };
        assert!(snow_ui_shell::ui::icon_asset_exists(CHECK_ICON));
    }

    /// 全部 27 个位置都有图标，自绘图标只用在缺口位置。
    #[test]
    fn icon_sources_are_as_planned() {
        let items = all_items();
        assert_eq!(items.len(), 27);
        let own: Vec<_> = items
            .iter()
            .filter(|i| i.icon_path().starts_with("icons/snow/"))
            .collect();
        assert_eq!(own.len(), 5);
    }

    /// 组当前项：默认第一项，记住最近使用，跨组互不影响，组外条目忽略。
    #[test]
    fn group_memory_remembers_per_group() {
        let mut m = GroupMemory::default();
        assert_eq!(
            m.current(ToolbarGroup::Shape),
            ToolbarItem::Tool(AnnotationTool::Rectangle)
        );
        assert!(m.remember(ToolbarItem::Tool(AnnotationTool::Arrow)));
        assert!(!m.remember(ToolbarItem::Tool(AnnotationTool::Arrow)));
        assert_eq!(
            m.current(ToolbarGroup::Shape),
            ToolbarItem::Tool(AnnotationTool::Arrow)
        );
        assert_eq!(
            m.current(ToolbarGroup::Pen),
            ToolbarItem::Tool(AnnotationTool::Pencil)
        );
        assert!(!m.remember(ToolbarItem::Action(ToolbarAction::Copy)));
        assert!(m.remember(ToolbarItem::Action(ToolbarAction::Record)));
        assert_eq!(
            m.current(ToolbarGroup::Output),
            ToolbarItem::Action(ToolbarAction::Record)
        );
    }

    /// 工具栏宽度由分组表推出：隐藏工具后变窄，并与固定布局常量吻合。
    #[test]
    fn toolbar_size_follows_layout() {
        let (full_w, h) = toolbar_logical_size(true);
        let (narrow_w, narrow_h) = toolbar_logical_size(false);
        assert_eq!(h, narrow_h);
        assert_eq!(h, 38);
        // tools 5 组 * 40 + 4 * 2 = 208；history 58；actions 2 * 40 + 2 * 28 + 3 * 2 = 142
        assert_eq!(narrow_w, 142 + 16 + 2);
        assert_eq!(full_w, 208 + 58 + 142 + 2 + 24 + 16 + 2);
        assert!(full_w < 1120, "应明显小于旧版估算宽度");
    }

    /// 菜单高度按行数增长，最高的是四项组。
    #[test]
    fn menu_height_by_rows() {
        assert!(menu_height(ToolbarGroup::Pen) < menu_height(ToolbarGroup::Shape));
        assert_eq!(menu_height(ToolbarGroup::Shape), max_menu_height());
    }

    /// 取出计时器令牌（测试辅助）。
    fn timer(effect: PopupEffect) -> (u64, u64) {
        match effect {
            PopupEffect::Timer { token, delay_ms } => (token, delay_ms),
            other => panic!("期望计时器，得到 {other:?}"),
        }
    }

    /// 悬停延迟打开：到点才弹出，期间离开则作废。
    #[test]
    fn hover_opens_after_delay_and_cancels() {
        let mut m = PopupMachine::default();
        let (t, delay) = timer(m.trigger_hover(ToolbarGroup::Shape, true));
        assert_eq!(delay, HOVER_OPEN_DELAY_MS);
        assert_eq!(m.open(), None);
        assert_eq!(m.fire(t), PopupEffect::Redraw);
        assert_eq!(m.open(), Some(ToolbarGroup::Shape));

        // 另一次：延迟期间离开，旧计时器作废
        let mut m = PopupMachine::default();
        let (t, _) = timer(m.trigger_hover(ToolbarGroup::Pen, true));
        assert_eq!(m.trigger_hover(ToolbarGroup::Pen, false), PopupEffect::Idle);
        assert_eq!(m.fire(t), PopupEffect::Idle);
        assert_eq!(m.open(), None);
    }

    /// 弹出后移向菜单不应收起；离开菜单后延迟收起。
    #[test]
    fn moving_to_menu_keeps_open() {
        let mut m = PopupMachine::default();
        let (t, _) = timer(m.trigger_hover(ToolbarGroup::Filter, true));
        m.fire(t);
        // 离开触发区：安排收起
        let (close_t, delay) = timer(m.trigger_hover(ToolbarGroup::Filter, false));
        assert_eq!(delay, HOVER_CLOSE_DELAY_MS);
        // 在到点前进入菜单：旧收起计时器作废
        assert_eq!(m.menu_hover(true), PopupEffect::Idle);
        assert_eq!(m.fire(close_t), PopupEffect::Idle);
        assert_eq!(m.open(), Some(ToolbarGroup::Filter));
        // 离开菜单：再次安排收起并生效
        let (t2, _) = timer(m.menu_hover(false));
        assert_eq!(m.fire(t2), PopupEffect::Redraw);
        assert_eq!(m.open(), None);
    }

    /// 已开着一组时，悬停另一组立即切换，无需再等延迟。
    #[test]
    fn switching_groups_is_immediate() {
        let mut m = PopupMachine::default();
        let (t, _) = timer(m.trigger_hover(ToolbarGroup::Shape, true));
        m.fire(t);
        // 先离开 Shape（收起计时中），再进入 Pen
        let (stale, _) = timer(m.trigger_hover(ToolbarGroup::Shape, false));
        assert_eq!(
            m.trigger_hover(ToolbarGroup::Pen, true),
            PopupEffect::Redraw
        );
        assert_eq!(m.open(), Some(ToolbarGroup::Pen));
        assert_eq!(m.fire(stale), PopupEffect::Idle);
        assert_eq!(m.open(), Some(ToolbarGroup::Pen));
    }

    /// 点击箭头立即开关；选中后 close 立即收起并清悬停状态。
    #[test]
    fn click_toggles_and_close_resets() {
        let mut m = PopupMachine::default();
        assert_eq!(m.click_arrow(ToolbarGroup::Edit), PopupEffect::Redraw);
        assert_eq!(m.open(), Some(ToolbarGroup::Edit));
        assert_eq!(m.click_arrow(ToolbarGroup::Edit), PopupEffect::Redraw);
        assert_eq!(m.open(), None);

        m.click_arrow(ToolbarGroup::Mark);
        m.menu_hover(true);
        assert_eq!(m.close(), PopupEffect::Redraw);
        assert_eq!(m.open(), None);
        assert_eq!(m.close(), PopupEffect::Idle);
        // 关闭后菜单悬停标记已清，下次打开离开能正常收起
        m.click_arrow(ToolbarGroup::Mark);
        let (t, _) = timer(m.trigger_hover(ToolbarGroup::Mark, false));
        assert_eq!(m.fire(t), PopupEffect::Redraw);
    }

    /// 鼠标仍在触发区时，菜单不会被误收起。
    #[test]
    fn stays_open_while_trigger_hovered() {
        let mut m = PopupMachine::default();
        m.click_arrow(ToolbarGroup::Output);
        assert_eq!(
            m.trigger_hover(ToolbarGroup::Output, true),
            PopupEffect::Idle
        );
        // 菜单从未被悬停，离开菜单也不会安排收起
        assert_eq!(m.menu_hover(false), PopupEffect::Idle);
        assert_eq!(m.fire(m.token), PopupEffect::Idle);
        assert_eq!(m.open(), Some(ToolbarGroup::Output));
    }
}
