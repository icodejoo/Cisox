//! 主线程收件箱：任意线程 `push`，GPUI 主线程 `await` 取出（纯 std，无新增依赖）。
//!
//! 热键、托盘、IPC 等线程只负责把事件塞进收件箱；唤醒由 `Waker` 完成
//! （GPUI 的前台执行器会被唤醒并回到主线程继续执行 future）。

use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};

/// 收件箱内部状态。
struct State<T> {
    /// 待处理事件（先进先出）。
    queue: VecDeque<T>,
    /// 消费端的唤醒器。
    waker: Option<Waker>,
    /// 是否已关闭（关闭后 `push` 被拒绝，队列取空后 `recv` 返回 `None`）。
    closed: bool,
}

/// 可克隆的收件箱句柄；所有克隆共享同一队列，应只有一个消费端调用 [`recv`](Self::recv)。
pub struct MainThreadInbox<T> {
    /// 共享状态。
    state: Arc<Mutex<State<T>>>,
}

impl<T> Clone for MainThreadInbox<T> {
    /// 克隆句柄（共享同一队列）。
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<T> Default for MainThreadInbox<T> {
    /// 等同于 [`MainThreadInbox::new`]。
    fn default() -> Self {
        Self::new()
    }
}

impl<T> MainThreadInbox<T> {
    /// 创建空收件箱。
    ///
    /// ```rust
    /// use snow_ui_shell::inbox::MainThreadInbox;
    /// let inbox: MainThreadInbox<u32> = MainThreadInbox::new();
    /// assert!(inbox.push(1));
    /// ```
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                queue: VecDeque::new(),
                waker: None,
                closed: false,
            })),
        }
    }

    /// 加锁；即使持锁线程曾 panic 也继续使用（队列本身不会处于不一致状态）。
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 投递一个事件并唤醒消费端；可在任意线程调用。
    ///
    /// # 参数
    /// - `event`：要投递的事件。
    ///
    /// # 返回
    /// 已关闭时返回 `false`（事件被丢弃），否则 `true`。
    ///
    /// ```rust
    /// use snow_ui_shell::inbox::MainThreadInbox;
    /// let inbox = MainThreadInbox::new();
    /// let tx = inbox.clone();
    /// std::thread::spawn(move || { tx.push("hello"); }).join().unwrap();
    /// ```
    pub fn push(&self, event: T) -> bool {
        let waker = {
            let mut st = self.lock();
            if st.closed {
                return false;
            }
            st.queue.push_back(event);
            st.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        true
    }

    /// 关闭收件箱：拒绝新事件，已入队事件仍可取出，取空后 `recv` 返回 `None`。
    pub fn close(&self) {
        let waker = {
            let mut st = self.lock();
            st.closed = true;
            st.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// 取出下一个事件；队列为空则挂起等待。
    ///
    /// # 返回
    /// `Some(事件)`；收件箱已关闭且取空时返回 `None`。
    ///
    /// ```no_run
    /// # async fn demo(inbox: snow_ui_shell::inbox::MainThreadInbox<u32>) {
    /// while let Some(ev) = inbox.recv().await {
    ///     println!("{ev}");
    /// }
    /// # }
    /// ```
    pub async fn recv(&self) -> Option<T> {
        poll_fn(|cx| {
            let mut st = self.lock();
            if let Some(ev) = st.queue.pop_front() {
                return Poll::Ready(Some(ev));
            }
            if st.closed {
                return Poll::Ready(None);
            }
            st.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// 非阻塞取出（测试与同步场景使用）。
    pub fn try_recv(&self) -> Option<T> {
        self.lock().queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Wake};
    use std::time::Duration;

    /// 计数型唤醒器：统计被唤醒次数。
    struct CountWaker(AtomicUsize);

    impl Wake for CountWaker {
        /// 计数加一。
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 阻塞地驱动 future 到完成（用 park 等唤醒）。
    fn block_on<F: Future>(fut: F) -> F::Output {
        struct ThreadWaker(std::thread::Thread);
        impl Wake for ThreadWaker {
            /// 唤醒等待线程。
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(fut);
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park_timeout(Duration::from_secs(5));
        }
    }

    /// 先入队再取：保持先进先出。
    #[test]
    fn fifo_order() {
        let inbox = MainThreadInbox::new();
        inbox.push(1);
        inbox.push(2);
        assert_eq!(block_on(inbox.recv()), Some(1));
        assert_eq!(block_on(inbox.recv()), Some(2));
    }

    /// 跨线程投递能唤醒挂起的消费端。
    #[test]
    fn cross_thread_push_wakes_receiver() {
        let inbox = MainThreadInbox::new();
        let tx = inbox.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.push(42u32);
        });
        assert_eq!(block_on(inbox.recv()), Some(42));
        h.join().unwrap();
    }

    /// 挂起时 push 恰好唤醒一次。
    #[test]
    fn push_wakes_pending_poll_once() {
        let inbox: MainThreadInbox<u8> = MainThreadInbox::new();
        let cw = Arc::new(CountWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&cw));
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(inbox.recv());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        inbox.push(7);
        assert_eq!(cw.0.load(Ordering::SeqCst), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Some(7)));
    }

    /// 关闭后：拒绝新事件，已入队事件取空后返回 None，挂起者被唤醒。
    #[test]
    fn close_drains_then_ends() {
        let inbox = MainThreadInbox::new();
        inbox.push(1);
        inbox.close();
        assert!(!inbox.push(2));
        assert_eq!(block_on(inbox.recv()), Some(1));
        assert_eq!(block_on(inbox.recv()), None);
    }

    /// 关闭能唤醒正在等待的消费端。
    #[test]
    fn close_wakes_pending_receiver() {
        let inbox: MainThreadInbox<u8> = MainThreadInbox::new();
        let closer = inbox.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            closer.close();
        });
        assert_eq!(block_on(inbox.recv()), None);
        h.join().unwrap();
    }
}
