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

/// 把窗口提到所属层级（置顶层或普通层）的最上面，同时设置置顶状态；不移动、不缩放、不激活。
///
/// 取消置顶必须显式用 `HWND_NOTOPMOST`（`HWND_TOP` 不会去掉置顶样式），之后再提到普通层顶部。
pub(crate) fn bring_to_top(hwnd: isize, topmost: bool) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{HWND_TOP, SWP_NOMOVE, SWP_NOSIZE};
    let place = |after: HWND| {
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
        .map_err(|e| platform_err("SetWindowPos(raise)", e))
    };
    if topmost {
        return place(HWND_TOPMOST);
    }
    place(HWND_NOTOPMOST)?;
    place(HWND_TOP)
}

/// 设置窗口是否从屏幕捕获中排除（`WDA_EXCLUDEFROMCAPTURE`，Windows 10 2004+）。
pub(crate) fn set_capture_excluded(hwnd: isize, excluded: bool) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };
    let affinity = if excluded { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    // SAFETY: 纯值参数。
    unsafe { SetWindowDisplayAffinity(to_hwnd(hwnd), affinity) }
        .map_err(|e| platform_err("SetWindowDisplayAffinity", e))
}

/// 设置整窗不透明度（`0` 全透明 ~ `255` 不透明），用分层窗口的整体 alpha 实现。
///
/// 会把窗口设为分层窗口（`WS_EX_LAYERED`）；与 [`set_input_transparent`] 同时使用时，应在它之后调用，
/// 否则不透明度会被它重置为 255。
pub(crate) fn set_window_alpha(hwnd: isize, alpha: u8) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
        WS_EX_LAYERED,
    };
    let hwnd = to_hwnd(hwnd);
    // SAFETY: 纯值参数；句柄无效时系统返回错误而不是崩溃。
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, current | WS_EX_LAYERED.0 as isize);
    }
    // SAFETY: 同上。
    unsafe { SetLayeredWindowAttributes(hwnd, Default::default(), alpha, LWA_ALPHA) }
        .map_err(|e| platform_err("SetLayeredWindowAttributes", e))
}

/// 设置窗口是否对输入透明（鼠标点击穿过窗口落到下层窗口，且窗口不抢焦点）。
///
/// 实现为 `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`；开启分层样式后补一次
/// `SetLayeredWindowAttributes`（整窗不透明），否则分层窗口不会显示。
pub(crate) fn set_input_transparent(hwnd: isize, transparent: bool) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
    };
    let hwnd = to_hwnd(hwnd);
    let mask = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize;
    // SAFETY: 纯值参数；句柄无效时返回 0 并设置错误，下面按样式读回结果判断。
    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let wanted = if transparent { current | mask } else { current & !mask };
    // SAFETY: 同上。
    unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted) };
    // SAFETY: 同上。
    let applied = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if applied != wanted {
        return Err(platform_err("SetWindowLongPtrW(EXSTYLE)", "样式未生效"));
    }
    if transparent {
        // SAFETY: 句柄已确认可设置样式；参数为纯值。
        unsafe { SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA) }
            .map_err(|e| platform_err("SetLayeredWindowAttributes", e))?;
    }
    Ok(())
}

/// 显示或隐藏窗口（显示时不激活）。
pub(crate) fn set_window_visible(hwnd: isize, visible: bool) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow};
    let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    // SAFETY: 纯值参数；返回值是“之前是否可见”，不是错误码。
    let _ = unsafe { ShowWindow(to_hwnd(hwnd), cmd) };
    Ok(())
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

/// 去掉窗口的标题栏 / 边框样式（无边框覆盖窗使用），并通知系统重算客户区。
///
/// gpui 的 PopUp 窗口样式是 `WS_OVERLAPPED`，保留了 8px 的非客户区边框，
/// 会让客户区比窗口矩形小一圈，导致铺屏内容错位。
pub(crate) fn strip_window_frame(hwnd: isize) -> Result<(), ShellError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongPtrW, WS_BORDER, WS_CAPTION, WS_DLGFRAME, WS_THICKFRAME,
    };
    let h = to_hwnd(hwnd);
    // SAFETY: 纯句柄 / 值参数；窗口无效时系统返回错误而不是崩溃。
    unsafe {
        let style = GetWindowLongPtrW(h, GWL_STYLE);
        let mask = (WS_CAPTION | WS_THICKFRAME | WS_BORDER | WS_DLGFRAME).0 as isize;
        SetWindowLongPtrW(h, GWL_STYLE, style & !mask);
        SetWindowPos(
            h,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| platform_err("SetWindowPos(frame)", e))
}

/// 让窗口成为前台窗口并获得键盘焦点。
///
/// 后台进程直接 `SetForegroundWindow` 会被系统拒绝（只闪任务栏）。这里临时把本线程的输入队列
/// 挂到当前前台窗口所在线程上，借用其前台权限完成切换，随后立即解除挂接。
///
/// # 参数
/// - `hwnd`：目标窗口句柄。
///
/// # 返回
/// 目标窗口最终成为前台窗口则 `Ok`；被系统拒绝返回错误说明。
pub(crate) fn force_foreground(hwnd: isize) -> Result<(), ShellError> {
    use windows::Win32::System::Threading::AttachThreadInput;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };
    let target = to_hwnd(hwnd);
    // SAFETY: 纯句柄 / 值参数；窗口无效时各调用返回失败而不是崩溃。挂接与解除成对出现。
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground == target {
            return Ok(());
        }
        let current_thread = GetCurrentThreadId();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, true).as_bool();
        let _ = BringWindowToTop(target);
        let switched = SetForegroundWindow(target).as_bool();
        let _ = SetFocus(Some(target));
        if attached {
            let _ = AttachThreadInput(current_thread, foreground_thread, false);
        }
        if switched || GetForegroundWindow() == target {
            Ok(())
        } else {
            Err(platform_err("SetForegroundWindow", "被系统拒绝"))
        }
    }
}

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` 属性号（Win10 20H1+ 为 20）。
const DWMWA_USE_IMMERSIVE_DARK_MODE_ATTR: i32 = 20;

/// 设置窗口原生标题栏的深浅色。
pub(crate) fn set_window_dark_title(hwnd: isize, dark: bool) -> Result<(), ShellError> {
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
    let value: i32 = dark.into();
    // SAFETY: 指针指向栈上 4 字节整数，长度与之相符。
    unsafe {
        DwmSetWindowAttribute(
            to_hwnd(hwnd),
            DWMWINDOWATTRIBUTE(DWMWA_USE_IMMERSIVE_DARK_MODE_ATTR),
            &value as *const i32 as *const c_void,
            std::mem::size_of::<i32>() as u32,
        )
    }
    .map_err(|e| platform_err("DwmSetWindowAttribute(dark)", e))
}

/// 设置进程内弹出菜单（托盘右键菜单等）的深浅色：`Some(true)` 深色、`Some(false)` 浅色、`None` 跟随系统。
pub(crate) fn set_popup_menu_dark(dark: Option<bool>) -> Result<(), ShellError> {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{PCSTR, w};
    /// `SetPreferredAppMode` 的 uxtheme 序号。
    const SET_PREFERRED_APP_MODE: usize = 135;
    /// `FlushMenuThemes` 的 uxtheme 序号。
    const FLUSH_MENU_THEMES: usize = 136;
    /// 跟随系统（AllowDark）。
    const MODE_ALLOW_DARK: i32 = 1;
    /// 强制深色。
    const MODE_FORCE_DARK: i32 = 2;
    /// 强制浅色。
    const MODE_FORCE_LIGHT: i32 = 3;
    let mode = match dark {
        Some(true) => MODE_FORCE_DARK,
        Some(false) => MODE_FORCE_LIGHT,
        None => MODE_ALLOW_DARK,
    };
    // SAFETY: 序号函数签名固定（`int(int)` / `void()`），加载失败时直接返回错误。
    unsafe {
        let lib = LoadLibraryW(w!("uxtheme.dll")).map_err(|e| platform_err("LoadLibrary(uxtheme)", e))?;
        let set = GetProcAddress(lib, PCSTR(SET_PREFERRED_APP_MODE as *const u8))
            .ok_or_else(|| ShellError::Platform("缺少 SetPreferredAppMode".into()))?;
        let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set);
        set(mode);
        if let Some(flush) = GetProcAddress(lib, PCSTR(FLUSH_MENU_THEMES as *const u8)) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GWL_EXSTYLE, GetWindowLongPtrW, WINDOW_EX_STYLE, WINDOW_STYLE,
        WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };
    use windows::core::w;

    /// 开启输入透明会带上 LAYERED / TRANSPARENT 样式，关闭后恢复。
    #[test]
    fn input_transparent_toggles_ex_style() {
        // SAFETY: 系统预定义的 STATIC 窗口类，建一个不显示的小窗口，用完销毁。
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("probe"),
                WINDOW_STYLE(0),
                0,
                0,
                10,
                10,
                None,
                None,
                None,
                None,
            )
        }
        .expect("建测试窗口");
        let id = hwnd.0 as isize;
        let style = || {
            // SAFETY: hwnd 在本测试内有效。
            unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
        };
        assert_eq!(style() & WS_EX_TRANSPARENT.0, 0);
        set_input_transparent(id, true).unwrap();
        assert_ne!(style() & WS_EX_TRANSPARENT.0, 0);
        assert_ne!(style() & WS_EX_LAYERED.0, 0);
        set_input_transparent(id, false).unwrap();
        assert_eq!(style() & WS_EX_TRANSPARENT.0, 0);
        // SAFETY: 销毁本测试创建的窗口。
        unsafe { DestroyWindow(hwnd) }.ok();
    }
}
