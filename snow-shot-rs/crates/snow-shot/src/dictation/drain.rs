//! 结束听写时“等剩余译文”的收尾状态：识别已收尾后，若还有句子在翻译，先等它们回来再真正结束。
//!
//! 只含时间与计数判断，不碰窗口与线程，可离屏单测。

use std::time::{Duration, Instant};

/// 等待剩余译文的上限，超时直接结束并提示。
pub const TRANSLATE_DRAIN_TIMEOUT: Duration = Duration::from_secs(12);

/// 一次检查的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainStep {
    /// 还有译文没回来，继续等。
    Wait,
    /// 译文都回来了，可以结束。
    Complete,
    /// 等太久了，放弃剩下的并结束。
    TimedOut,
}

/// 等待剩余译文的计时。
#[derive(Debug, Clone, Copy)]
pub struct Drain {
    /// 放弃等待的时刻。
    deadline: Instant,
}

impl Drain {
    /// 开始等待。
    ///
    /// # 参数
    /// - `now`：当前时间。
    /// - `pending`：还在翻译的句数；为 0 时无需等待，返回 `None`。
    pub fn begin(now: Instant, pending: usize) -> Option<Self> {
        (pending > 0).then(|| Self {
            deadline: now + TRANSLATE_DRAIN_TIMEOUT,
        })
    }

    /// 检查当前该继续等、已完成还是超时。
    ///
    /// # 参数
    /// - `now`：当前时间。
    /// - `pending`：还在翻译的句数。
    pub fn step(&self, now: Instant, pending: usize) -> DrainStep {
        if pending == 0 {
            DrainStep::Complete
        } else if now >= self.deadline {
            DrainStep::TimedOut
        } else {
            DrainStep::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有待译句不需要等待。
    #[test]
    fn no_pending_no_drain() {
        assert!(Drain::begin(Instant::now(), 0).is_none());
    }

    /// 有待译句：等待中，译文回齐后完成。
    #[test]
    fn waits_then_completes() {
        let now = Instant::now();
        let drain = Drain::begin(now, 2).unwrap();
        assert_eq!(drain.step(now, 2), DrainStep::Wait);
        assert_eq!(drain.step(now + Duration::from_secs(1), 1), DrainStep::Wait);
        assert_eq!(
            drain.step(now + Duration::from_secs(2), 0),
            DrainStep::Complete
        );
    }

    /// 超时兜底：到点仍有待译句就放弃；到点时已回齐仍算完成。
    #[test]
    fn times_out() {
        let now = Instant::now();
        let drain = Drain::begin(now, 1).unwrap();
        let end = now + TRANSLATE_DRAIN_TIMEOUT;
        assert_eq!(
            drain.step(end - Duration::from_millis(1), 1),
            DrainStep::Wait
        );
        assert_eq!(drain.step(end, 1), DrainStep::TimedOut);
        assert_eq!(drain.step(end, 0), DrainStep::Complete);
    }
}
