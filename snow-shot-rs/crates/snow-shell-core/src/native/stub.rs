//! 非 Windows 平台的降级桩：一律返回 `Unsupported`，不 panic。

use crate::error::{CAPABILITY_TRAY, REASON_NOT_IMPLEMENTED, ShellError};

/// 设置进程内弹出菜单（托盘右键菜单等）的深浅色：桩返回不支持。
///
/// # 参数
/// - `_dark`：`Some(true)` 深色、`Some(false)` 浅色、`None` 跟随系统（桩忽略）。
///
/// # 返回
/// 恒为 `Unsupported`。
pub fn set_popup_menu_dark(_dark: Option<bool>) -> Result<(), ShellError> {
    Err(ShellError::Unsupported {
        capability: CAPABILITY_TRAY,
        reason: REASON_NOT_IMPLEMENTED,
    })
}
