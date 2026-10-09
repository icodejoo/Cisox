//! 设置窗口里「非设置」的页：翻译（内嵌）、截图历史入口、贴图管理入口、关于。
//!
//! 主窗口已并入设置窗口：侧栏在原有设置分组下方追加这几页。导航 / 入口映射 / 单例判断是不依赖
//! GPUI 的纯逻辑（离屏可测）；渲染部分是 [`SettingsView`] 的扩展实现，窗口关闭即整体释放。

use crate::net_settings::{UpdateAction, UpdateUiState, update_panel};
use crate::settings_view::{Palette, SettingsView};
use crate::translate_page_view::TranslatePageView;
use snow_app_core::PRODUCT_NAME;
use snow_i18n::Args;
use snow_ui::ui::component::Sizable;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// 配置键：翻译页是否启用（侧栏里是否显示翻译页）。
pub const TRANSLATION_PAGE_ENABLED_KEY: &str = "extended_features/translation_page_enabled";
/// 许可证摘要文件名（仓库根目录）。
pub const LICENSE_FILE: &str = "LICENSE.md";
/// 查找许可证文件时最多向上找几层目录（开发时可执行文件在 `build/cargo/<profile>/` 下）。
const LICENSE_SEARCH_DEPTH: usize = 6;
/// 窗口位置 / 大小停住多久后才记忆（毫秒），避免拖动时反复写盘。
const GEOMETRY_SAVE_DEBOUNCE_MS: u64 = 400;
/// 内边距。
const PADDING: f32 = 16.0;
/// 控件间距。
const GAP: f32 = 8.0;
/// 标题字号。
const TITLE_SIZE: f32 = 20.0;
/// 正文字号。
const TEXT_SIZE: f32 = 13.0;

/// 创建内嵌翻译页视图的工厂（由运行时提供：读翻译配置、已装包、历史路径与自动翻译开关）。
pub type TranslateFactory = Rc<dyn Fn(&mut Window, &mut App) -> Entity<TranslatePageView>>;

/// 设置窗口交给宿主处理的动作（宿主转成主线程事件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAction {
    /// 打开截图历史窗口。
    OpenHistory,
    /// 打开贴图管理窗口。
    OpenPinManage,
    /// 打开翻译页独立窗口。
    OpenTranslateWindow,
    /// 用系统默认方式打开文件（许可证）。
    OpenFile(PathBuf),
    /// 窗口位置 / 大小已停稳，可以记忆。
    WindowSettled {
        /// 此刻是否最大化。
        maximized: bool,
    },
}

/// 追加在设置分组下方的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtraPage {
    /// 翻译（内嵌翻译页，受开关控制）。
    Translation,
    /// 截图历史入口页。
    History,
    /// 贴图管理入口页。
    PinManage,
    /// 关于。
    About,
}

impl ExtraPage {
    /// 侧栏显示顺序。
    pub const ALL: [ExtraPage; 4] = [
        ExtraPage::Translation,
        ExtraPage::History,
        ExtraPage::PinManage,
        ExtraPage::About,
    ];

    /// 页面标题的本地化 id。
    pub fn title_id(self) -> &'static str {
        match self {
            Self::Translation => "main-page-translation",
            Self::History => "main-page-history",
            Self::PinManage => "main-page-pins",
            Self::About => "main-page-about",
        }
    }

    /// 页面说明的本地化 id。
    pub fn desc_id(self) -> &'static str {
        match self {
            Self::Translation => "main-desc-translation",
            Self::History => "main-desc-history",
            Self::PinManage => "main-desc-pins",
            Self::About => "main-desc-about",
        }
    }

    /// 本页相关的设置分组 id：页面上的「相关设置」按钮跳到它。
    pub fn related_group(self) -> Option<&'static str> {
        match self {
            Self::Translation => Some("screenshot_translation"),
            Self::History => Some("capture_history"),
            Self::PinManage => Some("pin_to_screen"),
            Self::About => None,
        }
    }

    /// 入口页上「打开现成窗口」按钮的动作与文案 id；内嵌 / 无入口的页返回 `None`。
    pub fn open_action(self) -> Option<(PageAction, &'static str)> {
        match self {
            Self::History => Some((PageAction::OpenHistory, "main-open-history")),
            Self::PinManage => Some((PageAction::OpenPinManage, "main-open-pins")),
            Self::Translation | Self::About => None,
        }
    }
}

/// 唤起设置窗口的入口（托盘 / 热键 / IPC / MCP）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEntry {
    /// 「主窗口」类入口：托盘主窗口项、托盘左键、`--cmd show`、MCP `open_main_window`。
    Main,
    /// 「设置」类入口：托盘设置项、`open_settings` 热键、`--cmd settings`、MCP `open_settings`。
    Settings,
}

impl WindowEntry {
    /// 入口要落到的设置分组 id；`None` 表示不改变当前页（新开窗口时落在第一个分组）。
    pub fn target_group(self) -> Option<&'static str> {
        match self {
            Self::Main => None,
            Self::Settings => crate::settings_model::groups().first().map(|g| g.id),
        }
    }
}

/// 唤起窗口时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    /// 窗口已开：置顶激活，不重复创建（单例）。
    Activate,
    /// 窗口未开：新建。
    Create,
}

/// 按窗口当前是否存活决定处理方式。
///
/// # 参数
/// - `is_open`：窗口当前是否仍然打开。
pub fn plan_open(is_open: bool) -> OpenPlan {
    if is_open {
        OpenPlan::Activate
    } else {
        OpenPlan::Create
    }
}

/// 从 `start` 起逐级向上找许可证摘要文件。
///
/// # 参数
/// - `start`：起始目录（通常是可执行文件所在目录）。
///
/// # 返回
/// 找到的文件路径；找不到返回 `None`（关于页就只显示文字说明）。
pub fn find_license_file(start: &std::path::Path) -> Option<PathBuf> {
    start
        .ancestors()
        .take(LICENSE_SEARCH_DEPTH)
        .map(|dir| dir.join(LICENSE_FILE))
        .find(|candidate| candidate.is_file())
}

/// 追加页的导航状态：当前停在哪个追加页（`None` 表示在某个设置分组里）与翻译页开关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageNav {
    /// 当前追加页。
    current: Option<ExtraPage>,
    /// 翻译页是否启用。
    translation_enabled: bool,
}

impl PageNav {
    /// 创建导航状态，初始停在设置分组里。
    ///
    /// # 参数
    /// - `translation_enabled`：翻译页是否启用（来自配置）。
    pub fn new(translation_enabled: bool) -> Self {
        Self {
            current: None,
            translation_enabled,
        }
    }

    /// 当前追加页。
    pub fn current(&self) -> Option<ExtraPage> {
        self.current
    }

    /// 页面当前是否可见（翻译页受开关控制）。
    pub fn page_visible(&self, page: ExtraPage) -> bool {
        page != ExtraPage::Translation || self.translation_enabled
    }

    /// 侧栏要显示的追加页（按顺序）。
    pub fn visible_pages(&self) -> Vec<ExtraPage> {
        ExtraPage::ALL
            .into_iter()
            .filter(|p| self.page_visible(*p))
            .collect()
    }

    /// 切到某个追加页；页面不可见时忽略并返回 `false`。
    ///
    /// # 参数
    /// - `page`：目标页。
    pub fn select(&mut self, page: ExtraPage) -> bool {
        if !self.page_visible(page) {
            return false;
        }
        self.current = Some(page);
        true
    }

    /// 回到设置分组（离开追加页）。
    pub fn leave(&mut self) {
        self.current = None;
    }

    /// 翻译页开关变化；当前页正好是翻译页且被关闭时退回设置分组。
    ///
    /// # 参数
    /// - `enabled`：新的开关值。
    pub fn set_translation_enabled(&mut self, enabled: bool) {
        self.translation_enabled = enabled;
        if !enabled && self.current == Some(ExtraPage::Translation) {
            self.current = None;
        }
    }
}

/// 设置视图里追加页相关的状态（单独成块，避免 [`SettingsView`] 继续膨胀）。
pub struct ExtraState {
    /// 导航状态。
    pub nav: PageNav,
    /// 翻译页视图工厂（未接入时翻译页为空白）。
    pub translate_factory: Option<TranslateFactory>,
    /// 内嵌的翻译页视图（首次进入翻译页时创建，之后保留输入与历史直到窗口关闭）。
    pub translate: Option<Entity<TranslatePageView>>,
    /// 许可证摘要文件（找到才有；关于页据此决定是否显示「查看许可证」）。
    pub license_file: Option<PathBuf>,
    /// 交给宿主处理的动作出口（未接入时按钮不可用 / 不记忆几何）。
    pub hook: Option<Rc<dyn Fn(PageAction)>>,
    /// 窗口几何防抖代数。
    pub geometry_gen: u64,
}

impl ExtraState {
    /// 创建状态。
    ///
    /// # 参数
    /// - `translation_enabled`：翻译页是否启用。
    pub fn new(translation_enabled: bool) -> Self {
        Self {
            nav: PageNav::new(translation_enabled),
            translate_factory: None,
            translate: None,
            license_file: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().and_then(find_license_file)),
            hook: None,
            geometry_gen: 0,
        }
    }
}

impl SettingsView {
    /// 接入追加页的宿主动作出口。
    ///
    /// # 参数
    /// - `hook`：按钮点击 / 窗口几何停稳时调用，由宿主转成主线程事件。
    pub fn set_page_hook(&mut self, hook: Rc<dyn Fn(PageAction)>) {
        self.extra.hook = Some(hook);
    }

    /// 接入翻译页视图工厂。
    ///
    /// # 参数
    /// - `factory`：首次进入翻译页时调用。
    pub fn set_translate_factory(&mut self, factory: TranslateFactory) {
        self.extra.translate_factory = Some(factory);
    }

    /// 内嵌的翻译页视图（尚未进入过翻译页时为 `None`），运行时把翻译结果与开关变化同步给它。
    pub fn translate_view(&self) -> Option<Entity<TranslatePageView>> {
        self.extra.translate.clone()
    }

    /// 翻译页开关变化（设置里改了）。
    ///
    /// # 参数
    /// - `enabled`：新的开关值。
    pub fn set_translation_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.extra.nav.set_translation_enabled(enabled);
        cx.notify();
    }

    /// 界面偏好变化（语言 / 主题改了）：同步给内嵌翻译页。
    ///
    /// # 参数
    /// - `prefs`：新的界面偏好。
    pub fn sync_prefs(&mut self, prefs: crate::settings_state::UiPrefs, cx: &mut Context<Self>) {
        if let Some(view) = &self.extra.translate {
            view.update(cx, |v, vcx| v.set_prefs(prefs, vcx));
        }
        cx.notify();
    }

    /// 按入口跳页：设置类入口落到第一个设置分组，主窗口类入口保持当前页。
    ///
    /// # 参数
    /// - `entry`：唤起窗口的入口。
    pub fn go_entry(&mut self, entry: WindowEntry, cx: &mut Context<Self>) {
        if let Some(group) = entry.target_group() {
            self.show_group_id(group, cx);
        }
    }

    /// 切到某个追加页（侧栏点击）；会放弃进行中的输入编辑。
    ///
    /// # 参数
    /// - `page`：目标页。
    /// - `window`：所属窗口（收起已展开的下拉）。
    pub(crate) fn select_extra(
        &mut self,
        page: ExtraPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.extra.nav.select(page) {
            self.state.cancel_input();
            self.close_dropdowns(window, cx);
            cx.notify();
        }
    }

    /// 窗口位置 / 大小变化：停住一小会儿后通知宿主记忆几何。
    ///
    /// # 参数
    /// - `maximized`：此刻是否最大化。
    pub(crate) fn on_bounds_changed(&mut self, maximized: bool, cx: &mut Context<Self>) {
        self.extra.geometry_gen += 1;
        let generation = self.extra.geometry_gen;
        cx.spawn(async move |this, acx| {
            acx.background_executor()
                .timer(Duration::from_millis(GEOMETRY_SAVE_DEBOUNCE_MS))
                .await;
            let _ = this.update(acx, |view, _cx| {
                if view.extra.geometry_gen == generation
                    && let Some(hook) = &view.extra.hook
                {
                    hook(PageAction::WindowSettled { maximized });
                }
            });
        })
        .detach();
    }

    /// 渲染侧栏里追加页那一块（分隔线 + 各页条目）。
    ///
    /// # 参数
    /// - `p`：配色。
    pub(crate) fn render_extra_nav(&self, p: &Palette, cx: &mut Context<Self>) -> Vec<Div> {
        let i18n = crate::ocr_backend::i18n_for(self.state.prefs().locale);
        let mut items = vec![div().flex_none().h(px(1.0)).my_1().bg(p.border)];
        for page in self.extra.nav.visible_pages() {
            let active = self.extra.nav.current() == Some(page);
            items.push(
                div()
                    .h(px(32.0))
                    .px_3()
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .bg(if active { p.accent } else { rgba(0x00000000) })
                    .text_color(if active { p.on_accent } else { p.text })
                    .child(i18n.tr(page.title_id()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.select_extra(page, window, cx);
                        }),
                    ),
            );
        }
        items
    }

    /// 渲染追加页的内容区。
    ///
    /// # 参数
    /// - `page`：当前追加页。
    /// - `p`：配色。
    /// - `window`：所属窗口（首次进入翻译页时创建视图用）。
    pub(crate) fn render_extra_page(
        &mut self,
        page: ExtraPage,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.state.prefs().locale);
        if page == ExtraPage::Translation && self.extra.translate.is_none() {
            let factory = self.extra.translate_factory.clone();
            self.extra.translate = factory.map(|f| f(window, cx));
        }
        let hook = self.extra.hook.clone();
        let mut buttons = Vec::new();
        if let Some(group) = page.related_group() {
            buttons.push(
                Button::new("page-goto-settings")
                    .small()
                    .label(i18n.tr("main-goto-settings"))
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.show_group_id(group, cx);
                    })),
            );
        }
        let open = page.open_action().or((page == ExtraPage::Translation)
            .then_some((PageAction::OpenTranslateWindow, "main-open-translate")));
        if let Some((action, label_id)) = open {
            let hook = hook.clone();
            buttons.push(
                Button::new("page-open-window")
                    .small()
                    .label(i18n.tr(label_id))
                    .on_click(move |_e: &ClickEvent, _w, _cx| {
                        if let Some(hook) = &hook {
                            hook(action.clone());
                        }
                    }),
            );
        }
        let buttons = div().flex().items_center().gap(px(GAP)).children(buttons);
        if page == ExtraPage::Translation {
            // 翻译页：顶部标题行 + 内嵌翻译视图
            let head = div()
                .flex_none()
                .flex()
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
                .child(div().flex_1())
                .child(buttons);
            let mut col = div().flex_1().min_w_0().flex().flex_col().child(head);
            if let Some(view) = &self.extra.translate {
                col = col.child(div().flex_1().min_h_0().child(view.clone()));
            }
            return col;
        }
        let col = div()
            .flex_1()
            .min_w_0()
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
        match page {
            ExtraPage::About => self.render_about(col, p, cx),
            _ => col.child(buttons),
        }
    }

    /// 渲染关于页：名称与版本、来源与许可证、更新检查。
    ///
    /// # 参数
    /// - `col`：已放好标题与说明的内容列。
    fn render_about(&mut self, col: Div, p: &Palette, cx: &mut Context<Self>) -> Div {
        let locale = self.state.prefs().locale;
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
        col = match self.extra.license_file.clone() {
            Some(path) => {
                let hook = self.extra.hook.clone();
                col.child(
                    div().child(
                        Button::new("about-license-open")
                            .small()
                            .label(i18n.tr("main-about-license-open"))
                            .on_click(move |_e: &ClickEvent, _w, _cx| {
                                if let Some(hook) = &hook {
                                    hook(PageAction::OpenFile(path.clone()));
                                }
                            }),
                    ),
                )
            }
            None => col.child(text(i18n.tr("main-about-license-missing"))),
        };
        // 更新检查：与设置页「更新」分组共用同一套状态、动作入口与文案
        let panel = update_panel(locale, &self.update_state);
        let running = matches!(
            self.update_state,
            UpdateUiState::Running | UpdateUiState::Downloading
        );
        let mut check = Button::new("about-update-check")
            .small()
            .label(panel.button_label);
        check = match &self.update_hook {
            Some(hook) if !running => {
                let hook = Rc::clone(hook);
                check.on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    this.update_state = UpdateUiState::Running;
                    hook(UpdateAction::Check);
                    cx.notify();
                }))
            }
            _ => snow_ui::ui::component::Disableable::disabled(check, true),
        };
        let extra = match (&self.update_state, &self.update_hook) {
            (UpdateUiState::Available { info, .. }, Some(hook)) if !info.url.is_empty() => {
                let (hook, info) = (Rc::clone(hook), info.clone());
                Some(
                    Button::new("about-update-download")
                        .small()
                        .label(panel.download_label)
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                            this.update_state = UpdateUiState::Downloading;
                            hook(UpdateAction::Download(info.clone()));
                            cx.notify();
                        })),
                )
            }
            (UpdateUiState::Downloaded { dir, .. }, Some(hook)) => {
                let (hook, dir) = (Rc::clone(hook), dir.clone());
                Some(
                    Button::new("about-update-folder")
                        .small()
                        .label(panel.open_folder_label)
                        .on_click(move |_e: &ClickEvent, _w, _cx| {
                            hook(UpdateAction::OpenFolder(dir.clone()));
                        }),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 翻译页随开关显隐；关闭时若正停在翻译页则退回设置分组。
    #[test]
    fn translation_page_follows_switch() {
        let mut nav = PageNav::new(false);
        assert!(!nav.visible_pages().contains(&ExtraPage::Translation));
        assert!(!nav.select(ExtraPage::Translation));
        assert_eq!(nav.current(), None);
        nav.set_translation_enabled(true);
        assert!(nav.select(ExtraPage::Translation));
        assert_eq!(nav.current(), Some(ExtraPage::Translation));
        nav.set_translation_enabled(false);
        assert_eq!(nav.current(), None);
    }

    /// 追加页顺序固定，关于页最后；离开后回到设置分组。
    #[test]
    fn nav_order_and_leave() {
        let mut nav = PageNav::new(true);
        assert_eq!(nav.visible_pages(), ExtraPage::ALL.to_vec());
        assert_eq!(ExtraPage::ALL.last(), Some(&ExtraPage::About));
        assert!(nav.select(ExtraPage::About));
        nav.leave();
        assert_eq!(nav.current(), None);
    }

    /// 入口到页的映射：设置入口落到第一个设置分组，主窗口入口不改变当前页。
    #[test]
    fn entries_map_to_groups() {
        assert_eq!(WindowEntry::Main.target_group(), None);
        let first = crate::settings_model::groups().first().map(|g| g.id);
        assert!(first.is_some());
        assert_eq!(WindowEntry::Settings.target_group(), first);
    }

    /// 单例：已开则激活，未开才新建。
    #[test]
    fn open_plan_is_singleton() {
        assert_eq!(plan_open(true), OpenPlan::Activate);
        assert_eq!(plan_open(false), OpenPlan::Create);
    }

    /// 每个追加页的标题 / 说明 id 唯一，相关设置分组都真实存在，入口页有打开动作。
    #[test]
    fn extra_pages_are_well_formed() {
        let mut titles = std::collections::HashSet::new();
        for page in ExtraPage::ALL {
            assert!(titles.insert(page.title_id()));
            if let Some(group) = page.related_group() {
                assert!(
                    crate::settings_model::groups()
                        .iter()
                        .any(|g| g.id == group),
                    "分组 {group} 不存在"
                );
            }
        }
        assert_eq!(
            ExtraPage::History.open_action().map(|(a, _)| a),
            Some(PageAction::OpenHistory)
        );
        assert_eq!(
            ExtraPage::PinManage.open_action().map(|(a, _)| a),
            Some(PageAction::OpenPinManage)
        );
        assert!(ExtraPage::About.open_action().is_none());
    }

    /// 许可证文件向上查找：从深层目录能找到上层的，同层优先。
    #[test]
    fn license_file_is_found_by_walking_up() {
        let root = std::env::temp_dir().join(format!("cisox-license-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join(LICENSE_FILE), "x").unwrap();
        assert_eq!(find_license_file(&deep), Some(root.join(LICENSE_FILE)));
        std::fs::write(deep.join(LICENSE_FILE), "y").unwrap();
        assert_eq!(find_license_file(&deep), Some(deep.join(LICENSE_FILE)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
