//! 主窗口视图：左侧导航侧栏 + 右侧页面内容。
//!
//! 导航与状态在 [`crate::main_window_model`]（不依赖 GPUI）。页面内容目前是现成窗口的入口
//! （设置 / 历史 / 贴图管理 / 输入框翻译）、关于页和“暂未提供”占位；窗口关闭即释放，不常驻。

use crate::app_runtime::UiEvent;
use crate::main_window_model::{
    MainPage, MainWindowModel, NavItem, OpenTarget, PageContent, SIDEBAR_COLLAPSED_KEY,
    TRANSLATION_PAGE_ENABLED_KEY,
};
use crate::settings_state::{SharedConfig, UiPrefs};
use crate::settings_view::{Palette, palette};
use snow_app_core::PRODUCT_NAME;
use snow_i18n::Args;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::{Selectable, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;

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
/// 折叠态里页面项显示的字数（取标题前几个字符作缩写）。
const COLLAPSED_CHARS: usize = 2;

/// 主窗口视图。
pub struct MainWindowView {
    /// 导航状态。
    model: MainWindowModel,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 主线程收件箱（入口按钮与折叠落盘都经它回到主线程）。
    inbox: MainThreadInbox<UiEvent>,
}

impl MainWindowView {
    /// 创建视图。
    ///
    /// # 参数
    /// - `app`：应用上下文（用来同步组件库主题）。
    /// - `config`：共享配置（读取折叠状态与翻译页开关）。
    /// - `prefs`：界面偏好。
    /// - `inbox`：主线程收件箱。
    pub fn create(
        app: &mut App,
        config: &SharedConfig,
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
        let flag = |key: &str| config.borrow().value(key).as_bool().unwrap_or(false);
        let model = MainWindowModel::new(
            flag(SIDEBAR_COLLAPSED_KEY),
            flag(TRANSLATION_PAGE_ENABLED_KEY),
        );
        app.new(|_| Self {
            model,
            prefs,
            inbox,
        })
    }

    /// 翻译页开关变化（设置里改了）。
    ///
    /// # 参数
    /// - `enabled`：新的开关值。
    pub fn set_translation_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.model.set_translation_enabled(enabled);
        cx.notify();
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

    /// 渲染侧栏。
    fn render_sidebar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
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
            let (id, label, active, indent, click): (String, String, bool, bool, NavClick) =
                match item {
                    NavItem::Page { page, child } => (
                        format!("main-nav-{ix}"),
                        i18n.tr(page.title_id()),
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
                        false,
                        false,
                        NavClick::Group,
                    ),
                };
            let shown = if collapsed {
                label.chars().take(COLLAPSED_CHARS).collect()
            } else {
                label
            };
            let button = Button::new(SharedString::from(id))
                .with_size(ComponentSize::Small)
                .selected(active)
                .label(shown)
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    this.on_nav(click, cx);
                }));
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
        bar.child(div().flex_1()).child(
            Button::new("main-nav-toggle")
                .with_size(ComponentSize::Small)
                .label(if collapsed {
                    ">>".to_string()
                } else {
                    toggle_label
                })
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    let collapsed = this.model.toggle_collapsed();
                    this.inbox
                        .push(UiEvent::MainWindowSidebarCollapsed(collapsed));
                    cx.notify();
                })),
        )
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

    /// 渲染右侧页面内容。
    fn render_content(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.prefs.locale);
        let page = self.model.current();
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
            PageContent::Open(target) => col = col.child(open_button(target, cx)),
            PageContent::About => {
                col =
                    col.child(div().text_size(px(TEXT_SIZE)).child(PRODUCT_NAME))
                        .child(div().text_size(px(TEXT_SIZE)).text_color(p.dim).child(
                            i18n.tr_with(
                                "main-about-version",
                                &Args::new().arg(1, env!("CARGO_PKG_VERSION")),
                            ),
                        ));
            }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(self.prefs.dark, self.prefs.accent);
        div()
            .size_full()
            .flex()
            .bg(p.bg)
            .text_color(p.text)
            .child(self.render_sidebar(&p, cx))
            .child(self.render_content(&p, cx))
    }
}
