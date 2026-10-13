//! Windows 原生实现（`windows` 0.62，与 gpui 同版本）：消息循环、唤醒器、弹出菜单主题。

use crate::error::ShellError;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    TranslateMessage, WM_APP, WM_USER,
};

/// 唤醒后台消息循环用的私有消息号。
const WM_SHELL_WAKE: u32 = WM_APP + 0x51;

/// 把 `windows` 错误包成 [`ShellError::Platform`]。
fn platform_err(what: &str, err: impl std::fmt::Display) -> ShellError {
    ShellError::Platform(format!("{what}: {err}"))
}

/// 后台消息循环的唤醒/退出句柄，可跨线程使用。
#[derive(Debug, Clone, Copy)]
pub struct LoopWaker {
    /// 目标线程 ID。
    thread_id: u32,
}

impl LoopWaker {
    /// 唤醒循环，使其立即执行一次 `tick`；目标线程已退出时静默忽略。
    pub fn wake(&self) {
        // SAFETY: 目标线程已创建消息队列；失败（线程已退出）可忽略。
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_SHELL_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// 为当前线程创建消息队列并返回其唤醒句柄；必须先于任何跨线程 `wake`。
///
/// # 返回
/// 指向当前线程的唤醒句柄，可复制到其他线程使用。
pub fn current_thread_waker() -> LoopWaker {
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
///
/// # 参数
/// - `tick`：每条消息之后调用一次；返回 `false` 结束循环。
pub fn run_message_loop(mut tick: impl FnMut() -> bool) {
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

/// 在光标处弹出菜单，阻塞到菜单关闭，返回被选中项在菜单里的位置。
///
/// 与 `tray-icon` 自带的弹出行为一致：窗口先置前台（点菜单外部才会自动收起），菜单底边 / 左边对齐光标，
/// 关闭后补投一条 `WM_NULL`（外壳文档对通知区菜单的要求）。
///
/// # 参数
/// - `hwnd`：菜单的所有者窗口（托盘隐藏窗口，必须由当前线程创建）。
/// - `hmenu`：弹出菜单句柄（`muda::Menu::hpopupmenu`），本函数不销毁它。
///
/// # 返回
/// 被选中项的位置（0 起，与追加顺序一致）；取消、被系统关闭或选中的是子菜单时为 `None`。
///
/// # 示例
/// ```ignore
/// if let Some(pos) = track_popup_menu_at_cursor(tray_hwnd, menu.hpopupmenu()) {
///     println!("选中第 {pos} 项");
/// }
/// ```
pub fn track_popup_menu_at_cursor(hwnd: isize, hmenu: isize) -> Option<usize> {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetMenuItemCount, GetMenuItemID, HMENU, PostMessageW, SetForegroundWindow,
        TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, WM_NULL,
    };
    let owner = HWND(hwnd as *mut _);
    let menu = HMENU(hmenu as *mut _);
    let mut at = POINT::default();
    // SAFETY: 句柄由调用方保证有效且属于当前线程；失败时按取消处理。
    let picked = unsafe {
        let _ = GetCursorPos(&mut at);
        let _ = SetForegroundWindow(owner);
        let picked = TrackPopupMenu(
            menu,
            TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD,
            at.x,
            at.y,
            None,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        picked.0
    };
    if picked <= 0 {
        return None;
    }
    // SAFETY: 菜单仍有效；位置在 `0..count` 内。
    unsafe {
        (0..GetMenuItemCount(Some(menu)).max(0))
            .find(|&pos| GetMenuItemID(menu, pos) as i32 == picked)
    }
    .map(|pos| pos as usize)
}

/// 设置进程内弹出菜单（托盘右键菜单等）的深浅色。
///
/// # 参数
/// - `dark`：`Some(true)` 深色、`Some(false)` 浅色、`None` 跟随系统。
///
/// # 返回
/// 成功为 `Ok`；`uxtheme.dll` 加载失败或缺少序号函数时返回 `Platform` 错误。
pub fn set_popup_menu_dark(dark: Option<bool>) -> Result<(), ShellError> {
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
        let lib =
            LoadLibraryW(w!("uxtheme.dll")).map_err(|e| platform_err("LoadLibrary(uxtheme)", e))?;
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
