//! 前台窗口的屏幕矩形（直接截图“焦点窗口”用）与前台全屏窗口探测（全局热键抑制用）。

/// 屏幕矩形：`(x, y, 宽, 高)`，物理像素，原点为虚拟桌面左上角。
pub type ScreenRect = (i32, i32, u32, u32);

/// 把 `left/top/right/bottom` 换算成 `(x, y, 宽, 高)`；宽或高非正时返回 `None`。
///
/// # 参数
/// - `left` / `top` / `right` / `bottom`：窗口边界。
///
/// ```
/// use snow_platform::window_rect::rect_from_edges;
/// assert_eq!(rect_from_edges(10, 20, 110, 70), Some((10, 20, 100, 50)));
/// assert_eq!(rect_from_edges(10, 20, 10, 70), None);
/// ```
pub fn rect_from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Option<ScreenRect> {
    let width = right.checked_sub(left)?;
    let height = bottom.checked_sub(top)?;
    (width > 0 && height > 0).then_some((left, top, width as u32, height as u32))
}

/// 读取当前前台窗口的可见边界（优先 DWM 扩展边界，去掉阴影；失败退回 `GetWindowRect`）。
///
/// # 返回
/// 前台窗口矩形；没有前台窗口、窗口最小化或读取失败返回 `None`。非 Windows 恒返回 `None`。
///
/// ```ignore
/// if let Some((x, y, w, h)) = foreground_window_rect() { /* 采集该区域 */ }
/// ```
pub fn foreground_window_rect() -> Option<ScreenRect> {
    imp::foreground_window_rect()
}

/// 窗口外框与显示器范围的对齐容差（物理像素），与 C++ 实现一致。
pub const FULLSCREEN_TOLERANCE: i32 = 1;

/// 窗口外框是否铺满显示器：四条边与显示器范围的差都不超过容差。
///
/// # 参数
/// - `frame`：窗口可见外框 `(left, top, right, bottom)`。
/// - `monitor`：显示器范围 `(left, top, right, bottom)`。
/// - `tolerance`：允许的单边误差（物理像素）。
///
/// ```
/// use snow_platform::window_rect::frame_covers_monitor;
/// assert!(frame_covers_monitor((0, 0, 1920, 1080), (0, 0, 1920, 1080), 1));
/// assert!(frame_covers_monitor((-1, 0, 1921, 1081), (0, 0, 1920, 1080), 1));
/// assert!(!frame_covers_monitor((0, 0, 1920, 1040), (0, 0, 1920, 1080), 1));
/// ```
pub fn frame_covers_monitor(
    frame: (i32, i32, i32, i32),
    monitor: (i32, i32, i32, i32),
    tolerance: i32,
) -> bool {
    (frame.0 - monitor.0).abs() <= tolerance
        && (frame.1 - monitor.1).abs() <= tolerance
        && (frame.2 - monitor.2).abs() <= tolerance
        && (frame.3 - monitor.3).abs() <= tolerance
}

/// 前台是否有全屏窗口（外框铺满所在显示器；桌面、任务栏、最小化与被 DWM 隐藏的窗口不算）。
///
/// # 返回
/// 有则 `true`；读取失败或非 Windows 恒为 `false`。
///
/// ```ignore
/// if focused_fullscreen_window_exists() { /* 暂停全局热键 */ }
/// ```
pub fn focused_fullscreen_window_exists() -> bool {
    imp::focused_fullscreen_window_exists()
}

/// 把系统鼠标指针按物理像素偏移（键盘微调指针用）。
///
/// # 参数
/// - `dx` / `dy`：横纵偏移，正向为右 / 下。
///
/// # 返回
/// 成功 `true`；读取或设置失败、非 Windows 为 `false`。
///
/// ```ignore
/// nudge_cursor(1, 0);
/// ```
pub fn nudge_cursor(dx: i32, dy: i32) -> bool {
    imp::nudge_cursor(dx, dy)
}

#[cfg(windows)]
mod imp {
    use super::{ScreenRect, rect_from_edges};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, IsIconic};

    /// 指针偏移（Windows 实现）。
    pub fn nudge_cursor(dx: i32, dy: i32) -> bool {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let mut p = POINT::default();
        // SAFETY: p 是大小正好为 POINT 的输出缓冲。
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return false;
        }
        // SAFETY: 无内存前置条件，坐标越界由系统钳制。
        unsafe { SetCursorPos(p.x + dx, p.y + dy) }.is_ok()
    }

    /// 读取前台窗口矩形（Windows 实现）。
    pub fn foreground_window_rect() -> Option<ScreenRect> {
        // SAFETY: 无前置条件，返回当前前台窗口句柄（可能为空）。
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        // SAFETY: hwnd 来自上面的查询；最小化窗口没有可截的内容。
        if unsafe { IsIconic(hwnd) }.as_bool() {
            return None;
        }
        let mut rect = RECT::default();
        // SAFETY: rect 是大小正好为 RECT 的输出缓冲。
        let dwm = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut rect as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
        };
        if dwm.is_err() {
            // SAFETY: 同上，rect 为有效输出位置。
            unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
        }
        rect_from_edges(rect.left, rect.top, rect.right, rect.bottom)
    }

    /// 前台全屏窗口探测（Windows 实现，规则同 C++ `focusedFullscreenWindowExists`）。
    pub fn focused_fullscreen_window_exists() -> bool {
        use windows::Win32::Graphics::Dwm::DWMWA_CLOAKED;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetDesktopWindow, GetShellWindow, IsWindow, IsWindowVisible,
        };
        // SAFETY: 无前置条件，返回当前前台窗口句柄（可能为空）。
        let mut hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return false;
        }
        // SAFETY: hwnd 来自上面的查询，以下都是只读的窗口状态查询。
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return false;
            }
            let root = GetAncestor(hwnd, GA_ROOT);
            if !root.0.is_null() {
                hwnd = root;
            }
            if hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
                return false;
            }
        }
        let mut cloaked = 0u32;
        // SAFETY: cloaked 是大小正好为 u32 的输出缓冲。
        let cloak = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        if cloak.is_err() || cloaked != 0 {
            return false;
        }
        let mut frame = RECT::default();
        // SAFETY: frame 是大小正好为 RECT 的输出缓冲。
        let dwm = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut frame as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
        };
        if dwm.is_err() {
            return false;
        }
        // SAFETY: hwnd 有效；MONITOR_DEFAULTTONULL 下窗口不在任何显示器上时返回空。
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
        if monitor.0.is_null() {
            return false;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: info 的 cbSize 已按要求填好。
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        super::frame_covers_monitor(
            (frame.left, frame.top, frame.right, frame.bottom),
            (m.left, m.top, m.right, m.bottom),
            super::FULLSCREEN_TOLERANCE,
        )
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ScreenRect;

    /// 非 Windows 平台没有实现。
    pub fn foreground_window_rect() -> Option<ScreenRect> {
        None
    }

    /// 非 Windows 平台没有实现。
    pub fn focused_fullscreen_window_exists() -> bool {
        false
    }

    /// 非 Windows 平台没有实现。
    pub fn nudge_cursor(_dx: i32, _dy: i32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 边界换算：负坐标可用，退化矩形被拒绝。
    #[test]
    fn edges_to_rect() {
        assert_eq!(rect_from_edges(-100, -50, 0, 0), Some((-100, -50, 100, 50)));
        assert_eq!(rect_from_edges(5, 5, 4, 9), None);
        assert_eq!(rect_from_edges(i32::MIN, 0, i32::MAX, 10), None);
    }

    /// 铺满判定：容差内算铺满，被任务栏占掉一块不算。
    #[test]
    fn covers_monitor_tolerance() {
        let monitor = (0, 0, 2560, 1440);
        assert!(frame_covers_monitor((0, 0, 2560, 1440), monitor, FULLSCREEN_TOLERANCE));
        assert!(frame_covers_monitor((1, 1, 2559, 1439), monitor, FULLSCREEN_TOLERANCE));
        assert!(!frame_covers_monitor((2, 0, 2560, 1440), monitor, FULLSCREEN_TOLERANCE));
        assert!(!frame_covers_monitor((0, 0, 2560, 1400), monitor, FULLSCREEN_TOLERANCE));
    }

    /// 探测前台全屏窗口不会 panic（结果取决于桌面环境）。
    #[test]
    fn fullscreen_probe_is_safe() {
        let _ = focused_fullscreen_window_exists();
    }

    /// 读取前台窗口不会 panic（结果取决于桌面环境）。
    #[test]
    fn foreground_probe_is_safe() {
        let _ = foreground_window_rect();
    }
}
