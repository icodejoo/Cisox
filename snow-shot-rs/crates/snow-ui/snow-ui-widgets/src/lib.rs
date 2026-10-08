//! gpui-kit 缺口补齐组件库（方案 ADR-3）。
//!
//! 提供基于 `snow-ui-shell` 门面的自研组件集（Checkerboard、Segmented、Popconfirm 等），
//! 严格隔离外部直接依赖 `gpui`。

mod checkerboard;
mod magnifier;
mod popconfirm;
mod segmented;
mod toolbar;

pub use checkerboard::{
    Checkerboard, DEFAULT_CELL_SIZE, DEFAULT_DARK_COLOR, DEFAULT_LIGHT_COLOR,
};
pub use magnifier::{
    ColorFormat, Magnifier, MagnifierGrid, calculate_magnifier_placement,
};
pub use popconfirm::{
    Popconfirm, PopconfirmHandler, PopconfirmPlacement,
};
pub use segmented::{
    DEFAULT_ACTIVE_BG_COLOR, DEFAULT_ACTIVE_TEXT_COLOR, DEFAULT_BG_COLOR,
    DEFAULT_SEGMENTED_HEIGHT, DEFAULT_TEXT_COLOR, Segmented, SegmentedChangeHandler,
    SegmentedItem,
};
pub use toolbar::{
    AnnotationTool, ScreenshotToolbar, ToolbarAction, ToolbarLabel, calculate_toolbar_placement,
};

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P3";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }

    /// 测试 Popconfirm 构造与属性设置。
    #[test]
    fn popconfirm_builder() {
        let p = Popconfirm::new("test-pop", "确认删除？")
            .description("删除后无法恢复")
            .ok_text("删吧")
            .cancel_text("点错了")
            .ok_danger(true)
            .placement(PopconfirmPlacement::BottomLeft);

        assert_eq!(p.title(), "确认删除？");
    }
}
