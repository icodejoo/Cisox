//! 输入框翻译浮窗的视图：顶部可拖动的窄标题栏，上下两块卡片（源语言 + 输入框、目标语言 + 译文框），
//! 两块之间左侧是翻译模型下拉、右侧是交换按钮。
//!
//! 状态与逻辑都在 [`crate::translate_input_session`] 与 [`crate::translate_input`]（可离屏单测），
//! 这里只负责把它画出来、把操作转成事件。输入停顿后自动翻译（防抖，同翻译页），按 Enter 立即翻译，
//! Ctrl+Enter / Shift+Enter 换行。窗口失焦不会关闭；只有 Esc 与标题栏的关闭按钮关闭它。
//! 翻译在后台线程执行，结果经收件箱回到 [`TranslateInputView::finish`]。

use crate::app_runtime::UiEvent;
use crate::history_view::apply_theme;
use crate::ocr_backend::i18n_for;
use crate::pin_action_bar::ActionSpec;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::translate_input::{InputError, PackChoice, Phase};
use crate::translate_input_session::{
    EnterAction, TranslateInputSession, enter_action, source_choices, target_choices,
};
use crate::translate_page::{AUTO_TRANSLATE_DEBOUNCE_MS, AutoDecision};
use crate::translate_page_view::{ChoiceItem, ChoiceSelect, index_of, lang_items, pack_items};
use crate::translate_service::{TranslateConfig, Translated};
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::searchable_list::SearchableVec;
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{Icon, IndexPath, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

/// 浮窗逻辑宽度。
pub const WINDOW_WIDTH: f32 = 660.0;
/// 浮窗逻辑高度。
pub const WINDOW_HEIGHT: f32 = 552.0;
/// 标题栏高度。
const TITLE_BAR_HEIGHT: f32 = 28.0;
/// 输入框的行数（超过后框内滚动）。
const INPUT_ROWS: usize = 6;
/// 译文框的高度，与六行输入框大致等高。
const RESULT_BOX_HEIGHT: f32 = 152.0;
/// 语言下拉菜单宽度（触发器宽度随文字自适应，菜单不跟着变窄）。
const LANG_MENU_WIDTH: f32 = 150.0;
/// 模型下拉菜单宽度（同上）。
const PACK_MENU_WIDTH: f32 = 200.0;
/// 下拉触发器的文字字号（组件小号尺寸的字号）。
const SELECT_TEXT_SIZE: f32 = 14.0;
/// 下拉触发器文字以外的占宽：左右内边距 16 + 边框 2 + 文字与箭头间距 4 + 箭头 16 + 余量 4。
const SELECT_CHROME_WIDTH: f32 = 42.0;
/// 下拉触发器的最小宽度，避免选项文字很短时过窄。
const SELECT_MIN_WIDTH: f32 = 48.0;
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

/// 输入浮窗里的四个独立图标动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputAction {
    Close,
    Clear,
    Swap,
    Copy,
}

/// 混合工具栏只替换动作按钮，语言与模型下拉保持原有布局和焦点。
fn input_action(
    action: InputAction,
    locale: &str,
    has_input: bool,
    has_translation: bool,
    can_swap: bool,
) -> ActionSpec<InputAction> {
    let (id, icon, key, disabled) = match action {
        InputAction::Close => (
            "translate-input-close",
            ICON_CLOSE,
            "translate-input-close",
            false,
        ),
        InputAction::Clear => (
            "translate-input-clear",
            ICON_CLEAR,
            "translate-input-clear",
            !has_input && !has_translation,
        ),
        InputAction::Swap => (
            "translate-input-swap",
            ICON_SWAP,
            "translate-page-swap",
            !can_swap,
        ),
        InputAction::Copy => (
            "translate-input-copy",
            ICON_COPY,
            "translate-input-copy",
            !has_translation,
        ),
    };
    ActionSpec {
        id,
        icon,
        tooltip: text(locale, key),
        disabled,
        loading: false,
        action,
    }
}

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
    /// 源语言下拉的全部选项（量触发器文字宽度用）。
    source_items: Vec<ChoiceItem>,
    /// 目标语言下拉的全部选项。
    target_items: Vec<ChoiceItem>,
    /// 翻译模型下拉的全部选项。
    pack_entries: Vec<ChoiceItem>,
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
        let pack_select = make_select(pack_entries.clone(), Some(IndexPath::default()), app);
        let view = app.new(|cx| {
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, state, event: &InputEvent, window, cx| match event {
                    // Enter 立即翻译；Ctrl+Enter 由我们在光标处插入换行；Shift+Enter 组件已自行换行
                    InputEvent::PressEnter { shift, secondary } => {
                        match enter_action(*shift, *secondary) {
                            EnterAction::Submit => this.submit(cx),
                            EnterAction::InsertNewline => {
                                state.update(cx, |s, cx| s.replace("\n", window, cx))
                            }
                            EnterAction::Ignore => {}
                        }
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
                source_items,
                target_items,
                pack_entries,
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
        // 开始输入的那一下：通知运行时检查翻译包还在不在内存，不在就补预热（与下面的防抖等待重叠）
        if self.session.note_edit(&text) {
            self.inbox.push(UiEvent::TranslateInputTypingStarted);
        }
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

    /// 按当前选中项的文字量出下拉触发器的宽度（含内边距与箭头，不小于最小宽度）。
    ///
    /// # 参数
    /// - `select`：下拉状态。
    /// - `items`：该下拉的全部选项（用选中值反查展示名）。
    /// - `window` / `cx`：量字与读状态用。
    ///
    /// # 返回
    /// 触发器宽度（逻辑像素）；无选中项时按空文字取最小宽度。
    fn select_width(
        select: &Entity<ChoiceSelect>,
        items: &[ChoiceItem],
        window: &Window,
        cx: &App,
    ) -> f32 {
        let label = select
            .read(cx)
            .selected_value()
            .and_then(|id| items.iter().find(|item| &item.id == id))
            .map(|item| item.label.to_string())
            .unwrap_or_default();
        (measure_text_width(window, &label, SELECT_TEXT_SIZE) + SELECT_CHROME_WIDTH)
            .max(SELECT_MIN_WIDTH)
    }

    /// 小号下拉：触发器宽度随选中文字自适应，弹出菜单保持固定宽度。
    ///
    /// # 参数
    /// - `select` / `items`：下拉状态与它的全部选项。
    /// - `menu_width`：弹出菜单宽度。
    /// - `bordered`：是否画边框和底色（语言下拉不画，模型下拉画）。
    /// - `window` / `cx`：量字与读状态用。
    fn sized_select(
        select: &Entity<ChoiceSelect>,
        items: &[ChoiceItem],
        menu_width: f32,
        bordered: bool,
        window: &Window,
        cx: &App,
    ) -> Div {
        div()
            .flex_none()
            .w(px(Self::select_width(select, items, window, cx)))
            .h(px(SELECT_HEIGHT))
            .child(
                Select::new(select)
                    .appearance(bordered)
                    .with_size(ComponentSize::Small)
                    .menu_width(px(menu_width))
                    .menu_max_h(px(SELECT_MENU_MAX_HEIGHT)),
            )
    }

    /// 共用动作入口，保留清空后聚焦、交换重翻和复制状态反馈。
    fn apply_action(&mut self, action: InputAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            InputAction::Close => window.remove_window(),
            InputAction::Clear => self.clear(window, cx),
            InputAction::Swap => self.swap(window, cx),
            InputAction::Copy => self.copy(cx),
        }
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
    /// 渲染宿主内容并装配共用图标动作。
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
                input_action(
                    InputAction::Close,
                    locale,
                    has_input,
                    has_translation,
                    can_swap,
                )
                .bind(cx, Self::apply_action)
                .render(),
            );

        // 上块：源语言 + 清空，下面是输入框
        let source_card = Self::card(
            &p,
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(Self::sized_select(
                    &self.source_select,
                    &self.source_items,
                    LANG_MENU_WIDTH,
                    false,
                    window,
                    cx,
                ))
                .child(
                    input_action(
                        InputAction::Clear,
                        locale,
                        has_input,
                        has_translation,
                        can_swap,
                    )
                    .bind(cx, Self::apply_action)
                    .render(),
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
            .child(Self::sized_select(
                &self.pack_select,
                &self.pack_entries,
                PACK_MENU_WIDTH,
                true,
                window,
                cx,
            ))
            .child(div().flex_1())
            .child(
                input_action(
                    InputAction::Swap,
                    locale,
                    has_input,
                    has_translation,
                    can_swap,
                )
                .bind(cx, Self::apply_action)
                .render_with_icon(Icon::default().path(ICON_SWAP).rotate(radians(FRAC_PI_2))),
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
                .child(Self::sized_select(
                    &self.target_select,
                    &self.target_items,
                    LANG_MENU_WIDTH,
                    false,
                    window,
                    cx,
                ))
                .child(
                    input_action(
                        InputAction::Copy,
                        locale,
                        has_input,
                        has_translation,
                        can_swap,
                    )
                    .bind(cx, Self::apply_action)
                    .render(),
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

#[cfg(test)]
mod action_bar_tests {
    use super::*;

    /// 四个生产按钮的集合、所有可用状态组合和业务回调身份均可离屏验证。
    #[test]
    fn translate_input_action_bar_states_and_dispatch() {
        for locale in ["en-US", "zh-CN"] {
            for has_input in [false, true] {
                for has_translation in [false, true] {
                    for can_swap in [false, true] {
                        let mut ids = Vec::new();
                        let mut called = Vec::new();
                        for action in [
                            InputAction::Close,
                            InputAction::Clear,
                            InputAction::Swap,
                            InputAction::Copy,
                        ] {
                            let spec =
                                input_action(action, locale, has_input, has_translation, can_swap);
                            ids.push(spec.id);
                            assert!(icon_asset_exists(spec.icon));
                            assert!(
                                !spec.tooltip.is_empty() && !spec.tooltip.starts_with("translate-")
                            );
                            let disabled = match action {
                                InputAction::Close => false,
                                InputAction::Clear => !has_input && !has_translation,
                                InputAction::Swap => !can_swap,
                                InputAction::Copy => !has_translation,
                            };
                            assert_eq!(spec.disabled, disabled);
                            let before = called.len();
                            assert_eq!(spec.dispatch(|a| called.push(a)), !disabled);
                            assert_eq!(called.len() - before, usize::from(!disabled));
                            if !disabled {
                                assert_eq!(called.last(), Some(&action));
                            }
                        }
                        assert_eq!(
                            ids,
                            [
                                "translate-input-close",
                                "translate-input-clear",
                                "translate-input-swap",
                                "translate-input-copy"
                            ]
                        );
                    }
                }
            }
        }
    }
}
