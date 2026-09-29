//! Windows 原生实现（`windows` 0.62，与 gpui 同版本）。

use crate::error::ShellError;
use crate::geometry::{PhysicalPoint, PhysicalRect, Region, ScaleFactor};
use crate::monitor::{MonitorId, MonitorInfo};
use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC, HGDIOBJ,
    HMONITOR, HRGN, MONITORINFO, MONITORINFOEXW, RGN_OR, SetWindowRgn,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, GetWindowRect, HWND_NOTOPMOST, HWND_TOPMOST, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SetWindowPos,
    TranslateMessage, WM_APP, WM_USER,
};

/// 唤醒后台消息循环用的私有消息号。
const WM_SHELL_WAKE: u32 = WM_APP + 0x51;

/// 由整数句柄还原 `HWND`。
fn to_hwnd(hwnd: isize) -> HWND {
    HWND(hwnd as *mut c_void)
}

/// 把 `windows` 错误包成 [`ShellError::Platform`]。
fn platform_err(what: &str, err: impl std::fmt::Display) -> ShellError {
    ShellError::Platform(format!("{what}: {err}"))
}

/// 设置进程为 Per-Monitor-V2 DPI 感知；已设置（含 manifest 已声明）时返回 `false`。
pub(crate) fn ensure_dpi_awareness() -> bool {
    // SAFETY: 仅修改进程级 DPI 感知标志，无指针参数。
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() }
}

/// `RECT` 转物理矩形。
fn rect_from(r: RECT) -> PhysicalRect {
    PhysicalRect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// `EnumDisplayMonitors` 回调：把句柄收集进 `Vec<HMONITOR>`。
unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: `data` 由 `enumerate_monitors` 传入，指向存活的 `Vec<HMONITOR>`。
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(monitor);
    TRUE
}

/// 读取单个显示器信息。
fn monitor_info(handle: HMONITOR) -> Result<MonitorInfo, ShellError> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` 大小已声明；MONITORINFOEXW 首字段即 MONITORINFO，API 约定可如此传递。
    unsafe {
        GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO)
            .ok()
            .map_err(|e| platform_err("GetMonitorInfoW", e))?;
    }
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: 出参指向局部变量。DPI 取不到时按 96 处理。
    let scale = match unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
    {
        Ok(()) => ScaleFactor::from_dpi(dpi_x),
        Err(_) => ScaleFactor::ONE,
    };
    let name_len = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    Ok(MonitorInfo {
        id: MonitorId(handle.0 as usize as u64),
        name: String::from_utf16_lossy(&info.szDevice[..name_len]),
        bounds: rect_from(info.monitorInfo.rcMonitor),
        work_area: rect_from(info.monitorInfo.rcWork),
        scale,
        // MONITORINFOF_PRIMARY = 1
        is_primary: info.monitorInfo.dwFlags & 1 != 0,
    })
}

/// 枚举显示器（屏幕坐标为物理像素，依赖进程 DPI 感知）。
pub(crate) fn enumerate_monitors() -> Result<Vec<MonitorInfo>, ShellError> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: 回调只在本调用期间执行，`handles` 在其间存活。
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(ShellError::Platform("EnumDisplayMonitors 失败".into()));
    }
    handles.into_iter().map(monitor_info).collect()
}

/// 把 [`Region`] 转成 GDI 区域（矩形并集）。
fn build_hrgn(region: &Region) -> Result<HRGN, ShellError> {
    // SAFETY: GDI 区域对象由本函数创建，失败路径逐一释放。
    unsafe {
        let acc = CreateRectRgn(0, 0, 0, 0);
        if acc.is_invalid() {
            return Err(ShellError::Platform("CreateRectRgn 失败".into()));
        }
        for r in region.rects() {
            let piece = CreateRectRgn(r.x, r.y, r.right(), r.bottom());
            if piece.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(acc.0));
                return Err(ShellError::Platform("CreateRectRgn 失败".into()));
            }
            CombineRgn(Some(acc), Some(acc), Some(piece), RGN_OR);
            let _ = DeleteObject(HGDIOBJ(piece.0));
        }
        Ok(acc)
    }
}

/// 设置窗口形状区域（ADR-2b）；`None` 表示清除，恢复整窗。
///
/// 区域之外既不显示也不接收点击。调用成功后区域所有权归系统。
pub(crate) fn set_window_region(hwnd: isize, region: Option<&Region>) -> Result<(), ShellError> {
    let hrgn = region.map(build_hrgn).transpose()?;
    // SAFETY: 成功时系统接管 hrgn；失败时由我们释放。
    unsafe {
        let ret = SetWindowRgn(to_hwnd(hwnd), hrgn, true);
        if ret == 0 {
            if let Some(h) = hrgn {
                let _ = DeleteObject(HGDIOBJ(h.0));
            }
            return Err(ShellError::Platform(
                "SetWindowRgn 失败（句柄无效？）".into(),
            ));
        }
    }
    Ok(())
}

/// 读取窗口外框矩形（屏幕坐标，物理像素）。
pub(crate) fn window_rect(hwnd: isize) -> Result<PhysicalRect, ShellError> {
    let mut r = RECT::default();
    // SAFETY: 出参指向局部变量。
    unsafe { GetWindowRect(to_hwnd(hwnd), &mut r) }
        .map_err(|e| platform_err("GetWindowRect", e))?;
    Ok(rect_from(r))
}

/// 设置窗口外框矩形（屏幕坐标，物理像素），不激活、不改 Z 序。
pub(crate) fn set_window_rect(hwnd: isize, rect: PhysicalRect) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
    // SAFETY: 纯值参数。
    unsafe {
        SetWindowPos(
            to_hwnd(hwnd),
            None,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        )
    }
    .map_err(|e| platform_err("SetWindowPos", e))
}

/// 读取鼠标当前的屏幕坐标（物理像素）。
pub(crate) fn cursor_pos() -> Result<PhysicalPoint, ShellError> {
    let mut p = POINT::default();
    // SAFETY: 出参指向局部变量。
    unsafe { GetCursorPos(&mut p) }.map_err(|e| platform_err("GetCursorPos", e))?;
    Ok(PhysicalPoint::new(p.x, p.y))
}

/// 切换置顶状态，不移动、不缩放、不激活。
pub(crate) fn set_topmost(hwnd: isize, topmost: bool) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOMOVE, SWP_NOSIZE};
    let after = if topmost {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    // SAFETY: 纯值参数。
    unsafe {
        SetWindowPos(
            to_hwnd(hwnd),
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| platform_err("SetWindowPos(topmost)", e))
}

/// 后台消息循环的唤醒/退出句柄，可跨线程使用。
#[derive(Debug, Clone, Copy)]
pub(crate) struct LoopWaker {
    /// 目标线程 ID。
    thread_id: u32,
}

impl LoopWaker {
    /// 唤醒循环，使其立即执行一次 `tick`。
    pub(crate) fn wake(&self) {
        // SAFETY: 目标线程已创建消息队列；失败（线程已退出）可忽略。
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_SHELL_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// 为当前线程创建消息队列并返回其唤醒句柄；必须先于任何跨线程 `wake`。
pub(crate) fn current_thread_waker() -> LoopWaker {
    let mut msg = MSG::default();
    // SAFETY: 空范围 PeekMessage 只用于强制创建线程消息队列。
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        LoopWaker {
            thread_id: GetCurrentThreadId(),
        }
    }
}

/// 运行标准 Win32 消息循环；每处理一条消息后调用 `tick`，返回 `false` 或收到 `WM_QUIT` 时退出。
///
/// 托盘与全局热键的隐藏窗口都要求创建它们的线程持续泵消息。
pub(crate) fn run_message_loop(mut tick: impl FnMut() -> bool) {
    loop {
        let mut msg = MSG::default();
        // SAFETY: 标准消息泵；msg 为局部变量。
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        let is_wake = msg.hwnd.0.is_null() && msg.message == WM_SHELL_WAKE;
        if !is_wake {
            // SAFETY: msg 由 GetMessageW 填充。
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if !tick() {
            break;
        }
    }
}
