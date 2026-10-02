//! 多包路由：按语言对在若干翻译包之间选择，并控制同时常驻内存的 worker 个数。
//!
//! - [`RouteMode`]：`single`（指定包）、`specialized_first`（专用包优先）、`mixed_split`（混合拆分）；
//! - [`RoutedEngine`]：持有每个包一个的懒加载引擎，首次用到才拉起，各自空闲超时退出；
//! - 常驻上限 [`RoutePolicy::max_resident`]：要加载新包时先卸载**空闲**的旧包（最久未用优先），
//!   正在处理请求的引擎绝不会被驱逐；
//! - 混合拆分：同一次请求里的所有片段先全部分配好引擎，再按引擎分组，每个引擎每次请求最多
//!   加载一次，最后按原顺序拼回；没有任何包支持的片段原样保留；
//! - 池锁只保护记账：查询常驻状态与卸载都在锁外执行，被选中卸载的包先标记“卸载中”，
//!   期间的 acquire 等它卸完再用，避免阻塞其它请求；
//! - [`RoutedEngine::shutdown`] 之后所有请求直接报错，卸载用有界超时。

use crate::script_split::ScriptSplitter;
use crate::segment::{
    DEFAULT_MIN_SEGMENT_WEIGHT, SegmentSplitter, merge_short_segments, same_language,
    segment_weight, split_whitespace_edges,
};
use crate::worker::{MemorySnapshot, WorkerEngine};
use crate::{Lang, ModelManifest, TranslateError, TranslationEngine};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, mpsc};
use std::time::{Duration, Instant};

/// 默认最大同时常驻包数。
pub const DEFAULT_MAX_RESIDENT: usize = 1;
/// 引擎对外展示的名称（与单包时一致）。
const ENGINE_NAME: &str = "LocalNmt";
/// 关机时等待各包卸载的总时限（超时放弃等待，不再被忙碌的 worker 拖住）。
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 关机后再请求时的错误说明。
const CLOSED_MESSAGE: &str = "翻译引擎已关闭";

/// 路由模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteMode {
    /// 指定包：用户选哪个包就用哪个（未指定或不支持时取第一个支持的）。
    Single,
    /// 专用包优先：未指定包时，优先选显式声明语言对的窄包，其次才是通用多语包。
    #[default]
    SpecializedFirst,
    /// 混合拆分：文本切成单语片段，英文等走各自的专用包，其余走通用包，按原序拼回。
    MixedSplit,
}

impl RouteMode {
    /// 配置里的取值。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::router::RouteMode;
    /// assert_eq!(RouteMode::MixedSplit.code(), "mixed_split");
    /// ```
    pub const fn code(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::SpecializedFirst => "specialized_first",
            Self::MixedSplit => "mixed_split",
        }
    }

    /// 从配置取值解析（去首尾空白）；无法识别时返回 `None`。
    ///
    /// # 参数
    /// - `code`：配置值。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::router::RouteMode;
    /// assert_eq!(RouteMode::from_code(" single "), Some(RouteMode::Single));
    /// assert_eq!(RouteMode::from_code("x"), None);
    /// ```
    pub fn from_code(code: &str) -> Option<Self> {
        [Self::Single, Self::SpecializedFirst, Self::MixedSplit]
            .into_iter()
            .find(|mode| mode.code() == code.trim())
    }
}

/// 路由策略（可在不重建引擎的情况下随设置变化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePolicy {
    /// 路由模式。
    pub mode: RouteMode,
    /// 用户指定的包 ID，空串表示不指定（混合拆分模式下忽略）。
    pub preferred_id: String,
    /// 最大同时常驻内存的包数（至少 1）。
    pub max_resident: usize,
}

impl Default for RoutePolicy {
    /// 默认：专用包优先、不指定包、同时只留一个包在内存。
    fn default() -> Self {
        Self {
            mode: RouteMode::default(),
            preferred_id: String::new(),
            max_resident: DEFAULT_MAX_RESIDENT,
        }
    }
}

/// 可被路由器按需驱逐的引擎：在翻译引擎之上多出“是否常驻 / 卸载”两个操作。
pub trait PooledEngine: TranslationEngine {
    /// 模型当前是否占着内存（worker 进程在运行）。
    fn is_resident(&self) -> bool;

    /// 卸载模型释放内存；下次翻译时会重新加载。
    fn unload(&self);

    /// 累计加载（拉起）次数。
    fn launch_count(&self) -> u32;

    /// 运行中引擎的内存快照；未运行返回 `None`。
    fn memory_snapshot(&self) -> Option<MemorySnapshot>;
}

impl PooledEngine for WorkerEngine {
    /// worker 进程是否在运行。
    fn is_resident(&self) -> bool {
        self.is_running()
    }

    /// 结束 worker 进程。
    fn unload(&self) {
        self.shutdown();
    }

    /// 累计拉起次数。
    fn launch_count(&self) -> u32 {
        WorkerEngine::launch_count(self)
    }

    /// worker 内存快照。
    fn memory_snapshot(&self) -> Option<MemorySnapshot> {
        WorkerEngine::memory_snapshot(self)
    }
}

/// 在清单列表里选一个包的下标。
///
/// - `Single`：`preferred` 存在且支持就用它，否则取第一个支持的；
/// - `SpecializedFirst`：`preferred` 存在且支持就用它；否则（`default_eligible=false` 的包不参与，除非没有别的包支持）优先显式声明语言对的专用包
///   （语言对少的更窄，优先），没有专用包才取第一个支持的；
/// - `MixedSplit`：忽略 `preferred`，规则同上。
///
/// # 参数
/// - `manifests`：候选清单（顺序即优先顺序）。
/// - `preferred`：指定的包 ID，空串表示不指定。
/// - `src` / `tgt`：语言对（`src` 可为 `Auto`）。
/// - `mode`：路由模式。
///
/// # 返回
/// 选中的下标；没有任何包支持该语言对返回 `None`。
pub fn pick_index(
    manifests: &[&ModelManifest],
    preferred: &str,
    src: Lang,
    tgt: Lang,
    mode: RouteMode,
) -> Option<usize> {
    let candidates: Vec<usize> = (0..manifests.len())
        .filter(|&i| manifests[i].supports(src, tgt))
        .collect();
    candidates.first()?;
    let preferred = preferred.trim();
    if mode != RouteMode::MixedSplit
        && !preferred.is_empty()
        && let Some(index) = candidates
            .iter()
            .copied()
            .find(|&i| manifests[i].id == preferred)
    {
        return Some(index);
    }
    // 默认选包只看 default_eligible 的包；全是“仅手动指定”的包时才退回第一个支持者
    let eligible: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&i| manifests[i].default_eligible)
        .collect();
    let pool = if eligible.is_empty() {
        &candidates
    } else {
        &eligible
    };
    let first = pool[0];
    if mode == RouteMode::Single {
        return Some(first);
    }
    pool.iter()
        .copied()
        .filter(|&i| manifests[i].is_specialized())
        .min_by_key(|&i| (manifests[i].supported_pairs().len(), i))
        .or(Some(first))
}

/// 路由器里的一个包。
pub struct RoutedSlot {
    /// 包清单。
    pub manifest: ModelManifest,
    /// 该包的懒加载引擎。
    pub engine: Arc<dyn PooledEngine>,
}

/// 驱逐用的使用记录（同一把锁保护，保证“占用”与“驱逐”不会交错）。
struct PoolState {
    /// 每个包正在处理的请求数。
    in_flight: Vec<u32>,
    /// 每个包最近一次使用的逻辑时钟（越小越久未用）。
    last_used: Vec<u64>,
    /// 逻辑时钟。
    clock: u64,
    /// 每个包是否正被卸载（锁外执行卸载期间为真，acquire 需等其结束）。
    unloading: Vec<bool>,
    /// 路由器是否已关闭（关闭后拒绝新请求）。
    closed: bool,
}

/// 一次占用：存活期间该包不会被驱逐，释放时顺带收缩超限的常驻包。
struct Lease<'a> {
    /// 所属路由器。
    router: &'a RoutedEngine,
    /// 包下标。
    index: usize,
}

impl Lease<'_> {
    /// 被占用的引擎。
    fn engine(&self) -> &dyn PooledEngine {
        self.router.slots[self.index].engine.as_ref()
    }
}

impl Drop for Lease<'_> {
    /// 释放占用并按常驻上限收缩。
    fn drop(&mut self) {
        self.router.release(self.index);
    }
}

/// 一个混合拆分里待翻译的片段正文。
struct Job {
    /// 分配到的包下标。
    slot: usize,
    /// 解析后的具体源语言。
    src: Lang,
    /// 待翻译正文（已去首尾空白）。
    text: String,
    /// 译文。
    out: Option<String>,
}

/// 混合拆分后一段原文的组成部分。
enum Part {
    /// 原样保留（空白、与目标语言相同的片段）。
    Keep(String),
    /// 译文占位：前导空白 + 第 `job` 个任务的译文 + 尾部空白。
    Translated {
        /// 前导空白。
        lead: String,
        /// 尾部空白。
        trail: String,
        /// 任务下标。
        job: usize,
    },
}

/// 多包路由引擎：对外是一个 [`TranslationEngine`]，内部按策略在多个包之间分配请求。
pub struct RoutedEngine {
    /// 全部包（顺序即“第一个支持的”优先顺序，通常按 id 排序）。
    slots: Vec<RoutedSlot>,
    /// 当前路由策略。
    policy: RwLock<RoutePolicy>,
    /// 混合拆分用的识别器。
    splitter: Arc<dyn SegmentSplitter>,
    /// 短片段并入阈值。
    min_segment_weight: usize,
    /// 驱逐状态。
    pool: Mutex<PoolState>,
    /// 某个包卸载完成（或路由器关闭）时唤醒等待的 acquire。
    unload_done: Condvar,
    /// 关机时等待卸载的总时限。
    shutdown_timeout: Duration,
}

impl RoutedEngine {
    /// 创建路由引擎（不加载任何模型；识别器默认为 [`ScriptSplitter`]）。
    ///
    /// # 参数
    /// - `slots`：全部包。
    /// - `policy`：初始路由策略。
    pub fn new(slots: Vec<RoutedSlot>, policy: RoutePolicy) -> Self {
        let count = slots.len();
        Self {
            slots,
            policy: RwLock::new(policy),
            splitter: Arc::new(ScriptSplitter),
            min_segment_weight: DEFAULT_MIN_SEGMENT_WEIGHT,
            pool: Mutex::new(PoolState {
                in_flight: vec![0; count],
                last_used: vec![0; count],
                clock: 0,
                unloading: vec![false; count],
                closed: false,
            }),
            unload_done: Condvar::new(),
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// 调整关机时等待卸载的总时限。
    ///
    /// # 参数
    /// - `timeout`：总时限，超时后 [`RoutedEngine::shutdown`] 不再等待。
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// 换上混合拆分用的识别器。
    ///
    /// # 参数
    /// - `splitter`：识别器实现（自行懒加载、空闲释放）。
    pub fn with_splitter(mut self, splitter: Arc<dyn SegmentSplitter>) -> Self {
        self.splitter = splitter;
        self
    }

    /// 调整短片段并入阈值。
    ///
    /// # 参数
    /// - `weight`：分量阈值，见 [`crate::segment::segment_weight`]。
    pub fn with_min_segment_weight(mut self, weight: usize) -> Self {
        self.min_segment_weight = weight;
        self
    }

    /// 当前路由策略的副本。
    pub fn policy(&self) -> RoutePolicy {
        self.policy
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 更新路由策略（不重建引擎、不影响已加载的 worker）。
    ///
    /// # 参数
    /// - `policy`：新策略；`max_resident` 为 0 时按 1 处理。调小上限时立即驱逐空闲的最久未用包
    ///   （忙碌的包等其释放时再收缩）。
    pub fn set_policy(&self, mut policy: RoutePolicy) {
        policy.max_resident = policy.max_resident.max(1);
        let keep = policy.max_resident;
        *self.policy.write().unwrap_or_else(PoisonError::into_inner) = policy;
        self.evict_idle(keep, None);
    }

    /// 是否有任一包常驻内存。
    pub fn any_resident(&self) -> bool {
        !self.resident_ids().is_empty()
    }

    /// 当前常驻内存的包 ID（按包顺序）。忙碌与卸载中的包算常驻；其余在锁外查询状态。
    pub fn resident_ids(&self) -> Vec<String> {
        let busy = self.busy_flags();
        (0..self.slots.len())
            .filter(|&i| busy[i] || self.slots[i].engine.is_resident())
            .map(|i| self.slots[i].manifest.id.clone())
            .collect()
    }

    /// 全部包累计加载次数之和。
    pub fn launch_total(&self) -> u32 {
        self.slots.iter().map(|s| s.engine.launch_count()).sum()
    }

    /// 取第一个常驻且空闲的包的内存快照。
    pub fn memory_snapshot(&self) -> Option<MemorySnapshot> {
        let busy = self.busy_flags();
        (0..self.slots.len())
            .filter(|&i| !busy[i] && self.slots[i].engine.is_resident())
            .find_map(|i| self.slots[i].engine.memory_snapshot())
    }

    /// 关闭路由器并卸载全部包（应用退出时调用）。
    ///
    /// 关闭后新请求立即报 `WorkerUnavailable`，不会再拉起 worker；各包并行卸载，
    /// 总共最多等 [`RoutedEngine::with_shutdown_timeout`] 设定的时限，超时放弃等待
    /// （卸载线程在后台继续，不拖住调用方）。
    pub fn shutdown(&self) {
        self.lock_pool().closed = true;
        self.unload_done.notify_all();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let mut started = 0;
        for slot in &self.slots {
            let engine = Arc::clone(&slot.engine);
            let tx = done_tx.clone();
            let spawned = std::thread::Builder::new()
                .name("route-unload".into())
                .spawn(move || {
                    engine.unload();
                    let _ = tx.send(());
                });
            match spawned {
                Ok(_) => started += 1,
                Err(error) => tracing::warn!(%error, "无法启动卸载线程，放弃卸载该包"),
            }
        }
        drop(done_tx);
        let deadline = Instant::now() + self.shutdown_timeout;
        for _ in 0..started {
            let left = deadline.saturating_duration_since(Instant::now());
            if done_rx.recv_timeout(left).is_err() {
                tracing::warn!("关机卸载超时，不再等待仍在卸载的包");
                break;
            }
        }
    }

    /// 每个包是否“占着”：有请求在飞或正被卸载（这类包不查询引擎状态，也不可被驱逐）。
    fn busy_flags(&self) -> Vec<bool> {
        let pool = self.lock_pool();
        (0..self.slots.len())
            .map(|i| pool.in_flight[i] > 0 || pool.unloading[i])
            .collect()
    }

    /// 加驱逐状态的锁（忽略中毒）。
    fn lock_pool(&self) -> MutexGuard<'_, PoolState> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 全部清单的引用。
    fn manifests(&self) -> Vec<&ModelManifest> {
        self.slots.iter().map(|s| &s.manifest).collect()
    }

    /// 占用一个包：若它正被卸载先等其卸完；它尚未常驻时按上限驱逐空闲的旧包。
    ///
    /// # 错误
    /// 路由器已关闭时返回 `WorkerUnavailable`。
    fn acquire(&self, index: usize, max_resident: usize) -> Result<Lease<'_>, TranslateError> {
        let busy_elsewhere;
        {
            let mut pool = self.lock_pool();
            loop {
                if pool.closed {
                    return Err(TranslateError::WorkerUnavailable(CLOSED_MESSAGE.into()));
                }
                if !pool.unloading[index] {
                    break;
                }
                pool = self
                    .unload_done
                    .wait(pool)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            busy_elsewhere = pool.in_flight[index] > 0;
            pool.in_flight[index] += 1;
            pool.clock += 1;
            let now = pool.clock;
            pool.last_used[index] = now;
        }
        // 占用已登记：下面的状态查询与卸载都在池锁外，且本包不会被别人驱逐
        let lease = Lease {
            router: self,
            index,
        };
        if !busy_elsewhere && !self.slots[index].engine.is_resident() {
            self.evict_idle(max_resident.max(1) - 1, Some(index));
        }
        Ok(lease)
    }

    /// 释放占用并把常驻包数收缩到上限以内（只驱逐空闲的）。
    fn release(&self, index: usize) {
        let max_resident = self.policy().max_resident.max(1);
        {
            let mut pool = self.lock_pool();
            pool.in_flight[index] = pool.in_flight[index].saturating_sub(1);
            pool.clock += 1;
            let now = pool.clock;
            pool.last_used[index] = now;
        }
        self.evict_idle(max_resident, None);
    }

    /// 驱逐空闲的常驻包（最久未用优先），直到除 `except` 外的常驻数不超过 `keep`。
    ///
    /// 池锁内只做记账：先取快照，锁外查询常驻状态，选出受害者后在锁内复核并标记“卸载中”，
    /// 再在锁外卸载。有请求在飞或正在卸载的包算常驻但不可被选；候选都忙时放弃，等释放时再收缩。
    fn evict_idle(&self, keep: usize, except: Option<usize>) {
        loop {
            let (busy, last_used) = {
                let pool = self.lock_pool();
                if pool.closed {
                    return;
                }
                let busy: Vec<bool> = (0..self.slots.len())
                    .map(|i| pool.in_flight[i] > 0 || pool.unloading[i])
                    .collect();
                (busy, pool.last_used.clone())
            };
            let mut resident = 0;
            let mut victim: Option<usize> = None;
            for i in (0..self.slots.len()).filter(|&i| Some(i) != except) {
                if busy[i] {
                    resident += 1;
                } else if self.slots[i].engine.is_resident() {
                    resident += 1;
                    if victim.is_none_or(|v| last_used[i] < last_used[v]) {
                        victim = Some(i);
                    }
                }
            }
            if resident <= keep {
                return;
            }
            let Some(victim) = victim else {
                tracing::debug!("常驻包超出上限但都在处理请求，暂不驱逐");
                return;
            };
            {
                let mut pool = self.lock_pool();
                if pool.closed {
                    return;
                }
                if pool.in_flight[victim] > 0 || pool.unloading[victim] {
                    // 快照之后它被占用了：重新评估
                    continue;
                }
                pool.unloading[victim] = true;
            }
            tracing::info!(model = %self.slots[victim].manifest.id, "为腾出内存卸载空闲翻译包");
            self.slots[victim].engine.unload();
            self.lock_pool().unloading[victim] = false;
            self.unload_done.notify_all();
            if self.slots[victim].engine.is_resident() {
                return;
            }
        }
    }

    /// 单包路径（`single` / `specialized_first`）：选一个包，整批交给它。
    fn translate_routed(
        &self,
        texts: &[String],
        src: Lang,
        tgt: Lang,
        policy: &RoutePolicy,
    ) -> Result<Vec<String>, TranslateError> {
        let manifests = self.manifests();
        let index = pick_index(&manifests, &policy.preferred_id, src, tgt, policy.mode)
            .ok_or(TranslateError::UnsupportedLanguagePair(src, tgt))?;
        let resolved = manifests[index]
            .resolve_source(src, tgt)
            .ok_or(TranslateError::UnsupportedLanguagePair(src, tgt))?;
        let lease = self.acquire(index, policy.max_resident)?;
        lease.engine().translate_batch(texts, resolved, tgt)
    }

    /// 混合拆分路径：切片段 → 分配引擎 → 按引擎（再按源语言）分组翻译 → 按原序拼回。
    fn translate_mixed(
        &self,
        texts: &[String],
        src: Lang,
        tgt: Lang,
        policy: &RoutePolicy,
    ) -> Result<Vec<String>, TranslateError> {
        let manifests = self.manifests();
        let mut plans: Vec<Vec<Part>> = Vec::with_capacity(texts.len());
        let mut jobs: Vec<Job> = Vec::new();
        // 没有任何包支持的片段：原样保留；记下第一个，全部片段都不支持时才报错
        let mut first_unsupported: Option<(Lang, Lang)> = None;
        for text in texts {
            let mut parts = Vec::new();
            if text.trim().is_empty() {
                parts.push(Part::Keep(text.clone()));
                plans.push(parts);
                continue;
            }
            let segments =
                merge_short_segments(self.splitter.split(text, src), self.min_segment_weight);
            for segment in segments {
                let (lead, core, trail) = split_whitespace_edges(&segment.text);
                let route_src = segment.lang.unwrap_or(src);
                // 无字母的片段（数字、标点、emoji）没有可翻译的内容，也原样保留
                if segment_weight(core) == 0 || same_language(route_src, tgt) {
                    parts.push(Part::Keep(segment.text.clone()));
                    continue;
                }
                // 先把所有片段的引擎分配好：加载任何模型之前就确定路由
                let assigned =
                    pick_index(&manifests, "", route_src, tgt, RouteMode::SpecializedFirst)
                        .and_then(|slot| {
                            manifests[slot]
                                .resolve_source(route_src, tgt)
                                .map(|resolved| (slot, resolved))
                        });
                let Some((slot, resolved)) = assigned else {
                    first_unsupported.get_or_insert((route_src, tgt));
                    parts.push(Part::Keep(segment.text.clone()));
                    continue;
                };
                parts.push(Part::Translated {
                    lead: lead.to_string(),
                    trail: trail.to_string(),
                    job: jobs.len(),
                });
                jobs.push(Job {
                    slot,
                    src: resolved,
                    text: core.to_string(),
                    out: None,
                });
            }
            if parts.is_empty() {
                // 识别器对非空文本没给出任何片段：整段原样保留，不能变成空串
                parts.push(Part::Keep(text.clone()));
            }
            plans.push(parts);
        }
        if jobs.is_empty()
            && let Some((from, to)) = first_unsupported
        {
            return Err(TranslateError::UnsupportedLanguagePair(from, to));
        }
        let mut slot_order: Vec<usize> = Vec::new();
        for job in &jobs {
            if !slot_order.contains(&job.slot) {
                slot_order.push(job.slot);
            }
        }
        for slot in slot_order {
            // 每个引擎一次占用：期间不会被驱逐，也只加载一次
            let lease = self.acquire(slot, policy.max_resident)?;
            let mut sources: Vec<Lang> = Vec::new();
            for job in jobs.iter().filter(|j| j.slot == slot) {
                if !sources.contains(&job.src) {
                    sources.push(job.src);
                }
            }
            for source in sources {
                let members: Vec<usize> = (0..jobs.len())
                    .filter(|&j| jobs[j].slot == slot && jobs[j].src == source)
                    .collect();
                let batch: Vec<String> = members.iter().map(|&j| jobs[j].text.clone()).collect();
                let translated = lease.engine().translate_batch(&batch, source, tgt)?;
                if translated.len() != batch.len() {
                    return Err(TranslateError::Inference("后端返回的译文条数不符".into()));
                }
                for (&member, text) in members.iter().zip(translated) {
                    jobs[member].out = Some(text);
                }
            }
        }
        plans
            .into_iter()
            .map(|parts| {
                let mut joined = String::new();
                for part in parts {
                    match part {
                        Part::Keep(text) => joined.push_str(&text),
                        Part::Translated { lead, trail, job } => {
                            let out = jobs[job]
                                .out
                                .as_deref()
                                .ok_or_else(|| TranslateError::Inference("缺少片段译文".into()))?;
                            joined.push_str(&lead);
                            joined.push_str(out);
                            joined.push_str(&trail);
                        }
                    }
                }
                Ok(joined)
            })
            .collect()
    }
}

impl TranslationEngine for RoutedEngine {
    /// 翻译单条文本（走批量路径）。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        let mut out = self.translate_batch(&[text.to_string()], src, tgt)?;
        out.pop()
            .ok_or_else(|| TranslateError::Inference("后端没有返回译文".into()))
    }

    /// 批量翻译：全是空白时不占用任何引擎；否则按当前路由策略分流。
    fn translate_batch(
        &self,
        texts: &[String],
        src: Lang,
        tgt: Lang,
    ) -> Result<Vec<String>, TranslateError> {
        if texts.iter().all(|t| t.trim().is_empty()) {
            return Ok(texts.to_vec());
        }
        let policy = self.policy();
        match policy.mode {
            RouteMode::MixedSplit => self.translate_mixed(texts, src, tgt, &policy),
            RouteMode::Single | RouteMode::SpecializedFirst => {
                self.translate_routed(texts, src, tgt, &policy)
            }
        }
    }

    /// 全部包支持的语言对（去重）。
    fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
        let mut out: Vec<(Lang, Lang)> = Vec::new();
        for slot in &self.slots {
            for pair in slot.manifest.supported_pairs() {
                if !out.contains(&pair) {
                    out.push(pair);
                }
            }
        }
        out
    }

    /// 引擎名称。
    fn engine_name(&self) -> &'static str {
        ENGINE_NAME
    }

    /// 缓存标识：路由模式、指定包、识别器与各包标识（任何一项变化都不复用旧译文）。
    fn cache_id(&self) -> String {
        let policy = self.policy();
        let slots: Vec<String> = self.slots.iter().map(|s| s.engine.cache_id()).collect();
        format!(
            "route:{}:{}:{}:{}",
            policy.mode.code(),
            policy.preferred_id.trim(),
            self.splitter.name(),
            slots.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{NoSplit, Segment};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    /// 假引擎：记录每次调用，翻译成 `<id>:<src>:<text>`，可控“是否常驻”。
    struct FakeEngine {
        /// 包 ID。
        id: String,
        /// 是否常驻。
        resident: AtomicBool,
        /// 累计加载次数。
        launches: AtomicU32,
        /// 收到的批次：(源语言代号, 文本)。
        calls: Mutex<Vec<(String, Vec<String>)>>,
        /// 卸载次数。
        unloads: AtomicU32,
        /// 翻译进行中时被调用 `unload` 的次数（驱逐忙引擎的证据）。
        unloaded_while_busy: AtomicU32,
        /// 是否正在翻译。
        busy: AtomicBool,
        /// 卸载耗时（毫秒），模拟忙碌 worker 的慢卸载。
        unload_delay_ms: AtomicU64,
        /// 是否正处在卸载过程中。
        unloading_now: AtomicBool,
        /// 翻译时的回调（测试里用来在“忙”的时候触发别的请求）。
        on_translate: Mutex<Option<Box<dyn FnMut() + Send>>>,
    }

    impl FakeEngine {
        /// 新建（未常驻）。
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                resident: AtomicBool::new(false),
                launches: AtomicU32::new(0),
                calls: Mutex::new(Vec::new()),
                unloads: AtomicU32::new(0),
                unloaded_while_busy: AtomicU32::new(0),
                busy: AtomicBool::new(false),
                unload_delay_ms: AtomicU64::new(0),
                unloading_now: AtomicBool::new(false),
                on_translate: Mutex::new(None),
            })
        }

        /// 批次数。
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl TranslationEngine for FakeEngine {
        fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
            self.translate_batch(&[text.to_string()], src, tgt)
                .map(|mut v| v.remove(0))
        }

        fn translate_batch(
            &self,
            texts: &[String],
            src: Lang,
            _tgt: Lang,
        ) -> Result<Vec<String>, TranslateError> {
            if !self.resident.swap(true, Ordering::SeqCst) {
                self.launches.fetch_add(1, Ordering::SeqCst);
            }
            self.busy.store(true, Ordering::SeqCst);
            if let Some(hook) = self.on_translate.lock().unwrap().as_mut() {
                hook();
            }
            self.calls
                .lock()
                .unwrap()
                .push((src.code().to_string(), texts.to_vec()));
            self.busy.store(false, Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|t| format!("{}:{}:{t}", self.id, src.code()))
                .collect())
        }

        fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
            Vec::new()
        }

        fn engine_name(&self) -> &'static str {
            "fake"
        }

        fn cache_id(&self) -> String {
            format!("fake:{}", self.id)
        }
    }

    impl PooledEngine for FakeEngine {
        fn is_resident(&self) -> bool {
            self.resident.load(Ordering::SeqCst)
        }

        fn unload(&self) {
            if self.busy.load(Ordering::SeqCst) {
                self.unloaded_while_busy.fetch_add(1, Ordering::SeqCst);
            }
            self.unloads.fetch_add(1, Ordering::SeqCst);
            self.unloading_now.store(true, Ordering::SeqCst);
            let delay = self.unload_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
            self.resident.store(false, Ordering::SeqCst);
            self.unloading_now.store(false, Ordering::SeqCst);
        }

        fn launch_count(&self) -> u32 {
            self.launches.load(Ordering::SeqCst)
        }

        fn memory_snapshot(&self) -> Option<MemorySnapshot> {
            None
        }
    }

    /// 预设的切分结果：（片段文本, 语言）列表。
    type Parts<'a> = &'a [(&'a str, Option<Lang>)];

    /// 假识别器：按预设表返回片段，表里没有的文本整段未知。
    struct ScriptedSplitter {
        /// 文本 → 片段（文本, 语言）。
        table: HashMap<String, Vec<(String, Option<Lang>)>>,
    }

    impl SegmentSplitter for ScriptedSplitter {
        fn split(&self, text: &str, _hint: Lang) -> Vec<Segment> {
            match self.table.get(text) {
                Some(parts) => parts
                    .iter()
                    .map(|(t, l)| Segment {
                        text: t.clone(),
                        lang: *l,
                    })
                    .collect(),
                None => vec![Segment {
                    text: text.into(),
                    lang: None,
                }],
            }
        }

        fn name(&self) -> &'static str {
            "scripted"
        }
    }

    /// 造清单。`pairs` 为空时只声明 `languages`（通用多语包）。
    fn manifest(id: &str, languages: &[&str], pairs: &[(&str, &str)]) -> ModelManifest {
        let langs = languages
            .iter()
            .map(|l| format!("\"{l}\""))
            .collect::<Vec<_>>()
            .join(",");
        let pair_text = pairs
            .iter()
            .map(|(s, t)| format!("[\"{s}\",\"{t}\"]"))
            .collect::<Vec<_>>()
            .join(",");
        serde_json::from_str(&format!(
            r#"{{"schema_version":1,"id":"{id}","family":"marian","languages":[{langs}],"pairs":[{pair_text}]}}"#
        ))
        .expect("清单")
    }

    /// 通用包：中英日韩互译。
    fn general() -> ModelManifest {
        manifest("nllb", &["zh-CN", "en", "ja", "ko"], &[])
    }

    /// 专用包：只英译中。
    fn specialized() -> ModelManifest {
        manifest("opus", &["en", "zh-CN"], &[("en", "zh-CN")])
    }

    /// 一组测试用路由器：返回路由器与两个假引擎（通用、专用）。
    fn router(policy: RoutePolicy) -> (RoutedEngine, Arc<FakeEngine>, Arc<FakeEngine>) {
        let general_engine = FakeEngine::new("nllb");
        let special_engine = FakeEngine::new("opus");
        let slots = vec![
            RoutedSlot {
                manifest: general(),
                engine: general_engine.clone(),
            },
            RoutedSlot {
                manifest: specialized(),
                engine: special_engine.clone(),
            },
        ];
        (
            RoutedEngine::new(slots, policy),
            general_engine,
            special_engine,
        )
    }

    /// 带识别器的策略快捷构造。
    fn mixed_policy(max_resident: usize) -> RoutePolicy {
        RoutePolicy {
            mode: RouteMode::MixedSplit,
            preferred_id: String::new(),
            max_resident,
        }
    }

    /// 单文本便捷调用。
    fn run(
        engine: &RoutedEngine,
        text: &str,
        src: Lang,
        tgt: Lang,
    ) -> Result<String, TranslateError> {
        engine.translate(text, src, tgt)
    }

    /// 路由模式代号往返。
    #[test]
    fn route_mode_codes_round_trip() {
        for mode in [
            RouteMode::Single,
            RouteMode::SpecializedFirst,
            RouteMode::MixedSplit,
        ] {
            assert_eq!(RouteMode::from_code(mode.code()), Some(mode));
        }
        assert_eq!(RouteMode::from_code("nope"), None);
        assert_eq!(RouteMode::default(), RouteMode::SpecializedFirst);
        assert_eq!(RoutePolicy::default().max_resident, 1);
    }

    /// 选包：single 取第一个（按顺序）；专用包优先选显式声明的窄包；指定包优先；混合拆分忽略指定包。
    #[test]
    fn pick_index_rules() {
        let (g, s) = (general(), specialized());
        let list = [&g, &s];
        let pick = |preferred: &str, src, tgt, mode| pick_index(&list, preferred, src, tgt, mode);
        assert_eq!(pick("", Lang::En, Lang::ZhHans, RouteMode::Single), Some(0));
        assert_eq!(
            pick("", Lang::En, Lang::ZhHans, RouteMode::SpecializedFirst),
            Some(1)
        );
        assert_eq!(
            pick("", Lang::Auto, Lang::ZhHans, RouteMode::SpecializedFirst),
            Some(1)
        );
        assert_eq!(
            pick("nllb", Lang::En, Lang::ZhHans, RouteMode::SpecializedFirst),
            Some(0),
            "显式指定仍尊重"
        );
        assert_eq!(
            pick("nllb", Lang::En, Lang::ZhHans, RouteMode::MixedSplit),
            Some(1),
            "混合拆分忽略指定包"
        );
        assert_eq!(
            pick("opus", Lang::Ja, Lang::ZhHans, RouteMode::Single),
            Some(0),
            "指定包不支持时退回"
        );
        assert_eq!(
            pick("", Lang::Ja, Lang::ZhHans, RouteMode::SpecializedFirst),
            Some(0),
            "专用包不覆盖时用通用包"
        );
        assert_eq!(pick("", Lang::En, Lang::Fr, RouteMode::Single), None);
        let narrow = manifest("narrow", &["en", "zh-CN"], &[("en", "zh-CN")]);
        let wide = manifest(
            "wide",
            &["en", "zh-CN"],
            &[("en", "zh-CN"), ("zh-CN", "en"), ("en", "zh-TW")],
        );
        let pair = [&wide, &narrow];
        assert_eq!(
            pick_index(
                &pair,
                "",
                Lang::En,
                Lang::ZhHans,
                RouteMode::SpecializedFirst
            ),
            Some(1),
            "语言对少的更窄"
        );
    }

    /// 造一个 `default_eligible=false` 的可选包（语言对较多，id 排在最前，最容易抢位）。
    fn optional_pack(id: &str) -> ModelManifest {
        let mut m = manifest(
            id,
            &["zh-CN", "en", "ja"],
            &[
                ("en", "zh-CN"),
                ("zh-CN", "en"),
                ("ja", "zh-CN"),
                ("zh-CN", "ja"),
            ],
        );
        m.default_eligible = false;
        m
    }

    /// 可选包（default_eligible=false）：任何路由模式都不被默认选中，显式指定才用；没有别的包时作兜底。
    #[test]
    fn optional_pack_never_wins_by_default() {
        let (g, s, h) = (general(), specialized(), optional_pack("a-hymt"));
        let list = [&h, &g, &s];
        for mode in [
            RouteMode::Single,
            RouteMode::SpecializedFirst,
            RouteMode::MixedSplit,
        ] {
            let got = pick_index(&list, "", Lang::En, Lang::ZhHans, mode);
            assert_ne!(got, Some(0), "{mode:?} 不应默认选可选包");
        }
        assert_eq!(
            pick_index(&list, "", Lang::En, Lang::ZhHans, RouteMode::Single),
            Some(1)
        );
        assert_eq!(
            pick_index(
                &list,
                "",
                Lang::En,
                Lang::ZhHans,
                RouteMode::SpecializedFirst
            ),
            Some(2)
        );
        assert_eq!(
            pick_index(
                &list,
                "a-hymt",
                Lang::En,
                Lang::ZhHans,
                RouteMode::SpecializedFirst
            ),
            Some(0),
            "显式指定仍尊重"
        );
        // 只有可选包装着时退回它，不报“不支持”
        let only = [&h];
        assert_eq!(
            pick_index(
                &only,
                "",
                Lang::En,
                Lang::ZhHans,
                RouteMode::SpecializedFirst
            ),
            Some(0)
        );
        // 缺省清单（不写字段）视为可参与默认选包
        assert!(general().default_eligible);
    }

    /// 专用包优先：英译中走专用包且不加载通用包；日译中走通用包。
    #[test]
    fn specialized_first_routes_by_pair() {
        let (engine, general_engine, special_engine) = router(RoutePolicy::default());
        assert_eq!(
            run(&engine, "hello", Lang::En, Lang::ZhHans).unwrap(),
            "opus:en:hello"
        );
        assert_eq!(general_engine.launch_count(), 0);
        assert_eq!(
            run(&engine, "こんにちは", Lang::Ja, Lang::ZhHans).unwrap(),
            "nllb:ja:こんにちは"
        );
        assert_eq!(
            special_engine.unloads.load(Ordering::SeqCst),
            1,
            "上限 1：换包时先卸载专用包"
        );
        assert_eq!(
            run(&engine, "x", Lang::En, Lang::Fr).unwrap_err(),
            TranslateError::UnsupportedLanguagePair(Lang::En, Lang::Fr)
        );
    }

    /// single：不指定时取第一个支持的（通用包）；指定专用包则用专用包。
    #[test]
    fn single_mode_respects_choice() {
        let policy = RoutePolicy {
            mode: RouteMode::Single,
            ..RoutePolicy::default()
        };
        let (engine, general_engine, _) = router(policy.clone());
        assert_eq!(
            run(&engine, "hi", Lang::En, Lang::ZhHans).unwrap(),
            "nllb:en:hi"
        );
        assert_eq!(general_engine.call_count(), 1);
        engine.set_policy(RoutePolicy {
            preferred_id: "opus".into(),
            ..policy
        });
        assert_eq!(
            run(&engine, "hi", Lang::En, Lang::ZhHans).unwrap(),
            "opus:en:hi"
        );
    }

    /// 缓存标识随模式、指定包变化；换模式不需要重建引擎。
    #[test]
    fn cache_id_tracks_policy() {
        let (engine, _, _) = router(RoutePolicy::default());
        let before = engine.cache_id();
        engine.set_policy(RoutePolicy {
            mode: RouteMode::Single,
            ..RoutePolicy::default()
        });
        assert_ne!(before, engine.cache_id());
        assert!(engine.cache_id().contains("fake:nllb") && engine.cache_id().contains("fake:opus"));
        engine.set_policy(RoutePolicy {
            max_resident: 0,
            ..RoutePolicy::default()
        });
        assert_eq!(engine.policy().max_resident, 1, "0 按 1 处理");
        assert_eq!(
            engine.supported_pairs().len(),
            12,
            "通用包 12 对，专用包的 en->zh 已含在内"
        );
    }

    /// 常驻上限 1：用到另一个包时先卸载空闲的旧包，任何时刻最多一个常驻。
    #[test]
    fn max_resident_one_evicts_idle() {
        let (engine, general_engine, special_engine) = router(RoutePolicy::default());
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["opus"]);
        run(&engine, "b", Lang::Ja, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["nllb"]);
        assert!(!special_engine.is_resident());
        run(&engine, "c", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["opus"]);
        assert!(!general_engine.is_resident());
        assert_eq!(engine.launch_total(), 3);
    }

    /// 常驻上限 2：两个包可同时常驻，不驱逐。
    #[test]
    fn max_resident_two_keeps_both() {
        let (engine, general_engine, special_engine) = router(RoutePolicy {
            max_resident: 2,
            ..RoutePolicy::default()
        });
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        run(&engine, "b", Lang::Ja, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["nllb", "opus"]);
        assert_eq!(
            general_engine.unloads.load(Ordering::SeqCst)
                + special_engine.unloads.load(Ordering::SeqCst),
            0
        );
        // 降回 1 后，下一次释放时收缩（驱逐最久未用的 nllb 之外的空闲包）
        engine.set_policy(RoutePolicy {
            max_resident: 1,
            ..RoutePolicy::default()
        });
        run(&engine, "c", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["opus"]);
        assert_eq!(general_engine.unloads.load(Ordering::SeqCst), 1);
    }

    /// worker 自己空闲超时退出后，路由器看到的是未常驻，再次使用不会驱逐别人。
    #[test]
    fn self_unloaded_engine_is_not_counted() {
        let (engine, general_engine, special_engine) = router(RoutePolicy::default());
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        special_engine.resident.store(false, Ordering::SeqCst);
        run(&engine, "b", Lang::Ja, Lang::ZhHans).unwrap();
        assert_eq!(
            special_engine.unloads.load(Ordering::SeqCst),
            0,
            "已自行退出的不必再卸载"
        );
        assert!(general_engine.is_resident());
    }

    /// 忙碌的包不会被驱逐：在专用包处理请求期间，另一线程要用通用包，专用包不被卸载，
    /// 请求结束释放时才按上限收缩。
    #[test]
    fn busy_engine_is_never_evicted() {
        let (engine, _, special_engine) = router(RoutePolicy::default());
        let engine = Arc::new(engine);
        let inner = Arc::clone(&engine);
        let seen_output = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&seen_output);
        // 专用包翻译进行中（占用未释放）时，在同一线程里再发起通用包请求，模拟并发
        *special_engine.on_translate.lock().unwrap() = Some(Box::new(move || {
            let out = inner
                .translate("こんにちは", Lang::Ja, Lang::ZhHans)
                .unwrap();
            seen.lock().unwrap().push(out);
        }));
        assert_eq!(
            run(&engine, "hello", Lang::En, Lang::ZhHans).unwrap(),
            "opus:en:hello"
        );
        assert_eq!(
            special_engine.unloaded_while_busy.load(Ordering::SeqCst),
            0,
            "忙碌的专用包不得被卸载"
        );
        assert_eq!(*seen_output.lock().unwrap(), ["nllb:ja:こんにちは"]);
        // 专用包在飞时通用包也被加载（超限但不杀忙碌者）；通用包用完即被收缩，专用包留下
        assert_eq!(engine.resident_ids(), ["opus"], "全部释放后收缩到上限");
    }

    /// 全是空白的批次不占用任何引擎、不加载模型。
    #[test]
    fn blank_batches_touch_nothing() {
        let (engine, general_engine, special_engine) = router(mixed_policy(1));
        let out = engine
            .translate_batch(&["  ".into(), "\n".into()], Lang::En, Lang::ZhHans)
            .unwrap();
        assert_eq!(out, ["  ", "\n"]);
        assert_eq!(engine.launch_total(), 0);
        assert!(!general_engine.is_resident() && !special_engine.is_resident());
    }

    /// 造带识别器的混合拆分路由器。
    fn mixed_router(
        max_resident: usize,
        table: &[(&str, Parts<'_>)],
    ) -> (RoutedEngine, Arc<FakeEngine>, Arc<FakeEngine>) {
        let (engine, g, s) = router(mixed_policy(max_resident));
        let table = table
            .iter()
            .map(|(text, parts)| {
                (
                    (*text).to_string(),
                    parts.iter().map(|(t, l)| ((*t).to_string(), *l)).collect(),
                )
            })
            .collect();
        (
            engine.with_splitter(Arc::new(ScriptedSplitter { table })),
            g,
            s,
        )
    }

    /// 混合拆分：英文片段交专用包，其余交通用包；按原序拼回，空白与标点保持。
    #[test]
    fn mixed_split_routes_and_reassembles() {
        let text = "你好 hello world こんにちは";
        let (engine, general_engine, special_engine) = mixed_router(
            1,
            &[(
                text,
                &[
                    ("你好 ", Some(Lang::ZhHans)),
                    ("hello world ", Some(Lang::En)),
                    ("こんにちは", Some(Lang::Ja)),
                ],
            )],
        );
        let out = run(&engine, text, Lang::Auto, Lang::ZhHans).unwrap();
        // 中文片段与目标语言相同，原样保留；英文走 opus；日文走 nllb；片段间的空格保持
        assert_eq!(out, "你好 opus:en:hello world nllb:ja:こんにちは");
        assert_eq!(special_engine.call_count(), 1);
        assert_eq!(general_engine.call_count(), 1);
        assert_eq!(
            engine.resident_ids(),
            ["nllb"],
            "上限 1：专用包先用完被驱逐，最后用到的通用包留下"
        );
        assert_eq!(special_engine.unloads.load(Ordering::SeqCst), 1);
    }

    /// 混合拆分：同一请求里所有英文片段一次性交给专用包，各引擎每次请求只加载一次。
    #[test]
    fn mixed_split_groups_by_engine_across_texts() {
        let first = "I like apples. こんにちは。 I like pears.";
        let second = "Nice to meet you. 안녕하세요.";
        let (engine, general_engine, special_engine) = mixed_router(
            1,
            &[
                (
                    first,
                    &[
                        ("I like apples. ", Some(Lang::En)),
                        ("こんにちは。 ", Some(Lang::Ja)),
                        ("I like pears.", Some(Lang::En)),
                    ],
                ),
                (
                    second,
                    &[
                        ("Nice to meet you. ", Some(Lang::En)),
                        ("안녕하세요.", Some(Lang::Ko)),
                    ],
                ),
            ],
        );
        let texts = vec![first.to_string(), second.to_string()];
        let out = engine
            .translate_batch(&texts, Lang::Auto, Lang::ZhHans)
            .unwrap();
        assert_eq!(
            out,
            [
                "opus:en:I like apples. nllb:ja:こんにちは。 opus:en:I like pears.",
                "opus:en:Nice to meet you. nllb:ko:안녕하세요."
            ]
        );
        let special_calls = special_engine.calls.lock().unwrap();
        assert_eq!(special_calls.len(), 1, "英文片段合成一批");
        assert_eq!(
            special_calls[0].1,
            ["I like apples.", "I like pears.", "Nice to meet you."]
        );
        let general_calls = general_engine.calls.lock().unwrap();
        assert_eq!(general_calls.len(), 2, "同一引擎按源语言分批");
        assert_eq!(engine.launch_total(), 2, "每个引擎只加载一次");
    }

    /// 混合拆分：纯英文、纯中文（与目标同语言原样返回且不加载任何模型）。
    #[test]
    fn mixed_split_pure_texts() {
        let (engine, _, special_engine) = mixed_router(
            1,
            &[
                ("Hello there", &[("Hello there", Some(Lang::En))]),
                ("你好世界", &[("你好世界", Some(Lang::ZhHans))]),
            ],
        );
        assert_eq!(
            run(&engine, "Hello there", Lang::Auto, Lang::ZhHans).unwrap(),
            "opus:en:Hello there"
        );
        assert_eq!(special_engine.call_count(), 1);
        let launches = engine.launch_total();
        assert_eq!(
            run(&engine, "你好世界", Lang::Auto, Lang::ZhHant).unwrap(),
            "你好世界"
        );
        assert_eq!(
            engine.launch_total(),
            launches,
            "同语言原样返回，不加载模型"
        );
    }

    /// 混合拆分：短片段按阈值并入邻居后再分配引擎（夹在中文里的 OK 跟着中文走）。
    #[test]
    fn mixed_split_merges_short_segments_before_routing() {
        let text = "今天天气很好 OK 我们出去玩";
        let (engine, general_engine, special_engine) = mixed_router(
            1,
            &[(
                text,
                &[
                    ("今天天气很好 ", Some(Lang::ZhHans)),
                    ("OK ", Some(Lang::En)),
                    ("我们出去玩", Some(Lang::ZhHans)),
                ],
            )],
        );
        assert_eq!(
            run(&engine, text, Lang::Auto, Lang::En).unwrap(),
            "nllb:zh-CN:今天天气很好 OK 我们出去玩"
        );
        assert_eq!(special_engine.call_count(), 0);
        assert_eq!(general_engine.call_count(), 1);
    }

    /// 混合拆分：语言未知的片段按用户源语言走；源语言为 Auto 时整段按 Auto 走（占位识别器的退化行为）。
    #[test]
    fn mixed_split_unknown_language_falls_back() {
        let (engine, _, special_engine) = router(mixed_policy(1));
        let engine = engine.with_splitter(Arc::new(NoSplit));
        // 占位识别器：整段一个未知片段，等价于专用包优先的整段翻译
        assert_eq!(
            run(&engine, "hello", Lang::Auto, Lang::ZhHans).unwrap(),
            "opus:en:hello"
        );
        assert_eq!(special_engine.call_count(), 1);
        assert_eq!(
            run(&engine, "こんにちは", Lang::Ja, Lang::ZhHans).unwrap(),
            "nllb:ja:こんにちは"
        );
        assert_eq!(
            run(&engine, "你好", Lang::ZhHans, Lang::ZhHant).unwrap(),
            "你好",
            "源与目标同语言原样返回"
        );
    }

    /// 混合拆分：某个片段没有任何包支持时原样保留，其余片段照常翻译，且不影响其它片段加载模型。
    #[test]
    fn mixed_split_keeps_unsupported_segment() {
        let text = "Hello Привет";
        let (engine, _, special_engine) = mixed_router(
            1,
            &[(
                text,
                &[("Hello ", Some(Lang::En)), ("Привет", Some(Lang::Ru))],
            )],
        );
        assert_eq!(
            run(&engine, text, Lang::Auto, Lang::ZhHans).unwrap(),
            "opus:en:Hello Привет"
        );
        assert_eq!(special_engine.call_count(), 1);
    }

    /// 混合拆分：全部片段都不支持时仍报 UnsupportedLanguagePair，且不加载任何模型。
    #[test]
    fn mixed_split_all_unsupported_fails_before_loading() {
        let text = "Привет мир";
        let (engine, _, _) = mixed_router(1, &[(text, &[(text, Some(Lang::Ru))])]);
        let err = run(&engine, text, Lang::Auto, Lang::ZhHans).unwrap_err();
        assert_eq!(
            err,
            TranslateError::UnsupportedLanguagePair(Lang::Ru, Lang::ZhHans)
        );
        assert_eq!(engine.launch_total(), 0, "不应触发任何模型加载");
    }

    /// 识别器对非空文本返回空列表时，整段原样保留而不是变成空串。
    #[test]
    fn mixed_split_empty_segmentation_keeps_text() {
        let (engine, _, _) = router(mixed_policy(1));
        let engine = engine.with_splitter(Arc::new(ScriptedSplitter {
            table: HashMap::from([("abc".to_string(), Vec::new())]),
        }));
        assert_eq!(
            run(&engine, "abc", Lang::Auto, Lang::ZhHans).unwrap(),
            "abc"
        );
        assert_eq!(engine.launch_total(), 0);
    }

    /// 默认识别器是脚本分段：中英日混写按语言分流并按原序拼回；纯数字标点片段不送去翻译。
    #[test]
    fn default_splitter_is_script_based() {
        let (engine, general_engine, special_engine) = router(mixed_policy(2));
        let out = run(
            &engine,
            "你好 hello world こんにちは 2024",
            Lang::Auto,
            Lang::ZhHans,
        )
        .unwrap();
        assert_eq!(out, "你好 opus:en:hello world nllb:ja:こんにちは 2024");
        assert_eq!(special_engine.call_count(), 1);
        assert_eq!(general_engine.call_count(), 1);
        assert!(engine.cache_id().contains(":script:"));
        assert_eq!(
            run(&engine, "123 !?", Lang::Auto, Lang::ZhHans).unwrap(),
            "123 !?"
        );
    }

    /// 慢卸载不占池锁：卸载进行中查询常驻状态立即返回，请求被卸载中的包会等它卸完再重新加载。
    #[test]
    fn slow_unload_does_not_block_pool() {
        let (engine, _, special_engine) = router(RoutePolicy::default());
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        special_engine.unload_delay_ms.store(400, Ordering::SeqCst);
        std::thread::scope(|scope| {
            let evictor = scope.spawn(|| run(&engine, "b", Lang::Ja, Lang::ZhHans));
            let started = Instant::now();
            while !special_engine.unloading_now.load(Ordering::SeqCst) {
                assert!(started.elapsed() < Duration::from_secs(2), "卸载没有开始");
                std::thread::sleep(Duration::from_millis(5));
            }
            let probe = Instant::now();
            let resident = engine.resident_ids();
            assert!(
                probe.elapsed() < Duration::from_millis(200),
                "卸载期间查询状态不应被阻塞"
            );
            assert!(resident.contains(&"opus".to_string()), "卸载中仍算常驻");
            // 请求正被卸载的包：等它卸完，再重新加载使用
            assert_eq!(
                run(&engine, "c", Lang::En, Lang::ZhHans).unwrap(),
                "opus:en:c"
            );
            assert_eq!(evictor.join().unwrap().unwrap(), "nllb:ja:b");
        });
        assert_eq!(special_engine.launch_count(), 2, "卸完后重新拉起");
        assert_eq!(special_engine.unloaded_while_busy.load(Ordering::SeqCst), 0);
    }

    /// 关机后：请求明确报错且不会重新拉起 worker；各包都被卸载。
    #[test]
    fn shutdown_rejects_new_requests() {
        let (engine, general_engine, special_engine) = router(mixed_policy(2));
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        let launches = engine.launch_total();
        engine.shutdown();
        assert!(!special_engine.is_resident() && !general_engine.is_resident());
        for mode in [RouteMode::MixedSplit, RouteMode::SpecializedFirst] {
            engine.set_policy(RoutePolicy {
                mode,
                ..mixed_policy(2)
            });
            assert!(matches!(
                run(&engine, "b", Lang::En, Lang::ZhHans),
                Err(TranslateError::WorkerUnavailable(_))
            ));
        }
        assert_eq!(engine.launch_total(), launches, "关机后不得再拉起 worker");
    }

    /// 关机卸载有界：某个包卸载很慢时，shutdown 在时限内返回。
    #[test]
    fn shutdown_is_bounded() {
        let (engine, general_engine, _) = router(RoutePolicy::default());
        let engine = engine.with_shutdown_timeout(Duration::from_millis(100));
        general_engine.unload_delay_ms.store(3000, Ordering::SeqCst);
        run(&engine, "b", Lang::Ja, Lang::ZhHans).unwrap();
        let started = Instant::now();
        engine.shutdown();
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "shutdown 不应被慢卸载拖住"
        );
    }

    /// 调小常驻上限立即生效：空闲的最久未用包马上被卸载，不必等下一次请求。
    #[test]
    fn shrinking_max_resident_evicts_immediately() {
        let (engine, general_engine, special_engine) = router(RoutePolicy {
            max_resident: 2,
            ..RoutePolicy::default()
        });
        run(&engine, "a", Lang::En, Lang::ZhHans).unwrap();
        run(&engine, "b", Lang::Ja, Lang::ZhHans).unwrap();
        assert_eq!(engine.resident_ids(), ["nllb", "opus"]);
        engine.set_policy(RoutePolicy {
            max_resident: 1,
            ..RoutePolicy::default()
        });
        assert_eq!(engine.resident_ids(), ["nllb"], "最久未用的专用包被卸载");
        assert_eq!(special_engine.unloads.load(Ordering::SeqCst), 1);
        assert_eq!(general_engine.unloads.load(Ordering::SeqCst), 0);
    }

    /// 混合拆分：片段的前导 / 尾部空白原样保留。
    #[test]
    fn mixed_split_preserves_edge_whitespace() {
        let text = "  Hello there \n";
        let (engine, _, _) = mixed_router(1, &[(text, &[("  Hello there \n", Some(Lang::En))])]);
        assert_eq!(
            run(&engine, text, Lang::Auto, Lang::ZhHans).unwrap(),
            "  opus:en:Hello there \n"
        );
    }
}
