//! 有效录制时长时钟：暂停段不计入时长。

use std::time::{Duration, Instant};

/// 有效录制时钟（`Instant` 由调用方传入，便于确定性测试）。
#[derive(Debug, Clone, Default)]
pub struct ActiveClock {
    /// 已累计的有效时长（不含当前运行段）。
    accumulated: Duration,
    /// 当前运行段起点；`None` 表示暂停或未开始。
    running_since: Option<Instant>,
}

impl ActiveClock {
    /// 创建未开始的时钟。
    pub fn new() -> Self {
        Self::default()
    }

    /// 开始计时（已在运行则忽略）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn start(&mut self, now: Instant) {
        if self.running_since.is_none() {
            self.running_since = Some(now);
        }
    }

    /// 暂停计时，把当前运行段并入累计值。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn pause(&mut self, now: Instant) {
        if let Some(since) = self.running_since.take() {
            self.accumulated += now.saturating_duration_since(since);
        }
    }

    /// 恢复计时（等价于 [`ActiveClock::start`]）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn resume(&mut self, now: Instant) {
        self.start(now);
    }

    /// 当前是否处于运行（未暂停）状态。
    pub fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    /// 截至 `now` 的有效时长。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 不含暂停段的累计时长。
    pub fn elapsed(&self, now: Instant) -> Duration {
        match self.running_since {
            Some(since) => self.accumulated + now.saturating_duration_since(since),
            None => self.accumulated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 暂停段不计入时长。
    #[test]
    fn pause_segments_are_excluded() {
        let t0 = Instant::now();
        let mut clock = ActiveClock::new();
        clock.start(t0);
        clock.pause(t0 + Duration::from_secs(2));
        // 暂停 10 秒
        clock.resume(t0 + Duration::from_secs(12));
        let end = t0 + Duration::from_secs(15);
        assert_eq!(clock.elapsed(end), Duration::from_secs(5));
    }

    /// 暂停期间时长保持不变；重复 start/pause 无副作用。
    #[test]
    fn idempotent_transitions() {
        let t0 = Instant::now();
        let mut clock = ActiveClock::new();
        assert_eq!(clock.elapsed(t0), Duration::ZERO);
        clock.start(t0);
        clock.start(t0 + Duration::from_secs(1));
        clock.pause(t0 + Duration::from_secs(3));
        clock.pause(t0 + Duration::from_secs(9));
        assert!(!clock.is_running());
        assert_eq!(clock.elapsed(t0 + Duration::from_secs(20)), Duration::from_secs(3));
    }
}
