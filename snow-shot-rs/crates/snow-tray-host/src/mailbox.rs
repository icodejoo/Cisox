//! 有界非阻塞邮箱：入口与管道写线程共用，手势更新只合并相邻同代消息。
use crate::host::Event;
use snow_host_protocol::message::{GesturePhase, HostMessage};
use std::collections::VecDeque;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// 邮箱的固定容量。
const CAPACITY: usize = 256;

/// 消息的合并和优先规则。
pub trait Mail: Send {
    /// 是否为需要预留投递空间的控制消息。
    fn urgent(&self) -> bool;
    /// 退出请求的优先级高于后台完成通知。
    fn quit(&self) -> bool {
        false
    }
    /// 是否可替换队尾消息。
    fn replaces(&self, older: &Self) -> bool;
}

impl Mail for Event {
    /// 退出和后台完成事件使用预留空间，避免被高频输入淹没。
    fn urgent(&self) -> bool {
        matches!(self, Event::Action { id, .. } if id == snow_host_protocol::ACTION_QUIT || id == snow_host_protocol::ACTION_RESTART)
            || matches!(
                self,
                Event::SessionFailed { .. }
                    | Event::UiExited { .. }
                    | Event::HeadlessDone { .. }
                    | Event::SpecEmitted
            )
    }
    /// 退出始终越过其他事件。
    fn quit(&self) -> bool {
        matches!(self, Event::Action { id, .. } if id == snow_host_protocol::ACTION_QUIT || id == snow_host_protocol::ACTION_RESTART)
    }
    /// 只合并同一手势的相邻更新，保留结束边界。
    fn replaces(&self, older: &Self) -> bool {
        matches!((self, older), (Event::Gesture(new), Event::Gesture(old)) if new.phase == GesturePhase::Update && old.phase == GesturePhase::Update && new.id == old.id)
            || (self.urgent() && self == older)
    }
}
impl Mail for HostMessage {
    /// 关闭消息越过尚未写出的普通消息。
    fn urgent(&self) -> bool {
        matches!(self, Self::Shutdown)
    }
    /// 关闭命令优先于积压消息。
    fn quit(&self) -> bool {
        matches!(self, Self::Shutdown)
    }
    /// 手势更新不需要确认，可在写出前合并。
    fn replaces(&self, older: &Self) -> bool {
        matches!((self, older), (Self::Gesture { phase: GesturePhase::Update, id: a, .. }, Self::Gesture { phase: GesturePhase::Update, id: b, .. }) if a == b)
    }
}

/// 锁内的有限消息集合。
struct State<T> {
    /// 消息顺序。
    items: VecDeque<T>,
    /// 是否已经关闭。
    closed: bool,
}
/// 可克隆的发送和接收句柄；发送永不等待消费者。
pub struct Mailbox<T>(Arc<(Mutex<State<T>>, Condvar)>);
impl<T> Clone for Mailbox<T> {
    /// 共享同一个邮箱。
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Mail> Default for Mailbox<T> {
    /// 创建空邮箱。
    fn default() -> Self {
        Self::new()
    }
}
impl<T: Mail> Mailbox<T> {
    /// 创建空邮箱。
    pub fn new() -> Self {
        Self(Arc::new((
            Mutex::new(State {
                items: VecDeque::new(),
                closed: false,
            }),
            Condvar::new(),
        )))
    }
    /// 合并更新；满时拒绝普通消息，控制事件保留空间，退出优先消费。
    pub fn send(&self, message: T) -> Result<(), String> {
        let (lock, ready) = &*self.0;
        let mut state = lock.lock().map_err(|_| "邮箱锁损坏")?;
        if state.closed {
            return Err("邮箱已关闭".into());
        }
        if message.urgent() && state.items.iter().any(|old| message.replaces(old)) {
            return Ok(());
        }
        if state.items.back().is_some_and(|old| message.replaces(old)) {
            state.items.pop_back();
        }
        if message.urgent() {
            if state.items.len() == CAPACITY {
                state.items.pop_back();
            }
            if message.quit() {
                state.items.push_front(message);
            } else {
                // 完成事件保留到达顺序，不能越过此前的 Activity/Hello。
                state.items.push_back(message);
            }
        } else {
            if state.items.len() >= CAPACITY - 16 {
                tracing::warn!("宿主邮箱已满，拒绝新消息");
                return Err("邮箱已满".into());
            }
            state.items.push_back(message);
        }
        ready.notify_one();
        Ok(())
    }
    /// 等待下一条消息，关闭时立即返回。
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let (lock, ready) = &*self.0;
        let state = lock.lock().map_err(|_| RecvTimeoutError::Disconnected)?;
        let (mut state, _) = ready
            .wait_timeout_while(state, timeout, |s| s.items.is_empty() && !s.closed)
            .map_err(|_| RecvTimeoutError::Disconnected)?;
        state.items.pop_front().ok_or(if state.closed {
            RecvTimeoutError::Disconnected
        } else {
            RecvTimeoutError::Timeout
        })
    }
    /// 无截止时间地等待下一条消息，关闭时立即唤醒。
    pub fn recv(&self) -> Result<T, RecvTimeoutError> {
        let (lock, ready) = &*self.0;
        let state = lock.lock().map_err(|_| RecvTimeoutError::Disconnected)?;
        let mut state = ready
            .wait_while(state, |state| state.items.is_empty() && !state.closed)
            .map_err(|_| RecvTimeoutError::Disconnected)?;
        state
            .items
            .pop_front()
            .ok_or(RecvTimeoutError::Disconnected)
    }
    /// 关闭并清空邮箱，唤醒等待线程。
    pub fn close(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.closed = true;
            state.items.clear();
            self.0.1.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::GestureEvent;
    use snow_host_protocol::message::ActionOrigin;
    /// 堵塞消费者时内存有界，退出优先且相邻更新保留最新坐标。
    #[test]
    fn bounded_coalesced_and_quit_first() {
        let queue = Mailbox::new();
        for x in 0..1000 {
            queue
                .send(Event::Gesture(GestureEvent {
                    phase: GesturePhase::Update,
                    id: 1,
                    action: None,
                    x,
                    y: 0,
                }))
                .unwrap();
        }
        assert_eq!(queue.0.0.lock().unwrap().items.len(), 1);
        assert!(matches!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            Event::Gesture(GestureEvent { x: 999, .. })
        ));
        for _ in 0..1000 {
            let _ = queue.send(Event::Action {
                id: "history".into(),
                origin: ActionOrigin::Ipc,
            });
        }
        assert!(queue.0.0.lock().unwrap().items.len() < CAPACITY);
        queue
            .send(Event::Action {
                id: snow_host_protocol::ACTION_QUIT.into(),
                origin: ActionOrigin::Ipc,
            })
            .unwrap();
        queue
            .send(Event::HeadlessDone {
                kind: crate::host::HeadlessKind::SetConfig,
                notice: None,
            })
            .unwrap();
        assert!(
            matches!(queue.recv_timeout(Duration::ZERO).unwrap(), Event::Action { id, .. } if id == snow_host_protocol::ACTION_QUIT)
        );
        queue.close();
        assert_eq!(
            queue.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Disconnected)
        );
    }
    /// 异步写队列合并更新且不跨越结束边界，关闭消息优先。
    #[test]
    fn writer_keeps_gesture_boundaries_and_shutdown_priority() {
        let queue = Mailbox::new();
        let gesture = |phase, seq, x| HostMessage::Gesture {
            seq,
            phase,
            id: 7,
            action: None,
            x,
            y: 0,
        };
        queue.send(gesture(GesturePhase::Begin, 1, 0)).unwrap();
        queue.send(gesture(GesturePhase::Update, 2, 1)).unwrap();
        queue.send(gesture(GesturePhase::Update, 3, 2)).unwrap();
        queue.send(gesture(GesturePhase::Finish, 4, 3)).unwrap();
        assert_eq!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            gesture(GesturePhase::Begin, 1, 0)
        );
        assert_eq!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            gesture(GesturePhase::Update, 3, 2)
        );
        assert_eq!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            gesture(GesturePhase::Finish, 4, 3)
        );
        queue.send(HostMessage::ExitOk).unwrap();
        queue.send(HostMessage::Shutdown).unwrap();
        assert_eq!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            HostMessage::Shutdown
        );
    }
    /// 生命周期完成事件保留普通事件顺序，退出请求仍能越过两者。
    #[test]
    fn completion_preserves_session_event_order() {
        let queue = Mailbox::new();
        let line = Event::SessionLine {
            generation: 1,
            line: "activity".into(),
        };
        queue.send(line.clone()).unwrap();
        queue.send(Event::UiExited { generation: 1 }).unwrap();
        assert_eq!(queue.recv_timeout(Duration::ZERO).unwrap(), line);
        assert_eq!(
            queue.recv_timeout(Duration::ZERO).unwrap(),
            Event::UiExited { generation: 1 }
        );
    }
}
