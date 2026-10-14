//! Win32/Cocoa/Linux 原生调用封装。
//!
//! 所属阶段：P1。当前为最小骨架，占位实现为可运行的降级态。

pub mod atomic_file;
pub mod capture;
pub mod clipboard;
#[cfg(windows)]
mod com;
pub mod console;
pub mod crash;
pub mod dib;
pub mod file_dialog;
pub mod focus_probe;
#[cfg(windows)]
mod gdi;
pub mod global_mouse;
pub mod local_time;
pub mod menu;
pub mod process_mem;
pub mod random;
pub mod scroll_input;
pub mod selected_files;
pub mod shell;
pub mod single_instance;
pub mod system_integration;
pub mod text_inject;
pub mod text_raster;
pub mod unix_time;
pub mod win_ocr;
pub mod window_rect;

/// Windows 进程创建标志：不为子进程创建控制台窗口。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
