//! Win32/Cocoa/Linux 原生调用封装。
//!
//! 所属阶段：P1。当前为最小骨架，占位实现为可运行的降级态。

pub mod capture;
pub mod clipboard;
pub mod console;
pub mod crash;
pub mod dib;
pub mod focus_probe;
pub mod local_time;
pub mod menu;
pub mod process_mem;
pub mod scroll_input;
pub mod shell;
pub mod single_instance;
pub mod text_inject;
pub mod text_raster;
pub mod tray;
pub mod win_ocr;

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
