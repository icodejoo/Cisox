//! 进程加固：宿主 `main` 的第一件事，早于创建任何窗口 / 线程。
//!
//! 依据 `docs/research/tray-host-minimal.md` 的实测：
//! 1. `SetProcessMitigationPolicy(ProcessExtensionPointDisablePolicy)` 禁用扩展点（旧式输入法、AppInit DLL、
//!    全局钩子注入）：本机宿主启动态提交 2.80MB → 1.64MB，弹一次托盘菜单后 3.3MB → 2.1MB，
//!    并且挡住第三方输入法（搜狗）被注入宿主——不做的话宿主一弹菜单就会涨到 36MB。官方文档要求
//!    “进程初始化期间应用”，所以必须在任何窗口创建前。
//! 2. `ImmDisableIME(-1)`：对**整个进程**（含之后创建的托盘 / 热键线程）禁用 IME。`(0)` 是线程级的，
//!    对托盘线程上的窗口无效，所以必须用 `-1`。宿主没有文字输入，没有副作用。
//! 3. 进程声明 PerMonitorV2 DPI 感知：托盘菜单在高 DPI 下不被位图拉伸；鼠标手势钩子（阶段 3）的坐标
//!    也要求不被虚拟化。
//!
//! 全部失败都不致命（比如被企业端点软件限制），只记入返回值，由调用方在日志初始化后写日志。

/// 加固结果（日志初始化前拿不到 logger，所以先返回，稍后再记）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardenReport {
    /// 禁用扩展点是否生效。
    pub extension_points_disabled: bool,
    /// `ImmDisableIME(-1)` 是否返回成功。
    pub ime_disabled: bool,
    /// PerMonitorV2 DPI 感知是否设置成功（已设置过也算成功）。
    pub dpi_aware: bool,
}

/// 扩展点禁用策略的“禁用扩展点”位（`DisableExtensionPoints`，位 0）。
#[cfg(windows)]
const DISABLE_EXTENSION_POINTS_FLAG: u32 = 1;

/// 应用进程加固。必须是 `main` 的第一行：任何窗口、线程创建之前。
///
/// # 返回
/// 各项是否生效；非 Windows 平台全部为 `false`。
///
/// ```no_run
/// let report = snow_tray_host::harden::harden_process();
/// println!("{report:?}");
/// ```
#[cfg(windows)]
pub fn harden_process() -> HardenReport {
    use windows_sys::Win32::System::Threading::{
        ProcessExtensionPointDisablePolicy, SetProcessMitigationPolicy,
    };
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::Ime::ImmDisableIME;

    let policy = DISABLE_EXTENSION_POINTS_FLAG;
    // SAFETY: 缓冲区是一个 u32（`PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY` 的位域，大小 4 字节），
    // 在调用期间有效。
    let extension_points_disabled = unsafe {
        SetProcessMitigationPolicy(
            ProcessExtensionPointDisablePolicy,
            std::ptr::from_ref(&policy).cast(),
            std::mem::size_of::<u32>(),
        ) != 0
    };
    // SAFETY: 参数 `-1`（`u32::MAX`）表示整个进程；无其它前置条件。
    let ime_disabled = unsafe { ImmDisableIME(u32::MAX) != 0 };
    // SAFETY: 常量上下文值，无其它前置条件；已设置过会返回失败，不影响结果。
    let dpi_aware =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 };
    HardenReport {
        extension_points_disabled,
        ime_disabled,
        dpi_aware,
    }
}

/// 非 Windows：什么都不做。
#[cfg(not(windows))]
pub fn harden_process() -> HardenReport {
    HardenReport {
        extension_points_disabled: false,
        ime_disabled: false,
        dpi_aware: false,
    }
}

/// 读回本进程的扩展点禁用策略，确认加固真的生效。
///
/// # 返回
/// 策略里“禁用扩展点”位是否为 1；查询失败或非 Windows 返回 `false`。
///
/// ```no_run
/// snow_tray_host::harden::harden_process();
/// assert!(snow_tray_host::harden::extension_points_disabled());
/// ```
#[cfg(windows)]
pub fn extension_points_disabled() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessMitigationPolicy, ProcessExtensionPointDisablePolicy,
    };
    let mut policy: u32 = 0;
    // SAFETY: 出参是一个 u32，大小与传入长度一致；伪句柄 `GetCurrentProcess` 无需关闭。
    let ok = unsafe {
        GetProcessMitigationPolicy(
            GetCurrentProcess(),
            ProcessExtensionPointDisablePolicy,
            std::ptr::from_mut(&mut policy).cast(),
            std::mem::size_of::<u32>(),
        ) != 0
    };
    ok && policy & DISABLE_EXTENSION_POINTS_FLAG != 0
}

/// 非 Windows：恒为 `false`。
#[cfg(not(windows))]
pub fn extension_points_disabled() -> bool {
    false
}
