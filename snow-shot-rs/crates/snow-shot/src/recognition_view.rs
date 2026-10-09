//! 文字识别结果窗：左边是识别的图片（叠加文字块框，点框复制该段文字），右边是可编辑的全文，
//! 可以改字后再复制。覆盖窗里 OCR 完成后按 `E` 打开；窗口自带一份图片与文本，不依赖覆盖窗继续存在。

use crate::conversion_guide::ConversionGuide;
use crate::ocr_service::OcrTextBox;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::table_structure::TableTexts;
use crate::translate_flow::{LINK_BORDER, LINK_FILL, LinkHover};
use crate::translate_service::TranslatedParagraph;
use image::{Frame, RgbaImage};
use snow_i18n::Args;
use snow_platform::clipboard::copy_text_to_clipboard;
use snow_ui::shell::geometry::LogicalSize;
use snow_ui::shell::window::WindowSpec;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{Textarea, TextareaState};
use snow_ui::ui::component::{Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;
use std::sync::Arc;

/// 窗口逻辑宽度。
pub const WINDOW_WIDTH: f32 = 980.0;
/// 窗口逻辑高度。
pub const WINDOW_HEIGHT: f32 = 640.0;
/// 图片区最大边长（逻辑像素）：留出内边距、提示行与标题栏的余量，保证底部按钮完整可见。
const IMAGE_MAX_EDGE: f32 = 500.0;
/// 内边距。
const PADDING: f32 = 12.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;
/// 文字块框描边色。
const BOX_BORDER: u32 = 0x4096FFFF;
/// 文字块框填充色（半透明）。
const BOX_FILL: u32 = 0x4096FF26;

/// 打开识别结果窗所需的数据。
#[derive(Debug, Clone, PartialEq)]
pub struct RecognitionData {
    /// 图像宽（像素）。
    pub width: u32,
    /// 图像高（像素）。
    pub height: u32,
    /// 不透明 RGBA 像素。
    pub rgba: Vec<u8>,
    /// 按行拼接的完整文本。
    pub text: String,
    /// 文字块（图像像素坐标）。
    pub boxes: Vec<OcrTextBox>,
    /// 表格识别的 Markdown / TSV / HTML；普通文字识别为 `None`。
    pub table: Option<TableTexts>,
    /// 公式识别的纯 LaTeX；非公式识别为 `None`。
    pub latex: Option<String>,
    /// Markdown / HTML 模型转换通道的配置状态（只用于未配置引导）。
    pub conversion: ConversionGuide,
    /// 译文逐段对照（含每段对应的行框下标）；没有译文为 `None`。
    pub translation: Option<Vec<TranslatedParagraph>>,
}

/// 把 RGBA 像素换成 GPUI 渲染图（GPUI 内部按 BGRA 存放）；尺寸与像素数不符返回 `None`。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：RGBA 像素。
pub fn render_image(width: u32, height: u32, rgba: &[u8]) -> Option<Arc<RenderImage>> {
    let mut bgra = rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

/// 识别结果窗的窗口规格：置顶，否则会被置顶的覆盖窗盖住。
///
/// # 参数
/// - `title`：窗口标题。
pub fn window_spec(title: String) -> WindowSpec {
    let mut spec = WindowSpec::normal(title, LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
    spec.always_on_top = true;
    spec
}

/// 识别结果窗视图。
pub struct RecognitionView {
    /// 识别数据。
    data: RecognitionData,
    /// 图片的渲染图（构造失败为空）。
    image: Option<Arc<RenderImage>>,
    /// 可编辑的全文。
    text: Entity<TextareaState>,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 底部提示 `(文案, 是否错误)`。
    notice: Option<(String, bool)>,
    /// 译文段落与行框的悬停联动状态。
    hover: LinkHover,
    /// 待释放的图像资源。
    pending_drops: Vec<Arc<RenderImage>>,
}

impl RecognitionView {
    /// 创建视图。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `data`：识别数据。
    /// - `prefs`：界面偏好。
    pub fn create(
        window: &mut Window,
        app: &mut App,
        data: RecognitionData,
        prefs: UiPrefs,
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
        let initial = data.text.clone();
        let text = app.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        let image = render_image(data.width, data.height, &data.rgba);
        app.new(|_| Self {
            data,
            image,
            text,
            prefs,
            notice: None,
            hover: LinkHover::default(),
            pending_drops: Vec::new(),
        })
    }

    /// 用新识别结果替换窗口内容（单例复用）：重建图片与全文，清掉旧提示。
    ///
    /// # 参数
    /// - `data`：新的识别数据。
    /// - `window` / `cx`：窗口与视图上下文。
    pub fn replace_data(
        &mut self,
        data: RecognitionData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(old) = self.image.take() {
            self.pending_drops.push(old);
        }
        self.image = render_image(data.width, data.height, &data.rgba);
        let text = data.text.clone();
        self.text
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.data = data;
        self.notice = None;
        self.hover.clear();
        cx.notify();
    }

    /// 图像缩放到图片区后的逻辑尺寸与缩放系数（图像像素 → 逻辑像素）。
    fn fit(&self) -> (f32, f32, f32) {
        let longest = self.data.width.max(self.data.height).max(1) as f32;
        let k = (IMAGE_MAX_EDGE / longest).min(1.0);
        (self.data.width as f32 * k, self.data.height as f32 * k, k)
    }

    /// 显示模型转换通道的引导（未配置按错误色，已配置按普通提示）。
    fn show_conversion_guide(&mut self, cx: &mut Context<Self>) {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let ready = matches!(self.data.conversion, ConversionGuide::Ready(_));
        self.notice = Some((self.data.conversion.message(i18n), !ready));
        cx.notify();
    }

    /// 译文段落对照（没有译文时为空切片）。
    fn paragraphs(&self) -> &[TranslatedParagraph] {
        self.data.translation.as_deref().unwrap_or(&[])
    }

    /// 复制一段文字并给出提示。
    fn copy(&mut self, text: &str, cx: &mut Context<Self>) {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        self.notice = Some(match copy_text_to_clipboard(text) {
            Ok(()) => (i18n.tr("recwin-notice-copied"), false),
            Err(e) => (
                i18n.tr_with("recwin-notice-copy-failed", &Args::new().arg(1, e)),
                true,
            ),
        });
        cx.notify();
    }
}

impl Render for RecognitionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.pending_drops.drain(..) {
            let _ = window.drop_image(image);
        }
        let p: Palette = palette(self.prefs.dark, self.prefs.accent);
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let (w, h, k) = self.fit();

        let mut image_box = div().relative().w(px(w)).h(px(h)).flex_none().bg(p.control);
        if let Some(image) = &self.image {
            image_box = image_box.child(
                img(ImageSource::Render(Arc::clone(image)))
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .w(px(w))
                    .h(px(h))
                    .object_fit(ObjectFit::Fill),
            );
        }
        let linked = self.data.translation.is_some();
        for (ix, b) in self.data.boxes.iter().enumerate() {
            let text = b.text.clone();
            let active = linked && self.hover.box_active(self.paragraphs(), ix);
            image_box = image_box.child(
                div()
                    .id(("recwin-box", ix))
                    .absolute()
                    .top(px(b.rect.y as f32 * k))
                    .left(px(b.rect.x as f32 * k))
                    .w(px(b.rect.width as f32 * k))
                    .h(px(b.rect.height as f32 * k))
                    .border_1()
                    .border_color(rgba(if active { LINK_BORDER } else { BOX_BORDER }))
                    .bg(rgba(if active { LINK_FILL } else { BOX_FILL }))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                        if this.data.translation.is_some() {
                            this.hover.set_box(ix, *hovered);
                            cx.notify();
                        }
                    }))
                    .on_click(
                        cx.listener(move |this, _e: &ClickEvent, _w, cx| this.copy(&text, cx)),
                    ),
            );
        }

        let left = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(image_box)
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(p.dim)
                    .child(i18n.tr("recwin-hint-click-box")),
            );

        let copy_all = Button::new("recwin-copy-all")
            .with_size(ComponentSize::Small)
            .label(i18n.tr("recwin-copy-all"))
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                let text = this.text.read(cx).value().to_string();
                this.copy(&text, cx);
            }));
        let close = Button::new("recwin-close")
            .with_size(ComponentSize::Small)
            .label(i18n.tr("recwin-close"))
            .on_click(cx.listener(|_this, _e: &ClickEvent, window, _cx| window.remove_window()));
        let mut format_row = div().flex().flex_wrap().items_center().gap(px(GAP));
        if let Some(table) = self.data.table.clone() {
            let (markdown, html) = (table.markdown, table.html);
            format_row = format_row
                .child(
                    Button::new("recwin-copy-markdown")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("recwin-copy-markdown"))
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                            this.copy(&markdown, cx)
                        })),
                )
                .child(
                    Button::new("recwin-copy-html")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("recwin-copy-html"))
                        .on_click(
                            cx.listener(move |this, _e: &ClickEvent, _w, cx| this.copy(&html, cx)),
                        ),
                );
        }
        if let Some(latex) = self.data.latex.clone() {
            let block = crate::latex_ocr::wrap_display(&latex);
            format_row = format_row
                .child(
                    Button::new("recwin-copy-latex")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("recwin-copy-latex"))
                        .on_click(
                            cx.listener(move |this, _e: &ClickEvent, _w, cx| this.copy(&latex, cx)),
                        ),
                )
                .child(
                    Button::new("recwin-copy-latex-block")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("recwin-copy-latex-block"))
                        .on_click(
                            cx.listener(move |this, _e: &ClickEvent, _w, cx| this.copy(&block, cx)),
                        ),
                );
        }
        for (id, key) in [
            ("recwin-convert-markdown", "recwin-convert-markdown"),
            ("recwin-convert-html", "recwin-convert-html"),
        ] {
            format_row = format_row.child(
                Button::new(id)
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr(key))
                    .on_click(
                        cx.listener(|this, _e: &ClickEvent, _w, cx| this.show_conversion_guide(cx)),
                    ),
            );
        }
        let mut translation_area = None;
        if let Some(paragraphs) = &self.data.translation {
            let active = self.hover.active_paragraph(paragraphs);
            let mut list = div()
                .id("recwin-translation-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(GAP / 2.0))
                .p(px(GAP))
                .border_1()
                .border_color(p.control)
                .rounded_md();
            for (ix, para) in paragraphs.iter().enumerate() {
                let on = active == Some(ix);
                list = list.child(
                    div()
                        .id(("recwin-para", ix))
                        .px(px(GAP / 2.0))
                        .py(px(2.0))
                        .rounded_sm()
                        .border_1()
                        .border_color(rgba(if on { LINK_BORDER } else { 0x00000000 }))
                        .bg(rgba(if on { LINK_FILL } else { 0x00000000 }))
                        .text_size(px(TEXT_SIZE))
                        .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                            this.hover.set_paragraph(ix, *hovered);
                            cx.notify();
                        }))
                        .child(para.translated.clone()),
                );
            }
            translation_area = Some(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(GAP / 2.0))
                    .child(
                        div()
                            .text_size(px(TEXT_SIZE))
                            .child(i18n.tr("recwin-translation-title")),
                    )
                    .child(list),
            );
        }
        let right = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(div().text_size(px(TEXT_SIZE)).child(i18n.tr_with(
                if self.data.table.is_some() {
                    "recwin-table-title"
                } else if self.data.latex.is_some() {
                    "recwin-latex-title"
                } else {
                    "recwin-title"
                },
                &Args::new().arg(1, self.data.boxes.len()),
            )))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Textarea::new(&self.text).h_full()),
            )
            .children(translation_area)
            .child(format_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.0))
                            .text_color(match &self.notice {
                                Some((_, true)) => p.danger,
                                _ => p.ok,
                            })
                            .child(self.notice.clone().map(|(t, _)| t).unwrap_or_default()),
                    )
                    .child(copy_all)
                    .child(close),
            );

        div()
            .size_full()
            .flex()
            .gap(px(PADDING))
            .p(px(PADDING))
            .overflow_hidden()
            .bg(p.bg)
            .text_color(p.text)
            .child(left)
            .child(right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表格结果与转换引导随数据带入窗口。
    #[test]
    fn data_carries_table_and_guide() {
        let data = RecognitionData {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            text: "a	b".into(),
            boxes: Vec::new(),
            table: None,
            latex: None,
            conversion: ConversionGuide::NotConfigured,
            translation: None,
        };
        assert!(data.translation.is_none());
        assert!(data.table.is_none());
        assert!(data.latex.is_none());
        assert_eq!(data.conversion, ConversionGuide::NotConfigured);
    }

    /// 窗口必须置顶，覆盖窗存在时才看得见。
    #[test]
    fn window_spec_is_topmost() {
        let spec = window_spec("t".into());
        assert!(spec.always_on_top && spec.decorations);
    }

    /// RGBA 转 GPUI 渲染图：尺寸对得上才有图。
    #[test]
    fn render_image_requires_matching_size() {
        assert!(render_image(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]).is_some());
        assert!(render_image(2, 2, &[0; 8]).is_none());
    }
}
