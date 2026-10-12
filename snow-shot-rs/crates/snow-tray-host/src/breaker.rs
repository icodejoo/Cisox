//! UI 进程崩溃熔断：60 秒内崩溃 3 次，进入 5 分钟退避，期间不再拉起 UI（方案 §4、§10 第 9 条）。
//!
//! 不读时钟，时间由调用方传入。只有“异常退出”才计数：走过 `exit_ok` 的正常退出、
//! 宿主主动要求的关闭都不算。

use snow_host_protocol::{CRASH_BACKOFF, CRASH_BREAKER_THRESHOLD, CRASH_BREAKER_WINDOW};
use std::collections::VecDeque;
use std::time::Instant;

/// 记录一次崩溃后的熔断状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// 还没跳闸，可以继续拉起 UI。
    Closed,
    /// 已跳闸：到 `until` 之前不得拉起 UI。
    Open {
        /// 退避结束的时刻。
        until: Instant,
    },
}

/// 崩溃熔断器。
#[derive(Debug, Default)]
pub struct CrashBreaker {
    /// 统计窗口内的崩溃时刻（升序）。
    crashes: VecDeque<Instant>,
    /// 退避结束时刻；`None` 表示没在退避。
    open_until: Option<Instant>,
}

impl CrashBreaker {
    /// 新建（尚无崩溃记录）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次崩溃。
    ///
    /// # 参数
    /// - `now`：崩溃发生（被宿主察觉）的时刻。
    ///
    /// # 返回
    /// 记录后的状态：窗口内累计达到阈值就跳闸并开始退避。
    ///
    /// ```rust
    /// use snow_tray_host::breaker::{BreakerState, CrashBreaker};
    /// use std::time::Instant;
    /// let mut breaker = CrashBreaker::new();
    /// let t0 = Instant::now();
    /// assert_eq!(breaker.record_crash(t0), BreakerState::Closed);
    /// assert_eq!(breaker.record_crash(t0), BreakerState::Closed);
    /// assert!(matches!(breaker.record_crash(t0), BreakerState::Open { .. }));
    /// ```
    pub fn record_crash(&mut self, now: Instant) -> BreakerState {
        if let Some(until) = self.open_until {
            if now < until {
                return BreakerState::Open { until };
            }
            self.open_until = None;
            self.crashes.clear();
        }
        while let Some(&oldest) = self.crashes.front() {
            if now.saturating_duration_since(oldest) >= CRASH_BREAKER_WINDOW {
                self.crashes.pop_front();
            } else {
                break;
            }
        }
        self.crashes.push_back(now);
        if self.crashes.len() as u32 >= CRASH_BREAKER_THRESHOLD {
            let until = now + CRASH_BACKOFF;
            self.open_until = Some(until);
            self.crashes.clear();
            return BreakerState::Open { until };
        }
        BreakerState::Closed
    }

    /// 当前是否在退避中。
    ///
    /// # 参数
    /// - `now`：当前时刻；退避期满会就地复位。
    ///
    /// # 返回
    /// 退避中返回结束时刻，否则 `None`。
    pub fn open_until(&mut self, now: Instant) -> Option<Instant> {
        match self.open_until {
            Some(until) if now < until => Some(until),
            Some(_) => {
                self.open_until = None;
                self.crashes.clear();
                None
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 60 秒内第 3 次崩溃跳闸，退避 5 分钟。
    #[test]
    fn third_crash_in_window_trips() {
        let t0 = Instant::now();
        let mut breaker = CrashBreaker::new();
        assert_eq!(breaker.record_crash(t0), BreakerState::Closed);
        assert_eq!(
            breaker.record_crash(t0 + Duration::from_secs(20)),
            BreakerState::Closed
        );
        let third = t0 + Duration::from_secs(59);
        assert_eq!(
            breaker.record_crash(third),
            BreakerState::Open {
                until: third + CRASH_BACKOFF
            }
        );
    }

    /// 窗口外的旧崩溃不计数：每次间隔 61 秒的崩溃永远不会跳闸。
    #[test]
    fn crashes_outside_window_do_not_accumulate() {
        let t0 = Instant::now();
        let mut breaker = CrashBreaker::new();
        for i in 0..10u64 {
            assert_eq!(
                breaker.record_crash(t0 + Duration::from_secs(61 * i)),
                BreakerState::Closed,
                "第 {i} 次"
            );
        }
    }

    /// 恰好 60 秒的旧崩溃已滑出窗口（窗口是“不足 60 秒”）。
    #[test]
    fn window_edge_is_exclusive() {
        let t0 = Instant::now();
        let mut breaker = CrashBreaker::new();
        breaker.record_crash(t0);
        breaker.record_crash(t0 + Duration::from_secs(30));
        assert_eq!(
            breaker.record_crash(t0 + CRASH_BREAKER_WINDOW),
            BreakerState::Closed
        );
    }

    /// 退避期内：`open_until` 返回结束时刻；期满后复位，崩溃计数重新开始。
    #[test]
    fn backoff_expires_and_resets() {
        let t0 = Instant::now();
        let mut breaker = CrashBreaker::new();
        for _ in 0..3 {
            breaker.record_crash(t0);
        }
        let until = t0 + CRASH_BACKOFF;
        assert_eq!(
            breaker.open_until(t0 + Duration::from_secs(299)),
            Some(until)
        );
        assert_eq!(breaker.open_until(until), None);
        // 复位后重新计数：再崩两次也不跳闸
        assert_eq!(breaker.record_crash(until), BreakerState::Closed);
        assert_eq!(breaker.record_crash(until), BreakerState::Closed);
    }

    /// 退避期内又记录崩溃：仍是同一个结束时刻，不延长。
    #[test]
    fn crash_during_backoff_keeps_deadline() {
        let t0 = Instant::now();
        let mut breaker = CrashBreaker::new();
        for _ in 0..3 {
            breaker.record_crash(t0);
        }
        let until = t0 + CRASH_BACKOFF;
        assert_eq!(
            breaker.record_crash(t0 + Duration::from_secs(100)),
            BreakerState::Open { until }
        );
    }
}
