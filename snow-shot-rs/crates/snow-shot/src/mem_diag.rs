//! 内存诊断日志：仅在环境变量 `CISOX_MEM_DIAG` 设置时输出进程内存布局，默认完全关闭。
//!
//! 用于定位「截图一次后内存不回落」这类问题：区分堆里仍在使用的字节、分配器保留的空闲页
//! 与堆外的私有提交（驱动 / 显存映射 / 线程栈）。

use snow_platform::process_mem::{current_memory_layout, current_process_memory};
use std::time::Duration;

/// 开启内存诊断日志的环境变量（设置为任意值即开启）。
pub const ENV_MEM_DIAG: &str = "CISOX_MEM_DIAG";
/// 字节换算为 MiB 的除数。
const MIB: f64 = 1024.0 * 1024.0;

/// 诊断日志是否开启。
pub fn enabled() -> bool {
    std::env::var_os(ENV_MEM_DIAG).is_some()
}

/// 把字节数换算成 MiB（诊断输出用）。
fn mib(bytes: u64) -> f64 {
    bytes as f64 / MIB
}

/// 立即输出一条内存布局日志；诊断未开启时什么都不做。
///
/// # 参数
/// - `tag`：日志标签（说明在哪个时间点采样）。
///
/// ```ignore
/// mem_diag::log("overlay-closed");
/// ```
pub fn log(tag: &str) {
    if !enabled() {
        return;
    }
    let process = current_process_memory();
    let layout = current_memory_layout();
    let heap_allocated: u64 = layout.heaps.iter().map(|h| h.allocated).sum();
    let heap_committed: u64 = layout.heaps.iter().map(|h| h.committed).sum();
    let heaps: Vec<String> = layout
        .heaps
        .iter()
        .filter(|h| h.committed >= MIB as u64)
        .map(|h| format!("{:.1}/{:.1}", mib(h.allocated), mib(h.committed)))
        .collect();
    let largest: Vec<String> = layout
        .largest_private
        .iter()
        .map(|r| format!("{:.1}", mib(*r)))
        .collect();
    tracing::info!(
        tag,
        private_mb = process.map_or(0.0, |m| mib(m.private_bytes)),
        working_set_mb = process.map_or(0.0, |m| mib(m.working_set)),
        private_regions_mb = mib(layout.private_committed),
        mapped_mb = mib(layout.mapped_committed),
        heap_allocated_mb = mib(heap_allocated),
        heap_committed_mb = mib(heap_committed),
        heaps_alloc_commit_mb = %heaps.join(" "),
        largest_private_mb = %largest.join(" "),
        "mem-diag"
    );
}

/// 延迟一段时间后再输出内存布局日志（后台线程，不阻塞界面）；诊断未开启时什么都不做。
///
/// # 参数
/// - `tag`：日志标签。
/// - `delays`：采样延迟序列，每个延迟各输出一次。
///
/// ```ignore
/// mem_diag::log_later("after-close", &[Duration::from_secs(1), Duration::from_secs(5)]);
/// ```
pub fn log_later(tag: &'static str, delays: &'static [Duration]) {
    if !enabled() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("mem-diag".into())
        .spawn(move || {
            let mut elapsed = Duration::ZERO;
            for delay in delays {
                std::thread::sleep(delay.saturating_sub(elapsed));
                elapsed = *delay;
                log(&format!("{tag}+{}ms", delay.as_millis()));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字节到 MiB 的换算。
    #[test]
    fn mib_converts_bytes() {
        assert_eq!(mib(3 * 1024 * 1024), 3.0);
        assert_eq!(mib(0), 0.0);
    }

    /// 未设置环境变量时诊断关闭，且 `log` / `log_later` 不会 panic。
    #[test]
    fn disabled_by_default_is_noop() {
        if enabled() {
            return;
        }
        log("test");
        const DELAYS: [Duration; 1] = [Duration::from_millis(1)];
        log_later("test", &DELAYS);
    }
}
