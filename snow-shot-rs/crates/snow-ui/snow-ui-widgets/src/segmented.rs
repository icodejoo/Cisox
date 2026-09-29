//! 分段控制器组件（方案 ADR-3 / 缺口组件）。
//!
//! 提供多项互斥切换的胶囊形分段选择器，常用于工具栏模式切换与视图状态过滤。

use std::rc::Rc;
use snow_ui_shell::ui::{
    App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
    hsla, px,
};

/// 默认选项卡高度。
pub const DEFAULT_SEGMENTED_HEIGHT: f32 = 28.0;

/// 默认背景底色（浅中性灰）。
pub const DEFAULT_BG_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.94,
    a: 1.0,
};

/// 默认选中滑块底色（纯白卡片）。
pub const DEFAULT_ACTIVE_BG_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};

/// 默认常规文本颜色。
pub const DEFAULT_TEXT_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.25,
    a: 1.0,
};

/// 默认激活项文本颜色。
pub const DEFAULT_ACTIVE_TEXT_COLOR: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.1,
    a: 1.0,
};

/// 分段项描述。
#[derive(Debug, Clone)]
pub struct SegmentedItem {
    /// 显示文案。
    pub label: SharedString,
    /// 唯一键值或代号。
    pub value: SharedString,
    /// 是否禁用。
    pub disabled: bool,
}

impl SegmentedItem {
    /// 构造新的分段选项。
    ///
    /// # 参数
    /// - `value`：选项唯一值。
    /// - `label`：展示标签文本。
    ///
    /// # 返回
    /// 分段项实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::SegmentedItem;
    /// let item = SegmentedItem::new("grid", "网格视图");
    /// assert_eq!(item.value.as_ref(), "grid");
    /// ```
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            disabled: false,
        }
    }

    /// 设置是否禁用该项。
    ///
    /// # 参数
    /// - `disabled`：是否禁用。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// 分段选项变更回调函数类型。
pub type SegmentedChangeHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// 分段选择器控件。
pub struct Segmented {
    /// 组件元素 ID。
    id: ElementId,
    /// 候选选项列表。
    items: Vec<SegmentedItem>,
    /// 当前选中的选项下标。
    selected_index: usize,
    /// 选项变更时的回调函数。
    on_change: Option<SegmentedChangeHandler>,
}

impl Segmented {
    /// 创建新的分段选择器。
    ///
    /// # 参数
    /// - `id`：元素唯一标识。
    /// - `items`：候选选项项。
    ///
    /// # 返回
    /// 分段选择器实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::{Segmented, SegmentedItem};
    /// let items = vec![SegmentedItem::new("day", "日"), SegmentedItem::new("week", "周")];
    /// let seg = Segmented::new("time_range", items);
    /// assert_eq!(seg.selected_index(), 0);
    /// ```
    pub fn new(id: impl Into<ElementId>, items: Vec<SegmentedItem>) -> Self {
        Self {
            id: id.into(),
            items,
            selected_index: 0,
            on_change: None,
        }
    }

    /// 设置当前选中项的索引。
    ///
    /// # 参数
    /// - `index`：目标索引。
    ///
    /// # 返回
    /// 修改后的实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_widgets::{Segmented, SegmentedItem};
    /// let items = vec![SegmentedItem::new("1", "A"), SegmentedItem::new("2", "B")];
    /// let seg = Segmented::new("tab", items).selected(1);
    /// assert_eq!(seg.selected_index(), 1);
    /// ```
    pub fn selected(mut self, index: usize) -> Self {
        if index < self.items.len() {
            self.selected_index = index;
        }
        self
    }

    /// 查询当前选中的下标。
    ///
    /// # 返回
    /// 选中的下标索引。
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// 注册值变更事件监听。
    ///
    /// # 参数
    /// - `handler`：发生选择变更时的闭包，入参为新选中的项下标。
    ///
    /// # 返回
    /// 修改后的实例。
    pub fn on_change(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    /// 渲染分段控制器。
    ///
    /// # 参数
    /// - `_window`：窗口上下文。
    /// - `_cx`：应用上下文。
    ///
    /// # 返回
    /// 渲染出的容器元素。
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let mut container = div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_center()
            .h(px(DEFAULT_SEGMENTED_HEIGHT))
            .p(px(2.0))
            .bg(DEFAULT_BG_COLOR)
            .rounded(px(6.0));

        let selected = self.selected_index;
        let on_change = self.on_change;

        for (idx, item) in self.items.into_iter().enumerate() {
            let is_active = idx == selected;
            let is_disabled = item.disabled;

            let mut seg_item = div()
                .id(idx)
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .px(px(10.0))
                .rounded(px(4.0))
                .text_size(px(12.0));

            if is_active {
                seg_item = seg_item
                    .bg(DEFAULT_ACTIVE_BG_COLOR)
                    .text_color(DEFAULT_ACTIVE_TEXT_COLOR)
                    .font_weight(FontWeight::MEDIUM);
            } else if is_disabled {
                seg_item = seg_item
                    .text_color(hsla(0.0, 0.0, 0.65, 1.0));
            } else {
                let handler = on_change.clone();
                seg_item = seg_item
                    .text_color(DEFAULT_TEXT_COLOR)
                    .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.5)))
                    .on_click(move |_ev: &ClickEvent, window, cx| {
                        if let Some(ref cb) = handler {
                            cb(idx, window, cx);
                        }
                    });
            }

            seg_item = seg_item.child(item.label);
            container = container.child(seg_item);
        }

        container
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证选项项构造与禁用状态。
    #[test]
    fn test_segmented_item() {
        let item = SegmentedItem::new("opt1", "选项一").disabled(true);
        assert_eq!(item.value.as_ref(), "opt1");
        assert_eq!(item.label.as_ref(), "选项一");
        assert!(item.disabled);
    }

    /// 验证分段控制器选中下标更新与边界保护。
    #[test]
    fn test_segmented_selection() {
        let items = vec![
            SegmentedItem::new("a", "A"),
            SegmentedItem::new("b", "B"),
            SegmentedItem::new("c", "C"),
        ];

        let seg = Segmented::new("test_seg", items).selected(2);
        assert_eq!(seg.selected_index(), 2);

        // 越界保护测试
        let seg_out_of_bounds = seg.selected(99);
        assert_eq!(seg_out_of_bounds.selected_index(), 2);
    }
}
