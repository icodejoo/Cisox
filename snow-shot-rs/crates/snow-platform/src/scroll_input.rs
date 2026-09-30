//! 滚轮投递：向选区中心下方的窗口 `PostMessage(WM_MOUSEWHEEL)`，不移动光标、不使用 SendInput。
//!
//! 与上游做法一致：从 Z 序顶端找第一个“不属于本进程、可见、未禁用、矩形包含该点”的顶层窗口，
//! 再逐层下钻到最深的子窗口，把滚轮消息投递给它。部分应用（自绘 / Raw Input / UWP 等）
//! 不响应投递的滚轮消息，此时自动滚动无效，调用方应提示用户改为手动滚动。

/// 一个窗口候选（Z 序自上而下排列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowCandidate {
    /// 窗口句柄（数值）。
    pub handle: isize,
    /// 窗口矩形 `(左, 上, 右, 下)`，屏幕坐标。
    pub rect: (i32, i32, i32, i32),
    /// 所属进程 ID。
    pub pid: u32,
    /// 是否可见。
    pub visible: bool,
    /// 是否已禁用。
    pub disabled: bool,
}

/// 从 Z 序候选里挑出应接收滚轮的窗口：第一个可见、未禁用、非本进程且矩形包含该点的。
///
/// # 参数
/// - `candidates`：顶层窗口，Z 序从上到下。
/// - `point`：屏幕坐标 `(x, y)`。
/// - `own_pid`：本进程 ID（自身的覆盖窗 / 控制窗要跳过）。
///
/// # 返回
/// 目标窗口句柄；没有合适窗口返回 `None`。
///
/// ```
/// use snow_platform::scroll_input::{pick_target_window, WindowCandidate};
/// let w = |handle, pid| WindowCandidate { handle, rect: (0, 0, 100, 100), pid, visible: true, disabled: false };
/// assert_eq!(pick_target_window(&[w(1, 7), w(2, 8)], (10, 10), 7), Some(2));
/// ```
pub fn pick_target_window(
    candidates: &[WindowCandidate],
    point: (i32, i32),
    own_pid: u32,
) -> Option<isize> {
    candidates
        .iter()
        .find(|c| {
            c.visible
                && !c.disabled
                && c.pid != own_pid
                && point.0 >= c.rect.0
                && point.0 < c.rect.2
                && point.1 >= c.rect.1
                && point.1 < c.rect.3
        })
        .map(|c| c.handle)
}

/// 把滚轮增量与坐标打包成 `WM_MOUSEWHEEL` 的 `(wParam, lParam)`。
///
/// # 参数
/// - `delta`：滚轮增量（120 为一格，正数向上、负数向下）。
/// - `x` / `y`：屏幕坐标。
///
/// ```
/// let (w, l) = snow_platform::scroll_input::pack_wheel(-120, 100, 200);
/// assert_eq!((w >> 16) as u16 as i16, -120);
/// assert_eq!((l & 0xFFFF) as u16 as i16, 100);
/// assert_eq!(((l >> 16) & 0xFFFF) as u16 as i16, 200);
/// ```
pub fn pack_wheel(delta: i32, x: i32, y: i32) -> (usize, isize) {
    let wparam = ((delta as i16 as u16 as usize) << 16) & 0xFFFF_0000;
    let lparam = ((y as i16 as u16 as isize) << 16) | (x as i16 as u16 as isize);
    (wparam, lparam)
}

/// 向屏幕坐标 `point` 下方的窗口投递一次滚轮。
///
/// # 参数
/// - `point`：屏幕坐标（一般取选区中心）。
/// - `delta`：滚轮增量（-120 表示向下滚一格）。
///
/// # 返回
/// 投递成功返回 `Ok`（不代表目标响应）；找不到窗口或投递失败返回说明。非 Windows 恒返回错误。
pub fn post_wheel(point: (i32, i32), delta: i32) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::post_wheel(point, delta)
    }
    #[cfg(not(windows))]
    {
        let _ = (point, delta);
        Err("当前平台不支持投递滚轮".to_string())
    }
}

#[cfg(windows)]
mod win {
    use super::{WindowCandidate, pack_wheel, pick_target_window};
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::WindowsAndMessaging::{
        CWP_SKIPDISABLED, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, ChildWindowFromPointEx, GW_HWNDNEXT,
        GWL_STYLE, GetTopWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
        IsWindowVisible, PostMessageW, WM_MOUSEWHEEL, WS_DISABLED,
    };

    /// 子窗口下钻的最大层数（防止异常窗口树导致死循环）。
    const MAX_DESCENT: usize = 16;

    /// 把句柄数值还原成 HWND。
    fn hwnd(handle: isize) -> HWND {
        HWND(handle as *mut core::ffi::c_void)
    }

    /// 枚举顶层窗口（Z 序从上到下）。
    fn enumerate() -> Vec<WindowCandidate> {
        let mut out = Vec::new();
        // SAFETY: 只读枚举窗口；句柄有效性由系统保证，失败时提前结束。
        unsafe {
            let mut current = GetTopWindow(None).ok();
            while let Some(window) = current {
                let mut rect = RECT::default();
                let mut pid = 0u32;
                if GetWindowRect(window, &mut rect).is_ok() {
                    GetWindowThreadProcessId(window, Some(&mut pid));
                    out.push(WindowCandidate {
                        handle: window.0 as isize,
                        rect: (rect.left, rect.top, rect.right, rect.bottom),
                        pid,
                        visible: IsWindowVisible(window).as_bool(),
                        disabled: (GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_DISABLED.0) != 0,
                    });
                }
                current = GetWindow(window, GW_HWNDNEXT).ok();
            }
        }
        out
    }

    /// 投递滚轮：找顶层目标、下钻到最深子窗口、PostMessage。
    pub fn post_wheel(point: (i32, i32), delta: i32) -> Result<(), String> {
        let own_pid = std::process::id();
        let top = pick_target_window(&enumerate(), point, own_pid)
            .ok_or_else(|| "选区下方没有可接收滚轮的窗口".to_string())?;
        let mut target = hwnd(top);
        // SAFETY: 句柄来自上面的枚举；下钻与投递失败都以错误返回。
        unsafe {
            for _ in 0..MAX_DESCENT {
                let mut client = POINT { x: point.0, y: point.1 };
                if !ScreenToClient(target, &mut client).as_bool() {
                    break;
                }
                let child = ChildWindowFromPointEx(
                    target,
                    client,
                    CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT,
                );
                if child.0.is_null() || child == target {
                    break;
                }
                target = child;
            }
            let (wparam, lparam) = pack_wheel(delta, point.0, point.1);
            PostMessageW(Some(target), WM_MOUSEWHEEL, WPARAM(wparam), LPARAM(lparam))
                .map_err(|e| format!("投递滚轮失败: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造候选。
    fn window(handle: isize, rect: (i32, i32, i32, i32), pid: u32, visible: bool, disabled: bool) -> WindowCandidate {
        WindowCandidate { handle, rect, pid, visible, disabled }
    }

    /// 挑选规则：跳过本进程、不可见、已禁用与不含该点的窗口；取 Z 序最靠上的。
    #[test]
    fn picks_first_eligible_window() {
        let list = [
            window(1, (0, 0, 500, 500), 7, true, false),   // 本进程覆盖窗
            window(2, (0, 0, 500, 500), 8, false, false),  // 不可见
            window(3, (0, 0, 500, 500), 8, true, true),    // 已禁用
            window(4, (600, 0, 900, 500), 8, true, false), // 不含该点
            window(5, (0, 0, 500, 500), 8, true, false),   // 命中
            window(6, (0, 0, 500, 500), 9, true, false),
        ];
        assert_eq!(pick_target_window(&list, (100, 100), 7), Some(5));
        assert_eq!(pick_target_window(&list, (700, 100), 7), Some(4));
        assert_eq!(pick_target_window(&list, (5000, 5000), 7), None);
        assert_eq!(pick_target_window(&[], (0, 0), 7), None);
    }

    /// 矩形右 / 下边界不含（半开区间）。
    #[test]
    fn rect_is_half_open() {
        let list = [window(1, (10, 10, 20, 20), 1, true, false)];
        assert_eq!(pick_target_window(&list, (10, 10), 0), Some(1));
        assert_eq!(pick_target_window(&list, (20, 19), 0), None);
        assert_eq!(pick_target_window(&list, (19, 20), 0), None);
    }

    /// 滚轮参数打包：增量在 wParam 高 16 位，坐标在 lParam（负坐标用补码）。
    #[test]
    fn wheel_packing_handles_negative_values() {
        let (w, l) = pack_wheel(-120, -5, 300);
        assert_eq!((w >> 16) as u16 as i16, -120);
        assert_eq!((l & 0xFFFF) as u16 as i16, -5);
        assert_eq!(((l >> 16) & 0xFFFF) as u16 as i16, 300);
        let (w, _) = pack_wheel(120, 0, 0);
        assert_eq!((w >> 16) as u16, 120);
    }
}
