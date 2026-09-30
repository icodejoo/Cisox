//! 标注文本输入与平台 IME 事件交互处理器。
//!
//! 将 `TextDraft`、`CanvasTextStyle` 接入 GPUI 的 `EntityInputHandler`：
//! 渲染时在每帧绘制阶段把自己注册为输入处理器（否则系统输入法永远不会连通），
//! 所有与系统交换的 UTF-16 偏移都基于“显示文本”（已提交文本 + 预编辑串），
//! 光标、选区与预编辑下划线用 GPUI 的真实文字整形结果定位。

use crate::draft::TextDraft;
use crate::layout::TextLayoutResult;
use crate::style::CanvasTextStyle;
use snow_ui_shell::ui::*;
use std::ops::Range;

/// 光标条宽度（逻辑像素）。
const CARET_WIDTH: f32 = 1.5;
/// 输入框最小宽度相对字号的倍数（空文本时仍可点选、可见）。
const MIN_WIDTH_EM: f32 = 3.0;
/// 预编辑下划线粗细（逻辑像素）。
const PREEDIT_UNDERLINE: f32 = 1.5;
/// 选区高亮色（RGBA）。
const SELECTION_COLOR: u32 = 0x1677FF55;
/// 输入框虚线边框色（RGBA）。
const BOX_BORDER_COLOR: u32 = 0x1677FFCC;

/// 编辑按键的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKeyOutcome {
    /// 已处理（内容或光标变化）。
    Handled,
    /// 用户要求提交（Enter）。
    Commit,
    /// 用户要求取消（Esc）。
    Cancel,
    /// 与编辑无关，未处理。
    Ignored,
}

/// 把一次按键应用到草稿上（纯逻辑，不依赖 GPUI，可单测）。
///
/// # 参数
/// - `draft`：文本草稿。
/// - `key`：GPUI 按键名（小写，如 `backspace` / `left` / `enter`）。
/// - `shift` / `control`：修饰键。
///
/// # 返回
/// 处理结果；普通字符不在这里处理（走系统输入法通道）。
///
/// ```
/// use snow_canvas_text::{EditKeyOutcome, TextDraft, apply_edit_key};
/// let mut d = TextDraft::with_text("ab");
/// assert_eq!(apply_edit_key(&mut d, "backspace", false, false), EditKeyOutcome::Handled);
/// assert_eq!(d.text(), "a");
/// assert_eq!(apply_edit_key(&mut d, "enter", false, false), EditKeyOutcome::Commit);
/// assert_eq!(apply_edit_key(&mut d, "enter", true, false), EditKeyOutcome::Handled);
/// assert_eq!(d.text(), "a\n");
/// ```
pub fn apply_edit_key(draft: &mut TextDraft, key: &str, shift: bool, control: bool) -> EditKeyOutcome {
    let handled = match (key, control) {
        ("escape", _) => return EditKeyOutcome::Cancel,
        ("enter", false) if !shift => return EditKeyOutcome::Commit,
        ("enter", false) => draft.insert_text("\n"),
        ("backspace", _) => draft.delete_backward(),
        ("delete", _) => draft.delete_forward(),
        ("left", _) => draft.move_left(shift),
        ("right", _) => draft.move_right(shift),
        ("home", _) => draft.move_home(shift),
        ("end", _) => draft.move_end(shift),
        ("a", true) => draft.select_all(),
        ("z", true) if shift => draft.redo(),
        ("z", true) => draft.undo(),
        ("y", true) => draft.redo(),
        _ => return EditKeyOutcome::Ignored,
    };
    // 即使内容没变（例如光标已在边界）也算已处理，避免按键冒泡触发外层快捷键
    let _ = handled;
    EditKeyOutcome::Handled
}

/// 一行文本在显示文本中的位置与整形结果。
struct VisualLine {
    /// 行首在显示文本中的字节偏移。
    start: usize,
    /// 行文本字节长度（不含换行）。
    len: usize,
    /// GPUI 整形结果。
    shaped: ShapedLine,
}

/// 画布标注文本输入实体。
///
/// 具备独立焦点，对接操作系统 IME 与 GPUI 输入子系统。
pub struct CanvasTextInput {
    /// 焦点句柄。
    focus_handle: FocusHandle,
    /// 文本编辑草稿。
    pub draft: TextDraft,
    /// 文本样式（字号为逻辑像素）。
    pub style: CanvasTextStyle,
    /// 限制最大换行宽度。
    pub max_width: Option<f32>,
    /// 文本排版计算结果（启发式估算，仅供无窗口环境使用）。
    pub layout: TextLayoutResult,
    /// 累计接收的 IME 替换提交次数。
    pub replace_count: u32,
    /// 累计接收的 IME 组合标记次数。
    pub mark_count: u32,
    /// 最近一次绘制得到的输入框窗口坐标包围盒。
    last_bounds: Option<Bounds<Pixels>>,
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
    /// - `style`：排版样式（`font_size` 为逻辑像素）。
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
            last_bounds: None,
        }
    }

    /// 当前已提交文本。
    pub fn text(&self) -> &str {
        self.draft.text()
    }

    /// 处理一次编辑按键（Backspace / 方向键 / Enter / Esc 等），并刷新界面。
    ///
    /// # 参数
    /// - `key`：GPUI 按键名（小写）。
    /// - `shift` / `control`：修饰键。
    /// - `cx`：上下文通知。
    ///
    /// # 返回
    /// 处理结果；调用方据此决定提交 / 取消。
    pub fn handle_key(
        &mut self,
        key: &str,
        shift: bool,
        control: bool,
        cx: &mut Context<Self>,
    ) -> EditKeyOutcome {
        let outcome = apply_edit_key(&mut self.draft, key, shift, control);
        if outcome == EditKeyOutcome::Handled {
            self.relayout();
            cx.notify();
        }
        outcome
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

    /// 行高（逻辑像素）。
    fn line_height(&self) -> Pixels {
        px(self.style.font_size * self.style.line_height)
    }

    /// 把显示文本按行整形（真实字体度量）。
    fn shape_lines(&self, window: &Window) -> Vec<VisualLine> {
        let display = self.draft.display_text();
        let mut font = window.text_style().font();
        font.family = SharedString::from(self.style.font_family.clone());
        let color = hsla(0.0, 0.0, 0.0, 1.0);
        let mut out = Vec::new();
        let mut start = 0;
        for line in display.split('\n') {
            let runs = if line.is_empty() {
                Vec::new()
            } else {
                vec![TextRun {
                    len: line.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(line.to_string()),
                px(self.style.font_size),
                &runs,
                None,
            );
            out.push(VisualLine {
                start,
                len: line.len(),
                shaped,
            });
            start += line.len() + 1;
        }
        out
    }

    /// 显示文本字节偏移对应的框内坐标（相对输入框左上角）。
    fn position_of(&self, lines: &[VisualLine], offset: usize) -> (Pixels, Pixels) {
        let index = lines
            .iter()
            .rposition(|l| l.start <= offset)
            .unwrap_or(0);
        let Some(line) = lines.get(index) else {
            return (px(0.0), px(0.0));
        };
        let column = offset.saturating_sub(line.start).min(line.len);
        (
            line.shaped.x_for_index(column),
            self.line_height() * index as f32,
        )
    }
}

impl Focusable for CanvasTextInput {
    /// 获取焦点句柄。
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for CanvasTextInput {
    /// 按 UTF-16 区间（显示文本坐标）提取文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let display = self.draft.display_text();
        let start = self
            .draft
            .display_from_utf16(range_utf16.start.min(range_utf16.end));
        let end = self
            .draft
            .display_from_utf16(range_utf16.start.max(range_utf16.end));
        adjusted.replace(self.draft.display_to_utf16(start)..self.draft.display_to_utf16(end));
        Some(display[start..end].to_string())
    }

    /// 返回当前选区（UTF-16，显示文本坐标）。
    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.draft.display_selection_range();
        let range = self.draft.display_to_utf16(sel.start)..self.draft.display_to_utf16(sel.end);
        Some(UTF16Selection {
            range,
            reversed: !self.draft.has_preedit() && self.draft.cursor() < self.draft.anchor(),
        })
    }

    /// 返回预编辑区间（UTF-16，显示文本坐标）。
    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.draft
            .display_marked_range()
            .map(|r| self.draft.display_to_utf16(r.start)..self.draft.display_to_utf16(r.end))
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
        self.draft
            .ime_replace_and_mark(range_utf16, text, new_sel_utf16);
        self.relayout();
        cx.notify();
    }

    /// 计算指定 UTF-16 区间在窗口中的包围盒（供系统 IME 候选浮窗对齐）。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let lines = self.shape_lines(window);
        let start = self
            .draft
            .display_from_utf16(range_utf16.start.min(range_utf16.end));
        let end = self
            .draft
            .display_from_utf16(range_utf16.start.max(range_utf16.end));
        let (x0, y0) = self.position_of(&lines, start);
        let (x1, _) = self.position_of(&lines, end);
        // 跨行区间只取起点所在行
        let right = if x1 >= x0 { x1 } else { x0 };
        Some(Bounds::from_corners(
            point(element_bounds.left() + x0, element_bounds.top() + y0),
            point(
                element_bounds.left() + right,
                element_bounds.top() + y0 + self.line_height(),
            ),
        ))
    }

    /// 将窗口点坐标转换为对应的显示文本 UTF-16 偏移。
    fn character_index_for_point(
        &mut self,
        pt: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let lines = self.shape_lines(window);
        let rel_y = (pt.y - bounds.top()).max(px(0.0));
        let row = ((rel_y / self.line_height()).floor() as usize).min(lines.len().saturating_sub(1));
        let line = lines.get(row)?;
        let column = line.shaped.closest_index_for_x(pt.x - bounds.left());
        Some(self.draft.display_to_utf16(line.start + column.min(line.len)))
    }
}

impl Render for CanvasTextInput {
    /// 渲染文本输入视图：文字、选区、预编辑下划线与光标，并在绘制阶段注册输入处理器。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.shape_lines(window);
        let line_h = self.line_height();
        let font_size = px(self.style.font_size);
        let [r, g, b, a] = self.style.color;
        let text_color = rgba(u32::from_be_bytes([r, g, b, a]));
        let display = self.draft.display_text();

        let mut root = div()
            .id("canvas-text-input")
            .track_focus(&self.focus_handle)
            .relative()
            .min_w(px(self.style.font_size * MIN_WIDTH_EM))
            .cursor(CursorStyle::IBeam)
            .font_family(SharedString::from(self.style.font_family.clone()))
            .text_size(font_size)
            .text_color(text_color)
            .border_1()
            .border_dashed()
            .border_color(rgba(BOX_BORDER_COLOR));

        // 选区高亮（无预编辑时）
        if !self.draft.has_preedit() && self.draft.has_selection() {
            let sel = self.draft.selection_range();
            for (row, line) in lines.iter().enumerate() {
                let s = sel.start.max(line.start);
                let e = sel.end.min(line.start + line.len);
                if s < e {
                    let (x0, _) = self.position_of(&lines, s);
                    let (x1, _) = self.position_of(&lines, e);
                    root = root.child(
                        div()
                            .absolute()
                            .left(x0)
                            .top(line_h * row as f32)
                            .w(x1 - x0)
                            .h(line_h)
                            .bg(rgba(SELECTION_COLOR)),
                    );
                }
            }
        }

        // 每行文字
        for (row, line) in lines.iter().enumerate() {
            let text = display[line.start..line.start + line.len].to_string();
            root = root.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(line_h * row as f32)
                    .h(line_h)
                    .line_height(line_h)
                    .whitespace_nowrap()
                    .child(text),
            );
        }

        // 预编辑下划线
        if let Some(marked) = self.draft.display_marked_range() {
            for (row, line) in lines.iter().enumerate() {
                let s = marked.start.max(line.start);
                let e = marked.end.min(line.start + line.len);
                if s < e {
                    let (x0, _) = self.position_of(&lines, s);
                    let (x1, _) = self.position_of(&lines, e);
                    root = root.child(
                        div()
                            .absolute()
                            .left(x0)
                            .top(line_h * (row as f32 + 1.0) - px(PREEDIT_UNDERLINE))
                            .w(x1 - x0)
                            .h(px(PREEDIT_UNDERLINE))
                            .bg(text_color),
                    );
                }
            }
        }

        // 光标（预编辑期间跟随预编辑内部插入点）
        let (cx_pos, cy_pos) = self.position_of(&lines, self.draft.display_cursor());
        root = root.child(
            div()
                .absolute()
                .left(cx_pos)
                .top(cy_pos)
                .w(px(CARET_WIDTH))
                .h(line_h)
                .bg(text_color),
        );

        // 占位撑开高度：行数 × 行高
        root = root.child(div().h(line_h * lines.len().max(1) as f32).w(px(1.0)));

        // 绘制阶段注册输入处理器，并记录输入框包围盒
        let entity = cx.entity();
        let focus = self.focus_handle.clone();
        root.child(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, window, cx| {
                    entity.update(cx, |this, _| this.last_bounds = Some(bounds));
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, entity.clone()),
                        cx,
                    );
                },
            )
            .absolute()
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enter 提交、Shift+Enter 换行、Esc 取消。
    #[test]
    fn enter_and_escape_semantics() {
        let mut d = TextDraft::with_text("hi");
        assert_eq!(apply_edit_key(&mut d, "enter", false, false), EditKeyOutcome::Commit);
        assert_eq!(apply_edit_key(&mut d, "escape", false, false), EditKeyOutcome::Cancel);
        assert_eq!(apply_edit_key(&mut d, "enter", true, false), EditKeyOutcome::Handled);
        assert_eq!(d.text(), "hi\n");
    }

    /// 方向键、删除与全选、撤销重做。
    #[test]
    fn navigation_and_editing_keys() {
        let mut d = TextDraft::with_text("你好世界");
        apply_edit_key(&mut d, "left", false, false);
        apply_edit_key(&mut d, "backspace", false, false);
        assert_eq!(d.text(), "你好界");
        apply_edit_key(&mut d, "home", false, false);
        apply_edit_key(&mut d, "delete", false, false);
        assert_eq!(d.text(), "好界");
        apply_edit_key(&mut d, "a", false, true);
        assert!(d.has_selection());
        apply_edit_key(&mut d, "z", false, true);
        assert_eq!(d.text(), "你好界");
        apply_edit_key(&mut d, "y", false, true);
        assert_eq!(d.text(), "好界");
    }

    /// 与编辑无关的按键被忽略（交还外层）。
    #[test]
    fn unrelated_keys_are_ignored() {
        let mut d = TextDraft::new();
        assert_eq!(apply_edit_key(&mut d, "f5", false, false), EditKeyOutcome::Ignored);
        assert_eq!(apply_edit_key(&mut d, "c", false, true), EditKeyOutcome::Ignored);
    }

    /// 预编辑存在时：标记区间、选区都在显示文本（含预编辑）坐标下。
    #[test]
    fn ime_ranges_use_display_coordinates() {
        let mut d = TextDraft::with_text("ab");
        d.ime_replace_and_mark(None, "ni", None);
        // 显示文本 "abni"：预编辑 2..4（UTF-16 亦是 2..4），不能被夹成 2..2
        let marked = d.display_marked_range().unwrap();
        assert_eq!(d.display_to_utf16(marked.start)..d.display_to_utf16(marked.end), 2..4);
        // 插入点在预编辑末尾
        let sel = d.display_selection_range();
        assert_eq!(d.display_to_utf16(sel.start), 4);
    }

    /// 预编辑含增补平面字符（代理对）时 UTF-16 长度按 2 计。
    #[test]
    fn preedit_with_surrogate_pairs() {
        let mut d = TextDraft::with_text("a好");
        d.ime_replace_and_mark(None, "🚀x", None);
        let marked = d.display_marked_range().unwrap();
        let u16r = d.display_to_utf16(marked.start)..d.display_to_utf16(marked.end);
        assert_eq!(u16r, 2..5, "好=1 单元，🚀=2 单元，x=1 单元");
        assert_eq!(d.display_from_utf16(4), "a好🚀".len());
    }

    /// 预编辑位于文本中间：IME 传回的显示区间被正确换算回提交文本。
    #[test]
    fn preedit_in_the_middle_maps_back() {
        let mut d = TextDraft::with_text("abcd");
        d.set_cursor(2, false);
        d.ime_replace_and_mark(None, "你好", None);
        // 显示文本 "ab你好cd"；对整个预编辑区间（UTF-16 2..4）上屏
        d.ime_replace_text(Some(2..4), "好");
        assert_eq!(d.text(), "ab好cd");
        assert!(!d.has_preedit());
    }

    /// 端点落在预编辑内部的区间会收敛，不 panic，结果落在字符边界。
    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn range_inside_preedit_is_clamped() {
        let mut d = TextDraft::with_text("xy");
        d.ime_replace_and_mark(None, "你好吗", None);
        for r in [0..1, 2..3, 3..3, 5..2, 0..99, 99..100] {
            let c = d.committed_range_from_display_utf16(&r);
            assert!(c.start <= c.end && c.end <= d.text().len(), "{r:?} -> {c:?}");
            assert!(d.text().is_char_boundary(c.start) && d.text().is_char_boundary(c.end));
        }
    }

    /// 反向 / 越界区间的上屏不会 panic。
    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn reversed_and_out_of_range_replace_is_safe() {
        let mut d = TextDraft::with_text("hello");
        d.ime_replace_text(Some(4..1), "X");
        assert_eq!(d.text(), "hXo");
        d.ime_replace_text(Some(50..90), "!");
        assert!(d.text().ends_with('!'));
    }

    /// 无实际变化的替换不产生空撤销项。
    #[test]
    fn noop_replace_does_not_pollute_undo() {
        let mut d = TextDraft::with_text("same");
        d.ime_replace_text(Some(0..4), "same");
        assert!(!d.can_undo());
    }

    /// Home / End 在多行文本里只移动到当前行首尾。
    #[test]
    fn home_end_are_line_based() {
        let mut d = TextDraft::with_text("one\ntwo\nthree");
        d.set_cursor(6, false); // "two" 中间
        d.move_home(false);
        assert_eq!(d.cursor(), 4);
        d.move_end(false);
        assert_eq!(d.cursor(), 7);
    }
}
