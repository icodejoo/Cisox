//! 图标 + 文字的下拉菜单按钮：整体是一个按钮（图标、文字、小箭头同一底板），
//! 点击开 / 关菜单，菜单默认向上展开；开关状态由使用方持有（无状态组件）。

use crate::text_measure::{ARROW_ICON_PX, FRAME_PADDING, ROW_ICON};
use crate::toolbar::{COLOR_CURRENT_TEXT, Look, menu_check_slot};
use crate::toolbar_groups::{BAR_BORDER, ITEM_GAP, MENU_PADDING, MENU_ROW_HEIGHT, POPUP_OFFSET};
use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::component::button::{Button, ButtonVariants};
use snow_ui_shell::ui::component::tooltip::Tooltip;
use snow_ui_shell::ui::component::{Icon, Sizable, Size as ComponentSize};
use snow_ui_shell::ui::*;
use std::rc::Rc;

/// 按钮本体高度。
pub const MENU_BUTTON_HEIGHT: f32 = 28.0;
/// 分隔线占的高度（含上下留白）。
const SEPARATOR_HEIGHT: f32 = 7.0;
/// 悬停提示延迟。
const TOOLTIP_DELAY_MS: u64 = 350;
/// 背景色（与主工具栏一致）。
const COLOR_BG: u32 = 0x1F1F1FE6;
/// 边框色。
const COLOR_BORDER: u32 = 0x00000080;
/// 分隔线颜色。
const COLOR_SEPARATOR: u32 = 0xFFFFFF33;
/// 向上的小箭头图标（Lucide）。
const ARROW_UP_ICON: &str = "icons/chevron-up.svg";

/// 整个按钮（含外框）的尺寸 `(宽, 高)`，用于定位与居中。
///
/// # 参数
/// - `outer_width`：外宽，见 [`crate::menu_button_outer_width`]。
///
/// # 示例
/// ```rust
/// let (w, h) = snow_ui_widgets::menu_button_size(180.0);
/// assert_eq!(w, 180.0);
/// assert!(h > 0.0);
/// ```
pub fn menu_button_size(outer_width: f32) -> (f32, f32) {
    let pad = (FRAME_PADDING + BAR_BORDER) * 2.0;
    (outer_width, MENU_BUTTON_HEIGHT + pad)
}

/// 两个矩形是否有重叠面积。
fn overlaps(a: PhysicalRect, b: PhysicalRect) -> bool {
    a.intersect(&b).is_some()
}

/// 选区菜单按钮的摆放。
///
/// 1. 选区够大：叠在选区内右下角，四周留 `margin`（要大于缩放手柄的命中范围，免得盖住手柄）；
///    若此处被主工具栏占住（工具栏嵌进了选区内底端），改放到工具栏正上方、与上面同一右边缘。
/// 2. 选区放不下（宽或高小于按钮 + 两侧边距）：放到选区右侧外缘、底部对齐；
///    那里放不下或会压到工具栏，就放到工具栏正上方右对齐，上方也不行再放正下方。
/// 3. 最后夹进屏幕范围。
///
/// # 参数
/// - `selection`: 选区矩形（已裁到本屏）。
/// - `toolbar`: 主工具栏矩形。
/// - `size`: 按钮外尺寸 `(宽, 高)`。
/// - `screen`: 屏幕（显示器）范围。
/// - `margin`: 贴选区内缘时的边距。
/// - `gap`: 贴选区外缘 / 工具栏时的间距。
///
/// # 返回
/// 按钮左上角。
pub fn calculate_region_bar_placement(
    selection: PhysicalRect,
    toolbar: PhysicalRect,
    size: (i32, i32),
    screen: PhysicalRect,
    margin: i32,
    gap: i32,
) -> PhysicalPoint {
    let (w, h) = size;
    let rect_at = |x: i32, y: i32| PhysicalRect::new(x, y, w, h);
    // 放到工具栏正上方右对齐；上方放不下改正下方
    let above_toolbar = |x: i32| {
        let above = toolbar.y - gap - h;
        let y = if above >= screen.y {
            above
        } else {
            toolbar.bottom() + gap
        };
        (x, y)
    };
    let fits_inside = selection.width >= w + 2 * margin && selection.height >= h + 2 * margin;
    let (x, y) = if fits_inside {
        let x = selection.right() - margin - w;
        let y = selection.bottom() - margin - h;
        if overlaps(rect_at(x, y), toolbar) {
            above_toolbar(x)
        } else {
            (x, y)
        }
    } else {
        let x = selection.right() + gap;
        let y = selection.bottom() - h;
        if x + w <= screen.right() && !overlaps(rect_at(x, y), toolbar) {
            (x, y)
        } else {
            above_toolbar(toolbar.right() - w)
        }
    };
    let x = x.min(screen.right() - w).max(screen.x);
    let y = y.min(screen.bottom() - h).max(screen.y);
    PhysicalPoint::new(x, y)
}

/// 菜单高度：行数、分隔线数决定。
///
/// # 参数
/// - `rows`：菜单项行数。
/// - `separators`：分隔线条数。
pub fn menu_button_menu_height(rows: usize, separators: usize) -> f32 {
    let rows_f = rows as f32;
    rows_f * MENU_ROW_HEIGHT
        + ITEM_GAP * (rows_f - 1.0).max(0.0)
        + separators as f32 * SEPARATOR_HEIGHT
        + MENU_PADDING * 2.0
        + 2.0
}

/// 菜单里的一项。
#[derive(Debug, Clone)]
pub struct MenuEntry {
    /// 稳定的元素 id 片段。
    pub id: String,
    /// 图标资源路径。
    pub icon: &'static str,
    /// 已本地化的文字。
    pub label: String,
    /// 是否当前项（显示绿色对勾）。
    pub checked: bool,
    /// 在这一项之前画一条分隔线。
    pub separator_before: bool,
}

/// 点击回调。
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
/// 选中某项的回调，参数是项序号。
type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// 图标 + 文字 + 小箭头的下拉菜单按钮。
pub struct IconMenuButton {
    id: String,
    icon: &'static str,
    label: String,
    tooltip: String,
    entries: Vec<MenuEntry>,
    open: bool,
    up: bool,
    outer_width: Option<f32>,
    on_toggle: Option<Handler>,
    on_select: Option<SelectHandler>,
}

impl IconMenuButton {
    /// 构造按钮。
    ///
    /// # 参数
    /// - `id`：元素 id。
    /// - `icon` / `label`：按钮上显示的当前项图标与文字。
    /// - `entries`：菜单项。
    pub fn new(
        id: impl Into<String>,
        icon: &'static str,
        label: impl Into<String>,
        entries: Vec<MenuEntry>,
    ) -> Self {
        let label = label.into();
        Self {
            id: id.into(),
            icon,
            tooltip: label.clone(),
            label,
            entries,
            open: false,
            up: true,
            outer_width: None,
            on_toggle: None,
            on_select: None,
        }
    }

    /// 悬停提示与无障碍标签。
    pub fn tooltip(mut self, text: impl Into<String>) -> Self {
        self.tooltip = text.into();
        self
    }

    /// 菜单是否展开。
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// 菜单是否向上展开（`false` 为向下）。
    pub fn up(mut self, up: bool) -> Self {
        self.up = up;
        self
    }

    /// 按钮外宽（含外框），菜单同宽；见 [`crate::menu_button_outer_width`]。不设置则按内容自动撑开。
    pub fn width(mut self, outer_width: f32) -> Self {
        self.outer_width = Some(outer_width);
        self
    }

    /// 点击按钮的回调（使用方负责切换 `open`）。
    pub fn on_toggle(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    /// 选中菜单项的回调（使用方负责收起菜单并执行动作）。
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// 弹出菜单：仅展开时构造。
    fn menu(&self, cx: &App) -> AnyElement {
        let mut menu = div()
            .id(SharedString::from(format!("{}-menu", self.id)))
            .flex()
            .flex_col()
            .gap(px(ITEM_GAP))
            .p(px(MENU_PADDING))
            .when_some(self.outer_width, |m, w| m.w(px(w)))
            .rounded_md()
            .bg(rgba(COLOR_BG))
            .border_1()
            .border_color(rgba(COLOR_BORDER))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.separator_before {
                menu = menu.child(
                    div()
                        .h(px(SEPARATOR_HEIGHT - ITEM_GAP))
                        .flex()
                        .items_center()
                        .child(div().w_full().h(px(1.0)).bg(rgba(COLOR_SEPARATOR))),
                );
            }
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
                        .path(entry.icon)
                        .with_size(ComponentSize::Size(px(ROW_ICON))),
                )
                .child(
                    div()
                        .flex_1()
                        .when(entry.checked, |l| l.text_color(rgb(COLOR_CURRENT_TEXT)))
                        .child(entry.label.clone()),
                )
                .child(menu_check_slot(entry.checked));
            let mut row = Button::new(SharedString::from(format!("{}-item-{}", self.id, entry.id)))
                .custom(Look::Normal.variant(cx))
                .with_size(ComponentSize::Size(px(ROW_ICON)))
                .w_full()
                .h(px(MENU_ROW_HEIGHT))
                .child(content);
            if let Some(handler) = self.on_select.clone() {
                row = row.on_click(move |_, window, cx| handler(index, window, cx));
            }
            menu = menu.child(row);
        }
        menu.into_any_element()
    }
}

impl RenderOnce for IconMenuButton {
    /// 渲染：外框 + 按钮 + 锚定的菜单。
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
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
                    .path(self.icon)
                    .with_size(ComponentSize::Size(px(ROW_ICON))),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(self.label.clone()),
            )
            .child(
                Icon::default()
                    .path(ARROW_UP_ICON)
                    .with_size(ComponentSize::Size(px(ARROW_ICON_PX))),
            );
        let frame_pad = (FRAME_PADDING + BAR_BORDER) * 2.0;
        let look = if self.open {
            Look::Active
        } else {
            Look::Normal
        };
        let mut button = Button::new(SharedString::from(format!("{}-button", self.id)))
            .custom(look.variant(cx))
            .with_size(ComponentSize::Size(px(ROW_ICON)))
            .when_some(self.outer_width, |b, w| b.w(px(w - frame_pad)))
            .h(px(MENU_BUTTON_HEIGHT))
            .accessibility_label(SharedString::from(self.tooltip.clone()))
            .child(content);
        if let Some(handler) = self.on_toggle.clone() {
            button = button.on_click(move |_, window, cx| handler(window, cx));
        }
        // 展开时不再显示提示；提示用原生 tooltip（覆盖窗没有 Root）
        let tip = SharedString::from(self.tooltip.clone());
        let trigger: AnyElement = if self.open {
            button.into_any_element()
        } else {
            div()
                .id(SharedString::from(format!("{}-tip", self.id)))
                .tooltip_show_delay(std::time::Duration::from_millis(TOOLTIP_DELAY_MS))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(button)
                .into_any_element()
        };
        let frame = div()
            .flex()
            .items_center()
            .p(px(FRAME_PADDING))
            .rounded_md()
            .bg(rgba(COLOR_BG))
            .border(px(BAR_BORDER))
            .border_color(rgba(COLOR_BORDER))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(trigger);
        let menu = self.open.then(|| self.menu(cx));
        let mut popup = Popup::new(SharedString::from(format!("{}-popup", self.id)), frame)
            .anchor(if self.up {
                Anchor::BottomLeft
            } else {
                Anchor::TopLeft
            })
            .offset(px(POPUP_OFFSET));
        if let Some(menu) = menu {
            popup = popup.content(menu);
        }
        popup
    }
}

impl IntoElement for IconMenuButton {
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

    /// 整体尺寸含外框与边框。
    #[test]
    fn size_includes_frame() {
        assert_eq!(menu_button_size(180.0), (180.0, 34.0));
    }

    /// 摆放：选区内右下角、小选区回退、嵌入选区避让工具栏、副屏与贴边；都不重叠、不出屏、不盖缩放手柄。
    #[test]
    fn placement_cases() {
        let screen = PhysicalRect::new(0, 0, 1000, 600);
        let size = (180, 34);
        let (margin, gap) = (14, 6);
        let place = |sel: PhysicalRect, tb: PhysicalRect, screen: PhysicalRect| {
            let p = calculate_region_bar_placement(sel, tb, size, screen, margin, gap);
            let bar = PhysicalRect::new(p.x, p.y, size.0, size.1);
            assert!(!overlaps(bar, tb), "与工具栏重叠 {bar:?} {tb:?}");
            assert!(bar.x >= screen.x && bar.y >= screen.y);
            assert!(bar.right() <= screen.right() && bar.bottom() <= screen.bottom());
            (p, bar)
        };
        // 右下角缩放手柄所在的小方块（角点两侧各 12 像素，含命中容差）
        let corner =
            |sel: PhysicalRect| PhysicalRect::new(sel.right() - 12, sel.bottom() - 12, 24, 24);

        // 大选区：叠在选区内右下角，边距 14，工具栏在选区下方
        let sel = PhysicalRect::new(100, 100, 600, 300);
        let tb = PhysicalRect::new(250, 408, 450, 38);
        let (p, bar) = place(sel, tb, screen);
        assert_eq!((p.x, p.y), (700 - 14 - 180, 400 - 14 - 34));
        assert!(!overlaps(bar, corner(sel)), "盖住了缩放手柄");

        // 刚好够大 / 差一像素
        let exact = PhysicalRect::new(100, 100, 180 + 28, 34 + 28);
        let (p, _) = place(exact, PhysicalRect::new(0, 500, 450, 38), screen);
        assert_eq!((p.x, p.y), (100 + 14, 100 + 14));
        let small = PhysicalRect::new(100, 100, 180 + 27, 34 + 28);
        let (p, _) = place(small, PhysicalRect::new(0, 500, 450, 38), screen);
        // 回退：选区右侧外缘、底部对齐
        assert_eq!((p.x, p.y), (small.right() + gap, small.bottom() - 34));

        // 工具栏嵌进选区内底端：形状栏改放工具栏正上方
        let sel = PhysicalRect::new(100, 100, 600, 480);
        let tb = PhysicalRect::new(242, 580 - 8 - 38, 450, 38);
        let (p, _) = place(sel, tb, screen);
        assert_eq!((p.x, p.y), (700 - 14 - 180, tb.y - gap - 34));

        // 小选区贴屏幕右侧：右边放不下，放工具栏正上方右对齐
        let sel = PhysicalRect::new(880, 100, 110, 40);
        let tb = PhysicalRect::new(540, 148, 450, 38);
        let (p, _) = place(sel, tb, screen);
        assert_eq!((p.x, p.y), (tb.right() - 180, tb.y - gap - 34));

        // 小选区贴右 + 工具栏贴屏幕顶：改放正下方
        let sel = PhysicalRect::new(880, 4, 110, 20);
        let tb = PhysicalRect::new(540, 4, 450, 38);
        let (p, _) = place(sel, tb, screen);
        assert_eq!(p.y, tb.bottom() + gap);

        // 副屏非零原点
        let screen2 = PhysicalRect::new(-1920, 100, 1920, 1080);
        let sel = PhysicalRect::new(-1800, 200, 700, 400);
        let tb = PhysicalRect::new(-1100 - 450, 608, 450, 38);
        let (_, bar) = place(sel, tb, screen2);
        assert!(overlaps(bar, sel));
        // 选区左下角贴副屏左缘的小选区
        let sel = PhysicalRect::new(-1920, 900, 100, 30);
        let tb = PhysicalRect::new(-1920, 938, 450, 38);
        place(sel, tb, screen2);
    }

    /// 菜单高度随行数与分隔线增长。
    #[test]
    fn menu_height_grows() {
        let four = menu_button_menu_height(4, 0);
        let six = menu_button_menu_height(6, 1);
        assert!(six > four + 2.0 * MENU_ROW_HEIGHT);
    }
}
