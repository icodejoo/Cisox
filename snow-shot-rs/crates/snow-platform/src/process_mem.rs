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

/// 一个进程堆的占用摘要（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapUsage {
    /// 堆里已分配（仍被持有）的字节数。
    pub allocated: u64,
    /// 堆已向系统提交的字节数；与 `allocated` 的差值是分配器保留未还的空闲页。
    pub committed: u64,
}

/// 当前进程的内存布局摘要（只用于诊断，开销不小，勿放热路径）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryLayout {
    /// 各进程堆的占用。
    pub heaps: Vec<HeapUsage>,
    /// 私有已提交区域总字节数（堆、栈、驱动 / 显存映射等）。
    pub private_committed: u64,
    /// 映射文件 / 共享区已提交总字节数。
    pub mapped_committed: u64,
    /// 最大的若干私有已提交区域大小（字节，从大到小）。
    pub largest_private: Vec<u64>,
}

/// 最多记录的最大私有区域个数。
const LARGEST_REGIONS: usize = 12;

/// 读取当前进程的内存布局（各堆已分配 / 已提交，私有 / 映射已提交总量，最大的私有区域）。
///
/// # 返回
/// 摘要；非 Windows 返回空摘要。
///
/// ```ignore
/// let layout = snow_platform::process_mem::current_memory_layout();
/// let heap_committed: u64 = layout.heaps.iter().map(|h| h.committed).sum();
/// ```
pub fn current_memory_layout() -> MemoryLayout {
    #[cfg(windows)]
    {
        win::layout()
    }
    #[cfg(not(windows))]
    {
        MemoryLayout::default()
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

    /// 枚举进程堆与虚拟内存区域，汇总成诊断摘要。
    pub fn layout() -> super::MemoryLayout {
        use super::{HeapUsage, LARGEST_REGIONS, MemoryLayout};
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Memory::{
            GetProcessHeaps, HEAP_SUMMARY, HeapSummary, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED,
            MEM_PRIVATE, MEMORY_BASIC_INFORMATION, VirtualQuery,
        };
        let mut out = MemoryLayout::default();
        // SAFETY: 句柄数组由 GetProcessHeaps 填充；HEAP_SUMMARY 的 cb 已设置；VirtualQuery 缓冲大小与结构一致。
        unsafe {
            let mut handles = [HANDLE::default(); 256];
            let count = (GetProcessHeaps(&mut handles) as usize).min(handles.len());
            for heap in &handles[..count] {
                let mut summary = HEAP_SUMMARY {
                    cb: std::mem::size_of::<HEAP_SUMMARY>() as u32,
                    ..Default::default()
                };
                if HeapSummary(*heap, 0, &mut summary).as_bool() {
                    out.heaps.push(HeapUsage {
                        allocated: summary.cbAllocated as u64,
                        committed: summary.cbCommitted as u64,
                    });
                }
            }
            let mut address = 0usize;
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
            while VirtualQuery(Some(address as *const _), &mut info, size) == size {
                let region = info.RegionSize as u64;
                if info.State == MEM_COMMIT {
                    if info.Type == MEM_PRIVATE {
                        out.private_committed += region;
                        out.largest_private.push(region);
                    } else if info.Type == MEM_MAPPED {
                        out.mapped_committed += region;
                    } else if info.Type == MEM_IMAGE {
                        // 映像文件页不计入私有提交，诊断里不单列
                    }
                }
                let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
                if next <= address {
                    break;
                }
                address = next;
            }
        }
        out.largest_private.sort_unstable_by(|a, b| b.cmp(a));
        out.largest_private.truncate(LARGEST_REGIONS);
        out
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

    /// 内存布局摘要：至少有一个堆，私有提交不小于最大区域，最大区域从大到小排列。
    #[cfg(windows)]
    #[test]
    fn memory_layout_is_consistent() {
        let layout = current_memory_layout();
        assert!(!layout.heaps.is_empty());
        assert!(layout.heaps.iter().all(|h| h.committed >= h.allocated / 2));
        assert!(layout.private_committed >= layout.largest_private.first().copied().unwrap_or(0));
        assert!(layout.largest_private.windows(2).all(|w| w[0] >= w[1]));
    }
}
