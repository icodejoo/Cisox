//! 适配层统一错误类型（与 gpui 无关）。

use snow_capability::{Capability, CapabilityRegistry, CapabilityStatus};
use std::fmt;

/// 能力探测/初始化失败时使用的原因文案 key。
pub const REASON_INIT_FAILED: &str = "capability-reason-init-failed";

/// 适配层错误。所有平台调用失败都走这里，不 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// 当前平台/环境不支持该能力（已按能力表降级）。
    Unsupported {
        /// 被拒绝的能力项。
        capability: Capability,
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
                write!(f, "能力不可用: {} ({reason})", capability.id())
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

/// 按能力表校验能力是否可用；降级态视为可用。
///
/// # 参数
/// - `caps`：能力注册表。
/// - `cap`：要校验的能力。
///
/// # 返回
/// 可用返回 `Ok(())`，否则返回 [`ShellError::Unsupported`]。
///
/// ```rust
/// use snow_capability::{Capability, CapabilityRegistry, Platform};
/// use snow_ui_shell::error::require_capability;
/// let reg = CapabilityRegistry::for_platform(Platform::MacOs);
/// assert!(require_capability(&reg, Capability::Tray).is_err());
/// ```
pub fn require_capability(caps: &CapabilityRegistry, cap: Capability) -> Result<(), ShellError> {
    match caps.query(cap) {
        CapabilityStatus::Unsupported { reason } => Err(ShellError::Unsupported {
            capability: cap,
            reason,
        }),
        _ => Ok(()),
    }
}

/// 把某能力标记为“初始化失败”，让 UI 走禁用态。
///
/// # 参数
/// - `caps`：能力注册表（会被修改）。
/// - `cap`：初始化失败的能力。
///
/// ```rust
/// use snow_capability::{Capability, CapabilityRegistry, Platform};
/// use snow_ui_shell::error::mark_init_failed;
/// let mut reg = CapabilityRegistry::for_platform(Platform::Windows);
/// mark_init_failed(&mut reg, Capability::Tray);
/// assert!(!reg.query(Capability::Tray).is_usable());
/// ```
pub fn mark_init_failed(caps: &mut CapabilityRegistry, cap: Capability) {
    caps.override_status(
        cap,
        CapabilityStatus::Unsupported {
            reason: REASON_INIT_FAILED,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_capability::Platform;

    /// 不可用能力被拒绝，降级与完整支持放行。
    #[test]
    fn require_capability_rules() {
        let win = CapabilityRegistry::for_platform(Platform::Windows);
        assert!(require_capability(&win, Capability::GlobalHotkey).is_ok());
        let linux = CapabilityRegistry::for_platform(Platform::Linux);
        assert!(matches!(
            require_capability(&linux, Capability::OverlayClickThrough),
            Err(ShellError::Unsupported { .. })
        ));
        assert!(matches!(
            require_capability(&linux, Capability::Tray),
            Err(ShellError::Unsupported { .. })
        ));
    }

    /// 标记失败后能力变为不可用且带原因。
    #[test]
    fn mark_failed_sets_reason() {
        let mut reg = CapabilityRegistry::for_platform(Platform::Windows);
        mark_init_failed(&mut reg, Capability::GlobalHotkey);
        assert_eq!(
            reg.query(Capability::GlobalHotkey).reason(),
            Some(REASON_INIT_FAILED)
        );
    }

    /// 错误描述可读。
    #[test]
    fn display_is_readable() {
        assert!(ShellError::ServiceClosed.to_string().contains("停止"));
    }
}
