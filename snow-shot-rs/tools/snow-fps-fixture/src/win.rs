//! Win32 封装：显示器枚举与无边框置顶窗口（绘制由 gpu 模块负责）。

use snow_fps_fixture::{MonitorInfo, Rect};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassExW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, UnregisterClassW, WM_ERASEBKGND,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// MONITORINFOF_PRIMARY 标志。
const MONITORINFOF_PRIMARY: u32 = 1;
/// 窗口类名。
const CLASS_NAME: PCWSTR = w!("SnowFpsFixtureWindow");

/// 测试期间让显示器保持唤醒、不进入屏保/睡眠（只影响当前线程的执行状态，线程/进程结束即自动恢复）。
pub fn keep_display_awake() {
    // SAFETY: 仅设置当前线程的执行状态标志，无指针参数。
    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
    }
}

/// 设置进程为 per-monitor DPI 感知，使所有坐标都是物理像素；失败忽略（已设置过）。
pub fn set_dpi_aware() {
    // SAFETY: 仅设置进程级 DPI 标志，无指针参数。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// 枚举回调：把显示器信息追加进 `LPARAM` 指向的向量。
unsafe extern "system" fn enum_proc(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
    // SAFETY: data 是 enumerate_monitors 传入的 &mut Vec<MonitorInfo>，回调期间有效。
    let list = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() },
        ..Default::default()
    };
    // SAFETY: MONITORINFOEXW 以 MONITORINFO 开头，cbSize 已按扩展结构设置。
    if unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
        let r = info.monitorInfo.rcMonitor;
        let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        list.push(MonitorInfo {
            device: String::from_utf16_lossy(&info.szDevice[..len]),
            rect: Rect {
                x: r.left,
                y: r.top,
                w: (r.right - r.left).max(0) as u32,
                h: (r.bottom - r.top).max(0) as u32,
            },
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    true.into()
}

/// 枚举全部显示器（物理像素坐标，需先调用 [`set_dpi_aware`]）。
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut list: Vec<MonitorInfo> = Vec::new();
    // SAFETY: 回调在本调用返回前执行完毕，list 在此期间存活且仅被回调独占访问。
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM((&mut list as *mut Vec<MonitorInfo>) as isize));
    }
    list
}

/// 窗口过程：吞掉背景擦除，其余交给系统。
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    // SAFETY: 参数原样转交系统默认过程。
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// 夹具窗口：无边框置顶、不抢焦点。
pub struct FixtureWindow {
    /// 窗口句柄。
    hwnd: HWND,
}

impl FixtureWindow {
    /// 在指定矩形创建并显示窗口。
    ///
    /// # 参数
    /// - `rect`：窗口矩形（调用方已校验落在目标显示器内）。
    ///
    /// # 返回
    /// 窗口；Win32 失败返回原因。
    pub fn create(rect: Rect) -> Result<Self, String> {
        let (w, h) = (i32::try_from(rect.w).map_err(|e| e.to_string())?, i32::try_from(rect.h).map_err(|e| e.to_string())?);
        // SAFETY: 全部为 Win32 对象创建，参数在本函数内构造且有效；失败路径返回错误。
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 {
                return Err("RegisterClassExW 失败".into());
            }
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("snow-fps-fixture"),
                WS_POPUP,
                rect.x,
                rect.y,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(Self { hwnd })
        }
    }

    /// 窗口句柄（供交换链使用）。
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 泵一次消息队列（保持窗口响应）。
    pub fn pump(&self) {
        let mut msg = MSG::default();
        // SAFETY: msg 为局部结构，仅取本线程消息。
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for FixtureWindow {
    /// 销毁窗口并注销窗口类。
    fn drop(&mut self) {
        // SAFETY: 句柄由 create 创建，仅在此释放一次。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if let Ok(instance) = GetModuleHandleW(None) {
                let _ = UnregisterClassW(CLASS_NAME, Some(instance.into()));
            }
        }
    }
}

/// 枚举全部 DXGI 输出并逐行返回描述：`adapter=<i> output=<j> <设备名> rect=<x,y,wxh>`（只读，不创建窗口）。
///
/// # 返回
/// 文本行；工厂创建失败返回原因。
pub fn dxgi_output_lines() -> Result<Vec<String>, String> {
    // SAFETY: 仅调用 DXGI 枚举接口，所有接口对象由 windows crate 管理生命周期。
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(desc) = output.GetDesc() {
                    let len = desc.DeviceName.iter().position(|&c| c == 0).unwrap_or(desc.DeviceName.len());
                    let r = desc.DesktopCoordinates;
                    lines.push(format!(
                        "adapter={a} output={o} {} rect={},{},{}x{}",
                        String::from_utf16_lossy(&desc.DeviceName[..len]),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top
                    ));
                }
                o += 1;
            }
            a += 1;
        }
        Ok(lines)
    }
}
