//! 托盘宿主：常驻的最小 Win32 进程（方案：`docs/design/tray-host-split.md`）。
//!
//! 宿主只管“能让用户唤起 Cisox 的东西”：托盘图标与菜单、全局热键（含前台全屏闸门、暂停开关）、
//! 单实例与命令行入口、UI 进程的拉起与仲裁。窗口、配置读写、i18n、历史、贴图、录屏 / 语音 /
//! 翻译 / OCR 的调度全在 UI 进程（`snow-shot-ui.exe`）。宿主**不链接 GPUI、不碰 D3D / IME / UIA**，
//! 依赖闭包受 `workspace-guard` 约束，导入表受 `scripts/check-host-imports.ps1` 约束。
//!
//! # 模块
//! - [`machine`]：UI 进程状态机（拉起、握手、命令投递、退出仲裁、崩溃熔断），纯逻辑、可确定性测试。
//! - [`queue`]：命令队列（64 条 / 30s / 手势 update 只留最新）。
//! - [`breaker`]：崩溃熔断（60s 内 3 次 → 退避 5 分钟）。
//! - [`focus`]：前台焦点授权的决策。
//! - [`host`]：宿主核心（状态机 + 快照 + 暂停 / 闸门 + 托盘与热键的应用），系统调用都在 [`host::Platform`] 后面。
//! - [`view`]：从快照算出实际要显示 / 注册的东西（纯函数）。
//! - [`convert`]：快照类型转 `snow-shell-core` 的托盘 / 热键类型。
//! - [`ipc_map`]：`--cmd` 命令到动作 id 的映射。
//! - [`cli`]：命令行解析。
//! - [`harden`]：进程加固（扩展点禁用、`ImmDisableIME(-1)`、DPI 感知）。
//! - [`host_log`]：写 `host.log` 的最小 `tracing` 订阅者。
//! - `win` / `app`（仅 Windows）：真实平台与入口流程。

pub mod breaker;
pub mod cli;
pub mod convert;
pub mod focus;
pub mod harden;
pub mod host;
pub mod host_log;
pub mod ipc_map;
pub mod machine;
pub mod queue;
pub mod view;

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod win;
