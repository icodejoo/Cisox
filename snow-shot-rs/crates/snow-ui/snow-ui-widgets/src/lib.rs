//! gpui-kit 缺口补齐组件库（方案 ADR-3）。
//!
//! 提供基于 `snow-ui-shell` 门面的自研组件集（Checkerboard、Segmented、Popconfirm 等），
//! 严格隔离外部直接依赖 `gpui`。

mod checkerboard;
mod segmented;

pub use checkerboard::{
    Checkerboard, DEFAULT_CELL_SIZE, DEFAULT_DARK_COLOR, DEFAULT_LIGHT_COLOR,
};
pub use segmented::{
    DEFAULT_ACTIVE_BG_COLOR, DEFAULT_ACTIVE_TEXT_COLOR, DEFAULT_BG_COLOR,
    DEFAULT_SEGMENTED_HEIGHT, DEFAULT_TEXT_COLOR, Segmented, SegmentedChangeHandler,
    SegmentedItem,
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
}
