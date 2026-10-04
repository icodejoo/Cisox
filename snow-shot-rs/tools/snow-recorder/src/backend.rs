//! 录制后端抽象与装配：按优先级尝试各后端，第一个成功的生效，失败原因记日志后回落。
//!
//! 选择只在会话初始化时做一次；运行期每帧的热路径在各后端内部（流水线对三个阶段 trait 泛型单态化），
//! 这里的 trait 对象只管暂停/恢复/停止/取消这几个低频控制动作。

use std::path::PathBuf;

use snow_recorder_protocol::StartRequest;
#[cfg(windows)]
use snow_recorder_protocol::MediaFormat;

use crate::pipeline::Running;
use crate::settings::{self, HardwareMode};
use crate::soft;

/// 录制结束后的汇总。
#[derive(Debug, Clone)]
pub struct BackendReport {
    /// 写入的帧数。
    pub frames: u64,
    /// 丢弃的帧数。
    pub dropped: u64,
    /// 诊断文本（写进 stderr）。
    pub diagnostics: String,
}

/// 一个录制后端：负责采集、转换、编码与封装的全过程。
pub trait RecordingBackend: Send {
    /// 后端名（诊断用）。
    fn name(&self) -> &str;

    /// 暂停（暂停期间的时间不计入时长）。
    fn pause(&self) -> Result<(), String>;

    /// 恢复。
    fn resume(&self) -> Result<(), String>;

    /// 停止并写出文件（阻塞到封装完成）。
    fn stop(self: Box<Self>) -> Result<BackendReport, String>;

    /// 取消并丢弃（输出文件由后端清理）。
    fn cancel(self: Box<Self>);
}

/// 一次装配尝试：名称与延迟执行的启动闭包。
pub type Attempt = (&'static str, Box<dyn FnOnce() -> Result<Box<dyn RecordingBackend>, String>>);

/// 自建流水线适配器：`Running` + 输出路径（取消时清理）。
#[cfg_attr(not(windows), allow(dead_code))]
struct PipelineBackend {
    /// 运行中的流水线。
    running: Running,
    /// 输出文件路径。
    output: PathBuf,
    /// 后端名。
    name: String,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl RecordingBackend for PipelineBackend {
    /// 后端名。
    fn name(&self) -> &str {
        &self.name
    }

    /// 暂停。
    fn pause(&self) -> Result<(), String> {
        self.running.pause();
        Ok(())
    }

    /// 恢复。
    fn resume(&self) -> Result<(), String> {
        self.running.resume();
        Ok(())
    }

    /// 停止并收尾。
    fn stop(self: Box<Self>) -> Result<BackendReport, String> {
        let report = self.running.stop()?;
        Ok(BackendReport {
            frames: report.encoded_frames,
            dropped: report.capture.pool_drops + report.pool_dropped,
            diagnostics: report.describe(),
        })
    }

    /// 取消并删除输出文件。
    fn cancel(self: Box<Self>) {
        self.running.cancel();
        let _ = std::fs::remove_file(&self.output);
    }
}

/// 依次尝试各个装配，第一个成功的生效。
///
/// # 参数
/// - `attempts`：按优先级排列的装配尝试。
/// - `log`：失败原因的日志回调（形如 `"<名称> 不可用，回落: <原因>"`）。
///
/// # 返回
/// 第一个成功的后端；全部失败时返回拼接的原因。
///
/// # 示例
/// ```ignore
/// let backend = start_first_available(attempts, &mut |line| eprintln!("{line}"))?;
/// ```
pub fn start_first_available(attempts: Vec<Attempt>, log: &mut dyn FnMut(&str)) -> Result<Box<dyn RecordingBackend>, String> {
    let mut reasons = Vec::new();
    for (name, attempt) in attempts {
        match attempt() {
            Ok(backend) => return Ok(backend),
            Err(reason) => {
                log(&format!("{name} 不可用，回落: {reason}"));
                reasons.push(format!("{name}: {reason}"));
            }
        }
    }
    Err(if reasons.is_empty() { "没有可用的录制后端".to_string() } else { reasons.join("; ") })
}

/// 按请求与硬件模式排出装配顺序（只排序，不执行）。
///
/// # 参数
/// - `request`：开始请求。
/// - `partial`：中间输出路径。
/// - `mode`：硬件编码模式。
///
/// # 返回
/// 装配尝试列表：自建硬件流水线（仅 Windows + MP4 + `Gpu` 模式）在前，软件编码在后；
/// `Upstream` 模式只有上游 GPU 路径；`Off` 只有软件编码。
pub fn plan_attempts(request: &StartRequest, partial: &std::path::Path, mode: HardwareMode) -> Vec<Attempt> {
    let mut attempts: Vec<Attempt> = Vec::new();
    #[cfg(windows)]
    if request.format == MediaFormat::Mp4 {
        // (装配名, 是否用 Media Foundation 编码)：Auto 固化为 MF 优先，其次 FFmpeg 厂商硬编，最后才是软编
        let hardware: &[(&'static str, bool)] = match mode {
            HardwareMode::Gpu => &[("windows-hardware", false)],
            HardwareMode::MediaFoundation => &[("windows-media-foundation", true)],
            HardwareMode::Auto => &[("windows-media-foundation", true), ("windows-hardware", false)],
            HardwareMode::Off | HardwareMode::Upstream => &[],
        };
        for &(name, media_foundation) in hardware {
            let (request, partial) = (request.clone(), partial.to_path_buf());
            attempts.push((
                name,
                Box::new(move || {
                    let mut spec = windows_spec(&request, &partial)?;
                    spec.media_foundation = media_foundation;
                    let output = spec.output.clone();
                    match crate::win::assemble::start_hardware(&spec) {
                        Ok(running) => {
                            let name = running_name(&running);
                            Ok(Box::new(PipelineBackend { running, output, name }) as Box<dyn RecordingBackend>)
                        }
                        Err(e) => {
                            // 探测阶段可能已创建输出文件，回落前清掉
                            let _ = std::fs::remove_file(&output);
                            Err(e)
                        }
                    }
                }),
            ));
        }
    }
    let upstream_gpu = mode == HardwareMode::Upstream;
    let (request, partial) = (request.clone(), partial.to_path_buf());
    attempts.push((
        if upstream_gpu { "upstream-gpu" } else { "software-x264" },
        Box::new(move || soft::start_software(&request, partial, upstream_gpu)),
    ));
    attempts
}

/// 取运行中流水线的后端名（流水线自己保存了装配时给的名字）。
#[cfg(windows)]
fn running_name(running: &Running) -> String {
    running.backend_name().to_string()
}

/// 由请求构造 Windows 硬件装配参数（输出尺寸按产品上限缩放，环境变量可调编码参数）。
#[cfg(windows)]
fn windows_spec(request: &StartRequest, partial: &std::path::Path) -> Result<crate::win::assemble::HardwareSpec, String> {
    use snow_screen_recorder::{ExportFormat, scaled_output_dimensions};
    let (max_w, max_h) = settings::oriented_limit(settings::configured_size_limit(), request.width, request.height);
    let out = scaled_output_dimensions(request.width, request.height, max_w, max_h, ExportFormat::Mp4);
    let mut spec = crate::win::assemble::HardwareSpec::new(
        (request.x, request.y, request.width, request.height),
        out,
        request.fps,
        request.show_cursor,
        partial.to_path_buf(),
    );
    if let Some(d) = std::env::var(settings::ENV_QSV_ASYNC_DEPTH).ok().and_then(|v| v.parse::<u32>().ok()).filter(|d| (1..=8).contains(d)) {
        spec.async_depth = d;
    }
    if let Some(q) = std::env::var(settings::ENV_QSV_QUALITY).ok().and_then(|v| v.parse::<u32>().ok()).filter(|q| (1..=51).contains(q)) {
        spec.quality = q;
    }
    if let Ok(p) = std::env::var(settings::ENV_QSV_PRESET) {
        spec.preset = p;
    }
    spec.audio = request.audio.clone();
    match settings::parse_encoder_preference(std::env::var(settings::ENV_ENCODER).ok().as_deref()) {
        Ok(preference) => spec.encoder = preference,
        Err(reason) => eprintln!("{reason}"),
    }
    Ok(spec)
}

/// 启动录制：按当前环境变量决定硬件模式，依次尝试并回落。
///
/// # 参数
/// - `request`：开始请求。
/// - `partial`：中间输出路径。
///
/// # 返回
/// 运行中的后端；全部失败返回原因。
pub fn start(request: &StartRequest, partial: &std::path::Path) -> Result<Box<dyn RecordingBackend>, String> {
    let mode = settings::parse_hardware_mode(std::env::var(settings::ENV_PREFER_HARDWARE).ok().as_deref());
    start_first_available(plan_attempts(request, partial, mode), &mut |line| eprintln!("{line}"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use snow_recorder_protocol::MediaFormat;

    use super::*;

    /// 什么都不做的假后端。
    struct Fake(&'static str);

    impl RecordingBackend for Fake {
        fn name(&self) -> &str {
            self.0
        }
        fn pause(&self) -> Result<(), String> {
            Ok(())
        }
        fn resume(&self) -> Result<(), String> {
            Ok(())
        }
        fn stop(self: Box<Self>) -> Result<BackendReport, String> {
            Ok(BackendReport { frames: 1, dropped: 0, diagnostics: String::new() })
        }
        fn cancel(self: Box<Self>) {}
    }

    /// 构造测试请求。
    fn request(format: MediaFormat) -> StartRequest {
        StartRequest { x: 0, y: 0, width: 64, height: 64, format, fps: 30, show_cursor: true, output: PathBuf::from("o.mp4"), audio: Default::default() }
    }

    /// 首个装配成功就不再尝试后面的。
    #[test]
    fn first_success_wins_and_later_attempts_do_not_run() {
        let ran = Rc::new(RefCell::new(false));
        let ran2 = Rc::clone(&ran);
        let attempts: Vec<Attempt> = vec![
            ("hardware", Box::new(|| Ok(Box::new(Fake("hardware")) as Box<dyn RecordingBackend>))),
            (
                "software",
                Box::new(move || {
                    *ran2.borrow_mut() = true;
                    Ok(Box::new(Fake("software")) as Box<dyn RecordingBackend>)
                }),
            ),
        ];
        let mut logs = Vec::new();
        let backend = start_first_available(attempts, &mut |l| logs.push(l.to_string())).unwrap();
        assert_eq!(backend.name(), "hardware");
        assert!(!*ran.borrow());
        assert!(logs.is_empty());
    }

    /// 视频处理器/NV12 能力检测失败：记录原因并回落到软件编码。
    #[test]
    fn capability_failure_falls_back_to_software() {
        let attempts: Vec<Attempt> = vec![
            ("windows-hardware", Box::new(|| Err("视频处理器不支持 BGRA→NV12".to_string()))),
            ("software-x264", Box::new(|| Ok(Box::new(Fake("software-x264")) as Box<dyn RecordingBackend>))),
        ];
        let mut logs = Vec::new();
        let backend = start_first_available(attempts, &mut |l| logs.push(l.to_string())).unwrap();
        assert_eq!(backend.name(), "software-x264");
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("windows-hardware") && logs[0].contains("BGRA→NV12") && logs[0].contains("回落"));
    }

    /// 全部失败：返回拼接的原因；没有任何装配也返回错误。
    #[test]
    fn all_failures_are_reported_together() {
        let attempts: Vec<Attempt> = vec![("a", Box::new(|| Err("原因甲".to_string()))), ("b", Box::new(|| Err("原因乙".to_string())))];
        let error = start_first_available(attempts, &mut |_| {}).err().unwrap();
        assert!(error.contains("a: 原因甲") && error.contains("b: 原因乙"));
        assert!(start_first_available(Vec::new(), &mut |_| {}).is_err());
    }

    /// 装配顺序：Gpu/MF 模式（MP4）硬件在前软件在后，Auto 为 MF -> FFmpeg 厂商硬编 -> 软编（非 Windows 只有软件）；其余模式与格式只有软件/上游。
    #[test]
    fn attempt_order_follows_mode_and_format() {
        let names = |format, mode| plan_attempts(&request(format), std::path::Path::new("p.mp4"), mode).iter().map(|a| a.0).collect::<Vec<_>>();
        if cfg!(windows) {
            assert_eq!(names(MediaFormat::Mp4, HardwareMode::Gpu), vec!["windows-hardware", "software-x264"]);
        } else {
            assert_eq!(names(MediaFormat::Mp4, HardwareMode::Gpu), vec!["software-x264"]);
        }
        if cfg!(windows) {
            assert_eq!(names(MediaFormat::Mp4, HardwareMode::MediaFoundation), vec!["windows-media-foundation", "software-x264"]);
            assert_eq!(names(MediaFormat::Mp4, HardwareMode::Auto), vec!["windows-media-foundation", "windows-hardware", "software-x264"]);
        } else {
            assert_eq!(names(MediaFormat::Mp4, HardwareMode::Auto), vec!["software-x264"]);
        }
        assert_eq!(names(MediaFormat::Webp, HardwareMode::Auto), vec!["software-x264"]);
        assert_eq!(names(MediaFormat::Webp, HardwareMode::Gpu), vec!["software-x264"]);
        assert_eq!(names(MediaFormat::Gif, HardwareMode::Gpu), vec!["software-x264"]);
        assert_eq!(names(MediaFormat::Apng, HardwareMode::Gpu), vec!["software-x264"]);
        assert_eq!(names(MediaFormat::Mp4, HardwareMode::Off), vec!["software-x264"]);
        assert_eq!(names(MediaFormat::Mp4, HardwareMode::Upstream), vec!["upstream-gpu"]);
    }
}
