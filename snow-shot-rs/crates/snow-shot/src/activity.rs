//! 活动守卫：UI 进程“能不能退出”的依据（方案 `docs/design/tray-host-split.md` §5）。
//!
//! 双进程模式下 UI 进程在“没有任何窗口且没有活动”时请求退出。窗口数是 GPUI 外壳现成的；
//! “活动”是没有窗口、但退出会丢事的情况：下载、更新检查、OCR / 翻译 job 的结果还没人接、
//! 历史写入还没落盘……它们用 RAII 的 [`ActivityGuard`] 表示——持有即不退，丢弃（含 panic 展开）即放行，
//! 没有任何固定延时，也不会在异常路径泄漏。守卫可以跨线程搬运：后台线程持着它干活，干完（或出错）自动放行，
//! 放行时会唤醒主线程重新判断一次退出条件（[`ActivityTracker`] 的 `wake`），否则“最后一个活动在后台线程结束”
//! 之后没有任何事件再触发判断，UI 就永远退不掉了。
//!
//! 另有一类“状态型”原因（采集进行中、延迟截图倒计时、录屏 / 长截图控制窗在、语音转文字进行中、
//! 用户显式要求常驻）不是线程，由 [`busy_reasons`] 按 [`BusyInputs`] 现场判定。两类合在一起就是
//! 发给宿主的 `activity.busy_reasons`。纯逻辑，离屏可测。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// 阻止退出 / 保活的原因标识（写进 `activity.busy_reasons`、日志与测试）。
pub mod reason {
    /// 有窗口（覆盖窗、设置、历史、贴图、翻译浮窗、录屏控制条……都算）。
    pub const WINDOWS: &str = "windows";
    /// 采集进行中（窗口还没出来）。
    pub const CAPTURE: &str = "capture";
    /// 直接截图进行中。
    pub const DIRECT_CAPTURE: &str = "direct_capture";
    /// 延迟截图倒计时（期间没有窗口）。
    pub const DELAY_CAPTURE: &str = "delay_capture";
    /// 语音转文字进行中（含收尾阶段）。
    pub const DICTATION: &str = "dictation";
    /// 录屏控制窗在（录制进程存活）。
    pub const RECORDING: &str = "recording";
    /// 长截图控制窗在（采集线程存活）。
    pub const SCROLL_CAPTURE: &str = "scroll_capture";
    /// 模型 / 运行时 / 更新包下载中。
    pub const DOWNLOAD: &str = "download";
    /// 更新检查中（手动或到点）。
    pub const UPDATE_CHECK: &str = "update_check";
    /// 历史写入器还有没落盘的任务。
    pub const HISTORY_FLUSH: &str = "history_flush";
    /// OCR / 表格 / 公式识别 job 的结果还有人要接（覆盖窗可能已关）。
    pub const OCR_JOB: &str = "ocr_job";
    /// 翻译 job 的结果还有人要接（识别 + 翻译、翻译页、输入框翻译）。
    pub const TRANSLATE_JOB: &str = "translate_job";
    /// 贴图相关的后台 job（贴图 OCR、选中文件解码贴图）。
    pub const PIN_JOB: &str = "pin_job";
    /// 读取前台应用选中文字（翻译选中文字的第一步）。
    pub const SELECTED_TEXT: &str = "selected_text";
    /// OCR 模型预热（`model_hot_start` 开启后的后台加载）。
    pub const OCR_WARM_UP: &str = "ocr_warm_up";
    /// 保活：MCP 服务在跑或已开启。
    pub const MCP: &str = snow_host_protocol::KEEP_ALIVE_MCP;
    /// 保活：开启了 OCR 模型热启动。
    pub const OCR_HOT_START: &str = snow_host_protocol::KEEP_ALIVE_OCR_HOT_START;
}

/// 放行时的唤醒回调（在丢弃守卫的线程上调用，只应做投递，例如往主线程收件箱推一个“重新判断”事件）。
type Wake = Box<dyn Fn() + Send + Sync>;

/// 活动计数器：按原因记录当前有多少个守卫。可在线程间共享（`Arc`）。
pub struct ActivityTracker {
    /// 原因 → 守卫个数（个数为 0 的原因不留在表里）。
    counts: Mutex<BTreeMap<&'static str, u32>>,
    /// 放行时的唤醒回调。
    wake: Wake,
}

impl ActivityTracker {
    /// 创建计数器。
    ///
    /// # 参数
    /// - `wake`：任一守卫放行后调用（在放行它的线程上，可能是后台线程）；必须轻量、不阻塞。
    ///
    /// # 返回
    /// 可共享的计数器。
    ///
    /// ```ignore
    /// use snow_shot::activity::{ActivityTracker, reason};
    /// let tracker = ActivityTracker::new(|| {});
    /// let guard = tracker.guard(reason::DOWNLOAD);
    /// assert_eq!(tracker.reasons(), [reason::DOWNLOAD]);
    /// drop(guard);
    /// assert!(tracker.is_idle());
    /// ```
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            counts: Mutex::new(BTreeMap::new()),
            wake: Box::new(wake),
        })
    }

    /// 取计数表的锁（中毒时继续用：守卫的放行不能因为别处 panic 而失效）。
    fn lock(&self) -> MutexGuard<'_, BTreeMap<&'static str, u32>> {
        self.counts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 登记一个活动：返回的守卫被丢弃之前，这个原因一直算“忙”。
    ///
    /// # 参数
    /// - `reason`：原因标识（见 [`reason`]）。
    ///
    /// # 返回
    /// 守卫；应移进真正干活的线程 / 对象里，随它一起结束。
    pub fn guard(self: &Arc<Self>, reason: &'static str) -> ActivityGuard {
        *self.lock().entry(reason).or_insert(0) += 1;
        ActivityGuard {
            tracker: Arc::clone(self),
            reason,
        }
    }

    /// 当前有守卫的原因（去重、按名字排序）。
    pub fn reasons(&self) -> Vec<&'static str> {
        self.lock().keys().copied().collect()
    }

    /// 某个原因当前的守卫个数。
    ///
    /// # 参数
    /// - `reason`：原因标识。
    pub fn count(&self, reason: &'static str) -> u32 {
        self.lock().get(reason).copied().unwrap_or(0)
    }

    /// 全部守卫个数之和。
    pub fn total(&self) -> u32 {
        self.lock().values().sum()
    }

    /// 是否没有任何守卫。
    pub fn is_idle(&self) -> bool {
        self.lock().is_empty()
    }

    /// 断言所有守卫都已归零（泄漏检查：退出前与测试里用）。
    ///
    /// # Panics
    /// 仍有守卫时 panic，消息里列出还没放行的原因，方便定位泄漏的那一处。
    pub fn assert_idle(&self) {
        let left = self.lock().clone();
        assert!(left.is_empty(), "活动守卫没有归零，泄漏的原因: {left:?}");
    }

    /// 放行一个守卫（`ActivityGuard::drop` 调用）。
    fn release(&self, reason: &'static str) {
        {
            let mut counts = self.lock();
            if let Some(n) = counts.get_mut(reason) {
                *n -= 1;
                if *n == 0 {
                    counts.remove(reason);
                }
            }
        }
        (self.wake)();
    }
}

/// 一个活动的 RAII 守卫：持有期间对应原因算“忙”，丢弃（含 panic 展开、线程提前返回）即放行。
#[must_use = "守卫一旦被丢弃就放行；要让它活到活动结束"]
pub struct ActivityGuard {
    /// 所属计数器。
    tracker: Arc<ActivityTracker>,
    /// 原因标识。
    reason: &'static str,
}

impl ActivityGuard {
    /// 这个守卫的原因标识。
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl Drop for ActivityGuard {
    /// 放行并唤醒主线程重新判断退出条件。
    fn drop(&mut self) {
        self.tracker.release(self.reason);
    }
}

impl std::fmt::Debug for ActivityGuard {
    /// 只打印原因（计数器里有回调，不便打印）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityGuard")
            .field("reason", &self.reason)
            .finish()
    }
}

/// 判定“状态型”阻止退出原因所需的现场快照（由运行时状态填写）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusyInputs {
    /// 打开的窗口数。
    pub windows: usize,
    /// 采集进行中。
    pub capture_in_flight: bool,
    /// 直接截图进行中。
    pub direct_in_flight: bool,
    /// 延迟截图倒计时中。
    pub delay_pending: bool,
    /// 语音转文字进行中（含收尾）。
    pub dictation_active: bool,
    /// 录屏控制窗在。
    pub recording: bool,
    /// 长截图控制窗在。
    pub scroll_capture: bool,
    /// MCP 服务在跑，或配置里开启了 MCP。
    pub mcp: bool,
    /// 开启了 OCR 模型热启动。
    pub ocr_hot_start: bool,
}

/// 当前阻止 UI 退出的全部原因：状态型（按 [`BusyInputs`]）加上带守卫的（来自计数器）；为空才算空闲。
///
/// 顺序固定：状态型在前（窗口、采集……、保活），守卫型在后（按名字排序），同名不重复。
///
/// # 参数
/// - `inputs`：状态型原因的现场快照。
/// - `tracker`：活动计数器。
///
/// # 返回
/// 原因标识列表。
///
/// ```ignore
/// use snow_shot::activity::{ActivityTracker, BusyInputs, busy_reasons};
/// let tracker = ActivityTracker::new(|| {});
/// let idle = BusyInputs::default();
/// assert!(busy_reasons(&idle, &tracker).is_empty());
/// let busy = BusyInputs { windows: 1, ..idle };
/// assert_eq!(busy_reasons(&busy, &tracker), ["windows"]);
/// ```
pub fn busy_reasons(inputs: &BusyInputs, tracker: &ActivityTracker) -> Vec<String> {
    let flags = [
        (inputs.windows > 0, reason::WINDOWS),
        (inputs.capture_in_flight, reason::CAPTURE),
        (inputs.direct_in_flight, reason::DIRECT_CAPTURE),
        (inputs.delay_pending, reason::DELAY_CAPTURE),
        (inputs.dictation_active, reason::DICTATION),
        (inputs.recording, reason::RECORDING),
        (inputs.scroll_capture, reason::SCROLL_CAPTURE),
        (inputs.mcp, reason::MCP),
        (inputs.ocr_hot_start, reason::OCR_HOT_START),
    ];
    let mut reasons: Vec<String> = flags
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_string())
        .collect();
    for name in tracker.reasons() {
        if !reasons.iter().any(|existing| existing == name) {
            reasons.push(name.to_string());
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 带唤醒计数的计数器。
    fn tracker() -> (Arc<ActivityTracker>, Arc<AtomicU32>) {
        let wakes = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&wakes);
        let tracker = ActivityTracker::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (tracker, wakes)
    }

    /// 持有即忙、丢弃即放行；同一原因多个守卫按个数计，全部丢弃才归零。
    #[test]
    fn guards_count_per_reason_and_release_on_drop() {
        let (tracker, _) = tracker();
        assert!(tracker.is_idle());
        let a = tracker.guard(reason::DOWNLOAD);
        let b = tracker.guard(reason::DOWNLOAD);
        let c = tracker.guard(reason::OCR_JOB);
        assert_eq!(tracker.count(reason::DOWNLOAD), 2);
        assert_eq!(tracker.total(), 3);
        assert_eq!(tracker.reasons(), [reason::DOWNLOAD, reason::OCR_JOB]);
        drop(a);
        assert_eq!(tracker.count(reason::DOWNLOAD), 1);
        drop(c);
        assert_eq!(tracker.reasons(), [reason::DOWNLOAD]);
        drop(b);
        tracker.assert_idle();
        assert_eq!(tracker.count(reason::DOWNLOAD), 0);
    }

    /// 每次放行都唤醒一次（主线程据此重新判断退出条件）；登记不唤醒。
    #[test]
    fn every_release_wakes_the_owner_once() {
        let (tracker, wakes) = tracker();
        let a = tracker.guard(reason::UPDATE_CHECK);
        let b = tracker.guard(reason::UPDATE_CHECK);
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        drop(a);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        drop(b);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }

    /// 异常路径不泄漏：线程里 panic、展开时守卫照样放行并唤醒。
    #[test]
    fn guard_is_released_when_the_holder_panics() {
        let (tracker, wakes) = tracker();
        let guard = tracker.guard(reason::TRANSLATE_JOB);
        let result = std::thread::spawn(move || {
            let _guard = guard;
            panic!("模拟后台 job 出错");
        })
        .join();
        assert!(result.is_err());
        tracker.assert_idle();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }

    /// 线程提前返回（`?` / `return`）同样放行；守卫先于线程创建登记，不存在“线程还没跑起来就判定空闲”的空窗。
    #[test]
    fn early_return_releases_and_registration_precedes_spawn() {
        let (tracker, _) = tracker();
        let guard = tracker.guard(reason::PIN_JOB);
        assert!(!tracker.is_idle(), "线程创建之前就已经算忙");
        let handle = std::thread::spawn(move || {
            let _guard = guard;
            if Some(1).is_some() {
                return;
            }
            unreachable!();
        });
        handle.join().unwrap();
        tracker.assert_idle();
    }

    /// 线程创建失败（闭包被丢弃）也不泄漏：模拟“登记了守卫却没能把它交给线程”。
    #[test]
    fn dropped_unspawned_closure_releases() {
        let (tracker, _) = tracker();
        let guard = tracker.guard(reason::DOWNLOAD);
        let closure = move || {
            let _guard = &guard;
        };
        drop(closure);
        tracker.assert_idle();
    }

    /// 多线程并发登记 / 放行：最终一定归零，不会下溢。
    #[test]
    fn concurrent_guards_balance_to_zero() {
        let (tracker, wakes) = tracker();
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let tracker = Arc::clone(&tracker);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        let _g = tracker.guard(if i % 2 == 0 {
                            reason::DOWNLOAD
                        } else {
                            reason::OCR_JOB
                        });
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        tracker.assert_idle();
        assert_eq!(wakes.load(Ordering::SeqCst), 16 * 50);
    }

    /// 泄漏检查：有守卫没放行时 `assert_idle` 报出具体原因。
    #[test]
    #[should_panic(expected = "history_flush")]
    fn assert_idle_names_the_leak() {
        let (tracker, _) = tracker();
        let _leaked = tracker.guard(reason::HISTORY_FLUSH);
        tracker.assert_idle();
    }

    /// 持锁线程 panic 把锁毒化后，守卫仍能正常登记 / 放行（退出判定不能因此卡死）。
    #[test]
    fn poisoned_lock_does_not_wedge_the_tracker() {
        let (tracker, _) = tracker();
        let poisoner = Arc::clone(&tracker);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.counts.lock().unwrap();
            panic!("毒化计数表的锁");
        })
        .join();
        let guard = tracker.guard(reason::DOWNLOAD);
        assert_eq!(tracker.count(reason::DOWNLOAD), 1);
        drop(guard);
        tracker.assert_idle();
    }

    /// 方案 §5 表格逐行：每种“状态型”活动单独成立时，各自带出对应原因，别的都不出现。
    #[test]
    fn every_state_row_of_the_table_blocks_exit() {
        let (tracker, _) = tracker();
        let cases: [(BusyInputs, &str); 9] = [
            (
                BusyInputs {
                    windows: 2,
                    ..BusyInputs::default()
                },
                reason::WINDOWS,
            ),
            (
                BusyInputs {
                    capture_in_flight: true,
                    ..BusyInputs::default()
                },
                reason::CAPTURE,
            ),
            (
                BusyInputs {
                    direct_in_flight: true,
                    ..BusyInputs::default()
                },
                reason::DIRECT_CAPTURE,
            ),
            (
                BusyInputs {
                    delay_pending: true,
                    ..BusyInputs::default()
                },
                reason::DELAY_CAPTURE,
            ),
            (
                BusyInputs {
                    dictation_active: true,
                    ..BusyInputs::default()
                },
                reason::DICTATION,
            ),
            (
                BusyInputs {
                    recording: true,
                    ..BusyInputs::default()
                },
                reason::RECORDING,
            ),
            (
                BusyInputs {
                    scroll_capture: true,
                    ..BusyInputs::default()
                },
                reason::SCROLL_CAPTURE,
            ),
            (
                BusyInputs {
                    mcp: true,
                    ..BusyInputs::default()
                },
                reason::MCP,
            ),
            (
                BusyInputs {
                    ocr_hot_start: true,
                    ..BusyInputs::default()
                },
                reason::OCR_HOT_START,
            ),
        ];
        for (inputs, expected) in cases {
            assert_eq!(busy_reasons(&inputs, &tracker), [expected], "{expected}");
        }
        assert!(busy_reasons(&BusyInputs::default(), &tracker).is_empty());
    }

    /// 方案 §5 表格逐行：每种带守卫的活动（下载、更新检查、历史、识别 / 翻译 job、贴图 job……）
    /// 持有守卫时阻止退出，放行后立刻空闲。
    #[test]
    fn every_guard_row_of_the_table_blocks_exit_until_released() {
        let (tracker, _) = tracker();
        let rows = [
            reason::DOWNLOAD,
            reason::UPDATE_CHECK,
            reason::HISTORY_FLUSH,
            reason::OCR_JOB,
            reason::TRANSLATE_JOB,
            reason::PIN_JOB,
            reason::SELECTED_TEXT,
            reason::OCR_WARM_UP,
        ];
        for name in rows {
            let guard = tracker.guard(name);
            assert_eq!(
                busy_reasons(&BusyInputs::default(), &tracker),
                [name],
                "{name}"
            );
            drop(guard);
            assert!(
                busy_reasons(&BusyInputs::default(), &tracker).is_empty(),
                "{name} 放行后应空闲"
            );
        }
        tracker.assert_idle();
    }

    /// 状态型与守卫型叠加：状态型在前、守卫型在后；同名（保活原因与守卫重名）不重复。
    #[test]
    fn busy_reasons_merge_without_duplicates() {
        let (tracker, _) = tracker();
        let _download = tracker.guard(reason::DOWNLOAD);
        let _mcp_guard = tracker.guard(reason::MCP);
        let inputs = BusyInputs {
            windows: 1,
            mcp: true,
            ..BusyInputs::default()
        };
        assert_eq!(
            busy_reasons(&inputs, &tracker),
            ["windows", "mcp", "download"]
        );
    }

    /// 原因标识与协议里的保活常量一致（宿主、快照、UI 三处同词）。
    #[test]
    fn keep_alive_reasons_match_protocol() {
        assert_eq!(reason::MCP, "mcp");
        assert_eq!(reason::OCR_HOT_START, "ocr_hot_start");
    }
}
