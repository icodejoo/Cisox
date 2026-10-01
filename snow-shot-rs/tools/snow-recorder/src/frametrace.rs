//! 帧级数据链路追踪（诊断用，默认关闭）：记录每个捕获序号在各阶段的时刻，结束时一次性写成 CSV。
//!
//! 开关：环境变量 [`ENV_FRAME_TRACE`] 指向输出 CSV 路径；未设置时整个模块不分配、不读时钟、不做 I/O，
//! 每个追踪点只是对 `Option` 的一次分支判断。
//! 开启时：各线程只往自己预分配的缓冲追加定长记录（超出容量只计数、不扩容），线程结束时并入共享列表，
//! 录制停止且各线程都退出后由 [`finish`] 一次性排序写盘。
//!
//! 时钟：记录里只存相对 `origin` 的单调纳秒（`Instant`，Windows 上即 QPC，与 DXGI 的 `LastPresentTime` 同源）；
//! 挂钟 `unix_us = origin_unix_us + 单调纳秒 / 1000`，`origin` 与 `origin_unix_us` 在初始化时背靠背读取一次。
//! 误差预期：配对误差为微秒级（两次读取的间隔加 `SystemTime` 精度）；录制期间挂钟被 NTP 微调的漂移上限约
//! 500ppm（10 秒内 ≤5ms，常态远小于 0.1ms）；DXGI 呈现时刻经 QPC 锚点换算，误差同为微秒级。
//! 与夹具 `frames.csv` 的 `unix_us` 对齐时，另有一项**语义偏差**：夹具记录的是 `Present` 返回时刻，不是画面真正
//! 上屏的时刻，两者相差一个近似常数（数毫秒到两个 vsync），由联接脚本用幸存帧自标定扣除。

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 环境变量：非空时开启帧追踪，值为输出 CSV 路径（仅覆盖自建 GPU 流水线）。
pub const ENV_FRAME_TRACE: &str = "SNOW_RECORDER_FRAME_TRACE";
/// 采集线程缓冲容量（记录条数）。
const CAPACITY_CAPTURE: usize = 32_768;
/// 合成线程缓冲容量（记录条数）。
const CAPACITY_COMPOSE: usize = 32_768;
/// 编码线程缓冲容量（记录条数）。
const CAPACITY_ENCODE: usize = 8_192;
/// 外来线程（如 MFT 释放样本的线程）共享缓冲容量（记录条数）。
const CAPACITY_SHARED: usize = 16_384;
/// 表示"无时刻/无槽号"的哨兵值。
pub const NONE: i64 = i64::MIN;
/// CSV 表头。
pub const CSV_HEADER: &str = "mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code";

/// 事件码与取值约定（CSV 的 `code` 列）。
pub mod code {
    /// `acquire`：取到新桌面内容并已复制进共享槽。
    pub const ACQ_FRESH: u8 = 0;
    /// `acquire`：只有光标移动（`LastPresentTime == 0`）。
    pub const ACQ_CURSOR_ONLY: u8 = 1;
    /// `acquire`：采集池耗尽，帧在采集阶段被丢弃。
    pub const ACQ_POOL_DROP: u8 = 2;
    /// `acquire`：暂停期间只取走不复制。
    pub const ACQ_UNWANTED: u8 = 3;
    /// `discard`：被更新的帧取代（槽选择时丢弃）。
    pub const DISCARD_SUPERSEDED: u8 = 1;
    /// `discard`：待输出队列溢出，丢最旧。
    pub const DISCARD_OVERFLOW: u8 = 2;
    /// `slot`：该槽没有可输出的帧。
    pub const SLOT_EMPTY: u8 = 0;
    /// `slot`：取队列里的新帧。
    pub const SLOT_NEW: u8 = 1;
    /// `slot`：只因光标移动，复用最近一张桌面帧（成品里表现为重复序号）。
    pub const SLOT_CURSOR_REUSE: u8 = 2;
    /// `slot`：编码表面池耗尽，帧被丢弃。
    pub const SLOT_SURFACE_EXHAUSTED: u8 = 3;
    /// `slot`：启动探测帧（槽 0）。
    pub const SLOT_PROBE: u8 = 4;
}

/// 追踪事件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// DDA `AcquireNextFrame` 成功。
    Acquire,
    /// 一次 `next()` 调用因超时没有新帧。
    Idle,
    /// 桌面复制权限丢失。
    Lost,
    /// 采集线程把帧交给合成线程。
    Enqueue,
    /// 合成线程收到帧并放入待输出队列。
    Absorb,
    /// 帧被丢弃（槽选择取代或队列溢出）。
    Discard,
    /// 时间槽选择结果。
    Slot,
    /// 因落后被跳过的槽。
    Skip,
    /// 一次合成结束。
    Compose,
    /// 合成线程把表面送往编码线程。
    Send,
    /// 编码线程 `submit` 返回。
    Submit,
    /// 编码器释放样本（输入已被消费，仅 Media Foundation 路径）。
    Consumed,
    /// 编码器收尾结束。
    Finish,
}

impl Event {
    /// CSV 里的事件名。
    pub fn name(self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Idle => "idle",
            Self::Lost => "lost",
            Self::Enqueue => "enqueue",
            Self::Absorb => "absorb",
            Self::Discard => "discard",
            Self::Slot => "slot",
            Self::Skip => "skip",
            Self::Compose => "compose",
            Self::Send => "send",
            Self::Submit => "submit",
            Self::Consumed => "consumed",
            Self::Finish => "finish",
        }
    }
}

/// 记录来源线程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thread {
    /// 采集线程。
    Capture,
    /// 合成线程（含启动探测）。
    Compose,
    /// 编码线程。
    Encode,
    /// 编码器内部线程（MFT 释放样本）。
    Mf,
}

impl Thread {
    /// CSV 里的线程名。
    pub fn name(self) -> &'static str {
        match self {
            Self::Capture => "cap",
            Self::Compose => "cmp",
            Self::Encode => "enc",
            Self::Mf => "mf",
        }
    }

    /// 该线程缓冲的容量。
    fn capacity(self) -> usize {
        match self {
            Self::Capture => CAPACITY_CAPTURE,
            Self::Compose => CAPACITY_COMPOSE,
            Self::Encode => CAPACITY_ENCODE,
            Self::Mf => CAPACITY_SHARED,
        }
    }
}

/// 一条定长追踪记录。
#[derive(Debug, Clone, Copy)]
pub struct Rec {
    /// 事件时刻（相对 `origin` 的单调纳秒）。
    pub t_ns: i64,
    /// 呈现时刻（相对 `origin` 的单调纳秒，仅 `acquire`），无则 [`NONE`]。
    pub present_ns: i64,
    /// 捕获序号（录制进程自己的递增 id，0 表示无）。
    pub cap_id: u64,
    /// 槽号，无则 [`NONE`]。
    pub slot: i64,
    /// 计数：`acquire` 为 `AccumulatedFrames`，`skip` 为被跳过的槽数。
    pub n: u32,
    /// 耗时（微秒）：`idle` 为等待时长，`compose`/`submit`/`consumed`/`finish` 为各自耗时。
    pub dur_us: u32,
    /// 事件种类。
    pub event: Event,
    /// 来源线程。
    pub thread: Thread,
    /// 来源输出序号（跨屏时区分显示器；单屏为 0；`idle` 在跨屏下为 255 表示全部输出）。
    pub src: u8,
    /// 事件相关的取值码，见 [`code`]。
    pub code: u8,
}

impl Rec {
    /// 创建只带时刻的空记录。
    fn new(event: Event, thread: Thread, t_ns: i64) -> Self {
        Self { t_ns, present_ns: NONE, cap_id: 0, slot: NONE, n: 0, dur_us: 0, event, thread, src: 0, code: 0 }
    }
}

/// 耗时转微秒（饱和到 `u32`）。
fn micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

/// 追踪器：持有时钟原点、输出路径与合并后的记录；全进程至多一个。
pub struct Tracer {
    /// 输出 CSV 路径。
    path: PathBuf,
    /// 单调时钟原点。
    origin: Instant,
    /// 原点对应的挂钟（unix 微秒）。
    origin_unix_us: i64,
    /// 外来线程直接追加的记录（有界，不扩容）。
    shared: Mutex<Vec<Rec>>,
    /// 各线程缓冲并入后的记录。
    merged: Mutex<Vec<Rec>>,
    /// 因容量不足被丢弃的记录数。
    overflow: AtomicU64,
}

impl Tracer {
    /// 创建追踪器并背靠背读取单调时钟与挂钟作为原点。
    ///
    /// # 参数
    /// - `path`：输出 CSV 路径。
    pub fn new(path: PathBuf) -> Self {
        let origin = Instant::now();
        let origin_unix_us = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
        Self { path, origin, origin_unix_us, shared: Mutex::new(Vec::with_capacity(CAPACITY_SHARED)), merged: Mutex::new(Vec::new()), overflow: AtomicU64::new(0) }
    }

    /// 当前时刻相对原点的单调纳秒。
    pub fn now_ns(&self) -> i64 {
        self.ns_of(Instant::now())
    }

    /// 任意 `Instant` 相对原点的纳秒（早于原点为负）。
    ///
    /// # 参数
    /// - `at`：时刻。
    pub fn ns_of(&self, at: Instant) -> i64 {
        match at.checked_duration_since(self.origin) {
            Some(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            None => -i64::try_from(self.origin.saturating_duration_since(at).as_nanos()).unwrap_or(i64::MAX),
        }
    }

    /// 单调纳秒换算成挂钟 unix 微秒。
    ///
    /// # 参数
    /// - `ns`：相对原点的纳秒。
    pub fn unix_us(&self, ns: i64) -> i64 {
        self.origin_unix_us + ns.div_euclid(1000)
    }

    /// 外来线程追加"样本被编码器释放"记录（有界，满了只计数）。
    ///
    /// # 参数
    /// - `slot`：样本的槽号。
    /// - `sent`：送帧时刻（耗时 = 现在 - 送帧）。
    pub fn push_consumed(&self, slot: i64, sent: Instant) {
        let mut rec = Rec::new(Event::Consumed, Thread::Mf, self.now_ns());
        rec.slot = slot;
        rec.dur_us = micros(sent.elapsed());
        match self.shared.lock() {
            Ok(mut v) if v.len() < CAPACITY_SHARED => v.push(rec),
            _ => {
                self.overflow.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 并入一个线程缓冲。
    fn merge(&self, buf: Vec<Rec>) {
        if let Ok(mut v) = self.merged.lock() {
            v.extend(buf);
        }
    }

    /// 清空已记录内容（装配回落重试时丢弃上一次尝试的残留；原点不变）。
    pub fn reset(&self) {
        if let Ok(mut v) = self.merged.lock() {
            v.clear();
        }
        if let Ok(mut v) = self.shared.lock() {
            v.clear();
        }
        self.overflow.store(0, Ordering::Relaxed);
    }

    /// 取走全部记录并按时刻稳定排序（写盘前调用；测试也用它读回记录）。
    pub fn collect(&self) -> Vec<Rec> {
        let mut all: Vec<Rec> = self.merged.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        if let Ok(mut v) = self.shared.lock() {
            all.append(&mut v);
        }
        all.sort_by_key(|r| r.t_ns);
        all
    }

    /// 把记录渲染成 CSV 文本（`#` 开头的是元数据行）。
    ///
    /// # 参数
    /// - `recs`：已排序的记录。
    /// - `fps`：输出帧率。
    /// - `backend`：后端名。
    pub fn render(&self, recs: &[Rec], fps: u32, backend: &str) -> String {
        let mut out = String::with_capacity(recs.len() * 64 + 256);
        let _ = writeln!(out, "# snow-recorder frame trace v1");
        let _ = writeln!(out, "# origin_unix_us={}", self.origin_unix_us);
        let _ = writeln!(out, "# fps={fps}");
        let _ = writeln!(out, "# backend={backend}");
        let _ = writeln!(out, "# records={}", recs.len());
        let _ = writeln!(out, "# overflow={}", self.overflow.load(Ordering::Relaxed));
        let _ = writeln!(out, "{CSV_HEADER}");
        for r in recs {
            let present = if r.present_ns == NONE { String::new() } else { self.unix_us(r.present_ns).to_string() };
            let slot = if r.slot == NONE { String::new() } else { r.slot.to_string() };
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{}",
                r.t_ns,
                self.unix_us(r.t_ns),
                r.event.name(),
                r.thread.name(),
                r.src,
                r.cap_id,
                slot,
                present,
                r.n,
                r.dur_us,
                r.code
            );
        }
        out
    }

    /// 汇总、排序并写盘；失败只打印原因，不影响录制结果。
    ///
    /// # 参数
    /// - `fps`：输出帧率。
    /// - `backend`：后端名。
    pub fn write(&self, fps: u32, backend: &str) {
        let recs = self.collect();
        let text = self.render(&recs, fps, backend);
        match std::fs::write(&self.path, text) {
            Ok(()) => eprintln!("帧追踪已写出: {} ({} 条, 缓冲溢出 {})", self.path.display(), recs.len(), self.overflow.load(Ordering::Relaxed)),
            Err(e) => eprintln!("帧追踪写出失败: {}: {e}", self.path.display()),
        }
    }
}

/// 全进程唯一的追踪器（首次访问时按环境变量决定是否启用）。
static TRACER: OnceLock<Option<Arc<Tracer>>> = OnceLock::new();

/// 由环境变量取值构造追踪器；空值视为未开启。
fn tracer_from_env(value: Option<OsString>) -> Option<Arc<Tracer>> {
    value.filter(|v| !v.is_empty()).map(|v| Arc::new(Tracer::new(PathBuf::from(v))))
}

/// 取全局追踪器；未设置 [`ENV_FRAME_TRACE`] 返回 `None`。只在构造阶段调用，不在热路径。
///
/// # 示例
/// ```ignore
/// let buf = TraceBuf::attach(global().cloned(), Thread::Capture);
/// ```
pub fn global() -> Option<&'static Arc<Tracer>> {
    TRACER.get_or_init(|| tracer_from_env(std::env::var_os(ENV_FRAME_TRACE))).as_ref()
}

/// 装配重试前清掉上一次尝试的残留记录（未开启时什么也不做）。
pub fn reset() {
    if let Some(t) = global() {
        t.reset();
    }
}

/// 录制结束且各线程都已退出后写出追踪文件（未开启时什么也不做）。
///
/// # 参数
/// - `fps`：输出帧率。
/// - `backend`：后端名。
pub fn finish(fps: u32, backend: &str) {
    if let Some(t) = global() {
        t.write(fps, backend);
    }
}

/// 缓冲内部状态（仅在开启时存在）。
struct Inner {
    /// 追踪器。
    tracer: Arc<Tracer>,
    /// 所属线程。
    thread: Thread,
    /// 线程私有缓冲。
    buf: Vec<Rec>,
    /// 缓冲容量上限。
    limit: usize,
}

impl Inner {
    /// 追加一条记录；缓冲已满只计数。
    fn push(&mut self, rec: Rec) {
        if self.buf.len() < self.limit {
            self.buf.push(rec);
        } else {
            self.tracer.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 线程私有的追踪缓冲：关闭时是一个 `None`，所有追踪点都是一次分支判断；丢弃时并入全局记录。
pub struct TraceBuf {
    /// 开启时的内部状态。
    inner: Option<Inner>,
}

impl TraceBuf {
    /// 绑定全局追踪器；未开启追踪时返回关闭状态的缓冲。
    ///
    /// # 参数
    /// - `thread`：所属线程。
    pub fn new(thread: Thread) -> Self {
        Self::attach(global().cloned(), thread)
    }

    /// 绑定指定追踪器（`None` 为关闭）。
    ///
    /// # 参数
    /// - `tracer`：追踪器。
    /// - `thread`：所属线程。
    pub fn attach(tracer: Option<Arc<Tracer>>, thread: Thread) -> Self {
        Self::attach_limited(tracer, thread, thread.capacity())
    }

    /// 绑定指定追踪器并指定缓冲容量（测试用，便于构造溢出）。
    ///
    /// # 参数
    /// - `tracer`：追踪器。
    /// - `thread`：所属线程。
    /// - `limit`：缓冲容量（记录条数）。
    pub fn attach_limited(tracer: Option<Arc<Tracer>>, thread: Thread, limit: usize) -> Self {
        Self { inner: tracer.map(|tracer| Inner { tracer, thread, buf: Vec::with_capacity(limit), limit }) }
    }

    /// 是否开启。
    #[inline]
    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// 开启时返回当前时刻（用于测耗时的起点），关闭时返回 `None`，不读时钟。
    #[inline]
    pub fn mark(&self) -> Option<Instant> {
        self.inner.as_ref().map(|_| Instant::now())
    }

    /// 追加一条记录；`at` 为事件时刻（缺省取当前），`fill` 填充其余字段。
    #[inline]
    fn emit(&mut self, event: Event, at: Option<Instant>, fill: impl FnOnce(&Tracer, &mut Rec)) {
        let Some(inner) = &mut self.inner else { return };
        let t_ns = at.map_or_else(|| inner.tracer.now_ns(), |t| inner.tracer.ns_of(t));
        let mut rec = Rec::new(event, inner.thread, t_ns);
        fill(&inner.tracer, &mut rec);
        inner.push(rec);
    }

    /// 记录一次 DDA 取帧成功。
    ///
    /// # 参数
    /// - `src`：来源输出序号。
    /// - `at`：取帧时刻。
    /// - `present`：DXGI `LastPresentTime` 换算成的时刻（没有则 `None`）。
    /// - `accumulated`：`AccumulatedFrames`。
    /// - `cap_id`：分配的捕获序号（没有产出帧为 0）。
    /// - `outcome`：`code::ACQ_*`。
    #[inline]
    pub fn acquire(&mut self, src: u8, at: Instant, present: Option<Instant>, accumulated: u32, cap_id: u64, outcome: u8) {
        self.emit(Event::Acquire, Some(at), |t, r| {
            r.src = src;
            r.present_ns = present.map_or(NONE, |p| t.ns_of(p));
            r.n = accumulated;
            r.cap_id = cap_id;
            r.code = outcome;
        });
    }

    /// 记录一次因超时没有新帧。
    ///
    /// # 参数
    /// - `src`：来源输出序号。
    /// - `waited`：本次等待时长。
    #[inline]
    pub fn idle(&mut self, src: u8, waited: Duration) {
        self.emit(Event::Idle, None, |_, r| {
            r.src = src;
            r.dur_us = micros(waited);
        });
    }

    /// 记录桌面复制权限丢失。
    ///
    /// # 参数
    /// - `src`：来源输出序号。
    #[inline]
    pub fn lost(&mut self, src: u8) {
        self.emit(Event::Lost, None, |_, r| r.src = src);
    }

    /// 记录采集线程把帧交给合成线程。
    ///
    /// # 参数
    /// - `cap_id`：捕获序号。
    /// - `fresh`：是否带新桌面内容。
    #[inline]
    pub fn enqueue(&mut self, cap_id: u64, fresh: bool) {
        self.emit(Event::Enqueue, None, |_, r| {
            r.cap_id = cap_id;
            r.code = u8::from(fresh);
        });
    }

    /// 记录合成线程收到帧并放入待输出队列。
    ///
    /// # 参数
    /// - `cap_id`：捕获序号。
    /// - `queued`：是否进入了队列。
    #[inline]
    pub fn absorb(&mut self, cap_id: u64, queued: bool) {
        self.emit(Event::Absorb, None, |_, r| {
            r.cap_id = cap_id;
            r.code = u8::from(queued);
        });
    }

    /// 记录一帧被丢弃。
    ///
    /// # 参数
    /// - `cap_id`：被丢弃的捕获序号。
    /// - `slot`：触发丢弃的槽号（队列溢出时为 `None`）。
    /// - `reason`：`code::DISCARD_*`。
    #[inline]
    pub fn discard(&mut self, cap_id: u64, slot: Option<u64>, reason: u8) {
        self.emit(Event::Discard, None, |_, r| {
            r.cap_id = cap_id;
            r.slot = slot.map_or(NONE, slot_i64);
            r.code = reason;
        });
    }

    /// 记录时间槽选择结果。
    ///
    /// # 参数
    /// - `slot`：槽号。
    /// - `cap_id`：选中的捕获序号（没有为 0）。
    /// - `kind`：`code::SLOT_*`。
    #[inline]
    pub fn slot(&mut self, slot: u64, cap_id: u64, kind: u8) {
        self.emit(Event::Slot, None, |_, r| {
            r.slot = slot_i64(slot);
            r.cap_id = cap_id;
            r.code = kind;
        });
    }

    /// 记录因落后被跳过的槽。
    ///
    /// # 参数
    /// - `first_slot`：第一个被跳过的槽号。
    /// - `count`：被跳过的槽数。
    #[inline]
    pub fn skip(&mut self, first_slot: u64, count: u64) {
        self.emit(Event::Skip, None, |_, r| {
            r.slot = slot_i64(first_slot);
            r.n = u32::try_from(count).unwrap_or(u32::MAX);
        });
    }

    /// 记录一次合成结束（耗时 = 现在 - `started`）。
    ///
    /// # 参数
    /// - `slot`：槽号。
    /// - `cap_id`：捕获序号。
    /// - `started`：合成开始时刻。
    #[inline]
    pub fn compose(&mut self, slot: u64, cap_id: u64, started: Instant) {
        self.emit(Event::Compose, None, |_, r| {
            r.slot = slot_i64(slot);
            r.cap_id = cap_id;
            r.dur_us = micros(started.elapsed());
        });
    }

    /// 记录合成线程把表面送往编码线程。
    ///
    /// # 参数
    /// - `slot`：槽号。
    #[inline]
    pub fn send(&mut self, slot: u64) {
        self.emit(Event::Send, None, |_, r| r.slot = slot_i64(slot));
    }

    /// 记录编码线程 `submit` 返回。
    ///
    /// # 参数
    /// - `slot`：送帧的 pts（槽号）。
    /// - `started`：调用前由 [`TraceBuf::mark`] 取得的起点。
    #[inline]
    pub fn submit(&mut self, slot: i64, started: Option<Instant>) {
        self.emit(Event::Submit, None, |_, r| {
            r.slot = slot;
            r.dur_us = started.map_or(0, |s| micros(s.elapsed()));
        });
    }

    /// 记录编码器收尾结束。
    ///
    /// # 参数
    /// - `end`：排他终点槽号。
    /// - `started`：调用前由 [`TraceBuf::mark`] 取得的起点。
    #[inline]
    pub fn finish(&mut self, end: i64, started: Option<Instant>) {
        self.emit(Event::Finish, None, |_, r| {
            r.slot = end;
            r.dur_us = started.map_or(0, |s| micros(s.elapsed()));
        });
    }
}

/// 槽号转 `i64`（饱和）。
fn slot_i64(slot: u64) -> i64 {
    i64::try_from(slot).unwrap_or(i64::MAX)
}

impl Drop for TraceBuf {
    /// 线程结束时把私有缓冲并入全局记录。
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.tracer.merge(inner.buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个不落盘的追踪器。
    fn tracer() -> Arc<Tracer> {
        Arc::new(Tracer::new(PathBuf::from("unused-trace.csv")))
    }

    /// 关闭状态：所有追踪点空操作，`mark` 不读时钟。
    #[test]
    fn off_buffer_records_nothing() {
        let mut buf = TraceBuf::attach(None, Thread::Capture);
        assert!(!buf.enabled() && buf.mark().is_none());
        buf.acquire(0, Instant::now(), None, 1, 1, code::ACQ_FRESH);
        buf.slot(1, 1, code::SLOT_NEW);
        buf.submit(1, buf.mark());
    }

    /// 环境变量取值：空与未设置都不开启，非空取为路径。
    #[test]
    fn env_value_enables_only_when_non_empty() {
        assert!(tracer_from_env(None).is_none());
        assert!(tracer_from_env(Some(OsString::new())).is_none());
        assert!(tracer_from_env(Some(OsString::from("t.csv"))).is_some());
    }

    /// 记录经缓冲并入后按时刻排序，CSV 列数与元数据行正确，未设置的字段留空。
    #[test]
    fn render_sorts_and_formats_columns() {
        let t = tracer();
        {
            let mut cap = TraceBuf::attach(Some(Arc::clone(&t)), Thread::Capture);
            let mut cmp = TraceBuf::attach(Some(Arc::clone(&t)), Thread::Compose);
            let now = Instant::now();
            cmp.slot(5, 7, code::SLOT_NEW);
            cap.acquire(1, now, Some(now), 2, 7, code::ACQ_FRESH);
            cap.enqueue(7, true);
        }
        let recs = t.collect();
        assert_eq!(recs.len(), 3);
        assert!(recs.windows(2).all(|w| w[0].t_ns <= w[1].t_ns));
        let text = t.render(&recs, 60, "mock");
        assert!(text.contains("# fps=60") && text.contains("# backend=mock") && text.contains("# records=3") && text.contains("# overflow=0"));
        let rows: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("mono_ns")).skip(1).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.split(',').count() == CSV_HEADER.split(',').count()));
        let acquire = rows.iter().find(|r| r.contains(",acquire,")).unwrap();
        let cells: Vec<&str> = acquire.split(',').collect();
        assert_eq!((cells[4], cells[5], cells[6], cells[8], cells[10]), ("1", "7", "", "2", "0"));
        assert!(!cells[7].is_empty(), "acquire 应带 present_unix_us");
        let slot_row = rows.iter().find(|r| r.contains(",slot,")).unwrap();
        assert_eq!(slot_row.split(',').nth(6), Some("5"));
        assert_eq!(slot_row.split(',').nth(7), Some(""));
    }

    /// 缓冲写满后只计数不扩容。
    #[test]
    fn overflow_counts_instead_of_growing() {
        let t = tracer();
        {
            let mut buf = TraceBuf::attach_limited(Some(Arc::clone(&t)), Thread::Compose, 2);
            for slot in 0..5 {
                buf.send(slot);
            }
        }
        let recs = t.collect();
        assert_eq!(recs.len(), 2);
        assert!(t.render(&recs, 30, "x").contains("# overflow=3"));
    }

    /// 单调纳秒与挂钟换算；早于原点的时刻为负数。
    #[test]
    fn clock_conversion_is_consistent() {
        let t = tracer();
        assert_eq!(t.unix_us(2_500_000), t.origin_unix_us + 2500);
        let earlier = t.origin.checked_sub(Duration::from_millis(3)).unwrap();
        assert_eq!(t.ns_of(earlier), -3_000_000);
        assert_eq!(t.unix_us(t.ns_of(earlier)), t.origin_unix_us - 3000);
        assert!(t.now_ns() >= 0);
    }

    /// 外来线程追加的记录并入输出，满了只计数。
    #[test]
    fn shared_pushes_are_collected() {
        let t = tracer();
        t.push_consumed(9, Instant::now());
        let recs = t.collect();
        assert_eq!((recs.len(), recs[0].event, recs[0].slot), (1, Event::Consumed, 9));
    }

    /// `reset` 清掉上一次尝试的残留。
    #[test]
    fn reset_clears_previous_attempt() {
        let t = tracer();
        {
            let mut buf = TraceBuf::attach(Some(Arc::clone(&t)), Thread::Capture);
            buf.lost(0);
        }
        t.push_consumed(1, Instant::now());
        t.reset();
        assert!(t.collect().is_empty());
    }
}
