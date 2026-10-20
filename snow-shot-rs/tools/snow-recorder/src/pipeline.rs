//! 录制流水线：捕获 → 转换合成 → 编码，四个线程并行，阶段之间只通过有界队列和丢弃策略解耦。
//!
//! 三个阶段由三个 trait 定义边界（[`CaptureSource`]、[`FrameComposer`]、[`VideoEncoder`]），
//! 具体实现在会话初始化时装配好，流水线对它们泛型单态化，每帧热路径没有动态分发。
//! Windows 硬件实现（DXGI 复制、D3D11 VideoProcessor、QSV 等）在 `win` 模块，本文件与平台无关。
//!
//! - 捕获线程：阻塞取帧，帧留在采集实现自己的缓冲池里；
//! - 合成线程：按固定 `1/fps` 节拍输出，每个槽取"呈现时间不晚于切点的最新帧"，合成到编码表面；
//! - 编码线程：送帧、取包、封装。
//!
//! 背压语义全部是"有界 + 丢弃并计数"：采集池耗尽丢新帧、待输出队列溢出/被更新的帧取代时丢旧帧、
//! 编码表面池耗尽丢当前帧、落后的槽直接跳过（不补发）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use snow_cursor::AttachedCursorSample;

use crate::audio::AudioRecorder;
use crate::frametrace::{self, Thread, TraceBuf, code};
use crate::os::{TimerGuard, apply_capture_sched_from_env};
use crate::timeline::{NANOS_PER_SEC, PhaseTracker, TickClock, TimedQueue, Timeline, slot_of};

/// 合成线程等待采集事件的最长时间。
const COMPOSE_WAIT: Duration = Duration::from_millis(10);
/// 采集线程单次阻塞等待新帧的最长时间；超时只是为了定期检查停止标志。
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(50);
/// 启动时等待首帧的最长时间。
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(3);
/// 槽触发的额外等待 = 一个槽周期 + 该余量：切点在呈现时刻的对侧（约半个槽），
/// 再等一个槽周期，采集线程偶发的十几毫秒停顿也不会让帧错过自己的槽。延迟对录屏无害。
const SLOT_HOLD_MARGIN: Duration = Duration::from_millis(4);
/// 停止时最多补发的槽数（把停止前最后呈现的帧送出）。
const FINAL_FLUSH_SLOTS: u64 = 4;
/// 预热最长等待：静止画面没有足够的呈现样本时，超过该时长也开始输出。
const WARMUP_TIMEOUT: Duration = Duration::from_millis(500);
/// 采集故障后的最大重建次数。
const MAX_RECREATE_ATTEMPTS: u32 = 30;
/// 重建之间的等待。
const RECREATE_BACKOFF: Duration = Duration::from_millis(100);
/// 请求的计时器精度（毫秒）。
const TIMER_RESOLUTION_MS: u32 = 1;
/// 诊断样本上限。
const SAMPLE_LIMIT: usize = 20_000;
/// 采集消息容量；光标更新单独覆盖保存。
const CAPTURE_CAPACITY: usize = 16;

/// 一次采集的结果。
#[derive(Clone)]
pub struct Captured<F> {
    /// 平台相关的帧句柄（例如 GPU 纹理租约）。
    pub frame: F,
    /// 取帧时刻的光标采样（选区内坐标）。
    pub cursor: Option<AttachedCursorSample>,
    /// 桌面最后一次更新的呈现时刻。
    pub present: Instant,
    /// 取到这一帧的时刻。
    pub captured_at: Instant,
    /// 桌面是否有新内容（`false` 表示只有光标移动，画面沿用上一帧）。
    pub fresh: bool,
    /// 捕获序号（采集实现自己递增，从 1 起；光标补帧沿用所复用桌面帧的序号），仅供帧追踪关联。
    pub id: u64,
}

/// 采集故障分类。
#[derive(Debug)]
pub enum CaptureFault {
    /// 采集会话失效（分辨率切换、锁屏等），需要重建。
    Lost,
    /// 其他错误（附原因）。
    Other(String),
}

/// 采集实现的计数（线程内周期性同步给主控）。
#[derive(Debug, Default, Clone, Copy)]
pub struct CaptureStats {
    /// 取到的桌面更新总数。
    pub frames: u64,
    /// 缓冲池耗尽而丢弃的桌面更新数。
    pub pool_drops: u64,
    /// 被采集 API 合并掉的更新数（没来得及取走的呈现）。
    pub coalesced: u64,
}

/// 采集实现附带的诊断数据：命名的毫秒样本与计数。
#[derive(Debug, Default, Clone)]
pub struct CaptureDiag {
    /// 命名样本（毫秒）。
    pub samples: Vec<(&'static str, Vec<f32>)>,
}

/// 捕获阶段：产出带时间戳的桌面帧。实现自带缓冲池与背压（池耗尽时丢新帧并计数）。
pub trait CaptureSource: Send + 'static {
    /// 平台相关的帧句柄；克隆必须便宜（引用计数）。
    type Frame: Clone + Send + 'static;

    /// 阻塞等待下一次桌面更新或光标移动。
    ///
    /// # 参数
    /// - `timeout`：最长等待时间。
    /// - `want`：为 `false` 时只取走并释放（暂停期间用，不复制）。
    ///
    /// # 返回
    /// `Some(帧)`：有新内容；`None`：超时、被丢弃或没有可用画面。
    fn next(
        &mut self,
        timeout: Duration,
        want: bool,
    ) -> Result<Option<Captured<Self::Frame>>, CaptureFault>;

    /// 重建采集会话（[`CaptureFault::Lost`] 之后调用）。
    fn recreate(&mut self) -> Result<(), String>;

    /// 恢复后重新取得完整桌面。
    fn refresh(&mut self) -> Result<(), String> {
        self.recreate()
    }

    /// 当前计数。
    fn stats(&self) -> CaptureStats;

    /// 取走诊断数据（线程结束时调用一次）。
    fn take_diag(&mut self) -> CaptureDiag {
        CaptureDiag::default()
    }
}

/// 转换合成阶段：把桌面帧与光标合成到编码器输入表面。
pub trait FrameComposer: Send + 'static {
    /// 采集帧类型。
    type Frame;
    /// 编码输入表面类型。
    type Surface: Send + 'static;

    /// 取一张空表面；池耗尽返回 `None`（调用方丢帧）。
    fn acquire_surface(&mut self) -> Result<Option<Self::Surface>, String>;

    /// 把桌面帧与光标合成到表面。
    ///
    /// # 参数
    /// - `frame`：采集帧。
    /// - `cursor`：光标采样；`None` 不画光标。
    /// - `surface`：输出表面。
    fn compose(
        &mut self,
        frame: &Self::Frame,
        cursor: Option<&AttachedCursorSample>,
        surface: &mut Self::Surface,
    ) -> Result<(), String>;

    /// 在用的表面数（诊断用）。
    fn in_flight(&self) -> usize {
        0
    }
}

/// 编码阶段。
pub trait VideoEncoder: Send + 'static {
    /// 输入表面类型。
    type Surface: Send + 'static;

    /// 送入一帧（`pts` 为输出槽号，严格递增）。
    fn submit(&mut self, surface: Self::Surface, pts: i64) -> Result<(), String>;

    /// 结束：冲刷编码器并收尾封装。
    ///
    /// # 参数
    /// - `end_pts`：排他终点槽号（末帧时长 = 终点 - 末帧 pts）。
    fn finish(self, end_pts: i64) -> Result<EncoderStats, String>;
}

/// 编码阶段统计。
#[derive(Debug, Default, Clone)]
pub struct EncoderStats {
    /// 送入编码器的帧数。
    pub frames: u64,
    /// 每帧送帧耗时样本（毫秒）。
    pub send_ms: Vec<f32>,
}

/// 流水线配置。
#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    /// 输出帧率。
    pub fps: u32,
    /// 是否叠加光标。
    pub show_cursor: bool,
}

/// 样本集合（毫秒），用于阶段耗时分位数。
#[derive(Debug, Default, Clone)]
pub struct Samples(pub Vec<f32>);

impl Samples {
    /// 追加样本（超过上限忽略）。
    pub fn push(&mut self, ms: f32) {
        if self.0.len() < SAMPLE_LIMIT {
            self.0.push(ms);
        }
    }

    /// 分位数（0..=1）；无样本返回 0。
    ///
    /// # 示例
    /// ```ignore
    /// let s = Samples(vec![1.0, 2.0, 3.0]);
    /// assert_eq!(s.percentile(0.5), 2.0);
    /// ```
    pub fn percentile(&self, q: f32) -> f32 {
        if self.0.is_empty() {
            return 0.0;
        }
        let mut v = self.0.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v[((v.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize]
    }

    /// 摘要文本：`n=.. p50=.. p95=.. max=..`。
    pub fn summary(&self) -> String {
        format!(
            "n={} p50={:.2} p95={:.2} max={:.2}",
            self.0.len(),
            self.percentile(0.5),
            self.percentile(0.95),
            self.percentile(1.0)
        )
    }
}

/// 录制结果与诊断。
#[derive(Debug, Default, Clone)]
pub struct PipelineReport {
    /// 音频降级与收尾诊断。
    pub audio: Option<crate::audio::AudioReport>,
    /// 实际使用的后端名（如 `dxgi+videoprocessor+h264_qsv`）。
    pub backend: String,
    /// 送入编码器的帧数。
    pub encoded_frames: u64,
    /// 采集计数。
    pub capture: CaptureStats,
    /// 待输出队列溢出或被更新的帧取代而丢弃的帧数（源比输出快时的正常抽稀）。
    pub queue_dropped: u64,
    /// 合成线程来不及而跳过的输出槽数。
    pub missed_slots: u64,
    /// 只因光标移动而补出的帧数。
    pub cursor_frames: u64,
    /// 编码表面池耗尽丢弃的帧数。
    pub pool_dropped: u64,
    /// 编码表面在用数的峰值。
    pub pool_peak: usize,
    /// 合成耗时（取表面之后的合成）。
    pub compose_ms: Samples,
    /// 编码阶段实际送入的帧数（应与合成线程送出的帧数一致）。
    pub encoder_frames: u64,
    /// 编码线程每帧送帧耗时。
    pub send_ms: Samples,
    /// 采集到开始合成的延迟。
    pub queue_ms: Samples,
    /// 相邻桌面更新的呈现间隔（诊断源的节奏）。
    pub present_dt_ms: Samples,
    /// 帧呈现到被合成线程看到的延迟。
    pub arrive_lag_ms: Samples,
    /// 采集实现的诊断样本。
    pub capture_diag: Vec<(&'static str, Samples)>,
    /// 启动耗时分解（毫秒）：等首帧、首帧合成、首帧送编码。
    pub startup_ms: (f32, f32, f32),
}

impl PipelineReport {
    /// 诊断摘要（多行，写进 stderr）。
    pub fn describe(&self) -> String {
        let mut text = format!(
            "录制流水线: 后端={} 编码帧={} 采集更新={} 采集池丢弃={} 采集合并丢失={} 队列丢弃={} 跳过槽={} 光标补帧={} 编码表面池丢弃={} 编码表面峰值={}\n  合成ms {}\n  送帧ms {}\n  采集到合成ms {}\n  呈现间隔ms {}\n  呈现到看到ms {}",
            self.backend,
            self.encoded_frames,
            self.capture.frames,
            self.capture.pool_drops,
            self.capture.coalesced,
            self.queue_dropped,
            self.missed_slots,
            self.cursor_frames,
            self.pool_dropped,
            self.pool_peak,
            self.compose_ms.summary(),
            self.send_ms.summary(),
            self.queue_ms.summary(),
            self.present_dt_ms.summary(),
            self.arrive_lag_ms.summary()
        );
        text.push_str(&format!(
            "\n  启动ms 等首帧 {:.1} / 首帧合成 {:.1} / 首帧送编码 {:.1}",
            self.startup_ms.0, self.startup_ms.1, self.startup_ms.2
        ));
        if let Some(audio) = &self.audio {
            text.push_str(&format!("\n  audio: {}", audio.describe()));
        }
        for (name, samples) in &self.capture_diag {
            text.push_str(&format!("\n  {name} {}", samples.summary()));
        }
        text
    }
}

/// 发给合成线程的控制命令。
enum Control {
    /// 暂停（附时刻）。
    Pause(Instant),
    /// 恢复（附时刻）。
    Resume(Instant),
    /// 停止并收尾（附时刻）。
    Stop(Instant),
    /// 取消并丢弃。
    Cancel,
}

/// 发给编码线程的消息。
enum EncodeMsg<S> {
    /// 一帧已合成的表面与槽号。
    Frame(S, i64),
    /// 结束（排他终点槽号）。
    Finish(i64),
}

/// 采集线程发给合成线程的消息。
enum CaptureMsg<F> {
    /// 一次桌面更新或光标移动。
    Frame(Box<Captured<F>>),
    /// 采集出错（线程随后退出）。
    Fault(String),
}

/// 采集线程与主控共享的标志与计数。
#[derive(Default)]
struct CaptureShared {
    /// 采集首个故障，不受有界通道满载影响。
    failure: Mutex<Option<String>>,
    /// 请求停止。
    stop: AtomicBool,
    /// 暂停中（只取走不复制）。
    paused: AtomicBool,
    /// 恢复时请求重新采集。
    refresh: AtomicBool,
    /// 取到的桌面更新数。
    frames: AtomicU64,
    /// 缓冲池耗尽丢弃数。
    pool_drops: AtomicU64,
    /// 被合并掉的更新数。
    coalesced: AtomicU64,
}

/// 运行中的流水线（与具体实现无关的句柄）。
pub struct Running {
    /// 控制通道。
    control: Sender<Control>,
    /// 各后台阶段的首个故障。
    failure: Arc<Mutex<Option<String>>>,
    /// 采集线程共享状态。
    shared: Arc<CaptureShared>,
    /// 采集线程（返回诊断样本）。
    capture: Option<JoinHandle<CaptureDiag>>,
    /// 合成线程。
    compose: Option<JoinHandle<Result<PipelineReport, String>>>,
    /// 编码线程。
    encode: Option<JoinHandle<Result<EncoderStats, String>>>,
    /// 后端名。
    backend: String,
    /// 输出帧率（写帧追踪元数据用）。
    fps: u32,
    /// 启动耗时分解（毫秒）。
    startup_ms: (f32, f32, f32),
    /// 音频录制（没有音频时为 `None`）；停止时先于视频收尾。
    audio: Option<AudioRecorder>,
    /// 计时器精度守卫（随录制结束释放）。
    _timer: TimerGuard,
}

/// 光标采样的可比较摘要：位置、可见性与形状 ID。
fn cursor_key(c: &AttachedCursorSample) -> (i32, i32, bool, Option<u64>) {
    (c.x, c.y, c.visible, c.shape_id().map(|id| id.get()))
}

/// 槽号对应的有效时长。
pub fn slot_time(slot: u64, fps: u32) -> Duration {
    Duration::from_nanos(
        (u128::from(slot) * u128::from(NANOS_PER_SEC) / u128::from(fps.max(1)))
            .min(u128::from(u64::MAX)) as u64,
    )
}

/// 非阻塞投递桌面帧；光标只保留最新值，队列满时释放新桌面帧。
fn forward_capture<F>(
    captured: Captured<F>,
    tx: &SyncSender<CaptureMsg<F>>,
    cursor: &Mutex<Option<Captured<F>>>,
) -> bool {
    if !captured.fresh {
        *cursor.lock().unwrap() = Some(captured);
        true
    } else {
        !matches!(
            tx.try_send(CaptureMsg::Frame(Box::new(captured))),
            Err(TrySendError::Disconnected(_))
        )
    }
}

/// 故障先保存在共享状态，再尝试唤醒合成线程；队列满时也不会丢失原因。
fn capture_fault<F>(shared: &CaptureShared, tx: &SyncSender<CaptureMsg<F>>, error: String) {
    let mut first = shared.failure.lock().unwrap();
    if first.is_none() {
        *first = Some(error.clone());
    }
    let _ = tx.try_send(CaptureMsg::Fault(error));
}

/// 采集线程主循环：阻塞取帧并转交合成线程，会话失效时重建。
fn capture_loop<C: CaptureSource>(
    mut source: C,
    tx: SyncSender<CaptureMsg<C::Frame>>,
    cursor: Arc<Mutex<Option<Captured<C::Frame>>>>,
    shared: Arc<CaptureShared>,
) -> CaptureDiag {
    // 调度方式由环境变量决定（缺省与历史一致：HIGHEST）；守卫持有到线程退出，退出 MMCSS 须在本线程完成
    let _sched = apply_capture_sched_from_env();
    let mut trace = TraceBuf::new(Thread::Capture);
    let mut recreates = 0;
    while !shared.stop.load(Ordering::Acquire) {
        if shared.paused.load(Ordering::Acquire) {
            std::thread::sleep(COMPOSE_WAIT);
            continue;
        }
        if shared.refresh.swap(false, Ordering::AcqRel)
            && let Err(e) = source.refresh()
        {
            capture_fault(&shared, &tx, e);
            break;
        }
        let want = true;
        match source.next(ACQUIRE_TIMEOUT, want) {
            Ok(Some(captured)) => {
                trace.enqueue(captured.id, captured.fresh);
                if !forward_capture(captured, &tx, &cursor) {
                    break;
                }
            }
            Ok(None) => {}
            Err(CaptureFault::Lost) => {
                recreates += 1;
                if recreates > MAX_RECREATE_ATTEMPTS {
                    capture_fault(&shared, &tx, "采集会话持续失效".into());
                    break;
                }
                std::thread::sleep(RECREATE_BACKOFF);
                if let Err(e) = source.recreate() {
                    capture_fault(&shared, &tx, e);
                    break;
                }
            }
            Err(CaptureFault::Other(e)) => {
                capture_fault(&shared, &tx, e);
                break;
            }
        }
        let stats = source.stats();
        shared.frames.store(stats.frames, Ordering::Relaxed);
        shared.pool_drops.store(stats.pool_drops, Ordering::Relaxed);
        shared.coalesced.store(stats.coalesced, Ordering::Relaxed);
    }
    source.take_diag()
}

/// 编码线程主循环：送帧、取包、收尾。
fn encode_loop<E: VideoEncoder>(
    mut encoder: E,
    rx: Receiver<EncodeMsg<E::Surface>>,
) -> Result<EncoderStats, String> {
    let mut trace = TraceBuf::new(Thread::Encode);
    while let Ok(msg) = rx.recv() {
        match msg {
            EncodeMsg::Frame(surface, pts) => {
                let started = trace.mark();
                encoder.submit(surface, pts)?;
                trace.submit(pts, started);
            }
            EncodeMsg::Finish(end) => {
                let started = trace.mark();
                let result = encoder.finish(end);
                trace.finish(end, started);
                return result;
            }
        }
    }
    Err("编码线程在收到结束前通道关闭".into())
}

/// 等待采集线程送来第一张带桌面内容的帧。
fn wait_first_frame<F>(rx: &Receiver<CaptureMsg<F>>) -> Result<Box<Captured<F>>, String> {
    let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(CaptureMsg::Frame(f)) if f.fresh => return Ok(f),
            Ok(CaptureMsg::Frame(_)) => {}
            Ok(CaptureMsg::Fault(e)) => return Err(format!("采集出错: {e}")),
            Err(RecvTimeoutError::Timeout) => return Err("等待首帧超时".into()),
            Err(RecvTimeoutError::Disconnected) => return Err("采集提前结束".into()),
        }
    }
}

/// 合成线程持有的全部状态。
struct ComposeContext<C: CaptureSource, P, S> {
    /// 配置。
    config: PipelineConfig,
    /// 合成器。
    composer: P,
    /// 时间线。
    timeline: Timeline,
    /// 输出节拍。
    ticks: TickClock,
    /// 最新光标事件，消费者停滞时覆盖。
    cursor: Arc<Mutex<Option<Captured<C::Frame>>>>,
    /// 待输出队列（只放有新桌面内容的帧，按呈现时间排序）。
    queue: TimedQueue<Captured<C::Frame>>,
    /// 槽切点相位跟踪。
    phase: PhaseTracker,
    /// 槽触发的额外等待（一个槽周期 + 余量）。
    hold: Duration,
    /// 是否已度过预热（切点可信后才开始输出）。
    warmed: bool,
    /// 录制开始时刻（预热超时用）。
    started: Instant,
    /// 最近恢复时刻，拒绝恢复前积压的旧采集事件。
    resumed_at: Option<Instant>,
    /// 最近一张桌面帧（光标单独移动时用它补帧）。
    latest: Option<Captured<C::Frame>>,
    /// 最近的光标采样。
    latest_cursor: Option<AttachedCursorSample>,
    /// 上一次送出的光标摘要。
    last_emitted_cursor: Option<(i32, i32, bool, Option<u64>)>,
    /// 最后一个已送出的槽号。
    last_slot: Option<u64>,
    /// 上一帧的呈现时刻（算呈现间隔）。
    last_present: Option<Instant>,
    /// 诊断：呈现间隔样本。
    present_dt: Samples,
    /// 诊断：呈现到看到的延迟样本。
    arrive_lag: Samples,
    /// 控制通道。
    control: Receiver<Control>,
    /// 采集通道。
    capture: Receiver<CaptureMsg<C::Frame>>,
    /// 发往编码线程的通道。
    frames: Sender<EncodeMsg<S>>,
    /// 帧追踪缓冲（未开启时是空操作）。
    trace: TraceBuf,
    /// 已写进追踪的跳槽累计数（只在追踪开启时推进）。
    traced_missed: u64,
}

/// 距离下一个输出槽的等待时间（上限 [`COMPOSE_WAIT`]）。
fn wait_until_next_slot<C: CaptureSource, P, S>(
    c: &ComposeContext<C, P, S>,
    paused: bool,
) -> Duration {
    if paused {
        return COMPOSE_WAIT;
    }
    let active = c.timeline.active_at(Instant::now());
    (slot_time(c.ticks.next_slot(), c.config.fps) + c.phase.offset() + c.hold)
        .saturating_sub(active)
        .min(COMPOSE_WAIT)
}

/// 合成线程主循环。
fn compose_loop<C, P, S>(mut c: ComposeContext<C, P, S>) -> Result<PipelineReport, String>
where
    C: CaptureSource,
    P: FrameComposer<Frame = C::Frame, Surface = S>,
    S: Send + 'static,
{
    let mut report = PipelineReport::default();
    let mut paused = false;
    let mut stop_at: Option<Instant> = None;
    let mut canceled = false;
    let mut failure: Option<String> = None;
    // 每轮复用的采集消息暂存（避免热路径上每轮分配）
    let mut messages: Vec<CaptureMsg<C::Frame>> = Vec::new();
    'main: loop {
        while let Ok(cmd) = c.control.try_recv() {
            match cmd {
                Control::Pause(at) if !paused => {
                    c.timeline.pause(at);
                    c.queue.clear();
                    c.latest = None;
                    c.latest_cursor = None;
                    *c.cursor.lock().unwrap() = None;
                    paused = true;
                }
                Control::Resume(at) if paused => {
                    c.timeline.resume(at);
                    c.resumed_at = Some(at);
                    c.queue.clear();
                    paused = false;
                }
                Control::Stop(at) => {
                    stop_at = Some(at);
                    break 'main;
                }
                Control::Cancel => {
                    canceled = true;
                    break 'main;
                }
                Control::Pause(_) | Control::Resume(_) => {}
            }
        }
        let first = match c.capture.recv_timeout(wait_until_next_slot(&c, paused)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                failure = Some("采集线程意外退出".into());
                break;
            }
        };
        messages.extend(first);
        for m in c.capture.try_iter().take(CAPTURE_CAPACITY) {
            messages.push(m);
        }
        for message in messages.drain(..) {
            match message {
                CaptureMsg::Frame(frame) => absorb_frame(&mut c, *frame, paused),
                CaptureMsg::Fault(e) => {
                    failure = Some(format!("采集出错: {e}"));
                    break 'main;
                }
            }
        }
        let cursor = c.cursor.lock().unwrap().take();
        if let Some(cursor) = cursor {
            absorb_frame(&mut c, cursor, paused);
        }
        let due = slot_of(
            c.timeline
                .active_at(Instant::now())
                .saturating_sub(c.hold + c.phase.offset()),
            c.config.fps,
        );
        if !c.warmed && (c.phase.ready() || c.started.elapsed() >= WARMUP_TIMEOUT) {
            c.warmed = true;
            // 预热期间积压的槽依次补齐（不跳槽），避免开头丢帧
            while !paused && let Some(slot) = c.ticks.fire_next(due) {
                if let Err(e) = emit_slot(&mut c, &mut report, slot) {
                    failure = Some(e);
                    break 'main;
                }
            }
        }
        if !paused
            && c.warmed
            && let Some(slot) = c.ticks.fire(due)
            && let Err(e) = emit_slot(&mut c, &mut report, slot)
        {
            failure = Some(e);
            break;
        }
    }
    if failure.is_none()
        && !canceled
        && let Some(at) = stop_at
    {
        // 停止前最后呈现的帧可能还没到触发时刻：补发到终点之前的几个槽
        let endpoint = c.timeline.endpoint(at);
        let from = c.ticks.next_slot();
        for slot in from..endpoint.min(from + FINAL_FLUSH_SLOTS) {
            if let Err(e) = emit_slot(&mut c, &mut report, slot) {
                failure = Some(e);
                break;
            }
        }
    }
    report.queue_dropped = c.queue.dropped;
    report.missed_slots = c.ticks.missed;
    report.present_dt_ms = std::mem::take(&mut c.present_dt);
    report.arrive_lag_ms = std::mem::take(&mut c.arrive_lag);
    let end = stop_at.map(|at| c.timeline.endpoint(at));
    if canceled {
        let _ = c.frames.send(EncodeMsg::Finish(1));
        return Err("已取消".into());
    }
    if let Some(e) = failure {
        let _ = c.frames.send(EncodeMsg::Finish(1));
        return Err(e);
    }
    let end = end.unwrap_or(1).max(c.last_slot.map_or(1, |l| l + 1));
    c.frames
        .send(EncodeMsg::Finish(i64::try_from(end).unwrap_or(i64::MAX)))
        .map_err(|_| "编码线程已退出".to_string())?;
    Ok(report)
}

/// 吸收一次采集：更新最近帧与光标；有新桌面内容的帧进入待输出队列。
fn absorb_frame<C: CaptureSource, P, S>(
    c: &mut ComposeContext<C, P, S>,
    captured: Captured<C::Frame>,
    paused: bool,
) {
    if paused
        || c.timeline.in_pause(captured.captured_at)
        || c.resumed_at.is_some_and(|at| captured.captured_at < at)
    {
        return;
    }
    if c.config.show_cursor
        && let Some(cursor) = &captured.cursor
    {
        c.latest_cursor = Some(cursor.clone());
    }
    if captured.fresh {
        // 暂停期间呈现、恢复后才取到的帧仍是当前画面：有效时长自然冻结在暂停点，下个槽就会取用
        let active = c.timeline.active_at(captured.present);
        if let Some(prev) = c.last_present.replace(captured.present) {
            c.present_dt.push(
                captured
                    .present
                    .saturating_duration_since(prev)
                    .as_secs_f32()
                    * 1000.0,
            );
        }
        c.arrive_lag.push(
            Instant::now()
                .saturating_duration_since(captured.present)
                .as_secs_f32()
                * 1000.0,
        );
        c.phase.observe(active);
        c.trace.absorb(captured.id, true);
        c.queue.push_with(active, captured.clone(), |evicted| {
            c.trace.discard(evicted.id, None, code::DISCARD_OVERFLOW)
        });
    }
    if captured.fresh || c.latest.is_none() {
        c.latest = Some(captured);
    }
}

/// 输出一个槽：取切点之前最新的待输出帧（没有则在光标移动时用最近帧补一帧），合成并交给编码线程。
fn emit_slot<C, P, S>(
    c: &mut ComposeContext<C, P, S>,
    report: &mut PipelineReport,
    slot: u64,
) -> Result<(), String>
where
    C: CaptureSource,
    P: FrameComposer<Frame = C::Frame, Surface = S>,
    S: Send + 'static,
{
    let cursor_key_now = c.latest_cursor.as_ref().map(cursor_key);
    let cutoff = slot_time(slot, c.config.fps) + c.phase.offset();
    if c.trace.enabled() && c.ticks.missed > c.traced_missed {
        let skipped = c.ticks.missed - c.traced_missed;
        c.trace.skip(slot.saturating_sub(skipped), skipped);
        c.traced_missed = c.ticks.missed;
    }
    let mut slot_kind = code::SLOT_NEW;
    let captured = match c.queue.take_for_slot_with(cutoff, |old| {
        c.trace
            .discard(old.id, Some(slot), code::DISCARD_SUPERSEDED)
    }) {
        Some(f) => f,
        None if c.config.show_cursor
            && cursor_key_now.is_some()
            && cursor_key_now != c.last_emitted_cursor =>
        {
            let Some(f) = c.latest.clone() else {
                return Ok(());
            };
            report.cursor_frames += 1;
            slot_kind = code::SLOT_CURSOR_REUSE;
            f
        }
        None => {
            c.trace.slot(slot, 0, code::SLOT_EMPTY);
            return Ok(());
        }
    };
    let Some(mut surface) = c.composer.acquire_surface()? else {
        c.trace
            .slot(slot, captured.id, code::SLOT_SURFACE_EXHAUSTED);
        report.pool_dropped += 1;
        return Ok(());
    };
    c.trace.slot(slot, captured.id, slot_kind);
    report.pool_peak = report.pool_peak.max(c.composer.in_flight());
    report.queue_ms.push(
        Instant::now()
            .saturating_duration_since(captured.captured_at)
            .as_secs_f32()
            * 1000.0,
    );
    let started = Instant::now();
    c.composer.compose(
        &captured.frame,
        c.latest_cursor.as_ref().filter(|_| c.config.show_cursor),
        &mut surface,
    )?;
    report
        .compose_ms
        .push(started.elapsed().as_secs_f32() * 1000.0);
    c.trace.compose(slot, captured.id, started);
    c.last_emitted_cursor = cursor_key_now;
    c.last_slot = Some(slot);
    c.frames
        .send(EncodeMsg::Frame(
            surface,
            i64::try_from(slot).unwrap_or(i64::MAX),
        ))
        .map_err(|_| "编码线程已退出".to_string())?;
    c.trace.send(slot);
    report.encoded_frames += 1;
    Ok(())
}

/// 启动流水线：启动采集线程，等首帧，合成并编码首帧作为探测，再启动合成与编码线程。
///
/// 三个阶段实现在调用方装配好后以泛型传入；任一步失败返回原因（调用方据此回落）。
///
/// # 参数
/// - `config`：流水线配置。
/// - `backend`：后端名（进诊断报告）。
/// - `capture`：捕获阶段实现。
/// - `composer`：转换合成阶段实现。
/// - `encoder`：编码阶段实现。
///
/// # 返回
/// 运行中的流水线句柄；首帧探测失败返回原因。
#[cfg_attr(not(test), allow(dead_code))]
pub fn start<C, P, E>(
    config: PipelineConfig,
    backend: &str,
    capture: C,
    composer: P,
    encoder: E,
) -> Result<Running, String>
where
    C: CaptureSource,
    P: FrameComposer<Frame = C::Frame, Surface = E::Surface>,
    E: VideoEncoder,
{
    start_with_audio(config, backend, capture, composer, encoder, None)
}

/// 启动流水线，并带上音频录制：视频时间线起点会同步给音频，暂停、恢复、停止也随之转发。
///
/// # 参数
/// - `audio`：已绑定封装后端的音频录制句柄；`None` 等同 [`start`]。
///
/// 其余参数与返回同 [`start`]；出错时音频句柄随之丢弃（线程自行退出）。
pub fn start_with_audio<C, P, E>(
    config: PipelineConfig,
    backend: &str,
    capture: C,
    mut composer: P,
    mut encoder: E,
    audio: Option<AudioRecorder>,
) -> Result<Running, String>
where
    C: CaptureSource,
    P: FrameComposer<Frame = C::Frame, Surface = E::Surface>,
    E: VideoEncoder,
{
    let timer = TimerGuard::request(TIMER_RESOLUTION_MS);
    let shared = Arc::new(CaptureShared::default());
    let failure = Arc::new(Mutex::new(None));
    let cursor = Arc::new(Mutex::new(None));
    let (capture_tx, capture_rx) = mpsc::sync_channel(CAPTURE_CAPACITY);
    let capture_thread = std::thread::Builder::new()
        .name("snow-capture".into())
        .spawn({
            let shared = Arc::clone(&shared);
            let cursor = Arc::clone(&cursor);
            move || capture_loop(capture, capture_tx, cursor, shared)
        })
        .map_err(|e| e.to_string())?;
    let stop_capture = |shared: &CaptureShared, handle: JoinHandle<CaptureDiag>| {
        shared.stop.store(true, Ordering::Release);
        let _ = handle.join();
    };
    // 首帧探测：等一张真实采集帧，合成并送编码，失败则整体回落。
    let wait_started = Instant::now();
    let first = match wait_first_frame(&capture_rx) {
        Ok(f) => f,
        Err(e) => {
            stop_capture(&shared, capture_thread);
            return Err(e);
        }
    };
    // 时间线从首帧呈现时刻起算：探测期间已呈现的帧也能落到正确的槽，不会被并进同一个槽
    let start = first.present.min(Instant::now());
    if let Some(a) = &audio {
        a.set_origin(start);
    }
    let waited = wait_started.elapsed();
    let mut compose_took = Duration::ZERO;
    let mut probe_trace = TraceBuf::new(Thread::Compose);
    let probe = (|| -> Result<(), String> {
        let mut surface = composer.acquire_surface()?.ok_or("编码表面池为空")?;
        probe_trace.slot(0, first.id, code::SLOT_PROBE);
        let compose_started = Instant::now();
        composer.compose(
            &first.frame,
            first.cursor.as_ref().filter(|_| config.show_cursor),
            &mut surface,
        )?;
        compose_took = compose_started.elapsed();
        probe_trace.compose(0, first.id, compose_started);
        let submit_started = probe_trace.mark();
        let submitted = encoder.submit(surface, 0);
        probe_trace.submit(0, submit_started);
        submitted
    })();
    let startup_ms = (
        waited.as_secs_f32() * 1000.0,
        compose_took.as_secs_f32() * 1000.0,
        (wait_started.elapsed() - waited - compose_took).as_secs_f32() * 1000.0,
    );
    if let Err(e) = probe {
        stop_capture(&shared, capture_thread);
        return Err(e);
    }
    let (control_tx, control_rx) = mpsc::channel();
    let (frame_tx, frame_rx) = mpsc::channel();
    let encode = std::thread::Builder::new()
        .name("snow-encode".into())
        .spawn({
            let failure = Arc::clone(&failure);
            move || record_failure(encode_loop(encoder, frame_rx), &failure)
        })
        .map_err(|e| e.to_string())?;
    let period = Duration::from_secs_f64(1.0 / f64::from(config.fps.max(1)));
    let first = *first;
    let ctx = ComposeContext::<C, P, E::Surface> {
        config,
        composer,
        timeline: Timeline::new(start, config.fps),
        ticks: TickClock::starting_at(1),
        cursor,
        queue: TimedQueue::default(),
        phase: PhaseTracker::new(period),
        hold: period + SLOT_HOLD_MARGIN,
        warmed: false,
        started: start,
        last_emitted_cursor: first.cursor.as_ref().map(cursor_key),
        latest_cursor: first.cursor.clone(),
        resumed_at: None,
        latest: Some(first),
        last_slot: Some(0),
        last_present: None,
        present_dt: Samples::default(),
        arrive_lag: Samples::default(),
        control: control_rx,
        capture: capture_rx,
        frames: frame_tx,
        trace: TraceBuf::new(Thread::Compose),
        traced_missed: 0,
    };
    let compose = std::thread::Builder::new()
        .name("snow-compose".into())
        .spawn({
            let failure = Arc::clone(&failure);
            move || record_failure(compose_loop(ctx), &failure)
        })
        .map_err(|e| e.to_string())?;
    Ok(Running {
        failure,
        control: control_tx,
        shared,
        capture: Some(capture_thread),
        compose: Some(compose),
        encode: Some(encode),
        backend: backend.to_string(),
        fps: config.fps,
        startup_ms,
        audio,
        _timer: timer,
    })
}

/// 保存后台首个错误，供主循环及时读取。
fn record_failure<T>(
    result: Result<T, String>,
    failure: &Mutex<Option<String>>,
) -> Result<T, String> {
    if let Err(e) = &result {
        let mut first = failure.lock().unwrap();
        if first.is_none() {
            *first = Some(e.clone());
        }
    }
    result
}

impl Running {
    /// 读取后台故障；线程崩溃也视为录制失败。
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.shared.failure.lock().unwrap().clone())
            .or_else(|| {
                (self.compose.as_ref().is_some_and(|t| t.is_finished())
                    || self.encode.as_ref().is_some_and(|t| t.is_finished()))
                .then(|| "录制后台线程意外退出".into())
            })
    }

    /// 装配时给的后端名。
    pub fn backend_name(&self) -> &str {
        &self.backend
    }

    /// 暂停录制（暂停期间的时间不计入时长）。
    pub fn pause(&self) {
        let now = Instant::now();
        self.shared.paused.store(true, Ordering::Release);
        if let Some(a) = &self.audio {
            a.pause(now);
        }
        let _ = self.control.send(Control::Pause(now));
    }

    /// 恢复录制。
    pub fn resume(&self) {
        let now = Instant::now();
        if let Some(a) = &self.audio {
            a.resume(now);
        }
        let _ = self.control.send(Control::Resume(now));
        self.shared.refresh.store(true, Ordering::Release);
        self.shared.paused.store(false, Ordering::Release);
    }

    /// 停止并写出文件（阻塞到封装完成）。
    ///
    /// # 返回
    /// 录制报告；合成或编码阶段出错返回原因。
    pub fn stop(mut self) -> Result<PipelineReport, String> {
        let now = Instant::now();
        // 音频先收尾：容器收尾（Finalize / 写尾）之前，音轨必须已写完
        let audio = self.audio.take().and_then(|a| a.stop(now));
        let _ = self.control.send(Control::Stop(now));
        let compose = self
            .compose
            .take()
            .ok_or("录制已结束")?
            .join()
            .map_err(|_| "合成线程崩溃".to_string())?;
        self.shared.stop.store(true, Ordering::Release);
        let diag = self
            .capture
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default();
        let encode = self
            .encode
            .take()
            .ok_or("录制已结束")?
            .join()
            .map_err(|_| "编码线程崩溃".to_string())?;
        // 各线程都已退出，追踪缓冲已并入全局记录（未开启追踪时什么也不做）
        frametrace::finish(self.fps, &self.backend);
        let mut report = compose?;
        report.audio = audio;
        let stats = encode?;
        report.encoder_frames = stats.frames;
        report.send_ms = Samples(stats.send_ms);
        report.backend = self.backend.clone();
        report.startup_ms = self.startup_ms;
        report.capture = CaptureStats {
            frames: self.shared.frames.load(Ordering::Relaxed),
            pool_drops: self.shared.pool_drops.load(Ordering::Relaxed),
            coalesced: self.shared.coalesced.load(Ordering::Relaxed),
        };
        report.capture_diag = diag
            .samples
            .into_iter()
            .map(|(n, v)| (n, Samples(v)))
            .collect();
        Ok(report)
    }

    /// 取消录制：终止各线程（输出文件由调用方清理）。
    pub fn cancel(mut self) {
        self.shutdown();
    }

    /// 通知各线程退出并等待（取消与丢弃共用）。
    fn shutdown(&mut self) {
        let _ = self.control.send(Control::Cancel);
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.capture.take() {
            let _ = t.join();
        }
        if let Some(t) = self.compose.take() {
            let _ = t.join();
        }
        if let Some(t) = self.encode.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Running {
    /// 未显式 stop/cancel 就被丢弃时按取消处理，保证线程退出。
    fn drop(&mut self) {
        if self.compose.is_some() || self.capture.is_some() {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// 模拟采集：按固定速率产生带序号的帧，呈现时刻就是计划时刻。
    struct MockCapture {
        /// 下一帧序号。
        seq: u64,
        /// 帧间隔。
        period: Duration,
        /// 起点。
        origin: Instant,
        /// 最多产生的帧数。
        limit: u64,
        /// 计数。
        stats: CaptureStats,
    }

    impl MockCapture {
        /// 创建以 `hz` 出帧、最多 `limit` 帧的模拟采集。
        fn new(hz: u64, limit: u64) -> Self {
            Self {
                seq: 0,
                period: Duration::from_nanos(NANOS_PER_SEC / hz),
                origin: Instant::now(),
                limit,
                stats: CaptureStats::default(),
            }
        }
    }

    impl CaptureSource for MockCapture {
        type Frame = u64;

        fn next(
            &mut self,
            timeout: Duration,
            want: bool,
        ) -> Result<Option<Captured<u64>>, CaptureFault> {
            let due = self.origin + self.period * self.seq as u32;
            let now = Instant::now();
            if self.seq >= self.limit {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                return Ok(None);
            }
            if due > now {
                std::thread::sleep((due - now).min(timeout));
                if Instant::now() < due {
                    return Ok(None);
                }
            }
            let seq = self.seq;
            self.seq += 1;
            if !want {
                return Ok(None);
            }
            self.stats.frames += 1;
            Ok(Some(Captured {
                frame: seq,
                cursor: None,
                present: due,
                captured_at: Instant::now(),
                fresh: true,
                id: seq + 1,
            }))
        }

        fn recreate(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn stats(&self) -> CaptureStats {
            self.stats
        }
    }

    /// 模拟合成：把采集帧序号写进表面；`fail_compose` 为真时合成失败。
    struct MockComposer {
        /// 是否让合成失败。
        fail_compose: bool,
        /// 在用表面计数（测试里不回收）。
        used: usize,
        /// 表面容量。
        capacity: usize,
    }

    impl FrameComposer for MockComposer {
        type Frame = u64;
        type Surface = u64;

        fn acquire_surface(&mut self) -> Result<Option<u64>, String> {
            if self.used >= self.capacity {
                return Ok(None);
            }
            self.used += 1;
            Ok(Some(0))
        }

        fn compose(
            &mut self,
            frame: &u64,
            _cursor: Option<&AttachedCursorSample>,
            surface: &mut u64,
        ) -> Result<(), String> {
            if self.fail_compose {
                return Err("模拟合成失败".into());
            }
            *surface = *frame;
            self.used = self.used.saturating_sub(1);
            Ok(())
        }
    }

    /// 模拟编码：记录 `(序号, pts)` 与结束终点。
    struct MockEncoder {
        /// 已送入的（序号, pts）。
        log: EmitLog,
        /// 结束终点。
        end: EndMark,
    }

    impl VideoEncoder for MockEncoder {
        type Surface = u64;

        fn submit(&mut self, surface: u64, pts: i64) -> Result<(), String> {
            self.log.lock().unwrap().push((surface, pts));
            Ok(())
        }

        fn finish(self, end_pts: i64) -> Result<EncoderStats, String> {
            *self.end.lock().unwrap() = Some(end_pts);
            let frames = self.log.lock().unwrap().len() as u64;
            Ok(EncoderStats {
                frames,
                send_ms: Vec::new(),
            })
        }
    }

    /// 模拟编码收到的（序号, pts）记录。
    type EmitLog = Arc<Mutex<Vec<(u64, i64)>>>;
    /// 模拟编码收到的结束终点。
    type EndMark = Arc<Mutex<Option<i64>>>;

    /// 装配一条模拟流水线。
    fn mock_pipeline(
        hz: u64,
        fps: u32,
        limit: u64,
        fail_compose: bool,
    ) -> (Result<Running, String>, EmitLog, EndMark) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let end = Arc::new(Mutex::new(None));
        let running = start(
            PipelineConfig {
                fps,
                show_cursor: false,
            },
            "mock",
            MockCapture::new(hz, limit),
            MockComposer {
                fail_compose,
                used: 0,
                capacity: 100,
            },
            MockEncoder {
                log: Arc::clone(&log),
                end: Arc::clone(&end),
            },
        );
        (running, log, end)
    }

    /// 消费者完全停滞时，桌面帧队列和光标信箱保持容量上限与最新值。
    #[test]
    fn stalled_capture_consumer_has_bounded_memory() {
        let (tx, rx) = mpsc::sync_channel(CAPTURE_CAPACITY);
        let cursor = Mutex::new(None);
        let now = Instant::now();
        for id in 0..10_000 {
            for fresh in [true, false] {
                assert!(forward_capture(
                    Captured {
                        frame: id,
                        cursor: None,
                        present: now,
                        captured_at: now,
                        fresh,
                        id,
                    },
                    &tx,
                    &cursor
                ));
            }
        }
        assert_eq!(rx.try_iter().count(), CAPTURE_CAPACITY);
        assert_eq!(cursor.lock().unwrap().take().unwrap().id, 9_999);
        drop(rx);
        assert!(!forward_capture(
            Captured {
                frame: 0,
                cursor: None,
                present: now,
                captured_at: now,
                fresh: true,
                id: 0,
            },
            &tx,
            &cursor
        ));
    }

    /// 编码失败立即结束，不等待 Finish；后台首个原因不会被后续故障覆盖。
    #[test]
    fn encoder_failure_is_reported_without_stop() {
        /// 固定失败的编码替身。
        struct Failing;
        impl VideoEncoder for Failing {
            type Surface = u64;
            /// 注入写帧错误。
            fn submit(&mut self, _: u64, _: i64) -> Result<(), String> {
                Err("injected encode".into())
            }
            /// 验证失败后不再收尾。
            fn finish(self, _: i64) -> Result<EncoderStats, String> {
                panic!("unexpected finish")
            }
        }
        let (tx, rx) = mpsc::channel();
        tx.send(EncodeMsg::Frame(1, 0)).unwrap();
        let failure = Mutex::new(None);
        assert!(record_failure(encode_loop(Failing, rx), &failure).is_err());
        assert_eq!(failure.lock().unwrap().as_deref(), Some("injected encode"));
        let _ = record_failure::<()>(Err("later compose failure".into()), &failure);
        assert_eq!(failure.lock().unwrap().as_deref(), Some("injected encode"));
    }

    /// 恢复请求先强制更新采集源，再调用 next；不依赖真实桌面或线程睡眠。
    #[test]
    fn resume_refreshes_capture_before_next_frame() {
        /// 单步采集替身，确认刷新顺序并立即结束循环。
        struct Resumed {
            shared: Arc<CaptureShared>,
            refreshed: bool,
        }
        impl CaptureSource for Resumed {
            type Frame = u64;
            /// 验证完整帧在刷新后才采集。
            fn next(
                &mut self,
                _: Duration,
                want: bool,
            ) -> Result<Option<Captured<u64>>, CaptureFault> {
                assert!(want && self.refreshed);
                self.shared.stop.store(true, Ordering::Release);
                let now = Instant::now();
                Ok(Some(Captured {
                    frame: 42,
                    cursor: None,
                    present: now,
                    captured_at: now,
                    fresh: true,
                    id: 1,
                }))
            }
            /// 强制刷新替身。
            fn recreate(&mut self) -> Result<(), String> {
                self.refreshed = true;
                Ok(())
            }
            /// 返回固定诊断。
            fn stats(&self) -> CaptureStats {
                CaptureStats::default()
            }
        }
        let shared = Arc::new(CaptureShared::default());
        shared.refresh.store(true, Ordering::Release);
        let (tx, rx) = mpsc::sync_channel(CAPTURE_CAPACITY);
        capture_loop(
            Resumed {
                shared: Arc::clone(&shared),
                refreshed: false,
            },
            tx,
            Arc::new(Mutex::new(None)),
            shared,
        );
        let CaptureMsg::Frame(frame) = rx.recv().unwrap() else {
            panic!("missing frame")
        };
        assert_eq!(frame.frame, 42);
    }

    /// 满队列仍立即保留采集错误，主控无须等待 stop 或 join。
    #[test]
    fn capture_failure_survives_a_full_channel() {
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.try_send(CaptureMsg::<u64>::Fault("occupied".into()))
            .unwrap();
        let shared = CaptureShared::default();
        capture_fault(&shared, &tx, "first capture failure".into());
        capture_fault(&shared, &tx, "later failure".into());
        assert_eq!(
            shared.failure.lock().unwrap().as_deref(),
            Some("first capture failure")
        );
    }

    /// 首帧之后的合成故障通过后台通道保留，无须用户发送停止命令。
    #[test]
    fn compose_failure_is_reported_without_stop() {
        let base = Instant::now() - Duration::from_secs(1);
        let (_control_tx, control) = mpsc::channel();
        let (capture_tx, capture) = mpsc::channel();
        let (frames, _frame_rx) = mpsc::channel();
        capture_tx
            .send(CaptureMsg::Frame(Box::new(Captured {
                frame: 2,
                cursor: None,
                present: base,
                captured_at: base,
                fresh: true,
                id: 2,
            })))
            .unwrap();
        let context = ComposeContext::<MockCapture, MockComposer, u64> {
            config: PipelineConfig {
                fps: 30,
                show_cursor: false,
            },
            composer: MockComposer {
                fail_compose: true,
                used: 0,
                capacity: 1,
            },
            timeline: Timeline::new(base, 30),
            ticks: TickClock::starting_at(1),
            queue: TimedQueue::default(),
            cursor: Arc::new(Mutex::new(None)),
            phase: PhaseTracker::new(Duration::from_millis(33)),
            hold: Duration::ZERO,
            warmed: true,
            started: base,
            resumed_at: None,
            latest: None,
            latest_cursor: None,
            last_emitted_cursor: None,
            last_slot: Some(0),
            last_present: None,
            present_dt: Samples::default(),
            arrive_lag: Samples::default(),
            control,
            capture,
            frames,
            trace: TraceBuf::new(Thread::Compose),
            traced_missed: 0,
        };
        let failure = Mutex::new(None);
        assert!(record_failure(compose_loop(context), &failure).is_err());
        assert_eq!(failure.lock().unwrap().as_deref(), Some("模拟合成失败"));
    }

    /// 用受控呈现时刻逐槽推进：每帧按序送出，PTS 严格递增且终点晚于末帧。
    #[test]
    fn mock_pipeline_emits_every_frame_in_order() {
        let base = Instant::now();
        let fps = 30;
        let (_control_tx, control) = mpsc::channel();
        let (_capture_tx, capture) = mpsc::channel();
        let (frames, frame_rx) = mpsc::channel();
        let mut context = ComposeContext::<MockCapture, MockComposer, u64> {
            config: PipelineConfig {
                fps,
                show_cursor: false,
            },
            composer: MockComposer {
                fail_compose: false,
                used: 0,
                capacity: 1,
            },
            timeline: Timeline::new(base, fps),
            ticks: TickClock::starting_at(0),
            queue: TimedQueue::default(),
            cursor: Arc::new(Mutex::new(None)),
            phase: PhaseTracker::new(slot_time(1, fps)),
            hold: Duration::ZERO,
            warmed: true,
            started: base,
            resumed_at: None,
            latest: None,
            latest_cursor: None,
            last_emitted_cursor: None,
            last_slot: None,
            last_present: None,
            present_dt: Samples::default(),
            arrive_lag: Samples::default(),
            control,
            capture,
            frames,
            trace: TraceBuf::new(Thread::Compose),
            traced_missed: 0,
        };
        let mut report = PipelineReport::default();
        for seq in 0..90 {
            let at = base + slot_time(seq, fps);
            absorb_frame(
                &mut context,
                Captured {
                    frame: seq,
                    cursor: None,
                    present: at,
                    captured_at: at,
                    fresh: true,
                    id: seq + 1,
                },
                false,
            );
            let slot = context.ticks.fire_next(seq).unwrap();
            emit_slot(&mut context, &mut report, slot).unwrap();
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let end = Arc::new(Mutex::new(None));
        let encoder = MockEncoder {
            log: Arc::clone(&log),
            end: Arc::clone(&end),
        };
        let endpoint = context.timeline.endpoint(base + Duration::from_secs(3));
        context
            .frames
            .send(EncodeMsg::Finish(endpoint as i64))
            .unwrap();
        let stats = encode_loop(encoder, frame_rx).unwrap();
        assert_eq!(stats.frames, 90);
        assert_eq!(report.encoded_frames, 90);
        let expected: Vec<_> = (0..90).map(|seq| (seq, seq as i64)).collect();
        assert_eq!(*log.lock().unwrap(), expected);
        assert_eq!(*end.lock().unwrap(), Some(90));
    }

    /// 源是输出的两倍速：输出约等于输出速率，且序号递增。
    #[test]
    fn mock_pipeline_thins_a_faster_source() {
        let (running, log, _) = mock_pipeline(60, 30, 100, false);
        let running = running.expect("start");
        std::thread::sleep(Duration::from_millis(1900));
        running.stop().expect("stop");
        let log = log.lock().unwrap();
        assert!((40..=56).contains(&log.len()), "emitted {}", log.len());
        assert!(log.windows(2).all(|w| w[1].1 > w[0].1 && w[1].0 > w[0].0));
    }

    /// 暂停期间的时间不计入：恢复后的 pts 紧接暂停前，不留下暂停时长的空洞。
    #[test]
    fn mock_pipeline_pause_does_not_advance_pts() {
        let (running, log, _) = mock_pipeline(60, 60, 400, false);
        let running = running.expect("start");
        std::thread::sleep(Duration::from_millis(700));
        running.pause();
        std::thread::sleep(Duration::from_millis(900));
        running.resume();
        std::thread::sleep(Duration::from_millis(700));
        running.stop().expect("stop");
        let log = log.lock().unwrap();
        let last_seq = log.last().unwrap().0;
        assert!(last_seq > 70, "last_seq {last_seq}");
        // 暂停 0.9s 对应约 54 个序号的空洞，但 pts 不应出现这么大的跳变
        let max_pts_jump = log.windows(2).map(|w| w[1].1 - w[0].1).max().unwrap();
        let max_seq_jump = log.windows(2).map(|w| w[1].0 - w[0].0).max().unwrap();
        assert!(max_seq_jump >= 30, "max_seq_jump {max_seq_jump}");
        assert!(max_pts_jump <= 12, "pts 跳变 {max_pts_jump}");
    }

    /// 帧追踪：槽选择、被取代丢弃、空槽、合成、送编码与跳槽都按捕获序号记录。
    #[test]
    fn trace_records_slot_selection_discards_and_compose() {
        use crate::frametrace::{Event, Tracer};
        let tracer = Arc::new(Tracer::new(std::path::PathBuf::from("unused.csv")));
        let base = Instant::now();
        let fps = 60;
        let (_control_tx, control) = mpsc::channel();
        let (_capture_tx, capture) = mpsc::channel();
        let (frames, _frame_rx) = mpsc::channel();
        let mut report = PipelineReport::default();
        {
            let mut ctx = ComposeContext::<MockCapture, MockComposer, u64> {
                config: PipelineConfig {
                    fps,
                    show_cursor: false,
                },
                composer: MockComposer {
                    fail_compose: false,
                    used: 0,
                    capacity: 100,
                },
                timeline: Timeline::new(base, fps),
                ticks: TickClock::starting_at(1),
                cursor: Arc::new(Mutex::new(None)),
                queue: TimedQueue::default(),
                phase: PhaseTracker::new(Duration::from_nanos(NANOS_PER_SEC / u64::from(fps))),
                hold: Duration::from_millis(20),
                warmed: true,
                started: base,
                resumed_at: None,
                latest: None,
                latest_cursor: None,
                last_emitted_cursor: None,
                last_slot: Some(0),
                last_present: None,
                present_dt: Samples::default(),
                arrive_lag: Samples::default(),
                control,
                capture,
                frames,
                trace: TraceBuf::attach(Some(Arc::clone(&tracer)), Thread::Compose),
                traced_missed: 0,
            };
            let frame = |id: u64, ms: u64| {
                let at = base + Duration::from_millis(ms);
                Captured {
                    frame: id,
                    cursor: None,
                    present: at,
                    captured_at: at,
                    fresh: true,
                    id,
                }
            };
            absorb_frame(&mut ctx, frame(1, 2), false);
            absorb_frame(&mut ctx, frame(2, 4), false);
            absorb_frame(&mut ctx, frame(3, 60), false);
            // 槽 1：帧 1、2 都早于切点，选 2 并丢弃 1；槽 2：帧 3 晚于切点，空槽
            emit_slot(&mut ctx, &mut report, 1).unwrap();
            emit_slot(&mut ctx, &mut report, 2).unwrap();
            // 节拍落后：直接触发槽 5 会跳过 4 个槽
            let late = ctx.ticks.fire(5).unwrap();
            emit_slot(&mut ctx, &mut report, late).unwrap();
            // 恢复后，暂停前积压帧与暂停期间帧都不能替换当前桌面。
            let resumed = base + Duration::from_millis(100);
            ctx.trace = TraceBuf::attach(None, Thread::Compose);
            ctx.resumed_at = Some(resumed);
            ctx.latest = None;
            absorb_frame(&mut ctx, frame(90, 20), false);
            absorb_frame(&mut ctx, frame(91, 80), false);
            assert!(ctx.latest.is_none());
            absorb_frame(&mut ctx, frame(92, 110), false);
            assert_eq!(ctx.latest.as_ref().unwrap().frame, 92);
        }
        let recs = tracer.collect();
        let of = |event: Event| recs.iter().filter(|r| r.event == event).collect::<Vec<_>>();
        assert_eq!(of(Event::Absorb).len(), 3);
        let discard = of(Event::Discard);
        assert_eq!(
            (
                discard.len(),
                discard[0].cap_id,
                discard[0].slot,
                discard[0].code
            ),
            (1, 1, 1, code::DISCARD_SUPERSEDED)
        );
        let slots: Vec<(i64, u64, u8)> = of(Event::Slot)
            .iter()
            .map(|r| (r.slot, r.cap_id, r.code))
            .collect();
        assert_eq!(
            slots,
            vec![
                (1, 2, code::SLOT_NEW),
                (2, 0, code::SLOT_EMPTY),
                (5, 3, code::SLOT_NEW)
            ]
        );
        let skip = of(Event::Skip);
        assert_eq!((skip.len(), skip[0].slot, skip[0].n), (1, 1, 4));
        assert_eq!(
            of(Event::Compose)
                .iter()
                .map(|r| (r.slot, r.cap_id))
                .collect::<Vec<_>>(),
            vec![(1, 2), (5, 3)]
        );
        assert_eq!(of(Event::Send).len(), 2);
        assert_eq!(report.encoded_frames, 2);
    }

    /// 首帧探测阶段合成失败：启动返回错误（调用方据此回落），不会留下运行中的线程。
    #[test]
    fn start_fails_when_the_probe_frame_cannot_be_composed() {
        let (running, log, _) = mock_pipeline(60, 60, 50, true);
        assert!(running.is_err());
        assert!(log.lock().unwrap().is_empty());
    }

    /// 取消：线程全部退出且不产生终点。
    #[test]
    fn cancel_joins_all_threads() {
        let (running, log, end) = mock_pipeline(60, 60, 400, false);
        let running = running.expect("start");
        std::thread::sleep(Duration::from_millis(200));
        running.cancel();
        // 取消走"终点 1"收尾，线程已全部 join，记录不再增长
        assert_eq!(*end.lock().unwrap(), Some(1));
        let count = log.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(log.lock().unwrap().len(), count);
    }

    /// 槽号到有效时长的换算与分位数、样本上限。
    #[test]
    fn helpers_behave() {
        assert_eq!(slot_time(30, 30), Duration::from_secs(1));
        assert_eq!(slot_time(5, 0), Duration::from_secs(5));
        let s = Samples(vec![5.0, 1.0, 3.0, 2.0, 4.0]);
        assert_eq!(
            (s.percentile(0.0), s.percentile(0.5), s.percentile(1.0)),
            (1.0, 3.0, 5.0)
        );
        assert_eq!(Samples::default().percentile(0.5), 0.0);
        let mut bounded = Samples::default();
        for i in 0..SAMPLE_LIMIT + 5 {
            bounded.push(i as f32);
        }
        assert_eq!(bounded.0.len(), SAMPLE_LIMIT);
    }
}
