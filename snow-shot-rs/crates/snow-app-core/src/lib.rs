//! 命令总线、应用状态与会话编排（取代 Qt 版 screenshotcontroller）。
//!
//! 所属阶段：P1。当前仅提供产品常量与内核连通性验证。

pub mod bus;
pub mod command;
pub mod logging;

use snow_draw_engine_core::{EngineConfig, validate_config};

/// 阶段标记。
pub const PHASE: &str = "P1";

/// 产品显示名与通用应用标识符：唯一来源在零依赖的 `snow-data-paths`（托盘宿主也要用），
/// 这里重导出，`snow_app_core::{PRODUCT_NAME, APP_ID}` 路径不变。禁止写入可翻译字符串（约定 11）。
pub use snow_data_paths::{APP_ID, PRODUCT_NAME};

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
