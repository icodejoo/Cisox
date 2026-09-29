//! 资源访问：编译期嵌入的 SVG 模板表、查找与单色分层。

use crate::model::IconTheme;

include!(concat!(env!("OUT_DIR"), "/icon_table.rs"));

/// 主色占位符（与 C++ 生成器一致）。
pub(crate) const PRIMARY_PLACEHOLDER: &str = "__ADQT_SLOT_PRIMARY__";
/// 次色占位符。
pub(crate) const SECONDARY_PLACEHOLDER: &str = "__ADQT_SLOT_SECONDARY__";

/// 按主题与名称查找 SVG 模板；`theme.dir()` 与 `name` 需完全匹配。
pub(crate) fn find_template(theme: IconTheme, name: &str) -> Option<&'static str> {
    let dir = theme.dir();
    TEMPLATES
        .binary_search_by(|(t, n, _)| (*t, *n).cmp(&(dir, name)))
        .ok()
        .map(|i| TEMPLATES[i].2)
}

/// 取规范化后的 SVG 模板文本（含 `__ADQT_SLOT_*__` 颜色占位符）。
///
/// # 参数
/// - `theme`/`name`: 图标标识。
///
/// # 返回
/// 图标不存在时为 `None`。
///
/// # 示例
/// ```
/// use snow_ui_icons::{IconTheme, template_svg};
/// assert!(template_svg(IconTheme::Outlined, "setting").unwrap().contains("__ADQT_SLOT_PRIMARY__"));
/// ```
pub fn template_svg(theme: IconTheme, name: &str) -> Option<&'static str> {
    find_template(theme, name)
}

/// 返回内置图标总数（三个主题合计）。
///
/// # 返回
/// 嵌入的模板数量。
pub fn icon_count() -> usize {
    TEMPLATES.len()
}

/// 列出某主题下的全部图标名称（按字典序）。
///
/// # 参数
/// - `theme`: 图标主题。
///
/// # 示例
/// ```
/// use snow_ui_icons::{IconTheme, icon_names};
/// assert!(icon_names(IconTheme::Filled).any(|n| n == "camera"));
/// ```
pub fn icon_names(theme: IconTheme) -> impl Iterator<Item = &'static str> {
    let dir = theme.dir();
    TEMPLATES
        .iter()
        .filter(move |(t, _, _)| *t == dir)
        .map(|(_, n, _)| *n)
}

/// 单色分层结果：每层都是全黑单色 SVG，供 GPUI 的 alpha mask 渲染后叠加着色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconLayers {
    /// 主色层 SVG。
    pub primary: String,
    /// 次色层 SVG，非双色图标为 `None`。
    pub secondary: Option<String>,
}

/// 把图标拆成单色层（V7 结论：GPUI 只支持单色 mask，双色须分层叠加）。
///
/// # 参数
/// - `theme`/`name`: 图标标识。
///
/// # 返回
/// 图标不存在时为 `None`；单色图标只有主色层。
///
/// # 示例
/// ```
/// use snow_ui_icons::{IconTheme, mask_layers};
/// let l = mask_layers(IconTheme::TwoTone, "bell").unwrap();
/// assert!(l.secondary.is_some());
/// ```
pub fn mask_layers(theme: IconTheme, name: &str) -> Option<IconLayers> {
    let tpl = find_template(theme, name)?;
    if !tpl.contains(SECONDARY_PLACEHOLDER) {
        return Some(IconLayers {
            primary: blacken(tpl),
            secondary: None,
        });
    }
    Some(IconLayers {
        primary: blacken(&filter_elements(tpl, false)),
        secondary: Some(blacken(&filter_elements(tpl, true))),
    })
}

/// 把所有占位符替换为纯黑。
fn blacken(svg: &str) -> String {
    svg.replace(PRIMARY_PLACEHOLDER, "#000000")
        .replace(SECONDARY_PLACEHOLDER, "#000000")
}

/// 保留（`keep_secondary`）或剔除带次色占位符的 `<path>` 元素；双色模板只含 svg + path。
fn filter_elements(svg: &str, keep_secondary: bool) -> String {
    let mut parts = svg.split("<path");
    let mut out = String::from(parts.next().unwrap_or_default());
    for part in parts {
        let (elem, rest) = match part.find("/>") {
            Some(i) => part.split_at(i + 2),
            None => (part, ""),
        };
        if elem.contains(SECONDARY_PLACEHOLDER) == keep_secondary {
            out.push_str("<path");
            out.push_str(elem);
        }
        out.push_str(rest);
    }
    out
}
