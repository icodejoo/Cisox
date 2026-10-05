//! 截图历史窗口：分页列表（缩略图 / 时间 / 尺寸）+ 右侧预览 + 复制 / 贴图 / 定位 / 删除 / 清空。
//!
//! 列表逻辑在 [`HistoryModel`]（不依赖 GPUI，可离屏单测）；视图只负责把它画出来。
//! 缩略图只为当前页解码，且在后台线程完成，经收件箱逐张回到主线程；翻页或刷新时丢弃不在当前页的缩略图。

use crate::app_runtime::UiEvent;
use crate::history_store::{
    HistoryPage, HistorySource, HistoryStore, Thumbnail, decode_rgba, format_local,
    local_offset_secs, make_thumbnail, record_size,
};
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use image::{Frame, RgbaImage};
use snow_history::index::Record;
use snow_i18n::Args;
use snow_platform::clipboard::copy_image_to_clipboard;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::{Disableable, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;
use snow_ui::widgets::Popconfirm;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// 窗口逻辑宽度。
pub const WINDOW_WIDTH: f32 = 920.0;
/// 窗口逻辑高度。
pub const WINDOW_HEIGHT: f32 = 640.0;
/// 每页条数。
pub const PAGE_SIZE: usize = 20;
/// 缩略图长边上限（像素）。
const THUMB_MAX_EDGE: u32 = 320;
/// 列表行高（虚拟滚动要求定高）。
const ROW_HEIGHT: f32 = 104.0;
/// 行内缩略图框宽。
const ROW_THUMB_W: f32 = 140.0;
/// 行内缩略图框高。
const ROW_THUMB_H: f32 = 88.0;
/// 预览栏宽。
const PREVIEW_WIDTH: f32 = 300.0;
/// 预览图框高。
const PREVIEW_IMAGE_H: f32 = 240.0;
/// 内边距。
const PADDING: f32 = 12.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;
/// 次要文字字号。
const SMALL_SIZE: f32 = 12.0;
/// 缩略图后台线程名称。
const THUMB_THREAD_NAME: &str = "snow-history-thumbs";

/// 历史窗口里需要回报结果的异步动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAction {
    /// 复制到剪贴板。
    Copy,
    /// 再次贴图。
    Pin,
}

/// 历史列表模型：当前页、选中项，以及删除 / 清空后的刷新规则。
pub struct HistoryModel {
    /// 存取入口。
    store: HistoryStore,
    /// 每页条数。
    page_size: usize,
    /// 当前页数据。
    current: HistoryPage,
    /// 选中的记录 ID。
    selected: Option<String>,
}

impl HistoryModel {
    /// 创建模型并加载第一页（默认选中首条）。
    ///
    /// # 参数
    /// - `store`：存取入口。
    /// - `page_size`：每页条数。
    ///
    /// ```ignore
    /// let model = HistoryModel::new(store, 20);
    /// ```
    pub fn new(store: HistoryStore, page_size: usize) -> Self {
        let current = store.list_page(0, page_size);
        let mut model = Self {
            store,
            page_size,
            current,
            selected: None,
        };
        model.fix_selection();
        model
    }

    /// 存取入口（后台线程克隆使用）。
    pub fn store(&self) -> &HistoryStore {
        &self.store
    }

    /// 当前页记录。
    pub fn records(&self) -> &[Record] {
        &self.current.records
    }

    /// 总条数。
    pub fn total(&self) -> usize {
        self.current.total
    }

    /// 当前页码（从 0 起）。
    pub fn page(&self) -> usize {
        self.current.page
    }

    /// 总页数。
    pub fn page_count(&self) -> usize {
        self.current.page_count
    }

    /// 选中的记录。
    pub fn selected(&self) -> Option<&Record> {
        let id = self.selected.as_deref()?;
        self.current.records.iter().find(|r| r.id == id)
    }

    /// 选中指定记录；不在当前页则忽略。
    pub fn select(&mut self, id: &str) {
        if self.current.records.iter().any(|r| r.id == id) {
            self.selected = Some(id.to_string());
        }
    }

    /// 重新加载当前页（页码越界自动钳制）。
    pub fn refresh(&mut self) {
        self.current = self.store.list_page(self.current.page, self.page_size);
        self.fix_selection();
    }

    /// 跳到指定页。
    pub fn goto(&mut self, page: usize) {
        self.current = self.store.list_page(page, self.page_size);
        self.fix_selection();
    }

    /// 删除一条后刷新；删光当前页时自动回到上一页。
    ///
    /// # 返回
    /// 失败原因（成功为 `Ok`）。
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.store.remove(id)?;
        self.refresh();
        Ok(())
    }

    /// 清空全部历史并回到第一页。
    pub fn clear(&mut self) -> Result<(), String> {
        self.store.clear()?;
        self.goto(0);
        Ok(())
    }

    /// 选中项失效（被删或换页）时改选首条；空页取消选中。
    fn fix_selection(&mut self) {
        let valid = self
            .selected
            .as_deref()
            .is_some_and(|id| self.current.records.iter().any(|r| r.id == id));
        if !valid {
            self.selected = self.current.records.first().map(|r| r.id.clone());
        }
    }
}

/// 缩略图加载状态。
enum ThumbState {
    /// 已就绪。
    Ready(Arc<RenderImage>),
    /// 解码失败。
    Failed,
}

/// 截图历史窗口视图。
pub struct HistoryView {
    /// 列表模型。
    model: HistoryModel,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 主线程收件箱（后台线程回传用）。
    inbox: MainThreadInbox<UiEvent>,
    /// 当前页缩略图。
    thumbs: HashMap<String, ThumbState>,
    /// 正在加载缩略图的记录 ID。
    pending: HashSet<String>,
    /// 待释放的图像资源（下一帧交还给窗口）。
    pending_drops: Vec<Arc<RenderImage>>,
    /// 底部提示 `(文案, 是否错误)`。
    notice: Option<(String, bool)>,
    /// 历史记录是否开启（关闭时在空状态提示）。
    enabled: bool,
    /// 本地时区偏移（秒）。
    offset_secs: i64,
    /// 列表滚动句柄。
    scroll: UniformListScrollHandle,
    /// 主题是否已应用过。
    themed: bool,
}

impl HistoryView {
    /// 创建视图并开始加载当前页缩略图。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `store`：存取入口。
    /// - `enabled`：历史记录是否开启。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    pub fn create(
        _window: &mut Window,
        app: &mut App,
        store: HistoryStore,
        enabled: bool,
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
        let view = Self {
            model: HistoryModel::new(store, PAGE_SIZE),
            prefs,
            inbox,
            thumbs: HashMap::new(),
            pending: HashSet::new(),
            pending_drops: Vec::new(),
            notice: None,
            enabled,
            offset_secs: local_offset_secs(),
            scroll: UniformListScrollHandle::new(),
            themed: true,
        };
        let entity = app.new(|_| view);
        entity.update(app, |this, _| this.request_thumbs());
        entity
    }

    /// 外部事件通知列表变化（新截图写入等）：重新加载当前页。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.model.refresh();
        self.after_page_change();
        cx.notify();
    }

    /// 收到一张后台生成的缩略图；不在当前页的直接丢弃。
    ///
    /// # 参数
    /// - `id`：记录 ID。
    /// - `thumb`：缩略图；`None` 表示解码失败。
    pub fn set_thumb(&mut self, id: &str, thumb: Option<Thumbnail>, cx: &mut Context<Self>) {
        self.pending.remove(id);
        if !self.model.records().iter().any(|r| r.id == id) {
            return;
        }
        let state = thumb
            .and_then(|t| {
                let buffer = RgbaImage::from_raw(t.width, t.height, t.bgra)?;
                Some(ThumbState::Ready(Arc::new(RenderImage::new(vec![
                    Frame::new(buffer),
                ]))))
            })
            .unwrap_or(ThumbState::Failed);
        if let Some(ThumbState::Ready(old)) = self.thumbs.insert(id.to_string(), state) {
            self.pending_drops.push(old);
        }
        cx.notify();
    }

    /// 异步动作（复制 / 贴图）完成后的提示。
    ///
    /// # 参数
    /// - `action`：动作。
    /// - `error`：失败原因；`None` 表示成功。
    pub fn show_result(
        &mut self,
        action: HistoryAction,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        self.notice = Some(match (action, error) {
            (HistoryAction::Copy, None) => (i18n.tr("history-notice-copied"), false),
            (HistoryAction::Pin, None) => (i18n.tr("history-notice-pinned"), false),
            (HistoryAction::Copy, Some(e)) => (
                i18n.tr_with("history-notice-copy-failed", &Args::new().arg(1, e)),
                true,
            ),
            (HistoryAction::Pin, Some(e)) => (
                i18n.tr_with("history-notice-pin-failed", &Args::new().arg(1, e)),
                true,
            ),
        });
        cx.notify();
    }

    /// 页面数据变化后：丢弃不在当前页的缩略图并补请求缺失的。
    fn after_page_change(&mut self) {
        let ids: HashSet<&str> = self.model.records().iter().map(|r| r.id.as_str()).collect();
        let stale: Vec<String> = self
            .thumbs
            .keys()
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in stale {
            if let Some(ThumbState::Ready(image)) = self.thumbs.remove(&id) {
                self.pending_drops.push(image);
            }
        }
        self.pending.retain(|id| ids.contains(id.as_str()));
        self.request_thumbs();
    }

    /// 为缺失缩略图的记录启动后台解码（逐张经收件箱回传）。
    fn request_thumbs(&mut self) {
        let missing: Vec<Record> = self
            .model
            .records()
            .iter()
            .filter(|r| !self.thumbs.contains_key(&r.id) && !self.pending.contains(&r.id))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        self.pending.extend(missing.iter().map(|r| r.id.clone()));
        let store = self.model.store().clone();
        let inbox = self.inbox.clone();
        let spawned = std::thread::Builder::new()
            .name(THUMB_THREAD_NAME.into())
            .spawn(move || {
                for record in missing {
                    let thumb = store
                        .read_png(&record)
                        .and_then(|png| make_thumbnail(&png, THUMB_MAX_EDGE).ok());
                    inbox.push(UiEvent::HistoryThumb {
                        id: record.id,
                        thumb,
                    });
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "启动历史缩略图线程失败");
        }
    }

    /// 选中一行。
    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.model.select(id);
        cx.notify();
    }

    /// 翻页。
    fn goto(&mut self, page: usize, cx: &mut Context<Self>) {
        self.model.goto(page);
        self.after_page_change();
        cx.notify();
    }

    /// 删除一条。
    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let result = self.model.delete(id);
        self.after_page_change();
        self.finish_mutation(result, "history-notice-delete-failed", cx);
    }

    /// 清空全部。
    fn clear(&mut self, cx: &mut Context<Self>) {
        let result = self.model.clear();
        self.after_page_change();
        self.finish_mutation(result, "history-notice-delete-failed", cx);
    }

    /// 删除 / 清空后的收尾：失败给出提示，成功清掉旧提示。
    fn finish_mutation(
        &mut self,
        result: Result<(), String>,
        fail_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.notice = match result {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!(error = %e, "截图历史删除失败");
                let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
                Some((i18n.tr_with(fail_id, &Args::new().arg(1, e)), true))
            }
        };
        cx.notify();
    }

    /// 再次复制到剪贴板：解码与写剪贴板都在后台线程。
    fn copy(&self, record: &Record) {
        let store = self.model.store().clone();
        let record = record.clone();
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            let result = store
                .read_png(&record)
                .ok_or_else(|| "read failed".to_string())
                .and_then(|png| decode_rgba(&png))
                .and_then(|(w, h, rgba)| copy_image_to_clipboard(w, h, &rgba));
            inbox.push(UiEvent::HistoryActionDone {
                action: HistoryAction::Copy,
                error: result.err(),
            });
        });
    }

    /// 再次贴图：后台解码后交给主线程的贴图管线。
    fn pin(&self, record: &Record) {
        let store = self.model.store().clone();
        let record = record.clone();
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            let decoded = store
                .read_png(&record)
                .ok_or_else(|| "read failed".to_string())
                .and_then(|png| decode_rgba(&png));
            match decoded {
                Ok((width, height, rgba)) => {
                    inbox.push(UiEvent::HistoryPin {
                        width,
                        height,
                        rgba,
                    });
                }
                Err(error) => {
                    inbox.push(UiEvent::HistoryActionDone {
                        action: HistoryAction::Pin,
                        error: Some(error),
                    });
                }
            }
        });
    }

    /// 在资源管理器里定位记录的图片文件。
    fn locate(&mut self, record: &Record, cx: &mut Context<Self>) {
        let result = match self.model.store().png_path(record) {
            Some(path) => reveal_in_explorer(&path),
            None => Err("no image".to_string()),
        };
        if let Err(e) = result {
            let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
            self.notice = Some((
                i18n.tr_with("history-notice-locate-failed", &Args::new().arg(1, e)),
                true,
            ));
            cx.notify();
        }
    }

    /// 缩略图或占位文字，装进定宽定高的框。
    fn thumb_box(
        &self,
        id: &str,
        width: f32,
        height: f32,
        p: &Palette,
        i18n: &snow_i18n::I18n,
    ) -> Div {
        let frame = div()
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(p.control)
            .text_size(px(SMALL_SIZE))
            .text_color(p.dim);
        match self.thumbs.get(id) {
            Some(ThumbState::Ready(image)) => frame.child(
                img(ImageSource::Render(Arc::clone(image)))
                    .w(px(width))
                    .h(px(height))
                    .object_fit(ObjectFit::Contain),
            ),
            Some(ThumbState::Failed) => frame.child("-"),
            None => frame.child(i18n.tr("history-thumb-loading")),
        }
    }

    /// 描述文字三行：来源、本地时间、尺寸。
    fn describe(&self, record: &Record, i18n: &snow_i18n::I18n) -> (String, String, String) {
        let source = HistorySource::parse(&record.source)
            .map(|s| i18n.tr(s.message_id()))
            .unwrap_or_else(|| record.source.clone());
        let time = format_local(&record.created_utc, self.offset_secs);
        let (w, h) = record_size(record);
        let size = i18n.tr_with("history-size", &Args::new().arg(1, w).arg(2, h));
        (source, time, size)
    }

    /// 带确认气泡的删除按钮。
    fn delete_button(
        &self,
        ix: usize,
        id: String,
        i18n: &snow_i18n::I18n,
        cx: &mut Context<Self>,
    ) -> Popconfirm {
        let entity = cx.entity();
        Popconfirm::new(("history-delete-pop", ix), i18n.tr("history-delete-title"))
            .description(i18n.tr("history-delete-desc"))
            .ok_text(i18n.tr("history-confirm-delete"))
            .cancel_text(i18n.tr("history-confirm-cancel"))
            .ok_danger(true)
            .trigger_button(
                Button::new(("history-delete", ix))
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("history-action-delete")),
            )
            .on_confirm(move |_window, app| {
                let id = id.clone();
                entity.update(app, |this, cx| this.delete(&id, cx));
            })
    }

    /// 渲染一行。
    fn render_row(
        &self,
        ix: usize,
        record: &Record,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let selected = self.model.selected().is_some_and(|s| s.id == record.id);
        let (source, time, size) = self.describe(record, i18n);
        let id = record.id.clone();
        let select_id = record.id.clone();
        let copy_record = record.clone();
        let pin_record = record.clone();
        let locate_record = record.clone();
        div()
            .id(("history-row", ix))
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(PADDING))
            .px(px(PADDING))
            .border_b_1()
            .border_color(p.border)
            .when(selected, |d| d.bg(p.control))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| this.select(&select_id, cx)))
            .child(self.thumb_box(&record.id, ROW_THUMB_W, ROW_THUMB_H, p, i18n))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(TEXT_SIZE)).child(source))
                    .child(
                        div()
                            .text_size(px(SMALL_SIZE))
                            .text_color(p.dim)
                            .child(time),
                    )
                    .child(
                        div()
                            .text_size(px(SMALL_SIZE))
                            .text_color(p.dim)
                            .child(size),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .child(
                                Button::new(("history-copy", ix))
                                    .with_size(ComponentSize::Small)
                                    .label(i18n.tr("history-action-copy"))
                                    .on_click(cx.listener(
                                        move |this, _e: &ClickEvent, _w, _cx| {
                                            this.copy(&copy_record)
                                        },
                                    )),
                            )
                            .child(
                                Button::new(("history-pin", ix))
                                    .with_size(ComponentSize::Small)
                                    .label(i18n.tr("history-action-pin"))
                                    .on_click(cx.listener(
                                        move |this, _e: &ClickEvent, _w, _cx| this.pin(&pin_record),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .child(
                                Button::new(("history-locate", ix))
                                    .with_size(ComponentSize::Small)
                                    .label(i18n.tr("history-action-locate"))
                                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                                        this.locate(&locate_record, cx)
                                    })),
                            )
                            .child(self.delete_button(ix, id, i18n, cx)),
                    ),
            )
            .into_any_element()
    }

    /// 右侧预览栏：选中记录的大缩略图与详情。
    fn render_preview(&self, p: &Palette) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let pane = div()
            .w(px(PREVIEW_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .p(px(PADDING))
            .border_l_1()
            .border_color(p.border);
        let Some(record) = self.model.selected() else {
            return pane;
        };
        let (source, time, size) = self.describe(record, i18n);
        pane.child(self.thumb_box(
            &record.id,
            PREVIEW_WIDTH - 2.0 * PADDING,
            PREVIEW_IMAGE_H,
            p,
            i18n,
        ))
        .child(div().text_size(px(TEXT_SIZE)).child(source))
        .child(
            div()
                .text_size(px(SMALL_SIZE))
                .text_color(p.dim)
                .child(time),
        )
        .child(
            div()
                .text_size(px(SMALL_SIZE))
                .text_color(p.dim)
                .child(size),
        )
    }

    /// 空状态：标题 + 引导文案（历史被关闭时给出开启提示）。
    fn render_empty(&self, p: &Palette) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let hint = if self.enabled {
            i18n.tr("history-empty-hint")
        } else {
            i18n.tr("history-disabled-hint")
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(GAP))
            .child(
                div()
                    .text_size(px(TEXT_SIZE + 2.0))
                    .child(i18n.tr("history-empty-title")),
            )
            .child(
                div()
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(hint),
            )
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.pending_drops.drain(..) {
            if let Err(e) = window.drop_image(image) {
                tracing::debug!(error = %e, "释放历史缩略图失败");
            }
        }
        if self.themed {
            self.themed = false;
            Theme::change(
                if self.prefs.dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                },
                None,
                cx,
            );
        }
        let p = palette(self.prefs.dark, self.prefs.accent);
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let total = self.model.total();
        let page = self.model.page();
        let page_count = self.model.page_count();
        let entity = cx.entity();

        let clear = Popconfirm::new("history-clear-pop", i18n.tr("history-clear-title"))
            .description(i18n.tr("history-clear-desc"))
            .ok_text(i18n.tr("history-confirm-delete"))
            .cancel_text(i18n.tr("history-confirm-cancel"))
            .ok_danger(true)
            .trigger_button(
                Button::new("history-clear")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("history-action-clear")),
            )
            .on_confirm(move |_window, app| {
                entity.update(app, |this, cx| this.clear(cx));
            });

        let header = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .p(px(PADDING))
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex_1()
                    .text_size(px(TEXT_SIZE))
                    .child(i18n.tr_with("history-count", &Args::new().arg(1, total))),
            )
            .when(total > 0, |d| d.child(clear));

        let body = if total == 0 {
            self.render_empty(&p).into_any_element()
        } else {
            let list = uniform_list(
                "history-rows",
                self.model.records().len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    let records: Vec<(usize, Record)> = range
                        .map(|ix| (ix, this.model.records()[ix].clone()))
                        .collect();
                    records
                        .iter()
                        .map(|(ix, record)| this.render_row(*ix, record, &p, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1();
            div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(div().flex_1().flex().flex_col().child(list))
                .child(self.render_preview(&p))
                .into_any_element()
        };

        let (notice_text, notice_error) = self.notice.clone().unwrap_or_default();
        let footer = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .p(px(PADDING))
            .border_t_1()
            .border_color(p.border)
            .child(
                div()
                    .flex_1()
                    .text_size(px(SMALL_SIZE))
                    .text_color(if notice_error { p.danger } else { p.ok })
                    .child(notice_text),
            )
            .child(
                Button::new("history-prev")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("history-prev"))
                    .disabled(page == 0)
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.goto(page.saturating_sub(1), cx)
                    })),
            )
            .child(div().text_size(px(SMALL_SIZE)).child(i18n.tr_with(
                "history-page-indicator",
                &Args::new().arg(1, page + 1).arg(2, page_count),
            )))
            .child(
                Button::new("history-next")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("history-next"))
                    .disabled(page + 1 >= page_count)
                    .on_click(
                        cx.listener(move |this, _e: &ClickEvent, _w, cx| this.goto(page + 1, cx)),
                    ),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.bg)
            .text_color(p.text)
            .child(header)
            .child(body)
            .child(footer)
    }
}

/// 在资源管理器中选中文件。
///
/// # 参数
/// - `path`：文件路径。
///
/// # 返回
/// 启动失败的原因。
fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.raw_arg(format!("/select,\"{}\"", path.display()));
    }
    #[cfg(not(windows))]
    command.arg(path.parent().unwrap_or(path));
    command.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_store::{Deduper, process_record};
    use snow_history::capture_history::CaptureHistoryPolicy;
    use snow_history::timeutil::now_utc_ms;
    use std::path::PathBuf;

    /// 建一个临时数据根并写入 `count` 条不同内容的记录。
    fn seeded(tag: &str, count: u8) -> (PathBuf, HistoryStore) {
        let root =
            std::env::temp_dir().join(format!("cisox-history-view-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let policy = CaptureHistoryPolicy::default();
        let mut dedupe = Deduper::default();
        for i in 0..count {
            process_record(
                &root,
                &mut dedupe,
                &policy,
                HistorySource::Copied,
                (2, 2, &[i; 16]),
                now_utc_ms() + i64::from(i) * 1000,
            )
            .unwrap();
        }
        let store = HistoryStore::new(&root, policy);
        (root, store)
    }

    /// 初始选中首条（最新），翻页后改选新页首条。
    #[test]
    fn selection_follows_page() {
        let (root, store) = seeded("sel", 5);
        let mut model = HistoryModel::new(store, 2);
        assert_eq!((model.total(), model.page_count()), (5, 3));
        let first = model.records()[0].id.clone();
        assert_eq!(model.selected().map(|r| r.id.clone()), Some(first));
        model.goto(1);
        assert_eq!(model.page(), 1);
        assert_eq!(
            model.selected().map(|r| r.id.clone()),
            Some(model.records()[0].id.clone())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 删光最后一页的唯一一条后回到上一页，选中项有效。
    #[test]
    fn delete_last_item_steps_back_a_page() {
        let (root, store) = seeded("del", 3);
        let mut model = HistoryModel::new(store, 2);
        model.goto(1);
        assert_eq!(model.records().len(), 1);
        let id = model.records()[0].id.clone();
        model.delete(&id).unwrap();
        assert_eq!((model.total(), model.page(), model.page_count()), (2, 0, 1));
        assert!(model.selected().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 清空后为空页且无选中；之后新写入会出现在刷新后的列表里。
    #[test]
    fn clear_then_refresh_picks_up_new_record() {
        let (root, store) = seeded("clr", 2);
        let mut model = HistoryModel::new(store.clone(), 10);
        model.clear().unwrap();
        assert_eq!(model.total(), 0);
        assert!(model.selected().is_none());
        store
            .record(HistorySource::Saved, 2, 2, &[9; 16], now_utc_ms())
            .unwrap();
        model.refresh();
        assert_eq!(model.total(), 1);
        assert!(model.selected().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 选中不在当前页的 ID 会被忽略。
    #[test]
    fn select_unknown_is_ignored() {
        let (root, store) = seeded("unk", 2);
        let mut model = HistoryModel::new(store, 10);
        let before = model.selected().map(|r| r.id.clone());
        model.select("not-in-page");
        assert_eq!(model.selected().map(|r| r.id.clone()), before);
        let _ = std::fs::remove_dir_all(&root);
    }
}
