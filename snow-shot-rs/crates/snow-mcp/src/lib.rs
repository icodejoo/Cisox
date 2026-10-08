//! 进程内 MCP server（ADR-9）：JSON-RPC 2.0 over 本地命名管道，设计见 `docs/design/mcp-subsystem.md`。
//!
//! 第一期（M0 骨架）：协议、令牌鉴权、tool 注册表（全部 101 个登记，未实现的返回结构化错误）、
//! 按需启动的管道服务。协议与注册表与传输无关，可完全离屏测试。

pub mod auth;
pub mod descriptor;
pub mod manifest;
pub mod protocol;
pub mod registry;
pub mod schema;
pub mod session;
pub mod tools;

#[cfg(windows)]
pub mod service;

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P7";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }
}
