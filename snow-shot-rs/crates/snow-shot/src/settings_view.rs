//! 设置页视图：侧栏分组、搜索框、按控件类型渲染的虚拟滚动列表、状态栏。
//!
//! 渲染只读取 [`SettingsState`] 里预先构建好的行模型；列表使用定高虚拟滚动，
//! 每帧只构建屏幕内可见的几行，与配置项总数无关。

use crate::settings_model::{
    Control, SLIDER_CELLS, cycle_option, edit_text, parse_hex_color, preview_text,
    slider_active_cell, slider_cell_value, step_int, window_text,
};
use crate::settings_state::{
    ConfigChange, EditTarget, KeyMods, RowModel, Scope, SettingsAction, SettingsState,
    SharedConfig, StatusKind, SystemPrefs, read_only_note,
};
use crate::ocr_backend::{OcrBackend, OcrNotice};
use crate::settings_text::{Lang, Text, group_title, t};
use snow_config::extensions::KEY_OCR_BACKEND;
use serde_json::{Value, json};
use snow_ui::ui::*;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 行高（逻辑像素），虚拟滚动要求定高。
const ROW_HEIGHT: f32 = 60.0;
/// 侧栏宽度。
const SIDEBAR_WIDTH: f32 = 220.0;
/// 行左侧标签列宽度。
const LABEL_WIDTH: f32 = 300.0;
/// 文本输入框宽度。
const FIELD_WIDTH: f32 = 300.0;
/// 搜索框宽度。
const SEARCH_WIDTH: f32 = 240.0;
/// 文本框可见字符数上限。
const FIELD_VISIBLE_CHARS: usize = 36;
/// 只读预览字符数上限。
const READ_ONLY_PREVIEW_CHARS: usize = 28;
/// 插入符字符。
const CARET: &str = "\u{258F}";
/// 单行最多显示的快捷键数量。
const SHORTCUT_CHIPS_MAX: usize = 4;
/// 自动化测试操作的间隔。
pub const AUTOTEST_STEP_INTERVAL: Duration = Duration::from_millis(350);

/// 一套配色。
#[derive(Clone, Copy)]
struct Palette {
    /// 窗口底色。
    bg: Rgba,
    /// 侧栏底色。
    sidebar: Rgba,
    /// 分隔线。
    border: Rgba,
    /// 正文。
    text: Rgba,
    /// 次要文字。
    dim: Rgba,
    /// 控件底色。
    control: Rgba,
    /// 主色。
    accent: Rgba,
    /// 主色上的文字。
    on_accent: Rgba,
    /// 错误色。
    danger: Rgba,
    /// 成功色。
    ok: Rgba,
}

/// 按深浅色与主色生成配色。
///
/// # 参数
/// - `dark`：是否深色
/// - `accent`：主色 RGBA
fn palette(dark: bool, accent: [u8; 4]) -> Palette {
    let accent = rgba(u32::from_be_bytes(accent));
    if dark {
        Palette {
            bg: rgba(0x1F1F1FFF),
            sidebar: rgba(0x141414FF),
            border: rgba(0x303030FF),
            text: rgba(0xE6E6E6FF),
            dim: rgba(0x8C8C8CFF),
            control: rgba(0x3A3A3AFF),
            accent,
            on_accent: rgba(0xFFFFFFFF),
            danger: rgba(0xFF4D4FFF),
            ok: rgba(0x52C41AFF),
        }
    } else {
        Palette {
            bg: rgba(0xFFFFFFFF),
            sidebar: rgba(0xF3F3F3FF),
            border: rgba(0xE0E0E0FF),
            text: rgba(0x1F1F1FFF),
            dim: rgba(0x7A7A7AFF),
            control: rgba(0xE6E6E6FF),
            accent,
            on_accent: rgba(0xFFFFFFFF),
            danger: rgba(0xD9363EFF),
            ok: rgba(0x389E0DFF),
        }
    }
}

/// 渲染耗时探针。
#[derive(Debug, Clone, Copy, Default)]
struct RenderProbe {
    /// 根树构建次数。
    frames: u64,
    /// 根树构建总耗时。
    root_total: Duration,
    /// 根树构建最大耗时。
    root_max: Duration,
    /// 行构建次数。
    row_batches: u64,
    /// 行构建总耗时。
    rows_total: Duration,
    /// 行构建最大耗时。
    rows_max: Duration,
    /// 最近一次行构建的行数。
    last_rows_built: usize,
}

/// 自动化验收操作（经环境变量注入，走与点击相同的状态入口）。
#[derive(Debug, Clone, PartialEq)]
pub enum AutotestOp {
    /// 切换到某个分组 id。
    Group(String),
    /// 设置搜索文本。
    Search(String),
    /// 写入某项。
    Set {
        /// 配置键。
        key: String,
        /// 值。
        value: Value,
    },
    /// 重置某项。
    Reset(String),
    /// 录入一条快捷键（跳过键盘捕获，走冲突检测与写回）。
    Shortcut {
        /// 配置键。
        key: String,
        /// 替换下标。
        index: Option<usize>,
        /// 快捷键文本。
        text: String,
    },
    /// 模拟在文本框中逐字输入并回车。
    Type {
        /// 配置键。
        key: String,
        /// 输入文本。
        text: String,
    },
    /// 滚动到第 n 行。
    Scroll(usize),
    /// 输出性能探针日志。
    Perf,
    /// 输出当前状态日志。
    State,
}

/// 解析自动化操作 JSON 数组。
///
/// # 参数
/// - `text`：JSON 文本，如 `[{"op":"group","id":"screenshot"}]`
///
/// # 返回
/// 操作列表；格式不对返回错误文本。
///
/// ```ignore
/// let ops = parse_autotest_ops(r#"[{"op":"perf"}]"#).unwrap();
/// assert_eq!(ops, vec![AutotestOp::Perf]);
/// ```
pub fn parse_autotest_ops(text: &str) -> Result<Vec<AutotestOp>, String> {
    let items: Vec<Value> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    items.iter().map(parse_autotest_op).collect()
}

/// 解析单个自动化操作。
fn parse_autotest_op(item: &Value) -> Result<AutotestOp, String> {
    let field = |name: &str| item.get(name).and_then(Value::as_str).map(str::to_string);
    let need = |name: &str| field(name).ok_or_else(|| format!("缺少字段 {name}: {item}"));
    let op = need("op")?;
    Ok(match op.as_str() {
        "group" => AutotestOp::Group(need("id")?),
        "search" => AutotestOp::Search(field("q").unwrap_or_default()),
        "set" => AutotestOp::Set {
            key: need("key")?,
            value: item.get("value").cloned().unwrap_or(Value::Null),
        },
        "reset" => AutotestOp::Reset(need("key")?),
        "shortcut" => AutotestOp::Shortcut {
            key: need("key")?,
            index: item.get("index").and_then(Value::as_u64).map(|n| n as usize),
            text: need("text")?,
        },
        "type" => AutotestOp::Type {
            key: need("key")?,
            text: need("text")?,
        },
        "scroll" => AutotestOp::Scroll(item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize),
        "perf" => AutotestOp::Perf,
        "state" => AutotestOp::State,
        other => return Err(format!("未知操作: {other}")),
    })
}

/// 设置页视图。
pub struct SettingsView {
    /// 状态机。
    state: SettingsState,
    /// 变更通知出口（热键重注册等由上层响应）。
    notify: Rc<dyn Fn(ConfigChange)>,
    /// 根焦点句柄（接收键盘输入）。
    focus: FocusHandle,
    /// 列表滚动句柄。
    list_scroll: UniformListScrollHandle,
    /// 渲染耗时探针。
    probe: RenderProbe,
}

impl SettingsView {
    /// 创建设置页并把键盘焦点交给它。
    ///
    /// # 参数
    /// - `window`：所属窗口
    /// - `app`：应用上下文
    /// - `store`：共享配置存储
    /// - `system`：系统偏好快照
    /// - `notify`：配置变更通知出口
    pub fn create(
        window: &mut Window,
        app: &mut App,
        store: SharedConfig,
        system: SystemPrefs,
        notify: Rc<dyn Fn(ConfigChange)>,
    ) -> Entity<Self> {
        let view = app.new(|cx| Self {
            state: SettingsState::new(store, system),
            notify,
            focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            probe: RenderProbe::default(),
        });
        let handle = view.read(app).focus.clone();
        window.focus(&handle, app);
        view
    }

    /// 执行动作：抢焦点、更新状态、转发变更、重绘。
    fn act(&mut self, action: SettingsAction, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        let switched = matches!(action, SettingsAction::SwitchGroup(_) | SettingsAction::SetSearch(_));
        self.state.dispatch(action);
        if switched {
            self.scroll_to_top();
        }
        self.flush_changes();
        cx.notify();
    }

    /// 把状态机积累的变更交给上层。
    fn flush_changes(&mut self) {
        for change in self.state.take_pending() {
            (self.notify)(change);
        }
    }

    /// 列表滚回顶部。
    fn scroll_to_top(&self) {
        if self.state.visible_len() > 0 {
            self.list_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        }
    }

    /// 当前界面语言。
    pub fn language(&self) -> Lang {
        self.state.prefs().lang
    }

    /// 上层热键重注册失败并已还原配置后调用：刷新界面并提示。
    ///
    /// # 参数
    /// - `key`：被回滚的配置键
    /// - `message`：错误提示
    /// - `cx`：视图上下文
    pub fn notify_reverted(&mut self, key: &'static str, message: String, cx: &mut Context<Self>) {
        self.state.notify_reverted(key, message);
        cx.notify();
    }

    /// 执行一个自动化验收操作。
    ///
    /// # 参数
    /// - `op`：操作
    /// - `window`：所属窗口
    /// - `cx`：视图上下文
    pub fn run_autotest_op(&mut self, op: &AutotestOp, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings_model::groups;
        let static_key = |key: &str| snow_config::schema::entry_for(key).map(|e| e.key);
        match op {
            AutotestOp::Group(id) => {
                if let Some(index) = groups().iter().position(|g| g.id == id) {
                    self.act(SettingsAction::SwitchGroup(index), window, cx);
                }
            }
            AutotestOp::Search(q) => self.act(SettingsAction::SetSearch(q.clone()), window, cx),
            AutotestOp::Set { key, value } => {
                if let Some(key) = static_key(key) {
                    self.act(SettingsAction::Change { key, value: value.clone() }, window, cx);
                }
            }
            AutotestOp::Reset(key) => {
                if let Some(key) = static_key(key) {
                    self.act(SettingsAction::Reset(key), window, cx);
                }
            }
            AutotestOp::Shortcut { key, index, text } => {
                if let Some(key) = static_key(key) {
                    let _ = self.state.commit_shortcut(key, *index, text);
                    self.flush_changes();
                    cx.notify();
                }
            }
            AutotestOp::Type { key, text } => {
                if let Some(key) = static_key(key) {
                    self.state.dispatch(SettingsAction::BeginEdit(key));
                    let mods = KeyMods::default();
                    for _ in 0..64 {
                        self.state.on_key("backspace", None, mods, None);
                    }
                    for ch in text.chars() {
                        let s = ch.to_string();
                        self.state.on_key(&s, Some(&s), mods, None);
                    }
                    self.state.on_key("enter", None, mods, None);
                    self.flush_changes();
                    cx.notify();
                }
            }
            AutotestOp::Scroll(index) => {
                if *index < self.state.visible_len() {
                    self.list_scroll.scroll_to_item_strict(*index, ScrollStrategy::Top);
                    cx.notify();
                }
            }
            AutotestOp::Perf => self.log_perf(),
            AutotestOp::State => self.log_state(),
        }
    }

    /// 输出当前状态摘要日志。
    pub fn log_state(&self) {
        let prefs = self.state.prefs();
        tracing::info!(
            scope = ?self.state.scope(),
            visible = self.state.visible_len(),
            total = self.state.total_rows(),
            dark = prefs.dark,
            lang = ?prefs.lang,
            status = self.state.status().map(|s| s.text.as_str()).unwrap_or(""),
            "settings state"
        );
    }

    /// 输出性能探针日志。
    pub fn log_perf(&self) {
        let perf = self.state.perf();
        let p = self.probe;
        let avg = |total: Duration, n: u64| (total.as_micros() as u64).checked_div(n).unwrap_or(0);
        tracing::info!(
            build_all_rows_us = perf.build_all_rows.as_micros() as u64,
            last_switch_us = perf.last_switch.as_micros() as u64,
            last_search_us = perf.last_search.as_micros() as u64,
            frames = p.frames,
            root_avg_us = avg(p.root_total, p.frames),
            root_max_us = p.root_max.as_micros() as u64,
            row_batches = p.row_batches,
            rows_avg_us = avg(p.rows_total, p.row_batches),
            rows_max_us = p.rows_max.as_micros() as u64,
            last_rows_built = p.last_rows_built,
            visible = self.state.visible_len(),
            "settings perf"
        );
    }

    /// 生成点击（左键按下）监听器。
    fn click(
        cx: &mut Context<Self>,
        action: SettingsAction,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
            this.act(action.clone(), window, cx);
        })
    }

    /// 小按钮（文字按钮）。
    fn button(label: impl Into<SharedString>, enabled: bool, p: &Palette) -> Div {
        div()
            .px_2()
            .h(px(24.0))
            .flex()
            .items_center()
            .rounded_md()
            .bg(p.control)
            .text_size(px(12.0))
            .text_color(if enabled { p.text } else { p.dim })
            .when(enabled, |d| d.cursor_pointer())
            .child(label.into())
    }

    /// 平铺候选按钮。
    fn chip(label: impl Into<SharedString>, active: bool, p: &Palette) -> Div {
        div()
            .px_3()
            .h(px(26.0))
            .flex()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.0))
            .bg(if active { p.accent } else { p.control })
            .text_color(if active { p.on_accent } else { p.text })
            .child(label.into())
    }

    /// 渲染一行右侧的控件。
    fn render_control(&self, row: &RowModel, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> Div {
        let key = row.key;
        let row_div = div().flex().items_center().gap_2();
        match row.control {
            Control::Switch => {
                let on = row.value.as_bool().unwrap_or(false);
                row_div.child(
                    div()
                        .w(px(40.0))
                        .h(px(22.0))
                        .relative()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if on { p.accent } else { p.control })
                        .child(
                            div()
                                .absolute()
                                .top(px(2.0))
                                .left(px(if on { 20.0 } else { 2.0 }))
                                .size(px(18.0))
                                .rounded_full()
                                .bg(rgba(0xFFFFFFFF)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            Self::click(cx, SettingsAction::Change { key, value: json!(!on) }),
                        ),
                )
            }
            Control::Slider(range) => {
                let current = row.value.as_i64().unwrap_or(i64::from(range.min));
                let active = slider_active_cell(range, current);
                let mut cells = div().flex().items_center();
                for cell in 0..SLIDER_CELLS {
                    let value = slider_cell_value(range, cell);
                    cells = cells.child(
                        div()
                            .w(px(9.0))
                            .h(px(18.0))
                            .mr(px(1.0))
                            .cursor_pointer()
                            .bg(if cell <= active { p.accent } else { p.control })
                            .on_mouse_down(
                                MouseButton::Left,
                                Self::click(cx, SettingsAction::Change { key, value: json!(value) }),
                            ),
                    );
                }
                let step_button = |label: &'static str, direction: i64, cx: &mut Context<Self>| {
                    Self::button(label, true, p).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let value = step_int(current, range, direction, event.modifiers.shift);
                            this.act(SettingsAction::Change { key, value: json!(value) }, window, cx);
                        }),
                    )
                };
                row_div
                    .child(step_button("-", -1, cx))
                    .child(cells)
                    .child(step_button("+", 1, cx))
                    .child(self.text_field(row, 84.0, p, lang, cx))
            }
            Control::IntText | Control::Text | Control::ListText | Control::JsonText => {
                row_div.child(self.text_field(row, FIELD_WIDTH, p, lang, cx))
            }
            Control::Color => {
                let swatch = parse_hex_color(row.value.as_str().unwrap_or_default())
                    .map_or(p.control, |c| rgba(u32::from_be_bytes(c)));
                row_div
                    .child(div().size(px(22.0)).rounded_md().border_1().border_color(p.border).bg(swatch))
                    .child(self.text_field(row, FIELD_WIDTH - 30.0, p, lang, cx))
            }
            Control::Choice(options) => {
                let current = row.value.as_str().unwrap_or_default().to_string();
                let mut chips = row_div;
                let locale = self.state.prefs().locale;
                for option in options {
                    let text = match OcrBackend::from_config_value(option).filter(|_| key == KEY_OCR_BACKEND) {
                        Some(backend) => backend.label(locale),
                        None => (*option).to_string(),
                    };
                    chips = chips.child(Self::chip(text, current == *option, p).on_mouse_down(
                        MouseButton::Left,
                        Self::click(cx, SettingsAction::Change { key, value: json!(option) }),
                    ));
                }
                chips
            }
            Control::Cycle(options) => {
                let current = row.value.as_str().unwrap_or_default().to_string();
                let arrow = |label: &'static str, delta: i32, cx: &mut Context<Self>| {
                    let next = cycle_option(options, &current, delta);
                    Self::button(label, true, p).on_mouse_down(
                        MouseButton::Left,
                        Self::click(cx, SettingsAction::Change { key, value: json!(next) }),
                    )
                };
                let shown = preview_text(&row.value, 24);
                row_div
                    .child(arrow("<", -1, cx))
                    .child(div().min_w(px(120.0)).px_2().text_size(px(12.0)).child(shown))
                    .child(arrow(">", 1, cx))
            }
            Control::Shortcuts { max_items, .. } => self.shortcut_editor(row, max_items, p, lang, cx),
            Control::ReadOnly(reason) => {
                let note = read_only_note(lang, reason);
                let preview = if matches!(reason, crate::settings_model::ReadOnlyReason::Secret) {
                    String::new()
                } else {
                    format!("{}  ", preview_text(&row.value, READ_ONLY_PREVIEW_CHARS))
                };
                row_div.child(
                    div()
                        .max_w(px(FIELD_WIDTH + 60.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(12.0))
                        .text_color(p.dim)
                        .child(format!("{preview}[{}: {note}]", t(lang, Text::ReadOnly))),
                )
            }
        }
    }

    /// 单行文本框：编辑中显示带插入符的窗口化文本，否则显示预览，点击进入编辑。
    fn text_field(&self, row: &RowModel, width: f32, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> Div {
        let key = row.key;
        let editing = self
            .state
            .edit()
            .filter(|e| e.target == EditTarget::Row(key));
        let (shown, active) = match editing {
            Some(edit) => {
                let (visible, cursor) = window_text(&edit.buffer, FIELD_VISIBLE_CHARS);
                let left: String = visible.chars().take(cursor).collect();
                let right: String = visible.chars().skip(cursor).collect();
                (format!("{left}{CARET}{right}"), true)
            }
            None => (preview_text(&Value::String(edit_text(row.control, &row.value)), FIELD_VISIBLE_CHARS), false),
        };
        let _ = lang;
        div()
            .w(px(width))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded_md()
            .cursor_text()
            .border_1()
            .border_color(if active { p.accent } else { p.border })
            .bg(p.control)
            .text_size(px(12.0))
            .child(shown)
            .on_mouse_down(MouseButton::Left, Self::click(cx, SettingsAction::BeginEdit(key)))
    }

    /// 快捷键编辑器：已有绑定的芯片（点击替换、× 删除）与添加按钮。
    fn shortcut_editor(
        &self,
        row: &RowModel,
        max_items: Option<usize>,
        p: &Palette,
        lang: Lang,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = row.key;
        let capture = self.state.capture().filter(|c| c.key == key);
        let prompt = t(lang, Text::PressShortcut);
        let mut list = div().flex().items_center().gap_2();
        for (index, text) in row.shortcuts.iter().take(SHORTCUT_CHIPS_MAX).enumerate() {
            let capturing_this = capture.is_some_and(|c| c.index == Some(index));
            let label = if capturing_this { prompt.to_string() } else { text.clone() };
            let chip = Self::chip(label, capturing_this, p).on_mouse_down(
                MouseButton::Left,
                Self::click(cx, SettingsAction::BeginCapture { key, index: Some(index) }),
            );
            let remove = Self::button("x", true, p).on_mouse_down(
                MouseButton::Left,
                Self::click(cx, SettingsAction::RemoveShortcut { key, index }),
            );
            list = list.child(div().flex().items_center().gap_1().child(chip).child(remove));
        }
        let full = max_items.is_some_and(|m| row.shortcuts.len() >= m);
        if capture.is_some_and(|c| c.index.is_none()) {
            list = list.child(Self::chip(prompt, true, p));
        } else if !full {
            list = list.child(
                Self::button(t(lang, Text::AddShortcut), true, p).on_mouse_down(
                    MouseButton::Left,
                    Self::click(cx, SettingsAction::BeginCapture { key, index: None }),
                ),
            );
        }
        list
    }

    /// 渲染第 `position` 个可见行。
    fn render_row(&self, position: usize, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.state.visible_row(position) else {
            return div().into_any_element();
        };
        let key = row.key;
        let read_only = matches!(row.control, Control::ReadOnly(_));
        let resettable = !row.is_default && !read_only;
        let reset = {
            let button = Self::button(t(lang, Text::ResetItem), resettable, p);
            if resettable {
                button.on_mouse_down(MouseButton::Left, Self::click(cx, SettingsAction::Reset(key)))
            } else {
                button
            }
        };
        let backend_notice = if key == KEY_OCR_BACKEND { OcrNotice::for_config_value(&row.value) } else { None };
        let sub = match (&row.error, backend_notice) {
            (Some(error), _) => div().text_color(p.danger).child(error.clone()),
            (None, Some(notice)) => div().text_color(p.danger).child(notice.message(self.state.prefs().locale)),
            (None, None) => div().text_color(p.dim).child(key),
        };
        let label = div()
            .w(px(LABEL_WIDTH))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .child(row.label.clone()),
            )
            .child(div().text_size(px(11.0)).whitespace_nowrap().text_ellipsis().child(sub));
        div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.border)
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.render_control(row, p, lang, cx))
                    .child(reset),
            )
            .into_any_element()
    }

    /// 渲染侧栏。
    fn render_sidebar(&self, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> impl IntoElement {
        let active = match self.state.scope() {
            Scope::Group(index) => Some(index),
            Scope::Search => None,
        };
        let mut list = div()
            .id("settings-sidebar-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, group) in crate::settings_model::groups().iter().enumerate() {
            let is_active = active == Some(index);
            list = list.child(
                div()
                    .h(px(32.0))
                    .px_3()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .bg(if is_active { p.accent } else { rgba(0x00000000) })
                    .text_color(if is_active { p.on_accent } else { p.text })
                    .child(group_title(lang, group.id))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(if is_active { p.on_accent } else { p.dim })
                            .child(group.entries.len().to_string()),
                    )
                    .on_mouse_down(MouseButton::Left, Self::click(cx, SettingsAction::SwitchGroup(index))),
            );
        }
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .pb_2()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::BOLD)
                    .child(t(lang, Text::Title)),
            )
            .child(list)
    }

    /// 渲染顶部栏：标题、计数、搜索框、重置本组。
    fn render_header(&self, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self
            .state
            .edit()
            .is_some_and(|e| e.target == EditTarget::Search);
        let search_text = if searching {
            let edit = self.state.edit().map(|e| &e.buffer);
            match edit {
                Some(buffer) => {
                    let (visible, cursor) = window_text(buffer, 24);
                    let left: String = visible.chars().take(cursor).collect();
                    let right: String = visible.chars().skip(cursor).collect();
                    format!("{left}{CARET}{right}")
                }
                None => String::new(),
            }
        } else if self.state.search().is_empty() {
            t(lang, Text::SearchPlaceholder).to_string()
        } else {
            self.state.search().to_string()
        };
        let dim_placeholder = !searching && self.state.search().is_empty();
        let search = div()
            .w(px(SEARCH_WIDTH))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded_md()
            .cursor_text()
            .border_1()
            .border_color(if searching { p.accent } else { p.border })
            .bg(p.control)
            .text_size(px(12.0))
            .text_color(if dim_placeholder { p.dim } else { p.text })
            .child(search_text)
            .on_mouse_down(MouseButton::Left, Self::click(cx, SettingsAction::BeginSearch));
        let reset_group = Self::button(t(lang, Text::ResetGroup), true, p)
            .on_mouse_down(MouseButton::Left, Self::click(cx, SettingsAction::ResetScope));
        div()
            .h(px(56.0))
            .px_4()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::BOLD)
                            .child(self.state.scope_title()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(p.dim)
                            .child(format!("{} {}", self.state.visible_len(), t(lang, Text::ItemsCount))),
                    ),
            )
            .child(div().flex().items_center().gap_3().child(search).child(reset_group))
    }

    /// 渲染状态栏。
    fn render_status(&self, p: &Palette) -> impl IntoElement {
        let (text, color) = match self.state.status() {
            Some(status) if status.kind == StatusKind::Error => (status.text.clone(), p.danger),
            Some(status) => (status.text.clone(), p.ok),
            None => (String::new(), p.dim),
        };
        div()
            .h(px(28.0))
            .px_4()
            .flex()
            .flex_none()
            .items_center()
            .border_t_1()
            .border_color(p.border)
            .text_size(px(12.0))
            .text_color(color)
            .overflow_hidden()
            .whitespace_nowrap()
            .child(text)
    }
}

impl Render for SettingsView {
    /// 渲染设置窗口：左侧分组，右侧标题栏 + 虚拟滚动列表 + 状态栏。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = Instant::now();
        let prefs = self.state.prefs();
        let p = palette(prefs.dark, prefs.accent);
        let lang = prefs.lang;
        let visible = self.state.visible_len();

        let body = if visible == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.dim)
                .child(t(lang, Text::NoResults))
                .into_any_element()
        } else {
            uniform_list(
                "settings-rows",
                visible,
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    let started = Instant::now();
                    let rows: Vec<AnyElement> = range
                        .clone()
                        .map(|position| this.render_row(position, &p, lang, cx))
                        .collect();
                    let elapsed = started.elapsed();
                    this.probe.row_batches += 1;
                    this.probe.rows_total += elapsed;
                    this.probe.rows_max = this.probe.rows_max.max(elapsed);
                    this.probe.last_rows_built = rows.len();
                    rows
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .into_any_element()
        };

        let root = div()
            .id("settings-root")
            .track_focus(&self.focus)
            .flex()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    if this.state.edit().is_some() || this.state.capture().is_some() {
                        this.state.cancel_input();
                        cx.notify();
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let m = event.keystroke.modifiers;
                let mods = KeyMods {
                    ctrl: m.control,
                    alt: m.alt,
                    shift: m.shift,
                    win: m.platform,
                };
                let key = event.keystroke.key.as_str();
                let paste = (m.control && key == "v")
                    .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
                    .flatten();
                let handled = this
                    .state
                    .on_key(key, event.keystroke.key_char.as_deref(), mods, paste.as_deref());
                if handled {
                    this.flush_changes();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(self.render_sidebar(&p, lang, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(&p, lang, cx))
                    .child(body)
                    .child(self.render_status(&p)),
            );

        let elapsed = started.elapsed();
        self.probe.frames += 1;
        self.probe.root_total += elapsed;
        self.probe.root_max = self.probe.root_max.max(elapsed);
        root
    }
}

impl Drop for SettingsView {
    /// 窗口关闭时输出一次性能探针，便于事后核对。
    fn drop(&mut self) {
        self.log_perf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自动化操作 JSON 解析：全部操作类型与错误输入。
    #[test]
    fn autotest_ops_parse() {
        let text = r#"[
            {"op":"group","id":"screenshot"},
            {"op":"search","q":"theme"},
            {"op":"set","key":"screenshot/image_quality","value":80},
            {"op":"reset","key":"screenshot/image_quality"},
            {"op":"shortcut","key":"global_shortcuts/screenshot","index":0,"text":"F5"},
            {"op":"type","key":"screen_recording/frame_rate","text":"45"},
            {"op":"scroll","index":3},
            {"op":"perf"},
            {"op":"state"}
        ]"#;
        let ops = parse_autotest_ops(text).unwrap();
        assert_eq!(ops.len(), 9);
        assert_eq!(ops[0], AutotestOp::Group("screenshot".into()));
        assert_eq!(
            ops[2],
            AutotestOp::Set { key: "screenshot/image_quality".into(), value: json!(80) }
        );
        assert_eq!(
            ops[4],
            AutotestOp::Shortcut { key: "global_shortcuts/screenshot".into(), index: Some(0), text: "F5".into() }
        );
        assert_eq!(ops[6], AutotestOp::Scroll(3));
        assert!(parse_autotest_ops("not json").is_err());
        assert!(parse_autotest_ops(r#"[{"op":"boom"}]"#).is_err());
        assert!(parse_autotest_ops(r#"[{"op":"set"}]"#).is_err());
    }

    /// 深浅两套配色的底色不同，主色随配置。
    #[test]
    fn palette_follows_mode_and_accent() {
        let dark = palette(true, [1, 2, 3, 255]);
        let light = palette(false, [1, 2, 3, 255]);
        assert_ne!(dark.bg, light.bg);
        assert_eq!(dark.accent, light.accent);
        assert_eq!(dark.accent, rgba(0x010203FF));
    }
}
