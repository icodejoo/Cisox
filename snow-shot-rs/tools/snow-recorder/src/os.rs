//! 平台相关的小工具：计时器精度与线程优先级。非 Windows 平台是空实现。

use crate::settings::{CaptureSched, ENV_CAPTURE_SCHED, ENV_MMCSS_TASK, parse_capture_sched, parse_mmcss_task};

/// 把进程设为 per-monitor DPI 感知，使选区与采集坐标都是物理像素（缩放显示器上否则会被虚拟化成逻辑尺寸）。
///
/// 非 Windows 平台不做任何事；已设置过则忽略失败。
///
/// # 示例
/// ```ignore
/// enable_dpi_awareness();
/// ```
pub fn enable_dpi_awareness() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        // SAFETY: 仅设置进程级 DPI 标志，无指针参数。
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
}

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

/// 采集线程调度守卫：持有 MMCSS 任务句柄，丢弃时（须在加入它的那个线程上）退出任务；其余情况什么也不做。
pub struct SchedGuard {
    /// MMCSS 任务句柄（未加入或失败为 `None`）。
    #[cfg(windows)]
    mmcss: Option<windows::Win32::Foundation::HANDLE>,
}

#[cfg(windows)]
impl Drop for SchedGuard {
    /// 退出 MMCSS 任务。
    fn drop(&mut self) {
        if let Some(handle) = self.mmcss.take() {
            // SAFETY: 句柄来自 `AvSetMmThreadCharacteristicsW`，只退出一次。
            unsafe {
                let _ = windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}

/// 拼出一行调度结果说明（日志与追踪元数据共用）。
///
/// # 参数
/// - `sched`：请求的调度方式。
/// - `task`：MMCSS 任务名。
/// - `priority`：线程优先级设置结果的描述（`default` 模式不用）。
/// - `mmcss`：MMCSS 加入结果（`Ok(任务序号)` 或失败原因）；未请求 MMCSS 时为 `None`。
///
/// # 示例
/// ```ignore
/// let s = describe_sched(CaptureSched::Mmcss, "Capture", "HIGHEST", Some(&Ok(3)));
/// assert!(s.contains("mmcss(Capture) 已生效"));
/// ```
pub fn describe_sched(sched: CaptureSched, task: &str, priority: &str, mmcss: Option<&Result<u32, String>>) -> String {
    if sched == CaptureSched::Default {
        return "default (线程优先级 HIGHEST，未用 MMCSS)".to_string();
    }
    let mut text = format!("{}: 线程优先级 {priority}", sched.name());
    match mmcss {
        Some(Ok(index)) => text.push_str(&format!("; mmcss({task}) 已生效 (taskIndex={index})")),
        Some(Err(e)) => text.push_str(&format!("; mmcss({task}) 失败: {e}，未加入 MMCSS")),
        None => {}
    }
    text
}

/// 对当前线程应用采集调度方式（只由采集线程调用）。
///
/// `default` 与历史行为完全一致（仅 [`raise_thread_priority`]）；其余方式失败只记录，不致命，回落到 HIGHEST / 不加入 MMCSS。
///
/// # 参数
/// - `sched`：调度方式。
/// - `task`：MMCSS 任务名（仅 `mmcss` 系列用）。
///
/// # 返回
/// `(守卫, 一行结果说明)`；守卫须由同一线程持有到线程退出。
///
/// # 示例
/// ```ignore
/// let (_guard, summary) = apply_capture_sched(CaptureSched::Mmcss, "Capture");
/// eprintln!("采集线程调度: {summary}");
/// ```
pub fn apply_capture_sched(sched: CaptureSched, task: &str) -> (SchedGuard, String) {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };
        use windows::core::PCWSTR;
        if sched == CaptureSched::Default {
            raise_thread_priority();
            return (SchedGuard { mmcss: None }, describe_sched(sched, task, "", None));
        }
        let priority = if sched.uses_time_critical() {
            // SAFETY: 只修改当前线程的优先级。
            match unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) } {
                Ok(()) => "TIME_CRITICAL 已生效".to_string(),
                Err(e) => {
                    raise_thread_priority();
                    format!("TIME_CRITICAL 失败({e})，回落 HIGHEST")
                }
            }
        } else {
            raise_thread_priority();
            "HIGHEST".to_string()
        };
        let mut guard = SchedGuard { mmcss: None };
        let mmcss = sched.uses_mmcss().then(|| {
            let wide: Vec<u16> = task.encode_utf16().chain(std::iter::once(0)).collect();
            let mut index = 0u32;
            // SAFETY: `wide` 以 0 结尾且在调用期间有效；`index` 是局部变量。
            match unsafe { AvSetMmThreadCharacteristicsW(PCWSTR(wide.as_ptr()), &mut index) } {
                Ok(handle) => {
                    guard.mmcss = Some(handle);
                    Ok(index)
                }
                Err(e) => Err(e.to_string()),
            }
        });
        // SAFETY: 只读取当前线程的优先级。
        let readback = unsafe { GetThreadPriority(GetCurrentThread()) };
        let text = describe_sched(sched, task, &format!("{priority}（读回 {readback}）"), mmcss.as_ref());
        (guard, text)
    }
    #[cfg(not(windows))]
    {
        let _ = (sched, task);
        (SchedGuard {}, "default (非 Windows，不适用)".to_string())
    }
}

/// 按环境变量为采集线程应用调度方式，并把结果打一行日志、写进帧追踪元数据。
///
/// 环境变量见 [`ENV_CAPTURE_SCHED`] 与 [`ENV_MMCSS_TASK`]；取值无法识别时回落 `default` 并提示。
///
/// # 返回
/// 守卫；须在采集线程里持有到线程退出。
///
/// # 示例
/// ```ignore
/// let _sched = apply_capture_sched_from_env();
/// ```
pub fn apply_capture_sched_from_env() -> SchedGuard {
    let raw = std::env::var(ENV_CAPTURE_SCHED).ok();
    let sched = match parse_capture_sched(raw.as_deref()) {
        Ok(s) => s,
        Err(bad) => {
            eprintln!("{ENV_CAPTURE_SCHED}={bad} 无法识别，按 default 处理");
            CaptureSched::Default
        }
    };
    let task = parse_mmcss_task(std::env::var(ENV_MMCSS_TASK).ok().as_deref());
    let (guard, summary) = apply_capture_sched(sched, &task);
    eprintln!("采集线程调度: {summary}");
    crate::frametrace::note("capture_sched", sched.name());
    crate::frametrace::note("capture_sched_detail", &summary);
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 结果说明：default 固定文案；mmcss 成功/失败各自带任务名与原因。
    #[test]
    fn describe_sched_covers_outcomes() {
        assert!(describe_sched(CaptureSched::Default, "Capture", "", None).starts_with("default"));
        let ok = describe_sched(CaptureSched::Mmcss, "Capture", "HIGHEST", Some(&Ok(7)));
        assert!(ok.starts_with("mmcss:") && ok.contains("mmcss(Capture) 已生效 (taskIndex=7)"));
        let bad = describe_sched(CaptureSched::MmcssTimeCritical, "Games", "TIME_CRITICAL 已生效", Some(&Err("拒绝访问".into())));
        assert!(bad.contains("mmcss+timecritical") && bad.contains("mmcss(Games) 失败: 拒绝访问"));
        let tc = describe_sched(CaptureSched::TimeCritical, "Capture", "TIME_CRITICAL 已生效", None);
        assert!(!tc.contains("mmcss("));
    }

    /// 计时器守卫可创建与释放，优先级调整不崩溃。
    #[test]
    fn guards_are_harmless() {
        let _timer = TimerGuard::request(1);
        raise_thread_priority();
    }
}
