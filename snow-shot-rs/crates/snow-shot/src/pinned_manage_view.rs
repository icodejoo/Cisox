//! 贴图管理窗口：按分组筛选的贴图列表（缩略图 / 分组 / 状态 / 尺寸 / 时间），
//! 支持显示、删除、删除全部、新建分组、删除空分组与删除当前筛选的分组。
//!
//! 列表数据与筛选逻辑在 [`crate::pinned_manage`]（不依赖 GPUI）。缩略图只为前几十行解码，
//! 在后台线程完成并经收件箱逐张回到主线程；需要改动窗口的动作（显示 / 删除）都交给主线程的管理器执行。

use crate::app_runtime::UiEvent;
use crate::history_store::{Thumbnail, format_local, local_offset_secs, make_thumbnail};
use crate::history_view::{ThumbCache, apply_theme};
use crate::model_catalog::format_size_or_zero;
use crate::pinned_manage::{GroupFilter, PinRow, build_rows, valid_filter};
use crate::pinned_shared::PinShared;
use crate::settings_state::UiPrefs;
use crate::settings_view::{Palette, palette};
use snow_history::pinned::DEFAULT_GROUP_ID;
use snow_history::timeutil::format_iso_utc_ms;
use snow_i18n::Args;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::input::{InputEvent, Textarea, TextareaState};
use snow_ui::ui::component::{Disableable, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use snow_ui::widgets::Popconfirm;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

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
    thumbs: ThumbCache,
    /// 底部提示（错误）。
    notice: NoticeBar,
    /// 新分组名输入框。
    name_input: Entity<TextareaState>,
    /// 本地时区偏移（秒）。
    offset_secs: i64,
    /// 列表滚动句柄。
    scroll: UniformListScrollHandle,
}

/// 底部失败提示：只保留最近一条，新操作开始或切换分组时清除。
#[derive(Debug, Default, PartialEq, Eq)]
struct NoticeBar {
    /// 当前提示文字；`None` 表示没有。
    text: Option<String>,
}

impl NoticeBar {
    /// 显示一条提示（覆盖旧的）。
    fn set(&mut self, text: String) {
        self.text = Some(text);
    }

    /// 清除提示。
    fn clear(&mut self) {
        self.text = None;
    }

    /// 当前提示文字。
    fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
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
        apply_theme(prefs.dark, app);
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
                thumbs: ThumbCache::default(),
                notice: NoticeBar::default(),
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
        let in_list = self.rows.iter().any(|r| r.id == id);
        if self.thumbs.accept(id, thumb, in_list) {
            cx.notify();
        }
    }

    /// 显示一条失败提示。
    ///
    /// # 参数
    /// - `error`：失败原因。
    pub fn show_error(&mut self, error: String, cx: &mut Context<Self>) {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        self.notice
            .set(i18n.tr_with("pinmgr-notice-failed", &Args::new().arg(1, error)));
        cx.notify();
    }

    /// 行变化后：丢弃已不在列表里的缩略图并补请求缺失的。
    fn after_rows_change(&mut self) {
        self.thumbs.retain(self.rows.iter().map(|r| r.id.as_str()));
        self.request_thumbs();
    }

    /// 为前若干行缺失缩略图的贴图启动后台解码（源图字节在主线程读，解码缩放在后台）。
    fn request_thumbs(&mut self) {
        let jobs: Vec<(String, Vec<u8>)> = self
            .rows
            .iter()
            .take(THUMB_BATCH)
            .filter(|r| self.thumbs.is_missing(&r.id))
            .filter_map(|r| self.shared.source_png(&r.id).map(|png| (r.id.clone(), png)))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.thumbs
            .mark_pending(jobs.iter().map(|(id, _)| id.clone()));
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

    /// 发起一次操作：先清掉上一次的失败提示，再交给主线程处理（结果出错会重新显示）。
    fn dispatch(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        self.notice.clear();
        self.inbox.push(event);
        cx.notify();
    }

    /// 切换分组筛选。
    fn set_filter(&mut self, filter: GroupFilter, cx: &mut Context<Self>) {
        self.notice.clear();
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
        self.dispatch(UiEvent::PinGroupCreateNamed { name }, cx);
    }

    /// 缩略图或占位文字，装进定宽定高的框。
    fn thumb_box(&self, id: &str, p: &Palette, i18n: &snow_i18n::I18n) -> Div {
        self.thumbs.thumb_box(
            id,
            ROW_THUMB_W,
            ROW_THUMB_H,
            p,
            i18n.tr("pinmgr-thumb-loading"),
        )
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
                .arg(3, format_size_or_zero(row.payload_bytes)),
        );
        let show_id = row.id.clone();
        let delete_id = row.id.clone();
        let entity = cx.entity();
        let delete = delete_confirm(
            ("pinmgr-delete-pop", ix),
            ("pinmgr-delete", ix),
            [
                i18n.tr("pinmgr-delete-title"),
                i18n.tr("pinmgr-delete-desc"),
                i18n.tr("pinmgr-action-delete"),
            ],
            i18n,
        )
        .on_confirm(move |_window, app| {
            let id = delete_id.clone();
            entity.update(app, |this, cx| {
                this.dispatch(UiEvent::PinManageDelete { id }, cx)
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
                            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                                this.dispatch(
                                    UiEvent::PinManageShow {
                                        id: show_id.clone(),
                                    },
                                    cx,
                                );
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
        let delete_all = delete_confirm(
            "pinmgr-delete-all-pop",
            "pinmgr-delete-all",
            [
                i18n.tr("pinmgr-delete-all-title"),
                i18n.tr("pinmgr-delete-all-desc"),
                i18n.tr("pinmgr-action-delete-all"),
            ],
            i18n,
        )
        .on_confirm({
            let entity = entity.clone();
            move |_window, app| {
                entity.update(app, |this, cx| {
                    this.dispatch(UiEvent::PinManageDeleteAll, cx)
                });
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
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                        this.dispatch(UiEvent::PinGroupDeleteEmpty, cx);
                    })),
            );
        if let GroupFilter::Group(group_id) = &self.filter
            && group_id != DEFAULT_GROUP_ID
        {
            let group_id = group_id.clone();
            let entity = entity.clone();
            actions = actions.child(
                delete_confirm(
                    "pinmgr-delete-group-pop",
                    "pinmgr-delete-group",
                    [
                        i18n.tr("pinmgr-delete-group-title"),
                        i18n.tr("pinmgr-delete-group-desc"),
                        i18n.tr("pinmgr-action-delete-group"),
                    ],
                    i18n,
                )
                .on_confirm(move |_window, app| {
                    let id = group_id.clone();
                    entity.update(app, |this, cx| {
                        this.dispatch(UiEvent::PinGroupDelete { id }, cx)
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

/// 带确认气泡的删除按钮。
///
/// # 参数
/// - `pop_id` / `button_id`：气泡与触发按钮的元素 ID。
/// - `texts`：`[标题, 说明, 按钮文字]`（已本地化）。
/// - `i18n`：取「确认 / 取消」文案。
///
/// # 返回
/// 还没挂确认回调的气泡，调用处接 `.on_confirm(..)`。
fn delete_confirm(
    pop_id: impl Into<ElementId>,
    button_id: impl Into<ElementId>,
    texts: [String; 3],
    i18n: &snow_i18n::I18n,
) -> Popconfirm {
    let [title, description, action] = texts;
    Popconfirm::new(pop_id, title)
        .description(description)
        .ok_text(i18n.tr("pinmgr-confirm-delete"))
        .cancel_text(i18n.tr("pinmgr-confirm-cancel"))
        .ok_danger(true)
        .trigger_button(
            Button::new(button_id)
                .with_size(ComponentSize::Small)
                .label(action),
        )
}

impl Render for PinManageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.thumbs.release(window, "释放贴图缩略图失败");
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
                    range
                        .map(|ix| this.render_row(ix, &this.rows[ix], &p, cx))
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
                    .child(self.notice.text().unwrap_or_default().to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 新提示覆盖旧提示；清除后为空，重复清除无副作用。
    #[test]
    fn notice_set_replace_clear() {
        let mut bar = NoticeBar::default();
        assert_eq!(bar.text(), None);
        bar.set("a".into());
        bar.set("b".into());
        assert_eq!(bar.text(), Some("b"));
        bar.clear();
        bar.clear();
        assert_eq!(bar.text(), None);
    }
}
