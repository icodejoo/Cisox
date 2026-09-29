//! 标注文本输入与平台 IME 事件交互处理器。
//!
//! 将 `TextDraft`、`CanvasTextStyle` 与 `TextLayoutResult` 接入 GPUI 的 `EntityInputHandler`，
//! 实现完整的中文/日文/韩文输入法组合窗口定位、预编辑串标记以及光标键盘快捷交互。

use crate::draft::TextDraft;
use crate::layout::{TextLayoutResult, TextPoint};
use crate::style::CanvasTextStyle;
use snow_ui_shell::ui::*;
use std::ops::Range;

/// 画布标注文本输入实体。
///
/// 具备独立焦点，对接操作系统 IME 与 GPUI 输入子系统。
pub struct CanvasTextInput {
    /// 焦点句柄。
    focus_handle: FocusHandle,
    /// 文本编辑草稿。
    pub draft: TextDraft,
    /// 文本样式。
    pub style: CanvasTextStyle,
    /// 限制最大换行宽度。
    pub max_width: Option<f32>,
    /// 文本排版计算结果。
    pub layout: TextLayoutResult,
    /// 累计接收的 IME 替换提交次数。
    pub replace_count: u32,
    /// 累计接收的 IME 组合标记次数。
    pub mark_count: u32,
}

impl CanvasTextInput {
    /// 构造新的输入实体并聚焦。
    ///
    /// # 参数
    /// - `window`：GPUI 窗口句柄。
    /// - `cx`：实体上下文。
    ///
    /// # 返回
    /// 初始化的输入实体。
    ///
    /// # 示例
    /// ```no_run
    /// use snow_ui_shell::ui::{self, AppContext};
    /// use snow_canvas_text::CanvasTextInput;
    /// ui::run(|shell| {
    ///     let spec = snow_ui_shell::window::WindowSpec::normal("Test", snow_ui_shell::geometry::LogicalSize::new(200.0, 200.0));
    ///     let _ = shell.open_window(&spec, |w, app| {
    ///         app.new(|cx| CanvasTextInput::new(w, cx))
    ///     });
    /// });
    /// ```
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_text_and_style(String::new(), CanvasTextStyle::default(), None, window, cx)
    }

    /// 使用初始文本与样式构造输入实体。
    ///
    /// # 参数
    /// - `text`：初始文本。
    /// - `style`：排版样式。
    /// - `max_width`：限制最大宽度（可选）。
    /// - `window`：GPUI 窗口句柄。
    /// - `cx`：实体上下文。
    ///
    /// # 返回
    /// 初始化的输入实体。
    pub fn with_text_and_style(
        text: impl Into<String>,
        style: CanvasTextStyle,
        max_width: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let draft = TextDraft::with_text(text);
        let layout = TextLayoutResult::layout_text(&draft.display_text(), &style, max_width);
        Self {
            focus_handle,
            draft,
            style,
            max_width,
            layout,
            replace_count: 0,
            mark_count: 0,
        }
    }

    /// 重新计算排版结果。
    pub fn relayout(&mut self) {
        self.layout = TextLayoutResult::layout_text(
            &self.draft.display_text(),
            &self.style,
            self.max_width,
        );
    }

    /// 替换并重设输入文本。
    ///
    /// # 参数
    /// - `text`：新文本。
    /// - `cx`：上下文通知。
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.draft = TextDraft::with_text(text);
        self.relayout();
        cx.notify();
    }

    /// 更新文本样式并重新计算排版。
    ///
    /// # 参数
    /// - `style`：新样式。
    /// - `cx`：上下文通知。
    pub fn set_style(&mut self, style: CanvasTextStyle, cx: &mut Context<Self>) {
        self.style = style;
        self.relayout();
        cx.notify();
    }

    /// 向后回退删除一个字符（Backspace）。
    ///
    /// # 参数
    /// - `cx`：上下文通知。
    pub fn backspace(&mut self, cx: &mut Context<Self>) {
        if self.draft.delete_backward() {
            self.relayout();
            cx.notify();
        }
    }

    /// 向前删除一个字符（Delete）。
    ///
    /// # 参数
    /// - `cx`：上下文通知。
    pub fn delete(&mut self, cx: &mut Context<Self>) {
        if self.draft.delete_forward() {
            self.relayout();
            cx.notify();
        }
    }

    /// 光标向左移动一个字形团。
    ///
    /// # 参数
    /// - `keep_selection`：是否扩展选区。
    /// - `cx`：上下文通知。
    pub fn move_left(&mut self, keep_selection: bool, cx: &mut Context<Self>) {
        if self.draft.move_left(keep_selection) {
            cx.notify();
        }
    }

    /// 光标向右移动一个字形团。
    ///
    /// # 参数
    /// - `keep_selection`：是否扩展选区。
    /// - `cx`：上下文通知。
    pub fn move_right(&mut self, keep_selection: bool, cx: &mut Context<Self>) {
        if self.draft.move_right(keep_selection) {
            cx.notify();
        }
    }

    /// 光标移动到文本行首。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持选区。
    /// - `cx`：上下文通知。
    pub fn move_home(&mut self, keep_selection: bool, cx: &mut Context<Self>) {
        if self.draft.move_home(keep_selection) {
            cx.notify();
        }
    }

    /// 光标移动到文本行尾。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持选区。
    /// - `cx`：上下文通知。
    pub fn move_end(&mut self, keep_selection: bool, cx: &mut Context<Self>) {
        if self.draft.move_end(keep_selection) {
            cx.notify();
        }
    }

    /// 全选文本。
    ///
    /// # 参数
    /// - `cx`：上下文通知。
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.draft.select_all() {
            cx.notify();
        }
    }

    /// 撤销上次修改。
    ///
    /// # 参数
    /// - `cx`：上下文通知。
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if self.draft.undo() {
            self.relayout();
            cx.notify();
        }
    }

    /// 重做上次撤销。
    ///
    /// # 参数
    /// - `cx`：上下文通知。
    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if self.draft.redo() {
            self.relayout();
            cx.notify();
        }
    }
}

impl Focusable for CanvasTextInput {
    /// 获取焦点句柄。
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for CanvasTextInput {
    /// 按 UTF-16 区间提取已提交文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.draft.range_from_utf16(&range_utf16);
        adjusted.replace(self.draft.range_to_utf16(&r));
        let bounded_start = r.start.min(self.draft.text().len());
        let bounded_end = r.end.min(self.draft.text().len());
        Some(self.draft.text()[bounded_start..bounded_end].to_string())
    }

    /// 返回当前选区（以 UTF-16 偏移表示）。
    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.draft.selection_range();
        let r = self.draft.range_to_utf16(&sel);
        Some(UTF16Selection {
            range: r,
            reversed: self.draft.cursor() < self.draft.anchor(),
        })
    }

    /// 返回预编辑区间（以 UTF-16 偏移表示）。
    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.draft.marked_range().map(|r| self.draft.range_to_utf16(&r))
    }

    /// 撤销并结束当前预编辑标记。
    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.draft.ime_unmark();
        self.relayout();
        cx.notify();
    }

    /// 提交或替换指定区间的文本。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_count += 1;
        self.draft.ime_replace_text(range_utf16, text);
        self.relayout();
        cx.notify();
    }

    /// 写入并标记输入法预编辑组合串。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_sel_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mark_count += 1;
        self.draft.ime_replace_and_mark(range_utf16, text, new_sel_utf16);
        self.relayout();
        cx.notify();
    }

    /// 计算指定 UTF-16 区间在窗口中的包围盒（供系统 IME 候选浮窗对齐）。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let r = self.draft.range_from_utf16(&range_utf16);
        let rects = self.layout.selection_rects(r);
        let first = rects.first()?;
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + px(first.left()),
                element_bounds.top() + px(first.top()),
            ),
            point(
                element_bounds.left() + px(first.right()),
                element_bounds.top() + px(first.bottom()),
            ),
        ))
    }

    /// 将窗口点坐标转换为对应的文本字符索引。
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let text_pt = TextPoint::new(point.x.as_f32(), point.y.as_f32());
        let byte_pos = self.layout.hit_test(text_pt);
        Some(self.draft.to_utf16(byte_pos))
    }
}

impl Render for CanvasTextInput {
    /// 渲染文本输入视图。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let display_text = self.draft.display_text();
        div()
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .child(display_text)
    }
}

