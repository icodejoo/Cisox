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

/// 托盘图标边长（像素）。
pub const ICON_SIZE: u32 = 32;
/// 内置图标（128 像素 PNG，取自旧版应用图标）。
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
    document.value(key).as_str().and_then(TrayClick::parse).unwrap_or(default)
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
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
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
pub fn filter_menu(entries: Vec<(Option<&'static str>, TrayMenuEntry)>, enabled: &HashSet<String>) -> Vec<TrayMenuEntry> {
    let mut out: Vec<TrayMenuEntry> = Vec::new();
    for (id, entry) in entries {
        if id.is_some_and(|id| !enabled.contains(id)) {
            continue;
        }
        if matches!(entry, TrayMenuEntry::Separator) && matches!(out.last(), None | Some(TrayMenuEntry::Separator)) {
            continue;
        }
        out.push(entry);
    }
    while matches!(out.last(), Some(TrayMenuEntry::Separator)) {
        out.pop();
    }
    out
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
    let resized = img.resize_exact(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3).to_rgba8();
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
        return TrayIconImage::solid(ICON_SIZE, ICON_SIZE, [22, 119, 255, 255]).expect("纯色图标尺寸恒合法");
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
        assert_eq!(click_action(&doc, KEY_LEFT_CLICK, TrayClick::ShowMainWindow), TrayClick::Screenshot);
        assert_eq!(click_action(&doc, KEY_MIDDLE_CLICK, TrayClick::Screenshot), TrayClick::ScreenshotFixed);
        doc.set_value(KEY_LEFT_CLICK, json!("open_function_settings")).unwrap();
        assert_eq!(click_action(&doc, KEY_LEFT_CLICK, TrayClick::Screenshot), TrayClick::OpenFunctionSettings);
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

    /// 内置图标可解码；light / dark 变体是单色剪影；自定义路径不存在时回退内置图标。
    #[test]
    fn icon_styles() {
        let default_icon = decode_icon(BUILTIN_ICON_PNG).expect("内置图标应可解码");
        assert_eq!((default_icon.1, default_icon.2), (ICON_SIZE, ICON_SIZE));
        assert!(default_icon.0.chunks_exact(4).any(|p| p[3] > 0));
        let mut silhouette = default_icon.0.clone();
        tint_silhouette(&mut silhouette, [255, 255, 255]);
        assert!(silhouette.chunks_exact(4).all(|p| p[..3] == [255, 255, 255]));
        assert_eq!(
            silhouette.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>(),
            default_icon.0.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>(),
            "着色保留 alpha"
        );
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_ICON, json!("snow-dark")).unwrap();
        doc.set_value(KEY_CUSTOM_ICON, json!("Z:/definitely/missing.png")).unwrap();
        let _ = tray_icon_image(&doc);
    }
}
