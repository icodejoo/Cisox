//! 编码队列：有界、满时丢最旧帧、生产者永不阻塞。
//!
//! 编码提交偶尔会卡十几毫秒（QSV 帧时间尖峰）。队列给这种尖峰留出缓冲；真的堵死时丢最旧的帧
//! （被丢的帧 pts 留下空档，前一帧的时长自然延长），而不是让上游的取帧/合成线程等着。

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

/// 队列里的消息。
pub enum EncodeMsg<S> {
    /// 一帧已合成的表面与槽号。
    Frame(S, i64),
    /// 结束（排他终点槽号）；总是排在所有帧之后，且不会被丢弃。
    Finish(i64),
}

/// 队列内部状态。
struct State<S> {
    /// 待编码的帧（旧→新）。
    frames: VecDeque<(S, i64)>,
    /// 结束标记（排他终点槽号）。
    finish: Option<i64>,
    /// 因队列满被丢弃的帧数。
    dropped: u64,
}

/// 满时丢最旧的多生产者/单消费者编码队列。
pub struct EncodeQueue<S> {
    /// 状态。
    state: Mutex<State<S>>,
    /// 有新消息时唤醒消费者。
    ready: Condvar,
    /// 容量上限（帧数）。
    capacity: usize,
}

impl<S> EncodeQueue<S> {
    /// 创建队列。
    ///
    /// # 参数
    /// - `capacity`：最多缓冲的帧数（至少 1）。
    ///
    /// # 示例
    /// ```ignore
    /// let q = EncodeQueue::new(4);
    /// q.push(frame, 0);
    /// ```
    pub fn new(capacity: usize) -> Self {
        Self { state: Mutex::new(State { frames: VecDeque::new(), finish: None, dropped: 0 }), ready: Condvar::new(), capacity: capacity.max(1) }
    }

    /// 锁住状态（持锁线程 panic 时仍取回数据，队列不应因此整体不可用）。
    fn lock(&self) -> std::sync::MutexGuard<'_, State<S>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 放入一帧；队列已满则丢最旧的一帧并返回它（由调用方释放）。永不阻塞。
    ///
    /// # 参数
    /// - `surface`：合成好的表面。
    /// - `pts`：槽号。
    pub fn push(&self, surface: S, pts: i64) -> Option<S> {
        let mut state = self.lock();
        let dropped = if state.frames.len() >= self.capacity {
            state.dropped += 1;
            state.frames.pop_front().map(|(s, _)| s)
        } else {
            None
        };
        state.frames.push_back((surface, pts));
        drop(state);
        self.ready.notify_one();
        dropped
    }

    /// 放入结束标记（之后再 `push` 的帧会被忽略前的都会先被消费）。
    ///
    /// # 参数
    /// - `end_pts`：排他终点槽号。
    pub fn finish(&self, end_pts: i64) {
        self.lock().finish = Some(end_pts);
        self.ready.notify_one();
    }

    /// 阻塞取下一条消息：先取完所有帧，最后返回 [`EncodeMsg::Finish`]；结束后再调用继续返回 `Finish`。
    pub fn recv(&self) -> EncodeMsg<S> {
        let mut state = self.lock();
        loop {
            if let Some((surface, pts)) = state.frames.pop_front() {
                return EncodeMsg::Frame(surface, pts);
            }
            if let Some(end) = state.finish {
                return EncodeMsg::Finish(end);
            }
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// 因队列满被丢弃的帧数。
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// 取出所有帧的 pts，直到 Finish。
    fn drain(q: &EncodeQueue<u32>) -> (Vec<i64>, i64) {
        let mut pts = Vec::new();
        loop {
            match q.recv() {
                EncodeMsg::Frame(_, p) => pts.push(p),
                EncodeMsg::Finish(end) => return (pts, end),
            }
        }
    }

    /// 未满时保持顺序，Finish 在所有帧之后。
    #[test]
    fn keeps_order_and_finishes_last() {
        let q = EncodeQueue::new(4);
        for i in 0..3 {
            assert!(q.push(i as u32, i).is_none());
        }
        q.finish(9);
        assert_eq!(drain(&q), (vec![0, 1, 2], 9));
        assert_eq!(q.dropped(), 0);
    }

    /// 满时丢最旧，返回被丢的帧；生产者不阻塞。
    #[test]
    fn drops_oldest_when_full_without_blocking() {
        let q = EncodeQueue::new(2);
        assert!(q.push(10, 0).is_none());
        assert!(q.push(11, 1).is_none());
        assert_eq!(q.push(12, 2), Some(10));
        assert_eq!(q.push(13, 3), Some(11));
        assert_eq!(q.dropped(), 2);
        q.finish(4);
        assert_eq!(drain(&q), (vec![2, 3], 4));
    }

    /// Finish 之后再调用 recv 仍返回 Finish；消费者阻塞时被生产者唤醒。
    #[test]
    fn consumer_blocks_until_message_and_finish_is_sticky() {
        let q = Arc::new(EncodeQueue::new(2));
        let consumer = {
            let q = Arc::clone(&q);
            std::thread::spawn(move || drain(&q))
        };
        std::thread::sleep(Duration::from_millis(30));
        q.push(1, 5);
        q.finish(6);
        assert_eq!(consumer.join().unwrap(), (vec![5], 6));
        assert!(matches!(q.recv(), EncodeMsg::Finish(6)));
    }

    /// 容量至少为 1。
    #[test]
    fn capacity_is_at_least_one() {
        let q = EncodeQueue::new(0);
        assert!(q.push(1, 0).is_none());
        assert_eq!(q.push(2, 1), Some(1));
    }
}
