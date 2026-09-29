//! gpui-kit 缺口补齐组件（Popconfirm/Checkerboard/Segmented 等）。
//!
//! 所属阶段：P3。当前为最小骨架，占位实现为可运行的降级态。

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
