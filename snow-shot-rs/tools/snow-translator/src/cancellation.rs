//! 管道生命周期的协作取消：循环检查与 ORT 当前 Run 的终止共享同一状态。
use crate::engine::{EngineError, decode_err};
use crate::protocol::ErrorKind;
use ort::session::RunOptions;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 一条宿主管道的取消状态与当前推理选项。
#[derive(Default)]
pub struct Cancellation {
    /// EOF 后永久取消，不能复用已断开的管道。
    canceled: AtomicBool,
    /// 当前 ORT Run；取消时调用其线程安全终止接口。
    active: Mutex<Option<Arc<RunOptions>>>,
}
impl Cancellation {
    /// 关闭管道时通知循环，并终止正在执行的 ORT Run。
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
        if let Some(options) = self.active.lock().unwrap().as_ref() {
            let _ = options.terminate();
        }
    }
    /// 查询是否已取消。
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
    }
}
thread_local! {
    /// 仅推理线程持有当前管道上下文，测试互不污染。
    static CURRENT: RefCell<Option<Arc<Cancellation>>> = const { RefCell::new(None) };
}
/// 在当前线程执行带取消上下文的任务。
pub fn scope<R>(state: Arc<Cancellation>, f: impl FnOnce() -> R) -> R {
    /// 恢复线程原有的取消上下文。
    struct Restore(Option<Arc<Cancellation>>);
    impl Drop for Restore {
        /// 退出或展开时恢复外层上下文。
        fn drop(&mut self) {
            CURRENT.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(state))));
    f()
}
/// 在分段与逐 token 循环中检查取消。
pub fn check() -> Result<(), EngineError> {
    if CURRENT.with(|c| c.borrow().as_ref().is_some_and(|s| s.is_canceled())) {
        Err(EngineError::new(
            ErrorKind::DecodeFailed,
            "translation canceled: stdin closed",
        ))
    } else {
        Ok(())
    }
}
/// 一次 ORT Run 的注册期，必须活到其输出借用结束。
pub struct ActiveRun {
    /// 由运行线程和 stdin 线程共享的选项。
    pub options: Arc<RunOptions>,
    /// 对应的管道上下文。
    state: Option<Arc<Cancellation>>,
}
impl Drop for ActiveRun {
    /// Run 完成后撤销注册，防止取消触及后续推理。
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.active.lock().unwrap().take();
        }
    }
}
/// 注册当前 Run，锁内再次检查取消以封闭检查与启动之间的竞态。
pub fn active_run() -> Result<ActiveRun, EngineError> {
    check()?;
    active_options(RunOptions::new().map_err(|e| decode_err(&e))?)
}

/// 注册带自定义配置的 Run，内存收缩空跑也遵循相同取消约定。
pub fn active_options(options: RunOptions) -> Result<ActiveRun, EngineError> {
    check()?;
    let options = Arc::new(options);
    let state = CURRENT.with(|c| c.borrow().clone());
    if let Some(state) = &state {
        let mut active = state.active.lock().unwrap();
        if state.is_canceled() {
            return Err(EngineError::new(
                ErrorKind::DecodeFailed,
                "translation canceled",
            ));
        }
        *active = Some(Arc::clone(&options));
    }
    Ok(ActiveRun { options, state })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 推理循环运行中收到取消，下一步退出；外层不受影响。
    #[test]
    fn cancellation_reaches_running_loop() {
        let state = Arc::new(Cancellation::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            scope(worker_state, || {
                check().unwrap();
                entered_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                check()
            })
        });
        entered_rx.recv().unwrap();
        state.cancel();
        resume_tx.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(check().is_ok());
    }
}
