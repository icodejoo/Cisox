//! 把协议里的开始请求翻译为 snow-crates 的直录配置。

use std::path::PathBuf;

use snow_recorder_protocol::{MediaFormat, StartRequest};

use crate::settings::{SizeLimit, oriented_limit};
#[cfg(test)]
use snow_recorder_protocol::scratch_file;
use snow_screen_recorder::{
    CaptureBackendKind, DirectRecordingConfig, ExportFormat, RecordingRegion, VideoCodec,
    VideoEncodingSpeed,
};

/// 鼠标轨迹时长（毫秒）；运行时要求 100..=2000，即便轨迹被关闭也需合法。
const MOUSE_TRAIL_DURATION_MS: u64 = 500;
/// 全透明色：关闭对应叠加特效。
const EFFECT_OFF_RGBA: [u8; 4] = [0; 4];
/// 环境变量：覆盖 x264 预设（ultrafast / superfast / veryfast / faster / fast / medium）。
pub const ENV_PRESET: &str = "SNOW_RECORDER_PRESET";

/// 解析预设名；未知或空返回 `None`（沿用默认 VeryFast）。
///
/// # 参数
/// - `text`：预设名。
pub fn parse_preset(text: &str) -> Option<VideoEncodingSpeed> {
    match text.trim().to_ascii_lowercase().as_str() {
        "ultrafast" => Some(VideoEncodingSpeed::UltraFast),
        "superfast" => Some(VideoEncodingSpeed::SuperFast),
        "veryfast" => Some(VideoEncodingSpeed::VeryFast),
        "faster" => Some(VideoEncodingSpeed::Faster),
        "fast" => Some(VideoEncodingSpeed::Fast),
        "medium" => Some(VideoEncodingSpeed::Medium),
        _ => None,
    }
}

/// 协议格式到导出格式的映射。
///
/// # 参数
/// - `format`：协议格式。
pub fn export_format(format: MediaFormat) -> ExportFormat {
    match format {
        MediaFormat::Mp4 => ExportFormat::Mp4,
        MediaFormat::Gif => ExportFormat::Gif,
        MediaFormat::Apng => ExportFormat::Apng,
        MediaFormat::Webp => ExportFormat::Webp,
    }
}

/// 由开始请求构造直录配置；输出指向中间文件（完成后由调用方改名为最终路径）。
///
/// # 参数
/// - `request`：开始请求。
/// - `partial`：中间输出路径（扩展名须与格式一致）。
/// - `prefer_hardware`：是否优先硬件编码。
/// - `limit`：输出尺寸上限（长边、短边），`None` 不限。
pub fn build_config(
    request: &StartRequest,
    partial: PathBuf,
    prefer_hardware: bool,
    limit: SizeLimit,
) -> DirectRecordingConfig {
    let (maximum_width, maximum_height) = oriented_limit(limit, request.width, request.height);
    DirectRecordingConfig {
        loop_animated_images: true,
        region: RecordingRegion::new(request.x, request.y, request.width, request.height),
        capture_backend: CaptureBackendKind::Auto,
        output_path: partial,
        format: export_format(request.format),
        capture_fps: request.fps,
        output_fps: request.fps,
        maximum_width,
        maximum_height,
        codec: VideoCodec::H264,
        preset: VideoEncodingSpeed::SuperFast,
        prefer_hardware_encoder: prefer_hardware,
        enable_microphone: false,
        enable_system_audio: false,
        show_cursor: request.show_cursor,
        keyboard: None,
        mouse_trail_rgba: EFFECT_OFF_RGBA,
        mouse_trail_duration_ms: MOUSE_TRAIL_DURATION_MS,
        mouse_click_rgba: EFFECT_OFF_RGBA,
        mouse_highlight_rgba: EFFECT_OFF_RGBA,
        record_mouse_clicks: false,
        show_keyboard: false,
        excluded_windows: Default::default(),
        excluded_processes: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试请求。
    fn request(format: MediaFormat, output: &str) -> StartRequest {
        StartRequest {
            x: 10,
            y: 20,
            width: 640,
            height: 480,
            format,
            fps: 30,
            show_cursor: true,
            output: PathBuf::from(output),
        }
    }

    /// 生成的配置通过运行时自带校验，四种格式均可。
    #[test]
    fn built_config_passes_runtime_validation() {
        for format in [MediaFormat::Mp4, MediaFormat::Gif, MediaFormat::Apng, MediaFormat::Webp] {
            let final_path = PathBuf::from(format!("out.{}", format.as_str()));
            let req = request(format, final_path.to_str().unwrap_or("out.mp4"));
            let config = build_config(&req, scratch_file(&final_path, 1), false, None);
            assert_eq!(config.validate(), Ok(()), "格式 {format:?}");
            assert_eq!(config.region.width, 640);
            assert!(config.show_cursor);
        }
    }

    /// 预设名解析。
    #[test]
    fn preset_parsing_cases() {
        assert!(matches!(parse_preset(" UltraFast "), Some(VideoEncodingSpeed::UltraFast)));
        assert!(parse_preset("placebo").is_none());
        assert!(parse_preset("").is_none());
    }

    /// 零尺寸区域会被运行时校验拒绝（错误由录制进程转成 ERROR 事件）。
    #[test]
    fn zero_sized_region_is_rejected() {
        let mut req = request(MediaFormat::Mp4, "o.mp4");
        req.width = 0;
        let config = build_config(&req, scratch_file(&req.output, 1), false, None);
        assert!(config.validate().is_err());
    }
}
