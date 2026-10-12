//! 翻译完成后的结果按钮条：一排纯图标按钮（悬停有文字提示），以及它相对选区的摆放计算。

use crate::menu_button::overlaps;
use crate::text_measure::FRAME_PADDING;
use crate::toolbar::{COLOR_BAR_BG, COLOR_BAR_BORDER, Look, with_tooltip};
use crate::toolbar_groups::{BAR_BORDER, BUTTON_SIZE, ITEM_GAP};
use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui_shell::ui::component::button::{Button, ButtonVariants};
use snow_ui_shell::ui::component::{Icon, Sizable, Size as ComponentSize};
use snow_ui_shell::ui::*;
use std::rc::Rc;

/// 按钮内图标的边长（逻辑像素）。
const RESULT_ICON_SIZE: f32 = 18.0;

/// 结果按钮条里的一个按钮。
#[derive(Debug, Clone)]
pub struct ResultBarItem {
    /// 稳定的元素 id 片段。
    pub id: &'static str,
    /// 图标资源路径。
    pub icon: &'static str,
    /// 悬停提示与无障碍标签（已本地化）。
    pub tooltip: String,
    /// 是否按危险色显示（如“关闭”）。
    pub danger: bool,
}

/// 点击回调，参数是被点按钮的序号。
type ClickHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// 一排图标按钮（无状态组件）。
pub struct ResultBar {
    id: String,
    items: Vec<ResultBarItem>,
    on_click: Option<ClickHandler>,
}

/// 按钮条的外尺寸 `(宽, 高)`（逻辑像素），用于定位。
///
/// # 参数
/// - `count`：按钮个数。
///
/// # 示例
/// ```rust
/// let (w, h) = snow_ui_widgets::result_bar_size(5);
/// assert!(w > h);
/// ```
pub fn result_bar_size(count: usize) -> (f32, f32) {
    let n = count as f32;
    let pad = (FRAME_PADDING + BAR_BORDER) * 2.0;
    let width = n * BUTTON_SIZE + (n - 1.0).max(0.0) * ITEM_GAP + pad;
    (width, BUTTON_SIZE + pad)
}

impl ResultBar {
    /// 构造按钮条。
    ///
    /// # 参数
    /// - `id`：元素 id。
    /// - `items`：按钮（从左到右）。
    pub fn new(id: impl Into<String>, items: Vec<ResultBarItem>) -> Self {
        Self {
            id: id.into(),
            items,
            on_click: None,
        }
    }

    /// 点击按钮的回调，参数是按钮序号。
    pub fn on_click(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ResultBar {
    /// 渲染：圆角底板里横排的图标按钮；底板吞掉鼠标按下，免得点按钮被当成新的框选。
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(ITEM_GAP))
            .p(px(FRAME_PADDING))
            .rounded_md()
            .bg(rgba(COLOR_BAR_BG))
            .border(px(BAR_BORDER))
            .border_color(rgba(COLOR_BAR_BORDER))
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        for (index, item) in self.items.iter().enumerate() {
            let button_id = format!("{}-{}", self.id, item.id);
            let look = if item.danger {
                Look::Danger
            } else {
                Look::Normal
            };
            let mut button = Button::new(SharedString::from(button_id.clone()))
                .custom(look.variant(cx))
                .with_size(ComponentSize::Size(px(BUTTON_SIZE)))
                .icon(
                    Icon::default()
                        .path(item.icon)
                        .with_size(ComponentSize::Size(px(RESULT_ICON_SIZE))),
                )
                .accessibility_label(SharedString::from(item.tooltip.clone()));
            if let Some(handler) = self.on_click.clone() {
                button = button.on_click(move |_, window, cx| handler(index, window, cx));
            }
            row = row.child(with_tooltip(button_id, item.tooltip.clone(), button));
        }
        row
    }
}

impl IntoElement for ResultBar {
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

/// 结果按钮条的摆放：选区外面放得下就放外面（选区右侧外缘、底部对齐），放不下才放进选区里面（右下角）。
///
/// 1. 外面：左缘贴选区右边 + `gap`，下缘与选区底边对齐；整条要在屏内且不压主工具栏。
/// 2. 外面不行：叠在选区内右下角，四周留 `margin`（要大于缩放手柄的命中范围）；
///    选区也放不下或压到工具栏时，改放工具栏正上方右对齐，上方不行再放正下方。
/// 3. 最后夹进屏幕范围。
///
/// # 参数
/// - `selection`：选区矩形（已裁到本屏）。
/// - `toolbar`：主工具栏矩形。
/// - `size`：按钮条外尺寸 `(宽, 高)`。
/// - `screen`：屏幕（显示器）范围。
/// - `margin`：贴选区内缘时的边距。
/// - `gap`：贴选区外缘 / 工具栏时的间距。
///
/// # 返回
/// 按钮条左上角。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalRect;
/// let screen = PhysicalRect::new(0, 0, 1000, 600);
/// let sel = PhysicalRect::new(100, 100, 300, 200);
/// let toolbar = PhysicalRect::new(100, 308, 300, 38);
/// let at = snow_ui_widgets::calculate_outer_first_bar_placement(sel, toolbar, (150, 34), screen, 14, 6);
/// assert_eq!((at.x, at.y), (sel.right() + 6, sel.bottom() - 34));
/// ```
pub fn calculate_outer_first_bar_placement(
    selection: PhysicalRect,
    toolbar: PhysicalRect,
    size: (i32, i32),
    screen: PhysicalRect,
    margin: i32,
    gap: i32,
) -> PhysicalPoint {
    let (w, h) = size;
    let rect_at = |x: i32, y: i32| PhysicalRect::new(x, y, w, h);
    let in_screen = |x: i32, y: i32| {
        x >= screen.x && y >= screen.y && x + w <= screen.right() && y + h <= screen.bottom()
    };
    let (outer_x, outer_y) = (selection.right() + gap, selection.bottom() - h);
    let (x, y) = if in_screen(outer_x, outer_y) && !overlaps(rect_at(outer_x, outer_y), toolbar) {
        (outer_x, outer_y)
    } else {
        let inner_x = selection.right() - margin - w;
        let inner_y = selection.bottom() - margin - h;
        let fits_inside = selection.width >= w + 2 * margin && selection.height >= h + 2 * margin;
        if fits_inside && !overlaps(rect_at(inner_x, inner_y), toolbar) {
            (inner_x, inner_y)
        } else {
            // 放在工具栏正上方右对齐；上方放不下改正下方
            let above = toolbar.y - gap - h;
            let y = if above >= screen.y {
                above
            } else {
                toolbar.bottom() + gap
            };
            (
                if fits_inside {
                    inner_x
                } else {
                    toolbar.right() - w
                },
                y,
            )
        }
    };
    PhysicalPoint::new(
        x.min(screen.right() - w).max(screen.x),
        y.min(screen.bottom() - h).max(screen.y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按钮条尺寸随按钮数增长，高度固定。
    #[test]
    fn size_grows_with_count() {
        let (w1, h1) = result_bar_size(1);
        let (w5, h5) = result_bar_size(5);
        assert_eq!(h1, h5);
        assert!(w5 > w1 * 4.0);
        assert_eq!(result_bar_size(0).0, result_bar_size(1).0 - BUTTON_SIZE);
    }

    /// 摆放：外面放得下就在外面；外面不够才进选区里面；都不行落到工具栏上 / 下方；始终在屏内。
    #[test]
    fn outer_first_placement_cases() {
        let screen = PhysicalRect::new(0, 0, 1000, 600);
        let size = (150, 34);
        let (margin, gap) = (14, 6);
        let place = |sel: PhysicalRect, tb: PhysicalRect, screen: PhysicalRect| {
            let p = calculate_outer_first_bar_placement(sel, tb, size, screen, margin, gap);
            let bar = PhysicalRect::new(p.x, p.y, size.0, size.1);
            assert!(!overlaps(bar, tb), "与工具栏重叠 {bar:?} {tb:?}");
            assert!(bar.x >= screen.x && bar.y >= screen.y);
            assert!(bar.right() <= screen.right() && bar.bottom() <= screen.bottom());
            (p, bar)
        };

        // 外面够：右侧外缘、底部对齐（选区够大也不进里面）
        let sel = PhysicalRect::new(100, 100, 600, 300);
        let tb = PhysicalRect::new(250, 408, 450, 38);
        let (p, bar) = place(sel, tb, screen);
        assert_eq!((p.x, p.y), (sel.right() + gap, sel.bottom() - 34));
        assert!(!overlaps(bar, sel));

        // 外面不够（右侧只剩 100 像素）：进选区里面右下角，留边距
        let sel = PhysicalRect::new(300, 100, 600, 300);
        let tb = PhysicalRect::new(300, 408, 450, 38);
        let (p, bar) = place(sel, tb, screen);
        assert_eq!(
            (p.x, p.y),
            (sel.right() - margin - 150, sel.bottom() - margin - 34)
        );
        assert!(overlaps(bar, sel));

        // 外面刚好够 / 差一像素
        let sel = PhysicalRect::new(100, 100, 300, 300);
        let tb = PhysicalRect::new(100, 408, 300, 38);
        let edge = PhysicalRect::new(0, 0, sel.right() + gap + 150, 600);
        assert_eq!(place(sel, tb, edge).0.x, sel.right() + gap);
        let tight = PhysicalRect::new(0, 0, sel.right() + gap + 149, 600);
        assert_eq!(place(sel, tb, tight).0.x, sel.right() - margin - 150);

        // 外面不够且选区也太小：放工具栏正上方右对齐
        let sel = PhysicalRect::new(850, 100, 100, 40);
        let tb = PhysicalRect::new(500, 148, 450, 38);
        let (p, _) = place(sel, tb, screen);
        assert_eq!((p.x, p.y), (tb.right() - 150, tb.y - gap - 34));

        // 外面不够、选区够大但里面被嵌进选区的工具栏占住：放工具栏正上方
        let sel = PhysicalRect::new(400, 100, 590, 480);
        let tb = PhysicalRect::new(540, 580 - 8 - 38, 450, 38);
        let (p, _) = place(sel, tb, screen);
        assert_eq!(p.y, tb.y - gap - 34);

        // 副屏非零原点：外面够
        let screen2 = PhysicalRect::new(-1920, 100, 1920, 1080);
        let sel = PhysicalRect::new(-1800, 200, 700, 400);
        let tb = PhysicalRect::new(-1550, 608, 450, 38);
        let (p, _) = place(sel, tb, screen2);
        assert_eq!(p.x, sel.right() + gap);
    }
}
