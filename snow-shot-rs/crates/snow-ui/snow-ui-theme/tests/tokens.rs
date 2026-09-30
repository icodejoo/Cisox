//! 令牌层测试：颜色令牌以 C++ 黄金色板 + 手工推导的 Qt 通道语义为依据。
//!
//! 说明：令牌层（theme_types.cpp）依赖 QColor，没有可独立编译的 C++ 黄金输出；
//! 色阶类令牌用黄金色板（golden_dump 输出）按 C++ 映射表逐值核对，
//! 透明度类令牌按 Qt6 语义（16 位通道，8 位读取为 `>>8`）手工推导。

use snow_ui_theme::color::Color;
use snow_ui_theme::tokens::*;

/// C++ 黄金样本全文。
const GOLDEN: &str = include_str!("golden/palette_golden.txt");

/// 亮色映射：色阶号 1..=10 对应 10 阶色板下标（theme_color_utils.cpp `toMappedDefault`）。
const LIGHT_MAP: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 4, 5, 6];
/// 暗色映射（`toMappedDark`）。
const DARK_MAP: [usize; 10] = [0, 1, 2, 3, 6, 5, 4, 6, 5, 4];

/// 取黄金色板行（`base` + `mode` = light/dark）。
fn golden(base: &str, mode: &str) -> Vec<String> {
    let key = format!("P {base} {mode} ");
    let line = GOLDEN
        .lines()
        .find(|l| l.starts_with(&key))
        .expect("黄金色板缺失");
    line[key.len()..]
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// 取色阶号 `slot`（1..=10）对应的色值。
fn slot(pal: &[String], map: &[usize; 10], slot: usize) -> String {
    pal[map[slot - 1]].clone()
}

/// 默认配置生成主题。
fn theme(scheme: ThemeScheme, density: ThemeDensity) -> Theme {
    make_theme(&default_theme_config(scheme, density))
}

/// 亮色：主/成功/错误/警告四组色阶令牌与黄金色板一致。
#[test]
fn light_tone_tokens_match_golden() {
    let t = theme(ThemeScheme::Light, ThemeDensity::Comfortable);
    let c = &t.palette;
    let p = golden("#1677ff", "light");
    let s = golden("#52c41a", "light");
    let e = golden("#ff4d4f", "light");
    let w = golden("#faad14", "light");
    let m = &LIGHT_MAP;

    assert_eq!(c.color_primary_bg.name(), slot(&p, m, 1));
    assert_eq!(c.color_primary_bg_hover.name(), slot(&p, m, 2));
    assert_eq!(c.color_primary_border.name(), slot(&p, m, 3));
    assert_eq!(c.color_primary_border_hover.name(), slot(&p, m, 4));
    assert_eq!(c.color_primary_hover.name(), slot(&p, m, 5));
    assert_eq!(c.color_primary.name(), slot(&p, m, 6));
    assert_eq!(c.color_primary_active.name(), slot(&p, m, 7));
    assert_eq!(c.color_primary_solid.name(), slot(&p, m, 6));
    assert_eq!(c.color_primary_text_hover.name(), slot(&p, m, 5));

    assert_eq!(c.color_success_bg.name(), slot(&s, m, 1));
    assert_eq!(c.color_success_bg_hover.name(), slot(&s, m, 2));
    assert_eq!(c.color_success_border.name(), slot(&s, m, 3));
    assert_eq!(c.color_success_border_hover.name(), slot(&s, m, 4));
    assert_eq!(c.color_success_hover.name(), slot(&s, m, 4));
    assert_eq!(c.color_success.name(), slot(&s, m, 6));
    assert_eq!(c.color_success_active.name(), slot(&s, m, 7));
    assert_eq!(c.color_success_text_hover.name(), slot(&s, m, 8));

    assert_eq!(c.color_error_bg.name(), slot(&e, m, 1));
    assert_eq!(c.color_error_bg_hover.name(), slot(&e, m, 2));
    assert_eq!(c.color_error_bg_active.name(), slot(&e, m, 3));
    assert_eq!(c.color_error_border.name(), slot(&e, m, 3));
    assert_eq!(c.color_error_border_hover.name(), slot(&e, m, 4));
    assert_eq!(c.color_error_hover.name(), slot(&e, m, 5));
    assert_eq!(c.color_error.name(), slot(&e, m, 6));
    assert_eq!(c.color_error_active.name(), slot(&e, m, 7));
    assert_eq!(c.color_error_text_hover.name(), slot(&e, m, 8));

    assert_eq!(c.color_warning_bg.name(), slot(&w, m, 1));
    assert_eq!(c.color_warning_bg_hover.name(), slot(&w, m, 2));
    assert_eq!(c.color_warning_border.name(), slot(&w, m, 3));
    assert_eq!(c.color_warning_hover.name(), slot(&w, m, 4));
    assert_eq!(c.color_warning.name(), slot(&w, m, 6));
    assert_eq!(c.color_warning_active.name(), slot(&w, m, 7));
    assert_eq!(c.color_warning_text_hover.name(), slot(&w, m, 8));

    // info 默认取主色；link 默认也取主色
    assert_eq!(c.color_info.name(), slot(&p, m, 6));
    assert_eq!(c.color_info_bg.name(), slot(&p, m, 1));
    assert_eq!(c.color_link.name(), slot(&p, m, 6));
    assert_eq!(c.color_link_hover.name(), slot(&p, m, 4));
    assert_eq!(c.color_link_active.name(), slot(&p, m, 7));
}

/// 暗色：色阶令牌与黄金色板一致（混合背景 #141414）。
#[test]
fn dark_tone_tokens_match_golden() {
    let t = theme(ThemeScheme::Dark, ThemeDensity::Comfortable);
    let c = &t.palette;
    let p = golden("#1677ff", "dark");
    let s = golden("#52c41a", "dark");
    let e = golden("#ff4d4f", "dark");
    let w = golden("#faad14", "dark");
    let m = &DARK_MAP;

    assert_eq!(c.color_primary_bg.name(), slot(&p, m, 3));
    assert_eq!(c.color_primary_bg_hover.name(), slot(&p, m, 4));
    assert_eq!(c.color_primary_border.name(), slot(&p, m, 3));
    assert_eq!(c.color_primary_border_hover.name(), slot(&p, m, 4));
    assert_eq!(c.color_primary_hover.name(), slot(&p, m, 5));
    assert_eq!(c.color_primary.name(), slot(&p, m, 6));
    assert_eq!(c.color_primary_active.name(), slot(&p, m, 7));

    assert_eq!(c.color_success_bg.name(), slot(&s, m, 1));
    assert_eq!(c.color_success_border_hover.name(), slot(&s, m, 4));
    assert_eq!(c.color_success.name(), slot(&s, m, 6));
    assert_eq!(c.color_success_active.name(), slot(&s, m, 7));
    assert_eq!(c.color_success_text_hover.name(), slot(&s, m, 8));

    assert_eq!(c.color_error_bg.name(), slot(&e, m, 1));
    assert_eq!(c.color_error_bg_hover.name(), slot(&e, m, 2));
    assert_eq!(c.color_error_bg_active.name(), slot(&e, m, 3));
    assert_eq!(c.color_error_hover.name(), slot(&e, m, 5));
    assert_eq!(c.color_error.name(), slot(&e, m, 6));
    assert_eq!(c.color_error_text_hover.name(), slot(&e, m, 8));

    assert_eq!(c.color_warning_bg.name(), slot(&w, m, 1));
    assert_eq!(c.color_warning_hover.name(), slot(&w, m, 4));
    assert_eq!(c.color_warning.name(), slot(&w, m, 6));

    // 暗色 link 回退用 colorPrimary，但色阶存在时取 link 色阶
    assert_eq!(c.color_link.name(), slot(&p, m, 6));
    assert_eq!(c.color_link_hover.name(), slot(&p, m, 4));
    assert_eq!(c.color_link_active.name(), slot(&p, m, 7));
}

/// 亮色中性色：solidColor 基于白色调暗（Ant Design 默认值）。
#[test]
fn light_neutral_tokens() {
    let c = theme(ThemeScheme::Light, ThemeDensity::Comfortable).palette;
    assert_eq!(c.color_bg_layout.name(), "#f5f5f5");
    assert_eq!(c.color_bg_container.name(), "#ffffff");
    assert_eq!(c.color_bg_elevated.name(), "#ffffff");
    assert_eq!(c.color_border.name(), "#d9d9d9");
    assert_eq!(c.color_border_secondary.name(), "#f0f0f0");
    assert_eq!(c.color_bg_base, Color::WHITE);
    assert_eq!(c.color_text_base, Color::BLACK);
    assert_eq!(c.color_bg_blur, Color::TRANSPARENT);
}

/// 暗色中性色：由黑色向灰色提亮。
#[test]
fn dark_neutral_tokens() {
    let c = theme(ThemeScheme::Dark, ThemeDensity::Comfortable).palette;
    assert_eq!(c.color_bg_layout.name(), "#000000");
    assert_eq!(c.color_bg_container.name(), "#141414");
    assert_eq!(c.color_bg_elevated.name(), "#1f1f1f");
    assert_eq!(c.color_bg_spotlight.name(), "#424242");
    assert_eq!(c.color_border.name(), "#424242");
    assert_eq!(c.color_border_secondary.name(), "#303030");
    assert_eq!(c.color_bg_base.name(), "#000000");
    assert_eq!(c.color_text_base, Color::WHITE);
}

/// 透明度令牌：Qt6 语义下 16 位存储、8 位读取为高 8 位。
#[test]
fn alpha_tokens_follow_qt_semantics() {
    let light = theme(ThemeScheme::Light, ThemeDensity::Comfortable).palette;
    let cases = [
        (light.color_text, 224),
        (light.color_text_secondary, 166),
        (light.color_text_tertiary, 115),
        (light.color_text_quaternary, 64),
        (light.color_fill, 38),
        (light.color_fill_secondary, 15),
        (light.color_fill_tertiary, 10),
        (light.color_fill_quaternary, 5),
        (light.color_bg_solid, 255),
        (light.color_bg_solid_hover, 191),
        (light.color_bg_solid_active, 242),
        (light.color_bg_spotlight, 217),
        (light.color_bg_mask, 115),
    ];
    for (color, alpha) in cases {
        assert_eq!(color.alpha(), alpha, "{color:?}");
        assert_eq!((color.red(), color.green(), color.blue()), (0, 0, 0));
    }
    // 精确的 16 位值：0.88*65535 = 57670.8 -> 57671
    assert_eq!(light.color_text.raw16()[3], 57671);

    let dark = theme(ThemeScheme::Dark, ThemeDensity::Comfortable).palette;
    assert_eq!(dark.color_text.alpha(), 217);
    assert_eq!(dark.color_fill.alpha(), 46);
    assert_eq!(dark.color_bg_solid.alpha(), 242);
    assert_eq!(dark.color_bg_solid_hover.alpha(), 255);
    assert_eq!(dark.color_bg_solid_active.alpha(), 230);
    // 0.9*65535 恰为 58981.5，f32 舍入后 qRound 得 58982
    assert_eq!(dark.color_bg_solid_active.raw16()[3], 58982);
    assert_eq!(dark.color_bg_blur.alpha(), 10);
    assert_eq!((dark.color_text.red(), dark.color_text.blue()), (255, 255));
}

/// 语义调色板：不透明前景直取，半透明前景按 f32 通道混合。
#[test]
fn semantic_palette_composites() {
    let l = theme(ThemeScheme::Light, ThemeDensity::Comfortable).semantic;
    assert_eq!(l.window.name(), "#f5f5f5");
    assert_eq!(l.surface.name(), "#ffffff");
    assert_eq!(l.surface_subtle.name(), "#fafafa");
    assert_eq!(l.surface_disabled.name(), "#f5f5f5");
    assert_eq!(l.surface_elevated.name(), "#ffffff");
    assert_eq!(l.window.alpha(), 255);
    assert_eq!(l.text.alpha(), 224);
    assert_eq!(l.accent.name(), "#1677ff");
    assert_eq!(l.accent_subtle.name(), "#e6f4ff");
    assert_eq!(l.accent_border.name(), "#91caff");

    let d = theme(ThemeScheme::Dark, ThemeDensity::Comfortable).semantic;
    assert_eq!(d.window.name(), "#000000");
    assert_eq!(d.surface.name(), "#141414");
    assert_eq!(d.surface_elevated.name(), "#1f1f1f");
    assert_eq!(d.accent.name(), "#1668dc");
}

/// 舒适密度度量。
#[test]
fn metrics_comfortable() {
    let m = theme(ThemeScheme::Light, ThemeDensity::Comfortable).metrics;
    assert_eq!(
        (
            m.size_xxl, m.size_xl, m.size_lg, m.size_md, m.size_ms, m.size
        ),
        (48.0, 32.0, 24.0, 20.0, 16.0, 16.0)
    );
    assert_eq!((m.size_sm, m.size_xs, m.size_xxs), (12.0, 8.0, 4.0));
    assert_eq!((m.control_height, m.control_height_sm), (32.0, 24.0));
    assert_eq!((m.control_height_xs, m.control_height_lg), (16.0, 40.0));
    assert_eq!((m.size_unit, m.size_step), (4.0, 4.0));
    assert_eq!((m.line_width, m.line_width_bold), (1.0, 2.0));
    assert_eq!(m.border_radius, 6.0);
    assert_eq!(m.border_radius_xs, 6.0 / 3.0);
    assert_eq!(m.border_radius_sm, 6.0 * (2.0 / 3.0));
    assert_eq!(m.border_radius_lg, 6.0 * (4.0 / 3.0));
    assert_eq!(m.border_radius_outer, m.border_radius_lg);
    assert_eq!(
        (m.font_size_sm, m.font_size, m.font_size_lg, m.font_size_xl),
        (12.0, 14.0, 16.0, 20.0)
    );
    assert_eq!(
        (
            m.font_size_heading1,
            m.font_size_heading2,
            m.font_size_heading3
        ),
        (38.0, 30.0, 24.0)
    );
    assert_eq!((m.font_size_heading4, m.font_size_heading5), (20.0, 16.0));
    assert_eq!(m.line_height, 1.5715);
    assert_eq!(m.line_height_sm, 1.6667);
    assert_eq!(m.font_height, 14.0 * 1.5715);
    assert_eq!(m.font_height_lg, 16.0 * 1.5);
    assert_eq!(m.font_height_sm, 12.0 * 1.6667);
    assert_eq!((m.popup_arrow_size, m.popup_z_index_base), (16, 1000));
    assert_eq!(m.opacity_image, 1.0);
}

/// 紧凑密度度量。
#[test]
fn metrics_compact() {
    let m = theme(ThemeScheme::Light, ThemeDensity::Compact).metrics;
    assert_eq!(
        (
            m.size_xxl, m.size_xl, m.size_lg, m.size_md, m.size_ms, m.size
        ),
        (40.0, 28.0, 20.0, 16.0, 12.0, 12.0)
    );
    assert_eq!((m.size_sm, m.size_xs, m.size_xxs), (8.0, 4.0, 2.0));
    assert_eq!((m.control_height, m.control_height_sm), (28.0, 22.0));
    assert_eq!(m.size_step, 3.0);
}

/// 非法/边界配置的钳制与回退。
#[test]
fn metrics_clamping() {
    let cfg = ThemeConfig {
        size_unit: 0.0,
        size_step: -1.0,
        line_width: -3.0,
        border_radius: -2.0,
        control_height: 0.0,
        font_size: 0.0,
        size_popup_arrow: -4.0,
        z_index_popup_base: 999.6,
        opacity_image: -1.0,
        ..default_theme_config(ThemeScheme::Light, ThemeDensity::Compact)
    };
    let m = make_theme(&cfg).metrics;
    assert_eq!((m.size_unit, m.size_step), (4.0, 3.0));
    assert_eq!((m.line_width, m.line_width_bold), (0.0, 0.0));
    assert_eq!(m.border_radius, 0.0);
    assert_eq!((m.control_height, m.control_height_sm), (28.0, 22.0));
    assert_eq!(m.font_size, 14.0);
    assert_eq!((m.popup_arrow_size, m.popup_z_index_base), (0, 1000));
    assert_eq!(m.opacity_image, 0.0);
}

/// 动效开/关。
#[test]
fn motion_on_off() {
    let on = theme(ThemeScheme::Light, ThemeDensity::Comfortable).motion;
    assert!(on.motion);
    assert_eq!(
        (
            on.motion_duration_fast,
            on.motion_duration_mid,
            on.motion_duration_slow
        ),
        (100, 200, 300)
    );
    assert_eq!(
        (
            on.timing_frame_interval_ms,
            on.timing_spinner_cycle_ms,
            on.timing_wave_duration_ms
        ),
        (25, 1000, 560)
    );
    assert_eq!(
        (
            on.timing_menu_open_delay_ms,
            on.timing_menu_close_delay_ms,
            on.timing_loading_delay_ms
        ),
        (0, 100, 0)
    );
    assert_eq!(on.motion_ease_out, EasingCurve::OutCubic);
    assert_eq!(on.motion_ease_in_out_circ, EasingCurve::InOutCirc);

    let off_cfg = ThemeConfig {
        motion: false,
        ..default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable)
    };
    let off = make_theme(&off_cfg).motion;
    assert!(!off.motion);
    assert_eq!(
        (
            off.motion_duration_fast,
            off.timing_frame_interval_ms,
            off.timing_menu_close_delay_ms
        ),
        (0, 0, 0)
    );
    assert_eq!(off.motion_ease_out_quint, EasingCurve::OutQuint);
}

/// 默认配置与结构体默认值。
#[test]
fn default_config_values() {
    let c = default_theme_config(ThemeScheme::Dark, ThemeDensity::Comfortable);
    assert_eq!(c.scheme, ThemeScheme::Dark);
    assert_eq!(c.accents.blue.unwrap().name(), "#1677ff");
    assert_eq!(c.accents.lime.unwrap().name(), "#a0d911");
    assert_eq!(c.accents.magenta, c.accents.pink);
    assert_eq!(c.error.unwrap().name(), "#ff4d4f");
    assert_eq!(c.info, c.primary);
    assert!(c.link.is_none());
    let raw = ThemeConfig::default();
    assert!(raw.primary.is_none() && raw.accents.blue.is_none());
    assert_eq!(raw.font_size, 14.0);
    assert_eq!(ThemeMotion::default().motion_ease_out, EasingCurve::Linear);
    assert!(ThemeMotion::default().motion);
}

/// 覆盖合并与空覆盖判断。
#[test]
fn merge_and_empty_override() {
    let base = default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable);
    assert!(is_empty_theme_override(&ThemeOverride::default()));
    assert_eq!(merge_theme_config(&base, &ThemeOverride::default()), base);

    let mut o = ThemeOverride {
        scheme: Some(ThemeScheme::Dark),
        font_size: Some(16.0),
        motion: Some(false),
        primary: Color::from_hex("#722ed1"),
        ..Default::default()
    };
    o.accents.gold = Color::from_hex("#000000");
    assert!(!is_empty_theme_override(&o));
    let m = merge_theme_config(&base, &o);
    assert_eq!(m.scheme, ThemeScheme::Dark);
    assert_eq!(m.font_size, 16.0);
    assert!(!m.motion);
    assert_eq!(m.primary.unwrap().name(), "#722ed1");
    assert_eq!(m.accents.gold.unwrap().name(), "#000000");
    assert_eq!(m.accents.blue, base.accents.blue);
    assert_eq!(m.link, base.link);
}

/// 自定义主色：令牌随主色变化，accent 回退链（primary 无效 -> blue -> 默认）。
#[test]
fn custom_primary_and_fallback_chain() {
    let mut cfg = default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable);
    cfg.primary = Color::from_hex("#722ed1");
    cfg.info = None;
    let c = make_theme(&cfg).palette;
    assert_eq!(c.color_primary.name(), "#722ed1");
    // info 缺省回退到主色
    assert_eq!(c.color_info.name(), "#722ed1");

    // primary 无效时回退到 accents.blue，再无效则 #1677ff
    let mut cfg2 = default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable);
    cfg2.primary = None;
    cfg2.accents.blue = Color::from_hex("#f5222d");
    assert_eq!(make_theme(&cfg2).palette.color_primary.name(), "#f5222d");
    cfg2.accents.blue = None;
    assert_eq!(make_theme(&cfg2).palette.color_primary.name(), "#1677ff");
}
