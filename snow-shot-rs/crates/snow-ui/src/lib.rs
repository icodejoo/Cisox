//! GPUI 视图层总入口（聚合 snow-ui-* 子 crate）。
//!
//! 统一提供窗口与事件外壳（`shell`）、设计令牌与色彩算法（`theme`）、
//! 图标光栅化管理（`icons`）以及 Ant Design 补齐组件（`widgets`）。

pub use snow_ui_icons as icons;
pub use snow_ui_shell as shell;
pub use snow_ui_theme as theme;
pub use snow_ui_widgets as widgets;

pub use snow_ui_shell::ui;

/// 本 crate 的阶段标记。
pub const PHASE: &str = "P3";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert_eq!(PHASE, "P3");
    }

    /// 验证子模块聚合重导出可正常访问。
    #[test]
    fn subcrate_reexports_accessible() {
        assert_eq!(widgets::PHASE, "P3");
        let theme_color = theme::fast_color::FastColor::parse("#1677FF");
        assert!(theme_color.is_valid());
    }
}
