//! 右下角语音转文字浮窗的视图：可编辑文本区 + 区外的未落定文字 + 状态行 + 复制 / 关闭按钮。
//!
//! 文本合并与复制内容都在 [`super::overlay_model`]（可离屏单测），这里只负责把它画出来。
//! 窗口以“不激活”方式弹出（`focus: false`），用户点击文本区才获得键盘焦点；Esc 或关闭按钮关窗，
//! 结束后窗口保留，直到用户关闭或下一轮开始。

use super::overlay_model::{CopyState, OverlayModel};
use super::status::Status;
use super::translate::TranslationState;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{Textarea, TextareaState};
use snow_ui::ui::component::{Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;

/// 浮窗逻辑宽度。
pub const WINDOW_WIDTH: f32 = 380.0;
/// 浮窗逻辑高度。
pub const WINDOW_HEIGHT: f32 = 234.0;
/// 浮窗距工作区右、下边缘的逻辑边距。
pub const WINDOW_MARGIN: f32 = 16.0;
/// 文本区最少行数。
const INPUT_MIN_ROWS: usize = 4;
/// 文本区最多行数（超过后框内滚动）。
const INPUT_MAX_ROWS: usize = 7;
/// 窗口内边距。
const PADDING: f32 = 12.0;
/// 控件之间的间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;
/// 状态行字号。
const STATUS_SIZE: f32 = 12.0;
/// 译文区最多显示最近几句译文（窗口高度固定，更早的译文被省略）。
const TRANSLATION_MAX_SENTENCES: usize = 2;
/// 译文待出时的占位符。
const TRANSLATION_PENDING: &str = "…";

/// 语音转文字浮窗视图。
pub struct DictationView {
    /// 文本模型（待应用的改动、未落定文字、状态、复制结果）。
    model: OverlayModel,
    /// 可编辑文本区。
    input: Entity<TextareaState>,
    /// 界面偏好（深浅色、语言、主色）。
    prefs: UiPrefs,
}

impl DictationView {
    /// 创建视图（不抢焦点）。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `prefs`：界面偏好。
    pub fn create(window: &mut Window, app: &mut App, prefs: UiPrefs) -> Entity<Self> {
        Theme::change(
            if prefs.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            app,
        );
        let placeholder =
            crate::ocr_backend::i18n_for(prefs.locale).tr("dictation-overlay-placeholder");
        let input = app.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INPUT_MIN_ROWS, INPUT_MAX_ROWS)
                .placeholder(placeholder)
        });
        app.new(|_cx| Self {
            model: OverlayModel::default(),
            input,
            prefs,
        })
    }

    /// 开始新一轮：清空文本区与状态。
    pub fn begin_round(&mut self, cx: &mut Context<Self>) {
        self.model.begin_round();
        cx.notify();
    }

    /// 更新未落定文字。
    ///
    /// # 参数
    /// - `text`：PARTIAL 原文。
    pub fn push_partial(&mut self, text: &str, cx: &mut Context<Self>) {
        self.model.push_partial(text);
        cx.notify();
    }

    /// 落定一句，追加到文本区当前内容末尾。
    ///
    /// # 参数
    /// - `text`：FINAL 原文。
    pub fn push_final(&mut self, text: &str, cx: &mut Context<Self>) {
        self.model.push_final(text);
        cx.notify();
    }

    /// 用已有转写整体铺底（键入中途兜底到浮窗）。
    ///
    /// # 参数
    /// - `finals`：已落定文本。
    /// - `partial`：未落定文本。
    /// - `translations`：与定稿句序对位的按句译文状态。
    pub fn seed(
        &mut self,
        finals: &str,
        partial: &str,
        translations: &[Option<TranslationState>],
        cx: &mut Context<Self>,
    ) {
        self.model.seed(finals, partial);
        self.model.set_translations(translations);
        cx.notify();
    }

    /// 更新某一句的译文状态（按句序号对位）。
    ///
    /// # 参数
    /// - `seq`：句序号。
    /// - `state`：新状态。
    pub fn set_translation(&mut self, seq: usize, state: TranslationState, cx: &mut Context<Self>) {
        self.model.set_translation(seq, state);
        cx.notify();
    }

    /// 更新状态行。
    ///
    /// # 参数
    /// - `status`：新状态。
    pub fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.model.set_status(status);
        cx.notify();
    }

    /// 点击复制：整段（含未落定部分）写入剪贴板。
    fn copy(&mut self, cx: &mut Context<Self>) {
        let current = self.input.read(cx).value().to_string();
        self.model.copy_now(&current, copy_text_to_clipboard);
        cx.notify();
    }

    /// 把排队的文本改动应用到文本区（折叠到文本区当前内容上，不覆盖用户编辑）。
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.model.has_pending() {
            return;
        }
        let current = self.input.read(cx).value().to_string();
        if let Some(next) = self.model.take_text(&current)
            && next != current
        {
            self.input
                .update(cx, |state, cx| state.set_value(next, window, cx));
        }
    }
}

impl DictationView {
    /// 译文区的行：最近几句已参与翻译的句子，待出显示占位符，失败显示淡色提示；没有翻译时为空。
    fn translation_lines(&self, i18n: &snow_i18n::I18n) -> Vec<String> {
        let mut lines: Vec<String> = self
            .model
            .translations()
            .iter()
            .flatten()
            .map(|state| match state {
                TranslationState::Pending => TRANSLATION_PENDING.to_string(),
                TranslationState::Done(text) => text.clone(),
                TranslationState::Failed => i18n.tr("dictation-translate-failed"),
            })
            .collect();
        let skip = lines.len().saturating_sub(TRANSLATION_MAX_SENTENCES);
        lines.drain(..skip);
        lines
    }
}

impl Render for DictationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending(window, cx);
        let p: Palette = palette(self.prefs.dark, self.prefs.accent);
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let status = self
            .model
            .status()
            .map(|s| (s.message(self.prefs.locale), s.is_failed()));
        let (status_text, failed) = status.unwrap_or_default();
        let status_color = if failed { p.danger } else { p.dim };
        let partial = self.model.partial().to_string();
        let translation_lines = self.translation_lines(i18n);
        let copy_note = match self.model.copy_state() {
            CopyState::Idle => String::new(),
            CopyState::Copied => i18n.tr("dictation-overlay-copied"),
            CopyState::Failed(reason) => i18n.tr_with(
                "dictation-overlay-copy-failed",
                &snow_i18n::Args::new().named("reason", reason.as_str()),
            ),
        };
        let note_color = if matches!(self.model.copy_state(), CopyState::Failed(_)) {
            p.danger
        } else {
            p.ok
        };
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
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(GAP))
                    .text_size(px(STATUS_SIZE))
                    .child(div().flex_1().text_color(status_color).child(status_text)),
            )
            .child(Textarea::new(&self.input))
            .child(
                div()
                    .min_h(px(TEXT_SIZE * 0.8))
                    .text_size(px(TEXT_SIZE))
                    .text_color(p.dim)
                    .italic()
                    .underline()
                    .child(partial),
            )
            .when(!translation_lines.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .text_size(px(TEXT_SIZE))
                        .text_color(p.dim)
                        .children(translation_lines),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(STATUS_SIZE))
                            .text_color(note_color)
                            .child(copy_note),
                    )
                    .child(
                        Button::new("dictation-copy")
                            .with_size(ComponentSize::Small)
                            .label(i18n.tr("dictation-overlay-copy"))
                            .on_click(
                                cx.listener(|this, _event: &ClickEvent, _window, cx| this.copy(cx)),
                            ),
                    )
                    .child(
                        // 关窗即退出语音识别模式（流程层发现浮窗被关后结束本轮）
                        Button::new("dictation-close")
                            .with_size(ComponentSize::Small)
                            .label(i18n.tr("dictation-overlay-close"))
                            .on_click(cx.listener(|_this, _event: &ClickEvent, window, _cx| {
                                window.remove_window()
                            })),
                    ),
            )
    }
}

/// 计算浮窗在显示器工作区右下角的物理矩形（留边距，不盖任务栏）。
///
/// # 参数
/// - `work_area`：工作区（去掉任务栏，物理像素）。
/// - `scale`：该显示器的缩放比。
///
/// # 返回
/// 浮窗的物理矩形；工作区过小时收缩到工作区内。
///
/// ```ignore
/// let rect = bottom_right_rect(PhysicalRect::new(0, 0, 1920, 1040), ScaleFactor::ONE);
/// assert_eq!((rect.x + rect.width, rect.y + rect.height), (1920 - 16, 1040 - 16));
/// ```
pub fn bottom_right_rect(
    work_area: snow_ui::shell::geometry::PhysicalRect,
    scale: snow_ui::shell::geometry::ScaleFactor,
) -> snow_ui::shell::geometry::PhysicalRect {
    let margin = scale.to_physical(WINDOW_MARGIN);
    let width = scale.to_physical(WINDOW_WIDTH).min(work_area.width);
    let height = scale.to_physical(WINDOW_HEIGHT).min(work_area.height);
    let x = (work_area.x + work_area.width - margin - width).max(work_area.x);
    let y = (work_area.y + work_area.height - margin - height).max(work_area.y);
    snow_ui::shell::geometry::PhysicalRect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::{PhysicalRect, ScaleFactor};

    /// 100% 缩放：右下角留 16px 边距，尺寸为逻辑尺寸。
    #[test]
    fn placed_in_bottom_right_with_margin() {
        let area = PhysicalRect::new(0, 0, 1920, 1040);
        let r = bottom_right_rect(area, ScaleFactor::ONE);
        assert_eq!(
            (r.width, r.height),
            (WINDOW_WIDTH as i32, WINDOW_HEIGHT as i32)
        );
        assert_eq!(r.x + r.width, 1920 - WINDOW_MARGIN as i32);
        assert_eq!(r.y + r.height, 1040 - WINDOW_MARGIN as i32);
    }

    /// 副屏在负坐标、125% 缩放：尺寸与边距按缩放换算，仍贴工作区右下。
    #[test]
    fn secondary_monitor_with_scale() {
        let area = PhysicalRect::new(-1920, 100, 1920, 1000);
        let scale = ScaleFactor::new(1.25);
        let r = bottom_right_rect(area, scale);
        assert_eq!(r.width, scale.to_physical(WINDOW_WIDTH));
        assert_eq!(r.x + r.width, 0 - scale.to_physical(WINDOW_MARGIN));
        assert_eq!(r.y + r.height, 1100 - scale.to_physical(WINDOW_MARGIN));
        assert!(r.x >= area.x && r.y >= area.y);
    }

    /// 工作区比浮窗还小：收缩到工作区内，不越界。
    #[test]
    fn tiny_work_area_clamps() {
        let area = PhysicalRect::new(10, 20, 200, 100);
        let r = bottom_right_rect(area, ScaleFactor::ONE);
        assert_eq!((r.width, r.height), (200, 100));
        assert_eq!((r.x, r.y), (10, 20));
    }
}
