//! 视频编辑任务（worker 进程内）。
//!
//! 设计见 `docs/research/video-editor-mvp-design.md`：编辑任务并入 `snow-recorder`，
//! 通过 [`EditEngine`] 抽象出两个引擎：FFmpeg 引擎与系统引擎（`system`，Media Foundation + WIC）。
//! FFmpeg 引擎：探测（`probe`）、按时间戳精确 seek（`source`）、抽帧（`extract`）、
//! 降 fps / 缩放（`transcode`，H.264 重编码）、关键帧裁剪（`trim`，包级拷贝）；
//! 系统引擎：探测、精确 seek、PNG/JPEG 抽帧、硬件 H.264 降 fps / 缩放；
//! 无损 WebP 与无重编码裁剪它做不了，能力里标不可用。音频一律直通不重编码。
//! `Auto`：系统引擎优先，能力不支持或初始化失败时回落 FFmpeg；显式选系统引擎不支持时报明确错误。

pub mod extract;
pub mod image;
pub mod source;
pub mod system;
pub mod transcode;
pub mod trim;
pub mod yuv;

#[cfg(test)]
pub mod testclip;

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use snow_recorder_protocol::{EditOp, EditRequest, EngineKind, Event, ImageFormat, ProbeInfo};

pub use system::SystemEngine;

/// 进度事件的最小间隔（限频到每秒约 10 次）。
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// 编辑任务错误（单行可读文本）。
#[derive(Debug, Clone)]
pub struct EditError {
    /// 错误描述。
    message: String,
    /// 是否由用户取消造成。
    cancelled: bool,
    /// 是否属于"该引擎做不了 / 没能启动"（尚未产出任何结果，`Auto` 可回落到别的引擎）。
    fallback: bool,
}

impl EditError {
    /// 创建普通错误。
    ///
    /// # 参数
    /// - `message`：错误描述。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancelled: false,
            fallback: false,
        }
    }

    /// 创建"引擎不支持 / 启动失败"错误：尚未写出任何结果，`Auto` 会据此回落。
    ///
    /// # 参数
    /// - `message`：错误描述。
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancelled: false,
            fallback: true,
        }
    }

    /// 创建"已取消"错误。
    pub fn cancelled() -> Self {
        Self {
            message: "任务已取消".to_string(),
            cancelled: true,
            fallback: false,
        }
    }

    /// 是否允许 `Auto` 回落到别的引擎（引擎不支持或启动失败，且没有产出结果）。
    pub fn can_fallback(&self) -> bool {
        self.fallback && !self.cancelled
    }

    /// 是否由取消造成。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

impl fmt::Display for EditError {
    /// 输出错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EditError {}

/// 任务控制：取消标志与限频进度上报。
pub struct TaskCtl {
    /// 取消标志（与命令循环共享）。
    cancelled: Arc<AtomicBool>,
    /// 事件出口。
    emit: Box<dyn Fn(&Event) + Send + Sync>,
    /// 上次上报时间。
    last: Mutex<Option<Instant>>,
}

impl TaskCtl {
    /// 创建任务控制。
    ///
    /// # 参数
    /// - `cancelled`：取消标志，置位表示用户取消。
    /// - `emit`：事件出口（生产环境写 stdout，测试可捕获）。
    pub fn new(cancelled: Arc<AtomicBool>, emit: Box<dyn Fn(&Event) + Send + Sync>) -> Self {
        Self {
            cancelled,
            emit,
            last: Mutex::new(None),
        }
    }

    /// 任务是否已被取消；引擎应在帧循环里轮询。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 上报进度；超出限频的中间进度会被丢弃，完成那一次一定发出。
    ///
    /// # 参数
    /// - `done`：已完成数量。
    /// - `total`：总数，未知为 0。
    /// - `stage`：阶段标识（英文，由主程序翻译）。
    pub fn report(&self, done: u64, total: u64, stage: &str) {
        let finished = total > 0 && done >= total;
        if !finished {
            let Ok(mut last) = self.last.lock() else {
                return;
            };
            let now = Instant::now();
            if last.is_some_and(|t| now.duration_since(t) < REPORT_INTERVAL) {
                return;
            }
            *last = Some(now);
        }
        (self.emit)(&Event::EditProgress {
            done,
            total,
            stage: stage.to_string(),
        });
    }
}

/// 某引擎对某项操作的支持情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpSupport {
    /// 可用。
    Available,
    /// 不可用，附原因（供 UI 置灰文案使用）。
    Unavailable(String),
}

/// 引擎能力：按操作给出可用性。
#[derive(Debug, Clone)]
pub struct EngineCaps {
    /// 降 fps。
    pub reduce_fps: OpSupport,
    /// 缩放。
    pub scale: OpSupport,
    /// 抽帧（PNG / JPEG）。
    pub extract_frames: OpSupport,
    /// 抽帧导出无损 WebP（需要同时满足 `extract_frames`）。
    pub extract_webp_lossless: OpSupport,
    /// 关键帧裁剪。
    pub trim_keyframe: OpSupport,
}

impl EngineCaps {
    /// 查询某项操作的支持情况。
    ///
    /// # 参数
    /// - `op`：编辑操作。
    pub fn support(&self, op: &EditOp) -> &OpSupport {
        match op {
            EditOp::ReduceFps { .. } => &self.reduce_fps,
            EditOp::Scale { .. } => &self.scale,
            EditOp::ExtractFrames { format, .. } => {
                if self.extract_frames != OpSupport::Available
                    || format != &ImageFormat::WebpLossless
                {
                    &self.extract_frames
                } else {
                    &self.extract_webp_lossless
                }
            }
            EditOp::TrimKeyframe { .. } => &self.trim_keyframe,
        }
    }
}

/// 一次编辑任务的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditReport {
    /// 输出路径（抽帧为目录）。
    pub path: std::path::PathBuf,
    /// 输出帧（张）数。
    pub frames: u64,
}

/// 编辑引擎抽象（设计文档 §5）。
pub trait EditEngine: Send {
    /// 引擎种类（具体引擎，不会是 `Auto`）。
    fn kind(&self) -> EngineKind;

    /// 探测输入视频（时长、分辨率、帧率、帧数、关键帧数）。
    ///
    /// # 参数
    /// - `input`：输入视频路径。
    fn probe(&self, input: &Path) -> Result<ProbeInfo, EditError>;

    /// 对给定输入，各项操作是否可用。
    ///
    /// # 参数
    /// - `input`：输入视频的探测信息。
    fn capabilities(&self, input: &ProbeInfo) -> EngineCaps;

    /// 执行任务；应在帧循环里轮询 `ctl.is_cancelled()`，取消时返回 [`EditError::cancelled`]。
    ///
    /// # 参数
    /// - `req`：编辑请求。
    /// - `ctl`：进度与取消控制。
    fn run(&mut self, req: &EditRequest, ctl: &TaskCtl) -> Result<EditReport, EditError>;
}

/// FFmpeg 引擎（软件解码 + 系统 WIC / libwebp 图片编码）。
#[derive(Debug, Default)]
pub struct FfmpegEngine;

/// 缺少 H.264 编码器时的不可用原因。
const NO_H264_ENCODER: &str = "FFmpeg 缺少 libx264 编码器";
/// 缺少 libwebp 编码器时的不可用原因。
const NO_WEBP_ENCODER: &str = "FFmpeg 缺少 libwebp 编码器";

impl EditEngine for FfmpegEngine {
    /// 见 trait。
    fn kind(&self) -> EngineKind {
        EngineKind::Ffmpeg
    }

    /// 见 trait。
    fn probe(&self, input: &Path) -> Result<ProbeInfo, EditError> {
        probe_ffmpeg(input)
    }

    /// 抽帧与关键帧裁剪总是可用；降 fps / 缩放需要 libx264，无损 WebP 需要 libwebp
    /// （具体参数合法性在 `run` 里校验）。
    fn capabilities(&self, _input: &ProbeInfo) -> EngineCaps {
        let reencode = if ffmpeg_next::init().is_ok()
            && ffmpeg_next::encoder::find_by_name("libx264").is_some()
        {
            OpSupport::Available
        } else {
            OpSupport::Unavailable(NO_H264_ENCODER.to_string())
        };
        let webp = if ffmpeg_next::encoder::find_by_name("libwebp_anim").is_some() {
            OpSupport::Available
        } else {
            OpSupport::Unavailable(NO_WEBP_ENCODER.to_string())
        };
        EngineCaps {
            reduce_fps: reencode.clone(),
            scale: reencode,
            extract_frames: OpSupport::Available,
            extract_webp_lossless: webp,
            trim_keyframe: OpSupport::Available,
        }
    }

    /// 见 trait。
    fn run(&mut self, req: &EditRequest, ctl: &TaskCtl) -> Result<EditReport, EditError> {
        match req.op {
            EditOp::ExtractFrames {
                mode,
                format,
                quality,
            } => {
                let frames = extract::run(
                    &extract::ExtractParams {
                        input: &req.input,
                        out_dir: &req.output,
                        mode,
                        format,
                        quality,
                    },
                    ctl,
                )?;
                Ok(EditReport {
                    path: req.output.clone(),
                    frames,
                })
            }
            EditOp::ReduceFps { .. } | EditOp::Scale { .. } => {
                let frames = transcode::run(
                    &transcode::TranscodeParams {
                        input: &req.input,
                        output: &req.output,
                        op: req.op,
                    },
                    ctl,
                )?;
                Ok(EditReport {
                    path: req.output.clone(),
                    frames,
                })
            }
            EditOp::TrimKeyframe { start_ms, end_ms } => {
                let frames = trim::run(
                    &trim::TrimParams {
                        input: &req.input,
                        output: &req.output,
                        start_ms,
                        end_ms,
                    },
                    ctl,
                )?;
                Ok(EditReport {
                    path: req.output.clone(),
                    frames,
                })
            }
        }
    }
}

/// 按请求选择引擎实例。
///
/// `Auto` 返回首选的系统引擎；它做不了某项操作时的回落由 [`execute`] 负责。
///
/// # 参数
/// - `kind`：请求的引擎。
///
/// # 返回
/// 引擎实例。
pub fn pick_engine(kind: EngineKind) -> Result<Box<dyn EditEngine>, EditError> {
    match kind {
        EngineKind::Auto | EngineKind::System => Ok(Box::new(SystemEngine)),
        EngineKind::Ffmpeg => Ok(Box::new(FfmpegEngine)),
    }
}

/// 用 FFmpeg 探测视频信息。
///
/// # 参数
/// - `input`：输入视频。
fn probe_ffmpeg(input: &Path) -> Result<ProbeInfo, EditError> {
    let mut src = source::VideoSource::open(input)?;
    Ok(src.scan()?.0)
}

/// 探测视频信息：系统引擎优先，失败时回落 FFmpeg（与 `Auto` 的引擎顺序一致）。
///
/// # 参数
/// - `input`：输入视频。
pub fn probe(input: &Path) -> Result<ProbeInfo, EditError> {
    match SystemEngine.probe(input) {
        Ok(info) => Ok(info),
        Err(_) => probe_ffmpeg(input),
    }
}

/// 在指定引擎上执行：探测、检查能力、运行。
///
/// # 参数
/// - `engine`：引擎。
/// - `req`：编辑请求。
/// - `ctl`：进度与取消控制。
fn run_on(
    engine: &mut dyn EditEngine,
    req: &EditRequest,
    ctl: &TaskCtl,
) -> Result<(EditReport, EngineKind), EditError> {
    let info = engine.probe(&req.input)?;
    if let OpSupport::Unavailable(reason) = engine.capabilities(&info).support(&req.op) {
        return Err(EditError::unsupported(format!(
            "{}引擎不支持该操作: {reason}",
            engine.kind().as_str()
        )));
    }
    let report = engine.run(req, ctl)?;
    Ok((report, engine.kind()))
}

/// 执行一个编辑请求：选引擎、检查能力、运行。
///
/// `Auto`：先试系统引擎，它不支持该操作或没能启动时回落 FFmpeg；取消和运行中途的失败不回落。
/// 显式选 `System` / `Ffmpeg` 时绝不静默切换，不支持就返回明确错误。
///
/// # 参数
/// - `req`：编辑请求。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 结果（`engine` 为实际使用的引擎）。
pub fn execute(req: &EditRequest, ctl: &TaskCtl) -> Result<(EditReport, EngineKind), EditError> {
    let mut engine = pick_engine(req.engine)?;
    match run_on(engine.as_mut(), req, ctl) {
        Err(e) if req.engine == EngineKind::Auto && e.can_fallback() => {
            run_on(pick_engine(EngineKind::Ffmpeg)?.as_mut(), req, ctl)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_recorder_protocol::{ExtractMode, ImageFormat};
    use std::sync::atomic::AtomicUsize;

    /// 限频：连续上报只放行第一条，完成那条一定放行。
    #[test]
    fn report_is_rate_limited_but_final_passes() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let ctl = TaskCtl::new(
            Arc::new(AtomicBool::new(false)),
            Box::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        for i in 1..=50 {
            ctl.report(i, 100, "x");
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        ctl.report(100, 100, "x");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    /// 取消标志生效。
    #[test]
    fn cancel_flag_is_visible() {
        let flag = Arc::new(AtomicBool::new(false));
        let ctl = TaskCtl::new(Arc::clone(&flag), Box::new(|_| {}));
        assert!(!ctl.is_cancelled());
        flag.store(true, Ordering::SeqCst);
        assert!(ctl.is_cancelled());
    }

    /// 能力：FFmpeg 引擎各项都可用；系统引擎的 WebP 与裁剪标不可用；引擎选择符合约定。
    #[test]
    fn capabilities_and_engine_choice() {
        let info = ProbeInfo {
            width: 1,
            height: 1,
            duration_ms: 1,
            fps_milli: 1,
            frames: 1,
            keyframes: 1,
        };
        let caps = FfmpegEngine.capabilities(&info);
        let extract = EditOp::ExtractFrames {
            mode: ExtractMode::Keyframes,
            format: ImageFormat::Png,
            quality: 90,
        };
        assert_eq!(caps.support(&extract), &OpSupport::Available);
        assert_eq!(
            caps.support(&EditOp::ReduceFps { target_fps: 10 }),
            &OpSupport::Available
        );
        assert_eq!(
            caps.support(&EditOp::TrimKeyframe {
                start_ms: 0,
                end_ms: 1
            }),
            &OpSupport::Available
        );
        let webp = EditOp::ExtractFrames {
            mode: ExtractMode::Keyframes,
            format: ImageFormat::WebpLossless,
            quality: 90,
        };
        assert_eq!(caps.support(&webp), &OpSupport::Available);
        let sys = SystemEngine.capabilities(&info);
        assert_eq!(sys.support(&extract), &OpSupport::Available);
        assert!(matches!(sys.support(&webp), OpSupport::Unavailable(_)));
        assert!(matches!(
            sys.support(&EditOp::TrimKeyframe {
                start_ms: 0,
                end_ms: 1
            }),
            OpSupport::Unavailable(_)
        ));
        assert_eq!(
            pick_engine(EngineKind::System).unwrap().kind(),
            EngineKind::System
        );
        assert_eq!(
            pick_engine(EngineKind::Auto).unwrap().kind(),
            EngineKind::System
        );
        assert_eq!(
            pick_engine(EngineKind::Ffmpeg).unwrap().kind(),
            EngineKind::Ffmpeg
        );
    }

    /// 构造一个静默的任务控制。
    fn quiet() -> TaskCtl {
        TaskCtl::new(Arc::new(AtomicBool::new(false)), Box::new(|_| {}))
    }

    /// 显式选系统引擎做它不支持的操作：得到明确错误、不静默切换、不产生输出；
    /// 同样的请求用 Auto 会回落 FFmpeg 并成功。
    #[test]
    fn explicit_system_is_clear_error_but_auto_falls_back() {
        let dir = testclip::temp_dir("execute-fallback");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &testclip::Clip::default()).unwrap();
        let trim = EditOp::TrimKeyframe {
            start_ms: 0,
            end_ms: 1000,
        };
        let req = EditRequest {
            engine: EngineKind::System,
            op: trim,
            input: input.clone(),
            output: dir.join("o.mp4"),
        };
        let err = execute(&req, &quiet()).unwrap_err();
        assert!(err.to_string().contains("system引擎不支持"), "{err}");
        assert!(!dir.join("o.mp4").exists());
        let req = EditRequest {
            engine: EngineKind::Auto,
            ..req
        };
        let (_, engine) = execute(&req, &quiet()).unwrap();
        assert_eq!(engine, EngineKind::Ffmpeg);
        assert!(dir.join("o.mp4").exists());
        // 无损 WebP：显式系统引擎报错，Auto 回落 FFmpeg
        let webp = EditRequest {
            engine: EngineKind::System,
            op: EditOp::ExtractFrames {
                mode: ExtractMode::Single { at_ms: 500 },
                format: ImageFormat::WebpLossless,
                quality: 90,
            },
            input,
            output: dir.join("w"),
        };
        assert!(execute(&webp, &quiet()).is_err());
        assert!(!dir.join("w").exists());
        let webp = EditRequest {
            engine: EngineKind::Auto,
            ..webp
        };
        assert_eq!(execute(&webp, &quiet()).unwrap().1, EngineKind::Ffmpeg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不是视频的输入：Auto 两个引擎都失败，返回可读错误而非 panic。
    #[test]
    fn auto_with_corrupt_input_reports_error() {
        let dir = testclip::temp_dir("execute-corrupt");
        let input = dir.join("bad.mp4");
        std::fs::write(&input, b"not a video").unwrap();
        let req = EditRequest {
            engine: EngineKind::Auto,
            op: EditOp::ReduceFps { target_fps: 5 },
            input,
            output: dir.join("o.mp4"),
        };
        let err = execute(&req, &quiet()).unwrap_err();
        assert!(!err.is_cancelled() && !err.to_string().is_empty());
        assert!(!dir.join("o.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端到端：execute 抽帧并报告实际引擎与张数。
    #[test]
    fn execute_extracts_frames() {
        let dir = testclip::temp_dir("execute-extract");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &testclip::Clip::default()).unwrap();
        let events = Arc::new(Mutex::new(Vec::<Event>::new()));
        let sink = Arc::clone(&events);
        let ctl = TaskCtl::new(
            Arc::new(AtomicBool::new(false)),
            Box::new(move |e| sink.lock().unwrap().push(e.clone())),
        );
        let req = EditRequest {
            engine: EngineKind::Auto,
            op: EditOp::ExtractFrames {
                mode: ExtractMode::Interval { every_ms: 1000 },
                format: ImageFormat::Png,
                quality: 90,
            },
            input,
            output: dir.join("frames"),
        };
        let (report, engine) = execute(&req, &ctl).unwrap();
        assert_eq!(engine, EngineKind::System, "Auto 应优先系统引擎");
        assert_eq!(report.frames, 3);
        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::EditProgress {
                done: 3,
                total: 3,
                ..
            }
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
