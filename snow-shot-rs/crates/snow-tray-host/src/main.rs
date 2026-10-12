//! 托盘宿主入口（产物 `snow-shot.exe`）。
//!
//! `main` 的第一行必须是进程加固：禁用扩展点、`ImmDisableIME(-1)`——任何窗口、线程创建之前。

// 发布形态使用 GUI 子系统（不弹控制台窗口）；调试构建保留控制台便于看输出
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

/// 程序入口（Windows）。
#[cfg(windows)]
fn main() -> ExitCode {
    // 必须第一行：早于创建任何窗口 / 线程（实测依据见 docs/research/tray-host-minimal.md §3.4）
    let harden = snow_tray_host::harden::harden_process();
    snow_tray_host::app::run(std::env::args().skip(1).collect(), harden)
}

/// 程序入口（非 Windows：宿主只在 Windows 上有实现，见方案 ADR-7）。
#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("snow-shot 托盘宿主目前只支持 Windows");
    ExitCode::from(1)
}
