//! 平台相关的小工具：计时器精度与线程优先级。非 Windows 平台是空实现。

/// 计时器精度守卫：录制期间把系统计时器精度提到指定毫秒（否则等待最多抖 15ms）。
///
/// 非 Windows 平台不做任何事。
pub struct TimerGuard {
    /// 已请求的毫秒数（`None` 表示未生效）。
    #[cfg(windows)]
    requested: Option<u32>,
}

impl TimerGuard {
    /// 请求 `ms` 毫秒的计时器精度；失败时守卫仍可用，只是精度较差。
    ///
    /// # 参数
    /// - `ms`：期望的精度（毫秒）。
    ///
    /// # 示例
    /// ```ignore
    /// let _timer = TimerGuard::request(1);
    /// ```
    pub fn request(ms: u32) -> Self {
        #[cfg(windows)]
        {
            use windows::Win32::Media::{TIMERR_NOERROR, timeBeginPeriod};
            // SAFETY: 仅设置进程级计时器精度请求，drop 时成对释放。
            let ok = unsafe { timeBeginPeriod(ms) } == TIMERR_NOERROR;
            Self { requested: ok.then_some(ms) }
        }
        #[cfg(not(windows))]
        {
            let _ = ms;
            Self {}
        }
    }
}

#[cfg(windows)]
impl Drop for TimerGuard {
    /// 释放计时器精度请求。
    fn drop(&mut self) {
        if let Some(ms) = self.requested {
            // SAFETY: 与 `request` 成对。
            unsafe { windows::Win32::Media::timeEndPeriod(ms) };
        }
    }
}

/// 把当前线程优先级提到最高档（采集/合成/编码线程被抢占会让采集合并多次呈现或让编码积压，造成丢帧）。
///
/// 失败忽略；非 Windows 平台不做任何事。
pub fn raise_thread_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
        // SAFETY: 只修改当前线程的优先级。
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 计时器守卫可创建与释放，优先级调整不崩溃。
    #[test]
    fn guards_are_harmless() {
        let _timer = TimerGuard::request(1);
        raise_thread_priority();
    }
}
