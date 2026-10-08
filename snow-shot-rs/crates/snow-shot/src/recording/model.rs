//! 屏幕录制模型与配置规范（Recording Model）。
//!
//! 定义录制格式、录制配置与录制状态机的状态枚举。
//! 支持的格式以 snow-crates 的导出能力为准：MP4 / GIF / APNG / 动画 WebP（不含 WebM，见 docs/cisox-todo-webm.md）。

use serde::{Deserialize, Serialize};
use snow_i18n::{Args, I18n};
use snow_recorder_protocol::{AudioRequest, EffectsRequest, MediaFormat};
use snow_ui::shell::geometry::PhysicalRect;
use std::fmt;
use std::path::PathBuf;

/// 默认录制帧率。
pub const DEFAULT_FPS: u32 = 30;
/// 默认录制区域宽（仅 `Default` 使用）。
const DEFAULT_REGION_WIDTH: i32 = 1920;
/// 默认录制区域高（仅 `Default` 使用）。
const DEFAULT_REGION_HEIGHT: i32 = 1080;
/// 每小时秒数。
const SECS_PER_HOUR: u64 = 3600;
/// 每分钟秒数。
const SECS_PER_MINUTE: u64 = 60;

/// 输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RecordingFormat {
    /// MP4 视频（H.264，默认）。
    #[default]
    Mp4,
    /// GIF 动图。
    Gif,
    /// APNG 动图。
    Apng,
    /// 动画 WebP。
    Webp,
}

impl RecordingFormat {
    /// 文件拓展名。
    pub const fn extension(&self) -> &'static str {
        self.to_media().as_str()
    }

    /// 显示名称（随界面语言）。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn label(&self, i18n: &I18n) -> String {
        i18n.tr(match self {
            Self::Mp4 => "recording-format-mp4",
            Self::Gif => "recording-format-gif",
            Self::Apng => "recording-format-apng",
            Self::Webp => "recording-format-webp",
        })
    }

    /// 转为协议格式。
    pub const fn to_media(self) -> MediaFormat {
        match self {
            Self::Mp4 => MediaFormat::Mp4,
            Self::Gif => MediaFormat::Gif,
            Self::Apng => MediaFormat::Apng,
            Self::Webp => MediaFormat::Webp,
        }
    }

    /// 从配置文本归一化：无法识别（含磁盘里遗留的 `webm`）一律回落默认格式，不报错。
    ///
    /// # 参数
    /// - `text`：配置里的格式名。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(RecordingFormat::from_config("webm"), RecordingFormat::Mp4);
    /// assert_eq!(RecordingFormat::from_config("GIF"), RecordingFormat::Gif);
    /// ```
    pub fn from_config(text: &str) -> Self {
        match MediaFormat::normalize(text) {
            MediaFormat::Mp4 => Self::Mp4,
            MediaFormat::Gif => Self::Gif,
            MediaFormat::Apng => Self::Apng,
            MediaFormat::Webp => Self::Webp,
        }
    }
}

/// 录制目标区域与编码参数配置。
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingConfig {
    /// 捕获物理矩形区域（虚拟桌面坐标）。
    pub region: PhysicalRect,
    /// 帧率 (FPS)。
    pub fps: u32,
    /// 是否把鼠标光标录进画面。
    pub show_cursor: bool,
    /// 输出格式。
    pub format: RecordingFormat,
    /// 视频输出文件保存路径。
    pub output_path: PathBuf,
    /// 开始前倒计时秒数（0 = 立即开始）。
    pub countdown_secs: u32,
    /// 音频请求（仅 MP4 生效，其余格式恒为全关）。
    pub audio: AudioRequest,
    /// 输入特效请求（鼠标轨迹 / 点击 / 高亮、按键回显）。
    pub effects: EffectsRequest,
}

impl Default for RecordingConfig {
    /// 默认配置：1920x1080、30fps、带光标、MP4、无倒计时。
    fn default() -> Self {
        Self {
            region: PhysicalRect::new(0, 0, DEFAULT_REGION_WIDTH, DEFAULT_REGION_HEIGHT),
            fps: DEFAULT_FPS,
            show_cursor: true,
            format: RecordingFormat::Mp4,
            output_path: PathBuf::from("recording.mp4"),
            countdown_secs: 0,
            audio: AudioRequest::default(),
            effects: EffectsRequest::default(),
        }
    }
}

/// 录制生命周期状态枚举。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RecordingState {
    /// 空闲待命状态。
    #[default]
    Idle,
    /// 倒计时准备状态。
    Countdown {
        /// 剩余倒计时秒数（例如 3, 2, 1）。
        seconds_left: u32,
    },
    /// 正在录制中。
    Recording {
        /// 已录制有效时长（秒，不含暂停段）。
        elapsed_secs: u64,
        /// 当前是否处于暂停状态。
        is_paused: bool,
        /// 估算已采集帧数。
        frames_captured: u64,
    },
    /// 已发出停止指令，等待录制进程写完文件。
    Saving,
    /// 录制完成，已输出媒体文件。
    Finished {
        /// 输出文件物理路径。
        file_path: PathBuf,
        /// 总时长（秒）。
        duration_secs: u64,
        /// 文件大小（字节）。
        file_size_bytes: u64,
    },
    /// 录制发生异常终止。
    Error {
        /// 结构化的错误原因（界面边界再翻译）。
        reason: RecordingFailure,
    },
}

/// 录制失败的原因（界面边界再翻译，附带的技术细节原样保留）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingFailure {
    /// 找不到录制进程可执行文件。
    RecorderMissing,
    /// 无法启动录制进程。
    SpawnFailed {
        /// 可执行文件路径。
        exe: String,
        /// 系统给出的原因。
        detail: String,
    },
    /// 录制进程意外退出。
    ProcessExited {
        /// 退出码（未知为 `None`）。
        code: Option<i32>,
    },
    /// 与录制进程的通信失败。
    LinkFailed(String),
    /// 录制进程启动后长时间没有回应。
    NoResponse,
    /// 保存录制文件超时。
    SaveTimeout,
    /// 录制进程自己报告的错误（技术信息）。
    Worker(String),
}

impl RecordingFailure {
    /// 面向用户的失败说明。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::RecorderMissing => i18n.tr("recording-error-recorder-missing"),
            Self::SpawnFailed { exe, detail } => i18n.tr_with(
                "recording-error-spawn-failed",
                &Args::new()
                    .named("exe", exe.as_str())
                    .named("detail", detail.as_str()),
            ),
            Self::ProcessExited { code: Some(code) } => i18n.tr_with(
                "recording-error-process-exited",
                &Args::new().named("code", code.to_string()),
            ),
            Self::ProcessExited { code: None } => i18n.tr("recording-error-process-exited-unknown"),
            Self::LinkFailed(detail) => i18n.tr_with(
                "recording-error-link-failed",
                &Args::new().named("detail", detail.as_str()),
            ),
            Self::NoResponse => i18n.tr("recording-error-no-response"),
            Self::SaveTimeout => i18n.tr("recording-error-save-timeout"),
            Self::Worker(detail) => i18n.tr_with(
                "recording-error-worker",
                &Args::new().named("detail", detail.as_str()),
            ),
        }
    }
}

impl RecordingState {
    /// 判断是否处于活动录制状态（含暂停）。
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Recording { .. })
    }

    /// 会话是否已到终态（完成、出错），不再需要录制进程。
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Error { .. })
    }

    /// 格式化录制时间显示为 `HH:MM:SS` 或 `MM:SS`。
    ///
    /// # 参数
    /// - `seconds`：秒数。
    pub fn format_duration(seconds: u64) -> String {
        let hrs = seconds / SECS_PER_HOUR;
        let mins = (seconds % SECS_PER_HOUR) / SECS_PER_MINUTE;
        let secs = seconds % SECS_PER_MINUTE;

        if hrs > 0 {
            format!("{hrs:02}:{mins:02}:{secs:02}")
        } else {
            format!("{mins:02}:{secs:02}")
        }
    }
}

impl fmt::Display for RecordingState {
    /// 状态的技术描述（日志用，不翻译）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Countdown { seconds_left } => write!(f, "countdown: {seconds_left}"),
            Self::Recording {
                elapsed_secs,
                is_paused,
                ..
            } => {
                let status = if *is_paused { "paused" } else { "recording" };
                write!(f, "{} [{}]", status, Self::format_duration(*elapsed_secs))
            }
            Self::Saving => write!(f, "saving"),
            Self::Finished { file_path, .. } => {
                write!(f, "finished: {}", file_path.display())
            }
            Self::Error { reason } => write!(f, "failed: {reason:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_config::document::ConfigDocument;

    /// 失败原因与格式名在两种界面语言下都完整：英文无中文、没有缺失标记，参数被代入。
    #[test]
    fn failures_and_formats_are_localized() {
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        let failures = [
            RecordingFailure::RecorderMissing,
            RecordingFailure::SpawnFailed {
                exe: "snow-recorder.exe".into(),
                detail: "denied".into(),
            },
            RecordingFailure::ProcessExited { code: Some(3) },
            RecordingFailure::ProcessExited { code: None },
            RecordingFailure::LinkFailed("pipe closed".into()),
            RecordingFailure::NoResponse,
            RecordingFailure::SaveTimeout,
        ];
        for failure in &failures {
            let (z, e) = (failure.message(zh), failure.message(en));
            assert!(
                e.is_ascii() && !e.contains("[!") && !z.contains("[!"),
                "{failure:?}: {e}"
            );
            assert_ne!(z, e, "{failure:?}");
        }
        assert!(
            RecordingFailure::ProcessExited { code: Some(3) }
                .message(en)
                .contains('3')
        );
        assert_eq!(
            RecordingFailure::Worker("技术信息".into()).message(en),
            "技术信息"
        );
        for format in [
            RecordingFormat::Mp4,
            RecordingFormat::Gif,
            RecordingFormat::Apng,
            RecordingFormat::Webp,
        ] {
            assert!(format.label(en).is_ascii() && !format.label(en).contains("[!"));
            assert_ne!(format.label(zh), format.label(en));
        }
    }

    /// 验证录制时间格式化输出。
    #[test]
    fn test_format_duration() {
        assert_eq!(RecordingState::format_duration(5), "00:05");
        assert_eq!(RecordingState::format_duration(65), "01:05");
        assert_eq!(RecordingState::format_duration(3665), "01:01:05");
    }

    /// 验证状态机活跃判定与终态判定。
    #[test]
    fn test_state_predicates() {
        assert!(!RecordingState::Idle.is_active());
        assert!(!RecordingState::Countdown { seconds_left: 3 }.is_active());
        let recording = RecordingState::Recording {
            elapsed_secs: 10,
            is_paused: true,
            frames_captured: 600,
        };
        assert!(recording.is_active());
        assert!(!recording.is_terminal());
        assert!(!RecordingState::Saving.is_terminal());
        assert!(
            RecordingState::Error {
                reason: RecordingFailure::NoResponse
            }
            .is_terminal()
        );
    }

    /// 格式：扩展名、协议往返；不含 WebM。
    #[test]
    fn formats_map_to_protocol() {
        for f in [
            RecordingFormat::Mp4,
            RecordingFormat::Gif,
            RecordingFormat::Apng,
            RecordingFormat::Webp,
        ] {
            assert_eq!(RecordingFormat::from_config(f.extension()), f);
        }
        assert_eq!(RecordingFormat::Webp.extension(), "webp");
    }

    /// 遗留的 webm 或乱码值归一化为 MP4，不报错。
    #[test]
    fn legacy_webm_normalizes_to_default() {
        assert_eq!(RecordingFormat::from_config("webm"), RecordingFormat::Mp4);
        assert_eq!(RecordingFormat::from_config("WebM"), RecordingFormat::Mp4);
        assert_eq!(RecordingFormat::from_config(""), RecordingFormat::Mp4);
    }

    /// 磁盘配置里已有 webm：读取后回落到默认格式，且文档不崩溃。
    #[test]
    fn disk_config_with_webm_is_recovered() {
        let json = format!(
            r#"{{"storage":{{"schema_version":{}}},"screen_recording":{{"output_format":"webm"}}}}"#,
            snow_config::schema::current_version()
        );
        let doc = ConfigDocument::from_bytes(Some(json.as_bytes()));
        let value = doc.value("screen_recording/output_format");
        assert_eq!(value, serde_json::json!("mp4"));
        assert_eq!(
            RecordingFormat::from_config(value.as_str().unwrap_or_default()),
            RecordingFormat::Mp4
        );
    }
}
