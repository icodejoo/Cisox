//! 进程内存探针：读取进程的工作集峰值 / 当前值（性能探针与验收用）。

/// 进程内存快照（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    /// 工作集峰值。
    pub peak_working_set: u64,
    /// 当前工作集。
    pub working_set: u64,
    /// 提交内存（私有页）当前值。
    pub private_bytes: u64,
    /// 提交内存峰值。
    pub peak_private_bytes: u64,
}

/// 读取当前进程的内存快照。
///
/// # 返回
/// 快照；非 Windows 或调用失败返回 `None`。
///
/// # 示例
/// ```ignore
/// let m = snow_platform::process_mem::current_process_memory().unwrap();
/// assert!(m.peak_working_set >= m.working_set);
/// ```
pub fn current_process_memory() -> Option<ProcessMemory> {
    #[cfg(windows)]
    {
        win::query(None)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 读取指定进程（按 PID）的内存快照。
///
/// # 参数
/// - `pid`：进程 ID。
///
/// # 返回
/// 快照；进程不存在、无权限、非 Windows 时返回 `None`。
pub fn process_memory(pid: u32) -> Option<ProcessMemory> {
    #[cfg(windows)]
    {
        win::query(Some(pid))
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

#[cfg(windows)]
mod win {
    use super::ProcessMemory;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// 查询进程内存；`pid` 为空表示当前进程。
    pub fn query(pid: Option<u32>) -> Option<ProcessMemory> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        counters.cb = size;
        // SAFETY: 句柄来自 GetCurrentProcess / OpenProcess，用后关闭；缓冲区大小与 cb 一致。
        unsafe {
            let (handle, owned) = match pid {
                None => (GetCurrentProcess(), false),
                Some(pid) => (
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?,
                    true,
                ),
            };
            let ok = GetProcessMemoryInfo(
                handle,
                std::ptr::addr_of_mut!(counters).cast(),
                size,
            )
            .is_ok();
            if owned {
                let _ = CloseHandle(handle);
            }
            ok.then_some(ProcessMemory {
                peak_working_set: counters.PeakWorkingSetSize as u64,
                working_set: counters.WorkingSetSize as u64,
                private_bytes: counters.PrivateUsage as u64,
                peak_private_bytes: counters.PeakPagefileUsage as u64,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 当前进程快照应可读且峰值不小于当前值。
    #[cfg(windows)]
    #[test]
    fn current_process_snapshot() {
        let m = current_process_memory().expect("应能读取当前进程内存");
        assert!(m.working_set > 0);
        assert!(m.peak_working_set >= m.working_set);
    }

    /// 不存在的 PID 返回 None（不 panic）。
    #[test]
    fn missing_pid_is_none() {
        assert!(process_memory(u32::MAX - 1).is_none());
    }
}
