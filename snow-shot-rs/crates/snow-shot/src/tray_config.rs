//! 托盘的配置驱动部分：菜单项筛选（`tray/menu_options`）、左 / 中键动作（`tray/*_click_action`）、
//! 图标选择与着色（`tray/icon`、`tray/custom_icon`）。纯逻辑，不接触系统托盘，可离屏测试。

use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_ui::shell::tray::{TrayIconImage, TrayMenuEntry};
use std::collections::HashSet;

/// 托盘总开关配置键。
pub const KEY_ENABLED: &str = "tray/enabled";
/// 托盘图标样式配置键。
pub const KEY_ICON: &str = "tray/icon";
/// 自定义图标路径配置键。
pub const KEY_CUSTOM_ICON: &str = "tray/custom_icon";
/// 左键单击动作配置键。
pub const KEY_LEFT_CLICK: &str = "tray/left_click_action";
/// 中键单击动作配置键。
pub const KEY_MIDDLE_CLICK: &str = "tray/middle_click_action";
/// 菜单项列表配置键。
pub const KEY_MENU_OPTIONS: &str = "tray/menu_options";

/// 托盘图标可选文件的扩展名（与 `image` 已启用的解码格式一致）。
pub const ICON_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

/// 托盘图标边长（像素）。
pub const ICON_SIZE: u32 = 32;
/// 内置图标（128 像素 PNG，由新 logo `assets/logo.svg` 经 snow-ui-icons 的 gen_app_icons 示例生成）。
const BUILTIN_ICON_PNG: &[u8] = include_bytes!("../assets/tray-icon.png");

/// 点击托盘图标时可选的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayClick {
    /// 普通截图。
    Screenshot,
    /// 打开主窗口（设置页）。
    ShowMainWindow,
    /// 截图并复制。
    ScreenshotCopy,
    /// 截图并贴图。
    ScreenshotFixed,
    /// 打开功能设置。
    OpenFunctionSettings,
}

impl TrayClick {
    /// 由配置值解析；未知值返回 `None`。
    ///
    /// # 参数
    /// - `text`：配置里的动作名。
    ///
    /// ```ignore
    /// assert_eq!(TrayClick::parse("screenshot_copy"), Some(TrayClick::ScreenshotCopy));
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "screenshot" => Self::Screenshot,
            "show_main_window" => Self::ShowMainWindow,
            "screenshot_copy" => Self::ScreenshotCopy,
            "screenshot_fixed" => Self::ScreenshotFixed,
            "open_function_settings" => Self::OpenFunctionSettings,
            _ => return None,
        })
    }
}

/// 读取点击动作配置；缺失或非法回到给定默认值。
///
/// # 参数
/// - `document`：配置文档。
/// - `key`：配置键。
/// - `default`：默认动作。
pub fn click_action(document: &ConfigDocument, key: &str, default: TrayClick) -> TrayClick {
    document
        .value(key)
        .as_str()
        .and_then(TrayClick::parse)
        .unwrap_or(default)
}

/// 托盘是否启用；配置缺失按启用。
///
/// # 参数
/// - `document`：配置文档。
pub fn tray_enabled(document: &ConfigDocument) -> bool {
    !matches!(document.value(KEY_ENABLED), Value::Bool(false))
}

/// 读取菜单项列表（`quick.screenshot`、`tray.exit` 等）；缺失或类型不对得到空集合。
///
/// # 参数
/// - `document`：配置文档。
pub fn menu_options(document: &ConfigDocument) -> HashSet<String> {
    document
        .value(KEY_MENU_OPTIONS)
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 按菜单项列表筛选带标签的菜单项，并规整分隔线（去掉开头、结尾与连续的分隔线）。
///
/// # 参数
/// - `entries`：`(菜单项 ID, 菜单项)`；ID 为 `None` 的（分隔线等）总是保留。
/// - `enabled`：启用的菜单项 ID。
///
/// # 返回
/// 筛选并规整后的菜单。
pub fn filter_menu(
    entries: Vec<(Option<&'static str>, TrayMenuEntry)>,
    enabled: &HashSet<String>,
) -> Vec<TrayMenuEntry> {
    let mut out: Vec<TrayMenuEntry> = Vec::new();
    for (id, entry) in entries {
        if id.is_some_and(|id| !enabled.contains(id)) {
            continue;
        }
        if matches!(entry, TrayMenuEntry::Separator)
            && matches!(out.last(), None | Some(TrayMenuEntry::Separator))
        {
            continue;
        }
        out.push(entry);
    }
    while matches!(out.last(), Some(TrayMenuEntry::Separator)) {
        out.pop();
    }
    out
}

/// 把多选下拉的选中集合转成配置值，保留列表里不认识的值，已知项按候选顺序排列。
///
/// # 参数
/// - `current`：当前配置值（非数组按空列表），其中不在 `allowed` 内的值原样保留。
/// - `allowed`：全部合法候选，决定已知项的顺序。
/// - `selected`：下拉里当前选中的项（顺序与重复不影响结果）。
///
/// # 返回
/// 新的 JSON 数组：选中的已知项（候选顺序）在前，未知值（原顺序）在后。
///
/// ```ignore
/// let next = menu_options_from_selection(&json!(["a", "old"]), &["a", "b"], &["b"]);
/// assert_eq!(next, json!(["b", "old"]));
/// ```
pub fn menu_options_from_selection(current: &Value, allowed: &[&str], selected: &[&str]) -> Value {
    let mut out: Vec<Value> = allowed
        .iter()
        .filter(|name| selected.contains(name))
        .map(|name| Value::String((*name).to_string()))
        .collect();
    out.extend(
        current
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|name| !allowed.contains(name))
            .map(|name| Value::String(name.to_string())),
    );
    Value::Array(out)
}

/// 配置值里属于 `allowed` 的已启用项（候选顺序），用来同步多选下拉的选中状态。
///
/// # 参数
/// - `current`：当前配置值（非数组按空列表）。
/// - `allowed`：全部合法候选。
pub fn enabled_menu_options<'a>(current: &Value, allowed: &[&'a str]) -> Vec<&'a str> {
    let items: Vec<&str> = current
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    allowed
        .iter()
        .copied()
        .filter(|name| items.contains(name))
        .collect()
}

/// 托盘图标文件过滤器的匹配模式（如 `*.png;*.jpg`）。
pub fn icon_filter_pattern() -> String {
    ICON_EXTENSIONS
        .iter()
        .map(|ext| format!("*.{ext}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// 把对话框选中的路径转成要写回 `tray/custom_icon` 的配置值。
///
/// # 参数
/// - `path`：选中的文件路径。
pub fn picked_icon_value(path: &std::path::Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

/// 把 RGBA 图标着色成单色剪影（保留 alpha），用于 `light` / `dark` 样式。
///
/// # 参数
/// - `rgba`：RGBA 像素（就地修改）。
/// - `color`：目标 RGB。
pub fn tint_silhouette(rgba: &mut [u8], color: [u8; 3]) {
    for px in rgba.chunks_exact_mut(4) {
        px[0] = color[0];
        px[1] = color[1];
        px[2] = color[2];
    }
}

/// 解码并缩放一张图片到托盘图标大小；失败返回 `None`。
fn decode_icon(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    let resized = img
        .resize_exact(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    Some((resized.into_raw(), ICON_SIZE, ICON_SIZE))
}

/// 按配置生成托盘图标：`tray/custom_icon` 指向可读图片时优先；否则用内置图标，
/// `light` / `dark`（含 `snow-` 前缀）变体着成白 / 黑剪影。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 托盘图标；全部失败时退回纯色占位。
///
/// ```ignore
/// let icon = tray_icon_image(&document);
/// ```
pub fn tray_icon_image(document: &ConfigDocument) -> TrayIconImage {
    let style = document.value(KEY_ICON);
    let style = style.as_str().unwrap_or("default");
    let custom = document.value(KEY_CUSTOM_ICON);
    let custom = custom.as_str().map(str::trim).filter(|p| !p.is_empty());
    let decoded = custom
        .and_then(|path| match std::fs::read(path) {
            Ok(bytes) => decode_icon(&bytes),
            Err(e) => {
                tracing::warn!(path, error = %e, "读取自定义托盘图标失败，改用内置图标");
                None
            }
        })
        .or_else(|| decode_icon(BUILTIN_ICON_PNG));
    let Some((mut rgba, w, h)) = decoded else {
        return TrayIconImage::solid(ICON_SIZE, ICON_SIZE, [22, 119, 255, 255])
            .expect("纯色图标尺寸恒合法");
    };
    // 自定义图标保持原色；内置图标按样式着色
    if custom.is_none() {
        match style.trim_start_matches("snow-") {
            "light" => tint_silhouette(&mut rgba, [255, 255, 255]),
            "dark" => tint_silhouette(&mut rgba, [0, 0, 0]),
            _ => {}
        }
    }
    TrayIconImage::new(rgba, w, h).unwrap_or_else(|_| {
        TrayIconImage::solid(ICON_SIZE, ICON_SIZE, [22, 119, 255, 255]).expect("纯色图标尺寸恒合法")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_ui::shell::tray::TrayAction;

    /// 造一个只带标签的菜单项。
    fn item(label: &str) -> TrayMenuEntry {
        TrayMenuEntry::Item {
            label: label.into(),
            enabled: true,
            checked: None,
            icon: None,
            action: TrayAction::Signal(label.into()),
        }
    }

    /// 只留启用的项，并去掉开头 / 结尾 / 连续的分隔线。
    #[test]
    fn filter_keeps_enabled_and_normalizes_separators() {
        let enabled: HashSet<String> = ["a", "c", "e"].iter().map(|s| s.to_string()).collect();
        let entries = vec![
            (Some("b"), item("b")),
            (None, TrayMenuEntry::Separator),
            (Some("a"), item("a")),
            (None, TrayMenuEntry::Separator),
            (Some("d"), item("d")),
            (None, TrayMenuEntry::Separator),
            (Some("c"), item("c")),
            (None, TrayMenuEntry::Separator),
            (Some("e"), item("e")),
            (None, TrayMenuEntry::Separator),
        ];
        let out = filter_menu(entries, &enabled);
        let labels: Vec<String> = out
            .iter()
            .map(|e| match e {
                TrayMenuEntry::Item { label, .. } => label.clone(),
                TrayMenuEntry::Separator => "-".into(),
            })
            .collect();
        assert_eq!(labels, ["a", "-", "c", "-", "e"]);
    }

    /// 点击动作解析：默认值、非法值回退。
    #[test]
    fn click_actions_parse_with_fallback() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(
            click_action(&doc, KEY_LEFT_CLICK, TrayClick::ShowMainWindow),
            TrayClick::Screenshot
        );
        assert_eq!(
            click_action(&doc, KEY_MIDDLE_CLICK, TrayClick::Screenshot),
            TrayClick::ScreenshotFixed
        );
        doc.set_value(KEY_LEFT_CLICK, json!("open_function_settings"))
            .unwrap();
        assert_eq!(
            click_action(&doc, KEY_LEFT_CLICK, TrayClick::Screenshot),
            TrayClick::OpenFunctionSettings
        );
        assert_eq!(TrayClick::parse("nope"), None);
    }

    /// 默认配置：托盘启用，菜单项集合含默认 12 项。
    #[test]
    fn defaults_enable_tray_and_menu_options() {
        let doc = ConfigDocument::from_bytes(None);
        assert!(tray_enabled(&doc));
        let options = menu_options(&doc);
        assert_eq!(options.len(), 12);
        assert!(options.contains("tray.exit") && options.contains("quick.screenshot"));
        let mut off = ConfigDocument::from_bytes(None);
        off.set_value(KEY_ENABLED, json!(false)).unwrap();
        assert!(!tray_enabled(&off));
    }

    /// 选中集合转配置值：保留未知值、按候选顺序输出、空选中只剩未知值。
    #[test]
    fn selection_keeps_unknown_and_order() {
        let allowed = ["a", "b", "c"];
        assert_eq!(
            menu_options_from_selection(&json!(["c", "legacy", "a"]), &allowed, &["c", "b", "a"]),
            json!(["a", "b", "c", "legacy"])
        );
        assert_eq!(
            menu_options_from_selection(&json!(["a", "legacy"]), &allowed, &[]),
            json!(["legacy"])
        );
        assert_eq!(
            menu_options_from_selection(&json!(null), &allowed, &["c"]),
            json!(["c"])
        );
    }

    /// 已启用项只取候选内的值并按候选顺序，非数组为空。
    #[test]
    fn enabled_options_follow_allowed_order() {
        let allowed = ["a", "b", "c"];
        assert_eq!(
            enabled_menu_options(&json!(["c", "legacy", "a"]), &allowed),
            vec!["a", "c"]
        );
        assert!(enabled_menu_options(&json!("x"), &allowed).is_empty());
    }

    /// 默认值逐项对应 schema 候选，经勾选往返后与旧配置序列化一致（顺序按候选）。
    #[test]
    fn menu_options_round_trip_through_document() {
        let mut doc = ConfigDocument::from_bytes(None);
        let allowed = snow_config::schema::entry_for(KEY_MENU_OPTIONS)
            .unwrap()
            .allowed;
        let default = doc.value(KEY_MENU_OPTIONS);
        let without_exit: Vec<&str> = enabled_menu_options(&default, allowed)
            .into_iter()
            .filter(|name| *name != "tray.exit")
            .collect();
        let next = menu_options_from_selection(&default, allowed, &without_exit);
        doc.set_value(KEY_MENU_OPTIONS, next).unwrap();
        assert!(!menu_options(&doc).contains("tray.exit") && menu_options(&doc).len() == 11);
        let mut with_exit = enabled_menu_options(&doc.value(KEY_MENU_OPTIONS), allowed);
        with_exit.push("tray.exit");
        let back = menu_options_from_selection(&doc.value(KEY_MENU_OPTIONS), allowed, &with_exit);
        assert_eq!(back, default);
        let mut more = enabled_menu_options(&default, allowed);
        more.push("tray.restart-app");
        let enabled = menu_options_from_selection(&default, allowed, &more);
        doc.set_value(KEY_MENU_OPTIONS, enabled).unwrap();
        assert!(menu_options(&doc).contains("tray.restart-app"));
    }

    /// 过滤器扩展名表与匹配模式；选中路径写回配置并可读回。
    #[test]
    fn icon_filter_and_picked_value() {
        assert_eq!(icon_filter_pattern(), "*.png;*.jpg;*.jpeg;*.webp;*.bmp");
        let mut doc = ConfigDocument::from_bytes(None);
        let value = picked_icon_value(std::path::Path::new("C:/icons/my.png"));
        doc.set_value(KEY_CUSTOM_ICON, value).unwrap();
        assert_eq!(doc.value(KEY_CUSTOM_ICON), json!("C:/icons/my.png"));
    }

    /// 内置图标可解码；light / dark 变体是单色剪影；自定义路径不存在时回退内置图标。
    #[test]
    fn icon_styles() {
        let default_icon = decode_icon(BUILTIN_ICON_PNG).expect("内置图标应可解码");
        assert_eq!((default_icon.1, default_icon.2), (ICON_SIZE, ICON_SIZE));
        assert!(default_icon.0.chunks_exact(4).any(|p| p[3] > 0));
        let mut silhouette = default_icon.0.clone();
        tint_silhouette(&mut silhouette, [255, 255, 255]);
        assert!(
            silhouette
                .chunks_exact(4)
                .all(|p| p[..3] == [255, 255, 255])
        );
        assert_eq!(
            silhouette.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>(),
            default_icon
                .0
                .chunks_exact(4)
                .map(|p| p[3])
                .collect::<Vec<_>>(),
            "着色保留 alpha"
        );
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_ICON, json!("snow-dark")).unwrap();
        doc.set_value(KEY_CUSTOM_ICON, json!("Z:/definitely/missing.png"))
            .unwrap();
        let _ = tray_icon_image(&doc);
    }
}
