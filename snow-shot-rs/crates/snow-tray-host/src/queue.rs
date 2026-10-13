//! 宿主命令队列：UI 进程还没就绪（正在启动、正在退出）时，先把要转给它的命令存在这里。
//!
//! 规则（方案 §4）：上限 [`MAX_QUEUED_COMMANDS`] 条、每条 [`COMMAND_EXPIRY`] 过期；鼠标手势的
//! `update` 只保留最新一条；其余命令严格保序（语音“按住说话”的按下 / 松开顺序不能乱）。
//! 本模块不读时钟，时间由调用方传入，所以可以确定性地测试。

use serde_json::Value;
use snow_host_protocol::message::{ActionOrigin, GesturePhase};
use snow_host_protocol::{COMMAND_EXPIRY, MAX_QUEUED_COMMANDS};
use std::collections::VecDeque;
use std::time::Instant;

/// 要转给 UI 进程的一条消息（序号在真正写进管道时才分配）。
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// 一个动作（托盘菜单项、热键、`--cmd`）。
    Action {
        /// 动作 id。
        id: String,
        /// 触发来源。
        origin: Option<ActionOrigin>,
    },
    /// 恢复上次保存的贴图。
    RestorePins,
    /// 让 UI 改配置（UI 是配置唯一写者）。
    SetConfig {
        /// 配置键。
        key: String,
        /// 新值。
        value: Value,
    },
    /// 鼠标手势事件（坐标为虚拟桌面物理像素）。
    Gesture {
        /// 阶段。
        phase: GesturePhase,
        /// 手势编号。
        id: u64,
        /// 手势动作标识（只有 `begin` 带）。
        action: Option<String>,
        /// 横坐标。
        x: i32,
        /// 纵坐标。
        y: i32,
    },
}

impl Outbound {
    /// 要求 UI 常驻的占位消息：UI 收到后什么都不做，只为让宿主有东西可发、从而拉起它
    /// （登录时按 `keep_alive` 拉起、崩溃或退避结束后补拉）。
    ///
    /// ```rust
    /// use snow_tray_host::queue::Outbound;
    /// assert!(matches!(Outbound::keep_alive(), Outbound::Action { .. }));
    /// ```
    pub fn keep_alive() -> Self {
        Outbound::Action {
            id: snow_host_protocol::ACTION_KEEP_ALIVE.to_string(),
            origin: None,
        }
    }
}

/// 队列里的一项。
#[derive(Debug, Clone, PartialEq)]
struct Queued {
    /// 消息本体。
    message: Outbound,
    /// 入队时刻（过期判定用；被合并的手势 update 沿用新消息的时刻）。
    at: Instant,
}

/// 入队的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// 正常入队。
    Queued,
    /// 手势 `update` 合并进了队列里该手势较旧的 `update`（队列长度不变）。
    Coalesced,
    /// 队列已满，丢弃了最旧的一条后入队。
    DroppedOldest,
}

/// 命令队列。
#[derive(Debug, Default)]
pub struct CommandQueue {
    /// 按入队顺序排列。
    items: VecDeque<Queued>,
}

impl CommandQueue {
    /// 新建空队列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前条数。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 队首的消息（最先入队、握手后最先投递的那条）；不含已过期的判断，调用方按需先 `expire`。
    ///
    /// ```rust
    /// use snow_tray_host::queue::{CommandQueue, Outbound};
    /// use std::time::Instant;
    /// let mut queue = CommandQueue::new();
    /// assert!(queue.front().is_none());
    /// queue.push(Outbound::RestorePins, Instant::now());
    /// assert_eq!(queue.front(), Some(&Outbound::RestorePins));
    /// ```
    pub fn front(&self) -> Option<&Outbound> {
        self.items.front().map(|queued| &queued.message)
    }

    /// 入队一条消息。
    ///
    /// 手势 `update` 若队列里已有同一手势的 `update`，就地替换成最新坐标（不增长）；队列已满时
    /// 丢弃最旧的一条，保证用户最新的操作不被挡掉。
    ///
    /// # 参数
    /// - `message`：要入队的消息。
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 入队方式（调用方据此记日志）。
    ///
    /// ```rust
    /// use snow_tray_host::queue::{CommandQueue, Outbound, PushOutcome};
    /// use std::time::Instant;
    /// let mut queue = CommandQueue::new();
    /// let outcome = queue.push(Outbound::RestorePins, Instant::now());
    /// assert_eq!(outcome, PushOutcome::Queued);
    /// assert_eq!(queue.len(), 1);
    /// ```
    pub fn push(&mut self, message: Outbound, now: Instant) -> PushOutcome {
        if let Outbound::Gesture {
            phase: GesturePhase::Update,
            id: new_id,
            ..
        } = &message
        {
            let slot = self.items.iter_mut().rev().find(|queued| {
                matches!(
                    &queued.message,
                    Outbound::Gesture { phase: GesturePhase::Update, id, .. } if id == new_id
                )
            });
            if let Some(queued) = slot {
                queued.message = message;
                queued.at = now;
                return PushOutcome::Coalesced;
            }
        }
        let mut outcome = PushOutcome::Queued;
        if self.items.len() >= MAX_QUEUED_COMMANDS {
            self.items.pop_front();
            outcome = PushOutcome::DroppedOldest;
        }
        self.items.push_back(Queued { message, at: now });
        outcome
    }

    /// 丢弃已过期的消息（保持其余顺序）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 丢弃的条数。
    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.items.len();
        self.items
            .retain(|queued| now.saturating_duration_since(queued.at) < COMMAND_EXPIRY);
        before - self.items.len()
    }

    /// 取出全部未过期的消息（按入队顺序），队列清空。
    ///
    /// # 参数
    /// - `now`：当前时刻；已过期的消息被丢弃而不是返回。
    ///
    /// # 返回
    /// 未过期的消息与被丢弃的过期条数。
    ///
    /// ```rust
    /// use snow_tray_host::queue::{CommandQueue, Outbound};
    /// use std::time::Instant;
    /// let mut queue = CommandQueue::new();
    /// let t0 = Instant::now();
    /// queue.push(Outbound::RestorePins, t0);
    /// let (live, expired) = queue.drain_live(t0);
    /// assert_eq!((live.len(), expired), (1, 0));
    /// assert!(queue.is_empty());
    /// ```
    pub fn drain_live(&mut self, now: Instant) -> (Vec<Outbound>, usize) {
        let expired = self.expire(now);
        let live = self.items.drain(..).map(|queued| queued.message).collect();
        (live, expired)
    }

    /// 清空队列。
    ///
    /// # 返回
    /// 被丢弃的条数。
    pub fn clear(&mut self) -> usize {
        let dropped = self.items.len();
        self.items.clear();
        dropped
    }

    /// 最早一条消息的过期时刻（队列为空返回 `None`）。
    pub fn next_expiry(&self) -> Option<Instant> {
        self.items
            .iter()
            .map(|queued| queued.at + COMMAND_EXPIRY)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 构造动作消息。
    fn action(id: &str) -> Outbound {
        Outbound::Action {
            id: id.to_string(),
            origin: None,
        }
    }

    /// 构造手势消息。
    fn gesture(phase: GesturePhase, id: u64, x: i32) -> Outbound {
        Outbound::Gesture {
            phase,
            id,
            action: None,
            x,
            y: 0,
        }
    }

    /// 取出全部消息（测试用，时间取入队时刻）。
    fn drain(queue: &mut CommandQueue, now: Instant) -> Vec<Outbound> {
        queue.drain_live(now).0
    }

    /// 保序：语音“按住说话”的按下 / 松开顺序原样保留。
    #[test]
    fn keeps_order() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        for id in ["start_dictation", "capture", "stop_dictation"] {
            assert_eq!(queue.push(action(id), t0), PushOutcome::Queued);
        }
        let ids: Vec<_> = drain(&mut queue, t0)
            .into_iter()
            .map(|m| match m {
                Outbound::Action { id, .. } => id,
                other => panic!("意外的消息 {other:?}"),
            })
            .collect();
        assert_eq!(ids, ["start_dictation", "capture", "stop_dictation"]);
        assert!(queue.is_empty());
    }

    /// 上限 64 条：第 65 条挤掉最旧的，条数不超限。
    #[test]
    fn capacity_drops_oldest() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        for i in 0..MAX_QUEUED_COMMANDS {
            assert_eq!(
                queue.push(action(&format!("a{i}")), t0),
                PushOutcome::Queued
            );
        }
        assert_eq!(queue.len(), MAX_QUEUED_COMMANDS);
        assert_eq!(queue.push(action("newest"), t0), PushOutcome::DroppedOldest);
        assert_eq!(queue.len(), MAX_QUEUED_COMMANDS);
        let all = drain(&mut queue, t0);
        assert_eq!(all.first(), Some(&action("a1")), "最旧的 a0 被挤掉");
        assert_eq!(all.last(), Some(&action("newest")));
    }

    /// 30 秒过期：到点的丢弃，没到点的保留，顺序不变。
    #[test]
    fn expires_after_thirty_seconds() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        queue.push(action("old"), t0);
        queue.push(action("fresh"), t0 + Duration::from_secs(20));
        assert_eq!(queue.next_expiry(), Some(t0 + COMMAND_EXPIRY));
        let later = t0 + Duration::from_secs(31);
        let (live, expired) = queue.drain_live(later);
        assert_eq!(expired, 1);
        assert_eq!(live, vec![action("fresh")]);
        // 恰好 30 秒视为已过期（“不足 30 秒”才有效）
        queue.push(action("edge"), t0);
        assert_eq!(queue.expire(t0 + COMMAND_EXPIRY), 1);
        assert_eq!(queue.next_expiry(), None);
    }

    /// 手势 update 只留最新：同一手势的旧 update 被就地替换，Begin / Finish 与其它手势不受影响。
    #[test]
    fn gesture_updates_coalesce_per_gesture() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        queue.push(gesture(GesturePhase::Begin, 1, 0), t0);
        queue.push(gesture(GesturePhase::Update, 1, 10), t0);
        queue.push(action("capture"), t0);
        queue.push(gesture(GesturePhase::Update, 2, 5), t0);
        assert_eq!(
            queue.push(gesture(GesturePhase::Update, 1, 99), t0),
            PushOutcome::Coalesced
        );
        assert_eq!(
            queue.push(gesture(GesturePhase::Update, 1, 100), t0),
            PushOutcome::Coalesced
        );
        queue.push(gesture(GesturePhase::Finish, 1, 100), t0);
        assert_eq!(queue.len(), 5);
        assert_eq!(
            drain(&mut queue, t0),
            vec![
                gesture(GesturePhase::Begin, 1, 0),
                gesture(GesturePhase::Update, 1, 100),
                action("capture"),
                gesture(GesturePhase::Update, 2, 5),
                gesture(GesturePhase::Finish, 1, 100),
            ]
        );
    }

    /// 合并刷新时间：新坐标的过期时刻按新消息算。
    #[test]
    fn coalesced_update_refreshes_expiry() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        queue.push(gesture(GesturePhase::Update, 1, 1), t0);
        queue.push(
            gesture(GesturePhase::Update, 1, 2),
            t0 + Duration::from_secs(25),
        );
        assert_eq!(queue.next_expiry(), Some(t0 + Duration::from_secs(55)));
    }

    /// 清空返回丢弃条数。
    #[test]
    fn clear_reports_dropped() {
        let t0 = Instant::now();
        let mut queue = CommandQueue::new();
        queue.push(action("a"), t0);
        queue.push(action("b"), t0);
        assert_eq!(queue.clear(), 2);
        assert!(queue.is_empty());
        assert_eq!(queue.clear(), 0);
    }
}
