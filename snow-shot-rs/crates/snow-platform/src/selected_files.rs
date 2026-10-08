//! 读取前台资源管理器（或桌面）里当前选中的文件路径，供「贴选中的文件」使用。
//!
//! Windows 实现走 `IShellWindows` → `IShellBrowser` → `IFolderView2`，只认前台窗口里可见的那个视图，
//! 与旧版 `selectedfiles.cpp` 的规则一致；其它平台与取不到时返回空列表。

use std::path::PathBuf;

/// 读取前台资源管理器 / 桌面里选中的文件。
///
/// 内部会在当前线程初始化 COM（单线程套间），应在工作线程调用，不要在 UI 线程里调用（会阻塞）。
///
/// # 返回
/// 选中的文件系统路径；前台不是资源管理器 / 桌面、没有选中项或读取失败时为空。
///
/// ```ignore
/// let paths = foreground_selected_files();
/// ```
pub fn foreground_selected_files() -> Vec<PathBuf> {
    imp::foreground_selected_files()
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, IDispatch,
    };
    use windows::Win32::System::Variant::{VARIANT, VT_I4};
    use windows::Win32::UI::Shell::{
        IFolderView2, IShellBrowser, IShellItemArray, IShellView, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser,
        SIGDN_FILESYSPATH, SVGIO_SELECTION, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetForegroundWindow, IsChild, IsWindowVisible,
    };
    use windows::core::{BOOL, Interface};

    /// 桌面使用的 CSIDL 值。
    const CSIDL_DESKTOP: i32 = 0;

    /// 读取窗口类名。
    fn class_name(window: HWND) -> String {
        let mut buffer = [0u16; 128];
        // SAFETY: buffer 是有效的可写缓冲，长度由切片给出。
        let len = unsafe { GetClassNameW(window, &mut buffer) };
        String::from_utf16_lossy(&buffer[..len.max(0) as usize])
    }

    /// 子窗口枚举的搜索状态。
    struct Search<'a> {
        /// 要找的类名。
        name: &'a str,
        /// 找到的窗口。
        result: Option<HWND>,
    }

    /// `EnumChildWindows` 回调：记录第一个可见且类名匹配的子窗口。
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 来自 `visible_child` 传入的 &mut Search，枚举期间一直有效。
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        // SAFETY: child 由系统枚举提供。
        if unsafe { IsWindowVisible(child) }.as_bool() && class_name(child) == search.name {
            search.result = Some(child);
            return BOOL(0);
        }
        BOOL(1)
    }

    /// 在子窗口里找第一个可见且类名匹配的窗口。
    fn visible_child(parent: HWND, name: &str) -> Option<HWND> {
        let mut search = Search { name, result: None };
        // SAFETY: 回调只访问 search，枚举同步完成，指针在此期间有效。
        let _ = unsafe { EnumChildWindows(Some(parent), Some(visit), LPARAM(&mut search as *mut Search as isize)) };
        search.result
    }

    /// 从一个资源管理器视图读取选中项路径；视图窗口不是目标可见视图时返回 `None`。
    fn files_from_view(dispatch: &IDispatch, target_view: HWND, target_tab: Option<HWND>) -> Option<Vec<PathBuf>> {
        let provider: windows::Win32::System::Com::IServiceProvider = dispatch.cast().ok()?;
        // SAFETY: COM 调用，参数均为有效接口 / 常量。
        let browser: IShellBrowser = unsafe { provider.QueryService(&SID_STopLevelBrowser) }.ok()?;
        // SAFETY: 同上。
        let view: IShellView = unsafe { browser.QueryActiveShellView() }.ok()?;
        // SAFETY: 同上。
        let view_window = unsafe { view.GetWindow() }.ok()?;
        // SAFETY: 窗口句柄来自系统。
        if !unsafe { IsWindowVisible(view_window) }.as_bool() || view_window != target_view {
            return None;
        }
        if let Some(tab) = target_tab {
            // SAFETY: 两个句柄都来自系统。
            if !unsafe { IsChild(tab, view_window) }.as_bool() {
                return None;
            }
        }
        let folder: IFolderView2 = view.cast().ok()?;
        // SAFETY: COM 调用。
        let items: IShellItemArray = unsafe { folder.Items(SVGIO_SELECTION) }.ok()?;
        // SAFETY: COM 调用。
        let count = unsafe { items.GetCount() }.ok()?;
        let mut paths = Vec::new();
        for index in 0..count {
            // SAFETY: COM 调用；返回的字符串由我们用 CoTaskMemFree 释放。
            let Ok(item) = (unsafe { items.GetItemAt(index) }) else {
                continue;
            };
            // SAFETY: 同上。
            let Ok(name) = (unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }) else {
                continue;
            };
            // SAFETY: name 是以 0 结尾的有效宽字符串。
            let text = unsafe { name.to_string() }.unwrap_or_default();
            // SAFETY: name 由 GetDisplayName 以 CoTaskMem 分配。
            unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
            if !text.is_empty() {
                paths.push(PathBuf::from(text));
            }
        }
        Some(paths)
    }

    /// 配对 `CoInitializeEx` / `CoUninitialize` 的守卫。
    struct ComGuard;

    impl Drop for ComGuard {
        fn drop(&mut self) {
            // SAFETY: 与成功的 CoInitializeEx 配对。
            unsafe { CoUninitialize() };
        }
    }

    /// Windows 实现。
    pub fn foreground_selected_files() -> Vec<PathBuf> {
        // SAFETY: 无前置条件。
        let window = unsafe { GetForegroundWindow() };
        if window.0.is_null() {
            return Vec::new();
        }
        let class = class_name(window);
        let desktop = class == "Progman" || class == "WorkerW";
        if !desktop && class != "CabinetWClass" && class != "ExploreWClass" {
            return Vec::new();
        }
        let Some(view) = visible_child(window, "SHELLDLL_DefView") else {
            return Vec::new();
        };
        let tab = visible_child(window, "ShellTabWindowClass");
        // SAFETY: 初始化当前线程的 COM；配对的 CoUninitialize 由 ComGuard 释放。
        if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
            return Vec::new();
        }
        let _guard = ComGuard;
        // SAFETY: COM 调用。
        let created: windows::core::Result<IShellWindows> =
            unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) };
        let Ok(shell_windows) = created else {
            return Vec::new();
        };
        if desktop {
            let mut location = VARIANT::default();
            // SAFETY: 直接设置 VT_I4 变体的类型与值。
            unsafe {
                let inner = &mut location.Anonymous.Anonymous;
                inner.vt = VT_I4;
                inner.Anonymous.lVal = CSIDL_DESKTOP;
            }
            let root = VARIANT::default();
            let mut handle = 0i32;
            // SAFETY: COM 调用，输出参数为有效局部变量。
            let found =
                unsafe { shell_windows.FindWindowSW(&location, &root, SWC_DESKTOP, &mut handle, SWFO_NEEDDISPATCH) };
            return found
                .ok()
                .and_then(|d| files_from_view(&d, view, tab))
                .unwrap_or_default();
        }
        // SAFETY: COM 调用。
        let count = unsafe { shell_windows.Count() }.unwrap_or(0);
        for index in 0..count {
            let position = VARIANT::from(index);
            // SAFETY: COM 调用。
            let Ok(dispatch) = (unsafe { shell_windows.Item(&position) }) else {
                continue;
            };
            let Ok(app) = dispatch.cast::<IWebBrowserApp>() else {
                continue;
            };
            // SAFETY: COM 调用。
            let Ok(hwnd) = (unsafe { app.HWND() }) else {
                continue;
            };
            if hwnd.0 != window.0 as isize {
                continue;
            }
            if let Some(paths) = files_from_view(&dispatch, view, tab)
                && !paths.is_empty()
            {
                return paths;
            }
        }
        Vec::new()
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    /// 非 Windows 平台没有实现。
    pub fn foreground_selected_files() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试进程里前台通常不是资源管理器：不崩溃，返回空或实际选中项。
    #[test]
    fn probe_is_safe() {
        let _ = foreground_selected_files();
    }
}
