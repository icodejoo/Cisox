//! 主窗口的导航与状态模型：页面清单、侧栏折叠、设置分组展开、当前页。
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
    /// 输入框翻译浮窗。
    TranslateInput,
}

/// 页面内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageContent {
    /// 入口页：说明文字加一个打开现成窗口的按钮。
    Open(OpenTarget),
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
            Self::Translation => PageContent::Placeholder(Some(OpenTarget::TranslateInput)),
            Self::About => PageContent::About,
            _ => PageContent::Open(OpenTarget::Settings),
        }
    }

    /// 是否属于侧栏「设置」分组。
    pub fn in_settings_group(self) -> bool {
        SETTINGS_PAGES.contains(&self)
    }
}

impl OpenTarget {
    /// 入口按钮文案的本地化 id。
    pub fn button_id(self) -> &'static str {
        match self {
            Self::Settings => "main-open-settings",
            Self::History => "main-open-history",
            Self::PinManage => "main-open-pins",
            Self::TranslateInput => "main-open-translate",
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
        self.current = page;
        if page.in_settings_group() {
            self.settings_expanded = true;
        }
        true
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
        assert_eq!(
            MainPage::Translation.content(),
            PageContent::Placeholder(Some(OpenTarget::TranslateInput))
        );
        assert_eq!(MainPage::About.content(), PageContent::About);
        assert_eq!(
            MainPage::History.content(),
            PageContent::Open(OpenTarget::History)
        );
    }
}
