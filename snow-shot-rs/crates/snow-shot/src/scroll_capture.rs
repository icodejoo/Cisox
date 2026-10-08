//! 长截图采集会话：采集线程 → 重复帧过滤 → 拼接适配层 → 分块保存 + 复制到剪贴板。
//!
//! 默认由用户手动滚动，采集线程按固定节拍读取选区像素；可选“自动滚动”通过 `PostMessage(WM_MOUSEWHEEL)`
//! 驱动目标窗口（在部分应用里无效，见 `snow_platform::scroll_input`）。
//! 帧来源、自动滚动与输出都是可注入的，因此整条流水线可以用合成滚动帧序列离线验证。

use crate::stitch_service::{
    FrameOutcome, RejectReason, StitchService, StitchStats, export_part_rows, frame_fingerprint,
    plan_parts,
};
use snow_i18n::{Args, I18n};
use snow_platform::capture::{CapturedScreen, capture_display};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// 采集节拍（约 10 帧 / 秒；拼接耗时会自然拉长实际间隔）。
pub const CAPTURE_INTERVAL: Duration = Duration::from_millis(100);
/// 自动滚动的投递间隔（与上游一致）。
pub const AUTO_SCROLL_INTERVAL: Duration = Duration::from_millis(200);
/// 自动滚动每次的滚轮增量（向下一格）。
pub const AUTO_SCROLL_DELTA: i32 = -120;
/// 自动滚动时连续多少次采集没有进展就认为已到底部。
pub const AUTO_STALL_FRAMES: u32 = 12;
/// 连续采集失败多少次后放弃。
pub const MAX_CAPTURE_ERRORS: u32 = 30;
/// 复制到剪贴板的像素字节数上限（超出只保存文件）。
pub const CLIPBOARD_MAX_BYTES: usize = 256 * 1024 * 1024;
/// 帧像素每像素字节数。
const BYTES_PER_PIXEL: usize = 4;

/// 帧来源（真实屏幕区域或测试用合成序列）。
pub trait FrameSource: Send {
    /// 取下一帧（BGRA，紧凑排列，尺寸恒定）。
    fn grab(&mut self) -> Result<CapturedScreen, String>;
}

/// 屏幕区域帧来源（GDI 读屏）。
pub struct ScreenSource {
    /// 区域 `(x, y, 宽, 高)`，虚拟桌面物理坐标。
    region: (i32, i32, u32, u32),
}

impl ScreenSource {
    /// 创建屏幕区域帧来源。
    ///
    /// # 参数
    /// - `region`：区域 `(x, y, 宽, 高)`，虚拟桌面物理坐标。
    pub fn new(region: (i32, i32, u32, u32)) -> Self {
        Self { region }
    }
}

impl FrameSource for ScreenSource {
    /// 读取区域像素。
    fn grab(&mut self) -> Result<CapturedScreen, String> {
        capture_display(Some(self.region))
    }
}

/// 输出通道（真实剪贴板 / 文件，或测试记录）。
pub trait ScrollSink: Send {
    /// 把 RGBA 图像写入剪贴板。
    fn copy_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String>;
    /// 把 RGBA 图像保存为 PNG，返回文件路径。
    fn save_png(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String>;
}

/// 用户界面线程与采集线程之间的控制开关。
#[derive(Debug, Default)]
pub struct ScrollControl {
    /// 用户点了“完成”。
    finish: AtomicBool,
    /// 用户点了“取消”。
    cancel: AtomicBool,
    /// 自动滚动开关。
    auto_scroll: AtomicBool,
}

impl ScrollControl {
    /// 请求完成并输出。
    pub fn request_finish(&self) {
        self.finish.store(true, Ordering::SeqCst);
    }

    /// 请求取消（丢弃已采集内容）。
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// 设置自动滚动开关。
    ///
    /// # 参数
    /// - `on`：是否开启。
    pub fn set_auto_scroll(&self, on: bool) {
        self.auto_scroll.store(on, Ordering::SeqCst);
    }

    /// 自动滚动当前是否开启。
    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll.load(Ordering::SeqCst)
    }
}

/// 没有复制到剪贴板的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopySkip {
    /// 设置里没启用复制。
    Disabled,
    /// 图像太大，只保存文件。
    TooLarge {
        /// 图像大小（MB）。
        megabytes: usize,
    },
}

impl CopySkip {
    /// 面向用户的原因说明。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::Disabled => i18n.tr("scroll-copy-disabled"),
            Self::TooLarge { megabytes } => i18n.tr_with(
                "scroll-copy-too-large",
                &Args::new().named("size", megabytes.to_string()),
            ),
        }
    }
}

/// 剪贴板复制结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyStatus {
    /// 已复制。
    Copied,
    /// 跳过（附原因，例如图太大）。
    Skipped(CopySkip),
    /// 复制失败（附系统给出的原因）。
    Failed(String),
}

/// 采集过程中需要提示用户的信息（界面边界再翻译）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollHint {
    /// 一帧被拒（滚太快、内容太少等）。
    Rejected(RejectReason),
    /// 到达高度上限，自动完成。
    LimitReached {
        /// 当前画布高度（像素）。
        height: u32,
    },
    /// 自动滚动投递失败。
    AutoScrollFailed(String),
    /// 当前环境不支持自动滚动。
    AutoScrollUnsupported,
    /// 读取屏幕失败。
    ReadFailed(String),
    /// 已滚动到底部，自动完成。
    ReachedBottom,
}

impl ScrollHint {
    /// 面向用户的提示。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        let detail = |id: &str, text: &str| i18n.tr_with(id, &Args::new().named("detail", text));
        match self {
            Self::Rejected(reason) => reason.message(i18n),
            Self::LimitReached { height } => i18n.tr_with(
                "scroll-hint-limit",
                &Args::new().named("height", height.to_string()),
            ),
            Self::AutoScrollFailed(e) => detail("scroll-hint-auto-failed", e),
            Self::AutoScrollUnsupported => i18n.tr("scroll-hint-auto-unsupported"),
            Self::ReadFailed(e) => detail("scroll-hint-read-failed", e),
            Self::ReachedBottom => i18n.tr("scroll-hint-bottom"),
        }
    }
}

/// 长截图失败的原因（界面边界再翻译，附带的技术细节原样保留）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollFailure {
    /// 拼接层报错。
    Stitch(String),
    /// 连续读取屏幕失败。
    CaptureFailed(String),
    /// 没有采集到任何内容。
    NothingCaptured,
    /// 导出 / 保存失败。
    Output(String),
}

impl ScrollFailure {
    /// 面向用户的失败说明。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        let detail = |id: &str, text: &str| i18n.tr_with(id, &Args::new().named("detail", text));
        match self {
            Self::Stitch(e) => detail("scroll-fail-stitch", e),
            Self::CaptureFailed(e) => detail("scroll-fail-capture", e),
            Self::NothingCaptured => i18n.tr("scroll-fail-nothing"),
            Self::Output(e) => detail("scroll-fail-output", e),
        }
    }
}

/// 一次长截图的最终产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollDone {
    /// 成图宽。
    pub width: u32,
    /// 成图高。
    pub height: u32,
    /// 保存的文件（超长图会分成多张）。
    pub files: Vec<PathBuf>,
    /// 剪贴板复制结果。
    pub copied: CopyStatus,
}

/// 采集会话所处阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollPhase {
    /// 采集中。
    Capturing,
    /// 正在输出（保存 / 复制）。
    Saving,
    /// 已完成。
    Done(ScrollDone),
    /// 失败（附原因）。
    Failed(ScrollFailure),
    /// 已取消。
    Cancelled,
}

/// 供界面读取的进度快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollProgress {
    /// 阶段。
    pub phase: ScrollPhase,
    /// 送入拼接的帧数（不含被指纹过滤的重复帧）。
    pub frames: u32,
    /// 当前画布宽。
    pub width: u32,
    /// 当前画布高。
    pub height: u32,
    /// 需要提示用户的信息（滚太快 / 内容太少等）。
    pub hint: Option<ScrollHint>,
    /// 拼接统计。
    pub stats: StitchStats,
}

impl ScrollProgress {
    /// 初始快照。
    pub fn new() -> Self {
        Self {
            phase: ScrollPhase::Capturing,
            frames: 0,
            width: 0,
            height: 0,
            hint: None,
            stats: StitchStats::default(),
        }
    }
}

impl Default for ScrollProgress {
    /// 同 [`ScrollProgress::new`]。
    fn default() -> Self {
        Self::new()
    }
}

/// 共享进度。
pub type SharedProgress = Arc<Mutex<ScrollProgress>>;

/// 加锁并忽略中毒。
fn lock(progress: &Mutex<ScrollProgress>) -> MutexGuard<'_, ScrollProgress> {
    progress.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 自动滚动回调（返回错误表示投递失败）。
pub type AutoScroller = Box<dyn FnMut() -> Result<(), String> + Send>;

/// 采集选项（输出方式与节拍；测试可缩短间隔）。
#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// 完成时是否复制到剪贴板。
    pub copy_to_clipboard: bool,
    /// 节拍参数。
    pub timing: CaptureTiming,
}

/// 采集线程的节拍参数。
#[derive(Debug, Clone, Copy)]
pub struct CaptureTiming {
    /// 采集间隔。
    pub capture_interval: Duration,
    /// 自动滚动间隔。
    pub auto_interval: Duration,
}

impl Default for CaptureTiming {
    /// 生产参数。
    fn default() -> Self {
        Self {
            capture_interval: CAPTURE_INTERVAL,
            auto_interval: AUTO_SCROLL_INTERVAL,
        }
    }
}

/// 把一帧结果翻译成给用户的提示（无需提示返回 `None`）。
///
/// # 参数
/// - `outcome`：帧结果。
pub fn hint_for(outcome: &FrameOutcome) -> Option<ScrollHint> {
    match outcome {
        FrameOutcome::Rejected(reason) => Some(ScrollHint::Rejected(*reason)),
        FrameOutcome::LimitReached { height } => Some(ScrollHint::LimitReached { height: *height }),
        _ => None,
    }
}

/// 采集主循环（在采集线程里运行，阻塞到完成 / 取消 / 失败）。
///
/// # 参数
/// - `source`：帧来源。
/// - `control`：控制开关。
/// - `progress`：共享进度（界面读取）。
/// - `wake`：进度变化时唤醒界面。
/// - `auto_scroller`：自动滚动回调（无则不支持自动滚动）。
/// - `sink`：输出通道。
/// - `options`：输出方式与节拍。
pub fn run_capture(
    mut source: impl FrameSource,
    control: &ScrollControl,
    progress: &SharedProgress,
    wake: &dyn Fn(),
    mut auto_scroller: Option<AutoScroller>,
    sink: &mut dyn ScrollSink,
    options: CaptureOptions,
) {
    let timing = options.timing;
    let copy_to_clipboard = options.copy_to_clipboard;
    let mut service = StitchService::new();
    let mut last_fingerprint: Option<u64> = None;
    let mut last_auto = Instant::now();
    let mut stalled = 0u32;
    let mut errors = 0u32;
    let mut auto_finished = false;
    loop {
        if control.cancel.load(Ordering::SeqCst) {
            set_phase(progress, ScrollPhase::Cancelled, wake);
            return;
        }
        if control.finish.load(Ordering::SeqCst) {
            break;
        }
        let auto_on = control.auto_scroll();
        if auto_on && last_auto.elapsed() >= timing.auto_interval {
            last_auto = Instant::now();
            match auto_scroller.as_mut() {
                Some(scroll) => {
                    if let Err(e) = scroll() {
                        control.set_auto_scroll(false);
                        set_hint(progress, Some(ScrollHint::AutoScrollFailed(e)), wake);
                    }
                }
                None => {
                    control.set_auto_scroll(false);
                    set_hint(progress, Some(ScrollHint::AutoScrollUnsupported), wake);
                }
            }
        }
        match source.grab() {
            Ok(screen) => {
                errors = 0;
                let fingerprint = frame_fingerprint(&screen.data);
                if last_fingerprint == Some(fingerprint) {
                    stalled += 1;
                } else {
                    last_fingerprint = Some(fingerprint);
                    match service.push_captured(screen) {
                        Ok(outcome) => {
                            stalled = if outcome.is_progress() {
                                0
                            } else {
                                stalled + 1
                            };
                            update_progress(progress, &service, &outcome, wake);
                            if matches!(outcome, FrameOutcome::LimitReached { .. }) {
                                break;
                            }
                        }
                        Err(e) => {
                            set_phase(
                                progress,
                                ScrollPhase::Failed(ScrollFailure::Stitch(e)),
                                wake,
                            );
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                errors += 1;
                set_hint(progress, Some(ScrollHint::ReadFailed(e.clone())), wake);
                if errors >= MAX_CAPTURE_ERRORS {
                    set_phase(
                        progress,
                        ScrollPhase::Failed(ScrollFailure::CaptureFailed(e)),
                        wake,
                    );
                    return;
                }
            }
        }
        if auto_on
            && stalled >= AUTO_STALL_FRAMES
            && service.stats().appended + service.stats().prepended > 0
        {
            auto_finished = true;
            break;
        }
        std::thread::sleep(timing.capture_interval);
    }
    if auto_finished {
        set_hint(progress, Some(ScrollHint::ReachedBottom), wake);
    }
    set_phase(progress, ScrollPhase::Saving, wake);
    let phase = match finalize(&service, sink, copy_to_clipboard) {
        Ok(done) => ScrollPhase::Done(done),
        Err(e) => ScrollPhase::Failed(e),
    };
    set_phase(progress, phase, wake);
}

/// 输出：分块保存 PNG，并在大小允许时复制整图到剪贴板。
///
/// # 参数
/// - `service`：已拼接的服务。
/// - `sink`：输出通道。
/// - `copy_to_clipboard`：是否复制。
///
/// # 返回
/// 产出信息；没有任何内容或保存失败返回错误。
pub fn finalize(
    service: &StitchService,
    sink: &mut dyn ScrollSink,
    copy_to_clipboard: bool,
) -> Result<ScrollDone, ScrollFailure> {
    finalize_with_part_rows(
        service,
        sink,
        copy_to_clipboard,
        export_part_rows(service.width()),
    )
}

/// 同 [`finalize`]，但可指定分块行数（测试用小值验证超长图分块）。
///
/// # 参数
/// - `service` / `sink` / `copy_to_clipboard`：同 [`finalize`]。
/// - `part_rows`：每块最大行数。
pub fn finalize_with_part_rows(
    service: &StitchService,
    sink: &mut dyn ScrollSink,
    copy_to_clipboard: bool,
    part_rows: u32,
) -> Result<ScrollDone, ScrollFailure> {
    let (width, height) = (service.width(), service.height());
    if service.is_empty() || height == 0 {
        return Err(ScrollFailure::NothingCaptured);
    }
    let mut files = Vec::new();
    for (top, rows) in plan_parts(height, part_rows) {
        let rgba = service
            .export_rows(top, rows)
            .map_err(ScrollFailure::Output)?;
        files.push(
            sink.save_png(width, rows, &rgba)
                .map_err(ScrollFailure::Output)?,
        );
    }
    let bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(BYTES_PER_PIXEL);
    let copied = if !copy_to_clipboard {
        CopyStatus::Skipped(CopySkip::Disabled)
    } else if bytes > CLIPBOARD_MAX_BYTES {
        CopyStatus::Skipped(CopySkip::TooLarge {
            megabytes: bytes / (1024 * 1024),
        })
    } else {
        match service.export_all_rgba() {
            Ok((w, h, rgba)) => match sink.copy_image(w, h, &rgba) {
                Ok(()) => CopyStatus::Copied,
                Err(e) => CopyStatus::Failed(e),
            },
            Err(e) => CopyStatus::Failed(e),
        }
    };
    Ok(ScrollDone {
        width,
        height,
        files,
        copied,
    })
}

/// 更新阶段并唤醒界面。
fn set_phase(progress: &SharedProgress, phase: ScrollPhase, wake: &dyn Fn()) {
    lock(progress).phase = phase;
    wake();
}

/// 更新提示并唤醒界面。
fn set_hint(progress: &SharedProgress, hint: Option<ScrollHint>, wake: &dyn Fn()) {
    let mut guard = lock(progress);
    if guard.hint != hint {
        guard.hint = hint;
        drop(guard);
        wake();
    }
}

/// 一帧拼入（或被拒）后更新进度快照并唤醒界面。
fn update_progress(
    progress: &SharedProgress,
    service: &StitchService,
    outcome: &FrameOutcome,
    wake: &dyn Fn(),
) {
    {
        let mut guard = lock(progress);
        guard.frames += 1;
        guard.width = service.width();
        guard.height = service.height();
        guard.stats = service.stats();
        guard.hint = hint_for(outcome).or_else(|| service.attention().map(ScrollHint::Rejected));
    }
    wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// 测试文档像素（BGRA，哈希噪声）。
    fn doc_pixel(x: u32, y: u32) -> [u8; 4] {
        let mut hash = x.wrapping_mul(0xc2b2_ae35) ^ y.wrapping_mul(0x27d4_eb2d);
        hash ^= hash >> 16;
        hash = hash.wrapping_mul(0x7feb_352d);
        hash ^= hash >> 15;
        [
            (hash >> 24) as u8,
            (hash >> 16) as u8,
            (hash >> 8) as u8,
            255,
        ]
    }

    /// 截取文档第 `scroll` 行起的一帧。
    fn frame(w: u32, h: u32, scroll: u32) -> CapturedScreen {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&doc_pixel(x, y + scroll));
            }
        }
        CapturedScreen {
            width: w,
            height: h,
            data,
        }
    }

    /// 合成滚动来源：按脚本依次给出滚动位置，脚本用完后请求完成（或保持最后一帧）。
    struct ScriptSource {
        /// 帧宽。
        w: u32,
        /// 帧高。
        h: u32,
        /// 滚动位置脚本。
        script: Vec<u32>,
        /// 下一个脚本下标。
        next: usize,
        /// 用完后是否自动请求完成。
        finish_at_end: Option<Arc<ScrollControl>>,
        /// 抓取次数计数（自动滚动测试用）。
        grabs: Arc<AtomicUsize>,
    }

    impl FrameSource for ScriptSource {
        /// 给出脚本里的下一帧；脚本用完后重复最后一帧并按需请求完成。
        fn grab(&mut self) -> Result<CapturedScreen, String> {
            self.grabs.fetch_add(1, Ordering::SeqCst);
            let index = self.next.min(self.script.len().saturating_sub(1));
            if self.next >= self.script.len()
                && let Some(control) = &self.finish_at_end
            {
                control.request_finish();
            }
            self.next += 1;
            Ok(frame(self.w, self.h, self.script[index]))
        }
    }

    /// 记录型输出。
    #[derive(Default)]
    struct RecordingSink {
        /// 保存的图像 `(宽, 高, RGBA)`。
        saved: Vec<(u32, u32, Vec<u8>)>,
        /// 复制的图像。
        copied: Vec<(u32, u32, Vec<u8>)>,
        /// 复制是否失败。
        fail_copy: bool,
    }

    impl ScrollSink for RecordingSink {
        /// 记录复制。
        fn copy_image(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
            if self.fail_copy {
                return Err("剪贴板被占用".into());
            }
            self.copied.push((w, h, rgba.to_vec()));
            Ok(())
        }
        /// 记录保存。
        fn save_png(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<PathBuf, String> {
            self.saved.push((w, h, rgba.to_vec()));
            Ok(PathBuf::from(format!("part{}.png", self.saved.len())))
        }
    }

    /// 快节拍（测试用）。
    fn fast() -> CaptureTiming {
        CaptureTiming {
            capture_interval: Duration::from_millis(1),
            auto_interval: Duration::from_millis(2),
        }
    }

    /// 期望的 RGBA 文档前缀（doc_pixel 是 BGRA 顺序，转成 RGBA）。
    fn expected_rgba(w: u32, rows: u32, top: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..rows {
            for x in 0..w {
                let p = doc_pixel(x, y + top);
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        out
    }

    /// 跑一遍采集，返回最终进度、输出记录与唤醒次数。
    fn run(
        script: Vec<u32>,
        w: u32,
        h: u32,
        auto: Option<AutoScroller>,
        auto_on: bool,
        fail_copy: bool,
    ) -> (ScrollProgress, RecordingSink, usize) {
        let control = Arc::new(ScrollControl::default());
        control.set_auto_scroll(auto_on);
        let source = ScriptSource {
            w,
            h,
            script,
            next: 0,
            finish_at_end: if auto_on {
                None
            } else {
                Some(Arc::clone(&control))
            },
            grabs: Arc::new(AtomicUsize::new(0)),
        };
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let wakes = AtomicUsize::new(0);
        let mut sink = RecordingSink {
            fail_copy,
            ..RecordingSink::default()
        };
        run_capture(
            source,
            &control,
            &progress,
            &|| {
                wakes.fetch_add(1, Ordering::SeqCst);
            },
            auto,
            &mut sink,
            CaptureOptions {
                copy_to_clipboard: true,
                timing: fast(),
            },
        );
        let snapshot = lock(&progress).clone();
        (snapshot, sink, wakes.load(Ordering::SeqCst))
    }

    /// 慢滚序列（含重复帧）：完成后成图逐字节等于文档前缀，剪贴板与保存内容一致，重复帧不入拼接。
    #[test]
    fn manual_scroll_sequence_produces_exact_image() {
        let (w, h) = (320, 200);
        let (progress, sink, wakes) =
            run(vec![0, 0, 60, 60, 60, 120, 180], w, h, None, false, false);
        let ScrollPhase::Done(done) = &progress.phase else {
            panic!("应完成: {:?}", progress.phase);
        };
        assert_eq!((done.width, done.height), (w, h + 180));
        assert_eq!(done.copied, CopyStatus::Copied);
        assert_eq!(sink.saved.len(), 1);
        assert_eq!(sink.saved[0].2, expected_rgba(w, h + 180, 0));
        assert_eq!(sink.copied[0].2, sink.saved[0].2);
        assert_eq!(progress.frames, 4, "重复帧应在送入拼接前被指纹过滤掉");
        assert!(wakes > 0);
    }

    /// 完全没有滚动（一直是同一帧）：只有首帧，输出与首帧相同，没有多余内容。
    #[test]
    fn static_content_yields_single_frame_image() {
        let (progress, sink, _) = run(vec![0; 10], 320, 200, None, false, false);
        let ScrollPhase::Done(done) = &progress.phase else {
            panic!("应完成: {:?}", progress.phase);
        };
        assert_eq!(done.height, 200);
        assert_eq!(progress.frames, 1);
        assert_eq!(sink.saved[0].2, expected_rgba(320, 200, 0));
    }

    /// 滚太快（相邻位移超过 60%）：帧被拒并给出提示，成图不产生错位内容。
    #[test]
    fn too_fast_scrolling_is_reported_and_not_misplaced() {
        let (w, h) = (320, 200);
        let (progress, sink, _) = run(vec![0, 150, 300, 450], w, h, None, false, false);
        assert!(
            matches!(progress.phase, ScrollPhase::Done(_)),
            "{:?}",
            progress.phase
        );
        assert!(progress.stats.rejected >= 1, "{:?}", progress.stats);
        assert!(progress.hint.is_some());
        // 成图必须是文档前缀
        let (sw, sh, data) = &sink.saved[0];
        assert_eq!(*sw, w);
        assert_eq!(*data, expected_rgba(w, *sh, 0));
    }

    /// 自动滚动：回调被调用；到底（内容不再变化）后自动完成。
    #[test]
    fn auto_scroll_finishes_at_bottom() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let scroller: AutoScroller = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        // 滚动 3 步后内容不再变化（到底）
        let (progress, sink, _) = run(vec![0, 60, 120, 120], 320, 200, Some(scroller), true, false);
        assert!(
            matches!(progress.phase, ScrollPhase::Done(_)),
            "{:?}",
            progress.phase
        );
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert_eq!(sink.saved[0].1, 200 + 120);
        assert_eq!(progress.hint, Some(ScrollHint::ReachedBottom));
    }

    /// 自动滚动投递失败：关闭自动滚动并提示改为手动，不崩溃。
    #[test]
    fn auto_scroll_failure_falls_back_to_manual() {
        let scroller: AutoScroller = Box::new(|| Err("目标不响应".to_string()));
        let control = Arc::new(ScrollControl::default());
        control.set_auto_scroll(true);
        let source = ScriptSource {
            w: 320,
            h: 200,
            script: vec![0, 60],
            next: 0,
            finish_at_end: None,
            grabs: Arc::new(AtomicUsize::new(0)),
        };
        let finisher = Arc::clone(&control);
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let mut sink = RecordingSink::default();
        let watch = Arc::clone(&progress);
        // 另一个线程在看到提示后请求完成
        let helper = std::thread::spawn(move || {
            for _ in 0..500 {
                if matches!(lock(&watch).hint, Some(ScrollHint::AutoScrollFailed(_))) {
                    finisher.request_finish();
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            finisher.request_finish();
            false
        });
        run_capture(
            source,
            &control,
            &progress,
            &|| {},
            Some(scroller),
            &mut sink,
            CaptureOptions {
                copy_to_clipboard: true,
                timing: fast(),
            },
        );
        assert!(helper.join().expect("辅助线程"));
        assert!(!control.auto_scroll());
        assert!(matches!(lock(&progress).phase, ScrollPhase::Done(_)));
    }

    /// 取消：不输出任何东西。
    #[test]
    fn cancel_discards_everything() {
        let control = ScrollControl::default();
        control.request_cancel();
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let source = ScriptSource {
            w: 64,
            h: 64,
            script: vec![0],
            next: 0,
            finish_at_end: None,
            grabs: Arc::new(AtomicUsize::new(0)),
        };
        let mut sink = RecordingSink::default();
        run_capture(
            source,
            &control,
            &progress,
            &|| {},
            None,
            &mut sink,
            CaptureOptions {
                copy_to_clipboard: true,
                timing: fast(),
            },
        );
        assert_eq!(lock(&progress).phase, ScrollPhase::Cancelled);
        assert!(sink.saved.is_empty() && sink.copied.is_empty());
    }

    /// 高度上限：到限自动完成并提示，成图不超限。
    #[test]
    fn height_cap_ends_session_with_hint() {
        // 帧高 800、每步 240；上限 32768 需要 130+ 帧，这里直接验证 finalize 对超长图的分块
        let mut service = StitchService::with_max_height(2000);
        let (w, h) = (320, 800);
        for i in 0..20u32 {
            let screen = frame(w, h, i * 240);
            service.push_captured(screen).expect("push");
        }
        assert!(service.height() <= 2000);
        let mut sink = RecordingSink::default();
        let done = finalize(&service, &mut sink, true).expect("输出");
        assert_eq!(done.height, service.height());
        assert!(!done.files.is_empty());
        let limit = hint_for(&FrameOutcome::LimitReached { height: 1520 }).expect("高度上限提示");
        assert_eq!(
            limit.message(crate::ocr_backend::i18n_for("zh-CN")),
            "已达到高度上限（1520 像素），自动完成。"
        );
        assert!(
            limit
                .message(crate::ocr_backend::i18n_for("en-US"))
                .contains("1520")
        );
    }

    /// 超长图分块保存：每块不超过分块行数，拼起来等于整图；剪贴板复制失败时只标记失败。
    #[test]
    fn finalize_splits_tall_images_and_reports_copy_failure() {
        let mut service = StitchService::new();
        let (w, h) = (320, 200);
        for i in 0..5u32 {
            service.push_captured(frame(w, h, i * 60)).expect("push");
        }
        assert_eq!(service.height(), 200 + 240);
        let mut sink = RecordingSink {
            fail_copy: true,
            ..RecordingSink::default()
        };
        let done = finalize(&service, &mut sink, true).expect("输出");
        assert_eq!(done.copied, CopyStatus::Failed("剪贴板被占用".into()));
        assert_eq!(done.files.len(), 1);
        // 小分块：440 行按 128 行一块 -> 4 张，拼起来等于整图
        let mut parted = RecordingSink::default();
        let done = finalize_with_part_rows(&service, &mut parted, false, 128).expect("分块输出");
        assert_eq!(done.files.len(), 4);
        assert!(
            parted
                .saved
                .iter()
                .all(|(sw, sh, _)| *sw == w && *sh <= 128)
        );
        let joined: Vec<u8> = parted
            .saved
            .iter()
            .flat_map(|(_, _, d)| d.iter().copied())
            .collect();
        assert_eq!(joined, expected_rgba(w, 440, 0));
        let mut skipped = RecordingSink::default();
        let done = finalize(&service, &mut skipped, false).expect("输出");
        assert!(matches!(done.copied, CopyStatus::Skipped(_)));
        assert!(skipped.copied.is_empty());
        assert!(finalize(&StitchService::new(), &mut skipped, true).is_err());
    }

    /// 读屏连续失败：超过阈值后进入失败态并给出原因。
    #[test]
    fn capture_errors_fail_the_session() {
        struct Broken;
        impl FrameSource for Broken {
            /// 永远失败。
            fn grab(&mut self) -> Result<CapturedScreen, String> {
                Err("拒绝访问".into())
            }
        }
        let control = ScrollControl::default();
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let mut sink = RecordingSink::default();
        run_capture(
            Broken,
            &control,
            &progress,
            &|| {},
            None,
            &mut sink,
            CaptureOptions {
                copy_to_clipboard: true,
                timing: fast(),
            },
        );
        assert!(
            matches!(&lock(&progress).phase, ScrollPhase::Failed(ScrollFailure::CaptureFailed(e)) if e.contains("拒绝访问"))
        );
    }

    /// 提示翻译：被拒给出原因文案，普通帧无提示。
    #[test]
    fn hints_only_for_problems() {
        assert!(
            hint_for(&FrameOutcome::Rejected(RejectReason::TooFast {
                offset: 500
            }))
            .is_some_and(|h| h
                .message(crate::ocr_backend::i18n_for("zh-CN"))
                .contains("太快"))
        );
        assert!(hint_for(&FrameOutcome::Duplicate).is_none());
        assert!(
            hint_for(&FrameOutcome::Appended {
                growth: 1,
                height: 2,
                offset: -1
            })
            .is_none()
        );
    }
}
