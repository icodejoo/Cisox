//! 平台原生调用（不含 gpui）：显示器枚举、窗口区域/位置、后台消息循环。
//!
//! Windows 走 `windows` crate；其他平台是返回 `Unsupported` 的降级桩（ADR-7）。

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub(crate) use win::*;

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub(crate) use stub::*;
