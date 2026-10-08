//! 主窗口视图：左侧导航侧栏 + 右侧页面内容。
//!
//! 导航与状态在 [`crate::main_window_model`]（不依赖 GPUI）。设置各页直接内嵌设置页视图
//! （按页切换分组，复用 [`crate::settings_view::SettingsView`]），历史 / 贴图管理 / 翻译页是现成窗口的入口，
//! 另有关于页；折叠态侧栏用 snow-ui-icons 的描边图标。窗口关闭即释放，不常驻。

use crate::app_runtime::UiEvent;
use crate::main_window_model::{
    COLLAPSE_ICON, EXPAND_ICON, MainPage, MainWindowModel, NavItem, OpenTarget, PageContent,
    SETTINGS_GROUP_ICON, SIDEBAR_COLLAPSED_KEY, TRANSLATION_PAGE_ENABLED_KEY, find_license_file,
};
use crate::net_settings::{UpdateAction, UpdateUiState, update_panel};
use crate::settings_state::{SharedConfig, UiPrefs};
use crate::settings_text::group_title;
use crate::settings_view::{Palette, SettingsView, palette};
use crate::translate_page_view::TranslatePageView;
use image::{Frame, RgbaImage};
use snow_app_core::PRODUCT_NAME;
use snow_i18n::Args;
use snow_ui::icons::{IconColors, IconRef, IconRenderer, IconRequest, IconTheme, Rgba as IconRgba};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::{
    Disableable, Selectable, Sizable, Size as ComponentSize, Theme, ThemeMode,
};
use snow_ui::ui::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// 窗口逻辑宽度。
pub const WINDOW_WIDTH: f32 = 900.0;
/// 窗口逻辑高度。
pub const WINDOW_HEIGHT: f32 = 640.0;
/// 展开时侧栏宽度。
const SIDEBAR_WIDTH: f32 = 200.0;
/// 折叠时侧栏宽度。
const SIDEBAR_COLLAPSED_WIDTH: f32 = 56.0;
/// 内边距。
const PADDING: f32 = 16.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 子项缩进。
const CHILD_INDENT: f32 = 12.0;
/// 标题字号。
const TITLE_SIZE: f32 = 20.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;
/// 侧栏图标逻辑边长。
const ICON_SIZE: u32 = 18;
/// 深色界面的图标色。
const ICON_COLOR_DARK: [u8; 3] = [0xE6, 0xE6, 0xE6];
/// 浅色界面的图标色。
const ICON_COLOR_LIGHT: [u8; 3] = [0x26, 0x26, 0x26];
/// 设备像素比放大成整数缓存键时的倍率。
const SCALE_KEY_FACTOR: f32 = 100.0;
/// 窗口位置 / 大小停住多久后才记忆（毫秒），避免拖动时反复写盘。
const GEOMETRY_SAVE_DEBOUNCE_MS: u64 = 400;

/// 创建设置页视图的工厂（由运行时提供，负责接好热键 / 更新 / 导入导出 / 语音模型等回调）。
pub type SettingsFactory = Rc<dyn Fn(&mut Window, &mut App) -> Entity<SettingsView>>;

/// 创建内嵌翻译页视图的工厂（由运行时提供：读翻译配置、已装包、历史路径与自动翻译开关）。
pub type TranslateFactory = Rc<dyn Fn(&mut Window, &mut App) -> Entity<TranslatePageView>>;

/// 把图标光栅化成 GPUI 要的预乘 BGRA 缓冲。
///
/// # 参数
/// - `renderer`：图标光栅化器（带缓存）。
/// - `name`：描边图标名（kebab-case）。
/// - `color`：图标颜色 RGB。
/// - `size`：逻辑边长。
/// - `dpr`：设备像素比。
///
/// # 返回
/// `(物理宽, 物理高, 预乘 BGRA)`；图标不存在或渲染失败返回 `None`。
pub fn icon_bgra(
    renderer: &IconRenderer,
    name: &str,
    color: [u8; 3],
    size: u32,
    dpr: f32,
) -> Option<(u32, u32, Vec<u8>)> {
    let icon = IconRef::new(IconTheme::Outlined, name).with_colors(IconColors::primary(
        IconRgba::rgb(color[0], color[1], color[2]),
    ));
    if !icon.exists() {
        return None;
    }
    let bitmap = renderer.render(&icon, &IconRequest::square(size, dpr))?;
    let mut data = bitmap.data.to_vec();
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some((bitmap.width, bitmap.height, data))
}

/// 图标缓存键：(图标名, 是否深色, 设备像素比 x100)。
type IconKey = (&'static str, bool, u32);

/// 主窗口视图。
pub struct MainWindowView {
    /// 导航状态。
    model: MainWindowModel,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 主线程收件箱（入口按钮与折叠落盘都经它回到主线程）。
    inbox: MainThreadInbox<UiEvent>,
    /// 设置页视图工厂。
    settings_factory: SettingsFactory,
    /// 翻译页视图工厂。
    translate_factory: TranslateFactory,
    /// 内嵌的翻译页视图（首次进入翻译页时创建，之后保留输入与历史直到窗口关闭）。
    translate: Option<Entity<TranslatePageView>>,
    /// 关于页的更新检查状态。
    update_state: UpdateUiState,
    /// 许可证摘要文件（找到才有；关于页据此决定是否显示“查看许可证”）。
    license_file: Option<std::path::PathBuf>,
    /// 窗口几何防抖代数。
    geometry_gen: u64,
    /// 内嵌的设置页视图（首次进入设置页时创建）。
    settings: Option<Entity<SettingsView>>,
    /// 已推给内嵌设置页的分组 id（与模型不一致时下一帧补推）。
    synced_group: Option<&'static str>,
    /// 图标光栅化器。
    renderer: IconRenderer,
    /// 已转成 GPUI 图像的侧栏图标。
    icons: HashMap<IconKey, Arc<RenderImage>>,
}

impl MainWindowView {
    /// 创建视图。
    ///
    /// # 参数
    /// - `window`：窗口（用来监听位置 / 大小变化、恢复最大化）。
    /// - `app`：应用上下文（用来同步组件库主题）。
    /// - `config`：共享配置（读取折叠状态与翻译页开关）。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    /// - `settings_factory`：设置页视图工厂。
    /// - `translate_factory`：翻译页视图工厂。
    /// - `maximized`：是否按上次记忆恢复最大化。
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        window: &mut Window,
        app: &mut App,
        config: &SharedConfig,
        prefs: UiPrefs,
        inbox: MainThreadInbox<UiEvent>,
        settings_factory: SettingsFactory,
        translate_factory: TranslateFactory,
        maximized: bool,
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
        let flag = |key: &str| config.borrow().value(key).as_bool().unwrap_or(false);
        let model = MainWindowModel::new(
            flag(SIDEBAR_COLLAPSED_KEY),
            flag(TRANSLATION_PAGE_ENABLED_KEY),
        );
        if maximized {
            // 等首帧落位后再最大化，避免和创建时的物理落位互相覆盖
            window.on_next_frame(|window, _| window.zoom_window());
        }
        let license_file = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().and_then(find_license_file));
        app.new(|cx| {
            cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
                this.on_bounds_changed(window.is_maximized(), cx)
            })
            .detach();
            Self {
                model,
                prefs,
                inbox,
                settings_factory,
                translate_factory,
                translate: None,
                update_state: UpdateUiState::Idle,
                license_file,
                geometry_gen: 0,
                settings: None,
                synced_group: None,
                renderer: IconRenderer::new(),
                icons: HashMap::new(),
            }
        })
    }

    /// 窗口位置 / 大小变化：停住一小会儿后通知运行时记忆几何。
    ///
    /// # 参数
    /// - `maximized`：此刻是否最大化。
    fn on_bounds_changed(&mut self, maximized: bool, cx: &mut Context<Self>) {
        self.geometry_gen += 1;
        let generation = self.geometry_gen;
        cx.spawn(async move |this, acx| {
            acx.background_executor()
                .timer(Duration::from_millis(GEOMETRY_SAVE_DEBOUNCE_MS))
                .await;
            let _ = this.update(acx, |view, _cx| {
                if view.geometry_gen == generation {
                    view.inbox.push(UiEvent::MainWindowSettled { maximized });
                }
            });
        })
        .detach();
    }

    /// 内嵌的翻译页视图（尚未进入过翻译页时为 `None`），运行时把翻译结果与开关变化同步给它。
    pub fn translate_view(&self) -> Option<Entity<TranslatePageView>> {
        self.translate.clone()
    }

    /// 关于页的更新检查有了新状态（检查结果 / 下载结果）。
    ///
    /// # 参数
    /// - `state`：新的界面状态。
    pub fn finish_update_check(&mut self, state: UpdateUiState, cx: &mut Context<Self>) {
        self.update_state = state;
        cx.notify();
    }

    /// 翻译页开关变化（设置里改了）。
    ///
    /// # 参数
    /// - `enabled`：新的开关值。
    pub fn set_translation_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.model.set_translation_enabled(enabled);
        cx.notify();
    }

    /// 界面偏好变化（语言 / 主题改了）：换文案与图标颜色。
    ///
    /// # 参数
    /// - `prefs`：新的界面偏好。
    pub fn set_prefs(&mut self, prefs: UiPrefs, cx: &mut Context<Self>) {
        if let Some(view) = &self.translate {
            view.update(cx, |v, vcx| v.set_prefs(prefs, vcx));
        }
        self.prefs = prefs;
        cx.notify();
    }

    /// 内嵌的设置页视图（尚未进入过设置页时为 `None`），运行时把设置变更结果同步给它。
    pub fn settings_view(&self) -> Option<Entity<SettingsView>> {
        self.settings.clone()
    }

    /// 导航到某页（外部入口，如“显示关于”）。
    ///
    /// # 参数
    /// - `page`：目标页。
    pub fn select_page(&mut self, page: MainPage, cx: &mut Context<Self>) {
        if self.model.select(page) {
            cx.notify();
        }
    }

    /// 跳到某个设置分组（入口按钮用）：切到所在页并选中该分组。
    ///
    /// # 参数
    /// - `group_id`：设置分组 id，如 `screenshot_translation`。
    ///
    /// # 返回
    /// 分组存在且所在页可见时为 `true`。
    pub fn show_settings_group(&mut self, group_id: &str, cx: &mut Context<Self>) -> bool {
        let jumped = self.model.jump_to_group(group_id);
        if jumped {
            cx.notify();
        }
        jumped
    }

    /// 把入口目标转成主线程事件。
    ///
    /// # 参数
    /// - `target`：要打开的现成窗口。
    pub fn event_for(target: OpenTarget) -> UiEvent {
        match target {
            OpenTarget::Settings => UiEvent::OpenSettings,
            OpenTarget::History => UiEvent::OpenHistory,
            OpenTarget::PinManage => UiEvent::OpenPinManage,
            OpenTarget::TranslatePage => UiEvent::OpenTranslatePage,
        }
    }

    /// 取（必要时光栅化并缓存）侧栏图标。
    ///
    /// # 参数
    /// - `name`：图标名。
    /// - `scale`：窗口缩放比。
    fn icon_image(&mut self, name: &'static str, scale: f32) -> Option<Arc<RenderImage>> {
        let key: IconKey = (
            name,
            self.prefs.dark,
            (scale * SCALE_KEY_FACTOR).round() as u32,
        );
        if let Some(image) = self.icons.get(&key) {
            return Some(Arc::clone(image));
        }
        let color = if self.prefs.dark {
            ICON_COLOR_DARK
        } else {
            ICON_COLOR_LIGHT
        };
        let (w, h, bgra) = icon_bgra(&self.renderer, name, color, ICON_SIZE, scale)?;
        let image = Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::from_raw(
            w, h, bgra,
        )?)]));
        self.icons.insert(key, Arc::clone(&image));
        Some(image)
    }

    /// 渲染侧栏。
    fn render_sidebar(&mut self, p: &Palette, scale: f32, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let collapsed = self.model.collapsed();
        let mut bar = div()
            .flex_none()
            .w(px(if collapsed {
                SIDEBAR_COLLAPSED_WIDTH
            } else {
                SIDEBAR_WIDTH
            }))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(GAP))
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border);
        for (ix, item) in self.model.nav_items().into_iter().enumerate() {
            let (id, label, icon, active, indent, click): (
                String,
                String,
                &'static str,
                bool,
                bool,
                NavClick,
            ) = match item {
                NavItem::Page { page, child } => (
                    format!("main-nav-{ix}"),
                    i18n.tr(page.title_id()),
                    page.icon_name(),
                    self.model.current() == page,
                    child,
                    NavClick::Page(page),
                ),
                NavItem::SettingsGroup { expanded } => (
                    "main-nav-settings-group".to_string(),
                    format!(
                        "{} {}",
                        if expanded { "v" } else { ">" },
                        i18n.tr("main-nav-settings")
                    ),
                    SETTINGS_GROUP_ICON,
                    false,
                    false,
                    NavClick::Group,
                ),
            };
            let mut button = Button::new(SharedString::from(id))
                .with_size(ComponentSize::Small)
                .selected(active)
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    this.on_nav(click, cx);
                }));
            button = match (collapsed, self.icon_image(icon, scale)) {
                (true, Some(image)) => button
                    .tooltip(label)
                    .child(img(ImageSource::Render(image)).size(px(ICON_SIZE as f32))),
                // 图标缺失时退回标题前两个字符，保证折叠态仍可点
                (true, None) => button
                    .tooltip(label.clone())
                    .label(label.chars().take(2).collect::<String>()),
                (false, _) => button.label(label),
            };
            bar = bar.child(
                div()
                    .w_full()
                    .when(indent && !collapsed, |d| d.pl(px(CHILD_INDENT)))
                    .child(button),
            );
        }
        let toggle_label = i18n.tr(if collapsed {
            "main-sidebar-expand"
        } else {
            "main-sidebar-collapse"
        });
        let toggle_icon = if collapsed {
            EXPAND_ICON
        } else {
            COLLAPSE_ICON
        };
        let mut toggle = Button::new("main-nav-toggle")
            .with_size(ComponentSize::Small)
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                let collapsed = this.model.toggle_collapsed();
                this.inbox
                    .push(UiEvent::MainWindowSidebarCollapsed(collapsed));
                cx.notify();
            }));
        toggle = match (collapsed, self.icon_image(toggle_icon, scale)) {
            (true, Some(image)) => toggle
                .tooltip(toggle_label)
                .child(img(ImageSource::Render(image)).size(px(ICON_SIZE as f32))),
            (true, None) => toggle.tooltip(toggle_label).label(">>"),
            (false, _) => toggle.label(toggle_label),
        };
        bar.child(div().flex_1()).child(toggle)
    }

    /// 处理侧栏点击。
    fn on_nav(&mut self, click: NavClick, cx: &mut Context<Self>) {
        match click {
            NavClick::Page(page) => {
                self.model.select(page);
            }
            NavClick::Group => self.model.toggle_settings_group(),
        }
        cx.notify();
    }

    /// 渲染内嵌设置页：顶部分组切换行（含“在独立窗口打开”）+ 设置页视图。
    fn render_settings_page(
        &mut self,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let page = self.model.current();
        if self.settings.is_none() {
            let factory = Rc::clone(&self.settings_factory);
            let view = factory(window, cx);
            view.update(cx, |v, _| v.set_embedded(true));
            self.settings = Some(view);
            self.synced_group = None;
        }
        // 模型里的分组与设置页实际所在不一致时补推一次（换页 / 点分组 / 入口跳转）
        let wanted = self.model.settings_group();
        if let (Some(group), Some(view)) = (wanted, &self.settings)
            && self.synced_group != Some(group)
        {
            view.update(cx, |v, cx| v.show_group_id(group, cx));
            self.synced_group = Some(group);
        }
        let mut row = div()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(GAP))
            .px(px(PADDING))
            .py(px(GAP))
            .border_b_1()
            .border_color(p.border);
        let groups = page.settings_groups();
        if groups.len() > 1 {
            for (ix, id) in groups.iter().copied().enumerate() {
                row = row.child(
                    Button::new(SharedString::from(format!("main-group-{ix}")))
                        .with_size(ComponentSize::Small)
                        .selected(self.model.settings_group() == Some(id))
                        .label(group_title(self.prefs.lang, id))
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                            this.model.select_group(id);
                            cx.notify();
                        })),
                );
            }
        }
        row = row.child(div().flex_1()).child(
            Button::new("main-open-settings-window")
                .with_size(ComponentSize::Small)
                .label(i18n.tr(OpenTarget::Settings.button_id()))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, _cx| {
                    this.inbox.push(Self::event_for(OpenTarget::Settings));
                })),
        );
        let mut col = div().flex_1().min_w_0().flex().flex_col().child(row);
        if let Some(view) = &self.settings {
            col = col.child(div().flex_1().min_h_0().child(view.clone()));
        }
        col
    }

    /// 渲染内嵌翻译页：顶部入口行（独立窗口 / 相关设置）+ 翻译页视图。
    fn render_translate_page(
        &mut self,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let page = self.model.current();
        if self.translate.is_none() {
            let factory = Rc::clone(&self.translate_factory);
            self.translate = Some(factory(window, cx));
        }
        let mut row = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(GAP))
            .px(px(PADDING))
            .py(px(GAP))
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .text_size(px(TITLE_SIZE))
                    .child(i18n.tr(page.title_id())),
            )
            .child(div().flex_1());
        if let Some(group) = page.related_settings_group() {
            row = row.child(
                Button::new("main-goto-settings")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("main-goto-settings"))
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.show_settings_group(group, cx);
                    })),
            );
        }
        row = row.child(
            Button::new("main-open-translate-window")
                .with_size(ComponentSize::Small)
                .label(i18n.tr(OpenTarget::TranslatePage.button_id()))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, _cx| {
                    this.inbox.push(Self::event_for(OpenTarget::TranslatePage));
                })),
        );
        let mut col = div().flex_1().min_w_0().flex().flex_col().child(row);
        if let Some(view) = &self.translate {
            col = col.child(div().flex_1().min_h_0().child(view.clone()));
        }
        col
    }

    /// 渲染关于页：名称与版本、来源与许可证、更新检查。
    ///
    /// # 参数
    /// - `col`：已放好标题与说明的内容列。
    fn render_about(&mut self, col: Div, p: &Palette, cx: &mut Context<Self>) -> Div {
        let locale = self.prefs.locale;
        let i18n = crate::ocr_backend::i18n_for(locale);
        let text = |content: String| {
            div()
                .text_size(px(TEXT_SIZE))
                .text_color(p.dim)
                .child(content)
        };
        let mut col = col
            .child(div().text_size(px(TEXT_SIZE)).child(PRODUCT_NAME))
            .child(text(i18n.tr_with(
                "main-about-version",
                &Args::new().arg(1, env!("CARGO_PKG_VERSION")),
            )))
            .child(text(i18n.tr("main-about-upstream")))
            .child(text(i18n.tr("main-about-license")));
        col = match self.license_file.clone() {
            Some(path) => col.child(
                div().child(
                    Button::new("main-about-license-open")
                        .with_size(ComponentSize::Small)
                        .label(i18n.tr("main-about-license-open"))
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, _cx| {
                            this.inbox.push(UiEvent::OpenFile(path.clone()));
                        })),
                ),
            ),
            None => col.child(text(i18n.tr("main-about-license-missing"))),
        };
        // 更新检查：与设置页“更新”分组共用同一套状态与文案
        let panel = update_panel(locale, &self.update_state);
        let running = matches!(
            self.update_state,
            UpdateUiState::Running | UpdateUiState::Downloading
        );
        let check = Button::new("main-about-update-check")
            .with_size(ComponentSize::Small)
            .label(panel.button_label)
            .disabled(running)
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.update_state = UpdateUiState::Running;
                this.inbox
                    .push(UiEvent::UpdateActionRequested(UpdateAction::Check));
                cx.notify();
            }));
        let extra = match &self.update_state {
            UpdateUiState::Available { info, .. } if !info.url.is_empty() => {
                let info = info.clone();
                Some(
                    Button::new("main-about-update-download")
                        .with_size(ComponentSize::Small)
                        .label(panel.download_label)
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                            this.update_state = UpdateUiState::Downloading;
                            this.inbox.push(UiEvent::UpdateActionRequested(
                                UpdateAction::Download(info.clone()),
                            ));
                            cx.notify();
                        })),
                )
            }
            UpdateUiState::Downloaded { dir, .. } => {
                let dir = dir.clone();
                Some(
                    Button::new("main-about-update-folder")
                        .with_size(ComponentSize::Small)
                        .label(panel.open_folder_label)
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, _cx| {
                            this.inbox.push(UiEvent::UpdateActionRequested(
                                UpdateAction::OpenFolder(dir.clone()),
                            ));
                        })),
                )
            }
            _ => None,
        };
        let notice = panel.notice.map(|(content, danger)| {
            div()
                .text_size(px(TEXT_SIZE))
                .text_color(if danger { p.danger } else { p.dim })
                .child(content)
        });
        col.child(
            div()
                .text_size(px(TEXT_SIZE))
                .font_weight(FontWeight::BOLD)
                .child(panel.title),
        )
        .child(text(panel.current_line))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(check)
                .children(extra)
                .children(notice),
        )
    }

    /// 渲染右侧页面内容。
    fn render_content(&mut self, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let page = self.model.current();
        match page.content() {
            PageContent::Settings => return self.render_settings_page(p, window, cx),
            PageContent::Translate => return self.render_translate_page(p, window, cx),
            _ => {}
        }
        let mut col = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .p(px(PADDING))
            .child(
                div()
                    .text_size(px(TITLE_SIZE))
                    .child(i18n.tr(page.title_id())),
            )
            .child(
                div()
                    .text_size(px(TEXT_SIZE))
                    .text_color(p.dim)
                    .child(i18n.tr(page.desc_id())),
            );
        let open_button = |target: OpenTarget, cx: &mut Context<Self>| {
            Button::new("main-open")
                .with_size(ComponentSize::Small)
                .label(i18n.tr(target.button_id()))
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, _cx| {
                    this.inbox.push(Self::event_for(target));
                }))
        };
        match page.content() {
            // 设置页与翻译页在函数开头已处理
            PageContent::Settings | PageContent::Translate => {}
            PageContent::Open(target) => col = col.child(open_button(target, cx)),
            PageContent::About => col = self.render_about(col, p, cx),
            PageContent::Placeholder(fallback) => {
                col = col.child(
                    div()
                        .text_size(px(TEXT_SIZE))
                        .text_color(p.dim)
                        .child(i18n.tr("main-placeholder")),
                );
                if let Some(target) = fallback {
                    col = col.child(open_button(target, cx));
                }
            }
        }
        // 相关设置分组的入口：点一下跳到对应设置页的该分组
        if let Some(group) = page.related_settings_group() {
            col = col.child(
                Button::new("main-goto-settings")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("main-goto-settings"))
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.show_settings_group(group, cx);
                    })),
            );
        }
        col
    }
}

/// 侧栏点击的去向。
#[derive(Clone, Copy)]
enum NavClick {
    /// 切到某页。
    Page(MainPage),
    /// 展开 / 收起设置分组。
    Group,
}

impl Render for MainWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(self.prefs.dark, self.prefs.accent);
        let scale = window.scale_factor();
        let sidebar = self.render_sidebar(&p, scale, cx);
        let content = self.render_content(&p, window, cx);
        div()
            .size_full()
            .flex()
            .bg(p.bg)
            .text_color(p.text)
            .child(sidebar)
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标光栅化成预乘 BGRA：尺寸随 dpr，红色图标的 R 在第 3 字节、B 为 0，且有不透明像素。
    #[test]
    fn icon_bgra_is_bgra_ordered_and_scaled() {
        let renderer = IconRenderer::new();
        let (w, h, data) = icon_bgra(&renderer, "setting", [255, 0, 0], 18, 2.0).unwrap();
        assert_eq!((w, h), (36, 36));
        assert_eq!(data.len(), (w * h * 4) as usize);
        let opaque: Vec<&[u8]> = data.chunks_exact(4).filter(|px| px[3] > 200).collect();
        assert!(!opaque.is_empty(), "图标应有实心像素");
        assert!(
            opaque
                .iter()
                .all(|px| px[0] == 0 && px[1] == 0 && px[2] > 200)
        );
    }

    /// 不存在的图标返回 `None`，由视图退回文字。
    #[test]
    fn missing_icon_returns_none() {
        let renderer = IconRenderer::new();
        assert!(icon_bgra(&renderer, "no-such-icon-name", [0, 0, 0], 18, 1.0).is_none());
    }
}
