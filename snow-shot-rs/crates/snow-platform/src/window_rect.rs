//! 前台窗口的屏幕矩形（直接截图“焦点窗口”用）。

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

#[cfg(windows)]
mod imp {
    use super::{ScreenRect, rect_from_edges};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, IsIconic};

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
}

#[cfg(not(windows))]
mod imp {
    use super::ScreenRect;

    /// 非 Windows 平台没有实现。
    pub fn foreground_window_rect() -> Option<ScreenRect> {
        None
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

    /// 读取前台窗口不会 panic（结果取决于桌面环境）。
    #[test]
    fn foreground_probe_is_safe() {
        let _ = foreground_window_rect();
    }
}
