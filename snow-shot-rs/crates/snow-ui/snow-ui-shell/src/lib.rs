//! GPUI 适配隔离层：窗口/托盘/热键/DPI。
//!
//! 约束：所有 `gpui::` 引用只允许出现在本 crate（方案 ADR-1 / 约定 4）。
//! GPUI 依赖由 `vendor/` 锁定（见 vendor/README.md）。
//!
//! # 模块分层
//! - **与 gpui 无关**（上层可直接依赖）：[`geometry`]（坐标/DPI/区域）、[`monitor`]、
//!   [`window`]（`WindowSpec`）、[`overlay`]（`SetWindowRgn` 点击穿透，ADR-2b）、
//!   [`hotkey`]、[`tray`]、[`dispatch`]（出口为命令总线）、[`error`]。
//! - **gpui 门面**：[`ui`] 是唯一暴露 gpui 类型的模块，重导出精选子集并提供 `run` /
//!   `ShellContext::open_window`。视图层 crate 只通过 `snow_ui_shell::ui` 使用 GPUI。
//!
//! 所属阶段：P1。

pub mod dispatch;
pub mod error;
pub mod geometry;
pub mod hotkey;
pub mod inbox;
pub mod monitor;
mod native;
pub mod overlay;
pub mod pinned_geometry;
pub mod selection;
pub mod tray;
pub mod ui;
pub mod window;

use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Window, WindowOptions, div,
};

/// 冒烟用的最小视图，仅渲染一行文字。
struct SmokeView;

impl Render for SmokeView {
    /// 渲染最小内容。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("snow-ui-shell smoke")
    }
}

/// 创建并运行最小 GPUI 应用（阻塞直到窗口关闭）。
///
/// 仅用于验证 vendor 后的依赖可编译、可启动；正式入口见 [`ui::run`]。
///
/// # 示例
/// ```no_run
/// snow_ui_shell::run_smoke_app();
/// ```
pub fn run_smoke_app() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        if let Err(err) = gpui_kit::open_window(WindowOptions::default(), cx, |_window, cx| {
            cx.new(|_| SmokeView)
        }) {
            eprintln!("冒烟窗口创建失败: {err}");
            cx.quit();
        }
    });
}

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P1";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }
}
