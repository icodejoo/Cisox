//! 输入框翻译浮窗的视图：顶部可拖动的窄标题栏，上下两块卡片（源语言 + 输入框、目标语言 + 译文框），
//! 两块之间左侧是翻译模型下拉、右侧是交换按钮。
//!
//! 状态与逻辑都在 [`crate::translate_input_session`] 与 [`crate::translate_input`]（可离屏单测），
//! 这里只负责把它画出来、把操作转成事件。输入停顿后自动翻译（防抖，同翻译页），按 Enter 立即翻译，
//! Shift+Enter 换行。窗口失焦不会关闭；只有 Esc 与标题栏的关闭按钮关闭它。
//! 翻译在后台线程执行，结果经收件箱回到 [`TranslateInputView::finish`]。

use crate::app_runtime::UiEvent;
use crate::history_view::apply_theme;
use crate::ocr_backend::i18n_for;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::translate_input::{InputError, PackChoice, Phase};
use crate::translate_input_session::{
    TranslateInputSession, enter_submits, source_choices, target_choices,
};
use crate::translate_page::{AUTO_TRANSLATE_DEBOUNCE_MS, AutoDecision};
use crate::translate_page_view::{ChoiceItem, ChoiceSelect, index_of, lang_items, pack_items};
use crate::translate_service::{TranslateConfig, Translated};
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::{Button, ButtonVariants};
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::searchable_list::SearchableVec;
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{Disableable, Icon, IndexPath, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

/// 浮窗逻辑宽度。
pub const WINDOW_WIDTH: f32 = 660.0;
/// 浮窗逻辑高度。
pub const WINDOW_HEIGHT: f32 = 400.0;
/// 标题栏高度。
const TITLE_BAR_HEIGHT: f32 = 28.0;
/// 输入框的行数（超过后框内滚动）。
const INPUT_ROWS: usize = 3;
/// 译文框的高度，与三行输入框大致等高。
const RESULT_BOX_HEIGHT: f32 = 76.0;
/// 语言下拉宽度。
const LANG_SELECT_WIDTH: f32 = 150.0;
/// 模型下拉宽度。
const PACK_SELECT_WIDTH: f32 = 200.0;
/// 下拉高度。
const SELECT_HEIGHT: f32 = 28.0;
/// 下拉浮层最大高度。
const SELECT_MENU_MAX_HEIGHT: f32 = 220.0;
/// 窗口左右与底部内边距。
const PADDING: f32 = 12.0;
/// 窗口顶部（标题栏下方）内边距。
const PADDING_TOP: f32 = 8.0;
/// 卡片内边距。
const CARD_PADDING: f32 = 8.0;
/// 控件之间的间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 14.0;
/// 标题 / 状态行字号。
const SMALL_SIZE: f32 = 12.0;
/// 图标按钮内图标的边长。
const ICON_SIZE: f32 = 16.0;
/// 状态行高度。
const STATUS_HEIGHT: f32 = 18.0;
/// 模型下拉与交换按钮所在行的高度。
const MIDDLE_ROW_HEIGHT: f32 = 32.0;
/// 清空图标（antd）。
const ICON_CLEAR: &str = "icons/antd/clear.svg";
/// 复制图标（antd）。
const ICON_COPY: &str = "icons/antd/copy.svg";
/// 交换图标（antd 的左右箭头，旋转 90 度成上下箭头）。
const ICON_SWAP: &str = "icons/antd/swap.svg";
/// 关闭图标（antd）。
const ICON_CLOSE: &str = "icons/antd/close.svg";

/// 取界面语言下的文案。
fn text(locale: &str, id: &str) -> String {
    i18n_for(locale).tr(id)
}

/// 语言下拉的确认事件类型。
type ChoiceEvent = SelectEvent<SearchableVec<ChoiceItem>>;

/// 输入框翻译浮窗视图。
pub struct TranslateInputView {
    /// 会话状态（语言对、翻译包、防抖、阶段）。
    session: TranslateInputSession,
    /// 输入文本框状态。
    input: Entity<TextareaState>,
    /// 源语言下拉。
    source_select: Entity<ChoiceSelect>,
    /// 目标语言下拉。
    target_select: Entity<ChoiceSelect>,
    /// 翻译模型下拉（只影响本窗口内的请求）。
    pack_select: Entity<ChoiceSelect>,
    /// 界面偏好（深浅色、语言、主色）。
    prefs: UiPrefs,
    /// 主线程收件箱（发翻译请求用）。
    inbox: MainThreadInbox<UiEvent>,
    /// 输入框是否已拿到过焦点（只在首帧聚焦一次）。
    focused_once: bool,
}

impl TranslateInputView {
    /// 创建视图并聚焦文本框。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `config`：全局翻译配置（目标语言默认值取自这里的截图翻译目标语言）。
    /// - `packs`：已装翻译包（不含“自动”）。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    pub fn create(
        window: &mut Window,
        app: &mut App,
        config: &TranslateConfig,
        packs: Vec<PackChoice>,
        prefs: UiPrefs,
        inbox: MainThreadInbox<UiEvent>,
    ) -> Entity<Self> {
        apply_theme(prefs.dark, app);
        let session = TranslateInputSession::new(config.target);
        let locale = prefs.locale;
        let placeholder = text(locale, "translate-input-placeholder");
        let input = app.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INPUT_ROWS, INPUT_ROWS)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let source_items = lang_items(source_choices(locale));
        let target_items = lang_items(target_choices(locale));
        let pack_entries = pack_items(packs, locale);
        let mut make_select =
            |items: Vec<ChoiceItem>, selected: Option<IndexPath>, app: &mut App| {
                app.new(|cx| SelectState::new(SearchableVec::new(items), selected, window, cx))
            };
        let source_select = make_select(
            source_items.clone(),
            index_of(&source_items, session.source_code()),
            app,
        );
        let target_select = make_select(
            target_items.clone(),
            index_of(&target_items, session.target_code()),
            app,
        );
        let pack_select = make_select(pack_entries, Some(IndexPath::default()), app);
        let view = app.new(|cx| {
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _state, event: &InputEvent, _window, cx| match event {
                    // Enter 立即翻译；Shift+Enter 留给换行
                    InputEvent::PressEnter { shift, .. } if enter_submits(*shift) => {
                        this.submit(cx)
                    }
                    InputEvent::Change => this.on_input_changed(cx),
                    _ => {}
                },
            )
            .detach();
            cx.subscribe_in(
                &source_select,
                window,
                |this: &mut Self, _s, event: &ChoiceEvent, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    if let Some(code) = value
                        && this.session.set_source(code)
                    {
                        this.retranslate(cx);
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &target_select,
                window,
                |this: &mut Self, _s, event: &ChoiceEvent, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    if let Some(code) = value
                        && this.session.set_target(code)
                    {
                        this.retranslate(cx);
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &pack_select,
                window,
                |this: &mut Self, _s, event: &ChoiceEvent, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    if this.session.set_pack(&value.clone().unwrap_or_default()) {
                        this.retranslate(cx);
                    }
                },
            )
            .detach();
            Self {
                session,
                input: input.clone(),
                source_select: source_select.clone(),
                target_select: target_select.clone(),
                pack_select: pack_select.clone(),
                prefs,
                inbox,
                focused_once: false,
            }
        });
        input.update(app, |state, cx| state.focus(window, cx));
        view
    }

    /// 预填文本并立即翻译（选中文字翻译用）。
    ///
    /// # 参数
    /// - `text`：要翻译的原文。
    /// - `window`：当前窗口。
    pub fn prefill_and_submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
        self.submit(cx);
    }

    /// 立即翻译输入框里的内容：输入为空或正忙时只更新状态，不发请求。
    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        self.start_request(text, cx);
    }

    /// 对 `text` 发起一次翻译请求（空文本 / 正忙时只更新状态）。
    fn start_request(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(serial) = self.session.begin(&text) {
            self.inbox.push(UiEvent::TranslateInputRequested {
                serial,
                text,
                model_id: self.session.pack_id.clone(),
                source: self.session.source,
                target: self.session.target,
            });
        }
        cx.notify();
    }

    /// 切换语言或模型后以最新选择重新翻译：输入非空立即发起，空输入只刷新界面。
    fn retranslate(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if let Some(serial) = self.session.retranslate(&text) {
            self.inbox.push(UiEvent::TranslateInputRequested {
                serial,
                text,
                model_id: self.session.pack_id.clone(),
                source: self.session.source,
                target: self.session.target,
            });
        }
        cx.notify();
    }

    /// 输入内容变化：启动一轮防抖，到点再裁决是否翻译（防抖时长同翻译页）。
    fn on_input_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let Some(generation) = self.session.input_changed(&text) else {
            return;
        };
        cx.spawn(async move |this, acx| {
            acx.background_executor()
                .timer(Duration::from_millis(AUTO_TRANSLATE_DEBOUNCE_MS))
                .await;
            let _ = this.update(acx, |view, cx| view.auto_translate_due(generation, cx));
        })
        .detach();
    }

    /// 防抖到点：按会话裁决发起翻译。
    fn auto_translate_due(&mut self, generation: u64, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if self.session.debounce_due(generation, &text) == AutoDecision::Run {
            self.start_request(text, cx);
        }
    }

    /// 收到后台翻译结果（过期序号会被忽略）；翻译期间输入又变了就补一轮。
    ///
    /// # 参数
    /// - `serial`：请求序号。
    /// - `result`：译文或失败原因。
    pub fn finish(
        &mut self,
        serial: u64,
        result: Result<Translated, InputError>,
        cx: &mut Context<Self>,
    ) {
        if self.session.finish(serial, result) {
            let text = self.input.read(cx).value().to_string();
            if self.session.take_queued(&text) {
                self.start_request(text, cx);
            }
            cx.notify();
        }
    }

    /// 点击清空图标：清空输入框与译文（语言选择保留）。
    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.clear();
        self.input
            .update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// 点击交换按钮：对调源 / 目标语言并同步两个下拉，把译文放回输入框重新翻译。
    fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(moved) = self.session.swap() else {
            return;
        };
        let source = self.session.source_code();
        let target = self.session.target_code();
        self.source_select.update(cx, |s, cx| {
            s.set_selected_value(&source.to_string(), window, cx)
        });
        self.target_select.update(cx, |s, cx| {
            s.set_selected_value(&target.to_string(), window, cx)
        });
        if !moved.is_empty() {
            self.input
                .update(cx, |state, cx| state.set_value(moved.clone(), window, cx));
            self.start_request(moved, cx);
        }
        cx.notify();
    }

    /// 点击复制图标：复制译文到剪贴板（状态行随后显示“已复制”）。
    fn copy(&mut self, cx: &mut Context<Self>) {
        self.session.flow.copy_now(copy_text_to_clipboard);
        cx.notify();
    }

    /// 无边框的语言下拉。
    fn lang_select(select: &Entity<ChoiceSelect>) -> Div {
        div().w(px(LANG_SELECT_WIDTH)).h(px(SELECT_HEIGHT)).child(
            Select::new(select)
                .appearance(false)
                .with_size(ComponentSize::Small)
                .menu_max_h(px(SELECT_MENU_MAX_HEIGHT)),
        )
    }

    /// 图标按钮（幽灵样式，悬停有文字提示）。
    fn icon_button(id: &'static str, icon: Icon, tooltip: String) -> Button {
        Button::new(id)
            .ghost()
            .with_size(ComponentSize::Small)
            .icon(icon.with_size(ComponentSize::Size(px(ICON_SIZE))))
            .tooltip(tooltip)
    }

    /// 一块灰色卡片：标题行（语言下拉 + 图标）加下面的白色文本框。
    fn card(p: &Palette, header: Div, body: impl IntoElement) -> Div {
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .gap(px(CARD_PADDING))
            .p(px(CARD_PADDING))
            .rounded_md()
            .bg(p.control)
            .child(header)
            .child(body)
    }
}

impl Render for TranslateInputView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: Palette = palette(self.prefs.dark, self.prefs.accent);
        let locale = self.prefs.locale;
        if !self.focused_once {
            self.focused_once = true;
            self.input.update(cx, |state, cx| state.focus(window, cx));
        }
        let status = self.session.flow.status_line(locale);
        let status_color = if matches!(self.session.flow.phase, Phase::Failed(_)) {
            p.danger
        } else {
            p.dim
        };
        let translation = self.session.flow.translation().to_string();
        let has_translation = !translation.is_empty();
        let has_input = !self.input.read(cx).value().is_empty();
        let can_swap = self.session.can_swap();

        // 标题栏：左侧整块是拖动区，关闭按钮是它的兄弟节点（放进拖动区会把点击吞成拖动）
        let title_bar = div()
            .flex_none()
            .h(px(TITLE_BAR_HEIGHT))
            .flex()
            .items_center()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .id("translate-input-drag")
                    .flex_1()
                    .h_full()
                    .px(px(PADDING))
                    .flex()
                    .items_center()
                    .window_control_area(WindowControlArea::Drag)
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(text(locale, "translate-input-title")),
            )
            .child(
                Self::icon_button(
                    "translate-input-close",
                    Icon::default().path(ICON_CLOSE),
                    text(locale, "translate-input-close"),
                )
                .on_click(
                    cx.listener(|_this, _e: &ClickEvent, window, _cx| window.remove_window()),
                ),
            );

        // 上块：源语言 + 清空，下面是输入框
        let source_card = Self::card(
            &p,
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(Self::lang_select(&self.source_select))
                .child(
                    Self::icon_button(
                        "translate-input-clear",
                        Icon::default().path(ICON_CLEAR),
                        text(locale, "translate-input-clear"),
                    )
                    .disabled(!has_input && !has_translation)
                    .on_click(
                        cx.listener(|this, _e: &ClickEvent, window, cx| this.clear(window, cx)),
                    ),
                ),
            Textarea::new(&self.input).bordered(false).bg(p.bg),
        );

        // 两块之间：左侧翻译模型下拉，右侧交换按钮
        let middle_row = div()
            .flex_none()
            .h(px(MIDDLE_ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(
                div()
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(text(locale, "translate-input-model-label")),
            )
            .child(
                div().w(px(PACK_SELECT_WIDTH)).h(px(SELECT_HEIGHT)).child(
                    Select::new(&self.pack_select)
                        .with_size(ComponentSize::Small)
                        .menu_max_h(px(SELECT_MENU_MAX_HEIGHT)),
                ),
            )
            .child(div().flex_1())
            .child(
                Self::icon_button(
                    "translate-input-swap",
                    Icon::default().path(ICON_SWAP).rotate(radians(FRAC_PI_2)),
                    text(locale, "translate-page-swap"),
                )
                .disabled(!can_swap)
                .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| this.swap(window, cx))),
            );

        // 译文框：白色底，空时显示占位说明
        let result_box = div()
            .id("translate-input-result")
            .w_full()
            .h(px(RESULT_BOX_HEIGHT))
            .p(px(GAP))
            .rounded_md()
            .bg(p.bg)
            .overflow_y_scroll()
            .text_size(px(TEXT_SIZE))
            .when(has_translation, |d| d.text_color(p.text).child(translation))
            .when(!has_translation, |d| {
                d.text_color(p.dim)
                    .child(text(locale, "translate-input-result-empty"))
            });

        // 下块：目标语言 + 复制，下面是译文框
        let target_card = Self::card(
            &p,
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(Self::lang_select(&self.target_select))
                .child(
                    Self::icon_button(
                        "translate-input-copy",
                        Icon::default().path(ICON_COPY),
                        text(locale, "translate-input-copy"),
                    )
                    .disabled(!has_translation)
                    .on_click(cx.listener(|this, _e: &ClickEvent, _window, cx| this.copy(cx))),
                ),
            result_box,
        );

        div()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .border_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|_this, ev: &KeyDownEvent, window, _cx| {
                if crate::shared_keys::is_dismiss(&ev.keystroke.key) {
                    window.remove_window();
                }
            }))
            .child(title_bar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .px(px(PADDING))
                    .pt(px(PADDING_TOP))
                    .pb(px(PADDING))
                    .flex()
                    .flex_col()
                    .gap(px(GAP))
                    .child(source_card)
                    .child(middle_row)
                    .child(target_card)
                    .child(
                        div()
                            .flex_none()
                            .h(px(STATUS_HEIGHT))
                            .truncate()
                            .text_size(px(SMALL_SIZE))
                            .text_color(status_color)
                            .child(status.unwrap_or_default()),
                    ),
            )
    }
}
