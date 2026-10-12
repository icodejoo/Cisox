//! 托盘 / 热键后端统一错误类型（与 gpui、能力表无关）。

use std::fmt;

/// 非 Windows 降级后端使用的“未实现”原因文案 key。
///
/// 与 `snow-capability` 的 `REASON_NOT_IMPLEMENTED` 取值一致（本 crate 不依赖能力表，
/// 由 `snow-ui-shell` 的测试守住两边相等）。
pub const REASON_NOT_IMPLEMENTED: &str = "capability-reason-not-implemented";

/// 全局热键能力的标识（等同 `Capability::GlobalHotkey.id()`）。
pub const CAPABILITY_GLOBAL_HOTKEY: &str = "GlobalHotkey";

/// 托盘能力的标识（等同 `Capability::Tray.id()`）。
pub const CAPABILITY_TRAY: &str = "Tray";

/// 适配层错误。所有平台调用失败都走这里，不 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// 当前平台/环境不支持该能力（已按能力表降级）。
    Unsupported {
        /// 被拒绝的能力项标识（取自能力表 `Capability::id()`）。
        capability: &'static str,
        /// 原因文案 key。
        reason: &'static str,
    },
    /// 参数不合法（如空区域尺寸、找不到显示器）。
    InvalidArgument(String),
    /// 热键冲突：已被本进程或其他程序占用。
    HotkeyConflict(String),
    /// 热键字符串解析失败。
    InvalidHotkey(String),
    /// 系统调用失败。
    Platform(String),
    /// 后台服务已停止，无法再处理请求。
    ServiceClosed,
    /// 该服务在进程内只能启动一个实例。
    AlreadyRunning(&'static str),
}

impl fmt::Display for ShellError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellError::Unsupported { capability, reason } => {
                write!(f, "能力不可用: {capability} ({reason})")
            }
            ShellError::InvalidArgument(msg) => write!(f, "参数无效: {msg}"),
            ShellError::HotkeyConflict(msg) => write!(f, "热键冲突: {msg}"),
            ShellError::InvalidHotkey(msg) => write!(f, "热键格式无效: {msg}"),
            ShellError::Platform(msg) => write!(f, "系统调用失败: {msg}"),
            ShellError::ServiceClosed => write!(f, "后台服务已停止"),
            ShellError::AlreadyRunning(name) => write!(f, "服务已在运行: {name}"),
        }
    }
}

impl std::error::Error for ShellError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 错误描述可读，且能力不可用时带上能力标识与原因。
    #[test]
    fn display_is_readable() {
        assert!(ShellError::ServiceClosed.to_string().contains("停止"));
        let e = ShellError::Unsupported {
            capability: CAPABILITY_TRAY,
            reason: REASON_NOT_IMPLEMENTED,
        };
        let text = e.to_string();
        assert!(text.contains("Tray") && text.contains("not-implemented"));
    }
}
