//! 贴图管理窗口：按分组筛选的贴图列表（缩略图 / 分组 / 状态 / 尺寸 / 时间），
//! 支持显示、删除、删除全部、新建分组、删除空分组与删除当前筛选的分组。
//!
//! 列表数据与筛选逻辑在 [`crate::pinned_manage`]（不依赖 GPUI）。缩略图只为前几十行解码，
//! 在后台线程完成并经收件箱逐张回到主线程；需要改动窗口的动作（显示 / 删除）都交给主线程的管理器执行。

use crate::app_runtime::UiEvent;
use crate::history_store::{Thumbnail, format_local, local_offset_secs, make_thumbnail};
use crate::pinned_manage::{GroupFilter, PinRow, build_rows, default_group_id, valid_filter};
use crate::pinned_shared::PinShared;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use crate::stt_settings::format_size;
use image::{Frame, RgbaImage};
use snow_history::timeutil::format_iso_utc_ms;
use snow_i18n::Args;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::{Disableable, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;
use snow_ui::widgets::Popconfirm;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// 窗口逻辑宽度。
pub const WINDOW_WIDTH: f32 = 860.0;
/// 窗口逻辑高度。
pub const WINDOW_HEIGHT: f32 = 620.0;
/// 缩略图长边上限（像素）。
const THUMB_MAX_EDGE: u32 = 240;
/// 一次最多为多少行请求缩略图。
const THUMB_BATCH: usize = 60;
/// 列表行高（虚拟滚动要求定高）。
const ROW_HEIGHT: f32 = 92.0;
/// 行内缩略图框宽。
const ROW_THUMB_W: f32 = 120.0;
/// 行内缩略图框高。
const ROW_THUMB_H: f32 = 76.0;
/// 内边距。
const PADDING: f32 = 12.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;
/// 次要文字字号。
const SMALL_SIZE: f32 = 12.0;
/// 新分组名输入框宽度。
const NAME_INPUT_WIDTH: f32 = 180.0;
/// 缩略图后台线程名称。
const THUMB_THREAD_NAME: &str = "snow-pin-thumbs";

/// 缩略图加载状态。
enum ThumbState {
    /// 已就绪。
    Ready(Arc<RenderImage>),
    /// 解码失败。
    Failed,
}

/// 贴图管理窗口视图。
pub struct PinManageView {
    /// 贴图共享上下文（只在主线程读仓储）。
    shared: Rc<PinShared>,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
    /// 当前分组筛选。
    filter: GroupFilter,
    /// 当前筛选下的行。
    rows: Vec<PinRow>,
    /// 当前有窗口的贴图 ID。
    open_ids: BTreeSet<String>,
    /// 缩略图。
    thumbs: HashMap<String, ThumbState>,
    /// 正在加载缩略图的贴图 ID。
    pending: HashSet<String>,
    /// 待释放的图像资源。
    pending_drops: Vec<Arc<RenderImage>>,
    /// 底部提示（错误）。
    notice: Option<String>,
    /// 新分组名输入框。
    name_input: Entity<TextareaState>,
    /// 本地时区偏移（秒）。
    offset_secs: i64,
    /// 列表滚动句柄。
    scroll: UniformListScrollHandle,
}

impl PinManageView {
    /// 创建视图并开始加载缩略图。
    ///
    /// # 参数
    /// - `window` / `app`：窗口与应用上下文。
    /// - `shared`：贴图共享上下文。
    /// - `open_ids`：当前有窗口的贴图 ID。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    pub fn create(
        window: &mut Window,
        app: &mut App,
        shared: Rc<PinShared>,
        open_ids: BTreeSet<String>,
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
        let placeholder =
            crate::ocr_backend::i18n_for(prefs.locale).tr("pinmgr-new-group-placeholder");
        let name_input = app.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 1)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let view = app.new(|cx| {
            cx.subscribe_in(
                &name_input,
                window,
                |this: &mut Self, _state, event: &InputEvent, _window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.create_group(cx);
                    }
                },
            )
            .detach();
            let rows = build_rows(&shared, &open_ids, &GroupFilter::All);
            Self {
                shared,
                prefs,
                inbox,
                filter: GroupFilter::All,
                rows,
                open_ids,
                thumbs: HashMap::new(),
                pending: HashSet::new(),
                pending_drops: Vec::new(),
                notice: None,
                name_input: name_input.clone(),
                offset_secs: local_offset_secs(),
                scroll: UniformListScrollHandle::new(),
            }
        });
        view.update(app, |this, _| this.request_thumbs());
        view
    }

    /// 外部变化（贴图增删、分组变化、窗口开关）后重新整理列表。
    ///
    /// # 参数
    /// - `open_ids`：当前有窗口的贴图 ID。
    pub fn refresh(&mut self, open_ids: BTreeSet<String>, cx: &mut Context<Self>) {
        self.open_ids = open_ids;
        self.filter = valid_filter(std::mem::take(&mut self.filter), &self.shared);
        self.rows = build_rows(&self.shared, &self.open_ids, &self.filter);
        self.after_rows_change();
        cx.notify();
    }

    /// 收到一张后台生成的缩略图；不在列表里的直接丢弃。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    /// - `thumb`：缩略图；`None` 表示解码失败。
    pub fn set_thumb(&mut self, id: &str, thumb: Option<Thumbnail>, cx: &mut Context<Self>) {
        self.pending.remove(id);
        if !self.rows.iter().any(|r| r.id == id) {
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

    /// 显示一条失败提示。
    ///
    /// # 参数
    /// - `error`：失败原因。
    pub fn show_error(&mut self, error: String, cx: &mut Context<Self>) {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        self.notice = Some(i18n.tr_with("pinmgr-notice-failed", &Args::new().arg(1, error)));
        cx.notify();
    }

    /// 行变化后：丢弃已不在列表里的缩略图并补请求缺失的。
    fn after_rows_change(&mut self) {
        let ids: HashSet<&str> = self.rows.iter().map(|r| r.id.as_str()).collect();
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

    /// 为前若干行缺失缩略图的贴图启动后台解码（源图字节在主线程读，解码缩放在后台）。
    fn request_thumbs(&mut self) {
        let jobs: Vec<(String, Vec<u8>)> = self
            .rows
            .iter()
            .take(THUMB_BATCH)
            .filter(|r| !self.thumbs.contains_key(&r.id) && !self.pending.contains(&r.id))
            .filter_map(|r| self.shared.source_png(&r.id).map(|png| (r.id.clone(), png)))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.pending.extend(jobs.iter().map(|(id, _)| id.clone()));
        let inbox = self.inbox.clone();
        let spawned = std::thread::Builder::new()
            .name(THUMB_THREAD_NAME.into())
            .spawn(move || {
                for (id, png) in jobs {
                    let thumb = make_thumbnail(&png, THUMB_MAX_EDGE).ok();
                    inbox.push(UiEvent::PinManageThumb { id, thumb });
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "启动贴图缩略图线程失败");
        }
    }

    /// 切换分组筛选。
    fn set_filter(&mut self, filter: GroupFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.rows = build_rows(&self.shared, &self.open_ids, &self.filter);
        self.after_rows_change();
        cx.notify();
    }

    /// 按输入框里的名字新建分组（交给主线程处理，以便同步刷新托盘）。
    fn create_group(&mut self, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.inbox.push(UiEvent::PinGroupCreateNamed { name });
    }

    /// 缩略图或占位文字，装进定宽定高的框。
    fn thumb_box(&self, id: &str, p: &Palette, i18n: &snow_i18n::I18n) -> Div {
        let frame = div()
            .w(px(ROW_THUMB_W))
            .h(px(ROW_THUMB_H))
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
                    .w(px(ROW_THUMB_W))
                    .h(px(ROW_THUMB_H))
                    .object_fit(ObjectFit::Contain),
            ),
            Some(ThumbState::Failed) => frame.child("-"),
            None => frame.child(i18n.tr("pinmgr-thumb-loading")),
        }
    }

    /// 渲染一行。
    fn render_row(
        &self,
        ix: usize,
        row: &PinRow,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let group = row
            .group_name
            .clone()
            .unwrap_or_else(|| i18n.tr("pinmgr-group-default"));
        let status = i18n.tr(if row.open {
            "pinmgr-status-open"
        } else {
            "pinmgr-status-stored"
        });
        let time = format_local(&format_iso_utc_ms(row.created_ms), self.offset_secs);
        let size = i18n.tr_with(
            "pinmgr-size",
            &Args::new()
                .arg(1, row.width)
                .arg(2, row.height)
                .arg(3, format_size(row.payload_bytes)),
        );
        let show_id = row.id.clone();
        let delete_id = row.id.clone();
        let entity = cx.entity();
        let delete = Popconfirm::new(("pinmgr-delete-pop", ix), i18n.tr("pinmgr-delete-title"))
            .description(i18n.tr("pinmgr-delete-desc"))
            .ok_text(i18n.tr("pinmgr-confirm-delete"))
            .cancel_text(i18n.tr("pinmgr-confirm-cancel"))
            .ok_danger(true)
            .trigger_button(
                Button::new(("pinmgr-delete", ix))
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("pinmgr-action-delete")),
            )
            .on_confirm(move |_window, app| {
                let id = delete_id.clone();
                entity.update(app, |this, _| {
                    this.inbox.push(UiEvent::PinManageDelete { id })
                });
            });
        div()
            .id(("pinmgr-row", ix))
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(PADDING))
            .px(px(PADDING))
            .border_b_1()
            .border_color(p.border)
            .child(self.thumb_box(&row.id, p, i18n))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(TEXT_SIZE)).child(format!(
                        "{}  ·  {status}",
                        i18n.tr_with("pinmgr-group-prefix", &Args::new().arg(1, group))
                    )))
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
                    .gap(px(4.0))
                    .child(
                        Button::new(("pinmgr-show", ix))
                            .with_size(ComponentSize::Small)
                            .label(i18n.tr("pinmgr-action-show"))
                            .disabled(row.open)
                            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, _cx| {
                                this.inbox.push(UiEvent::PinManageShow {
                                    id: show_id.clone(),
                                });
                            })),
                    )
                    .child(delete),
            )
            .into_any_element()
    }

    /// 顶部：分组筛选按钮条 + 新建分组 + 删除空分组 + 删除此分组 + 全部删除。
    fn render_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let entity = cx.entity();
        let chip =
            |id: String,
             label: String,
             active: bool,
             filter: GroupFilter,
             cx: &mut Context<Self>| {
                let mut button = Button::new(SharedString::from(format!("pinmgr-chip-{id}")))
                    .with_size(ComponentSize::Small)
                    .label(label);
                if active {
                    button = button.disabled(true);
                }
                button.on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    this.set_filter(filter.clone(), cx)
                }))
            };
        let mut chips = div().flex().flex_wrap().gap(px(4.0)).child(chip(
            "all".into(),
            i18n.tr("pinmgr-filter-all"),
            self.filter == GroupFilter::All,
            GroupFilter::All,
            cx,
        ));
        for group in self.shared.groups() {
            let label = if group.built_in {
                i18n.tr("pinmgr-group-default")
            } else {
                group.name.clone()
            };
            let filter = GroupFilter::Group(group.id.clone());
            chips = chips.child(chip(
                group.id.clone(),
                label,
                self.filter == filter,
                filter,
                cx,
            ));
        }
        let delete_all =
            Popconfirm::new("pinmgr-delete-all-pop", i18n.tr("pinmgr-delete-all-title"))
                .description(i18n.tr("pinmgr-delete-all-desc"))
                .ok_text(i18n.tr("pinmgr-confirm-delete"))
                .cancel_text(i18n.tr("pinmgr-confirm-cancel"))
                .ok_danger(true)
                .trigger_button(
                    Button::new("pinmgr-delete-all")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("pinmgr-action-delete-all")),
                )
                .on_confirm({
                    let entity = entity.clone();
                    move |_window, app| {
                        entity.update(app, |this, _| this.inbox.push(UiEvent::PinManageDeleteAll));
                    }
                });
        let mut actions = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(
                div()
                    .w(px(NAME_INPUT_WIDTH))
                    .child(Textarea::new(&self.name_input)),
            )
            .child(
                Button::new("pinmgr-new-group")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("pinmgr-action-new-group"))
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| this.create_group(cx))),
            )
            .child(
                Button::new("pinmgr-delete-empty")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("pinmgr-action-delete-empty"))
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, _cx| {
                        this.inbox.push(UiEvent::PinGroupDeleteEmpty);
                    })),
            );
        if let GroupFilter::Group(group_id) = &self.filter
            && group_id != default_group_id()
        {
            let group_id = group_id.clone();
            let entity = entity.clone();
            actions = actions.child(
                Popconfirm::new(
                    "pinmgr-delete-group-pop",
                    i18n.tr("pinmgr-delete-group-title"),
                )
                .description(i18n.tr("pinmgr-delete-group-desc"))
                .ok_text(i18n.tr("pinmgr-confirm-delete"))
                .cancel_text(i18n.tr("pinmgr-confirm-cancel"))
                .ok_danger(true)
                .trigger_button(
                    Button::new("pinmgr-delete-group")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("pinmgr-action-delete-group")),
                )
                .on_confirm(move |_window, app| {
                    let id = group_id.clone();
                    entity.update(app, |this, _| {
                        this.inbox.push(UiEvent::PinGroupDelete { id })
                    });
                }),
            );
        }
        if !self.rows.is_empty() || self.filter == GroupFilter::All {
            actions = actions.child(delete_all);
        }
        div()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .p(px(PADDING))
            .border_b_1()
            .border_color(p.border)
            .child(chips)
            .child(actions)
    }

    /// 空状态：标题 + 引导文案。
    fn render_empty(&self, p: &Palette) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
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
                    .child(i18n.tr("pinmgr-empty-title")),
            )
            .child(
                div()
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(i18n.tr("pinmgr-empty-hint")),
            )
    }
}

impl Render for PinManageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.pending_drops.drain(..) {
            if let Err(e) = window.drop_image(image) {
                tracing::debug!(error = %e, "释放贴图缩略图失败");
            }
        }
        let p = palette(self.prefs.dark, self.prefs.accent);
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let total = self.rows.len();
        let toolbar = self.render_toolbar(&p, cx);
        let body = if total == 0 {
            self.render_empty(&p).into_any_element()
        } else {
            uniform_list(
                "pinmgr-rows",
                total,
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    let rows: Vec<(usize, PinRow)> =
                        range.map(|ix| (ix, this.rows[ix].clone())).collect();
                    rows.iter()
                        .map(|(ix, row)| this.render_row(*ix, row, &p, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        };
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
                    .text_color(p.danger)
                    .child(self.notice.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .text_size(px(SMALL_SIZE))
                    .text_color(p.dim)
                    .child(i18n.tr_with("pinmgr-count", &Args::new().arg(1, total))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.bg)
            .text_color(p.text)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(body),
            )
            .child(footer)
    }
}
