//! 系统引擎（Windows）：Media Foundation 读取与编码 + WIC 图片编码。
//!
//! 能力（设计 §5、§11）：
//! - 探测与精确 seek：`IMFSourceReader`（`reader`），seek 到目标前的关键帧再解码到目标时间戳。
//! - 抽帧：PNG / JPEG 走 WIC；无损 WebP 系统做不了，能力里标不可用，由 `Auto` 回落 FFmpeg。
//! - 降 fps / 缩放：SourceReader 解码（缩放时由其内置视频处理器完成）+ SinkWriter 硬件 H.264 MFT，
//!   音频包直通；没有硬件 H.264 编码 MFT 时标不可用（不静默使用微软软件 MFT）。
//! - 关键帧裁剪（包级拷贝）：系统引擎做不了，标不可用，由 `Auto` 回落 FFmpeg。
//!
//! 打不开输入、没有硬件编码器这类"还没产出任何结果"的失败都带回落标志（见 `EditError::unsupported`）。

pub mod reader;

mod extract;
mod transcode;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bench;

use std::path::Path;

use snow_recorder_protocol::{EditOp, EditRequest, EngineKind, ProbeInfo};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};

use super::image::ComGuard;
use super::{EditEngine, EditError, EditReport, EngineCaps, OpSupport, TaskCtl};
use crate::win::mfenc::enumerate_hardware_h264;

/// 系统引擎无法导出无损 WebP 的原因（WIC 的 WebP 编码器是可选的商店扩展，且不能指定无损）。
const NO_SYSTEM_WEBP: &str = "系统 WIC 无法保证无损 WebP 编码";
/// 系统引擎不做无重编码裁剪的原因。
const NO_SYSTEM_TRIM: &str = "系统引擎不支持无重编码裁剪";
/// 没有硬件 H.264 编码 MFT 的原因。
const NO_HW_H264: &str = "系统没有硬件 H.264 编码 MFT";

/// Media Foundation 会话守卫：持有当前线程的 COM 与 `MFStartup`，释放时配对关闭。
pub(super) struct MfSession {
    /// COM 守卫，在 `MFShutdown` 之后才释放。
    _com: ComGuard,
}

impl MfSession {
    /// 初始化 COM 并启动 Media Foundation。
    ///
    /// # 返回
    /// 会话；系统组件不可用时返回可回落的错误。
    pub(super) fn start() -> Result<Self, EditError> {
        let com = ComGuard::init();
        // SAFETY: 标准启动调用，成功才会在 Drop 里配对关闭。
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .map_err(|e| EditError::unsupported(format!("启动 Media Foundation 失败: {e}")))?;
        Ok(Self { _com: com })
    }
}

impl Drop for MfSession {
    /// 配对关闭 Media Foundation。
    fn drop(&mut self) {
        // SAFETY: 与成功的 MFStartup 配对。
        let _ = unsafe { MFShutdown() };
    }
}

/// 把 windows 错误转成"已开始产出后的普通失败"。
pub(super) fn mf_err(what: &str, e: windows::core::Error) -> EditError {
    EditError::new(format!("{what}: {e}"))
}

/// 把 windows 错误转成"还没产出结果的失败"（`Auto` 可回落）。
pub(super) fn mf_init_err(what: &str, e: windows::core::Error) -> EditError {
    EditError::unsupported(format!("{what}: {e}"))
}

/// 系统是否有硬件 H.264 编码 MFT。
fn has_hardware_h264() -> bool {
    match MfSession::start() {
        Ok(_session) => enumerate_hardware_h264(None).is_ok_and(|names| !names.is_empty()),
        Err(_) => false,
    }
}

/// 系统引擎（Media Foundation + WIC）。
#[derive(Debug, Default)]
pub struct SystemEngine;

impl EditEngine for SystemEngine {
    /// 见 trait。
    fn kind(&self) -> EngineKind {
        EngineKind::System
    }

    /// 见 trait；失败带回落标志。
    fn probe(&self, input: &Path) -> Result<ProbeInfo, EditError> {
        let _session = MfSession::start()?;
        Ok(reader::scan(input)?.info)
    }

    /// 抽帧（PNG / JPEG）总是可用；降 fps / 缩放需要硬件 H.264 MFT；WebP 与裁剪不可用。
    fn capabilities(&self, _input: &ProbeInfo) -> EngineCaps {
        let reencode = if has_hardware_h264() {
            OpSupport::Available
        } else {
            OpSupport::Unavailable(NO_HW_H264.to_string())
        };
        EngineCaps {
            reduce_fps: reencode.clone(),
            scale: reencode,
            extract_frames: OpSupport::Available,
            extract_webp_lossless: OpSupport::Unavailable(NO_SYSTEM_WEBP.to_string()),
            trim_keyframe: OpSupport::Unavailable(NO_SYSTEM_TRIM.to_string()),
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
                let frames = transcode::run(&req.input, &req.output, req.op, ctl)?;
                Ok(EditReport {
                    path: req.output.clone(),
                    frames,
                })
            }
            EditOp::TrimKeyframe { .. } => Err(EditError::unsupported(NO_SYSTEM_TRIM)),
        }
    }
}
