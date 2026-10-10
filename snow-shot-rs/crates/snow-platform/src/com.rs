//! COM 套间初始化的 RAII 守卫（仅 Windows 内部使用）。

use windows::Win32::System::Com::{COINIT, CoInitializeEx, CoUninitialize};
use windows::core::HRESULT;

/// 线程级 COM 初始化守卫：初始化成功才存在，Drop 时配对反初始化。
pub(crate) struct ComGuard;

impl ComGuard {
    /// 以指定套间模式初始化当前线程的 COM。
    ///
    /// # 参数
    /// - `mode`：套间模式，如 `COINIT_MULTITHREADED`。
    ///
    /// # 返回
    /// 成功（含 `S_FALSE` 即本线程已初始化）返回守卫；线程已是别的套间模式等情况返回失败的 `HRESULT`。
    pub(crate) fn init(mode: COINIT) -> Result<Self, HRESULT> {
        // SAFETY: 标准的线程级 COM 初始化；成功后由 Drop 配对 CoUninitialize。
        let hr = unsafe { CoInitializeEx(None, mode) };
        if hr.is_ok() { Ok(Self) } else { Err(hr) }
    }
}

impl Drop for ComGuard {
    /// 与成功的 `CoInitializeEx` 配对。
    fn drop(&mut self) {
        // SAFETY: 本守卫只在初始化成功后构造。
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};

    /// 同线程可嵌套初始化；套间模式冲突时返回 `RPC_E_CHANGED_MODE`（在新线程里测，不影响其它测试）。
    #[test]
    fn nested_init_ok_and_mode_conflict_is_error() {
        std::thread::spawn(|| {
            let outer = ComGuard::init(COINIT_APARTMENTTHREADED).expect("首次初始化应成功");
            let inner = ComGuard::init(COINIT_APARTMENTTHREADED).expect("同模式重复初始化应成功");
            let conflict = ComGuard::init(COINIT_MULTITHREADED);
            assert_eq!(conflict.err(), Some(RPC_E_CHANGED_MODE));
            drop(inner);
            drop(outer);
        })
        .join()
        .unwrap();
    }
}
