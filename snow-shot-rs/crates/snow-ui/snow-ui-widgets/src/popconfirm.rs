//! 气泡确认框组件（方案 ADR-3 / 缺口组件）。
//!
//! 提供锚定在目标元素旁侧的轻量级内联确认气泡，包含警告提示图标、标题说明与确认/取消按钮，
//! 保持与原 Ant Design Qt `Popconfirm` 一致的交互体验。

use snow_ui_shell::ui::{
    Anchor, AnyElement, App, CursorStyle, Element, ElementId, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, RenderOnce, SharedString, Styled, ViewElement, Window,
    component, div, px, rgb,
};
use std::rc::Rc;

/// 气泡弹出方向与对齐锚点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PopconfirmPlacement {
    /// 顶部居中（默认）。
    #[default]
    Top,
    /// 顶部偏左。
    TopLeft,
    /// 顶部偏右。
    TopRight,
    /// 底部居中。
    Bottom,
    /// 底部偏左。
    BottomLeft,
    /// 底部偏右。
    BottomRight,
    /// 左侧。
    Left,
    /// 右侧。
    Right,
}

impl PopconfirmPlacement {
    /// 转换为底层 Popover 的锚点。
    pub fn to_anchor(self) -> Anchor {
        match self {
            Self::Top | Self::TopLeft => Anchor::BottomLeft,
            Self::TopRight => Anchor::BottomRight,
            Self::Bottom | Self::BottomLeft => Anchor::TopLeft,
            Self::BottomRight => Anchor::TopRight,
            Self::Left => Anchor::BottomRight,
            Self::Right => Anchor::TopLeft,
        }
    }
}

/// 气泡动作回调函数。
pub type PopconfirmHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// 气泡确认框组件。
pub struct Popconfirm {
    /// 组件元素 ID。
    id: ElementId,
    /// 提示标题文本。
    title: SharedString,
    /// 补充描述文本。
    description: Option<SharedString>,
    /// 确认按钮文本（默认 "确定"）。
    ok_text: SharedString,
    /// 取消按钮文本（默认 "取消"）。
    cancel_text: SharedString,
    /// 确认按钮是否为危险高亮样式（如红色）。
    ok_danger: bool,
    /// 气泡出现位置。
    placement: PopconfirmPlacement,
    /// 受控展开状态（若为 `Some` 则由外部驱动）。
    open: Option<bool>,
    /// 点击确认按钮的回调。
    on_confirm: Option<PopconfirmHandler>,
    /// 点击取消按钮的回调。
    on_cancel: Option<PopconfirmHandler>,
    /// 触发按钮对象。
    trigger: Option<component::button::Button>,
    /// 触发按钮文本。
    trigger_label: Option<SharedString>,
}

impl Popconfirm {
    /// 构造新的气泡确认框。
    ///
    /// # 参数
    /// - `id`：组件唯一标识。
    /// - `title`：确认提示标题。
    ///
    /// # 返回
    /// 气泡确认框实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::Popconfirm;
    /// let p = Popconfirm::new("del-confirm", "确定要删除这条记录吗？");
    /// assert_eq!(p.title(), "确定要删除这条记录吗？");
    /// ```
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            ok_text: "确定".into(),
            cancel_text: "取消".into(),
            ok_danger: false,
            placement: PopconfirmPlacement::default(),
            open: None,
            on_confirm: None,
            on_cancel: None,
            trigger: None,
            trigger_label: None,
        }
    }

    /// 获取提示标题文本。
    ///
    /// # 返回
    /// 标题切片。
    pub fn title(&self) -> &str {
        self.title.as_ref()
    }

    /// 设置补充描述文本。
    ///
    /// # 参数
    /// - `desc`：描述文本。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn description(mut self, desc: impl Into<SharedString>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// 设置确认按钮文本。
    ///
    /// # 参数
    /// - `text`：确认按钮文案。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn ok_text(mut self, text: impl Into<SharedString>) -> Self {
        self.ok_text = text.into();
        self
    }

    /// 设置取消按钮文本。
    ///
    /// # 参数
    /// - `text`：取消按钮文案。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn cancel_text(mut self, text: impl Into<SharedString>) -> Self {
        self.cancel_text = text.into();
        self
    }

    /// 设置确认按钮是否为危险色（红色）。
    ///
    /// # 参数
    /// - `danger`：是否危险。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn ok_danger(mut self, danger: bool) -> Self {
        self.ok_danger = danger;
        self
    }

    /// 设置弹出位置。
    ///
    /// # 参数
    /// - `placement`：放置方位。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn placement(mut self, placement: PopconfirmPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// 设置受控展开状态。
    ///
    /// # 参数
    /// - `open`：是否展开。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn open(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    /// 设置点击确认按钮的回调。
    ///
    /// # 参数
    /// - `handler`：确认触发回调。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn on_confirm(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_confirm = Some(Rc::new(handler));
        self
    }

    /// 设置点击取消按钮的回调。
    ///
    /// # 参数
    /// - `handler`：取消触发回调。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn on_cancel(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }

    /// 设置触发锚定按钮。
    ///
    /// # 参数
    /// - `button`：触发按钮。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn trigger_button(mut self, button: component::button::Button) -> Self {
        self.trigger = Some(button);
        self
    }

    /// 设置触发锚定文本标签。
    ///
    /// # 参数
    /// - `label`：触发标签文本。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn trigger_label(mut self, label: impl Into<SharedString>) -> Self {
        self.trigger_label = Some(label.into());
        self
    }
}

impl RenderOnce for Popconfirm {
    /// 渲染气泡确认框结构。
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let title = self.title;
        let description = self.description;
        let ok_text = self.ok_text;
        let cancel_text = self.cancel_text;
        let ok_danger = self.ok_danger;
        let on_confirm = self.on_confirm;
        let on_cancel = self.on_cancel;
        let anchor = self.placement.to_anchor();

        let mut popover = component::popover::Popover::new(self.id)
            .anchor(anchor)
            .offset(px(6.0))
            .content(move |_state, _w, pop_cx| {
                // 点击确认 / 取消后关闭气泡
                let pop = pop_cx.entity();
                let on_confirm_cb = on_confirm.clone();
                let on_cancel_cb = on_cancel.clone();
                let ok_btn_bg = if ok_danger {
                    rgb(0xFF4D4F) // 危险红
                } else {
                    rgb(0x1677FF) // 品牌蓝
                };

                let mut card = div()
                    .w(px(240.0))
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0xFFFFFF))
                    .shadow_lg()
                    .border_1()
                    .border_color(rgb(0xF0F0F0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    // 第一行：警告图标与标题
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap_2()
                            // 黄色叹号图标
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded_full()
                                    .bg(rgb(0xFAAD14))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(0xFFFFFF))
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("!"),
                            )
                            // 标题
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x262626))
                                    .child(title.clone()),
                            ),
                    );

                // 第二行（可选描述文本）
                if let Some(desc) = description.clone() {
                    card = card.child(
                        div()
                            .pl(px(24.0))
                            .text_size(px(12.0))
                            .text_color(rgb(0x8C8C8C))
                            .child(desc),
                    );
                }

                // 第三行：操作按钮栏
                card.child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .mt_1()
                        // 取消按钮
                        .child({
                            let cancel_click = on_cancel_cb.clone();
                            let pop = pop.clone();
                            div()
                                .id("popconfirm-cancel")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(0xD9D9D9))
                                .bg(rgb(0xFFFFFF))
                                .hover(|s| s.bg(rgb(0xF5F5F5)))
                                .cursor(CursorStyle::PointingHand)
                                .text_size(px(12.0))
                                .text_color(rgb(0x595959))
                                .on_mouse_down(MouseButton::Left, move |_, w, cx| {
                                    pop.update(cx, |state, pc| state.dismiss(w, pc));
                                    if let Some(cb) = &cancel_click {
                                        cb(w, cx);
                                    }
                                })
                                .child(cancel_text.clone())
                        })
                        // 确定按钮
                        .child({
                            let confirm_click = on_confirm_cb.clone();
                            let pop = pop.clone();
                            div()
                                .id("popconfirm-ok")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(ok_btn_bg)
                                .hover(|s| s.opacity(0.85))
                                .cursor(CursorStyle::PointingHand)
                                .text_size(px(12.0))
                                .text_color(rgb(0xFFFFFF))
                                .on_mouse_down(MouseButton::Left, move |_, w, cx| {
                                    pop.update(cx, |state, pc| state.dismiss(w, pc));
                                    if let Some(cb) = &confirm_click {
                                        cb(w, cx);
                                    }
                                })
                                .child(ok_text.clone())
                        }),
                )
            });

        if let Some(is_open) = self.open {
            popover = popover.open(is_open);
        }

        let trigger_btn = self.trigger.unwrap_or_else(|| {
            let label = self
                .trigger_label
                .unwrap_or_else(|| "确认操作".into());
            component::button::Button::new("popconfirm-trigger").label(label)
        });

        popover.trigger(trigger_btn)
    }
}

/// 允许直接作为子元素使用（与其他 `RenderOnce` 组件一致）。
impl IntoElement for Popconfirm {
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
