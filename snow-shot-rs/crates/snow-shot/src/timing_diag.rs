//! 计时诊断日志：仅在环境变量 `CISOX_TIMING_DIAG` 设置时输出阶段时间点，默认完全关闭。
//!
//! 用于拆解「热键 -> 覆盖窗首帧」这类延迟：每个打点只记一条带标签的日志，
//! 阶段耗时由日志自带的 UTC 微秒时间戳相减得到。

use std::sync::OnceLock;

/// 开启计时诊断的环境变量（设置为任意值即开启）。
pub const ENV_TIMING_DIAG: &str = "CISOX_TIMING_DIAG";

/// 诊断是否开启（进程内只读一次环境变量）。
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(ENV_TIMING_DIAG).is_some())
}

/// 记一个时间点；诊断未开启时什么都不做。
///
/// # 参数
/// - `tag`：时间点标签（说明走到了哪一步）。
///
/// ```ignore
/// timing_diag::mark("open-overlays-begin");
/// ```
pub fn mark(tag: &str) {
    if enabled() {
        tracing::info!(tag, "timing-diag");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未设置环境变量时诊断关闭，且 `mark` 不会 panic。
    #[test]
    fn disabled_by_default_is_noop() {
        if enabled() {
            return;
        }
        mark("test");
    }
}
