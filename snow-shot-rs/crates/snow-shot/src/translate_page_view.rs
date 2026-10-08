//! 翻译页（G07）的视图：源 / 目标语言与翻译包下拉、交换按钮、输入框、译文区与复制按钮。
//!
//! 状态与逻辑都在 [`crate::translate_page`]（可离屏单测），这里只负责画出来、把操作转成事件。
//! 翻译在后台线程执行，结果经收件箱回到 [`TranslatePageView::finish`]。

use crate::app_runtime::UiEvent;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::translate_input::{InputError, PackChoice, dropdown_choices};
use crate::translate_page::{AUTO_LANGUAGE_ID, LangChoice, TranslatePageModel, language_choices};
use crate::translate_service::{TranslateConfig, Translated};
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_translate::Lang;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{
    Disableable, IndexPath, Sizable, Size as ComponentSize, Theme, ThemeMode,
};
use snow_ui::ui::*;

/// 窗口逻辑宽度。
pub const WINDOW_WIDTH: f32 = 640.0;
/// 窗口逻辑高度。
pub const WINDOW_HEIGHT: f32 = 560.0;
/// 输入框最少行数。
const INPUT_MIN_ROWS: usize = 5;
/// 输入框最多行数（超过后框内滚动）。
const INPUT_MAX_ROWS: usize = 10;
/// 语言下拉宽度。
const LANG_SELECT_WIDTH: f32 = 170.0;
/// 翻译包下拉宽度。
const PACK_SELECT_WIDTH: f32 = 200.0;
/// 下拉高度。
const SELECT_HEIGHT: f32 = 28.0;
/// 下拉浮层最大高度。
const SELECT_MENU_MAX_HEIGHT: f32 = 260.0;
/// 窗口内边距。
const PADDING: f32 = 14.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 14.0;
/// 标签 / 状态行字号。
const SMALL_SIZE: f32 = 12.0;

/// 下拉里的一个选项（语言与翻译包共用）。
#[derive(Clone)]
struct ChoiceItem {
    /// 取值。
    id: String,
    /// 展示名。
    label: SharedString,
}

impl SearchableListItem for ChoiceItem {
    type Value = String;

    /// 下拉与触发器显示的标签。
    fn title(&self) -> SharedString {
        self.label.clone()
    }

    /// 选项取值。
    fn value(&self) -> &Self::Value {
        &self.id
    }
}

/// 下拉状态实体类型。
type ChoiceSelect = SelectState<SearchableVec<ChoiceItem>>;

/// 由语言选项建下拉条目。
fn lang_items(choices: Vec<LangChoice>) -> Vec<ChoiceItem> {
    choices
        .into_iter()
        .map(|c| ChoiceItem {
            id: c.id,
            label: c.label.into(),
        })
        .collect()
}

/// 语言的下拉取值：自动检测用 [`AUTO_LANGUAGE_ID`]，其余用语言代码。
fn lang_id(lang: Lang) -> &'static str {
    if lang == Lang::Auto {
        AUTO_LANGUAGE_ID
    } else {
        lang.code()
    }
}

/// 取值在选项里的位置。
fn index_of(items: &[ChoiceItem], id: &str) -> Option<IndexPath> {
    items
        .iter()
        .position(|i| i.id == id)
        .map(|row| IndexPath::default().row(row))
}

/// 取界面语言下的文案。
fn text(locale: &str, id: &str) -> String {
    crate::ocr_backend::i18n_for(locale).tr(id)
}

/// 翻译页视图。
pub struct TranslatePageView {
    /// 页面状态。
    model: TranslatePageModel,
    /// 输入文本框状态。
    input: Entity<TextareaState>,
    /// 源语言下拉。
    source_select: Entity<ChoiceSelect>,
    /// 目标语言下拉。
    target_select: Entity<ChoiceSelect>,
    /// 翻译包下拉。
    pack_select: Entity<ChoiceSelect>,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 主线程收件箱（发翻译请求用）。
    inbox: MainThreadInbox<UiEvent>,
    /// 输入框是否已聚焦过（只在首帧聚焦一次）。
    focused_once: bool,
}

impl TranslatePageView {
    /// 创建视图并聚焦输入框。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `config`：全局翻译配置（默认语言与后端）。
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
        Theme::change(
            if prefs.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            app,
        );
        let model = TranslatePageModel::new(config, &packs);
        let locale = prefs.locale;
        let placeholder = text(locale, "translate-page-placeholder");
        let input = app.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INPUT_MIN_ROWS, INPUT_MAX_ROWS)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let source_items = lang_items(language_choices(true, locale));
        let target_items = lang_items(language_choices(false, locale));
        let pack_items: Vec<ChoiceItem> = dropdown_choices(packs, locale)
            .into_iter()
            .map(|c| ChoiceItem {
                id: c.id,
                label: c.label.into(),
            })
            .collect();
        let mut make_select =
            |items: Vec<ChoiceItem>, selected: Option<IndexPath>, app: &mut App| {
                app.new(|cx| SelectState::new(SearchableVec::new(items), selected, window, cx))
            };
        let source_select = make_select(
            source_items.clone(),
            index_of(&source_items, lang_id(model.source)),
            app,
        );
        let target_select = make_select(
            target_items.clone(),
            index_of(&target_items, lang_id(model.target)),
            app,
        );
        let pack_select = make_select(pack_items, Some(IndexPath::default()), app);
        let view = app.new(|cx| {
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _state, event: &InputEvent, window, cx| {
                    // Enter 翻译；Shift+Enter 留给换行
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.submit(window, cx);
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &source_select,
                window,
                |this: &mut Self, _s, event: &SelectEvent<SearchableVec<ChoiceItem>>, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    if let Some(code) = value
                        && this.model.set_source(code)
                    {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &target_select,
                window,
                |this: &mut Self, _s, event: &SelectEvent<SearchableVec<ChoiceItem>>, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    if let Some(code) = value
                        && this.model.set_target(code)
                    {
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &pack_select,
                window,
                |this: &mut Self, _s, event: &SelectEvent<SearchableVec<ChoiceItem>>, _w, cx| {
                    let SelectEvent::Confirm(value) = event;
                    this.model.pack_id = value.clone().unwrap_or_default();
                    cx.notify();
                },
            )
            .detach();
            Self {
                model,
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

    /// 预填文本（不自动翻译），供入口调用方带入原文。
    ///
    /// # 参数
    /// - `text`：原文。
    /// - `window`：当前窗口。
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
    }

    /// 发起翻译：输入为空或正忙时只更新状态，不发请求。
    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if let Some(serial) = self.model.begin(&text) {
            self.inbox.push(UiEvent::TranslatePageRequested {
                serial,
                text,
                model_id: self.model.pack_id.clone(),
                source: self.model.source,
                target: self.model.target,
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

    /// 交换语言并同步两个下拉的显示。
    fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.model.swap_languages() {
            return;
        }
        let source = lang_id(self.model.source).to_string();
        let target = lang_id(self.model.target).to_string();
        self.source_select
            .update(cx, |s, cx| s.set_selected_value(&source, window, cx));
        self.target_select
            .update(cx, |s, cx| s.set_selected_value(&target, window, cx));
        cx.notify();
    }

    /// 复制译文；`close` 为真时随后关闭窗口。
    fn copy(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.model.copy_now(copy_text_to_clipboard);
        if close && !self.model.translation().is_empty() {
            window.remove_window();
        }
        cx.notify();
    }

    /// 带小标签的下拉。
    fn labeled(
        &self,
        p: &Palette,
        label: String,
        width: f32,
        select: &Entity<ChoiceSelect>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(label),
            )
            .child(
                div().w(px(width)).h(px(SELECT_HEIGHT)).child(
                    Select::new(select)
                        .with_size(ComponentSize::Small)
                        .menu_max_h(px(SELECT_MENU_MAX_HEIGHT)),
                ),
            )
    }
}

impl Render for TranslatePageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: Palette = palette(self.prefs.dark, self.prefs.accent);
        let locale = self.prefs.locale;
        if !self.focused_once {
            self.focused_once = true;
            self.input.update(cx, |state, cx| state.focus(window, cx));
        }
        let busy = self.model.is_busy();
        let status = self.model.status_line(locale);
        let status_color = if self.model.is_failed() {
            p.danger
        } else {
            p.dim
        };
        let translation = self.model.translation().to_string();
        let has_translation = !translation.is_empty();
        let result_box = div()
            .id("translate-page-result")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .p(px(GAP))
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .overflow_y_scroll()
            .text_size(px(TEXT_SIZE))
            .when(has_translation, |d| d.text_color(p.text).child(translation))
            .when(!has_translation, |d| {
                d.text_color(p.dim)
                    .child(text(locale, "translate-page-result-empty"))
            });
        let swap_button = Button::new("translate-page-swap")
            .label(text(locale, "translate-page-swap"))
            .disabled(!self.model.can_swap())
            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| this.swap(window, cx)));
        let source = self.labeled(
            &p,
            text(locale, "translate-page-label-source"),
            LANG_SELECT_WIDTH,
            &self.source_select,
        );
        let target = self.labeled(
            &p,
            text(locale, "translate-page-label-target"),
            LANG_SELECT_WIDTH,
            &self.target_select,
        );
        let pack = self.labeled(
            &p,
            text(locale, "translate-page-label-model"),
            PACK_SELECT_WIDTH,
            &self.pack_select,
        );
        div()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .p(px(PADDING))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .on_key_down(cx.listener(|_this, ev: &KeyDownEvent, window, _cx| {
                if ev.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(GAP))
                    .child(source)
                    .child(swap_button)
                    .child(target)
                    .child(div().flex_1())
                    .child(pack),
            )
            .child(Textarea::new(&self.input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(
                        Button::new("translate-page-go")
                            .label(text(locale, "translate-page-button-translate"))
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                                this.submit(window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("translate-page-copy")
                            .label(text(locale, "translate-page-button-copy"))
                            .disabled(!has_translation)
                            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                                this.copy(false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("translate-page-copy-close")
                            .label(text(locale, "translate-page-button-copy-close"))
                            .disabled(!has_translation)
                            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                                this.copy(true, window, cx)
                            })),
                    ),
            )
            .child(result_box)
            .child(
                div()
                    .min_h(px(SMALL_SIZE * 1.6))
                    .text_size(px(SMALL_SIZE))
                    .text_color(status_color)
                    .child(status.unwrap_or_default()),
            )
    }
}
