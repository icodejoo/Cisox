//! 适配层统一错误类型（与 gpui 无关）。
//!
//! [`ShellError`] 本体在 `snow-shell-core`（托盘宿主也要用），这里重导出，并提供依赖能力表的
//! 校验函数（能力表不进宿主的依赖闭包）。

use snow_capability::{Capability, CapabilityRegistry, CapabilityStatus};
pub use snow_shell_core::error::ShellError;

/// 能力探测/初始化失败时使用的原因文案 key。
pub const REASON_INIT_FAILED: &str = "capability-reason-init-failed";

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
            capability: cap.id(),
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

    /// 能力不可用的错误带能力标识（取自能力表）。
    #[test]
    fn unsupported_carries_capability_id() {
        let linux = CapabilityRegistry::for_platform(Platform::Linux);
        let err = require_capability(&linux, Capability::Tray).unwrap_err();
        assert!(err.to_string().contains(Capability::Tray.id()));
    }

    /// `snow-shell-core` 里复刻的能力标识与原因 key 必须与能力表一致（核心 crate 不依赖能力表）。
    #[test]
    fn core_constants_match_capability_table() {
        use snow_shell_core::error::{
            CAPABILITY_GLOBAL_HOTKEY, CAPABILITY_TRAY, REASON_NOT_IMPLEMENTED,
        };
        assert_eq!(CAPABILITY_GLOBAL_HOTKEY, Capability::GlobalHotkey.id());
        assert_eq!(CAPABILITY_TRAY, Capability::Tray.id());
        assert_eq!(
            REASON_NOT_IMPLEMENTED,
            snow_capability::REASON_NOT_IMPLEMENTED
        );
    }
}
