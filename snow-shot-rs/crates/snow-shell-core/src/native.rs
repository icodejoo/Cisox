//! 后台服务线程要用的原生调用（不含 gpui）：消息循环、跨线程唤醒、弹出菜单主题。
//!
//! Windows 走 `windows` crate；其他平台只保留返回 `Unsupported` 的降级桩（ADR-7）。

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::*;
