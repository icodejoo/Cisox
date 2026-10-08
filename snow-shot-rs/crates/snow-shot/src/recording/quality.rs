//! 录屏编码质量的配置读取：把 `screen_recording/{clarity, animated_image_clarity, encoder,
//! encoding_preset, loop_animated_images}` 翻成协议里的 [`QualityRequest`]。

use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_recorder_protocol::QualityRequest;

/// 视频清晰度配置键。
pub const KEY_CLARITY: &str = "screen_recording/clarity";
/// 动图清晰度配置键。
pub const KEY_ANIMATED_CLARITY: &str = "screen_recording/animated_image_clarity";
/// 编码器配置键。
pub const KEY_ENCODER: &str = "screen_recording/encoder";
/// 编码预设配置键。
pub const KEY_PRESET: &str = "screen_recording/encoding_preset";
/// 动图是否循环配置键。
pub const KEY_LOOP: &str = "screen_recording/loop_animated_images";
/// 编码器取值：软件 H.264（其余取值都允许硬件编码）。
const ENCODER_H264: &str = "h264";

/// 清晰度取值对应的输出尺寸上限（长边、短边）；未知取值返回 `None`。
///
/// # 参数
/// - `value`：清晰度配置值，如 `1080p`、`4k`。
///
/// # 示例
/// ```ignore
/// assert_eq!(clarity_limit("720p"), Some((1280, 720)));
/// assert_eq!(clarity_limit("8k"), None);
/// ```
pub fn clarity_limit(value: &str) -> Option<(u32, u32)> {
    match value {
        "4k" => Some((3840, 2160)),
        "2k" => Some((2560, 1440)),
        "1080p" => Some((1920, 1080)),
        "720p" => Some((1280, 720)),
        "480p" => Some((854, 480)),
        _ => None,
    }
}

/// 由配置构造编码质量请求。
///
/// # 参数
/// - `document`：配置文档。
/// - `animated`：是否输出动图（GIF / APNG / WebP），决定读哪一个清晰度键。
///
/// # 返回
/// 质量请求。`h265` 暂无实现（录制进程不带 x265），按硬件 H.264 处理；配置值非法时按缺省处理。
///
/// # 示例
/// ```ignore
/// let q = quality_request(&document, false);
/// assert_eq!(q.max_size, Some((1920, 1080)));
/// ```
pub fn quality_request(document: &ConfigDocument, animated: bool) -> QualityRequest {
    let clarity_key = if animated {
        KEY_ANIMATED_CLARITY
    } else {
        KEY_CLARITY
    };
    let preset = document
        .value(KEY_PRESET)
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_ascii_lowercase);
    QualityRequest {
        max_size: document.value(clarity_key).as_str().and_then(clarity_limit),
        hardware: document.value(KEY_ENCODER).as_str() != Some(ENCODER_H264),
        preset,
        loop_animated: !matches!(document.value(KEY_LOOP), Value::Bool(false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 默认配置：视频 1080p、动图 720p、允许硬编、预设 veryfast、循环。
    #[test]
    fn defaults_follow_schema() {
        let doc = ConfigDocument::from_bytes(None);
        let video = quality_request(&doc, false);
        assert_eq!(video.max_size, Some((1920, 1080)));
        assert!(video.hardware && video.loop_animated);
        assert_eq!(video.preset.as_deref(), Some("veryfast"));
        assert_eq!(quality_request(&doc, true).max_size, Some((1280, 720)));
    }

    /// 改键后请求跟着变：软编、慢预设、不循环、动图用动图清晰度键。
    #[test]
    fn overrides_are_read() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_ENCODER, json!("h264")).unwrap();
        doc.set_value(KEY_PRESET, json!("veryslow")).unwrap();
        doc.set_value(KEY_LOOP, json!(false)).unwrap();
        doc.set_value(KEY_CLARITY, json!("4k")).unwrap();
        doc.set_value(KEY_ANIMATED_CLARITY, json!("480p")).unwrap();
        let video = quality_request(&doc, false);
        assert!(!video.hardware && !video.loop_animated);
        assert_eq!(video.preset.as_deref(), Some("veryslow"));
        assert_eq!(video.max_size, Some((3840, 2160)));
        assert_eq!(quality_request(&doc, true).max_size, Some((854, 480)));
        // h265 暂按硬件 H.264 处理
        doc.set_value(KEY_ENCODER, json!("h265")).unwrap();
        assert!(quality_request(&doc, false).hardware);
    }

    /// 清晰度表覆盖五档，未知值无上限。
    #[test]
    fn clarity_table() {
        for (name, long) in [
            ("4k", 3840),
            ("2k", 2560),
            ("1080p", 1920),
            ("720p", 1280),
            ("480p", 854),
        ] {
            assert_eq!(clarity_limit(name).map(|l| l.0), Some(long));
        }
        assert_eq!(clarity_limit("8k"), None);
    }
}
