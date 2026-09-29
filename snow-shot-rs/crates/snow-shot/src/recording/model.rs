//! 屏幕录制模型与配置规范（Recording Model）。
//!
//! 定义录制格式、音频捕获开关、键鼠特效配置、录制状态机以及输出元数据。

use std::fmt;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use snow_ui::shell::geometry::PhysicalRect;

/// 视频封装输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RecordingFormat {
    /// MP4 视频（H.264 / AAC，默认）。
    #[default]
    Mp4,
    /// 动图 GIF 格式。
    Gif,
    /// WebM 格式（VP9 / Opus）。
    WebM,
}

impl RecordingFormat {
    /// 文件拓展名。
    pub const fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Gif => "gif",
            Self::WebM => "webm",
        }
    }

    /// 显示名称。
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4 视频",
            Self::Gif => "GIF 动图",
            Self::WebM => "WebM 视频",
        }
    }
}

/// 录制目标区域与编码参数配置。
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingConfig {
    /// 捕获物理矩形区域。
    pub region: PhysicalRect,
    /// 帧率 (FPS)。
    pub fps: u32,
    /// 是否录制麦克风输入。
    pub record_microphone: bool,
    /// 是否录制系统扬声器声音（Loopback）。
    pub record_system_audio: bool,
    /// 是否开启鼠标点击水波纹特效。
    pub show_mouse_clicks: bool,
    /// 是否开启键盘按键屏幕回显。
    pub show_keystrokes: bool,
    /// 输出格式。
    pub format: RecordingFormat,
    /// 视频输出文件保存路径。
    pub output_path: PathBuf,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            region: PhysicalRect::new(0, 0, 1920, 1080),
            fps: 60,
            record_microphone: false,
            record_system_audio: true,
            show_mouse_clicks: true,
            show_keystrokes: false,
            format: RecordingFormat::Mp4,
            output_path: PathBuf::from("recording.mp4"),
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
        /// 已录制累计时长（秒）。
        elapsed_secs: u64,
        /// 当前是否处于暂停状态。
        is_paused: bool,
        /// 累计捕获帧数。
        frames_captured: u64,
    },
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
        /// 错误原因描述。
        reason: String,
    },
}

impl RecordingState {
    /// 判断是否处于活动录制状态（含暂停）。
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Recording { .. })
    }

    /// 格式化录制时间显示为 `HH:MM:SS` 或 `MM:SS`。
    pub fn format_duration(seconds: u64) -> String {
        let hrs = seconds / 3600;
        let mins = (seconds % 3600) / 60;
        let secs = seconds % 60;

        if hrs > 0 {
            format!("{:02}:{:02}:{:02}", hrs, mins, secs)
        } else {
            format!("{:02}:{:02}", mins, secs)
        }
    }
}

impl fmt::Display for RecordingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "就绪"),
            Self::Countdown { seconds_left } => write!(f, "倒计时: {seconds_left}"),
            Self::Recording {
                elapsed_secs,
                is_paused,
                ..
            } => {
                let status = if *is_paused { "已暂停" } else { "录制中" };
                write!(
                    f,
                    "{} [{}]",
                    status,
                    Self::format_duration(*elapsed_secs)
                )
            }
            Self::Finished { file_path, .. } => {
                write!(f, "已完成: {}", file_path.display())
            }
            Self::Error { reason } => write!(f, "录制失败: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证录制时间格式化输出。
    #[test]
    fn test_format_duration() {
        assert_eq!(RecordingState::format_duration(5), "00:05");
        assert_eq!(RecordingState::format_duration(65), "01:05");
        assert_eq!(RecordingState::format_duration(3665), "01:01:05");
    }

    /// 验证状态机活跃判定。
    #[test]
    fn test_state_is_active() {
        assert!(!RecordingState::Idle.is_active());
        assert!(!RecordingState::Countdown { seconds_left: 3 }.is_active());
        assert!(
            RecordingState::Recording {
                elapsed_secs: 10,
                is_paused: false,
                frames_captured: 600,
            }
            .is_active()
        );
        assert!(
            RecordingState::Recording {
                elapsed_secs: 10,
                is_paused: true,
                frames_captured: 600,
            }
            .is_active()
        );
    }
}
