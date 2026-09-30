//! 非 Windows 平台的降级桩：一律返回 `Unsupported`，不 panic。

use crate::error::ShellError;
use crate::geometry::{PhysicalPoint, PhysicalRect, Region};
use crate::monitor::MonitorInfo;
use snow_capability::Capability;

/// 桩使用的原因 key（与能力表的“未实现”一致）。
const REASON: &str = snow_capability::REASON_NOT_IMPLEMENTED;

/// 构造“能力未实现”错误。
fn unsupported(capability: Capability) -> ShellError {
    ShellError::Unsupported {
        capability,
        reason: REASON,
    }
}

/// 设置 DPI 感知：桩无操作。
pub(crate) fn ensure_dpi_awareness() -> bool {
    false
}

/// 枚举显示器：桩返回不支持。
pub(crate) fn enumerate_monitors() -> Result<Vec<MonitorInfo>, ShellError> {
    Err(unsupported(Capability::ScreenCapture))
}

/// 设置窗口区域：桩返回不支持。
pub(crate) fn set_window_region(_hwnd: isize, _region: Option<&Region>) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 读取窗口矩形：桩返回不支持。
pub(crate) fn window_rect(_hwnd: isize) -> Result<PhysicalRect, ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 设置窗口矩形：桩返回不支持。
pub(crate) fn set_window_rect(_hwnd: isize, _rect: PhysicalRect) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 设置置顶：桩返回不支持。
pub(crate) fn set_topmost(_hwnd: isize, _topmost: bool) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 提到层级最上面：桩返回不支持。
pub(crate) fn bring_to_top(_hwnd: isize, _topmost: bool) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 读取鼠标位置：桩返回不支持。
pub(crate) fn cursor_pos() -> Result<PhysicalPoint, ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 去掉窗口边框：桩返回不支持。
pub(crate) fn strip_window_frame(_hwnd: isize) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 抢占前台：桩返回不支持。
pub(crate) fn force_foreground(_hwnd: isize) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}

/// 设置窗口捕获排除：桩返回不支持。
pub(crate) fn set_capture_excluded(_hwnd: isize, _excluded: bool) -> Result<(), ShellError> {
    Err(unsupported(Capability::OverlayClickThrough))
}
