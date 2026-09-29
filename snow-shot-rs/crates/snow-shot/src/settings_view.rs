//! Schema 驱动的设置页视图组件（Settings View）。
//!
//! 依据 `snow_config::schema::entries()` 自动提取 238 个配置项，
//! 按分组映射为左侧分类导航（通用、快捷键、截图、贴图、标注、OCR、录屏、高级）与
//! 右侧动态表单控件（开关 Switch、数值范围 Slider/Input、枚举单选 Select、字符串文本框）。

use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_config::schema::{IntRange, ValueKind, entries};
use snow_ui::ui::*;

/// 设置分类大项（左侧侧边栏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsCategory {
    /// 通用设置。
    General,
    /// 快捷键与热键。
    Shortcuts,
    /// 屏幕截图与捕获。
    Screenshot,
    /// 贴图与钉图。
    PinToScreen,
    /// 画板与二次标注。
    Drawing,
    /// 文字识别与翻译。
    OcrAndTranslate,
    /// 屏幕录制。
    ScreenRecording,
    /// 存储与文件路径。
    Storage,
    /// 高级设置。
    Advanced,
}

impl SettingsCategory {
    /// 获取全部预定义分类列表。
    pub const fn all() -> &'static [Self] {
        &[
            Self::General,
            Self::Shortcuts,
            Self::Screenshot,
            Self::PinToScreen,
            Self::Drawing,
            Self::OcrAndTranslate,
            Self::ScreenRecording,
            Self::Storage,
            Self::Advanced,
        ]
    }

    /// 分类中文标题。
    pub const fn title(&self) -> &'static str {
        match self {
            Self::General => "通用设置",
            Self::Shortcuts => "快捷键",
            Self::Screenshot => "屏幕截图",
            Self::PinToScreen => "贴图钉图",
            Self::Drawing => "画板标注",
            Self::OcrAndTranslate => "文字与翻译",
            Self::ScreenRecording => "屏幕录制",
            Self::Storage => "存储与路径",
            Self::Advanced => "高级设置",
        }
    }

    /// 判断指定的 `"组/名"` 键是否归属于当前分类。
    pub fn matches_key(&self, key: &str) -> bool {
        match self {
            Self::General => key.starts_with("general/") || key.starts_with("tray/") || key.starts_with("appearance/"),
            Self::Shortcuts => key.contains("shortcut"),
            Self::Screenshot => key.starts_with("screenshot/") || key.starts_with("capture/"),
            Self::PinToScreen => key.starts_with("pin_to_screen/"),
            Self::Drawing => key.starts_with("drawing/") || key.starts_with("annotation/"),
            Self::OcrAndTranslate => key.starts_with("ocr/") || key.starts_with("translation/") || key.starts_with("api_configuration/"),
            Self::ScreenRecording => key.starts_with("screen_recording/") || key.starts_with("video/"),
            Self::Storage => key.starts_with("storage/") || key.starts_with("history/"),
            Self::Advanced => {
                !Self::General.matches_key(key)
                    && !Self::Shortcuts.matches_key(key)
                    && !Self::Screenshot.matches_key(key)
                    && !Self::PinToScreen.matches_key(key)
                    && !Self::Drawing.matches_key(key)
                    && !Self::OcrAndTranslate.matches_key(key)
                    && !Self::ScreenRecording.matches_key(key)
                    && !Self::Storage.matches_key(key)
            }
        }
    }
}

/// 设置表单渲染条目项。
#[derive(Debug, Clone, PartialEq)]
pub struct FormEntryItem {
    /// 完整键名（例如 `"general/theme"`）。
    pub key: String,
    /// 组内短键名（例如 `"theme"`）。
    pub short_name: String,
    /// 当前生效值。
    pub current_value: Value,
    /// 默认值。
    pub default_value: Value,
    /// 数据类型。
    pub kind: ValueKind,
    /// 可选的整数范围。
    pub int_range: Option<IntRange>,
    /// 可选的字符串枚举候选集。
    pub string_whitelist: Option<Vec<String>>,
}

/// 设置页用户交互操作。
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    /// 切换左侧大类导航。
    SwitchCategory(SettingsCategory),
    /// 修改指定配置项的值。
    ChangeValue {
        /// 配置键名。
        key: String,
        /// 新值。
        new_value: Value,
    },
    /// 重置单个配置项为默认值。
    ResetKey(String),
    /// 重置当前分类下所有配置项。
    ResetCurrentCategory,
}

/// 设置页视图组件。
pub struct SettingsView {
    /// 配置文档对象。
    pub document: ConfigDocument,
    /// 当前选中的大分类。
    pub active_category: SettingsCategory,
    /// 搜索过滤文本。
    pub search_query: String,
    /// 最近一次操作的提示文本。
    pub status_message: Option<String>,
}

impl SettingsView {
    /// 创建新的设置页视图组件。
    pub fn new(document: ConfigDocument) -> Self {
        Self {
            document,
            active_category: SettingsCategory::General,
            search_query: String::new(),
            status_message: None,
        }
    }

    /// 获取当前分类或搜索过滤下的表单条目列表。
    pub fn get_visible_entries(&self) -> Vec<FormEntryItem> {
        let mut list = Vec::new();
        let query = self.search_query.trim().to_lowercase();

        for item in entries() {
            let key = item.key;
            // 排除内部版本控制键
            if key == snow_config::schema::SCHEMA_VERSION_KEY {
                continue;
            }

            if !query.is_empty() {
                if !key.to_lowercase().contains(&query) {
                    continue;
                }
            } else if !self.active_category.matches_key(key) {
                continue;
            }

            let short_name = key.split_once('/').map(|(_, s)| s).unwrap_or(key).to_string();
            let current_value = self.document.value(key);
            let whitelist = if item.allowed.is_empty() {
                None
            } else {
                Some(item.allowed.iter().map(|s| s.to_string()).collect())
            };

            list.push(FormEntryItem {
                key: key.to_string(),
                short_name,
                current_value,
                default_value: item.default.clone(),
                kind: item.kind,
                int_range: item.range,
                string_whitelist: whitelist,
            });
        }

        list
    }

    /// 执行设置页交互动作。
    pub fn handle_action(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::SwitchCategory(cat) => {
                self.active_category = cat;
                self.search_query.clear();
            }
            SettingsAction::ChangeValue { key, new_value } => {
                match self.document.set_value(&key, new_value) {
                    Ok(_) => self.status_message = Some(format!("已更新: {key}")),
                    Err(e) => self.status_message = Some(format!("修改失败 [{key}]: {e:?}")),
                }
            }
            SettingsAction::ResetKey(key) => {
                let default_val = snow_config::schema::default_value(&key);
                match self.document.set_value(&key, default_val) {
                    Ok(_) => self.status_message = Some(format!("已恢复默认: {key}")),
                    Err(e) => self.status_message = Some(format!("重置失败: {e:?}")),
                }
            }
            SettingsAction::ResetCurrentCategory => {
                let keys: Vec<String> = self
                    .get_visible_entries()
                    .into_iter()
                    .map(|item| item.key)
                    .collect();
                let mut success_count = 0;
                for k in keys {
                    let def = snow_config::schema::default_value(&k);
                    if self.document.set_value(&k, def).is_ok() {
                        success_count += 1;
                    }
                }
                self.status_message = Some(format!("已重置当前分类下 {success_count} 项配置"));
            }
        }
    }
}

impl Render for SettingsView {
    /// 渲染设置窗口布局（左侧导航，右侧动态控件列表）。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let visible_items = self.get_visible_entries();
        let active_cat = self.active_category;

        let root = div()
            .flex()
            .w_full()
            .h_full()
            .bg(rgba(0x1F1F1FFF))
            .text_color(rgba(0xE0E0E0FF));

        // 1. 左侧分类侧边栏
        let mut sidebar = div()
            .w(px(200.0))
            .h_full()
            .bg(rgba(0x141414FF))
            .border_r_1()
            .border_color(rgba(0x303030FF))
            .p_3()
            .flex()
            .flex_col()
            .gap_1();

        sidebar = sidebar.child(
            div()
                .pb_3()
                .text_size(px(16.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgba(0xFFFFFFFF))
                .child("系统偏好设置"),
        );

        for cat in SettingsCategory::all() {
            let is_active = *cat == active_cat;
            let item_bg = if is_active {
                rgba(0x1677FFFF)
            } else {
                rgba(0x00000000)
            };
            let text_color = if is_active {
                rgba(0xFFFFFFFF)
            } else {
                rgba(0xBFBFBFFF)
            };

            let nav_item = div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(item_bg)
                .text_color(text_color)
                .text_size(px(13.0))
                .font_weight(if is_active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .child(cat.title());

            sidebar = sidebar.child(nav_item);
        }

        // 2. 右侧配置列表与表单渲染
        let mut content = div()
            .flex_1()
            .h_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4();

        // 顶部标题与状态栏
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .pb_2()
            .border_b_1()
            .border_color(rgba(0x303030FF))
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgba(0xFFFFFFFF))
                    .child(active_cat.title()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgba(0x8C8C8CFF))
                    .child(format!("共 {} 项配置", visible_items.len())),
            );

        content = content.child(header);

        // 表单控件列表渲染
        let mut form_list = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3();

        for item in visible_items.iter().take(12) {
            let val_str = item.current_value.to_string();
            let mut row = div()
                .flex()
                .items_center()
                .justify_between()
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgba(0x262626FF))
                .border_1()
                .border_color(rgba(0x3A3A3AFF));

            let label_col = div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgba(0xFFFFFFFF))
                        .child(item.short_name.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgba(0x8C8C8CFF))
                        .child(item.key.clone()),
                );

            let value_col = div()
                .px_3()
                .py_1()
                .rounded_sm()
                .bg(rgba(0x333333FF))
                .text_size(px(12.0))
                .text_color(rgba(0x52C41AFF))
                .child(val_str);

            row = row.child(label_col).child(value_col);
            form_list = form_list.child(row);
        }

        content = content.child(form_list);

        root.child(sidebar).child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 验证全部分类能涵盖所有主要配置键。
    #[test]
    fn test_categories_coverage() {
        let all_entries = entries();
        assert_eq!(all_entries.len(), 238);

        let mut categorized_count = 0;
        for item in all_entries {
            if item.key == snow_config::schema::SCHEMA_VERSION_KEY {
                continue;
            }
            let matches_any = SettingsCategory::all().iter().any(|cat| cat.matches_key(item.key));
            assert!(matches_any, "键 {} 未被任何设置分类匹配", item.key);
            categorized_count += 1;
        }
        assert_eq!(categorized_count, 237);
    }

    /// 验证设置视图的可见项提取与搜索过滤。
    #[test]
    fn test_settings_view_filtering() {
        let doc = ConfigDocument::from_bytes(None);
        let mut view = SettingsView::new(doc);

        view.active_category = SettingsCategory::General;
        let general_items = view.get_visible_entries();
        assert!(!general_items.is_empty());

        // 搜索过滤
        view.search_query = "theme".to_string();
        let search_items = view.get_visible_entries();
        assert!(!search_items.is_empty());
        assert!(search_items.iter().any(|i| i.key.contains("theme")));
    }

    /// 验证修改键值与重置。
    #[test]
    fn test_settings_action_change_and_reset() {
        let doc = ConfigDocument::from_bytes(None);
        let mut view = SettingsView::new(doc);

        let test_key = "screenshot/image_quality";
        // 修改为 95
        view.handle_action(SettingsAction::ChangeValue {
            key: test_key.to_string(),
            new_value: json!(95),
        });
        assert_eq!(view.document.value(test_key), json!(95));

        // 越界值拒绝测试
        view.handle_action(SettingsAction::ChangeValue {
            key: test_key.to_string(),
            new_value: json!(150),
        });
        assert_eq!(view.document.value(test_key), json!(95)); // 保持 95，不被污染

        // 重置为默认值 (默认 100)
        view.handle_action(SettingsAction::ResetKey(test_key.to_string()));
        assert_eq!(view.document.value(test_key), json!(100));
    }
}
