//! 绕开 gpui-kit 的最小自定义输入视图：直接实现 EntityInputHandler，
//! 每个平台回调都写日志，用来判定 gpui 平台层与 IME 是否真的接通。

use std::ops::Range;

use gpui_kit::gpui::prelude::*;
use gpui_kit::gpui::{
    App, Bounds, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, GlobalElementId, LayoutId, Pixels, ShapedLine, SharedString, Style,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, hsla, point,
    px, relative, rgb, size, white,
};

use crate::trace::log;

actions!(ime_spike, [Backspace]);

/// 回退键的按键绑定名
pub const BACKSPACE_KEY: &str = "backspace";

/// 自定义输入视图状态（偏移均为 UTF-8 字节，与 trait 交互时转 UTF-16）
pub struct RawInput {
    /// 焦点句柄
    focus_handle: FocusHandle,
    /// 当前文本
    content: String,
    /// 选区（字节）
    selected: Range<usize>,
    /// 预编辑区间（字节）
    marked: Option<Range<usize>>,
    /// 上次布局的文本行，供 bounds_for_range 使用
    last_layout: Option<ShapedLine>,
    /// 累计 replace_and_mark_text_in_range 调用次数
    n_mark: u32,
    /// 累计 replace_text_in_range 调用次数
    n_replace: u32,
    /// 累计 render 次数
    n_render: u32,
}

impl RawInput {
    /// 创建视图并聚焦。
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            content: String::new(),
            selected: 0..0,
            marked: None,
            last_layout: None,
            n_mark: 0,
            n_replace: 0,
            n_render: 0,
        }
    }

    /// 回退一个字符（有预编辑时不处理，交给输入法）。
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() || self.selected.start == 0 {
            return;
        }
        let end = self.selected.start;
        let start = self.content[..end]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.content.replace_range(start..end, "");
        self.selected = start..start;
        cx.notify();
    }

    /// UTF-8 偏移转 UTF-16 偏移。
    fn to_utf16(&self, off: usize) -> usize {
        self.content[..off.min(self.content.len())]
            .chars()
            .map(char::len_utf16)
            .sum()
    }

    /// UTF-16 偏移转 UTF-8 偏移。
    fn from_utf16(&self, off: usize) -> usize {
        let (mut u8o, mut u16c) = (0, 0);
        for ch in self.content.chars() {
            if u16c >= off {
                break;
            }
            u16c += ch.len_utf16();
            u8o += ch.len_utf8();
        }
        u8o
    }

    /// 区间 UTF-8 转 UTF-16。
    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.to_utf16(r.start)..self.to_utf16(r.end)
    }

    /// 区间 UTF-16 转 UTF-8。
    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.from_utf16(r.start)..self.from_utf16(r.end)
    }
}

impl EntityInputHandler for RawInput {
    /// 按 UTF-16 区间取文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.range_from_utf16(&range_utf16);
        adjusted.replace(self.range_to_utf16(&r));
        let out = self.content[r].to_string();
        log(format!("HANDLER text_for_range({range_utf16:?}) -> {out:?}"));
        Some(out)
    }

    /// 返回当前选区（UTF-16）。
    fn selected_text_range(
        &mut self,
        _ignore: bool,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let r = self.range_to_utf16(&self.selected);
        log(format!("HANDLER selected_text_range() -> {r:?}"));
        Some(UTF16Selection {
            range: r,
            reversed: false,
        })
    }

    /// 返回预编辑区间（UTF-16），判据的核心读数。
    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        let r = self.marked.as_ref().map(|r| self.range_to_utf16(r));
        log(format!("HANDLER marked_text_range() -> {r:?}"));
        r
    }

    /// 结束预编辑标记。
    fn unmark_text(&mut self, _w: &mut Window, _cx: &mut Context<Self>) {
        log("HANDLER unmark_text()");
        self.marked = None;
    }

    /// 提交/替换文本（GCS_RESULTSTR 及普通字符输入走这里）。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.n_replace += 1;
        log(format!(
            "HANDLER replace_text_in_range({range_utf16:?}, {text:?}) 前 content={:?} marked={:?}",
            self.content, self.marked
        ));
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.content.replace_range(range.clone(), text);
        self.selected = range.start + text.len()..range.start + text.len();
        self.marked = None;
        log(format!(
            "  └ 后 content={:?} marked=None，已调用 cx.notify()",
            self.content
        ));
        cx.notify();
    }

    /// 写入并标记预编辑文本（GCS_COMPSTR 走这里）。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_sel_utf16: Option<Range<usize>>,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.n_mark += 1;
        log(format!(
            "HANDLER replace_and_mark_text_in_range({range_utf16:?}, {text:?}, sel={new_sel_utf16:?}) 前 content={:?} marked={:?}",
            self.content, self.marked
        ));
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.content.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        self.selected = new_sel_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|n| n.start + range.start..n.end + range.start)
            .unwrap_or_else(|| range.start + text.len()..range.start + text.len());
        log(format!(
            "  └ 后 content={:?} marked={:?}，已调用 cx.notify()",
            self.content, self.marked
        ));
        cx.notify();
    }

    /// 给输入法候选窗定位：返回整行范围。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let r = self.range_from_utf16(&range_utf16);
        log(format!("HANDLER bounds_for_range({range_utf16:?})"));
        Some(Bounds::from_corners(
            point(bounds.left() + layout.x_for_index(r.start), bounds.top()),
            point(bounds.left() + layout.x_for_index(r.end), bounds.bottom()),
        ))
    }

    /// 坐标转字符索引，本 spike 不需要，恒返回 0。
    fn character_index_for_point(
        &mut self,
        _p: gpui_kit::gpui::Point<Pixels>,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        log("HANDLER character_index_for_point()");
        Some(0)
    }
}

impl Focusable for RawInput {
    /// 返回焦点句柄。
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// 负责绘制并向窗口注册输入处理器的元素
struct RawElement {
    /// 对应的视图实体
    input: Entity<RawInput>,
}

/// prepaint 产物：文本行与光标矩形
struct Prepaint {
    /// 已排版文本行
    line: Option<ShapedLine>,
    /// 光标矩形
    cursor: Option<gpui_kit::gpui::PaintQuad>,
}

impl IntoElement for RawElement {
    type Element = Self;
    /// 自身即元素。
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RawElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    /// 无元素 id。
    fn id(&self) -> Option<ElementId> {
        None
    }

    /// 无源码位置。
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    /// 布局：撑满宽度、单行高度。
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui_kit::gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    /// 预绘制：排版文本，预编辑区间加下划线。
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _rl: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let text: SharedString = if input.content.is_empty() {
            "请用搜狗输入法敲 nihao 空格".into()
        } else {
            input.content.clone().into()
        };
        let color = if input.content.is_empty() {
            hsla(0., 0., 0., 0.3)
        } else {
            style.color
        };
        let base = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match input.marked.as_ref() {
            Some(m) if !input.content.is_empty() => vec![
                TextRun { len: m.start, ..base.clone() },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun { len: text.len() - m.end, ..base },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![base],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(text, font_size, &runs, None);
        let cx_x = line.x_for_index(input.selected.end);
        let cursor = fill(
            Bounds::new(
                point(bounds.left() + cx_x, bounds.top()),
                size(px(2.), bounds.size.height),
            ),
            gpui_kit::gpui::blue(),
        );
        Prepaint {
            line: Some(line),
            cursor: Some(cursor),
        }
    }

    /// 绘制：向窗口注册输入处理器（IME 关联的关键一步），再画文本和光标。
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _insp: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _rl: &mut Self::RequestLayoutState,
        pp: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line = pp.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), TextAlign::Left, None, window, cx)
            .unwrap();
        if focus.is_focused(window)
            && let Some(c) = pp.cursor.take()
        {
            window.paint_quad(c);
        }
        self.input.update(cx, |i, _| i.last_layout = Some(line));
    }
}

impl Render for RawInput {
    /// 渲染输入行和调试信息（含各回调累计次数）。
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.n_render += 1;
        let lines = vec![
            format!("[RAW 模式] content={:?}", self.content),
            format!("marked(UTF-8)={:?}", self.marked),
            format!(
                "replace_and_mark 调用 {} 次 / replace_text 调用 {} 次 / render {} 次",
                self.n_mark, self.n_replace, self.n_render
            ),
        ];
        div()
            .size_full()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .key_context("RawInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .bg(rgb(0xeeeeee))
            .line_height(px(30.))
            .text_size(px(24.))
            .child(
                div()
                    .h(px(38.))
                    .w_full()
                    .p(px(4.))
                    .bg(white())
                    .child(RawElement { input: cx.entity() }),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .children(lines),
            )
    }
}
