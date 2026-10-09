//! 应用资源源：Lucide 默认包 + antd 单色图标 + 自绘线性图标。
//!
//! 路径约定：
//! - `icons/antd/<名称>.svg`：`snow-ui-icons` 里 antd outlined 图标的单色层（黑色，GPUI 当遮罩着色）。
//! - `icons/snow/<名称>.svg`：本 crate `assets/icons/snow/` 自绘图标（24x24、描边 1.75、线性）。
//! - 其余路径交给 gpui-kit 自带的 Lucide 包。
//!
//! 全部按需加载，不常驻位图；GPUI 会按尺寸缓存光栅化结果。

use gpui_kit::{AssetSource, Result, SharedString};
use snow_ui_icons::{IconTheme, mask_layers};
use std::borrow::Cow;

/// antd 图标路径前缀。
pub const ANTD_ICON_PREFIX: &str = "icons/antd/";
/// 自绘图标路径前缀。
pub const OWN_ICON_PREFIX: &str = "icons/snow/";
/// 图标文件扩展名。
const ICON_EXT: &str = ".svg";

/// 自绘图标表：（名称，SVG 文本）。
const OWN_ICONS: &[(&str, &str)] = &[
    ("ellipse", include_str!("../assets/icons/snow/ellipse.svg")),
    ("arrow", include_str!("../assets/icons/snow/arrow.svg")),
    ("line", include_str!("../assets/icons/snow/line.svg")),
    ("blur", include_str!("../assets/icons/snow/blur.svg")),
    ("check", include_str!("../assets/icons/snow/check.svg")),
    (
        "region-polyline",
        include_str!("../assets/icons/snow/region-polyline.svg"),
    ),
    (
        "region-curve",
        include_str!("../assets/icons/snow/region-curve.svg"),
    ),
    (
        "region-freehand",
        include_str!("../assets/icons/snow/region-freehand.svg"),
    ),
    (
        "scroll-capture",
        include_str!("../assets/icons/snow/scroll-capture.svg"),
    ),
];

/// 取 `prefix` 与 `.svg` 之间的图标名；路径不符合约定返回 `None`。
fn icon_name<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)?.strip_suffix(ICON_EXT)
}

/// 读取自绘图标的 SVG 文本。
fn own_icon(name: &str) -> Option<&'static str> {
    OWN_ICONS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// 应用资源源：在 gpui-kit 默认 Lucide 包之上叠加 antd 与自绘图标。
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    /// 按路径加载资源；antd / 自绘前缀命中返回内容，未命中返回 `None`，其余转给 Lucide 包。
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(name) = icon_name(path, ANTD_ICON_PREFIX) {
            return Ok(mask_layers(IconTheme::Outlined, name)
                .map(|layers| Cow::Owned(layers.primary.into_bytes())));
        }
        if let Some(name) = icon_name(path, OWN_ICON_PREFIX) {
            return Ok(own_icon(name).map(|svg| Cow::Borrowed(svg.as_bytes())));
        }
        AssetSource::load(&gpui_kit::assets::Assets, path)
    }

    /// 列出前缀下的资源；只列 Lucide 与自绘图标（antd 数量大，按名字直接加载）。
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = AssetSource::list(&gpui_kit::assets::Assets, path)?;
        all.extend(
            OWN_ICONS
                .iter()
                .map(|(name, _)| format!("{OWN_ICON_PREFIX}{name}{ICON_EXT}"))
                .filter(|p| p.starts_with(path))
                .map(SharedString::from),
        );
        Ok(all)
    }
}

/// 资源路径能否解析到内容（供各视图 crate 的图标清单测试使用）。
///
/// # 参数
/// - `path`：资源路径，如 `icons/antd/edit.svg`。
///
/// # 返回
/// 能加载且内容是 SVG 时为 `true`。
///
/// # 示例
/// ```
/// assert!(snow_ui_shell::ui::icon_asset_exists("icons/antd/edit.svg"));
/// assert!(!snow_ui_shell::ui::icon_asset_exists("icons/antd/no-such.svg"));
/// ```
pub fn icon_asset_exists(path: &str) -> bool {
    matches!(AppAssets.load(path), Ok(Some(data)) if data.starts_with(b"<svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自绘图标全部可加载，且是描边线性 SVG。
    #[test]
    fn own_icons_load() {
        for (name, _) in OWN_ICONS {
            let path = format!("{OWN_ICON_PREFIX}{name}{ICON_EXT}");
            let data = AppAssets.load(&path).unwrap().expect("自绘图标缺失");
            let text = String::from_utf8(data.into_owned()).unwrap();
            assert!(text.contains("viewBox=\"0 0 24 24\""), "{name}");
            assert!(text.contains("stroke-width="), "{name}");
        }
    }

    /// antd 图标按名加载，未知名字与 Lucide 回退行为正确。
    #[test]
    fn antd_and_fallback() {
        assert!(icon_asset_exists("icons/antd/undo.svg"));
        assert!(!icon_asset_exists("icons/antd/__none__.svg"));
        assert!(icon_asset_exists("icons/chevron-down.svg"));
        assert!(AppAssets.load("").unwrap().is_none());
    }

    /// list 同时含 Lucide 与自绘图标。
    #[test]
    fn list_includes_own() {
        let listed = AppAssets.list("icons/").unwrap();
        assert!(listed.iter().any(|p| p.as_ref() == "icons/check.svg"));
        assert!(listed.iter().any(|p| p.as_ref() == "icons/snow/blur.svg"));
    }
}
