//! 软件编码后端（ffmpeg + x264，跨平台回退）：封装上游 `DirectRecordingSession`。
//!
//! 采集、转换、编码都在上游会话里完成；这里只负责按请求构造配置、按环境变量调优、并适配成 [`RecordingBackend`]。
//! 上游的 GPU 路径（`upstream_gpu`）也走这里，仅用于和自建硬件流水线对照。

use std::path::PathBuf;

use snow_recorder_protocol::StartRequest;
use snow_screen_recorder::{DirectRecordingSession, ScreenRecorderError};

use crate::backend::{BackendReport, RecordingBackend};
use crate::{plan, settings};

/// 环境变量：像素转换线程数（0 = 单线程 swscale，最多 4）。
const ENV_CONV_THREADS: &str = "SNOW_RECORDER_CONV_THREADS";
/// 环境变量：x264 编码线程数（1..=16；未设置时按默认策略）。
const ENV_ENCODE_THREADS: &str = "SNOW_RECORDER_ENCODE_THREADS";
/// 环境变量：设为 `1` 启用异步编码（编码与转换不占用采集后的处理线程）。
const ENV_ASYNC: &str = "SNOW_RECORDER_ASYNC";
/// 默认转换线程数。
const DEFAULT_CONV_THREADS: u8 = 2;
/// 默认是否异步编码。
const DEFAULT_ASYNC: bool = true;

/// 把上游错误转成单行原因文本。
fn describe(error: &ScreenRecorderError) -> String {
    format!("{error}")
}

/// 解析软编调优：`(转换线程数, 是否异步编码)`。
///
/// # 参数
/// - `upstream_gpu`：是否上游 GPU 路径（它不接受转换线程与异步编码，强制 0 线程、同步）。
/// - `conv`：`SNOW_RECORDER_CONV_THREADS` 的值。
/// - `asynchronous`：`SNOW_RECORDER_ASYNC` 的值。
///
/// # 示例
/// ```ignore
/// assert_eq!(resolve_tuning(false, None, None), (2, true));
/// assert_eq!(resolve_tuning(true, Some("4"), Some("1")), (0, false));
/// ```
pub fn resolve_tuning(upstream_gpu: bool, conv: Option<&str>, asynchronous: Option<&str>) -> (u8, bool) {
    if upstream_gpu {
        return (0, false);
    }
    let threads = conv.and_then(|v| v.parse::<u8>().ok()).unwrap_or(DEFAULT_CONV_THREADS);
    let asynchronous = asynchronous.map_or(DEFAULT_ASYNC, |v| v == "1");
    (threads, asynchronous)
}

/// 按环境变量调优会话（性能实验用；默认值见常量）。
fn tune_session(session: &mut DirectRecordingSession, upstream_gpu: bool) -> Result<(), String> {
    let (threads, asynchronous) = resolve_tuning(upstream_gpu, std::env::var(ENV_CONV_THREADS).ok().as_deref(), std::env::var(ENV_ASYNC).ok().as_deref());
    if let Some(n) = std::env::var(ENV_ENCODE_THREADS).ok().and_then(|v| v.parse::<u8>().ok()).filter(|n| (1..=16).contains(n)) {
        session.set_encode_threads(n).map_err(|e| describe(&e))?;
    }
    session.set_bench_encoding(threads, false).map_err(|e| describe(&e))?;
    session.set_bench_async_encoding(asynchronous).map_err(|e| describe(&e))?;
    session.set_bench_automatic_policies(true).map_err(|e| describe(&e))
}

/// 上游会话适配器。
struct SoftBackend {
    /// 直录会话（stop 会阻塞，仅在控制线程调用）。
    session: DirectRecordingSession,
    /// 后端名。
    name: &'static str,
}

impl RecordingBackend for SoftBackend {
    /// 后端名。
    fn name(&self) -> &str {
        self.name
    }

    /// 暂停。
    fn pause(&self) -> Result<(), String> {
        self.session.pause().map_err(|e| describe(&e))
    }

    /// 恢复。
    fn resume(&self) -> Result<(), String> {
        self.session.resume().map_err(|e| describe(&e))
    }

    /// 停止并收尾。
    fn stop(self: Box<Self>) -> Result<BackendReport, String> {
        let report = self.session.stop().map_err(|e| describe(&e))?;
        Ok(BackendReport {
            frames: report.encoded_frames,
            dropped: report.dropped_capture_frames,
            diagnostics: format!("录制报告: {report:?}"),
        })
    }

    /// 取消：丢弃会话（内部会终止工作线程）。
    fn cancel(self: Box<Self>) {
        drop(self.session);
    }
}

/// 启动上游软件编码会话。
///
/// # 参数
/// - `request`：开始请求。
/// - `partial`：中间输出路径。
/// - `upstream_gpu`：是否启用上游自带的 GPU 路径（对照用）。
///
/// # 返回
/// 适配后的后端；配置非法或会话启动失败返回原因。
pub fn start_software(request: &StartRequest, partial: PathBuf, upstream_gpu: bool) -> Result<Box<dyn RecordingBackend>, String> {
    let mut config = plan::build_config(request, partial, upstream_gpu, settings::configured_size_limit());
    if let Some(p) = std::env::var(plan::ENV_PRESET).ok().and_then(|v| plan::parse_preset(&v)) {
        config.preset = p;
    }
    config.validate()?;
    let mut session = DirectRecordingSession::create(config).map_err(|e| describe(&e))?;
    tune_session(&mut session, upstream_gpu)?;
    session.start().map_err(|e| describe(&e))?;
    Ok(Box::new(SoftBackend { session, name: if upstream_gpu { "upstream-gpu" } else { "software-x264" } }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 调优解析：默认、环境覆盖、上游 GPU 强制同步。
    #[test]
    fn tuning_resolution() {
        assert_eq!(resolve_tuning(false, None, None), (2, true));
        assert_eq!(resolve_tuning(false, Some("0"), Some("0")), (0, false));
        assert_eq!(resolve_tuning(false, Some("x"), Some("1")), (2, true));
        assert_eq!(resolve_tuning(true, Some("4"), Some("1")), (0, false));
    }
}
