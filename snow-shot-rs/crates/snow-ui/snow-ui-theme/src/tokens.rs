//! 设计令牌结构：移植自 `theme_types.h/.cpp`（字体、QPalette、扁平化视图属 Qt/GPUI 侧，不在此处）。

use crate::color::Color;
use crate::theme_colors::{
    apply_dark_semantic_colors, apply_light_semantic_colors, make_semantic_palette,
};

/// 声明一个令牌结构体：每个字段自动生成中文文档，并派生 Debug/Clone/PartialEq/Default。
macro_rules! token_struct {
    ($(#[$meta:meta])* $name:ident { $($ty:ty : [ $($field:ident),* $(,)? ]),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct $name {
            $($(
                #[doc = concat!("令牌 `", stringify!($field), "`（同 C++ 同名字段）。")]
                pub $field: $ty,
            )*)*
        }
    };
}

/// 主题明暗方案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeScheme {
    /// 亮色
    #[default]
    Light,
    /// 暗色
    Dark,
}

/// 主题密度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeDensity {
    /// 舒适
    #[default]
    Comfortable,
    /// 紧凑
    Compact,
}

/// 缓动曲线种类，对应 `QEasingCurve::Type`（默认 Linear，同 Qt 默认构造）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EasingCurve {
    /// 线性
    #[default]
    Linear,
    /// OutCirc
    OutCirc,
    /// InOutCirc
    InOutCirc,
    /// OutCubic
    OutCubic,
    /// InOutCubic
    InOutCubic,
    /// OutBack
    OutBack,
    /// InBack
    InBack,
    /// InQuint
    InQuint,
    /// OutQuint
    OutQuint,
}

token_struct! {
    /// 13 个强调色种子（None 表示无效色）。
    ThemeAccents {
        Option<Color>: [blue, purple, cyan, green, magenta, pink, red, orange, yellow, volcano, geekblue, gold, lime]
    }
}

token_struct! {
    /// 全部颜色令牌（对应 `ThemeColors`）。
    ThemeColors {
        Color: [
            color_bg_base, color_text_base, color_text, color_text_secondary, color_text_tertiary,
            color_text_quaternary, color_text_disabled, color_text_placeholder, color_text_light_solid,
            color_fill, color_fill_alter, color_fill_secondary, color_fill_tertiary, color_fill_quaternary,
            color_bg_solid, color_bg_solid_hover, color_bg_solid_active, color_bg_layout, color_bg_container,
            color_bg_container_disabled, color_bg_elevated, color_bg_spotlight, color_bg_blur, color_border,
            color_border_disabled, color_border_secondary, color_primary_bg, color_primary_bg_hover,
            color_primary_border, color_primary_border_hover, color_primary_hover, color_primary,
            color_primary_active, color_primary_solid, color_primary_solid_hover, color_primary_solid_active,
            color_primary_solid_text, color_primary_text_hover, color_primary_text, color_primary_text_active,
            color_success_bg, color_success_bg_hover, color_success_border, color_success_border_hover,
            color_success_hover, color_success, color_success_active, color_success_solid,
            color_success_solid_hover, color_success_solid_active, color_success_solid_text,
            color_success_text_hover, color_success_text, color_success_text_active,
            color_error_bg, color_error_bg_hover, color_error_bg_filled_hover, color_error_bg_active,
            color_error_border, color_error_border_hover, color_error_hover, color_error, color_error_active,
            color_error_solid, color_error_solid_hover, color_error_solid_active, color_error_solid_text,
            color_error_text_hover, color_error_text, color_error_text_active,
            color_warning_bg, color_warning_bg_hover, color_warning_border, color_warning_border_hover,
            color_warning_hover, color_warning, color_warning_active, color_warning_solid,
            color_warning_solid_hover, color_warning_solid_active, color_warning_solid_text,
            color_warning_text_hover, color_warning_text, color_warning_text_active,
            color_info_bg, color_info_bg_hover, color_info_border, color_info_border_hover, color_info_hover,
            color_info, color_info_active, color_info_solid, color_info_solid_hover, color_info_solid_active,
            color_info_solid_text, color_info_text_hover, color_info_text, color_info_text_active,
            color_link_hover, color_link, color_link_active, color_bg_mask, color_white
        ]
    }
}

token_struct! {
    /// 语义化调色板（对应 `ThemeSemanticPalette`）。
    ThemeSemanticPalette {
        Color: [
            window, window_disabled, surface, surface_disabled, surface_elevated, surface_subtle,
            surface_solid, surface_solid_hover, surface_solid_active, surface_spotlight, mask, fill,
            fill_secondary, fill_tertiary, fill_quaternary, text, text_secondary, text_tertiary,
            text_quaternary, text_disabled, text_placeholder, text_on_accent, border, border_disabled,
            border_secondary, accent, accent_hover, accent_active, accent_subtle, accent_subtle_hover,
            accent_border, accent_border_hover, accent_solid, accent_solid_hover, accent_solid_active,
            accent_solid_text, accent_disabled, success, success_hover, success_active, success_subtle,
            success_border, warning, warning_hover, warning_active, warning_subtle, warning_border, error,
            error_hover, error_active, error_subtle, error_border, info, info_hover, info_active,
            info_subtle, info_border, link, link_hover, link_active, white
        ]
    }
}

token_struct! {
    /// 尺寸/字号/行高/圆角/控件高度等度量令牌（对应 `ThemeMetrics`）。
    ThemeMetrics {
        f64: [
            size_xxl, size_xl, size_lg, size_md, size_ms, size, size_sm, size_xs, size_xxs,
            font_size_sm, font_size, font_size_lg, font_size_xl, font_size_heading1, font_size_heading2,
            font_size_heading3, font_size_heading4, font_size_heading5, line_height, line_height_lg,
            line_height_sm, line_height_heading1, line_height_heading2, line_height_heading3,
            line_height_heading4, line_height_heading5, font_height, font_height_lg, font_height_sm,
            line_width, line_width_bold, border_radius, border_radius_xs, border_radius_sm,
            border_radius_lg, border_radius_outer, control_height, control_height_sm, control_height_xs,
            control_height_lg, size_unit, size_step, opacity_image
        ],
        i32: [popup_arrow_size, popup_z_index_base]
    }
}

/// 动效令牌（对应 `ThemeMotion`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeMotion {
    /// 是否启用动效
    pub motion: bool,
    /// 快速时长（毫秒）
    pub motion_duration_fast: i32,
    /// 中速时长（毫秒）
    pub motion_duration_mid: i32,
    /// 慢速时长（毫秒）
    pub motion_duration_slow: i32,
    /// 帧间隔（毫秒）
    pub timing_frame_interval_ms: i32,
    /// 加载圈周期（毫秒）
    pub timing_spinner_cycle_ms: i32,
    /// 波纹时长（毫秒）
    pub timing_wave_duration_ms: i32,
    /// 菜单展开延迟（毫秒）
    pub timing_menu_open_delay_ms: i32,
    /// 菜单收起延迟（毫秒）
    pub timing_menu_close_delay_ms: i32,
    /// 加载态延迟（毫秒）
    pub timing_loading_delay_ms: i32,
    /// 缓动 OutCirc
    pub motion_ease_out_circ: EasingCurve,
    /// 缓动 InOutCirc
    pub motion_ease_in_out_circ: EasingCurve,
    /// 缓动 OutCubic
    pub motion_ease_out: EasingCurve,
    /// 缓动 InOutCubic
    pub motion_ease_in_out: EasingCurve,
    /// 缓动 OutBack
    pub motion_ease_out_back: EasingCurve,
    /// 缓动 InBack
    pub motion_ease_in_back: EasingCurve,
    /// 缓动 InQuint
    pub motion_ease_in_quint: EasingCurve,
    /// 缓动 OutQuint
    pub motion_ease_out_quint: EasingCurve,
}

impl Default for ThemeMotion {
    /// 与 C++ 结构体默认值一致：`motion=true`，时长为 0，缓动为 Linear。
    fn default() -> Self {
        Self {
            motion: true,
            motion_duration_fast: 0,
            motion_duration_mid: 0,
            motion_duration_slow: 0,
            timing_frame_interval_ms: 0,
            timing_spinner_cycle_ms: 0,
            timing_wave_duration_ms: 0,
            timing_menu_open_delay_ms: 0,
            timing_menu_close_delay_ms: 0,
            timing_loading_delay_ms: 0,
            motion_ease_out_circ: EasingCurve::Linear,
            motion_ease_in_out_circ: EasingCurve::Linear,
            motion_ease_out: EasingCurve::Linear,
            motion_ease_in_out: EasingCurve::Linear,
            motion_ease_out_back: EasingCurve::Linear,
            motion_ease_in_back: EasingCurve::Linear,
            motion_ease_in_quint: EasingCurve::Linear,
            motion_ease_out_quint: EasingCurve::Linear,
        }
    }
}

/// 主题配置（种子令牌，对应 `ThemeConfig`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeConfig {
    /// 明暗方案
    pub scheme: ThemeScheme,
    /// 密度
    pub density: ThemeDensity,
    /// 强调色种子
    pub accents: ThemeAccents,
    /// 主色（None 为无效）
    pub primary: Option<Color>,
    /// 成功色
    pub success: Option<Color>,
    /// 警告色
    pub warning: Option<Color>,
    /// 错误色
    pub error: Option<Color>,
    /// 信息色
    pub info: Option<Color>,
    /// 链接色
    pub link: Option<Color>,
    /// 基础字号
    pub font_size: f64,
    /// 线宽
    pub line_width: f64,
    /// 基础圆角
    pub border_radius: f64,
    /// 尺寸单位
    pub size_unit: f64,
    /// 尺寸步长
    pub size_step: f64,
    /// 弹层箭头尺寸
    pub size_popup_arrow: f64,
    /// 控件高度
    pub control_height: f64,
    /// 弹层 z-index 基准
    pub z_index_popup_base: f64,
    /// 图片不透明度
    pub opacity_image: f64,
    /// 线框模式
    pub wireframe: bool,
    /// 是否启用动效
    pub motion: bool,
}

impl Default for ThemeConfig {
    /// 与 C++ `ThemeConfig` 成员默认值一致（颜色均无效）。
    fn default() -> Self {
        Self {
            scheme: ThemeScheme::Light,
            density: ThemeDensity::Comfortable,
            accents: ThemeAccents::default(),
            primary: None,
            success: None,
            warning: None,
            error: None,
            info: None,
            link: None,
            font_size: 14.0,
            line_width: 1.0,
            border_radius: 6.0,
            size_unit: 4.0,
            size_step: 4.0,
            size_popup_arrow: 16.0,
            control_height: 32.0,
            z_index_popup_base: 1000.0,
            opacity_image: 1.0,
            wireframe: false,
            motion: true,
        }
    }
}

/// 主题覆盖（字段为 None 表示不覆盖，对应 `ThemeOverride`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThemeOverride {
    /// 覆盖明暗方案
    pub scheme: Option<ThemeScheme>,
    /// 覆盖密度
    pub density: Option<ThemeDensity>,
    /// 覆盖强调色（Some 表示覆盖）
    pub accents: ThemeAccents,
    /// 覆盖主色
    pub primary: Option<Color>,
    /// 覆盖成功色
    pub success: Option<Color>,
    /// 覆盖警告色
    pub warning: Option<Color>,
    /// 覆盖错误色
    pub error: Option<Color>,
    /// 覆盖信息色
    pub info: Option<Color>,
    /// 覆盖链接色
    pub link: Option<Color>,
    /// 覆盖字号
    pub font_size: Option<f64>,
    /// 覆盖线宽
    pub line_width: Option<f64>,
    /// 覆盖圆角
    pub border_radius: Option<f64>,
    /// 覆盖尺寸单位
    pub size_unit: Option<f64>,
    /// 覆盖尺寸步长
    pub size_step: Option<f64>,
    /// 覆盖箭头尺寸
    pub size_popup_arrow: Option<f64>,
    /// 覆盖控件高度
    pub control_height: Option<f64>,
    /// 覆盖弹层 z-index 基准
    pub z_index_popup_base: Option<f64>,
    /// 覆盖图片不透明度
    pub opacity_image: Option<f64>,
    /// 覆盖线框模式
    pub wireframe: Option<bool>,
    /// 覆盖动效开关
    pub motion: Option<bool>,
}

/// 完整主题（对应 `AdTheme`，不含字体）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Theme {
    /// 明暗方案
    pub scheme: ThemeScheme,
    /// 密度
    pub density: ThemeDensity,
    /// 强调色
    pub accents: ThemeAccents,
    /// 语义调色板
    pub semantic: ThemeSemanticPalette,
    /// 颜色令牌
    pub palette: ThemeColors,
    /// 度量令牌
    pub metrics: ThemeMetrics,
    /// 动效令牌
    pub motion: ThemeMotion,
    /// 线框模式
    pub wireframe: bool,
}

/// 生成默认主题配置（`defaultThemeConfig`）。
///
/// 参数：`scheme` 明暗方案；`density` 密度。返回：默认配置。
///
/// ```rust
/// use snow_ui_theme::tokens::{default_theme_config, ThemeDensity, ThemeScheme};
/// let c = default_theme_config(ThemeScheme::Light, ThemeDensity::Compact);
/// assert_eq!(c.control_height, 28.0);
/// ```
pub fn default_theme_config(scheme: ThemeScheme, density: ThemeDensity) -> ThemeConfig {
    let compact = density == ThemeDensity::Compact;
    let hex = |s: &str| Color::from_hex(s);
    let primary = hex("#1677ff");
    ThemeConfig {
        scheme,
        density,
        accents: ThemeAccents {
            blue: hex("#1677ff"),
            purple: hex("#722ed1"),
            cyan: hex("#13c2c2"),
            green: hex("#52c41a"),
            magenta: hex("#eb2f96"),
            pink: hex("#eb2f96"),
            red: hex("#f5222d"),
            orange: hex("#fa8c16"),
            yellow: hex("#fadb14"),
            volcano: hex("#fa541c"),
            geekblue: hex("#2f54eb"),
            gold: hex("#faad14"),
            lime: hex("#a0d911"),
        },
        primary,
        success: hex("#52c41a"),
        warning: hex("#faad14"),
        error: hex("#ff4d4f"),
        info: primary,
        link: None,
        font_size: 14.0,
        line_width: 1.0,
        border_radius: 6.0,
        size_unit: 4.0,
        size_step: if compact { 3.0 } else { 4.0 },
        size_popup_arrow: 16.0,
        control_height: if compact { 28.0 } else { 32.0 },
        z_index_popup_base: 1000.0,
        opacity_image: 1.0,
        wireframe: false,
        motion: true,
    }
}

/// 合并覆盖项到基础配置（`mergeThemeConfig`）。
///
/// 参数：`base` 基础配置；`o` 覆盖项（Some 才生效）。返回：合并结果。
///
/// ```rust
/// use snow_ui_theme::tokens::*;
/// let base = default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable);
/// let o = ThemeOverride { scheme: Some(ThemeScheme::Dark), ..Default::default() };
/// assert_eq!(merge_theme_config(&base, &o).scheme, ThemeScheme::Dark);
/// ```
pub fn merge_theme_config(base: &ThemeConfig, o: &ThemeOverride) -> ThemeConfig {
    let mut m = base.clone();
    /// 覆盖项有值时写入目标。
    macro_rules! apply {
        ($($t:ident).+ <- $($s:ident).+) => {
            if let Some(v) = o.$($s).+ { m.$($t).+ = v; }
        };
    }
    /// 强调色覆盖：Some 才写入。
    macro_rules! apply_accent {
        ($($f:ident),*) => { $( if o.accents.$f.is_some() { m.accents.$f = o.accents.$f; } )* };
    }
    apply!(scheme <- scheme);
    apply!(density <- density);
    apply_accent!(
        blue, purple, cyan, green, magenta, pink, red, orange, yellow, volcano, geekblue, gold,
        lime
    );
    for (dst, src) in [
        (&mut m.primary, o.primary),
        (&mut m.success, o.success),
        (&mut m.warning, o.warning),
        (&mut m.error, o.error),
        (&mut m.info, o.info),
        (&mut m.link, o.link),
    ] {
        if src.is_some() {
            *dst = src;
        }
    }
    apply!(font_size <- font_size);
    apply!(line_width <- line_width);
    apply!(border_radius <- border_radius);
    apply!(size_unit <- size_unit);
    apply!(size_step <- size_step);
    apply!(size_popup_arrow <- size_popup_arrow);
    apply!(control_height <- control_height);
    apply!(z_index_popup_base <- z_index_popup_base);
    apply!(opacity_image <- opacity_image);
    apply!(wireframe <- wireframe);
    apply!(motion <- motion);
    m
}

/// 覆盖项是否为空（`isEmptyThemeOverride`）。
///
/// 参数：`o` 覆盖项。返回：与默认值相等时为 true。
///
/// ```rust
/// use snow_ui_theme::tokens::{is_empty_theme_override, ThemeOverride};
/// assert!(is_empty_theme_override(&ThemeOverride::default()));
/// ```
pub fn is_empty_theme_override(o: &ThemeOverride) -> bool {
    *o == ThemeOverride::default()
}

/// 由配置生成完整主题（`makeTheme(config)`）。
///
/// 参数：`config` 主题配置。返回：含颜色、语义色、度量与动效的完整主题。
///
/// ```rust
/// use snow_ui_theme::tokens::*;
/// let t = make_theme(&default_theme_config(ThemeScheme::Light, ThemeDensity::Comfortable));
/// assert_eq!(t.palette.color_primary.name(), "#1677ff");
/// ```
pub fn make_theme(config: &ThemeConfig) -> Theme {
    let mut theme = Theme {
        scheme: config.scheme,
        density: config.density,
        wireframe: config.wireframe,
        accents: config.accents.clone(),
        ..Theme::default()
    };
    apply_density_metrics(&mut theme.metrics, config);
    apply_motion(&mut theme.motion, config);

    if theme.scheme == ThemeScheme::Dark {
        apply_dark_semantic_colors(&mut theme.palette, config);
    } else {
        apply_light_semantic_colors(&mut theme.palette, config);
    }
    theme.semantic = make_semantic_palette(&theme.palette);
    theme
}

/// 值为正取原值，否则取回退值。
fn positive_or(value: f64, fallback: f64) -> f64 {
    if value > 0.0 { value } else { fallback }
}

/// 由字号与行高推导字体高度。
fn apply_typography_metrics(m: &mut ThemeMetrics) {
    m.font_height = m.font_size * m.line_height;
    m.font_height_lg = m.font_size_lg * m.line_height_lg;
    m.font_height_sm = m.font_size_sm * m.line_height_sm;
}

/// 按密度与配置计算全部度量令牌。
fn apply_density_metrics(m: &mut ThemeMetrics, config: &ThemeConfig) {
    let compact = config.density == ThemeDensity::Compact;

    if compact {
        m.size_xxl = 40.0;
        m.size_xl = 28.0;
        m.size_lg = 20.0;
        m.size_md = 16.0;
        m.size_ms = 12.0;
        m.size = 12.0;
        m.size_sm = 8.0;
        m.size_xs = 4.0;
        m.size_xxs = 2.0;
    } else {
        m.size_xxl = 48.0;
        m.size_xl = 32.0;
        m.size_lg = 24.0;
        m.size_md = 20.0;
        m.size_ms = 16.0;
        m.size = 16.0;
        m.size_sm = 12.0;
        m.size_xs = 8.0;
        m.size_xxs = 4.0;
    }

    m.size_unit = positive_or(config.size_unit, 4.0);
    m.size_step = positive_or(config.size_step, if compact { 3.0 } else { 4.0 });

    m.line_width = f64::max(0.0, config.line_width);
    m.line_width_bold = f64::max(m.line_width, m.line_width * 2.0);

    m.border_radius = f64::max(0.0, config.border_radius);
    m.border_radius_xs = m.border_radius / 3.0;
    m.border_radius_sm = m.border_radius * (2.0 / 3.0);
    m.border_radius_lg = m.border_radius * (4.0 / 3.0);
    m.border_radius_outer = m.border_radius_lg;

    m.control_height = positive_or(config.control_height, if compact { 28.0 } else { 32.0 });
    m.control_height_sm = if compact {
        f64::max(16.0, m.control_height - 6.0)
    } else {
        f64::max(16.0, m.control_height - 8.0)
    };
    m.control_height_xs = 16.0;
    m.control_height_lg = m.control_height + 8.0;

    let base_font_size = positive_or(config.font_size, 14.0);
    m.font_size_sm = f64::max(10.0, base_font_size - 2.0);
    m.font_size = base_font_size;
    m.font_size_lg = base_font_size + 2.0;
    m.font_size_xl = base_font_size + 6.0;
    m.font_size_heading1 = base_font_size + 24.0;
    m.font_size_heading2 = base_font_size + 16.0;
    m.font_size_heading3 = base_font_size + 10.0;
    m.font_size_heading4 = base_font_size + 6.0;
    m.font_size_heading5 = base_font_size + 2.0;

    m.line_height = 1.5715;
    m.line_height_lg = 1.5;
    m.line_height_sm = 1.6667;
    m.line_height_heading1 = 1.2105;
    m.line_height_heading2 = 1.2667;
    m.line_height_heading3 = 1.3333;
    m.line_height_heading4 = 1.4;
    m.line_height_heading5 = 1.5;

    m.popup_arrow_size = i32::max(0, config.size_popup_arrow.round() as i32);
    m.popup_z_index_base = i32::max(0, config.z_index_popup_base.round() as i32);
    m.opacity_image = f64::max(0.0, config.opacity_image);

    apply_typography_metrics(m);
}

/// 按动效开关计算动效令牌。
fn apply_motion(motion: &mut ThemeMotion, config: &ThemeConfig) {
    let on = config.motion;
    let ms = |v: i32| if on { v } else { 0 };
    motion.motion = on;
    motion.motion_duration_fast = ms(100);
    motion.motion_duration_mid = ms(200);
    motion.motion_duration_slow = ms(300);

    motion.timing_frame_interval_ms = ms(25);
    motion.timing_spinner_cycle_ms = ms(1000);
    motion.timing_wave_duration_ms = ms(560);
    motion.timing_menu_open_delay_ms = 0;
    motion.timing_menu_close_delay_ms = ms(100);
    motion.timing_loading_delay_ms = 0;

    motion.motion_ease_out_circ = EasingCurve::OutCirc;
    motion.motion_ease_in_out_circ = EasingCurve::InOutCirc;
    motion.motion_ease_out = EasingCurve::OutCubic;
    motion.motion_ease_in_out = EasingCurve::InOutCubic;
    motion.motion_ease_out_back = EasingCurve::OutBack;
    motion.motion_ease_in_back = EasingCurve::InBack;
    motion.motion_ease_in_quint = EasingCurve::InQuint;
    motion.motion_ease_out_quint = EasingCurve::OutQuint;
}
