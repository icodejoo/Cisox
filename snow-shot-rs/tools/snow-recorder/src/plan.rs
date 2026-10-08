//! 把协议里的开始请求翻译为 snow-crates 的直录配置。

use std::path::PathBuf;

use snow_recorder_protocol::{EffectsRequest, MediaFormat, StartRequest};

use crate::settings::{SizeLimit, oriented_limit};
#[cfg(test)]
use snow_recorder_protocol::scratch_file;
use snow_screen_recorder::{
    CaptureBackendKind, DirectRecordingConfig, ExportFormat, KeyboardOverlayConfig, RecordingRegion, VideoCodec,
    VideoEncodingSpeed,
};

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

/// 按钮帽边框的透明度（相对文字色）。
const KEYCAP_BORDER_ALPHA: u8 = 0x40;

/// 按特效请求构造按键回显配置；未开启返回 `None`。
///
/// # 参数
/// - `effects`：特效请求。
fn keyboard_overlay(effects: &EffectsRequest) -> Option<KeyboardOverlayConfig> {
    effects.keyboard.then(|| {
        let [r, g, b, _] = effects.keyboard_text;
        KeyboardOverlayConfig {
            font: None,
            keycap_size: effects.keyboard_size,
            background_rgba: effects.keyboard_background,
            text_rgba: effects.keyboard_text,
            border_rgba: [r, g, b, KEYCAP_BORDER_ALPHA],
            labels: Default::default(),
        }
    })
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
        // 音频只在 MP4 启用；软件路径用上游会话自带的混音，音量与设备选择由自建路径支持
        enable_microphone: request.audio.microphone && request.format == MediaFormat::Mp4,
        enable_system_audio: request.audio.system && request.format == MediaFormat::Mp4,
        show_cursor: request.show_cursor,
        keyboard: keyboard_overlay(&request.effects),
        mouse_trail_rgba: request.effects.trail,
        mouse_trail_duration_ms: u64::from(request.effects.trail_ms).clamp(100, 2000),
        mouse_click_rgba: request.effects.click,
        mouse_highlight_rgba: request.effects.highlight,
        record_mouse_clicks: request.effects.record_clicks,
        show_keyboard: request.effects.keyboard,
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
            audio: Default::default(),
            effects: Default::default(),
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

    /// 音频开关只在 MP4 透传，动图格式一律关闭。
    #[test]
    fn audio_flags_follow_request_and_format() {
        for (format, expect) in [(MediaFormat::Mp4, true), (MediaFormat::Gif, false), (MediaFormat::Apng, false), (MediaFormat::Webp, false)] {
            let mut req = request(format, "o.mp4");
            req.audio.microphone = true;
            req.audio.system = true;
            let config = build_config(&req, scratch_file(&req.output, 1), false, None);
            assert_eq!((config.enable_microphone, config.enable_system_audio), (expect, expect), "格式 {format:?}");
        }
        let config = build_config(&request(MediaFormat::Mp4, "o.mp4"), scratch_file(std::path::Path::new("o.mp4"), 1), false, None);
        assert!(!config.enable_microphone && !config.enable_system_audio);
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

    /// 特效请求映射到直录配置：全关时与旧行为一致，打开后各字段透传并通过运行时校验。
    #[test]
    fn effects_map_into_config() {
        let mut req = request(MediaFormat::Mp4, "o.mp4");
        let off = build_config(&req, scratch_file(&req.output, 1), false, None);
        assert!(!off.show_keyboard && !off.record_mouse_clicks && off.keyboard.is_none());
        assert_eq!(off.mouse_trail_rgba, [0; 4]);
        assert_eq!(off.mouse_trail_duration_ms, 500);

        req.effects = EffectsRequest {
            trail: [255, 0, 0, 200],
            trail_ms: 900,
            click: [0, 255, 0, 255],
            highlight: [255, 255, 0, 90],
            record_clicks: true,
            keyboard: true,
            keyboard_size: 80,
            ..EffectsRequest::default()
        };
        let on = build_config(&req, scratch_file(&req.output, 1), false, None);
        assert_eq!(on.validate(), Ok(()));
        assert_eq!(on.mouse_trail_rgba, [255, 0, 0, 200]);
        assert_eq!(on.mouse_trail_duration_ms, 900);
        assert_eq!(on.mouse_click_rgba, [0, 255, 0, 255]);
        assert_eq!(on.mouse_highlight_rgba, [255, 255, 0, 90]);
        assert!(on.record_mouse_clicks && on.show_keyboard);
        assert_eq!(on.keyboard.as_ref().map(|k| k.keycap_size), Some(80));
    }
}
