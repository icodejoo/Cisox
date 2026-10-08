//! 主窗口的导航与状态模型：页面清单、侧栏折叠、设置分组展开、当前页、页内设置分组。
//!
//! 纯逻辑，不依赖 GPUI，可离屏测试。窗口关闭即整体释放，不做后台常驻。

/// 配置键：侧栏是否折叠。
pub const SIDEBAR_COLLAPSED_KEY: &str = "interface/sidebar_collapsed";
/// 配置键：翻译页是否启用（侧栏里是否显示翻译页）。
pub const TRANSLATION_PAGE_ENABLED_KEY: &str = "extended_features/translation_page_enabled";
/// 主窗口默认页。
pub const DEFAULT_PAGE: MainPage = MainPage::GlobalHotkeys;

/// 主窗口里的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainPage {
    /// 全局快捷键。
    GlobalHotkeys,
    /// 全局鼠标手势。
    GlobalMouse,
    /// 截图历史。
    History,
    /// 贴图管理。
    PinManage,
    /// 翻译。
    Translation,
    /// 设置 / 界面。
    Interface,
    /// 设置 / 功能。
    Function,
    /// 设置 / 应用内快捷键。
    AppShortcuts,
    /// 设置 / 存储与隐私。
    Storage,
    /// 设置 / API 配置。
    ApiConfig,
    /// 设置 / 扩展功能。
    ExtendedFeatures,
    /// 设置 / 系统。
    System,
    /// 关于。
    About,
}

/// 页面内容能唤起的现成窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTarget {
    /// 设置窗口。
    Settings,
    /// 截图历史窗口。
    History,
    /// 贴图管理窗口。
    PinManage,
    /// 翻译页独立窗口。
    TranslatePage,
}

/// 页面内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageContent {
    /// 入口页：说明文字加一个打开现成窗口的按钮。
    Open(OpenTarget),
    /// 内嵌设置页：直接显示本页所属的设置分组（复用设置页视图）。
    Settings,
    /// 内嵌翻译页：直接在内容区翻译（复用翻译页视图），另可在独立窗口打开。
    Translate,
    /// 关于页。
    About,
    /// 尚未提供的页面；可附带一个可用的替代入口。
    Placeholder(Option<OpenTarget>),
}

/// 侧栏里「设置」分组下的页面（按显示顺序）。
pub const SETTINGS_PAGES: [MainPage; 7] = [
    MainPage::Interface,
    MainPage::Function,
    MainPage::AppShortcuts,
    MainPage::Storage,
    MainPage::ApiConfig,
    MainPage::ExtendedFeatures,
    MainPage::System,
];

/// 侧栏顶层页面（设置分组之前，按显示顺序）。
const TOP_PAGES: [MainPage; 5] = [
    MainPage::GlobalHotkeys,
    MainPage::GlobalMouse,
    MainPage::History,
    MainPage::PinManage,
    MainPage::Translation,
];

impl MainPage {
    /// 页面标题的本地化 id。
    pub fn title_id(self) -> &'static str {
        match self {
            Self::GlobalHotkeys => "main-page-hotkeys",
            Self::GlobalMouse => "main-page-mouse",
            Self::History => "main-page-history",
            Self::PinManage => "main-page-pins",
            Self::Translation => "main-page-translation",
            Self::Interface => "main-page-interface",
            Self::Function => "main-page-function",
            Self::AppShortcuts => "main-page-app-shortcuts",
            Self::Storage => "main-page-storage",
            Self::ApiConfig => "main-page-api",
            Self::ExtendedFeatures => "main-page-extended",
            Self::System => "main-page-system",
            Self::About => "main-page-about",
        }
    }

    /// 页面说明的本地化 id。
    pub fn desc_id(self) -> &'static str {
        match self {
            Self::GlobalHotkeys => "main-desc-hotkeys",
            Self::GlobalMouse => "main-desc-mouse",
            Self::History => "main-desc-history",
            Self::PinManage => "main-desc-pins",
            Self::Translation => "main-desc-translation",
            Self::Interface => "main-desc-interface",
            Self::Function => "main-desc-function",
            Self::AppShortcuts => "main-desc-app-shortcuts",
            Self::Storage => "main-desc-storage",
            Self::ApiConfig => "main-desc-api",
            Self::ExtendedFeatures => "main-desc-extended",
            Self::System => "main-desc-system",
            Self::About => "main-desc-about",
        }
    }

    /// 页面内容类型。
    pub fn content(self) -> PageContent {
        match self {
            Self::History => PageContent::Open(OpenTarget::History),
            Self::PinManage => PageContent::Open(OpenTarget::PinManage),
            Self::Translation => PageContent::Translate,
            Self::About => PageContent::About,
            _ => PageContent::Settings,
        }
    }

    /// 本页内嵌的设置分组 id（按显示顺序）；非设置页为空。
    ///
    /// 28 个设置分组恰好分摊到各设置页，每个分组只属于一页。
    pub fn settings_groups(self) -> &'static [&'static str] {
        match self {
            Self::GlobalHotkeys => &["global_shortcuts"],
            Self::GlobalMouse => &["global_mouse"],
            Self::Interface => &["interface", "tray"],
            Self::Function => &[
                "screenshot",
                "screenshot_ui",
                "screenshot_selection",
                "screenshot_toolbar",
                "screenshot_translation",
                "screenshot_conversion",
                "drawing",
                "pin_to_screen",
                "pinned_history",
                "capture_history",
                "text_recognition",
                "dictation",
                "screen_recording",
            ],
            Self::AppShortcuts => &[
                "screenshot_shortcuts",
                "drawing_shortcuts",
                "pin_to_screen_shortcuts",
                "screen_recording_shortcuts",
            ],
            Self::Storage => &["storage"],
            Self::ApiConfig => &["api_configuration", "mcp"],
            Self::ExtendedFeatures => &["extended_features"],
            Self::System => &["system", "updates", "network"],
            Self::History | Self::PinManage | Self::Translation | Self::About => &[],
        }
    }

    /// 设置分组 id 所在的页面；不属于任何页返回 `None`。
    ///
    /// # 参数
    /// - `group_id`：设置分组 id（如 `screenshot_ui`）。
    pub fn for_settings_group(group_id: &str) -> Option<MainPage> {
        TOP_PAGES
            .iter()
            .chain(SETTINGS_PAGES.iter())
            .copied()
            .find(|page| page.settings_groups().contains(&group_id))
    }

    /// 入口页（历史 / 贴图 / 翻译）相关的设置分组 id：页面上的按钮点一下跳到该分组。
    pub fn related_settings_group(self) -> Option<&'static str> {
        match self {
            Self::History => Some("capture_history"),
            Self::PinManage => Some("pin_to_screen"),
            Self::Translation => Some("screenshot_translation"),
            _ => None,
        }
    }

    /// 侧栏图标名（snow-ui-icons 的描边图标）；折叠态只显示它。
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::GlobalHotkeys => "key",
            Self::GlobalMouse => "aim",
            Self::History => "history",
            Self::PinManage => "pushpin",
            Self::Translation => "translation",
            Self::Interface => "skin",
            Self::Function => "appstore",
            Self::AppShortcuts => "thunderbolt",
            Self::Storage => "database",
            Self::ApiConfig => "api",
            Self::ExtendedFeatures => "experiment",
            Self::System => "desktop",
            Self::About => "info-circle",
        }
    }

    /// 是否属于侧栏「设置」分组。
    pub fn in_settings_group(self) -> bool {
        SETTINGS_PAGES.contains(&self)
    }
}

/// 许可证摘要文件名（仓库根目录）。
pub const LICENSE_FILE: &str = "LICENSE.md";
/// 查找许可证文件时最多向上找几层目录（开发时可执行文件在 `build/cargo/<profile>/` 下）。
const LICENSE_SEARCH_DEPTH: usize = 6;

/// 从 `start` 起逐级向上找许可证摘要文件。
///
/// # 参数
/// - `start`：起始目录（通常是可执行文件所在目录）。
///
/// # 返回
/// 找到的文件路径；找不到返回 `None`（关于页就只显示文字说明）。
pub fn find_license_file(start: &std::path::Path) -> Option<std::path::PathBuf> {
    start
        .ancestors()
        .take(LICENSE_SEARCH_DEPTH)
        .map(|dir| dir.join(LICENSE_FILE))
        .find(|candidate| candidate.is_file())
}

/// 侧栏「设置」分组标题的图标名。
pub const SETTINGS_GROUP_ICON: &str = "setting";
/// 折叠按钮图标名（展开态时点击收起）。
pub const COLLAPSE_ICON: &str = "menu-fold";
/// 展开按钮图标名（折叠态时点击展开）。
pub const EXPAND_ICON: &str = "menu-unfold";

impl OpenTarget {
    /// 入口按钮文案的本地化 id。
    pub fn button_id(self) -> &'static str {
        match self {
            Self::Settings => "main-open-settings",
            Self::History => "main-open-history",
            Self::PinManage => "main-open-pins",
            Self::TranslatePage => "main-open-translate",
        }
    }
}

/// 侧栏的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    /// 普通页面项；`child` 表示缩进显示（设置分组内）。
    Page {
        /// 页面。
        page: MainPage,
        /// 是否为分组子项。
        child: bool,
    },
    /// 「设置」分组标题（可展开 / 收起）。
    SettingsGroup {
        /// 是否展开。
        expanded: bool,
    },
}

/// 主窗口导航状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainWindowModel {
    /// 当前页。
    current: MainPage,
    /// 侧栏是否折叠。
    collapsed: bool,
    /// 设置分组是否展开。
    settings_expanded: bool,
    /// 翻译页是否启用。
    translation_enabled: bool,
    /// 当前设置页里选中的分组下标（相对 [`MainPage::settings_groups`]）。
    group_index: usize,
}

impl MainWindowModel {
    /// 创建模型，默认落在 [`DEFAULT_PAGE`]。
    ///
    /// # 参数
    /// - `collapsed`：侧栏初始是否折叠（来自配置）。
    /// - `translation_enabled`：翻译页是否启用（来自配置）。
    pub fn new(collapsed: bool, translation_enabled: bool) -> Self {
        Self {
            current: DEFAULT_PAGE,
            collapsed,
            settings_expanded: false,
            translation_enabled,
            group_index: 0,
        }
    }

    /// 当前页。
    pub fn current(&self) -> MainPage {
        self.current
    }

    /// 侧栏是否折叠。
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// 页面当前是否可见（翻译页受开关控制）。
    pub fn page_visible(&self, page: MainPage) -> bool {
        page != MainPage::Translation || self.translation_enabled
    }

    /// 导航到某页；页面不可见时忽略并返回 `false`。选中设置分组子页会自动展开分组。
    ///
    /// # 参数
    /// - `page`：目标页。
    pub fn select(&mut self, page: MainPage) -> bool {
        if !self.page_visible(page) {
            return false;
        }
        if self.current != page {
            self.group_index = 0;
        }
        self.current = page;
        if page.in_settings_group() {
            self.settings_expanded = true;
        }
        true
    }

    /// 当前设置页选中的分组 id；当前页不是设置页返回 `None`。
    pub fn settings_group(&self) -> Option<&'static str> {
        self.current
            .settings_groups()
            .get(self.group_index)
            .copied()
    }

    /// 在当前设置页内切换分组；分组不属于当前页时忽略并返回 `false`。
    ///
    /// # 参数
    /// - `group_id`：设置分组 id。
    pub fn select_group(&mut self, group_id: &str) -> bool {
        match self
            .current
            .settings_groups()
            .iter()
            .position(|g| *g == group_id)
        {
            Some(index) => {
                self.group_index = index;
                true
            }
            None => false,
        }
    }

    /// 跳到某个设置分组：切到它所在的页并选中该分组（入口按钮用）。
    ///
    /// # 参数
    /// - `group_id`：设置分组 id。
    ///
    /// # 返回
    /// 分组不属于任何页（或所在页不可见）时返回 `false`。
    pub fn jump_to_group(&mut self, group_id: &str) -> bool {
        let Some(page) = MainPage::for_settings_group(group_id) else {
            return false;
        };
        if !self.select(page) {
            return false;
        }
        self.select_group(group_id)
    }

    /// 切换侧栏折叠，返回切换后的值（调用方据此落盘）。
    pub fn toggle_collapsed(&mut self) -> bool {
        self.collapsed = !self.collapsed;
        self.collapsed
    }

    /// 展开 / 收起设置分组。
    pub fn toggle_settings_group(&mut self) {
        self.settings_expanded = !self.settings_expanded;
    }

    /// 翻译页开关变化；当前页正好是翻译页且被关闭时退回默认页。
    ///
    /// # 参数
    /// - `enabled`：新的开关值。
    pub fn set_translation_enabled(&mut self, enabled: bool) {
        self.translation_enabled = enabled;
        if !enabled && self.current == MainPage::Translation {
            self.current = DEFAULT_PAGE;
        }
    }

    /// 侧栏要显示的行（按顺序）；设置分组收起时不含子项。
    pub fn nav_items(&self) -> Vec<NavItem> {
        let mut items: Vec<NavItem> = TOP_PAGES
            .iter()
            .copied()
            .filter(|p| self.page_visible(*p))
            .map(|page| NavItem::Page { page, child: false })
            .collect();
        items.push(NavItem::SettingsGroup {
            expanded: self.settings_expanded,
        });
        if self.settings_expanded {
            items.extend(
                SETTINGS_PAGES
                    .iter()
                    .map(|&page| NavItem::Page { page, child: true }),
            );
        }
        items.push(NavItem::Page {
            page: MainPage::About,
            child: false,
        });
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认落在默认页，设置分组收起。
    #[test]
    fn starts_on_default_page_with_group_collapsed() {
        let m = MainWindowModel::new(false, true);
        assert_eq!(m.current(), DEFAULT_PAGE);
        let items = m.nav_items();
        assert!(items.contains(&NavItem::SettingsGroup { expanded: false }));
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, NavItem::Page { child: true, .. }))
        );
        assert_eq!(
            items.last(),
            Some(&NavItem::Page {
                page: MainPage::About,
                child: false
            })
        );
    }

    /// 选中设置子页会展开分组并显示全部子页。
    #[test]
    fn selecting_settings_child_expands_group() {
        let mut m = MainWindowModel::new(false, true);
        assert!(m.select(MainPage::Storage));
        assert_eq!(m.current(), MainPage::Storage);
        let children = m
            .nav_items()
            .into_iter()
            .filter(|i| matches!(i, NavItem::Page { child: true, .. }))
            .count();
        assert_eq!(children, SETTINGS_PAGES.len());
        m.toggle_settings_group();
        assert!(
            m.nav_items()
                .contains(&NavItem::SettingsGroup { expanded: false })
        );
    }

    /// 翻译页关闭时不显示、不可选；开着时被关掉会退回默认页。
    #[test]
    fn translation_page_follows_switch() {
        let mut m = MainWindowModel::new(false, false);
        assert!(!m.select(MainPage::Translation));
        assert!(!m.nav_items().contains(&NavItem::Page {
            page: MainPage::Translation,
            child: false
        }));
        m.set_translation_enabled(true);
        assert!(m.select(MainPage::Translation));
        m.set_translation_enabled(false);
        assert_eq!(m.current(), DEFAULT_PAGE);
    }

    /// 折叠切换返回新值。
    #[test]
    fn toggle_collapsed_returns_new_value() {
        let mut m = MainWindowModel::new(true, true);
        assert!(m.collapsed());
        assert!(!m.toggle_collapsed());
        assert!(m.toggle_collapsed());
    }

    /// 每个页面都有标题 / 说明 id，且 id 互不重复；翻译页是带替代入口的占位。
    #[test]
    fn page_ids_unique_and_contents_defined() {
        let all: Vec<MainPage> = TOP_PAGES
            .iter()
            .chain(SETTINGS_PAGES.iter())
            .copied()
            .chain([MainPage::About])
            .collect();
        let mut ids = std::collections::HashSet::new();
        for p in &all {
            assert!(ids.insert(p.title_id()));
            assert!(ids.insert(p.desc_id()));
        }
        assert_eq!(MainPage::Translation.content(), PageContent::Translate);
        assert_eq!(MainPage::About.content(), PageContent::About);
        assert_eq!(
            MainPage::History.content(),
            PageContent::Open(OpenTarget::History)
        );
        assert_eq!(MainPage::Storage.content(), PageContent::Settings);
        assert_eq!(MainPage::GlobalHotkeys.content(), PageContent::Settings);
    }

    /// 28 个设置分组恰好分摊到各设置页：不重不漏，且非设置页没有分组。
    #[test]
    fn every_settings_group_belongs_to_exactly_one_page() {
        let mut seen = std::collections::HashSet::new();
        let pages = TOP_PAGES
            .iter()
            .chain(SETTINGS_PAGES.iter())
            .copied()
            .chain([MainPage::About]);
        for page in pages {
            assert_eq!(
                page.content() == PageContent::Settings,
                !page.settings_groups().is_empty(),
                "{page:?}"
            );
            for id in page.settings_groups() {
                assert!(seen.insert(*id), "分组重复：{id}");
                assert_eq!(MainPage::for_settings_group(id), Some(page));
            }
        }
        for id in crate::settings_text::GROUP_IDS {
            assert!(seen.contains(id), "分组未分配到页面：{id}");
        }
        assert_eq!(seen.len(), crate::settings_text::GROUP_IDS.len());
    }

    /// 设置页默认选中第一个分组；换页重置，页内可切换，跨页分组被拒绝。
    #[test]
    fn settings_group_selection_follows_page() {
        let mut m = MainWindowModel::new(false, true);
        assert_eq!(m.settings_group(), Some("global_shortcuts"));
        assert!(m.select(MainPage::Function));
        assert_eq!(m.settings_group(), Some("screenshot"));
        assert!(m.select_group("drawing"));
        assert_eq!(m.settings_group(), Some("drawing"));
        assert!(!m.select_group("storage"), "别页的分组不能在本页选");
        assert_eq!(m.settings_group(), Some("drawing"));
        // 重选同一页不重置分组，换页则重置
        assert!(m.select(MainPage::Function));
        assert_eq!(m.settings_group(), Some("drawing"));
        assert!(m.select(MainPage::Storage));
        assert!(m.select(MainPage::Function));
        assert_eq!(m.settings_group(), Some("screenshot"));
        assert!(m.select(MainPage::About));
        assert_eq!(m.settings_group(), None);
    }

    /// 入口按钮按分组 id 跳页并选中分组，同时展开设置分组。
    #[test]
    fn jump_to_group_selects_page_and_group() {
        let mut m = MainWindowModel::new(false, true);
        assert!(m.jump_to_group("screen_recording"));
        assert_eq!(m.current(), MainPage::Function);
        assert_eq!(m.settings_group(), Some("screen_recording"));
        assert!(
            m.nav_items()
                .contains(&NavItem::SettingsGroup { expanded: true })
        );
        assert!(!m.jump_to_group("no_such_group"));
        assert_eq!(m.current(), MainPage::Function);
    }

    /// 入口页的相关设置分组都存在，点击入口能跳过去。
    #[test]
    fn related_groups_are_jumpable() {
        for page in [
            MainPage::History,
            MainPage::PinManage,
            MainPage::Translation,
        ] {
            let group = page.related_settings_group().unwrap();
            assert!(MainPage::for_settings_group(group).is_some(), "{group}");
        }
        assert_eq!(MainPage::Storage.related_settings_group(), None);
        let mut m = MainWindowModel::new(false, true);
        assert!(m.jump_to_group(MainPage::History.related_settings_group().unwrap()));
        assert_eq!(m.settings_group(), Some("capture_history"));
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

    /// 每个页面都有可渲染的侧栏图标。
    #[test]
    fn every_page_icon_exists() {
        let pages = TOP_PAGES
            .iter()
            .chain(SETTINGS_PAGES.iter())
            .copied()
            .chain([MainPage::About]);
        for page in pages {
            let icon =
                snow_ui::icons::IconRef::new(snow_ui::icons::IconTheme::Outlined, page.icon_name());
            assert!(icon.exists(), "缺少图标 {}", page.icon_name());
        }
        for name in [SETTINGS_GROUP_ICON, COLLAPSE_ICON, EXPAND_ICON] {
            let icon = snow_ui::icons::IconRef::new(snow_ui::icons::IconTheme::Outlined, name);
            assert!(icon.exists(), "缺少图标 {name}");
        }
    }
}
