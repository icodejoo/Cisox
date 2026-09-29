//! 命令总线、应用状态与会话编排（取代 Qt 版 screenshotcontroller）。
//!
//! 所属阶段：P1。当前仅提供产品常量与内核连通性验证。

pub mod bus;
pub mod command;

use snow_draw_engine_core::{EngineConfig, validate_config};

/// 阶段标记。
pub const PHASE: &str = "P1";

/// 产品显示名，唯一来源；禁止写入可翻译字符串（约定 11）。
pub const PRODUCT_NAME: &str = "Cisox";

/// 通用应用标识符（数据目录、锁名等由此派生）。
pub const APP_ID: &str = "cisox";

/// 单实例互斥体 / 锁名。
pub const SINGLE_INSTANCE_NAME: &str = "Cisox.SingleInstance";

/// 校验共享绘图内核的默认配置是否合法，用于验证跨 workspace 依赖可用。
pub fn engine_default_config_is_valid() -> bool {
    validate_config(&EngineConfig::default()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内核默认配置应通过校验。
    #[test]
    fn engine_default_config_valid() {
        assert!(engine_default_config_is_valid());
    }

    /// 产品常量应与方案 T1/T6 取值一致。
    #[test]
    fn product_constants() {
        assert_eq!(PRODUCT_NAME, "Cisox");
        assert_eq!(APP_ID, "cisox");
        assert_eq!(SINGLE_INSTANCE_NAME, "Cisox.SingleInstance");
    }
}
