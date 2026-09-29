//! 设计令牌（移植 ant_design_qt `palette_generate` 及主题令牌，纯 Rust、零依赖、与 gpui 无关）。
//!
//! 所属阶段：P1。模块分工：
//! - [`fast_color`]：`FastColorLite` 颜色解析/转换/混合；
//! - [`palette`]：Ant Design 10 阶色板生成与色阶映射；
//! - [`color`]：复刻 `QColor` 16 位通道语义的令牌颜色；
//! - [`tokens`]：令牌结构、配置、度量与动效；
//! - [`theme_colors`]：亮/暗颜色令牌与语义调色板推导。
//!
//! ```rust
//! use snow_ui_theme::tokens::{default_theme_config, make_theme, ThemeDensity, ThemeScheme};
//! let theme = make_theme(&default_theme_config(ThemeScheme::Dark, ThemeDensity::Comfortable));
//! assert_eq!(theme.palette.color_primary.name(), "#1668dc");
//! ```

pub mod color;
pub mod fast_color;
pub mod palette;
pub mod theme_colors;
pub mod tokens;

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P1";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }
}
