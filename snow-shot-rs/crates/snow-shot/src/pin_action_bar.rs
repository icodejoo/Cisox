//! 共用图标操作栏：宿主参数化，统一事件隔离、外部优先布局与内部抓手。

use snow_ui::ui::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use snow_ui::ui::component::{Disableable, Icon, Selectable, Sizable, Size as ButtonSize};
use snow_ui::ui::*;
use std::cell::Cell;
use std::rc::Rc;

/// 操作按钮逻辑宽度。
pub const BUTTON_WIDTH: f32 = 24.0;
/// 按钮间逻辑间距。
pub const BUTTON_GAP: f32 = 4.0;
/// 图标按钮的实际逻辑高度；显式传给组件库，避免主题 rem 改变几何与命中尺寸。
pub const BUTTON_HEIGHT: f32 = 24.0;
/// 操作条上下各留 4px，既保持紧凑也留出空白拖窗区域。
pub const BAR_VERTICAL_PADDING: f32 = 4.0;

/// 从实际按钮高度与上下内边距计算条高，所有宿主与原生区域共用。
pub(crate) const fn bar_height() -> f32 {
    BUTTON_HEIGHT + 2.0 * BAR_VERTICAL_PADDING
}
/// 抓手逻辑宽度。
pub const GRIP_WIDTH: f32 = 24.0;
/// 操作条内边距，也是内部放置时距选区的边距。
pub const BAR_PADDING: f32 = 4.0;
/// 门面按需提供现有 Lucide 抓手图标。
use snow_ui::ui::GRIP_VERTICAL_ICON as GRIP_ICON;

/// 操作条相对选区的放置模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinActionBarPosition {
    /// 选区下方。
    Below,
    /// 选区上方。
    Above,
    /// 选区内部，可拖动抓手。
    Inside,
}

impl PinActionBarPosition {
    /// 仅内部操作条提供抓手。
    pub fn has_grip(self) -> bool {
        self == Self::Inside
    }
}

/// 依据按钮数量与抓手计算条宽，包含相邻间距和两侧内边距。
pub(crate) const fn bar_width(buttons: usize, position: PinActionBarPosition) -> f32 {
    let grip = matches!(position, PinActionBarPosition::Inside);
    let items = buttons + grip as usize;
    buttons as f32 * BUTTON_WIDTH
        + if grip { GRIP_WIDTH } else { 0.0 }
        + items.saturating_sub(1) as f32 * BUTTON_GAP
        + 2.0 * BAR_PADDING
}

/// 更新相对选区左上角的逻辑偏移；超大条在相应轴退化为零并由容器裁剪。
pub(crate) fn clamp_bar_offset(
    selection: (f32, f32),
    bar: (f32, f32),
    offset: (f32, f32),
    delta: (f32, f32),
) -> (f32, f32) {
    /// 单轴保留可用边距，空间不足时先缩减边距，条更大时归零。
    fn axis(selection: f32, bar: f32, offset: f32) -> f32 {
        let space = (selection - bar).max(0.0);
        let margin = BAR_PADDING.min(space / 2.0);
        offset.clamp(margin, space - margin)
    }
    (
        axis(selection.0, bar.0, offset.0 + delta.0),
        axis(selection.1, bar.1, offset.1 + delta.1),
    )
}

/// 抓手拖动起点，逻辑坐标不受整窗屏幕位置影响。
#[derive(Clone, Copy)]
struct GripDrag {
    /// 按下时鼠标窗口坐标。
    cursor: (f32, f32),
    /// 按下时条的相对偏移。
    offset: (f32, f32),
}

/// 由宿主视图持有的操作条状态，重绘时共享而非重建。
#[derive(Default)]
pub(crate) struct PinActionBarState {
    /// 相对选区的逻辑偏移；首次渲染前为空。
    pub offset: Cell<Option<(f32, f32)>>,
    /// 当前抓手拖动起点。
    drag: Cell<Option<GripDrag>>,
    /// 最近选区；几何变化时丢弃拖动偏移。
    selection: Cell<Option<snow_ui::shell::geometry::PhysicalRect>>,
}

impl PinActionBarState {
    /// 清除已有偏移和拖动会话。
    pub fn reset(&self) {
        self.offset.set(None);
        self.drag.set(None);
    }

    /// 选区改变时重置；相同几何跨帧保留偏移。
    pub fn sync_selection(&self, selection: snow_ui::shell::geometry::PhysicalRect) {
        if self.selection.replace(Some(selection)) != Some(selection) {
            self.reset();
        }
    }

    /// 宿主原始监听在抓手拖动时让出选区事件。
    pub fn is_dragging(&self) -> bool {
        self.drag.get().is_some()
    }

    /// 首次默认靠右贴底；以后只夹紧已有偏移，不受窗口屏幕原点影响。
    pub fn resolve(&self, selection: (f32, f32), bar: (f32, f32)) -> (f32, f32) {
        let offset = clamp_bar_offset(
            selection,
            bar,
            self.offset.get().unwrap_or((
                selection.0 - bar.0 - BAR_PADDING,
                selection.1 - bar.1 - BAR_PADDING,
            )),
            (0.0, 0.0),
        );
        self.offset.set(Some(offset));
        offset
    }

    /// 仅内部模式开始抓手拖动。
    pub(crate) fn begin(
        &self,
        position: PinActionBarPosition,
        cursor: (f32, f32),
        offset: (f32, f32),
    ) {
        if position.has_grip() {
            self.drag.set(Some(GripDrag { cursor, offset }));
        }
    }

    /// 按下点到当前鼠标的总增量更新偏移，避免边界处累积误差。
    pub(crate) fn move_to(
        &self,
        selection: (f32, f32),
        bar: (f32, f32),
        cursor: (f32, f32),
    ) -> bool {
        let Some(drag) = self.drag.get() else {
            return false;
        };
        let offset = clamp_bar_offset(
            selection,
            bar,
            drag.offset,
            (cursor.0 - drag.cursor.0, cursor.1 - drag.cursor.1),
        );
        self.offset.replace(Some(offset)) != Some(offset)
    }
}

/// 操作按钮点击回调，经 UI 门面提供的窗口与应用上下文执行。
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// 可离屏检查的业务动作定义，绑定实体前不依赖窗口或应用上下文。
pub(crate) struct ActionSpec<A> {
    /// 稳定标识。
    pub id: &'static str,
    /// 图标路径。
    pub icon: &'static str,
    /// 已本地化的提示。
    pub tooltip: String,
    /// 禁用条件。
    pub disabled: bool,
    /// 忙碌反馈。
    pub loading: bool,
    /// 宿主业务动作。
    pub action: A,
}

impl<A: Copy + 'static> ActionSpec<A> {
    /// 离屏与 UI 共用同一分发入口，禁用时不调用业务回调。
    pub fn dispatch(&self, callback: impl FnOnce(A)) -> bool {
        dispatch_enabled(self.disabled, || callback(self.action))
    }

    /// 将业务动作绑定宿主实体；组件不依赖任何具体视图类型。
    pub fn bind<T: 'static>(
        self,
        cx: &Context<T>,
        callback: impl Fn(&mut T, A, &mut Window, &mut Context<T>) + 'static,
    ) -> PinActionButton {
        let entity = cx.entity().downgrade();
        let loading = self.loading;
        PinActionButton::new(
            self.id,
            self.icon,
            self.tooltip.clone(),
            self.disabled,
            move |_, window, app| {
                self.dispatch(|action| {
                    let _ = entity.update(app, |this, cx| callback(this, action, window, cx));
                });
            },
        )
        .loading(loading)
    }
}

/// 业务按钮与抓手共用同一实际高度，覆盖组件库 rem 尺寸以匹配所有宿主几何。
fn sized_icon_button(
    id: impl Into<ElementId>,
    icon: Option<Icon>,
    tooltip: Option<String>,
    width: f32,
) -> Button {
    Button::new(id)
        .when_some(icon, |button, icon| button.icon(icon))
        .when_some(tooltip, |button, tip| button.tooltip(tip))
        .with_size(ButtonSize::Small)
        .w(px(width))
        .h(px(BUTTON_HEIGHT))
}

/// 调用方只提供图标、提示、禁用状态和业务回调。
pub(crate) struct PinActionButton {
    /// 稳定组件标识。
    pub id: ElementId,
    /// 主栏当前工具高亮。
    pub selected: bool,
    /// 按钮逻辑尺寸；截图工具栏沿用既有几何。
    pub size: (f32, f32),
    /// 图标库资源路径。
    pub icon: &'static str,
    /// 已本地化提示。
    pub tooltip: String,
    /// 是否禁用。
    pub disabled: bool,
    /// 是否显示加载动画。
    pub loading: bool,
    /// 分组展开方向图标。
    indicator: Option<&'static str>,
    /// 无 Root 的覆盖窗使用原生 tooltip。
    native_tooltip: bool,
    /// 菜单展开时暂停主按钮提示，避免覆盖菜单。
    tooltip_enabled: bool,
    /// 右键分组菜单回调。
    on_right_down: Option<snow_ui::widgets::ToolbarItemRunner>,
    /// 业务点击回调。
    pub on_click: ClickHandler,
}

impl PinActionButton {
    /// 构造宿主无关的图标动作；提示由调用方本地化。
    pub fn new(
        id: &'static str,
        icon: &'static str,
        tooltip: String,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            selected: false,
            size: (BUTTON_WIDTH, BUTTON_HEIGHT),
            icon,
            tooltip,
            disabled,
            loading: false,
            indicator: None,
            native_tooltip: false,
            tooltip_enabled: true,
            on_right_down: None,
            on_click: Box::new(on_click),
        }
    }

    /// 保留翻译提交按钮的忙碌反馈。
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// 渲染单个动作，混合工具栏中的独立按钮也沿用事件隔离。
    pub fn render(self) -> impl IntoElement {
        let icon = Icon::default().path(self.icon);
        self.render_with_icon(icon)
    }

    /// 将截图主栏转换为共用按钮输入，保留全部状态及业务回调。
    pub(crate) fn from_toolbar(mut spec: snow_ui::widgets::ToolbarButtonSpec) -> Self {
        Self {
            id: SharedString::from(std::mem::take(&mut spec.id)).into(),
            icon: spec.item.icon_path(),
            tooltip: std::mem::take(&mut spec.tooltip),
            selected: spec.selected,
            disabled: spec.disabled,
            size: spec.size,
            loading: false,
            indicator: spec.indicator,
            native_tooltip: true,
            tooltip_enabled: spec.tooltip_enabled,
            on_right_down: spec.on_right_down.take(),
            on_click: Box::new(move |_, window, cx| {
                spec.dispatch(|_| {
                    if let Some(run) = &spec.on_click {
                        run(window, cx);
                    }
                });
            }),
        }
    }

    /// 允许宿主调整图标方向，其余按钮行为仍由组件负责。
    pub fn render_with_icon(self, icon: Icon) -> impl IntoElement {
        self.render_button(icon, None)
    }

    /// 所有宿主共用按钮构造、禁用分发和事件隔离；截图主栏可传入原有主题配色。
    fn render_button(self, icon: Icon, variant: Option<ButtonCustomVariant>) -> impl IntoElement {
        use snow_ui::ui::component::tooltip::Tooltip;
        let tip = SharedString::from(self.tooltip.clone());
        let mut button = sized_icon_button(
            self.id.clone(),
            (!self.native_tooltip).then(|| icon.clone()),
            (!self.native_tooltip).then_some(self.tooltip),
            self.size.0,
        )
        .h(px(self.size.1))
        .selected(self.selected)
        .disabled(self.disabled)
        .loading(self.loading)
        .accessibility_label(tip.clone())
        .when_some(variant, |button, variant| button.custom(variant));
        if self.native_tooltip {
            // 分组图标与箭头放在同一内容槽，沿用原来的 18 / 10px 与 2px 间距。
            let mut content = div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(2.0))
                .child(icon);
            if let Some(indicator) = self.indicator {
                content = content.child(
                    Icon::default()
                        .path(indicator)
                        .with_size(ButtonSize::Size(px(10.0))),
                );
            }
            button = button.with_size(ButtonSize::Size(px(10.0))).child(content);
        }
        let click = self.on_click;
        let right = self.on_right_down;
        let disabled = self.disabled;
        isolated_button(
            self.id,
            button.on_click(move |event, window, cx| {
                dispatch_enabled(disabled, || click(event, window, cx));
            }),
            |_, _, _| {},
            right,
        )
        .when(self.native_tooltip && self.tooltip_enabled, |element| {
            element
                .tooltip_show_delay(std::time::Duration::from_millis(350))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        })
    }
}

/// 截图工具栏渲染适配器：仅保留宿主分组模型，所有按钮与分隔布局统一在此实现。
#[derive(Default)]
pub(crate) struct ScreenshotActionRenderer {
    /// 共用抓手输入；外置时为空。
    pub grip: Option<ActionBarGrip>,
}

impl snow_ui::widgets::ToolbarRenderer for ScreenshotActionRenderer {
    /// 主栏使用组件库图标 Button，保留原有尺寸、配色和右键菜单。
    fn button(&self, spec: snow_ui::widgets::ToolbarButtonSpec, cx: &App) -> AnyElement {
        let variant = spec.look.variant(cx);
        let icon = Icon::default()
            .path(spec.item.icon_path())
            .with_size(ButtonSize::Size(px(18.0)));
        PinActionButton::from_toolbar(spec)
            .render_button(icon, Some(variant))
            .into_any_element()
    }

    /// 固定分隔线尺寸与原主栏尺寸模型一致。
    fn separator(&self) -> AnyElement {
        div()
            .flex_shrink_0()
            .w(px(1.0))
            .h(px(16.0))
            .bg(rgba(0xFFFFFF33))
            .into_any_element()
    }

    /// 抓手始终位于首个业务按钮之前。
    fn leading(&self) -> Option<AnyElement> {
        self.grip.as_ref().map(|grip| {
            div()
                .flex_none()
                .pr(px(BUTTON_GAP))
                .child(grip.render())
                .into_any_element()
        })
    }

    /// 内置抓手占用共用宽度和间距。
    fn leading_width(&self) -> f32 {
        if self.grip.is_some() {
            GRIP_WIDTH + BUTTON_GAP
        } else {
            0.0
        }
    }

    /// 主栏横向布局，宿主提供既有间距。
    fn row(&self, gap: f32) -> Div {
        div().flex().items_center().gap(px(gap))
    }
}

/// 禁用态同时在回调入口阻断，避免宿主误触发业务动作。
fn dispatch_enabled(disabled: bool, callback: impl FnOnce()) -> bool {
    if disabled {
        return false;
    }
    callback();
    true
}

/// 非贴图宿主的普通流布局：不提供抓手，空白事件由宿主决定是否拖窗。
pub(crate) fn inline_actions(buttons: Vec<PinActionButton>) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .flex_wrap()
        .py(px(BAR_VERTICAL_PADDING))
        .gap(px(BUTTON_GAP))
        .children(buttons.into_iter().map(PinActionButton::render))
}

/// 共用外部优先的位置决策，物理和逻辑坐标均可使用，但调用方须保持单位一致。
pub(crate) fn outer_position(
    selection: snow_ui::shell::geometry::PhysicalRect,
    screen: snow_ui::shell::geometry::PhysicalRect,
    height: i32,
) -> PinActionBarPosition {
    if selection.bottom() + height <= screen.bottom() {
        PinActionBarPosition::Below
    } else if selection.y - height >= screen.y {
        PinActionBarPosition::Above
    } else {
        PinActionBarPosition::Inside
    }
}

/// 外置条按内容尺寸靠选区右缘，并夹紧到宿主可用宽度；所有坐标单位一致。
pub(crate) fn outer_bar_rect(
    selection: snow_ui::shell::geometry::PhysicalRect,
    screen: snow_ui::shell::geometry::PhysicalRect,
    width: i32,
    height: i32,
    position: PinActionBarPosition,
) -> snow_ui::shell::geometry::PhysicalRect {
    snow_ui::shell::geometry::PhysicalRect::new(
        (selection.right() - width).clamp(screen.x, (screen.right() - width).max(screen.x)),
        if position == PinActionBarPosition::Above {
            selection.y - height
        } else {
            selection.bottom()
        },
        width,
        height,
    )
}

/// 覆盖窗的浮动操作条：外部优先，内部才提供抓手；空白沿用宿主的鼠标路径。
pub(crate) fn floating_actions(
    buttons: Vec<PinActionButton>,
    selection: snow_ui::shell::geometry::PhysicalRect,
    screen: snow_ui::shell::geometry::PhysicalRect,
    state: Rc<PinActionBarState>,
    grip_tooltip: String,
    notify: impl Fn(&mut App) + 'static,
) -> (AnyElement, snow_ui::shell::geometry::PhysicalRect) {
    let layout = action_bar_layout(
        selection,
        screen,
        (
            bar_width(buttons.len(), PinActionBarPosition::Below),
            bar_height(),
        ),
        &state,
    );
    let position = layout.position;
    let rect = layout.rect;
    let width = rect.width as f32;
    let container = if position.has_grip() { selection } else { rect };
    let bar = PinActionBar {
        blank_behavior: BarBlankBehavior::Block,
        buttons,
        position,
        size: (width, bar_height()),
        image_origin: (0.0, 0.0),
        selection: (selection.width as f32, selection.height as f32),
        state,
        grip_tooltip,
    };
    (
        div()
            .absolute()
            .left(px(container.x as f32))
            .top(px(container.y as f32))
            .w(px(container.width as f32))
            .h(px(container.height as f32))
            .overflow_hidden()
            .child(bar.render(notify))
            .into_any_element(),
        rect,
    )
}

/// 共用定位容器：内部条裁剪在选区内，空白区域保持宿主事件路径。
pub(crate) fn positioned_actions(
    content: AnyElement,
    layout: ActionBarLayout,
    selection: snow_ui::shell::geometry::PhysicalRect,
) -> AnyElement {
    let inside = layout.position.has_grip();
    let bounds = if inside { selection } else { layout.rect };
    div()
        .absolute()
        .left(px(bounds.x as f32))
        .top(px(bounds.y as f32))
        .w(px(bounds.width as f32))
        .h(px(bounds.height as f32))
        .when(inside, |container| container.overflow_hidden())
        .child(
            div()
                .absolute()
                .left(px((layout.rect.x - bounds.x) as f32))
                .top(px((layout.rect.y - bounds.y) as f32))
                .child(content),
        )
        .into_any_element()
}

/// 操作条空白处的宿主策略：贴图沿用拖窗，覆盖窗阻止重新框选。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarBlankBehavior {
    /// 由父节点处理空白处鼠标事件。
    Bubble,
    /// 宿主没有拖窗路径，拦截空白处三种鼠标按下。
    Block,
}

/// 操作条渲染输入；尺寸及坐标均为逻辑像素。
pub(crate) struct PinActionBar {
    /// 空白区域如何交给宿主。
    pub blank_behavior: BarBlankBehavior,
    /// 业务按钮列表。
    pub buttons: Vec<PinActionButton>,
    /// 放置模式。
    pub position: PinActionBarPosition,
    /// 条尺寸，窄选区可裁剪。
    pub size: (f32, f32),
    /// 图像在窗口内的原点。
    pub image_origin: (f32, f32),
    /// 图像选区尺寸。
    pub selection: (f32, f32),
    /// 宿主视图拥有的相对位置状态。
    pub state: Rc<PinActionBarState>,
    /// 已本地化的抓手提示。
    pub grip_tooltip: String,
}

impl PinActionBar {
    /// 统一渲染与事件处理；空白按宿主策略冒泡拖窗或隔离框选。
    pub fn render(self, notify: impl Fn(&mut App) + 'static) -> impl IntoElement {
        let notify = Rc::new(notify);
        let offset = self.state.resolve(self.selection, self.size);
        let mut bar = div()
            .absolute()
            .w(px(self.size.0))
            .h(px(self.size.1))
            .flex()
            .items_center()
            .gap(px(BUTTON_GAP))
            .px(px(BAR_PADDING))
            .py(px(BAR_VERTICAL_PADDING))
            .overflow_hidden()
            .bg(rgba(0x202020FF))
            .when(self.blank_behavior == BarBlankBehavior::Block, |bar| {
                bar.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            });
        bar = match self.position {
            PinActionBarPosition::Above => bar.right(px(0.0)).top(px(0.0)),
            PinActionBarPosition::Below => bar.right(px(0.0)).bottom(px(0.0)),
            PinActionBarPosition::Inside => bar
                .left(px(self.image_origin.0 + offset.0))
                .top(px(self.image_origin.1 + offset.1)),
        };
        if self.position.has_grip() {
            bar = bar.child(
                ActionBarGrip {
                    selection: self.selection,
                    size: self.size,
                    state: self.state,
                    tooltip: self.grip_tooltip,
                    notify,
                }
                .render(),
            );
        }
        bar.children(self.buttons.into_iter().map(PinActionButton::render))
    }
}

/// 所有宿主共用抓手构造与窗口级拖动监听，不绑定具体视图。
pub(crate) struct ActionBarGrip {
    /// 选区逻辑尺寸。
    pub selection: (f32, f32),
    /// 包含抓手的条尺寸。
    pub size: (f32, f32),
    /// 宿主持有的稳定状态。
    pub state: Rc<PinActionBarState>,
    /// 已本地化提示。
    pub tooltip: String,
    /// 仅偏移改变时通知宿主。
    pub notify: Rc<dyn Fn(&mut App)>,
}

/// 所有宿主和离屏检查共用抓手按钮定义，外置不构造按钮。
fn grip_button(position: PinActionBarPosition, tooltip: &str) -> Option<Button> {
    position.has_grip().then(|| {
        sized_icon_button(
            "pin-action-grip-button",
            Some(Icon::default().path(GRIP_ICON)),
            Some(tooltip.to_owned()),
            GRIP_WIDTH,
        )
    })
}

impl ActionBarGrip {
    /// 与普通动作共用组件库按钮及按下隔离，窗口外拖动仍能夹紧。
    fn render(&self) -> AnyElement {
        let grip = grip_button(PinActionBarPosition::Inside, &self.tooltip)
            .expect("inside action bar has a grip");
        let down = self.state.clone();
        let notify = self.notify.clone();
        let selection = self.selection;
        let size = self.size;
        let offset = self.state.resolve(selection, size);
        let moves = self.state.clone();
        let ups = self.state.clone();
        div()
            .flex()
            .items_center()
            .child(
                isolated_button(
                    "pin-action-grip",
                    grip,
                    move |event, _, _| {
                        down.begin(
                            PinActionBarPosition::Inside,
                            (event.position.x.as_f32(), event.position.y.as_f32()),
                            offset,
                        );
                    },
                    None,
                )
                .cursor(CursorStyle::OpenHand),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        let notify_move = notify.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if event.pressed_button != Some(MouseButton::Left) {
                                moves.drag.set(None);
                            } else if moves.move_to(
                                selection,
                                size,
                                (event.position.x.as_f32(), event.position.y.as_f32()),
                            ) {
                                notify_move(cx);
                            }
                        });
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                if ups.move_to(
                                    selection,
                                    size,
                                    (event.position.x.as_f32(), event.position.y.as_f32()),
                                ) {
                                    notify(cx);
                                }
                                ups.drag.set(None);
                            }
                        });
                    },
                )
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size(px(1.0)),
            )
            .into_any_element()
    }
}

/// 共用布局结果，坐标与输入使用同一单位。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActionBarLayout {
    /// 实际位置模式。
    pub position: PinActionBarPosition,
    /// 包含内部抓手的实际条矩形。
    pub rect: snow_ui::shell::geometry::PhysicalRect,
}

/// 截图和浮动条共用布局；宿主只提供几何与持久状态。
pub(crate) fn action_bar_layout(
    selection: snow_ui::shell::geometry::PhysicalRect,
    screen: snow_ui::shell::geometry::PhysicalRect,
    size: (f32, f32),
    state: &PinActionBarState,
) -> ActionBarLayout {
    let position = outer_position(selection, screen, size.1.ceil() as i32);
    let width = size.0
        + if position.has_grip() {
            GRIP_WIDTH + BUTTON_GAP
        } else {
            0.0
        };
    let rect = if position.has_grip() {
        let offset = state.resolve(
            (selection.width as f32, selection.height as f32),
            (width, size.1),
        );
        snow_ui::shell::geometry::PhysicalRect::new(
            selection.x + offset.0.round() as i32,
            selection.y + offset.1.round() as i32,
            width.ceil() as i32,
            size.1.ceil() as i32,
        )
    } else {
        outer_bar_rect(
            selection,
            screen,
            width.ceil() as i32,
            size.1.ceil() as i32,
            position,
        )
    };
    ActionBarLayout { position, rect }
}

impl ActionBarLayout {
    /// 外置菜单随条同向展开，内部时选择剩余空间较大的一侧。
    pub fn popup_up(self, screen: snow_ui::shell::geometry::PhysicalRect) -> bool {
        match self.position {
            PinActionBarPosition::Below => false,
            PinActionBarPosition::Above => true,
            PinActionBarPosition::Inside => {
                self.rect.y - screen.y > screen.bottom() - self.rect.bottom()
            }
        }
    }
}

/// 先隔离再执行宿主按钮回调，离屏测试与真实按下事件共用。
fn isolate_down<T>(context: &mut T, stop: impl FnOnce(&mut T), action: impl FnOnce(&mut T)) {
    stop(context);
    action(context);
}

/// 应用上下文上的鼠标按下隔离入口。
fn isolate_button_down(cx: &mut App, action: impl FnOnce(&mut App)) {
    isolate_down(cx, App::stop_propagation, action);
}

/// 按钮及禁用按钮均隔离按下事件，点击仍交给组件库 Button。
fn isolated_button(
    id: impl Into<ElementId>,
    button: Button,
    on_left_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    on_right_down: Option<snow_ui::widgets::ToolbarItemRunner>,
) -> impl IntoElement + StatefulInteractiveElement + Styled {
    div()
        .id(id)
        .flex_shrink_0()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            isolate_button_down(cx, |cx| on_left_down(event, window, cx));
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            isolate_button_down(cx, |cx| {
                if let Some(run) = &on_right_down {
                    run(window, cx);
                }
            })
        })
        .on_mouse_down(MouseButton::Middle, |_, _, cx| {
            isolate_button_down(cx, |_| {})
        })
        .child(button)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主栏生产渲染只含图标与可选分组箭头，文字仅用于提示和无障碍标签。
    #[test]
    fn action_bar_toolbar_buttons_are_icon_only() {
        let source = include_str!("pin_action_bar.rs");
        let render = source
            .split("    fn render_button(")
            .nth(1)
            .unwrap()
            .split("/// 截图工具栏渲染适配器")
            .next()
            .unwrap();
        assert!(render.contains(".child(icon)"));
        assert!(render.contains(".path(indicator)"));
        assert!(render.contains(".accessibility_label(tip.clone())"));
        assert!(render.contains("Tooltip::new(tip.clone())"));
        assert!(!render.contains(".label("));
        assert!(!render.contains(".child(tip"));
        assert!(!render.contains(".child(self.tooltip"));
        let sized = source
            .split("fn sized_icon_button(")
            .nth(1)
            .unwrap()
            .split("/// 调用方")
            .next()
            .unwrap();
        assert!(sized.contains("button.icon(icon)"));
        assert!(!sized.contains(".label("));
    }

    /// 生产按钮输入转换后保持选中、禁用、提示、尺寸与分组箭头，不创建窗口。
    #[test]
    fn action_bar_toolbar_specs_preserve_presentation() {
        use snow_ui::widgets::{AnnotationTool, ScreenshotToolbar, ToolbarButtonLook, ToolbarItem};
        let toolbar = ScreenshotToolbar::new("test")
            .active_tool(AnnotationTool::Arrow)
            .labels(|_| "localized tip".into());
        for (tool, selected) in [
            (AnnotationTool::Arrow, true),
            (AnnotationTool::Rectangle, false),
        ] {
            let mut spec = toolbar.button_spec(
                "tool".into(),
                ToolbarItem::Tool(tool),
                ToolbarButtonLook::Normal,
                (40.0, 28.0),
            );
            spec.indicator = Some("icons/chevron-down.svg");
            let button = PinActionButton::from_toolbar(spec);
            assert_eq!(button.selected, selected);
            assert!(!button.disabled);
            assert_eq!(button.tooltip, "localized tip");
            assert_eq!(button.size, (40.0, 28.0));
            assert_eq!(button.indicator, Some("icons/chevron-down.svg"));
            assert!(button.native_tooltip);
            let _ = button.render().into_any_element();
        }
    }

    /// 真实按下处理先阻止父级，再执行按钮回调；禁用只阻断动作，不放行选区拖动。
    #[test]
    fn action_bar_toolbar_isolates_down_before_dispatch() {
        for mouse in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            for disabled in [false, true] {
                let mut trace = Vec::new();
                isolate_down(
                    &mut trace,
                    |trace| trace.push("stop"),
                    |trace| {
                        assert_eq!(trace.as_slice(), ["stop"]);
                        dispatch_enabled(disabled, || trace.push("button"));
                    },
                );
                assert_eq!(trace.len(), if disabled { 1 } else { 2 }, "{mouse:?}");
                assert_eq!(trace[0], "stop");
            }
        }
    }

    /// 实际按钮与上下内边距决定所有宿主条高，空间恰好时仍优先外置。
    #[test]
    fn action_bar_height_tracks_button_and_padding() {
        use snow_ui::shell::geometry::PhysicalRect;
        assert_eq!(bar_height(), BUTTON_HEIGHT + 2.0 * BAR_VERTICAL_PADDING);
        assert_eq!(bar_height(), 32.0);
        let height = bar_height() as i32;
        let screen = PhysicalRect::new(0, 0, 1000, 558 + height);
        assert_eq!(
            outer_position(PhysicalRect::new(0, 0, 572, 558), screen, height),
            PinActionBarPosition::Below
        );
        assert_eq!(
            outer_position(PhysicalRect::new(0, 1, 572, 558), screen, height),
            PinActionBarPosition::Inside
        );
        assert_eq!(
            outer_position(PhysicalRect::new(0, height, 572, 558), screen, height),
            PinActionBarPosition::Above
        );
    }

    /// 覆盖窗与贴图使用同一外部优先规则，内部宽度包含抓手并按选区裁剪。
    #[test]
    fn action_bar_floating_placement_and_relative_drag() {
        use snow_ui::shell::geometry::PhysicalRect;
        let screen = PhysicalRect::new(-1000, 100, 1000, 600);
        for (selection, expected_position, expected_y) in [
            (
                PhysicalRect::new(-700, 200, 300, 200),
                PinActionBarPosition::Below,
                400,
            ),
            (
                PhysicalRect::new(-700, 500, 300, 200),
                PinActionBarPosition::Above,
                500 - bar_height() as i32,
            ),
            (
                PhysicalRect::new(-700, 100, 300, 600),
                PinActionBarPosition::Inside,
                700 - bar_height() as i32 - BAR_PADDING as i32,
            ),
        ] {
            assert_eq!(
                outer_position(selection, screen, bar_height() as i32),
                expected_position
            );
            let state = Rc::new(PinActionBarState::default());
            let buttons = (0..5)
                .map(|_| {
                    PinActionButton::new(
                        "test",
                        "icons/antd/copy.svg",
                        String::new(),
                        false,
                        |_, _, _| {},
                    )
                })
                .collect();
            let (_, rect) = floating_actions(
                buttons,
                selection,
                screen,
                state.clone(),
                String::new(),
                |_| {},
            );
            assert_eq!(rect.y, expected_y);
            assert_eq!(rect.width, bar_width(5, expected_position) as i32);
            assert!(rect.x >= selection.x && rect.right() <= selection.right());
            if expected_position.has_grip() {
                let offset = state.offset.get().unwrap();
                state.begin(expected_position, (rect.x as f32, rect.y as f32), offset);
                assert!(state.move_to(
                    (300.0, 600.0),
                    (rect.width as f32, bar_height()),
                    (rect.x as f32 - 20.0, rect.y as f32 - 30.0)
                ));
                assert_eq!(state.offset.get(), Some((offset.0 - 20.0, offset.1 - 30.0)));
            }
        }
        let tiny = PhysicalRect::new(-1000, 100, 10, 600);
        let (_, rect) = floating_actions(
            vec![PinActionButton::new(
                "copy",
                "icons/antd/copy.svg",
                String::new(),
                false,
                |_, _, _| {},
            )],
            tiny,
            screen,
            Rc::default(),
            String::new(),
            |_| {},
        );
        assert_eq!(rect.x, tiny.x);
        assert_eq!(
            rect.width,
            bar_width(1, PinActionBarPosition::Inside) as i32
        );
    }

    /// 四边都保留边距，拖动输入按总增量夹紧。
    #[test]
    fn pinned_bar_clamps_to_all_four_edges() {
        for (delta, expected) in [
            ((-1000.0, 0.0), (4.0, 30.0)),
            ((1000.0, 0.0), (176.0, 30.0)),
            ((0.0, -1000.0), (50.0, 4.0)),
            ((0.0, 1000.0), (50.0, 200.0 - bar_height() - BAR_PADDING)),
        ] {
            assert_eq!(
                clamp_bar_offset((300.0, 200.0), (120.0, bar_height()), (50.0, 30.0), delta),
                expected
            );
        }
    }

    /// 超大条归零；只差少量空间时缩小边距，不产生负坐标或夹紧异常。
    #[test]
    fn pinned_bar_oversized_selection_degrades_per_axis() {
        assert_eq!(
            clamp_bar_offset(
                (20.0, 10.0),
                (144.0, bar_height()),
                (60.0, 60.0),
                (100.0, -100.0)
            ),
            (0.0, 0.0)
        );
        assert_eq!(
            clamp_bar_offset(
                (146.0, 100.0),
                (144.0, bar_height()),
                (60.0, 60.0),
                (100.0, -100.0)
            ),
            (1.0, 4.0)
        );
        assert_eq!(
            clamp_bar_offset((0.0, 0.0), (0.0, 0.0), (0.0, 0.0), (1.0, 1.0)),
            (0.0, 0.0)
        );
    }

    /// 内部抓手默认靠右贴底，拖动后重绘保留偏移；越界回来仍按起点计算。
    #[test]
    fn pinned_bar_grip_drag_updates_relative_offset() {
        let state = PinActionBarState::default();
        let selection = (400.0, 200.0);
        let bar = (144.0, bar_height());
        let initial = state.resolve(selection, bar);
        assert_eq!(initial, (252.0, 200.0 - bar_height() - BAR_PADDING));
        state.begin(PinActionBarPosition::Inside, (270.0, 170.0), initial);
        assert!(state.move_to(selection, bar, (230.0, 140.0)));
        assert_eq!(
            state.resolve(selection, bar),
            (212.0, 170.0 - bar_height() - BAR_PADDING)
        );
        assert!(state.move_to(selection, bar, (-1000.0, -1000.0)));
        assert_eq!(state.offset.get(), Some((4.0, 4.0)));
        assert!(state.move_to(selection, bar, (10000.0, 10000.0)));
        assert_eq!(
            state.offset.get(),
            Some((252.0, 200.0 - bar_height() - BAR_PADDING))
        );
        assert!(state.move_to(selection, bar, (230.0, 140.0)));
        assert_eq!(
            state.offset.get(),
            Some((212.0, 170.0 - bar_height() - BAR_PADDING))
        );
        state.drag.set(None);
        assert!(!state.move_to(selection, bar, (10.0, 10.0)));
    }

    /// 直接检查生产渲染消费的抓手 Button，而非仅检查模式布尔值。
    #[test]
    fn action_bar_render_input_contains_grip_only_inside() {
        for position in [
            PinActionBarPosition::Below,
            PinActionBarPosition::Above,
            PinActionBarPosition::Inside,
        ] {
            let bar = PinActionBar {
                blank_behavior: BarBlankBehavior::Bubble,
                buttons: Vec::new(),
                position,
                size: (bar_width(4, position), bar_height()),
                image_origin: (0.0, 0.0),
                selection: (800.0, 500.0),
                state: Rc::default(),
                grip_tooltip: crate::ocr_backend::i18n_for("en-US").tr("translation-pin-move-bar"),
            };
            assert_eq!(
                grip_button(bar.position, &bar.grip_tooltip).is_some(),
                position.has_grip()
            );
            // 生产分支渲染抓手在业务按钮之前；整个元素可在无窗口条件下构造。
            let _ = bar.render(|_| {}).into_any_element();
        }
    }

    /// 外置条右对齐，跨屏边缘时向左夹紧，宽度始终保持内容宽度。
    #[test]
    fn action_bar_outer_rect_clamps_without_stretching() {
        use snow_ui::shell::geometry::PhysicalRect;
        let screen = PhysicalRect::new(-800, 0, 800, 600);
        for (selection, x) in [
            (PhysicalRect::new(-700, 50, 600, 300), -216),
            (PhysicalRect::new(-20, 50, 100, 300), -116),
            (PhysicalRect::new(-800, 50, 20, 300), -800),
        ] {
            for position in [PinActionBarPosition::Below, PinActionBarPosition::Above] {
                let rect = outer_bar_rect(selection, screen, 116, bar_height() as i32, position);
                assert_eq!(rect.x, x);
                assert_eq!((rect.width, rect.height), (116, bar_height() as i32));
            }
        }
    }

    /// 三种模式的宽度来自同一公式，外部不渲染也不接受抓手拖动。
    #[test]
    fn pinned_bar_grip_only_exposed_inside() {
        for position in [
            PinActionBarPosition::Below,
            PinActionBarPosition::Above,
            PinActionBarPosition::Inside,
        ] {
            let inside = position == PinActionBarPosition::Inside;
            assert_eq!(position.has_grip(), inside);
            assert_eq!(bar_width(4, position), if inside { 144.0 } else { 116.0 });
            let state = PinActionBarState::default();
            state.begin(position, (0.0, 0.0), (4.0, 4.0));
            assert_eq!(state.drag.get().is_some(), inside);
        }
        assert_eq!(bar_width(0, PinActionBarPosition::Below), 8.0);
        assert_eq!(bar_width(2, PinActionBarPosition::Inside), 88.0);
        assert!(icon_asset_exists(GRIP_ICON));
    }
}
