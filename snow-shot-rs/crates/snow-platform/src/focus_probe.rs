//! 前台焦点探测：读取“当前键盘焦点落在什么控件上、目标进程权限是否更高”。
//!
//! 这里只负责采集原始读数（[`FocusReading`]），是否可键入的判定是纯逻辑，放在调用方并可离屏测试。
//! Windows 用 UI Automation 取焦点元素，辅以 `GetGUIThreadInfo` 的系统插入符，
//! 再用令牌完整性级别对比判断 UIPI。UIA 调用可能被无响应的目标程序拖住，
//! 调用方应在后台线程执行 [`read_focus`] 并自行设超时。

/// UIA 控件类型：编辑框。
pub const CONTROL_TYPE_EDIT: i32 = 50004;
/// UIA 控件类型：文档。
pub const CONTROL_TYPE_DOCUMENT: i32 = 50030;

/// UIA 焦点元素的读数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiaElement {
    /// UIA 控件类型编号（如 [`CONTROL_TYPE_EDIT`]）。
    pub control_type: i32,
    /// 是否启用。
    pub enabled: bool,
    /// 是否正持有键盘焦点。
    pub has_keyboard_focus: bool,
    /// 是否可获键盘焦点。
    pub keyboard_focusable: bool,
    /// 是否密码框。
    pub password: bool,
    /// ValuePattern 的只读状态；`None` 表示不支持 ValuePattern。
    pub value_read_only: Option<bool>,
    /// 是否支持 TextPattern。
    pub has_text_pattern: bool,
}

/// 一次焦点探测的原始读数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusReading {
    /// 前台窗口句柄（数值）；没有前台窗口为 `None`。
    pub foreground: Option<isize>,
    /// 前台窗口所属进程 ID（未知为 0）。
    pub foreground_pid: u32,
    /// 前台进程完整性级别是否高于本进程；`None` 表示读不出（按不确定处理）。
    pub target_above_us: Option<bool>,
    /// 前台线程是否有系统插入符（传统 Win32 控件的辅助证据）。
    pub caret: bool,
    /// UIA 结果：`Err` 为 UIA 不可用或调用出错，`Ok(None)` 为没有焦点元素。
    pub uia: Result<Option<UiaElement>, String>,
}

/// 读取当前前台焦点。
///
/// # 返回
/// 原始读数；任何一步失败都落进对应字段，不会 panic。非 Windows 的 `uia` 恒为 `Err`。
pub fn read_focus() -> FocusReading {
    #[cfg(windows)]
    {
        win::read_focus()
    }
    #[cfg(not(windows))]
    {
        FocusReading {
            foreground: None,
            foreground_pid: 0,
            target_above_us: None,
            caret: false,
            uia: Err("当前平台不支持焦点探测".to_string()),
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{FocusReading, UiaElement};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, IUIAutomationValuePattern,
        UIA_TextPatternId, UIA_ValuePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
    };

    /// 读取完整性级别（RID）；失败返回 `None`。
    fn integrity_rid(process: HANDLE) -> Option<u32> {
        let mut token = HANDLE::default();
        // SAFETY: process 是有效进程句柄，token 是有效的输出位置。
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
        let rid = (|| {
            let mut needed = 0u32;
            // SAFETY: 第一次调用只为取所需缓冲区大小，预期以“缓冲区不足”失败。
            let _ =
                unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed) };
            if needed == 0 {
                return None;
            }
            // 用 u64 对齐的缓冲区，保证按 TOKEN_MANDATORY_LABEL 解引用时对齐合法
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            // SAFETY: buffer 至少 needed 字节。
            unsafe {
                GetTokenInformation(
                    token,
                    TokenIntegrityLevel,
                    Some(buffer.as_mut_ptr().cast()),
                    needed,
                    &mut needed,
                )
            }
            .ok()?;
            // SAFETY: 缓冲区已被系统写成 TOKEN_MANDATORY_LABEL，SID 指针在缓冲区内有效。
            unsafe {
                let label = &*(buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
                let sid = label.Label.Sid;
                let count = *GetSidSubAuthorityCount(sid);
                if count == 0 {
                    return None;
                }
                Some(*GetSidSubAuthority(sid, u32::from(count) - 1))
            }
        })();
        // SAFETY: token 由 OpenProcessToken 打开，只关闭一次。
        let _ = unsafe { CloseHandle(token) };
        rid
    }

    /// 目标进程完整性是否高于本进程；任何一步读不出返回 `None`。
    ///
    /// 打不开目标进程（受保护进程等）按“读不出”处理，调用方一律视为不确定。
    fn target_above_us(pid: u32) -> Option<bool> {
        // SAFETY: 只请求受限查询权限。
        let target = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let theirs = integrity_rid(target);
        // SAFETY: target 由 OpenProcess 打开，只关闭一次。
        let _ = unsafe { CloseHandle(target) };
        // SAFETY: 伪句柄，无需关闭。
        let ours = integrity_rid(unsafe { GetCurrentProcess() });
        Some(theirs? > ours?)
    }

    /// 线程级 COM 初始化守卫：本线程初始化成功才在退出时反初始化。
    struct ComGuard(bool);

    impl ComGuard {
        /// 以多线程套间初始化 COM；线程已是别的套间模式时不强改（也不反初始化）。
        fn init() -> Self {
            // SAFETY: 标准的线程级 COM 初始化。
            Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for ComGuard {
        /// 配对反初始化。
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: 与成功的 CoInitializeEx 配对。
                unsafe { CoUninitialize() };
            }
        }
    }

    /// 读 UIA 焦点元素。
    fn read_uia() -> Result<Option<UiaElement>, String> {
        let _com = ComGuard::init();
        // SAFETY: 进程内 UIA 对象，COM 已初始化。
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("创建 UIA 失败: {e}"))?;
        // SAFETY: 查询当前焦点元素；失败（含无焦点）按“不确定”上报。
        let Some(element) = (unsafe { automation.GetFocusedElement() }).ok() else {
            return Err("取焦点元素失败".to_string());
        };
        // SAFETY: 以下均为对有效元素的只读属性查询。
        unsafe {
            let control_type = element.CurrentControlType().map_err(|e| e.to_string())?.0;
            let flag = |r: windows::core::Result<windows::core::BOOL>| {
                r.map(|b| b.as_bool()).unwrap_or(false)
            };
            let value_read_only = element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|p| p.CurrentIsReadOnly().ok())
                .map(|b| b.as_bool());
            let has_text_pattern = element
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .is_ok();
            Ok(Some(UiaElement {
                control_type,
                enabled: flag(element.CurrentIsEnabled()),
                has_keyboard_focus: flag(element.CurrentHasKeyboardFocus()),
                keyboard_focusable: flag(element.CurrentIsKeyboardFocusable()),
                password: flag(element.CurrentIsPassword()),
                value_read_only,
                has_text_pattern,
            }))
        }
    }

    /// 前台线程是否有系统插入符。
    fn has_caret(thread_id: u32) -> bool {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: info 已设置 cbSize，是有效的输出位置。
        unsafe { GetGUIThreadInfo(thread_id, &mut info) }.is_ok() && !info.hwndCaret.0.is_null()
    }

    /// 汇总一次读数。
    pub(super) fn read_focus() -> FocusReading {
        // SAFETY: 只读查询前台窗口。
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return FocusReading {
                foreground: None,
                foreground_pid: 0,
                target_above_us: None,
                caret: false,
                uia: Ok(None),
            };
        }
        let mut pid = 0u32;
        // SAFETY: hwnd 有效，pid 是有效输出位置。
        let thread_id = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        FocusReading {
            foreground: Some(hwnd.0 as isize),
            foreground_pid: pid,
            target_above_us: if pid == 0 { None } else { target_above_us(pid) },
            caret: has_caret(thread_id),
            uia: read_uia(),
        }
    }
}
