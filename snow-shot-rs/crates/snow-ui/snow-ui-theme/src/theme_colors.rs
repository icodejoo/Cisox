//! 颜色令牌推导：移植自 `theme_types.cpp` 的亮/暗色令牌与语义色，及 `generateMappedPalette`。

use crate::color::{Color, alpha_color, composite_on, mix_color, solid_color};
use crate::palette::{generate_palette, map_palette};
use crate::tokens::{ThemeColors, ThemeConfig, ThemeSemanticPalette};

/// 默认主色（无效时回退）。
const FALLBACK_PRIMARY: &str = "#1677ff";
/// 暗色主题混合背景。
const DARK_MIX_BACKGROUND: &str = "#141414";

/// 解析十六进制字面量，失败给全透明黑（字面量均合法，仅作兜底）。
fn hex(s: &str) -> Color {
    Color::from_hex(s).unwrap_or_default()
}

/// 取色阶 `index`，槽位无效时用 `fallback`（对应 C++ `tone`）。
fn tone(mapped: &[Option<Color>; 11], index: usize, fallback: Color) -> Color {
    mapped.get(index).copied().flatten().unwrap_or(fallback)
}

/// 生成 1..=10 号色阶（`generateMappedPalette`，下标 0 恒为 None）。
///
/// 参数：`base` 基色（None 时用 `#1677ff`）；`dark_theme` 是否暗色；`background` 暗色混合背景（None 用默认）。
/// 返回：长度 11 的色阶，无效槽为 None。
///
/// ```rust
/// use snow_ui_theme::color::Color;
/// use snow_ui_theme::theme_colors::generate_mapped_palette;
/// let m = generate_mapped_palette(Color::from_hex("#1677ff"), false, None);
/// assert_eq!(m[6].unwrap().name(), "#1677ff");
/// ```
pub fn generate_mapped_palette(
    base: Option<Color>,
    dark_theme: bool,
    background: Option<Color>,
) -> [Option<Color>; 11] {
    let resolved_base = base.unwrap_or_else(|| hex(FALLBACK_PRIMARY));
    let background_value = background.map(|c| c.name()).unwrap_or_default();
    let raw = generate_palette(&resolved_base.name(), dark_theme, &background_value);
    let mapped_raw = map_palette(&raw, dark_theme);
    let mut mapped = [None; 11];
    for (slot, text) in mapped.iter_mut().zip(mapped_raw.iter()) {
        if !text.is_empty() {
            *slot = Color::from_hex(text);
        }
    }
    mapped
}

/// 由颜色令牌推导语义调色板（`makeSemanticPalette`）。
///
/// 参数：`colors` 颜色令牌。返回：语义调色板。
///
/// ```rust
/// use snow_ui_theme::tokens::*;
/// let t = make_theme(&default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable));
/// assert_eq!(t.semantic.window.name(), "#f5f5f5");
/// ```
pub fn make_semantic_palette(colors: &ThemeColors) -> ThemeSemanticPalette {
    let over = |fg: Color, bg: Color| composite_on(fg, bg, 1.0);
    let window = over(colors.color_bg_layout, colors.color_bg_base);
    let surface = over(colors.color_bg_container, window);
    let surface_disabled = over(colors.color_bg_container_disabled, surface);
    let surface_elevated = over(colors.color_bg_elevated, window);
    let surface_subtle = over(colors.color_fill_alter, surface);

    ThemeSemanticPalette {
        window,
        window_disabled: window,
        surface,
        surface_disabled,
        surface_elevated,
        surface_subtle,
        surface_solid: over(colors.color_bg_solid, surface),
        surface_solid_hover: over(colors.color_bg_solid_hover, surface),
        surface_solid_active: over(colors.color_bg_solid_active, surface),
        surface_spotlight: over(colors.color_bg_spotlight, window),
        mask: colors.color_bg_mask,
        fill: colors.color_fill,
        fill_secondary: colors.color_fill_secondary,
        fill_tertiary: colors.color_fill_tertiary,
        fill_quaternary: colors.color_fill_quaternary,
        text: colors.color_text,
        text_secondary: colors.color_text_secondary,
        text_tertiary: colors.color_text_tertiary,
        text_quaternary: colors.color_text_quaternary,
        text_disabled: colors.color_text_disabled,
        text_placeholder: colors.color_text_placeholder,
        text_on_accent: colors.color_text_light_solid,
        border: over(colors.color_border, surface),
        border_disabled: over(colors.color_border_disabled, surface_disabled),
        border_secondary: over(colors.color_border_secondary, surface),
        accent: colors.color_primary,
        accent_hover: colors.color_primary_hover,
        accent_active: colors.color_primary_active,
        accent_subtle: over(colors.color_primary_bg, surface),
        accent_subtle_hover: over(colors.color_primary_bg_hover, surface),
        accent_border: colors.color_primary_border,
        accent_border_hover: colors.color_primary_border_hover,
        accent_solid: colors.color_primary_solid,
        accent_solid_hover: colors.color_primary_solid_hover,
        accent_solid_active: colors.color_primary_solid_active,
        accent_solid_text: colors.color_primary_solid_text,
        accent_disabled: composite_on(colors.color_primary, surface_disabled, 0.38),
        success: colors.color_success,
        success_hover: colors.color_success_hover,
        success_active: colors.color_success_active,
        success_subtle: over(colors.color_success_bg, surface),
        success_border: colors.color_success_border,
        warning: colors.color_warning,
        warning_hover: colors.color_warning_hover,
        warning_active: colors.color_warning_active,
        warning_subtle: over(colors.color_warning_bg, surface),
        warning_border: colors.color_warning_border,
        error: colors.color_error,
        error_hover: colors.color_error_hover,
        error_active: colors.color_error_active,
        error_subtle: over(colors.color_error_bg, surface),
        error_border: colors.color_error_border,
        info: colors.color_info,
        info_hover: colors.color_info_hover,
        info_active: colors.color_info_active,
        info_subtle: over(colors.color_info_bg, surface),
        info_border: colors.color_info_border,
        link: colors.color_link,
        link_hover: colors.color_link_hover,
        link_active: colors.color_link_active,
        white: colors.color_white,
    }
}

/// 六个语义基色（主/成功/错误/警告/信息/链接）。
struct SemanticBases {
    primary: Color,
    success: Color,
    error: Color,
    warning: Color,
    info: Color,
    link: Color,
}

/// 由配置解出六个基色（嵌套 `validOr` 回退）。
fn resolve_bases(config: &ThemeConfig) -> SemanticBases {
    let primary = config
        .primary
        .or(config.accents.blue)
        .unwrap_or_else(|| hex(FALLBACK_PRIMARY));
    SemanticBases {
        primary,
        success: config
            .success
            .or(config.accents.green)
            .unwrap_or_else(|| hex("#52c41a")),
        error: config.error.unwrap_or_else(|| hex("#ff4d4f")),
        warning: config.warning.unwrap_or_else(|| hex("#faad14")),
        info: config.info.unwrap_or(primary),
        link: config.link.unwrap_or(primary),
    }
}

/// 应用亮色主题的颜色令牌（`applyLightSemanticColors`）。
///
/// 参数：`colors` 被写入的颜色令牌；`config` 主题配置。
///
/// ```rust
/// use snow_ui_theme::tokens::*;
/// use snow_ui_theme::theme_colors::apply_light_semantic_colors;
/// let mut colors = ThemeColors::default();
/// apply_light_semantic_colors(&mut colors, &default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable));
/// assert_eq!(colors.color_primary.name(), "#1677ff");
/// ```
pub fn apply_light_semantic_colors(colors: &mut ThemeColors, config: &ThemeConfig) {
    let b = resolve_bases(config);
    let bg_base = Color::WHITE;
    let text_base = Color::BLACK;

    let primary = generate_mapped_palette(Some(b.primary), false, None);
    let success = generate_mapped_palette(Some(b.success), false, None);
    let error = generate_mapped_palette(Some(b.error), false, None);
    let warning = generate_mapped_palette(Some(b.warning), false, None);
    let info = generate_mapped_palette(Some(b.info), false, None);
    let link = generate_mapped_palette(Some(b.link), false, None);

    colors.color_bg_base = bg_base;
    colors.color_text_base = text_base;
    colors.color_text = alpha_color(text_base, 0.88);
    colors.color_text_secondary = alpha_color(text_base, 0.65);
    colors.color_text_tertiary = alpha_color(text_base, 0.45);
    colors.color_text_quaternary = alpha_color(text_base, 0.25);
    colors.color_text_disabled = colors.color_text_quaternary;
    colors.color_text_placeholder = colors.color_text_quaternary;
    colors.color_text_light_solid = Color::WHITE;

    colors.color_fill = alpha_color(text_base, 0.15);
    colors.color_fill_secondary = alpha_color(text_base, 0.06);
    colors.color_fill_tertiary = alpha_color(text_base, 0.04);
    colors.color_fill_quaternary = alpha_color(text_base, 0.02);
    colors.color_fill_alter = colors.color_fill_quaternary;

    colors.color_bg_solid = alpha_color(text_base, 1.0);
    colors.color_bg_solid_hover = alpha_color(text_base, 0.75);
    colors.color_bg_solid_active = alpha_color(text_base, 0.95);
    colors.color_bg_layout = solid_color(bg_base, 4.0, false);
    colors.color_bg_container = solid_color(bg_base, 0.0, false);
    colors.color_bg_container_disabled = colors.color_fill_tertiary;
    colors.color_bg_elevated = solid_color(bg_base, 0.0, false);
    colors.color_bg_spotlight = alpha_color(text_base, 0.85);
    colors.color_bg_blur = Color::TRANSPARENT;

    colors.color_border = solid_color(bg_base, 15.0, false);
    colors.color_border_disabled = colors.color_border;
    colors.color_border_secondary = solid_color(bg_base, 6.0, false);

    colors.color_primary_bg = tone(&primary, 1, hex("#e6f4ff"));
    colors.color_primary_bg_hover = tone(&primary, 2, hex("#bae0ff"));
    colors.color_primary_border = tone(&primary, 3, hex("#91caff"));
    colors.color_primary_border_hover = tone(&primary, 4, hex("#69b1ff"));
    colors.color_primary_hover = tone(&primary, 5, hex("#4096ff"));
    colors.color_primary = tone(&primary, 6, b.primary);
    colors.color_primary_active = tone(&primary, 7, hex("#0958d9"));
    colors.color_primary_solid = colors.color_primary;
    colors.color_primary_solid_hover = colors.color_primary_hover;
    colors.color_primary_solid_active = colors.color_primary_active;
    colors.color_primary_solid_text = colors.color_text_light_solid;
    colors.color_primary_text_hover = colors.color_primary_hover;
    colors.color_primary_text = colors.color_primary;
    colors.color_primary_text_active = colors.color_primary_active;

    colors.color_success_bg = tone(&success, 1, hex("#f6ffed"));
    colors.color_success_bg_hover = tone(&success, 2, hex("#d9f7be"));
    colors.color_success_border = tone(&success, 3, hex("#b7eb8f"));
    colors.color_success_border_hover = tone(&success, 4, hex("#95de64"));
    colors.color_success_hover = tone(&success, 4, hex("#95de64"));
    colors.color_success = tone(&success, 6, b.success);
    colors.color_success_active = tone(&success, 7, hex("#389e0d"));
    colors.color_success_solid = colors.color_success;
    colors.color_success_solid_hover = colors.color_success_hover;
    colors.color_success_solid_active = colors.color_success_active;
    colors.color_success_solid_text = colors.color_text_light_solid;
    colors.color_success_text_hover = tone(&success, 8, hex("#73d13d"));
    colors.color_success_text = colors.color_success;
    colors.color_success_text_active = colors.color_success_active;

    colors.color_error_bg = tone(&error, 1, hex("#fff2f0"));
    colors.color_error_bg_hover = tone(&error, 2, hex("#fff1f0"));
    colors.color_error_bg_active = tone(&error, 3, hex("#ffccc7"));
    colors.color_error_bg_filled_hover =
        mix_color(colors.color_error_bg, colors.color_error_bg_active, 50.0);
    colors.color_error_border = tone(&error, 3, hex("#ffccc7"));
    colors.color_error_border_hover = tone(&error, 4, hex("#ff7875"));
    colors.color_error_hover = tone(&error, 5, hex("#ff7875"));
    colors.color_error = tone(&error, 6, b.error);
    colors.color_error_active = tone(&error, 7, hex("#cf1322"));
    colors.color_error_solid = colors.color_error;
    colors.color_error_solid_hover = colors.color_error_hover;
    colors.color_error_solid_active = colors.color_error_active;
    colors.color_error_solid_text = colors.color_text_light_solid;
    colors.color_error_text_hover = tone(&error, 8, hex("#ff7875"));
    colors.color_error_text = colors.color_error;
    colors.color_error_text_active = colors.color_error_active;

    colors.color_warning_bg = tone(&warning, 1, hex("#fffbe6"));
    colors.color_warning_bg_hover = tone(&warning, 2, hex("#fff1b8"));
    colors.color_warning_border = tone(&warning, 3, hex("#ffe58f"));
    colors.color_warning_border_hover = tone(&warning, 4, hex("#ffd666"));
    colors.color_warning_hover = tone(&warning, 4, hex("#ffd666"));
    colors.color_warning = tone(&warning, 6, b.warning);
    colors.color_warning_active = tone(&warning, 7, hex("#d48806"));
    colors.color_warning_solid = colors.color_warning;
    colors.color_warning_solid_hover = colors.color_warning_hover;
    colors.color_warning_solid_active = colors.color_warning_active;
    colors.color_warning_solid_text = colors.color_text_light_solid;
    colors.color_warning_text_hover = tone(&warning, 8, hex("#ffc53d"));
    colors.color_warning_text = colors.color_warning;
    colors.color_warning_text_active = colors.color_warning_active;

    apply_info_and_link(colors, &info, &link, &b, false);
    colors.color_bg_mask = alpha_color(Color::BLACK, 0.45);
    colors.color_white = Color::WHITE;
}

/// 应用暗色主题的颜色令牌（`applyDarkSemanticColors`）。
///
/// 参数：`colors` 被写入的颜色令牌；`config` 主题配置。
///
/// ```rust
/// use snow_ui_theme::tokens::*;
/// use snow_ui_theme::theme_colors::apply_dark_semantic_colors;
/// let mut colors = ThemeColors::default();
/// apply_dark_semantic_colors(&mut colors, &default_theme_config(ThemeScheme::Dark, ThemeDensity::Comfortable));
/// assert_eq!(colors.color_bg_container.name(), "#141414");
/// ```
pub fn apply_dark_semantic_colors(colors: &mut ThemeColors, config: &ThemeConfig) {
    let b = resolve_bases(config);
    let dark_mix_background = hex(DARK_MIX_BACKGROUND);
    let bg_base = hex("#000000");
    let text_base = Color::WHITE;

    let mapped = |base: Color| generate_mapped_palette(Some(base), true, Some(dark_mix_background));
    let primary = mapped(b.primary);
    let success = mapped(b.success);
    let error = mapped(b.error);
    let warning = mapped(b.warning);
    let info = mapped(b.info);
    let link = mapped(b.link);

    colors.color_bg_base = bg_base;
    colors.color_text_base = text_base;
    colors.color_text = alpha_color(text_base, 0.85);
    colors.color_text_secondary = alpha_color(text_base, 0.65);
    colors.color_text_tertiary = alpha_color(text_base, 0.45);
    colors.color_text_quaternary = alpha_color(text_base, 0.25);
    colors.color_text_disabled = colors.color_text_quaternary;
    colors.color_text_placeholder = colors.color_text_quaternary;
    colors.color_text_light_solid = Color::WHITE;

    colors.color_fill = alpha_color(text_base, 0.18);
    colors.color_fill_secondary = alpha_color(text_base, 0.12);
    colors.color_fill_tertiary = alpha_color(text_base, 0.08);
    colors.color_fill_quaternary = alpha_color(text_base, 0.04);
    colors.color_fill_alter = colors.color_fill_quaternary;

    colors.color_bg_solid = alpha_color(text_base, 0.95);
    colors.color_bg_solid_hover = alpha_color(text_base, 1.0);
    colors.color_bg_solid_active = alpha_color(text_base, 0.9);
    // 暗色中性色由黑色基底向灰色提亮得到，而不是继续压暗
    colors.color_bg_layout = solid_color(bg_base, 0.0, true);
    colors.color_bg_container = solid_color(bg_base, 8.0, true);
    colors.color_bg_container_disabled = colors.color_fill_tertiary;
    colors.color_bg_elevated = solid_color(bg_base, 12.0, true);
    colors.color_bg_spotlight = solid_color(bg_base, 26.0, true);
    colors.color_bg_blur = alpha_color(text_base, 0.04);

    colors.color_border = solid_color(bg_base, 26.0, true);
    colors.color_border_disabled = colors.color_border;
    colors.color_border_secondary = solid_color(bg_base, 19.0, true);

    colors.color_primary_bg = tone(&primary, 3, hex("#15325b"));
    colors.color_primary_bg_hover = tone(&primary, 4, hex("#15417e"));
    colors.color_primary_border = tone(&primary, 3, hex("#15325b"));
    colors.color_primary_border_hover = tone(&primary, 4, hex("#15417e"));
    colors.color_primary_hover = tone(&primary, 5, hex("#3c89e8"));
    colors.color_primary = tone(&primary, 6, b.primary);
    colors.color_primary_active = tone(&primary, 7, hex("#1554ad"));
    colors.color_primary_solid = colors.color_primary;
    colors.color_primary_solid_hover = colors.color_primary_hover;
    colors.color_primary_solid_active = colors.color_primary_active;
    colors.color_primary_solid_text = colors.color_text_light_solid;
    colors.color_primary_text_hover = colors.color_primary_hover;
    colors.color_primary_text = colors.color_primary;
    colors.color_primary_text_active = colors.color_primary_active;

    colors.color_success_bg = tone(&success, 1, hex("#162312"));
    colors.color_success_bg_hover = tone(&success, 2, hex("#1d3712"));
    colors.color_success_border = tone(&success, 3, hex("#274916"));
    colors.color_success_border_hover = tone(&success, 4, hex("#306317"));
    colors.color_success_hover = tone(&success, 4, hex("#306317"));
    colors.color_success = tone(&success, 6, b.success);
    colors.color_success_active = tone(&success, 7, hex("#3c8618"));
    colors.color_success_solid = colors.color_success;
    colors.color_success_solid_hover = colors.color_success_hover;
    colors.color_success_solid_active = colors.color_success_active;
    colors.color_success_solid_text = colors.color_text_light_solid;
    colors.color_success_text_hover = tone(&success, 8, hex("#6abe39"));
    colors.color_success_text = colors.color_success;
    colors.color_success_text_active = colors.color_success_active;

    colors.color_error_bg = tone(&error, 1, hex("#2a1215"));
    colors.color_error_bg_hover = tone(&error, 2, hex("#431418"));
    colors.color_error_bg_active = tone(&error, 3, hex("#58181c"));
    colors.color_error_bg_filled_hover =
        mix_color(colors.color_error_bg, colors.color_error_bg_active, 50.0);
    colors.color_error_border = tone(&error, 3, hex("#58181c"));
    colors.color_error_border_hover = tone(&error, 4, hex("#791a1f"));
    colors.color_error_hover = tone(&error, 5, hex("#e86b6b"));
    colors.color_error = tone(&error, 6, b.error);
    colors.color_error_active = tone(&error, 7, hex("#ad393a"));
    colors.color_error_solid = colors.color_error;
    colors.color_error_solid_hover = colors.color_error_hover;
    colors.color_error_solid_active = colors.color_error_active;
    colors.color_error_solid_text = colors.color_text_light_solid;
    colors.color_error_text_hover = tone(&error, 8, hex("#e86b6b"));
    colors.color_error_text = colors.color_error;
    colors.color_error_text_active = colors.color_error_active;

    colors.color_warning_bg = tone(&warning, 1, hex("#2b2111"));
    colors.color_warning_bg_hover = tone(&warning, 2, hex("#443111"));
    colors.color_warning_border = tone(&warning, 3, hex("#594214"));
    colors.color_warning_border_hover = tone(&warning, 4, hex("#7c5914"));
    colors.color_warning_hover = tone(&warning, 4, hex("#7c5914"));
    colors.color_warning = tone(&warning, 6, b.warning);
    colors.color_warning_active = tone(&warning, 7, hex("#ad7412"));
    colors.color_warning_solid = colors.color_warning;
    colors.color_warning_solid_hover = colors.color_warning_hover;
    colors.color_warning_solid_active = colors.color_warning_active;
    colors.color_warning_solid_text = colors.color_text_light_solid;
    colors.color_warning_text_hover = tone(&warning, 8, hex("#e8b339"));
    colors.color_warning_text = colors.color_warning;
    colors.color_warning_text_active = colors.color_warning_active;

    apply_info_and_link(colors, &info, &link, &b, true);
    colors.color_bg_mask = alpha_color(Color::BLACK, 0.45);
    colors.color_white = Color::WHITE;
}

/// 写入 info 与 link 两组令牌；亮/暗两份 C++ 仅 link 的回退色不同，用 `dark` 区分。
fn apply_info_and_link(
    colors: &mut ThemeColors,
    info: &[Option<Color>; 11],
    link: &[Option<Color>; 11],
    b: &SemanticBases,
    dark: bool,
) {
    colors.color_info_bg = tone(info, 1, colors.color_primary_bg);
    colors.color_info_bg_hover = tone(info, 2, colors.color_primary_bg_hover);
    colors.color_info_border = tone(info, 3, colors.color_primary_border);
    colors.color_info_border_hover = tone(info, 4, colors.color_primary_border_hover);
    colors.color_info_hover = tone(info, 4, colors.color_primary_border_hover);
    colors.color_info = tone(info, 6, b.info);
    colors.color_info_active = tone(info, 7, colors.color_primary_active);
    colors.color_info_solid = colors.color_info;
    colors.color_info_solid_hover = colors.color_info_hover;
    colors.color_info_solid_active = colors.color_info_active;
    colors.color_info_solid_text = colors.color_text_light_solid;
    colors.color_info_text_hover = tone(info, 8, colors.color_primary_hover);
    colors.color_info_text = colors.color_info;
    colors.color_info_text_active = colors.color_info_active;

    // 亮色 colorLink 回退到 linkBase，暗色回退到 colorPrimary
    let link_fallback = if dark { colors.color_primary } else { b.link };
    colors.color_link_hover = tone(link, 4, colors.color_primary_border_hover);
    colors.color_link = tone(link, 6, link_fallback);
    colors.color_link_active = tone(link, 7, colors.color_primary_active);
}
