//! 输入框翻译浮窗的视图：上方文本框 + 模型下拉 + 翻译按钮，下方译文（点击复制）。
//!
//! 状态与逻辑都在 [`crate::translate_input`]（可离屏单测），这里只负责把它画出来、把操作转成事件。
//! 窗口失焦或按 Esc 即关闭；翻译在后台线程执行，结果经收件箱回到 [`TranslateInputView::finish`]。

use crate::app_runtime::UiEvent;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::translate_input::{
    InputError, PackChoice, Phase, TranslateInputModel, dropdown_choices,
};
use crate::translate_service::Translated;
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{Disableable, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;

/// 浮窗逻辑宽度。
pub const WINDOW_WIDTH: f32 = 520.0;
/// 浮窗逻辑高度。
pub const WINDOW_HEIGHT: f32 = 380.0;
/// 文本框最少行数。
const INPUT_MIN_ROWS: usize = 3;
/// 文本框最多行数（超过后框内滚动）。
const INPUT_MAX_ROWS: usize = 6;
/// 模型下拉宽度。
const SELECT_WIDTH: f32 = 220.0;
/// 模型下拉高度。
const SELECT_HEIGHT: f32 = 28.0;
/// 下拉浮层最大高度。
const SELECT_MENU_MAX_HEIGHT: f32 = 240.0;
/// 窗口内边距。
const PADDING: f32 = 12.0;
/// 控件之间的间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 14.0;
/// 状态行字号。
const STATUS_SIZE: f32 = 12.0;

/// 下拉里的一个选项（取值为包 ID，空串表示自动）。
#[derive(Clone)]
struct PackItem {
    /// 包 ID。
    id: String,
    /// 展示名。
    label: SharedString,
}

impl SearchableListItem for PackItem {
    type Value = String;

    /// 下拉与触发器显示的标签。
    fn title(&self) -> SharedString {
        self.label.clone()
    }

    /// 选项取值（包 ID）。
    fn value(&self) -> &Self::Value {
        &self.id
    }
}

/// 下拉状态实体类型。
type PackSelect = SelectState<SearchableVec<PackItem>>;

/// 输入框翻译浮窗视图。
pub struct TranslateInputView {
    /// 翻译状态机。
    model: TranslateInputModel,
    /// 输入文本框状态。
    input: Entity<TextareaState>,
    /// 模型下拉状态。
    select: Entity<PackSelect>,
    /// 当前选中的包 ID（空串为自动）。
    selected: String,
    /// 界面偏好（深浅色、语言、主色）。
    prefs: UiPrefs,
    /// 主线程收件箱（发翻译请求用）。
    inbox: MainThreadInbox<UiEvent>,
    /// 窗口是否曾经处于激活态（避免创建瞬间的“未激活”被当成失焦）。
    was_active: bool,
    /// 输入框是否已拿到过焦点（只在首帧聚焦一次）。
    focused_once: bool,
}

impl TranslateInputView {
    /// 创建视图并聚焦文本框。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `packs`：已装翻译包（不含“自动”）。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    pub fn create(
        window: &mut Window,
        app: &mut App,
        packs: Vec<PackChoice>,
        prefs: UiPrefs,
        inbox: MainThreadInbox<UiEvent>,
    ) -> Entity<Self> {
        Theme::change(
            if prefs.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            app,
        );
        let placeholder = snow_i18n_text(prefs.locale, "translate-input-placeholder");
        let input = app.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INPUT_MIN_ROWS, INPUT_MAX_ROWS)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let items: Vec<PackItem> = dropdown_choices(packs, prefs.locale)
            .into_iter()
            .map(|c| PackItem {
                id: c.id,
                label: c.label.into(),
            })
            .collect();
        let select = app.new(|cx| {
            SelectState::new(
                SearchableVec::new(items),
                Some(Default::default()),
                window,
                cx,
            )
        });
        let view = app.new(|cx| {
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _state, event: &InputEvent, window, cx| {
                    // Enter 与 Ctrl+Enter 触发翻译；Shift+Enter 留给换行
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.submit(window, cx);
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &select,
                window,
                |this: &mut Self,
                 _state,
                 event: &SelectEvent<SearchableVec<PackItem>>,
                 _window,
                 cx| {
                    let SelectEvent::Confirm(value) = event;
                    this.selected = value.clone().unwrap_or_default();
                    cx.notify();
                },
            )
            .detach();
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.was_active = true;
                } else if this.was_active {
                    window.remove_window();
                }
                cx.notify();
            })
            .detach();
            Self {
                model: TranslateInputModel::default(),
                input: input.clone(),
                select: select.clone(),
                selected: String::new(),
                prefs,
                inbox,
                was_active: false,
                focused_once: false,
            }
        });
        input.update(app, |state, cx| state.focus(window, cx));
        view
    }

    /// 发起翻译：输入为空或正忙时只更新状态，不发请求。
    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if let Some(serial) = self.model.begin(&text) {
            self.inbox.push(UiEvent::TranslateInputRequested {
                serial,
                text,
                model_id: self.selected.clone(),
            });
        }
        cx.notify();
    }

    /// 收到后台翻译结果（过期序号会被忽略）。
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
        if self.model.finish(serial, result) {
            cx.notify();
        }
    }

    /// 点击译文：复制到剪贴板。
    fn copy(&mut self, cx: &mut Context<Self>) {
        self.model.copy_now(copy_text_to_clipboard);
        cx.notify();
    }
}

/// 取界面语言下的文案。
fn snow_i18n_text(locale: &str, id: &str) -> String {
    crate::ocr_backend::i18n_for(locale).tr(id)
}

impl Render for TranslateInputView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: Palette = palette(self.prefs.dark, self.prefs.accent);
        let locale = self.prefs.locale;
        if !self.focused_once {
            self.focused_once = true;
            self.input.update(cx, |state, cx| state.focus(window, cx));
        }
        let busy = self.model.is_busy();
        let status = self.model.status_line(locale);
        let failed = matches!(self.model.phase, Phase::Failed(_));
        let status_color = if failed { p.danger } else { p.dim };
        let translation = self.model.translation().to_string();
        let has_translation = !translation.is_empty();
        let result_box = div()
            .id("translate-input-result")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .p(px(GAP))
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .overflow_y_scroll()
            .text_size(px(TEXT_SIZE))
            .when(has_translation, |d| {
                d.text_color(p.text)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| this.copy(cx)))
                    .child(translation.clone())
            })
            .when(!has_translation, |d| {
                d.text_color(p.dim)
                    .child(snow_i18n_text(locale, "translate-input-result-empty"))
            });
        let hint = has_translation.then(|| snow_i18n_text(locale, "translate-input-click-copy"));
        div()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .border_1()
            .border_color(p.border)
            .p(px(PADDING))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .on_key_down(cx.listener(|_this, ev: &KeyDownEvent, window, _cx| {
                if ev.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(Textarea::new(&self.input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(
                        div().w(px(SELECT_WIDTH)).h(px(SELECT_HEIGHT)).child(
                            Select::new(&self.select)
                                .with_size(ComponentSize::Small)
                                .menu_max_h(px(SELECT_MENU_MAX_HEIGHT)),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("translate-input-go")
                            .label(snow_i18n_text(locale, "translate-input-button"))
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                                this.submit(window, cx)
                            })),
                    ),
            )
            .child(result_box)
            .child(
                div()
                    .h(px(STATUS_SIZE * 1.6))
                    .flex()
                    .justify_between()
                    .text_size(px(STATUS_SIZE))
                    .child(
                        div()
                            .text_color(status_color)
                            .child(status.unwrap_or_default()),
                    )
                    .child(div().text_color(p.dim).child(hint.unwrap_or_default())),
            )
    }
}
