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
//!
//! # `slow_iter` 事件（采集环路慢迭代）
//! 用来区分"采集线程被晚调度"与"调用本身被卡住"。采集环路的一次迭代 = 取帧轮询的一圈：从 `AcquireNextFrame` 前
//! 开始，到睡眠前（轮询无新帧）或 `next()` 返回前（取到帧/超时/出错）结束。某次迭代的"间隔"或"总耗时"
//! 达到阈值（默认 4ms，环境变量 `SNOW_RECORDER_SLOW_ITER_US` 调整）才记一条，正常迭代只做廉价计时累加。
//! 列约定（`slow_iter` 行；其余事件的这六列留空）：
//! - `mono_ns`/`unix_us`：**迭代开始**（睡醒、准备取帧）的时刻；`src`：单屏 0，跨屏 255（一圈轮询全部输出）；
//! - `dur_us`：迭代总耗时（开始到结束）；`n`、`code`：0；
//! - `sleep_req_us`：上一圈迭代末尾请求的睡眠时长（上一圈不是以睡眠结束则为 0）；
//! - `sleep_over_us`：睡眠超额 = 间隔 - 请求时长（线程被晚唤醒/晚调度；`sleep_req_us` 为 0 时为 0）；
//! - `acq_us`：本圈 `AcquireNextFrame` 调用耗时（调用被卡住）；
//! - `lock_us`：本圈等设备锁耗时（复制与 Flush 里的 `Enter`）；
//! - `copy_us`：本圈复制 + Flush 总耗时（含 `lock_us`）；
//! - `gap_us`：间隔 = 上一圈结束到本圈开始。`sleep_req_us` 为 0 时它是"线程在两次调用之间/调用方里"的时间。
//!
//! # `poll` / `release` 事件（逐调用追踪）
//! 开关：环境变量 [`ENV_POLL_TRACE`] 设为 `1`，且 [`ENV_FRAME_TRACE`] 也已设置才有效；未开启时每个调用点只是一次分支。
//! 开启时采集线程另有一块预分配的专用缓冲（[`CAPACITY_POLL`] 条，约 8 秒 x 2500 次/秒），写满只计数，
//! 文件头多两行 `# poll_trace=1`、`# poll_overflow=<丢弃条数>`；追加时不做 I/O、不分配、不额外读时钟
//! （起点与耗时复用取帧环路本来就测的值）。未开启时输出文件与之前完全一致。
//!
//! `poll`：**每一次** `AcquireNextFrame` 调用一行。通用列：
//! - `mono_ns`/`unix_us`：调用开始时刻；`src`：输出序号（单屏 0）；`dur_us`：调用耗时；
//! - `code`：结果，0 = 取到帧，1 = 超时（`DXGI_ERROR_WAIT_TIMEOUT`），2 = 权限丢失（需重建），3 = 其他错误；
//! - `present_unix_us`：成功且 `LastPresentTime != 0` 时的呈现挂钟，否则留空；`n`：成功时的 `AccumulatedFrames`，否则 0；
//! - `cap_id`、`slot`：留空/0。
//!
//! `poll` 复用六个扩展列（列名沿用 `slow_iter` 的，含义如下；无值留空）：
//! - 第 1 列（`sleep_req_us`）= `flags` 位集：bit0 `LastMouseUpdateTime != 0`，bit1 `LastPresentTime != 0`，
//!   bit2 `RectsCoalesced`，bit3 `ProtectedContentMaskedOut`；仅成功时有值；
//! - 第 2 列（`sleep_over_us`）= `PointerShapeBufferSize`，第 3 列（`acq_us`）= `TotalMetadataBufferSize`（仅成功时有值）；
//! - 第 4 列（`lock_us`）= 上一次 `ReleaseFrame` 结束到本次调用开始的间隔（微秒，此前没有释放过则留空）；
//! - 第 5、6 列留空。
//!
//! `release`：每次 `ReleaseFrame` 一行（只记最常见的"复制后释放/光标帧释放/不要的帧释放/池耗尽释放"路径；
//! 出错提前返回时隐式释放的不记）。`mono_ns`/`unix_us`：调用开始；`dur_us`：耗时；`src`：输出序号；其余留空。

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::settings::{DEFAULT_SLOW_ITER_US, ENV_SLOW_ITER_US, parse_slow_iter_us};

/// 环境变量：非空时开启帧追踪，值为输出 CSV 路径（仅覆盖自建 GPU 流水线）。
pub const ENV_FRAME_TRACE: &str = "SNOW_RECORDER_FRAME_TRACE";
/// 逐调用追踪的环境变量：设为 `1` 且已设置 [`ENV_FRAME_TRACE`] 时记录每次 `AcquireNextFrame`（见模块文档）。
pub const ENV_POLL_TRACE: &str = "SNOW_RECORDER_POLL_TRACE";
/// 逐调用追踪缓冲容量（记录条数）：约 8 秒 x 2500 次/秒的轮询加释放事件。
pub const CAPACITY_POLL: usize = 24_576;
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
pub const CSV_HEADER: &str = "mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code,sleep_req_us,sleep_over_us,acq_us,lock_us,copy_us,gap_us";

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
    /// `poll`：取到帧。
    pub const POLL_OK: u8 = 0;
    /// `poll`：超时没有新帧。
    pub const POLL_TIMEOUT: u8 = 1;
    /// `poll`：桌面复制权限丢失。
    pub const POLL_LOST: u8 = 2;
    /// `poll`：其他错误。
    pub const POLL_ERROR: u8 = 3;
}

/// `poll` 事件第 1 扩展列 `flags` 的位。
pub mod poll_flag {
    /// `LastMouseUpdateTime != 0`。
    pub const MOUSE_UPDATE: u32 = 1;
    /// `LastPresentTime != 0`（带桌面内容更新）。
    pub const PRESENT: u32 = 2;
    /// `RectsCoalesced`。
    pub const RECTS_COALESCED: u32 = 4;
    /// `ProtectedContentMaskedOut`。
    pub const PROTECTED_MASKED: u32 = 8;
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
    /// 采集环路慢迭代（列约定见模块文档）。
    SlowIter,
    /// 一次 `AcquireNextFrame` 调用（逐调用追踪，列约定见模块文档）。
    Poll,
    /// 一次 `ReleaseFrame` 调用（逐调用追踪）。
    Release,
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
            Self::SlowIter => "slow_iter",
            Self::Poll => "poll",
            Self::Release => "release",
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
    /// 仅 `slow_iter` 用的六个微秒数：`sleep_req, sleep_over, acq, lock, copy, gap`（顺序同 CSV 列）。
    pub x: [u32; 6],
}

impl Rec {
    /// 创建只带时刻的空记录。
    fn new(event: Event, thread: Thread, t_ns: i64) -> Self {
        Self { t_ns, present_ns: NONE, cap_id: 0, slot: NONE, n: 0, dur_us: 0, event, thread, src: 0, code: 0, x: [0; 6] }
    }
}

/// `poll` 记录里"无值"的哨兵（渲染成空单元格）。
const NA: u32 = u32::MAX;

/// 一次成功取帧的 `DXGI_OUTDUPL_FRAME_INFO` 摘要（由调用方从帧信息读出，零成本字段）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PollSample {
    /// `AccumulatedFrames`。
    pub accumulated: u32,
    /// `LastPresentTime` 换算的时刻（为 0 或无锚点时 `None`）。
    pub present: Option<Instant>,
    /// 标志位，见 [`poll_flag`]。
    pub flags: u32,
    /// `PointerShapeBufferSize`。
    pub pointer_bytes: u32,
    /// `TotalMetadataBufferSize`。
    pub meta_bytes: u32,
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
    /// 附加的元数据行（键唯一，后写覆盖先写），渲染成 `# 键=值`。
    notes: Mutex<Vec<(String, String)>>,
    /// 采集环路慢迭代阈值（微秒）。
    slow_iter_us: u32,
    /// 是否开启逐调用追踪（`poll`/`release`）。
    poll: bool,
    /// 逐调用缓冲因容量不足被丢弃的记录数。
    poll_overflow: AtomicU64,
}

impl Tracer {
    /// 创建追踪器并背靠背读取单调时钟与挂钟作为原点。
    ///
    /// # 参数
    /// - `path`：输出 CSV 路径。
    pub fn new(path: PathBuf) -> Self {
        let origin = Instant::now();
        let origin_unix_us = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
        Self { path, origin, origin_unix_us, shared: Mutex::new(Vec::with_capacity(CAPACITY_SHARED)), merged: Mutex::new(Vec::new()), overflow: AtomicU64::new(0), notes: Mutex::new(Vec::new()), slow_iter_us: DEFAULT_SLOW_ITER_US, poll: false, poll_overflow: AtomicU64::new(0) }
    }

    /// 开关逐调用追踪（`poll`/`release` 事件）。
    ///
    /// # 参数
    /// - `on`：是否开启。
    pub fn with_poll_trace(mut self, on: bool) -> Self {
        self.poll = on;
        self
    }

    /// 指定慢迭代阈值（微秒）。
    ///
    /// # 参数
    /// - `us`：阈值。
    pub fn with_slow_iter_us(mut self, us: u32) -> Self {
        self.slow_iter_us = us;
        self
    }

    /// 写入一条元数据（同名覆盖；值里的换行换成空格）。
    ///
    /// # 参数
    /// - `key`：键。
    /// - `value`：值。
    pub fn set_note(&self, key: &str, value: &str) {
        let value = value.replace(['\r', '\n'], " ");
        if let Ok(mut notes) = self.notes.lock() {
            match notes.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => notes.push((key.to_string(), value)),
            }
        }
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
        self.poll_overflow.store(0, Ordering::Relaxed);
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
        let _ = writeln!(out, "# slow_iter_us={}", self.slow_iter_us);
        if let Ok(notes) = self.notes.lock() {
            for (k, v) in notes.iter() {
                let _ = writeln!(out, "# {k}={v}");
            }
        }
        let _ = writeln!(out, "# records={}", recs.len());
        let _ = writeln!(out, "# overflow={}", self.overflow.load(Ordering::Relaxed));
        if self.poll {
            let _ = writeln!(out, "# poll_trace=1");
            let _ = writeln!(out, "# poll_overflow={}", self.poll_overflow.load(Ordering::Relaxed));
        }
        let _ = writeln!(out, "{CSV_HEADER}");
        for r in recs {
            let present = if r.present_ns == NONE { String::new() } else { self.unix_us(r.present_ns).to_string() };
            let slot = if r.slot == NONE { String::new() } else { r.slot.to_string() };
            let cell = |v: u32| if v == NA { String::new() } else { v.to_string() };
            let ext = match r.event {
                Event::SlowIter => {
                    let [a, b, c, d, e, f] = r.x;
                    format!("{a},{b},{c},{d},{e},{f}")
                }
                Event::Poll => format!("{},{},{},{},,", cell(r.x[0]), cell(r.x[1]), cell(r.x[2]), cell(r.x[3])),
                _ => ",,,,,".to_string(),
            };
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{},{ext}",
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

/// 逐调用追踪开关的取值判断：仅 `1` 开启。
fn poll_trace_requested(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// 由环境变量取值构造追踪器；空值视为未开启。
fn tracer_from_env(value: Option<OsString>) -> Option<Arc<Tracer>> {
    value.filter(|v| !v.is_empty()).map(|v| {
        Arc::new(
            Tracer::new(PathBuf::from(v))
                .with_slow_iter_us(parse_slow_iter_us(std::env::var(ENV_SLOW_ITER_US).ok().as_deref()))
                .with_poll_trace(poll_trace_requested(std::env::var(ENV_POLL_TRACE).ok().as_deref())),
        )
    })
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

/// 写入一条帧追踪文件头元数据（`# 键=值`；未开启时什么也不做，同名覆盖）。
///
/// # 参数
/// - `key`：键。
/// - `value`：值。
pub fn note(key: &str, value: &str) {
    if let Some(t) = global() {
        t.set_note(key, value);
    }
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
    /// 采集环路迭代计时状态。
    iter: IterState,
    /// 慢迭代阈值。
    slow: Duration,
    /// 是否记录逐调用事件（只有采集线程且追踪器开启时为真）。
    poll: bool,
    /// 逐调用专用缓冲（预分配，不扩容）。
    poll_buf: Vec<Rec>,
    /// 逐调用缓冲容量上限。
    poll_limit: usize,
    /// 上一次 `ReleaseFrame` 的结束时刻。
    last_release_end: Option<Instant>,
}

/// 迭代内的分段累加项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterStage {
    /// `AcquireNextFrame` 调用。
    Acquire,
    /// 等设备锁。
    Lock,
    /// 复制 + Flush（含等锁）。
    Copy,
}

/// 一次慢迭代的各项数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlowIter {
    /// 迭代开始时刻。
    start: Instant,
    /// 迭代总耗时。
    total: Duration,
    /// 上一圈请求的睡眠时长。
    sleep_req: Duration,
    /// 睡眠超额。
    sleep_over: Duration,
    /// 取帧调用耗时。
    acq: Duration,
    /// 设备锁等待。
    lock: Duration,
    /// 复制 + Flush。
    copy: Duration,
    /// 上一圈结束到本圈开始的间隔。
    gap: Duration,
}

/// 采集环路迭代计时状态（纯逻辑，时刻由调用方传入，便于确定性测试）。
#[derive(Debug, Default)]
struct IterState {
    /// 当前是否有未结束的迭代。
    open: bool,
    /// 记录里的来源输出序号（单屏 0，跨屏 255）。
    src: u8,
    /// 本圈开始时刻。
    start: Option<Instant>,
    /// 上一圈结束时刻。
    prev_end: Option<Instant>,
    /// 上一圈末尾请求的睡眠时长（本圈开始时消费）。
    pending_req: Duration,
    /// 本圈对应的睡眠请求时长。
    req: Duration,
    /// 本圈间隔。
    gap: Duration,
    /// 本圈睡眠超额。
    over: Duration,
    /// 取帧调用耗时。
    acq: Duration,
    /// 设备锁等待。
    lock: Duration,
    /// 复制 + Flush 耗时。
    copy: Duration,
}

impl IterState {
    /// 一圈开始：算出与上一圈结束的间隔与睡眠超额，清空分段累加。
    fn begin(&mut self, now: Instant) {
        self.gap = self.prev_end.map_or(Duration::ZERO, |end| now.saturating_duration_since(end));
        self.req = std::mem::take(&mut self.pending_req);
        self.over = if self.req.is_zero() { Duration::ZERO } else { self.gap.saturating_sub(self.req) };
        self.acq = Duration::ZERO;
        self.lock = Duration::ZERO;
        self.copy = Duration::ZERO;
        self.start = Some(now);
        self.open = true;
    }

    /// 累加分段耗时。
    fn add(&mut self, stage: IterStage, d: Duration) {
        match stage {
            IterStage::Acquire => self.acq += d,
            IterStage::Lock => self.lock += d,
            IterStage::Copy => self.copy += d,
        }
    }

    /// 一圈结束：记下结束时刻与接下来要睡的时长；间隔或总耗时达到阈值时返回这一圈的数据。
    fn end(&mut self, now: Instant, sleep_req: Duration, threshold: Duration) -> Option<SlowIter> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.prev_end = Some(now);
        self.pending_req = sleep_req;
        let start = self.start?;
        let total = now.saturating_duration_since(start);
        (self.gap >= threshold || total >= threshold).then_some(SlowIter {
            start,
            total,
            sleep_req: self.req,
            sleep_over: self.over,
            acq: self.acq,
            lock: self.lock,
            copy: self.copy,
            gap: self.gap,
        })
    }
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

    /// 追加一条逐调用记录；缓冲已满只计数。
    fn push_poll(&mut self, rec: Rec) {
        if self.poll_buf.len() < self.poll_limit {
            self.poll_buf.push(rec);
        } else {
            self.tracer.poll_overflow.fetch_add(1, Ordering::Relaxed);
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
        Self::attach_full(tracer, thread, limit, CAPACITY_POLL)
    }

    /// 绑定指定追踪器并分别指定普通缓冲与逐调用缓冲的容量（逐调用只在采集线程且追踪器开启时分配）。
    fn attach_full(tracer: Option<Arc<Tracer>>, thread: Thread, limit: usize, poll_limit: usize) -> Self {
        Self {
            inner: tracer.map(|tracer| {
                let slow = Duration::from_micros(u64::from(tracer.slow_iter_us));
                let poll = tracer.poll && thread == Thread::Capture;
                let poll_buf = Vec::with_capacity(if poll { poll_limit } else { 0 });
                Inner { tracer, thread, buf: Vec::with_capacity(limit), limit, iter: IterState::default(), slow, poll, poll_buf, poll_limit, last_release_end: None }
            }),
        }
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

    /// 逐调用追踪是否开启（关闭时只是一次分支）。
    #[inline]
    pub fn poll_enabled(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.poll)
    }

    /// 逐调用追踪开启时返回当前时刻（给 [`TraceBuf::release`] 当起点），否则 `None`，不读时钟。
    #[inline]
    pub fn poll_mark(&self) -> Option<Instant> {
        self.inner.as_ref().filter(|i| i.poll).map(|_| Instant::now())
    }

    /// 记录一次 `AcquireNextFrame` 调用（逐调用追踪未开启时什么也不做）。
    ///
    /// # 参数
    /// - `src`：输出序号。
    /// - `start`：调用开始时刻。
    /// - `dur`：调用耗时。
    /// - `outcome`：`code::POLL_*`。
    /// - `sample`：成功时的帧信息摘要，失败为 `None`。
    #[inline]
    pub fn poll(&mut self, src: u8, start: Instant, dur: Duration, outcome: u8, sample: Option<PollSample>) {
        let Some(inner) = &mut self.inner else { return };
        if !inner.poll {
            return;
        }
        let mut rec = Rec::new(Event::Poll, inner.thread, inner.tracer.ns_of(start));
        rec.src = src;
        rec.dur_us = micros(dur);
        rec.code = outcome;
        rec.x = [NA; 6];
        rec.x[3] = inner.last_release_end.map_or(NA, |end| micros(start.saturating_duration_since(end)));
        if let Some(s) = sample {
            rec.n = s.accumulated;
            rec.present_ns = s.present.map_or(NONE, |p| inner.tracer.ns_of(p));
            rec.x[0] = s.flags;
            rec.x[1] = s.pointer_bytes;
            rec.x[2] = s.meta_bytes;
        }
        inner.push_poll(rec);
    }

    /// 记录一次 `ReleaseFrame` 调用（耗时 = 现在 - `started`；未开启或 `started` 为 `None` 时什么也不做）。
    ///
    /// # 参数
    /// - `src`：输出序号。
    /// - `started`：调用前由 [`TraceBuf::poll_mark`] 取得的起点。
    #[inline]
    pub fn release(&mut self, src: u8, started: Option<Instant>) {
        let (Some(inner), Some(s)) = (&mut self.inner, started) else { return };
        let dur = s.elapsed();
        inner.last_release_end = Some(s + dur);
        let mut rec = Rec::new(Event::Release, inner.thread, inner.tracer.ns_of(s));
        rec.src = src;
        rec.dur_us = micros(dur);
        inner.push_poll(rec);
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

    /// 采集环路一圈开始（关闭时只是一次分支）。
    ///
    /// # 参数
    /// - `now`：本圈开始时刻（调用方通常已经读过时钟，直接传入以免重复读取）。
    /// - `src`：记录里的来源输出序号（单屏 0，跨屏 255）。
    #[inline]
    pub fn iter_begin(&mut self, now: Instant, src: u8) {
        if let Some(inner) = &mut self.inner {
            inner.iter.src = src;
            inner.iter.begin(now);
        }
    }

    /// 给当前这一圈累加分段耗时（关闭时只是一次分支）。
    ///
    /// # 参数
    /// - `stage`：分段。
    /// - `d`：耗时。
    #[inline]
    pub fn iter_add(&mut self, stage: IterStage, d: Duration) {
        if let Some(inner) = &mut self.inner {
            inner.iter.add(stage, d);
        }
    }

    /// 给当前这一圈累加分段耗时，起点由 [`TraceBuf::mark`] 取得（关闭时 `started` 为 `None`，不读时钟）。
    ///
    /// # 参数
    /// - `stage`：分段。
    /// - `started`：起点。
    #[inline]
    pub fn iter_since(&mut self, stage: IterStage, started: Option<Instant>) {
        if let (Some(inner), Some(s)) = (&mut self.inner, started) {
            inner.iter.add(stage, s.elapsed());
        }
    }

    /// 当前一圈以"即将睡眠"结束：记下请求的睡眠时长，慢迭代才写记录。须紧挨在睡眠调用之前。
    ///
    /// # 参数
    /// - `req`：请求的睡眠时长。
    #[inline]
    pub fn iter_sleep(&mut self, req: Duration) {
        self.iter_end(req);
    }

    /// 当前一圈以"返回调用方"结束（没有睡眠）；没有未结束的一圈时什么也不做。
    #[inline]
    pub fn iter_finish(&mut self) {
        self.iter_end(Duration::ZERO);
    }

    /// 结束当前一圈；慢迭代才追加记录，正常迭代只读一次时钟、不分配。
    #[inline]
    fn iter_end(&mut self, req: Duration) {
        let Some(inner) = &mut self.inner else { return };
        if !inner.iter.open {
            return;
        }
        let now = Instant::now();
        if let Some(s) = inner.iter.end(now, req, inner.slow) {
            let mut rec = Rec::new(Event::SlowIter, inner.thread, inner.tracer.ns_of(s.start));
            rec.src = inner.iter.src;
            rec.dur_us = micros(s.total);
            rec.x = [micros(s.sleep_req), micros(s.sleep_over), micros(s.acq), micros(s.lock), micros(s.copy), micros(s.gap)];
            inner.push(rec);
        }
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
            inner.tracer.merge(inner.poll_buf);
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

    /// 迭代计时：睡眠超额 = 间隔 - 请求时长；达到阈值才返回，分段累加正确。
    #[test]
    fn iter_state_detects_slow_iterations() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let thr = ms(4);
        let mut it = IterState::default();
        // 首圈没有间隔；0.5ms 的正常迭代不报，并请求睡 400us
        it.begin(t0);
        assert!(it.end(t0 + Duration::from_micros(500), Duration::from_micros(400), thr).is_none());
        // 睡了 7ms 才醒（请求 0.4ms）：间隔 7ms >= 4ms，睡眠超额 ≈ 6.6ms
        it.begin(t0 + Duration::from_micros(500) + ms(7));
        it.add(IterStage::Acquire, Duration::from_micros(30));
        it.add(IterStage::Lock, Duration::from_micros(10));
        it.add(IterStage::Copy, Duration::from_micros(80));
        it.add(IterStage::Copy, Duration::from_micros(20));
        let s = it.end(t0 + ms(7) + Duration::from_micros(700), Duration::ZERO, thr).expect("应为慢迭代");
        assert_eq!((s.gap, s.sleep_req, s.sleep_over), (ms(7), Duration::from_micros(400), Duration::from_micros(6600)));
        assert_eq!((s.acq, s.lock, s.copy), (Duration::from_micros(30), Duration::from_micros(10), Duration::from_micros(100)));
        assert_eq!(s.total, Duration::from_micros(200));
        // 上一圈以返回结束：请求时长为 0，间隔大也不算睡眠超额，但仍记为慢迭代
        it.begin(t0 + ms(20));
        let s = it.end(t0 + ms(20) + Duration::from_micros(100), Duration::ZERO, thr).expect("间隔过大");
        assert_eq!((s.sleep_req, s.sleep_over, s.gap), (Duration::ZERO, Duration::ZERO, ms(12) + Duration::from_micros(300)));
        // 迭代自身耗时过长（间隔正常）：取帧调用卡了 5ms
        it.begin(t0 + ms(20) + Duration::from_micros(200));
        it.add(IterStage::Acquire, ms(5));
        let s = it.end(t0 + ms(26), Duration::ZERO, thr).expect("总耗时过长");
        assert!(s.gap < thr && s.total >= thr && s.acq == ms(5));
        // 没有未结束的迭代时 end 不产出
        assert!(it.end(t0 + ms(30), Duration::ZERO, thr).is_none());
    }

    /// 关闭状态下迭代追踪点不读时钟、不产出记录；开启时慢迭代才落一条且列正确，正常迭代不落记录。
    #[test]
    fn slow_iter_records_only_when_slow() {
        let mut off = TraceBuf::attach(None, Thread::Capture);
        off.iter_begin(Instant::now(), 0);
        off.iter_add(IterStage::Lock, Duration::from_millis(9));
        off.iter_sleep(Duration::from_micros(400));
        off.iter_finish();
        let t = Arc::new(Tracer::new(PathBuf::from("unused-trace.csv")).with_slow_iter_us(2000));
        {
            let mut buf = TraceBuf::attach(Some(Arc::clone(&t)), Thread::Capture);
            buf.iter_begin(Instant::now(), 0);
            buf.iter_sleep(Duration::from_micros(400));
            std::thread::sleep(Duration::from_millis(5));
            buf.iter_begin(Instant::now(), 0);
            buf.iter_add(IterStage::Acquire, Duration::from_micros(77));
            buf.iter_finish();
            buf.iter_finish();
        }
        let recs = t.collect();
        assert_eq!(recs.len(), 1, "只有睡过头的那一圈该落记录");
        let r = &recs[0];
        assert_eq!((r.event, r.src, r.x[0], r.x[2]), (Event::SlowIter, 0, 400, 77));
        assert!(r.x[1] >= 3000 && r.x[5] >= 4000, "睡眠超额与间隔应反映 5ms 睡眠");
        let text = t.render(&recs, 60, "mock");
        assert!(text.contains("# slow_iter_us=2000"));
        let row = text.lines().find(|l| l.contains(",slow_iter,")).unwrap();
        assert_eq!(row.split(',').count(), CSV_HEADER.split(',').count());
    }

    /// 逐调用开关：仅 `1` 开启。
    #[test]
    fn poll_trace_flag_parsing() {
        assert!(!poll_trace_requested(None) && !poll_trace_requested(Some("")) && !poll_trace_requested(Some("0")));
        assert!(poll_trace_requested(Some("1")) && poll_trace_requested(Some(" 1 ")));
    }

    /// 逐调用追踪关闭（追踪器未开 poll）：不分配、不记录、不出现头部行。
    #[test]
    fn poll_off_records_nothing() {
        let t = tracer();
        {
            let mut buf = TraceBuf::attach(Some(Arc::clone(&t)), Thread::Capture);
            assert!(!buf.poll_enabled() && buf.poll_mark().is_none());
            buf.poll(0, Instant::now(), Duration::from_millis(1), code::POLL_OK, None);
            buf.release(0, buf.poll_mark());
        }
        let recs = t.collect();
        assert!(recs.is_empty());
        assert!(!t.render(&recs, 60, "x").contains("poll_overflow"));
    }

    /// 逐调用追踪开启：poll/release 列正确，释放间隔被换算，写满只计数。
    #[test]
    fn poll_records_columns_and_overflow() {
        let t = Arc::new(Tracer::new(PathBuf::from("unused-trace.csv")).with_poll_trace(true));
        {
            let mut buf = TraceBuf::attach_full(Some(Arc::clone(&t)), Thread::Capture, 16, 4);
            assert!(buf.poll_enabled());
            let t0 = Instant::now();
            // 超时调用：没有帧信息，也没有上一次释放
            buf.poll(0, t0, Duration::from_micros(4400), code::POLL_TIMEOUT, None);
            // 成功调用，随后释放，再来一次调用以验证释放间隔
            let sample = PollSample { accumulated: 2, present: Some(t0), flags: poll_flag::PRESENT | poll_flag::MOUSE_UPDATE, pointer_bytes: 7, meta_bytes: 9 };
            buf.poll(0, t0, Duration::from_micros(30), code::POLL_OK, Some(sample));
            let started = buf.poll_mark();
            buf.release(0, started);
            std::thread::sleep(Duration::from_millis(2));
            buf.poll(0, Instant::now(), Duration::from_micros(10), code::POLL_TIMEOUT, None);
            // 第 5 条超出容量 4
            buf.poll(0, Instant::now(), Duration::from_micros(10), code::POLL_TIMEOUT, None);
        }
        let recs = t.collect();
        assert_eq!(recs.len(), 4);
        let text = t.render(&recs, 60, "x");
        assert!(text.contains("# poll_trace=1
") && text.contains("# poll_overflow=1
"));
        let rows: Vec<Vec<&str>> = text.lines().skip_while(|l| !l.starts_with("mono_ns")).skip(1).map(|l| l.split(',').collect()).collect();
        assert!(rows.iter().all(|c| c.len() == CSV_HEADER.split(',').count()));
        let by_event = |name: &str| rows.iter().filter(|c| c[2] == name).collect::<Vec<_>>();
        assert_eq!(by_event("poll").len(), 3);
        assert_eq!(by_event("release").len(), 1);
        let timeout = by_event("poll")[0].clone();
        assert_eq!((timeout[9], timeout[10], timeout[8], timeout[7]), ("4400", "1", "0", ""));
        assert_eq!((timeout[11], timeout[12], timeout[13], timeout[14]), ("", "", "", ""));
        let ok = by_event("poll")[1].clone();
        assert_eq!((ok[8], ok[10], ok[11], ok[12], ok[13]), ("2", "0", "3", "7", "9"));
        assert!(!ok[7].is_empty());
        let later = by_event("poll")[2].clone();
        assert!(later[14].parse::<u32>().unwrap() >= 1500, "释放结束到下次调用约 2ms");
    }

    /// 元数据行：同名覆盖、换行被替换。
    #[test]
    fn notes_overwrite_and_render() {
        let t = tracer();
        t.set_note("capture_sched", "mmcss");
        t.set_note("capture_sched", "timecritical");
        t.set_note("capture_sched_detail", "a\nb");
        let text = t.render(&[], 60, "x");
        assert!(text.contains("# capture_sched=timecritical\n") && !text.contains("# capture_sched=mmcss"));
        assert!(text.contains("# capture_sched_detail=a b\n"));
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
