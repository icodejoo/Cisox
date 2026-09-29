//! Fluent 运行时与 Qt `.ts` → `.ftl` 转换工具（ADR-6）。
//!
//! - [`ts`]：`.ts` 解析。
//! - [`convert`]：转换规则与 id 命名。
//! - [`I18n`]：运行时，按回退链查询译文。
//!
//! 本 crate 不依赖 gpui。产品名不进翻译文案，运行时以变量 `product` 注入（约定 11）。

pub mod convert;
mod embedded;
mod runtime;
pub mod ts;

pub use runtime::{Args, I18n, I18nError};

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
