//! 并行编码线程池：作业队列 + 首错收集 + 取消检查，FFmpeg 与系统引擎共用。
//!
//! 本模块不碰 COM：线程相关的初始化 / 反初始化全部由调用方的 `make_worker` 闭包
//! 与 worker 自己的 `Drop` 承担，且都发生在各 worker 线程内。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use super::{EditError, TaskCtl};

/// 生产者句柄：在调用线程里向 worker 投递作业、上报失败、查询是否该停。
pub struct Feeder<'a, J> {
    /// 作业队列发送端。
    tx: SyncSender<J>,
    /// 首个失败槽位。
    failure: &'a Mutex<Option<EditError>>,
    /// 停止标志（有 worker 或生产者失败时置位）。
    stop: &'a AtomicBool,
    /// 取消控制。
    ctl: &'a TaskCtl,
}

impl<J> Feeder<'_, J> {
    /// 是否应停止生产（已取消，或已有失败）。
    pub fn should_stop(&self) -> bool {
        self.ctl.is_cancelled() || self.stop.load(Ordering::SeqCst)
    }

    /// 投递一项作业（队列满时阻塞）。
    ///
    /// # 返回
    /// 为 false 表示所有 worker 已退出，应停止生产。
    pub fn send(&self, job: J) -> bool {
        self.tx.send(job).is_ok()
    }

    /// 记录一个失败并通知 worker 停止；只保留第一个。
    pub fn fail(&self, e: EditError) {
        record_failure(self.failure, self.stop, e);
    }
}

/// 记录首个失败并置停止标志。
fn record_failure(failure: &Mutex<Option<EditError>>, stop: &AtomicBool, e: EditError) {
    if let Ok(mut slot) = failure.lock() {
        slot.get_or_insert(e);
    }
    stop.store(true, Ordering::SeqCst);
}

/// 用 `threads` 个 worker 并行处理作业，生产者在调用线程里产出作业。
///
/// # 参数
/// - `threads`：worker 线程数（至少 1）。
/// - `total`：预期作业总数，仅用于进度回调。
/// - `stage`：进度阶段名。
/// - `ctl`：进度与取消控制。
/// - `make_worker`：在每个 worker 线程内创建线程私有的 worker（COM 初始化放这里）；失败记为首错并退出该线程。
/// - `run`：处理一项作业；失败记为首错并退出该线程。成功后自动累计并上报进度。
/// - `produce`：在调用线程里循环产出作业，通过 [`Feeder`] 投递。
///
/// # 返回
/// 成功处理的作业数；已取消返回 `cancelled` 错误（优先于失败），否则有失败返回首个失败。
///
/// # 示例
/// ```ignore
/// let n = encode_parallel(4, total, "extract", &ctl,
///     || Encoder::new(), |enc, job: Job| enc.encode(&job),
///     |feed| { for j in jobs { if feed.should_stop() || !feed.send(j) { break; } } })?;
/// ```
pub fn encode_parallel<J, W>(
    threads: usize,
    total: u64,
    stage: &str,
    ctl: &TaskCtl,
    make_worker: impl Fn() -> Result<W, EditError> + Sync,
    run: impl Fn(&mut W, J) -> Result<(), EditError> + Sync,
    produce: impl FnOnce(&Feeder<'_, J>),
) -> Result<u64, EditError>
where
    J: Send,
{
    let failure: Mutex<Option<EditError>> = Mutex::new(None);
    let stop = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    let (tx, rx) = sync_channel::<J>(threads * 2);
    let rx = Arc::new(Mutex::new(rx));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            let (failure, stop, done) = (&failure, &stop, &done);
            let (make_worker, run) = (&make_worker, &run);
            scope.spawn(move || {
                let mut worker = match make_worker() {
                    Ok(w) => w,
                    Err(e) => return record_failure(failure, stop, e),
                };
                loop {
                    let job = match rx.lock() {
                        Ok(guard) => guard.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    if let Err(e) = run(&mut worker, job) {
                        return record_failure(failure, stop, e);
                    }
                    let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                    ctl.report(n, total, stage);
                }
            });
        }
        drop(rx);
        let feeder = Feeder {
            tx,
            failure: &failure,
            stop: &stop,
            ctl,
        };
        produce(&feeder);
        drop(feeder);
    });
    if ctl.is_cancelled() {
        return Err(EditError::cancelled());
    }
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    Ok(done.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip;
    use std::sync::atomic::AtomicUsize;

    /// 投递 `0..n` 作业的标准生产者。
    fn feed_range(n: u32) -> impl FnOnce(&Feeder<'_, u32>) {
        move |feed| {
            for i in 0..n {
                if feed.should_stop() || !feed.send(i) {
                    break;
                }
            }
        }
    }

    /// 作业全部成功：返回总数，每个 worker 只创建一次。
    #[test]
    fn all_jobs_succeed() {
        let made = AtomicUsize::new(0);
        let sum = AtomicU64::new(0);
        let ctl = testclip::quiet_ctl(false);
        let n = encode_parallel(
            3,
            20,
            "t",
            &ctl,
            || {
                made.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_, j: u32| {
                sum.fetch_add(j as u64, Ordering::SeqCst);
                Ok(())
            },
            feed_range(20),
        )
        .unwrap();
        assert_eq!(n, 20);
        assert_eq!(sum.load(Ordering::SeqCst), 190);
        assert_eq!(made.load(Ordering::SeqCst), 3);
    }

    /// 空作业：直接返回 0。
    #[test]
    fn empty_jobs() {
        let ctl = testclip::quiet_ctl(false);
        let n = encode_parallel(
            2,
            0,
            "t",
            &ctl,
            || Ok(()),
            |_, _: u32| Ok(()),
            feed_range(0),
        )
        .unwrap();
        assert_eq!(n, 0);
    }

    /// 中途失败：上报首个错误，且生产者被叫停。
    #[test]
    fn first_error_reported() {
        let ctl = testclip::quiet_ctl(false);
        let err = encode_parallel(
            1,
            100,
            "t",
            &ctl,
            || Ok(()),
            |_, j: u32| {
                if j == 3 {
                    Err(EditError::new("boom3"))
                } else {
                    Ok(())
                }
            },
            feed_range(100),
        )
        .unwrap_err();
        assert!(err.to_string().contains("boom3"), "{err}");
    }

    /// worker 创建失败：作为首错上报。
    #[test]
    fn make_worker_failure_reported() {
        let ctl = testclip::quiet_ctl(false);
        let err = encode_parallel(
            2,
            5,
            "t",
            &ctl,
            || Err::<(), _>(EditError::new("no worker")),
            |_, _: u32| Ok(()),
            feed_range(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no worker"), "{err}");
    }

    /// 已取消：返回 cancelled，不处理作业。
    #[test]
    fn cancelled_wins() {
        let ctl = testclip::quiet_ctl(true);
        let ran = AtomicUsize::new(0);
        let err = encode_parallel(
            2,
            10,
            "t",
            &ctl,
            || Ok(()),
            |_, _: u32| {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            feed_range(10),
        )
        .unwrap_err();
        assert!(err.is_cancelled());
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }
}
