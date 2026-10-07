//! 覆盖窗内截图历史翻页的状态机（对应 Qt `ScreenshotHistoryService` 的导航部分）。
//!
//! 约定：索引 0 是「当前截图」（实时端点），索引 `n >= 1` 是历史记录里第 `n` 新的一条。
//! 读取是异步的：状态机只负责决定「下一步要加载哪条 / 回到当前」，并在读取完成后判定结果。
//! 状态机本身是纯逻辑，不接触界面与磁盘；读盘由 [`ThreadedHistoryProvider`] 在后台线程完成。

use crate::history_store::{HistoryStore, LoadedEntry};
use snow_history::capture_history::CaptureHistoryPolicy;
use snow_history::index::Record;
use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};

/// 历史读取线程名称。
const LOADER_THREAD_NAME: &str = "snow-history-loader";

/// 历史数据来源：给视图提供记录列表与异步读取（视图自身不碰磁盘，便于离屏测试）。
pub trait HistoryProvider {
    /// 当前全部记录 ID，新的在前（每次翻页前刷新，别处新写入的记录会出现在这里）。
    fn ids(&mut self) -> Vec<String>;

    /// 开始异步读取指定记录；结果由 [`HistoryProvider::poll_loaded`] 取走。
    fn begin_load(&mut self, id: &str);

    /// 取走一个已读完的结果：`(记录 ID, 现场)`；读取失败时现场为 `None`。没有就绪结果返回 `None`。
    fn poll_loaded(&mut self) -> Option<(String, Option<LoadedEntry>)>;
}

/// 基于后台线程的历史数据来源：索引读取在调用线程，整帧 PNG 解码在读取线程。
pub struct ThreadedHistoryProvider {
    /// 存取入口。
    store: HistoryStore,
    /// 最近一次 `ids` 看到的记录（按 ID 查元数据用）。
    records: Vec<Record>,
    /// 读取请求通道。
    requests: Sender<Record>,
    /// 读取结果通道。
    results: Receiver<(String, Option<LoadedEntry>)>,
}

impl ThreadedHistoryProvider {
    /// 启动读取线程。
    ///
    /// # 参数
    /// - `data_root`：应用数据根。
    /// - `policy`：历史策略（读取时顺带维护过期记录用）。
    ///
    /// # 返回
    /// 数据来源；线程创建失败返回 IO 错误。提供者被丢弃时读取线程随通道关闭退出。
    ///
    /// ```ignore
    /// let provider = ThreadedHistoryProvider::start(&data_root, policy)?;
    /// ```
    pub fn start(data_root: &Path, policy: CaptureHistoryPolicy) -> std::io::Result<Self> {
        let store = HistoryStore::new(data_root, policy);
        let (requests, request_rx) = channel::<Record>();
        let (result_tx, results) = channel::<(String, Option<LoadedEntry>)>();
        let loader = store.clone();
        std::thread::Builder::new()
            .name(LOADER_THREAD_NAME.into())
            .spawn(move || {
                while let Ok(record) = request_rx.recv() {
                    let entry = loader.load_entry(&record);
                    if result_tx.send((record.id, entry)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            store,
            records: Vec::new(),
            requests,
            results,
        })
    }
}

impl HistoryProvider for ThreadedHistoryProvider {
    fn ids(&mut self) -> Vec<String> {
        self.records = self.store.records();
        self.records.iter().map(|r| r.id.clone()).collect()
    }

    fn begin_load(&mut self, id: &str) {
        let Some(record) = self.records.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        if self.requests.send(record).is_err() {
            tracing::warn!("截图历史读取线程已退出");
        }
    }

    fn poll_loaded(&mut self) -> Option<(String, Option<LoadedEntry>)> {
        self.results.try_recv().ok()
    }
}

/// 一次导航请求的下一步动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavStep {
    /// 无事可做（没有更旧 / 更新的记录、正在读取、或已在当前截图）。
    None,
    /// 异步读取指定记录；读完后调用 [`HistoryNav::finish`]。
    Load {
        /// 要读取的记录 ID。
        id: String,
    },
    /// 切回当前截图（实时端点）。
    ShowLive,
}

/// 一次异步读取的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOutcome {
    /// 读取成功且可以显示。
    Loaded,
    /// 读取失败（文件损坏等）或内容不适用（如底图尺寸与当前不同）；该记录从导航列表中移除。
    Failed,
}

/// 读取完成后的判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishStep {
    /// 应用刚读到的记录，导航位置已更新。
    Apply,
    /// 不应用：读取已过期，或该记录已被移除，停在原处。
    Stay,
}

/// 读取中的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    /// 目标导航索引（1 起）。
    target: usize,
    /// 目标记录 ID。
    id: String,
}

/// 历史翻页状态机。
#[derive(Debug, Clone, Default)]
pub struct HistoryNav {
    /// 记录 ID，新的在前。
    ids: Vec<String>,
    /// 当前导航索引：0 = 当前截图。
    index: usize,
    /// 正在显示的历史记录 ID（索引 0 时为 `None`）。
    current_id: Option<String>,
    /// 正在进行的读取。
    pending: Option<Pending>,
    /// 本次会话里读取失败或不适用的记录 ID：刷新列表时不再出现，免得反复卡在同一条。
    skipped: HashSet<String>,
}

impl HistoryNav {
    /// 创建空状态机（位于当前截图）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前导航索引（0 = 当前截图）。
    pub fn index(&self) -> usize {
        self.index
    }

    /// 是否正在读取（读取期间忽略新的导航，对应 Qt `navigationInProgress`）。
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// 是否正停留在某条历史记录上（而不是当前截图）。
    pub fn in_history(&self) -> bool {
        self.index > 0
    }

    /// 刷新记录列表：别处新写入的记录会让索引整体后移，这里按当前显示的记录 ID 重新定位。
    ///
    /// # 参数
    /// - `ids`：最新的记录 ID 列表（新的在前）。
    pub fn set_ids(&mut self, mut ids: Vec<String>) {
        ids.retain(|id| !self.skipped.contains(id));
        if let Some(current) = &self.current_id {
            match ids.iter().position(|id| id == current) {
                Some(position) => self.index = position + 1,
                // 正显示的记录已被清理：保持在原索引附近
                None => self.index = self.index.min(ids.len()),
            }
        } else {
            self.index = 0;
        }
        if self.index == 0 {
            self.current_id = None;
        }
        self.ids = ids;
    }

    /// 往更旧的一条翻。
    ///
    /// # 返回
    /// 要读取的记录；没有更旧的记录或正在读取时为 [`NavStep::None`]。
    ///
    /// ```ignore
    /// let step = nav.older();
    /// ```
    pub fn older(&mut self) -> NavStep {
        self.go(self.index + 1)
    }

    /// 往更新的一条翻（到 0 即回到当前截图）。
    ///
    /// # 返回
    /// 下一步动作；已在当前截图或正在读取时为 [`NavStep::None`]。
    pub fn newer(&mut self) -> NavStep {
        match self.index {
            0 => NavStep::None,
            n => self.go(n - 1),
        }
    }

    /// 直接回到当前截图（右键退出历史时用）。
    ///
    /// # 返回
    /// [`NavStep::ShowLive`]；已在当前截图或正在读取时为 [`NavStep::None`]。
    pub fn return_to_live(&mut self) -> NavStep {
        self.go(0)
    }

    /// 导航到指定索引。
    fn go(&mut self, target: usize) -> NavStep {
        if self.busy() || target > self.ids.len() || target == self.index {
            return NavStep::None;
        }
        if target == 0 {
            self.index = 0;
            self.current_id = None;
            return NavStep::ShowLive;
        }
        let id = self.ids[target - 1].clone();
        self.pending = Some(Pending {
            target,
            id: id.clone(),
        });
        NavStep::Load { id }
    }

    /// 读取完成。
    ///
    /// # 参数
    /// - `id`：读完的记录 ID（与进行中的读取不符视为过期）。
    /// - `outcome`：读取结果。
    ///
    /// # 返回
    /// [`FinishStep::Apply`] 表示应当应用这条记录；失败、过期或记录已消失时为 [`FinishStep::Stay`]。
    pub fn finish(&mut self, id: &str, outcome: LoadOutcome) -> FinishStep {
        let Some(pending) = self.pending.take_if(|p| p.id == id) else {
            return FinishStep::Stay;
        };
        // 读取期间列表可能已刷新：以 ID 重新定位目标
        let position = self
            .ids
            .iter()
            .position(|candidate| *candidate == pending.id);
        match (outcome, position) {
            (LoadOutcome::Loaded, Some(position)) => {
                self.index = position + 1;
                self.current_id = Some(pending.id);
                FinishStep::Apply
            }
            (LoadOutcome::Failed, Some(position)) => {
                self.skipped.insert(pending.id);
                self.ids.remove(position);
                // 被移除的记录比当前位置更靠前（更新）时，当前索引随之前移
                if position < self.index.saturating_sub(1) {
                    self.index -= 1;
                }
                FinishStep::Stay
            }
            (_, None) => FinishStep::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造三条记录 `c`（最新）、`b`、`a`（最旧）的状态机。
    fn nav() -> HistoryNav {
        let mut nav = HistoryNav::new();
        nav.set_ids(vec!["c".into(), "b".into(), "a".into()]);
        nav
    }

    /// 读取指定记录并应用。
    fn load_and_apply(nav: &mut HistoryNav, id: &str) {
        assert_eq!(nav.finish(id, LoadOutcome::Loaded), FinishStep::Apply);
    }

    /// 从当前截图往旧翻：依次读取 c、b、a，到最旧一条后不再前进。
    #[test]
    fn previous_walks_to_oldest_and_stops() {
        let mut nav = nav();
        assert_eq!(nav.older(), NavStep::Load { id: "c".into() });
        load_and_apply(&mut nav, "c");
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.older(), NavStep::Load { id: "b".into() });
        load_and_apply(&mut nav, "b");
        assert_eq!(nav.older(), NavStep::Load { id: "a".into() });
        load_and_apply(&mut nav, "a");
        assert_eq!(nav.index(), 3);
        assert_eq!(nav.older(), NavStep::None);
    }

    /// 往新翻到 0 时是切回当前截图（不再读盘）；已在当前截图再往新翻无动作。
    #[test]
    fn next_returns_to_live_without_loading() {
        let mut nav = nav();
        assert_eq!(nav.newer(), NavStep::None);
        nav.older();
        load_and_apply(&mut nav, "c");
        assert!(nav.in_history());
        assert_eq!(nav.newer(), NavStep::ShowLive);
        assert_eq!(nav.index(), 0);
        assert!(!nav.in_history());
    }

    /// 读取期间忽略新的导航；过期的完成回调被丢弃。
    #[test]
    fn busy_ignores_navigation_and_stale_finish() {
        let mut nav = nav();
        nav.older();
        assert!(nav.busy());
        assert_eq!(nav.older(), NavStep::None);
        assert_eq!(nav.return_to_live(), NavStep::None);
        assert_eq!(nav.finish("zzz", LoadOutcome::Loaded), FinishStep::Stay);
        assert!(nav.busy());
        load_and_apply(&mut nav, "c");
        assert!(!nav.busy());
    }

    /// 右键直接回当前截图；本来就在当前截图时无动作。
    #[test]
    fn return_to_live_from_deep_history() {
        let mut nav = nav();
        assert_eq!(nav.return_to_live(), NavStep::None);
        nav.older();
        load_and_apply(&mut nav, "c");
        nav.older();
        load_and_apply(&mut nav, "b");
        assert_eq!(nav.return_to_live(), NavStep::ShowLive);
        assert_eq!(nav.index(), 0);
    }

    /// 读取失败：该记录从导航列表移除，位置不动；再翻一次会跳到后面的记录；
    /// 即使列表从磁盘刷新后又带上它，也不会再出现。
    #[test]
    fn failed_load_removes_entry_and_stays() {
        let mut nav = nav();
        nav.older();
        assert_eq!(nav.finish("c", LoadOutcome::Failed), FinishStep::Stay);
        assert_eq!(nav.index(), 0);
        nav.set_ids(vec!["c".into(), "b".into(), "a".into()]);
        assert_eq!(nav.older(), NavStep::Load { id: "b".into() });
    }

    /// 往新翻读取失败：被移除的记录比当前更新，当前索引随之前移。
    #[test]
    fn failed_load_toward_newer_shifts_index() {
        let mut nav = nav();
        nav.older();
        load_and_apply(&mut nav, "c");
        nav.older();
        load_and_apply(&mut nav, "b");
        nav.older();
        load_and_apply(&mut nav, "a");
        assert_eq!(nav.index(), 3);
        assert_eq!(nav.newer(), NavStep::Load { id: "b".into() });
        assert_eq!(nav.finish("b", LoadOutcome::Failed), FinishStep::Stay);
        // 列表变成 [c, a]，仍停在 a，索引 2
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.newer(), NavStep::Load { id: "c".into() });
    }

    /// 刷新列表：别处新写入记录后，按正显示的记录 ID 重新定位索引。
    #[test]
    fn set_ids_relocates_current_record() {
        let mut nav = nav();
        nav.older();
        load_and_apply(&mut nav, "c");
        assert_eq!(nav.index(), 1);
        nav.set_ids(vec!["d".into(), "c".into(), "b".into(), "a".into()]);
        assert_eq!(nav.index(), 2);
        // 正显示的记录被清理：索引夹在列表范围内
        nav.set_ids(vec!["d".into()]);
        assert_eq!(nav.index(), 1);
    }

    /// 真实仓储 + 后台线程：写两条带现场的记录，提供者能列出 ID 并异步读回整帧与标注历史。
    #[test]
    fn threaded_provider_loads_entries_from_disk() {
        use crate::history_store::{HistorySnapshot, HistorySource};
        use snow_history::pin_id::new_uuid_v4;
        use snow_history::timeutil::now_utc_ms;
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!(
            "cisox-nav-{}-{}",
            std::process::id(),
            new_uuid_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let policy = crate::history_store::policy_from_document(
            &snow_config::document::ConfigDocument::from_bytes(None),
        );
        let store = HistoryStore::new(&root, policy.clone());
        let canvas = br#"{"schema_version":1}"#.to_vec();
        let snapshot = |shade: u8| HistorySnapshot {
            frame_width: 16,
            frame_height: 12,
            frame_rgba: vec![shade; 16 * 12 * 4],
            selection: (1, 2, 8, 6),
            canvas_history: canvas.clone(),
            result_width: 8,
            result_height: 6,
            result_rgba: vec![shade; 8 * 6 * 4],
        };
        let now = now_utc_ms();
        store
            .record_snapshot(HistorySource::Copied, &snapshot(10), now)
            .unwrap();
        store
            .record_snapshot(HistorySource::Saved, &snapshot(20), now + 1000)
            .unwrap();

        let mut provider = ThreadedHistoryProvider::start(&root, policy).unwrap();
        let ids = provider.ids();
        assert_eq!(ids.len(), 2);
        provider.begin_load(&ids[0]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let (id, entry) = loop {
            if let Some(done) = provider.poll_loaded() {
                break done;
            }
            assert!(Instant::now() < deadline, "后台读取超时");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(id, ids[0]);
        let entry = entry.expect("应读回现场");
        assert_eq!(entry.frame_rgba, vec![20; 16 * 12 * 4]);
        assert_eq!(entry.canvas_history, canvas);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 没有任何记录时无处可翻。
    #[test]
    fn empty_history_has_nowhere_to_go() {
        let mut nav = HistoryNav::new();
        assert_eq!(nav.older(), NavStep::None);
        assert_eq!(nav.newer(), NavStep::None);
        assert_eq!(nav.return_to_live(), NavStep::None);
    }
}
