//! 托盘与全局热键后端：与 gpui、命令总线、能力表都无关的那一半（托盘宿主与 UI 进程共用）。
//!
//! 从 `snow-ui-shell` 抽出（方案：`docs/design/tray-host-split.md` §3），`snow-ui-shell`
//! 以原路径重导出并在外面套一层 `AppCommand` 与能力表适配。
//!
//! # 约束
//! - 不得依赖 `gpui`、`snow-app-core`、`snow-capability`（`workspace-guard` 检查依赖闭包）。
//! - 动作负载是泛型 `A`：UI 进程用 `AppCommand`，托盘宿主用动作 id 字符串；本 crate 不解释它。
//!
//! # 模块
//! - [`hotkey`]：热键解析（`Ctrl+Alt+S`）、登记表与全局热键服务。
//! - [`tray`]：托盘描述（图标、菜单、点击动作）与托盘服务。
//! - [`dispatch`]：动作出口 [`dispatch::Dispatcher`]。
//! - [`native`]：后台线程消息循环、唤醒器、弹出菜单深浅色。
//! - [`error`]：统一错误 [`error::ShellError`]。

pub mod dispatch;
pub mod error;
pub mod hotkey;
pub mod native;
#[cfg(any(windows, test))]
mod service_thread;
pub mod tray;
