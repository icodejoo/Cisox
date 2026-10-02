//! 视频编辑任务（worker 进程内）。
//!
//! 设计见 `docs/research/video-editor-mvp-design.md`：编辑任务并入 `snow-recorder`，
//! 通过 [`EditEngine`] 抽象出引擎，目前只有 FFmpeg 引擎（系统引擎是后续阶段）。
//! 已实现：探测（`probe`）、按时间戳精确 seek（`source`）、抽帧（`extract`）；
//! 降 fps / 缩放 / 关键帧裁剪尚未实现，请求它们会得到明确的"未实现"错误，不会崩溃。

pub mod extract;
pub mod image;
pub mod source;
pub mod yuv;

#[cfg(test)]
pub mod testclip;

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use snow_recorder_protocol::{EditOp, EditRequest, EngineKind, Event, ProbeInfo};

/// 进度事件的最小间隔（限频到每秒约 10 次）。
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// 编辑任务错误（单行可读文本）。
#[derive(Debug, Clone)]
pub struct EditError {
    /// 错误描述。
    message: String,
    /// 是否由用户取消造成。
    cancelled: bool,
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
        }
    }

    /// 创建"已取消"错误。
    pub fn cancelled() -> Self {
        Self {
            message: "任务已取消".to_string(),
            cancelled: true,
        }
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
    /// 抽帧。
    pub extract_frames: OpSupport,
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
            EditOp::ExtractFrames { .. } => &self.extract_frames,
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

/// "尚未实现"的原因文案。
const NOT_IMPLEMENTED: &str = "该操作尚未实现";

impl EditEngine for FfmpegEngine {
    /// 见 trait。
    fn kind(&self) -> EngineKind {
        EngineKind::Ffmpeg
    }

    /// 目前只有抽帧可用。
    fn capabilities(&self, _input: &ProbeInfo) -> EngineCaps {
        let todo = || OpSupport::Unavailable(NOT_IMPLEMENTED.to_string());
        EngineCaps {
            reduce_fps: todo(),
            scale: todo(),
            extract_frames: OpSupport::Available,
            trim_keyframe: todo(),
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
            _ => Err(EditError::new(NOT_IMPLEMENTED)),
        }
    }
}

/// 按请求选择引擎。
///
/// `Auto` 的设计顺序是"系统引擎优先、FFmpeg 回落"；系统引擎还没有实现，
/// 目前 `Auto` 直接落到 FFmpeg，显式要求系统引擎会得到明确错误（不静默切换）。
///
/// # 参数
/// - `kind`：请求的引擎。
///
/// # 返回
/// 引擎实例；所选引擎不可用返回错误。
pub fn pick_engine(kind: EngineKind) -> Result<Box<dyn EditEngine>, EditError> {
    match kind {
        EngineKind::Auto | EngineKind::Ffmpeg => Ok(Box::new(FfmpegEngine)),
        EngineKind::System => Err(EditError::new("系统引擎尚未实现，请选择 FFmpeg 引擎")),
    }
}

/// 探测视频信息。
///
/// # 参数
/// - `input`：输入视频。
pub fn probe(input: &Path) -> Result<ProbeInfo, EditError> {
    let mut src = source::VideoSource::open(input)?;
    Ok(src.scan()?.0)
}

/// 执行一个编辑请求：选引擎、检查能力、运行。
///
/// # 参数
/// - `req`：编辑请求。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 结果（`engine` 为实际使用的引擎）。
pub fn execute(req: &EditRequest, ctl: &TaskCtl) -> Result<(EditReport, EngineKind), EditError> {
    let mut engine = pick_engine(req.engine)?;
    let info = probe(&req.input)?;
    if let OpSupport::Unavailable(reason) = engine.capabilities(&info).support(&req.op) {
        return Err(EditError::new(reason.clone()));
    }
    let report = engine.run(req, ctl)?;
    Ok((report, engine.kind()))
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

    /// 能力：抽帧可用，其余明确"未实现"；系统引擎显式选择时报错。
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
        assert!(matches!(
            caps.support(&EditOp::ReduceFps { target_fps: 10 }),
            OpSupport::Unavailable(_)
        ));
        assert!(pick_engine(EngineKind::System).is_err());
        assert_eq!(
            pick_engine(EngineKind::Auto).unwrap().kind(),
            EngineKind::Ffmpeg
        );
    }

    /// 未实现的操作经 `execute` 得到明确错误而非 panic。
    #[test]
    fn unimplemented_op_is_clear_error() {
        let dir = testclip::temp_dir("execute-unimpl");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &testclip::Clip::default()).unwrap();
        let ctl = TaskCtl::new(Arc::new(AtomicBool::new(false)), Box::new(|_| {}));
        let req = EditRequest {
            engine: EngineKind::Auto,
            op: EditOp::ReduceFps { target_fps: 10 },
            input,
            output: dir.join("o.mp4"),
        };
        let err = execute(&req, &ctl).unwrap_err();
        assert!(err.to_string().contains("尚未实现"));
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
        assert_eq!(engine, EngineKind::Ffmpeg);
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
