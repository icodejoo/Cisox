//! 进程内存快照（Windows 工作集；其他平台返回 0）。

/// 进程内存快照（字节）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemSnapshot {
    /// 当前工作集。
    pub working_set: u64,
    /// 峰值工作集。
    pub peak_working_set: u64,
}

#[cfg(windows)]
mod imp {
    use super::MemSnapshot;

    /// Win32 `PROCESS_MEMORY_COUNTERS`。
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 当前进程伪句柄。
        fn GetCurrentProcess() -> isize;
        /// 读取进程内存计数（kernel32 导出的 psapi 入口）。
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    /// 读取当前进程内存快照，失败返回全 0。
    pub fn snapshot() -> MemSnapshot {
        let mut c = ProcessMemoryCounters {
            cb: size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        // SAFETY: c 是合法的、大小已声明的输出缓冲；GetCurrentProcess 无前置条件。
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        if ok == 0 {
            return MemSnapshot::default();
        }
        MemSnapshot {
            working_set: c.working_set_size as u64,
            peak_working_set: c.peak_working_set_size as u64,
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MemSnapshot;

    /// 非 Windows 平台暂不采集。
    pub fn snapshot() -> MemSnapshot {
        MemSnapshot::default()
    }
}

/// 读取当前进程内存快照。
///
/// # 返回
/// [`MemSnapshot`]；平台不支持或系统调用失败时字段为 0。
///
/// # 示例
/// ```ignore
/// let m = snapshot();
/// println!("{} bytes", m.working_set);
/// ```
pub fn snapshot() -> MemSnapshot {
    imp::snapshot()
}

#[cfg(test)]
mod tests {
    /// Windows 下应能读到非零工作集，且峰值不小于当前值。
    #[cfg(windows)]
    #[test]
    fn snapshot_reads_nonzero_on_windows() {
        let m = super::snapshot();
        assert!(m.working_set > 0);
        assert!(m.peak_working_set >= m.working_set);
    }
}
