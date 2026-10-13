//! 后台“服务线程”的共享生命周期：单例守卫、就绪握手、退出通知与 join。
//!
//! 托盘与全局热键都要求：进程内只能有一个实例，由专用线程创建系统对象并泵消息，
//! 启动方阻塞等待线程回报“初始化成功/失败”，丢弃时先通知退出、再 join、最后释放单例标记。

use crate::error::ShellError;
use snow_light_thread::LightThread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;

/// 服务线程句柄。`T` 是线程初始化成功后回传给启动方的值（如消息循环唤醒器）。
///
/// 丢弃时依次执行：`notify` 通知退出、join 线程、释放单例标记（顺序固定，且不持任何锁）。
pub(crate) struct ServiceThread<T> {
    /// 进程级单例标记。
    flag: &'static AtomicBool,
    /// 后台线程；丢弃时取出 join。
    join: Option<LightThread>,
    /// 线程回传的就绪值。
    ready: T,
    /// 通知线程退出（发控制消息、唤醒消息循环等）。
    notify: Box<dyn Fn(&T) + Send>,
}

impl<T: Send + 'static> ServiceThread<T> {
    /// 抢占单例标记并启动服务线程，阻塞到线程回报初始化结果。
    ///
    /// # 参数
    /// - `flag`：进程级单例标记；已被占用返回 `AlreadyRunning(service)`。
    /// - `service`：服务名，用于单例错误。
    /// - `thread_name`：线程名。
    /// - `spawn_err`：创建线程失败时的错误前缀（形如“创建托盘线程失败”）。
    /// - `init`：在新线程内执行，返回线程私有状态与回传给启动方的就绪值；失败原样回报给启动方。
    /// - `run`：初始化成功后在同一线程运行（通常是消息循环），返回即线程结束。
    /// - `notify`：丢弃时在 join 前调用，应让 `run` 尽快返回。
    ///
    /// # 返回
    /// 句柄；`init` 失败返回其错误（已 join 线程并释放标记），`init` panic 返回 `ServiceClosed`。
    ///
    /// ```ignore
    /// let t = ServiceThread::spawn(&RUNNING, "Demo", "demo", "创建线程失败",
    ///     || Ok(((), ())), |()| {}, |_| {})?;
    /// ```
    pub(crate) fn spawn<S>(
        flag: &'static AtomicBool,
        service: &'static str,
        thread_name: &str,
        spawn_err: &str,
        init: impl FnOnce() -> Result<(S, T), ShellError> + Send + 'static,
        run: impl FnOnce(S) + Send + 'static,
        notify: impl Fn(&T) + Send + 'static,
    ) -> Result<Self, ShellError> {
        if flag.swap(true, Ordering::SeqCst) {
            return Err(ShellError::AlreadyRunning(service));
        }
        let (ready_tx, ready_rx) = channel::<Result<T, ShellError>>();
        let spawned = LightThread::spawn(thread_name, move || {
            let (state, ready) = match init() {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(ready));
            run(state);
        });
        let join = match spawned {
            Ok(j) => j,
            Err(e) => {
                flag.store(false, Ordering::SeqCst);
                return Err(ShellError::Platform(format!("{spawn_err}: {e}")));
            }
        };
        match ready_rx.recv() {
            Ok(Ok(ready)) => Ok(Self {
                flag,
                join: Some(join),
                ready,
                notify: Box::new(notify),
            }),
            Ok(Err(e)) => {
                join.join();
                flag.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => {
                flag.store(false, Ordering::SeqCst);
                Err(ShellError::ServiceClosed)
            }
        }
    }

    /// 线程回传的就绪值。
    pub(crate) fn ready(&self) -> &T {
        &self.ready
    }
}

impl<T> Drop for ServiceThread<T> {
    /// 通知退出、join 线程、释放单例标记。
    fn drop(&mut self) {
        (self.notify)(&self.ready);
        if let Some(j) = self.join.take() {
            j.join();
        }
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};

    /// 初始化成功：就绪值可取，Drop 通知退出并 join，之后单例标记释放。
    #[test]
    fn success_then_drop_notifies_joins_and_releases() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let (stop_tx, stop_rx) = channel::<()>();
        let stop_tx: Mutex<Sender<()>> = Mutex::new(stop_tx);
        let exited = Arc::new(AtomicBool::new(false));
        let exited2 = exited.clone();
        let t = ServiceThread::spawn(
            &FLAG,
            "Demo",
            "svc-test-ok",
            "创建线程失败",
            || Ok((stop_rx, 7u32)),
            move |rx: Receiver<()>| {
                let _ = rx.recv();
                exited2.store(true, Ordering::SeqCst);
            },
            move |_| {
                let _ = stop_tx.lock().unwrap().send(());
            },
        )
        .unwrap();
        assert_eq!(*t.ready(), 7);
        assert!(FLAG.load(Ordering::SeqCst));
        drop(t);
        assert!(exited.load(Ordering::SeqCst), "drop 返回时线程已 join");
        assert!(!FLAG.load(Ordering::SeqCst));
    }

    /// 初始化失败：原样回报错误，run 不执行，标记释放。
    #[test]
    fn init_failure_is_reported_and_releases_flag() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = ran.clone();
        let r = ServiceThread::<()>::spawn(
            &FLAG,
            "Demo",
            "svc-test-fail",
            "创建线程失败",
            || Err::<((), ()), _>(ShellError::Platform("boom".into())),
            move |()| {
                ran2.fetch_add(1, Ordering::SeqCst);
            },
            |_| {},
        );
        assert!(matches!(r, Err(ShellError::Platform(m)) if m == "boom"));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(!FLAG.load(Ordering::SeqCst));
    }

    /// 单例：第二次启动返回 AlreadyRunning，且不清掉现有实例的标记。
    #[test]
    fn second_start_is_rejected() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let (stop_tx, stop_rx) = channel::<()>();
        let stop_tx: Mutex<Sender<()>> = Mutex::new(stop_tx);
        let first = ServiceThread::spawn(
            &FLAG,
            "Demo",
            "svc-test-single",
            "创建线程失败",
            || Ok((stop_rx, ())),
            |rx: Receiver<()>| {
                let _ = rx.recv();
            },
            move |_| {
                let _ = stop_tx.lock().unwrap().send(());
            },
        )
        .unwrap();
        let second = ServiceThread::<()>::spawn(
            &FLAG,
            "Demo",
            "svc-test-single-2",
            "创建线程失败",
            || Ok(((), ())),
            |()| {},
            |_| {},
        );
        assert!(matches!(second, Err(ShellError::AlreadyRunning("Demo"))));
        assert!(FLAG.load(Ordering::SeqCst), "被拒绝的启动不得清掉现有标记");
        drop(first);
        assert!(!FLAG.load(Ordering::SeqCst));
    }

    /// init panic：启动方得到 ServiceClosed，不死锁，标记释放。
    #[test]
    fn init_panic_does_not_deadlock() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let r = ServiceThread::<()>::spawn(
            &FLAG,
            "Demo",
            "svc-test-panic",
            "创建线程失败",
            || -> Result<((), ()), ShellError> { panic!("init panic（测试预期）") },
            |()| {},
            |_| {},
        );
        assert!(matches!(r, Err(ShellError::ServiceClosed)));
        assert!(!FLAG.load(Ordering::SeqCst));
    }
}
