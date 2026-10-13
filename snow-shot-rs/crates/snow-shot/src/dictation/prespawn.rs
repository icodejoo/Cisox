//! 语音 worker 预拉起：宿主发现队首命令是语音转文字，会给 UI 加 `--hint dictation`；
//! UI 在 `main` 最早处（不等 GPUI 初始化）就在后台线程里读配置、拉起 `snow-stt` 并发 START，
//! 之后事件循环收到听写命令时，由 [`claim`] 认领这个已经在开麦、加载模型的 worker。
//!
//! 约束：
//! - 认领时 START 请求必须与此刻重新算出的一致（配置在这几百毫秒里变了就作废重拉）；
//! - 没人认领（命令被丢弃、前置检查失败等）会在 [`PRESPAWN_TTL`] 后丢弃，进程随之结束、麦克风关闭；
//! - UI 进程在此之前退出，worker 的 stdin 随之关闭，worker 视为中止自行退出。

use super::client::{ProcessSttLink, locate_stt_exe};
use super::config::{DictationConfig, prepare_launch};
use super::engine::SttLink;
use snow_config::document::ConfigDocument;
use snow_config::paths::config_file_path;
use snow_stt_protocol::{Command, StartRequest};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// 预拉起的 worker 无人认领时的存活上限；宿主等 UI 握手的上限是 10 s，这里略放宽。
pub const PRESPAWN_TTL: Duration = Duration::from_secs(15);
/// 认领时若预拉起线程还没拉完，最多等多久（等不到就放弃预拉起、走普通流程）。
pub const CLAIM_WAIT: Duration = Duration::from_secs(2);

/// 晚绑定的唤醒回调：预拉起时 UI 的收件箱还不存在，认领时再接上；绑定前到的事件留在通道里，
/// 认领后引擎的定时轮询会取走。
#[derive(Default)]
pub struct LateWake(Mutex<Option<Arc<dyn Fn() + Send + Sync>>>);

impl LateWake {
    /// 绑定真正的唤醒回调。
    ///
    /// # 参数
    /// - `wake`：有新事件时调用的回调（线程安全）。
    pub fn bind(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    /// 触发唤醒；还没绑定时什么都不做。
    pub fn call(&self) {
        let wake = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// 已预拉起、START 已发出的 worker。
pub struct Prespawned {
    /// 与 worker 的通道（丢弃即结束进程）；要在预拉起线程里构造再交给主线程，所以须是 `Send`。
    pub link: Box<dyn SttLink + Send>,
    /// 已发出的 START 请求（认领时核对）。
    pub request: StartRequest,
    /// 晚绑定的唤醒回调（认领时接上）。
    pub wake: Arc<LateWake>,
}

/// 槽位状态。
enum Slot {
    /// 从未开始。
    Empty,
    /// 预拉起线程正在干活。
    Pending,
    /// 已就绪，等人认领。
    Ready(Prespawned),
    /// 已被认领、已过期或已放弃，不再可用。
    Gone,
}

/// 预拉起槽位：一个进程里至多预拉起一次。
pub struct Prespawn {
    /// 槽位状态。
    slot: Mutex<Slot>,
    /// 状态变化通知（预拉起完成、被认领）。
    changed: Condvar,
}

impl Default for Prespawn {
    /// 空槽位。
    fn default() -> Self {
        Self::new()
    }
}

impl Prespawn {
    /// 创建空槽位。
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(Slot::Empty),
            changed: Condvar::new(),
        }
    }

    /// 加锁；毒锁不影响继续使用。
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 在后台线程里拉起 worker 放进槽位；`ttl` 内没人认领就丢弃（连带结束进程）。已开始过则忽略。
    ///
    /// # 参数
    /// - `make`：拉起 worker 的函数，失败返回 `None`（之后认领方走普通流程）。
    /// - `ttl`：无人认领的存活上限。
    ///
    /// # 示例
    /// ```ignore
    /// let slot = Arc::new(Prespawn::new());
    /// slot.begin(|| spawn_worker(&data_root), PRESPAWN_TTL);
    /// ```
    pub fn begin(
        self: &Arc<Self>,
        make: impl FnOnce() -> Option<Prespawned> + Send + 'static,
        ttl: Duration,
    ) {
        {
            let mut slot = self.lock();
            if !matches!(*slot, Slot::Empty) {
                return;
            }
            *slot = Slot::Pending;
        }
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("snow-dictation-prespawn".into())
            .spawn(move || this.run(make, ttl));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "无法创建语音预拉起线程，走普通流程");
            *self.lock() = Slot::Gone;
            self.changed.notify_all();
        }
    }

    /// 预拉起线程主体：拉起、放进槽位、等认领或到期。
    fn run(&self, make: impl FnOnce() -> Option<Prespawned>, ttl: Duration) {
        let made = make();
        let mut slot = self.lock();
        let Some(prespawned) = made else {
            *slot = Slot::Gone;
            self.changed.notify_all();
            return;
        };
        if !matches!(*slot, Slot::Pending) {
            // 认领方等不及已经放弃：这个 worker 没用了，释放锁后结束它
            drop(slot);
            drop(prespawned);
            return;
        }
        *slot = Slot::Ready(prespawned);
        self.changed.notify_all();
        let (mut slot, _) = self
            .changed
            .wait_timeout_while(slot, ttl, |s| matches!(s, Slot::Ready(_)))
            .unwrap_or_else(|e| e.into_inner());
        if matches!(*slot, Slot::Ready(_)) {
            let expired = std::mem::replace(&mut *slot, Slot::Gone);
            drop(slot);
            tracing::info!("预拉起的语音进程无人认领，已结束");
            drop(expired);
        }
    }

    /// 认领预拉起的 worker：槽位里的 START 请求与 `request` 一致才交出，否则丢弃它。
    ///
    /// 预拉起线程还没拉完时最多等 `wait`；等不到则放弃预拉起（之后拉完的会被自行丢弃）。
    ///
    /// # 参数
    /// - `request`：此刻按当前配置算出的 START 请求。
    /// - `wait`：最多等预拉起线程多久。
    /// - `wake`：有新事件时调用的回调（接到预拉起时留下的晚绑定位置，并立即触发一次）。
    ///
    /// # 返回
    /// 已发过 START 的通道；没有可用的预拉起（未开始、失败、过期、请求不一致）返回 `None`。
    pub fn claim(
        &self,
        request: &StartRequest,
        wait: Duration,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn SttLink + Send>> {
        let mut slot = self.lock();
        if matches!(*slot, Slot::Pending) {
            slot = self
                .changed
                .wait_timeout_while(slot, wait, |s| matches!(s, Slot::Pending))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        match std::mem::replace(&mut *slot, Slot::Gone) {
            Slot::Ready(prespawned) if prespawned.request == *request => {
                drop(slot);
                self.changed.notify_all();
                prespawned.wake.bind(Arc::clone(&wake));
                wake();
                Some(prespawned.link)
            }
            Slot::Ready(stale) => {
                drop(slot);
                self.changed.notify_all();
                tracing::info!("预拉起的语音进程与当前配置不一致，已丢弃");
                drop(stale);
                None
            }
            Slot::Empty => {
                // 没预拉起过：保持空槽位
                *slot = Slot::Empty;
                None
            }
            // 还在拉起（等不到）：标成已放弃，拉完的由预拉起线程自行丢弃
            Slot::Pending | Slot::Gone => None,
        }
    }
}

/// 进程级的预拉起槽位。
fn global() -> &'static Arc<Prespawn> {
    static GLOBAL: OnceLock<Arc<Prespawn>> = OnceLock::new();
    GLOBAL.get_or_init(|| Arc::new(Prespawn::new()))
}

/// 按配置拉起 `snow-stt` 并发出 START 与 PING（引擎对普通启动也是这两条）。
///
/// # 参数
/// - `config`：听写配置。
/// - `data_root`：应用数据根目录（默认模型目录所在）。
///
/// # 返回
/// 已启动的 worker；启动前检查不通过（没装模型、找不到 worker 等）或拉起、写命令失败返回 `None`，
/// 这些错误留给之后的普通流程按原样提示用户。
pub fn spawn_worker(config: &DictationConfig, data_root: &Path) -> Option<Prespawned> {
    let (exe, request) =
        prepare_launch(config, data_root, locate_stt_exe(), |p| p.is_dir()).ok()?;
    let wake = Arc::new(LateWake::default());
    let late = Arc::clone(&wake);
    let mut link = ProcessSttLink::spawn(&exe, Arc::new(move || late.call())).ok()?;
    for command in [Command::Start(request.clone()), Command::Ping] {
        link.send(&command).ok()?;
    }
    Some(Prespawned {
        link: Box::new(link),
        request,
        wake,
    })
}

/// 读配置文件得到听写配置（只读，不做损坏备份等副作用，与 UI 随后正式打开配置互不干扰）。
fn read_config(data_root: &Path) -> DictationConfig {
    let bytes = std::fs::read(config_file_path(data_root)).ok();
    DictationConfig::from_document(&ConfigDocument::from_bytes(bytes.as_deref()))
}

/// UI 进程最早处调用：后台线程里读配置、拉起语音 worker 并发 START，不阻塞调用线程。
///
/// # 参数
/// - `data_root`：应用数据根目录。
///
/// # 示例
/// ```ignore
/// if launch.hint == UiHint::Dictation {
///     dictation::prespawn::begin_early(ctx.data_root.clone());
/// }
/// ```
pub fn begin_early(data_root: PathBuf) {
    global().begin(
        move || spawn_worker(&read_config(&data_root), &data_root),
        PRESPAWN_TTL,
    );
}

/// 听写开始时认领预拉起的 worker。
///
/// # 参数
/// - `request`：按当前配置算出的 START 请求。
/// - `wake`：有新事件时调用的回调。
///
/// # 返回
/// 已发过 START 的通道；没有可用的预拉起返回 `None`，调用方走普通拉起流程。
pub fn claim(
    request: &StartRequest,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Option<Box<dyn SttLink + Send>> {
    global().claim(request, CLAIM_WAIT, wake)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::engine::LinkEvent;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// 假通道：记录是否被结束。
    struct Probe(Arc<AtomicBool>);

    impl SttLink for Probe {
        fn send(&mut self, _command: &Command) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<LinkEvent> {
            Vec::new()
        }
        fn shutdown(&mut self, _grace: Duration) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Drop for Probe {
        /// 丢弃即视为结束进程（与真实通道一致）。
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// 带唯一标记的请求，便于核对认领匹配。
    fn request(language: &str) -> StartRequest {
        StartRequest {
            language: language.into(),
            model_dir: "D:/m".into(),
            ..Default::default()
        }
    }

    /// 造一个预拉起结果，返回它与“已被结束”标志。
    fn made(language: &str) -> (Prespawned, Arc<AtomicBool>) {
        let killed = Arc::new(AtomicBool::new(false));
        (
            Prespawned {
                link: Box::new(Probe(Arc::clone(&killed))),
                request: request(language),
                wake: Arc::new(LateWake::default()),
            },
            killed,
        )
    }

    /// 空唤醒回调。
    fn noop() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    /// 轮询等条件成立（预拉起线程是异步的）。
    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "5 s 内没等到: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// 请求一致：认领成功并立刻触发一次唤醒；晚绑定回调之后收到后续事件的唤醒。
    #[test]
    fn claim_returns_matching_worker_and_binds_wake() {
        let slot = Arc::new(Prespawn::new());
        let (prespawned, killed) = made("zh-en");
        let late = Arc::clone(&prespawned.wake);
        slot.begin(move || Some(prespawned), Duration::from_secs(30));
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let link = slot.claim(
            &request("zh-en"),
            Duration::from_secs(5),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert!(link.is_some());
        assert_eq!(woken.load(Ordering::SeqCst), 1, "认领时立即唤醒一次");
        late.call();
        assert_eq!(
            woken.load(Ordering::SeqCst),
            2,
            "之后 worker 事件经晚绑定回调唤醒"
        );
        assert!(!killed.load(Ordering::SeqCst), "认领成功不结束进程");
        // 只能认领一次
        assert!(
            slot.claim(&request("zh-en"), Duration::ZERO, noop())
                .is_none()
        );
    }

    /// 绑定前触发唤醒什么都不做，不会 panic。
    #[test]
    fn late_wake_before_bind_is_noop() {
        let wake = LateWake::default();
        wake.call();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        wake.bind(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        wake.call();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// 配置变了（请求不一致）：不交出，并结束预拉起的进程。
    #[test]
    fn mismatched_request_discards_and_kills() {
        let slot = Arc::new(Prespawn::new());
        let (prespawned, killed) = made("zh-en");
        slot.begin(move || Some(prespawned), Duration::from_secs(30));
        assert!(
            slot.claim(&request("en"), Duration::from_secs(5), noop())
                .is_none()
        );
        assert!(killed.load(Ordering::SeqCst));
    }

    /// 无人认领：到期后自动丢弃并结束进程（麦克风不会一直开着）；之后再认领也没有。
    #[test]
    fn unclaimed_worker_expires() {
        let slot = Arc::new(Prespawn::new());
        let (prespawned, killed) = made("zh-en");
        slot.begin(move || Some(prespawned), Duration::from_millis(30));
        wait_until("过期后结束进程", || killed.load(Ordering::SeqCst));
        assert!(
            slot.claim(&request("zh-en"), Duration::ZERO, noop())
                .is_none()
        );
    }

    /// 预拉起失败（没模型 / 找不到 worker）：认领得到 `None`，走普通流程。
    #[test]
    fn failed_prespawn_yields_none() {
        let slot = Arc::new(Prespawn::new());
        slot.begin(|| None, PRESPAWN_TTL);
        assert!(
            slot.claim(&request("zh-en"), Duration::from_secs(5), noop())
                .is_none()
        );
    }

    /// 从没预拉起过：认领得到 `None`，且不占用槽位（之后仍可预拉起）。
    #[test]
    fn claim_without_begin_is_none() {
        let slot = Arc::new(Prespawn::new());
        assert!(
            slot.claim(&request("zh-en"), Duration::ZERO, noop())
                .is_none()
        );
        let (prespawned, _) = made("zh-en");
        slot.begin(move || Some(prespawned), Duration::from_secs(30));
        assert!(
            slot.claim(&request("zh-en"), Duration::from_secs(5), noop())
                .is_some()
        );
    }

    /// 认领时预拉起线程还没拉完且等不到：放弃，晚到的 worker 被丢弃并结束（不会泄漏进程）。
    #[test]
    fn slow_prespawn_is_abandoned_and_released() {
        let slot = Arc::new(Prespawn::new());
        let (prespawned, killed) = made("zh-en");
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        slot.begin(
            move || {
                let _ = go_rx.recv_timeout(Duration::from_secs(5));
                Some(prespawned)
            },
            Duration::from_secs(30),
        );
        assert!(
            slot.claim(&request("zh-en"), Duration::from_millis(20), noop())
                .is_none()
        );
        go_tx.send(()).unwrap();
        wait_until("晚到的 worker 被结束", || {
            killed.load(Ordering::SeqCst)
        });
    }

    /// 只预拉起一次：已开始后再次 `begin` 被忽略。
    #[test]
    fn begin_is_idempotent() {
        let slot = Arc::new(Prespawn::new());
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let counter = Arc::clone(&calls);
            let (prespawned, _) = made("zh-en");
            slot.begin(
                move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Some(prespawned)
                },
                Duration::from_secs(30),
            );
        }
        assert!(
            slot.claim(&request("zh-en"), Duration::from_secs(5), noop())
                .is_some()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// 读配置：缺文件取默认；目录里有配置文件就按它来（只读，不改动文件）。
    #[test]
    fn read_config_is_read_only_and_defaults_when_missing() {
        let dir = std::env::temp_dir().join(format!("snow-prespawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let defaults = read_config(&dir);
        assert_eq!(
            defaults,
            DictationConfig::from_document(&ConfigDocument::from_bytes(None))
        );
        assert!(
            !config_file_path(&dir).exists(),
            "读配置不能顺手创建配置文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
