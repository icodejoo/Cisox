//! 录制输出：从配置文档解析保存目录、文件名与录制参数。

use crate::recording::audio::audio_request;
use crate::recording::model::{DEFAULT_FPS, RecordingConfig, RecordingFormat};
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_platform::local_time::LocalDateTime;
use snow_ui::shell::geometry::PhysicalRect;
use std::path::{Path, PathBuf};

/// 输出格式配置键。
pub const KEY_FORMAT: &str = "screen_recording/output_format";
/// 帧率配置键（视频）。
pub const KEY_FPS: &str = "screen_recording/frame_rate";
/// 动图帧率配置键（GIF / APNG / WebP 使用）。
pub const KEY_ANIMATED_FPS: &str = "screen_recording/animated_image_frame_rate";
/// 是否录制光标配置键。
pub const KEY_SHOW_CURSOR: &str = "screen_recording/show_cursor";
/// 开始延迟（秒）配置键。
pub const KEY_START_DELAY: &str = "screen_recording/start_delay_seconds";
/// 保存目录配置键。
pub const KEY_SAVE_DIR: &str = "screen_recording/video_save_directory";
/// 文件名格式配置键。
pub const KEY_FILENAME_FORMAT: &str = "screen_recording/video_filename_format";
/// 默认视频子目录名（位于用户目录下）。
const VIDEOS_DIR_NAME: &str = "Videos";
/// 文件名兜底主名（模板展开后为空时使用）。
const FALLBACK_STEM: &str = "Recording";
/// 同名文件最多尝试的序号。
const MAX_NAME_ATTEMPTS: u32 = 1000;
/// 文件名里不允许出现的字符。
const INVALID_FILENAME_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// 展开文件名模板：`{YYYY-MM-DD_HH-mm-ss}` 这类花括号内的 `YYYY MM DD HH mm ss` 记号按时间替换，
/// 其余文字原样保留；Windows 非法字符替换为 `_`。
///
/// # 参数
/// - `template`：文件名格式。
/// - `now`：当前本地时间。
///
/// # 返回
/// 不含扩展名的文件主名；结果为空时返回兜底名。
///
/// # 示例
/// ```ignore
/// let t = LocalDateTime { year: 2026, month: 9, day: 30, hour: 8, minute: 5, second: 3 };
/// assert_eq!(expand_filename("Snow_{YYYY-MM-DD_HH-mm-ss}", t), "Snow_2026-09-30_08-05-03");
/// ```
pub fn expand_filename(template: &str, now: LocalDateTime) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                out.push_str(&expand_tokens(&after[..end], now));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let cleaned: String = out
        .chars()
        .map(|c| {
            if INVALID_FILENAME_CHARS.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        FALLBACK_STEM.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 替换花括号内的时间记号。
fn expand_tokens(body: &str, now: LocalDateTime) -> String {
    body.replace("YYYY", &format!("{:04}", now.year))
        .replace("MM", &format!("{:02}", now.month))
        .replace("DD", &format!("{:02}", now.day))
        .replace("HH", &format!("{:02}", now.hour))
        .replace("mm", &format!("{:02}", now.minute))
        .replace("ss", &format!("{:02}", now.second))
}

/// 解析录制保存目录：配置优先；为空则 `<用户目录>/Videos`；都没有用系统临时目录。
///
/// # 参数
/// - `document`：配置文档。
/// - `home`：用户目录。
pub fn resolve_video_directory(document: &ConfigDocument, home: Option<&Path>) -> PathBuf {
    if let Value::String(text) = document.value(KEY_SAVE_DIR) {
        let text = text.trim();
        if !text.is_empty() {
            return PathBuf::from(text);
        }
    }
    match home {
        Some(home) => home.join(VIDEOS_DIR_NAME),
        None => std::env::temp_dir(),
    }
}

/// 生成不与已有文件冲突的输出路径：`<目录>/<主名>[-序号].<扩展名>`。
///
/// # 参数
/// - `dir`：输出目录。
/// - `stem`：文件主名。
/// - `extension`：扩展名（不含点）。
///
/// # 返回
/// 尚不存在的路径；序号耗尽返回错误。
pub fn unique_path(dir: &Path, stem: &str, extension: &str) -> Result<PathBuf, String> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let name = if attempt == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{attempt}.{extension}")
        };
        let path = dir.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("could not find a file name that does not collide".to_string())
}

/// 读取布尔配置，非布尔时回退。
fn bool_of(document: &ConfigDocument, key: &str, fallback: bool) -> bool {
    document.value(key).as_bool().unwrap_or(fallback)
}

/// 读取正整数配置，缺失或非法时回退。
fn u32_of(document: &ConfigDocument, key: &str, fallback: u32) -> u32 {
    document
        .value(key)
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(fallback)
}

/// 由配置文档与选区构造录制配置（含不冲突的输出路径）。
///
/// # 参数
/// - `document`：配置文档。
/// - `region`：录制区域（虚拟桌面物理坐标）。
/// - `home`：用户目录。
/// - `now`：当前本地时间（用于文件名）。
///
/// # 返回
/// 录制配置；无法生成输出文件名时返回错误说明。
///
/// # 示例
/// ```ignore
/// let cfg = build_recording_config(&doc, PhysicalRect::new(0, 0, 800, 600), home, now)?;
/// assert_eq!(cfg.region.width, 800);
/// ```
pub fn build_recording_config(
    document: &ConfigDocument,
    region: PhysicalRect,
    home: Option<&Path>,
    now: LocalDateTime,
) -> Result<RecordingConfig, String> {
    let format =
        RecordingFormat::from_config(document.value(KEY_FORMAT).as_str().unwrap_or_default());
    // 视频与动图各有一套帧率配置
    let fps_key = if format == RecordingFormat::Mp4 {
        KEY_FPS
    } else {
        KEY_ANIMATED_FPS
    };
    let dir = resolve_video_directory(document, home);
    let template = document.value(KEY_FILENAME_FORMAT);
    let stem = expand_filename(template.as_str().unwrap_or_default(), now);
    let output_path = unique_path(&dir, &stem, format.extension())?;
    Ok(RecordingConfig {
        region,
        fps: u32_of(document, fps_key, DEFAULT_FPS),
        show_cursor: bool_of(document, KEY_SHOW_CURSOR, true),
        format,
        output_path,
        countdown_secs: document
            .value(KEY_START_DELAY)
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        audio: audio_request(document, format),
        effects: crate::recording::effects::effects_request(document),
        quality: crate::recording::quality::quality_request(
            document,
            format != RecordingFormat::Mp4,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定测试时间。
    fn t() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 9,
            day: 30,
            hour: 8,
            minute: 5,
            second: 3,
        }
    }

    /// 时间记号展开、非法字符替换、空结果兜底。
    #[test]
    fn filename_expansion() {
        assert_eq!(
            expand_filename("Cisox_Video_{YYYY-MM-DD_HH-mm-ss}", t()),
            "Cisox_Video_2026-09-30_08-05-03"
        );
        assert_eq!(expand_filename("a:b*c", t()), "a_b_c");
        assert_eq!(expand_filename("   ", t()), FALLBACK_STEM);
        // 未闭合的花括号原样保留
        assert_eq!(expand_filename("x{YYYY", t()), "x{YYYY");
    }

    /// 保存目录：配置优先，其次用户目录/Videos，最后临时目录。
    #[test]
    fn video_directory_fallbacks() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_SAVE_DIR, Value::String(String::new()))
            .unwrap();
        assert_eq!(
            resolve_video_directory(&doc, Some(Path::new("C:/Users/a"))),
            Path::new("C:/Users/a").join("Videos")
        );
        assert_eq!(resolve_video_directory(&doc, None), std::env::temp_dir());
        doc.set_value(KEY_SAVE_DIR, Value::String("D:/rec".into()))
            .unwrap();
        assert_eq!(resolve_video_directory(&doc, None), PathBuf::from("D:/rec"));
    }

    /// 同名文件存在时自动加序号。
    #[test]
    fn unique_path_avoids_collision() {
        let dir = std::env::temp_dir().join(format!("snow-rec-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a", "mp4").unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "a", "mp4").unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("a-1.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置构造：读取格式、帧率、光标、延迟；动图使用动图帧率键。
    #[test]
    fn config_from_document() {
        let dir = std::env::temp_dir().join(format!("snow-rec-cfg-{}", std::process::id()));
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(
            KEY_SAVE_DIR,
            Value::String(dir.to_string_lossy().into_owned()),
        )
        .unwrap();
        doc.set_value(KEY_FORMAT, Value::String("gif".into()))
            .unwrap();
        doc.set_value(KEY_START_DELAY, serde_json::json!(3))
            .unwrap();
        doc.set_value(KEY_SHOW_CURSOR, serde_json::json!(false))
            .unwrap();
        let cfg =
            build_recording_config(&doc, PhysicalRect::new(1, 2, 300, 200), None, t()).unwrap();
        assert_eq!(cfg.format, RecordingFormat::Gif);
        assert_eq!(cfg.countdown_secs, 3);
        assert!(!cfg.show_cursor);
        assert_eq!(cfg.region, PhysicalRect::new(1, 2, 300, 200));
        assert_eq!(
            cfg.output_path.extension().and_then(|e| e.to_str()),
            Some("gif")
        );
        assert!(cfg.fps > 0);
        // 动图格式强制不录音频
        assert!(!cfg.audio.enabled());
    }

    /// MP4 时音频请求来自两个旧键，且每次构造都读最新值。
    #[test]
    fn config_audio_follows_latest_switches() {
        let dir = std::env::temp_dir().join(format!("snow-rec-aud-{}", std::process::id()));
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(
            KEY_SAVE_DIR,
            Value::String(dir.to_string_lossy().into_owned()),
        )
        .unwrap();
        let region = PhysicalRect::new(0, 0, 100, 100);
        let first = build_recording_config(&doc, region, None, t()).unwrap();
        assert!(!first.audio.microphone && first.audio.system);
        doc.set_value(
            "screen_recording/enable_microphone",
            serde_json::json!(true),
        )
        .unwrap();
        doc.set_value(
            "screen_recording/enable_system_audio",
            serde_json::json!(false),
        )
        .unwrap();
        let second = build_recording_config(&doc, region, None, t()).unwrap();
        assert!(second.audio.microphone && !second.audio.system);
    }
}
