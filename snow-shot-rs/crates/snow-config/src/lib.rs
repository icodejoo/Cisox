//! 配置层：schema、键专属规范化、加载修复与 Qt 兼容的磁盘格式（方案 ADR-8）。
//!
//! 磁盘格式沿用 upstream `config.json`（`"组/名"` 两级 JSON、`storage/schema_version = 3`、
//! 4 空格缩进），数据根目录使用自有名称（见 [`paths`]），不读写 upstream 目录。
//!
//! - [`schema`]：238 个键的类型/默认值/取值约束（另有 [`extensions`] 的 Cisox 扩展项）
//! - [`normalize`]：约 20 个键专属规范化，非法值回退默认
//! - [`document`]：加载修复、迁移、未知字段保留、Qt 风格序列化
//! - [`store`]：文件读写（损坏留档、原子写入）
//! - [`paths`]：数据根目录与便携模式
//! - [`typed`]：历史保留策略等强类型视图
//!
//! # 示例
//! ```
//! use serde_json::json;
//! use snow_config::document::ConfigDocument;
//!
//! let mut doc = ConfigDocument::from_bytes(None);
//! doc.set_value("screenshot/image_quality", json!(80)).unwrap();
//! assert!(doc.set_value("screenshot/image_quality", json!(101)).is_err());
//! ```

pub mod custom_models;
pub mod document;
pub mod extensions;
pub mod normalize;
pub mod paths;
pub mod schema;
mod schema_table;
pub mod selection;
pub mod shortcut;
pub mod store;
pub mod templates;
pub mod toolbar;
pub mod typed;
pub mod value;

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
