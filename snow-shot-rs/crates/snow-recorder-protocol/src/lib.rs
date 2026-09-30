//! 主程序与录制工作进程之间的行协议（Recorder Line Protocol）。
//!
//! 每条消息占一行 UTF-8 文本，以 `\n` 结尾，字段以单个空格分隔；
//! 路径与错误原因这类自由文本永远放在行尾。零第三方依赖，两端共用同一份解析代码。
//!
//! - 主程序 → 录制进程（stdin）：[`Command`]
//! - 录制进程 → 主程序（stdout）：[`Event`]

use std::fmt;
use std::path::{Path, PathBuf};

/// 录制过程中间产物目录的名字前缀（位于最终文件同目录，隐藏目录）。
pub const SCRATCH_PREFIX: &str = ".snow-recording-";

/// 某个录制进程的中间产物目录：`<最终文件目录>/.snow-recording-<pid>`。
///
/// 录制进程把编码中的文件写在这里，完成后再原子改名到最终路径；
/// 进程崩溃时主程序按同样规则定位并清理，用户目录里不会留下半截文件。
///
/// # 参数
/// - `final_path`：最终输出路径。
/// - `pid`：录制进程 ID。
///
/// # 示例
/// ```
/// use std::path::{Path, PathBuf};
/// let dir = snow_recorder_protocol::scratch_dir(Path::new("D:/v/a.mp4"), 42);
/// assert_eq!(dir, PathBuf::from("D:/v/.snow-recording-42"));
/// ```
pub fn scratch_dir(final_path: &Path, pid: u32) -> PathBuf {
    final_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!("{SCRATCH_PREFIX}{pid}"))
}

/// 中间产物文件路径：中间目录下与最终文件同名（扩展名保持一致）。
///
/// # 参数
/// - `final_path`：最终输出路径。
/// - `pid`：录制进程 ID。
///
/// # 示例
/// ```
/// use std::path::{Path, PathBuf};
/// let f = snow_recorder_protocol::scratch_file(Path::new("D:/v/a.mp4"), 42);
/// assert_eq!(f, PathBuf::from("D:/v/.snow-recording-42/a.mp4"));
/// ```
pub fn scratch_file(final_path: &Path, pid: u32) -> PathBuf {
    let name = final_path.file_name().unwrap_or_default();
    scratch_dir(final_path, pid).join(name)
}

/// 命令字：开始录制。
const CMD_START: &str = "START";
/// 命令字：暂停。
const CMD_PAUSE: &str = "PAUSE";
/// 命令字：恢复。
const CMD_RESUME: &str = "RESUME";
/// 命令字：停止并写出文件。
const CMD_STOP: &str = "STOP";
/// 命令字：取消并丢弃。
const CMD_CANCEL: &str = "CANCEL";
/// 事件字：进程就绪。
const EVT_READY: &str = "READY";
/// 事件字：录制中状态。
const EVT_RECORDING: &str = "RECORDING";
/// 事件字：已暂停。
const EVT_PAUSED: &str = "PAUSED";
/// 事件字：已恢复。
const EVT_RESUMED: &str = "RESUMED";
/// 事件字：录制完成。
const EVT_FINISHED: &str = "FINISHED";
/// 事件字：错误。
const EVT_ERROR: &str = "ERROR";

/// 录制进程支持的输出格式（与 snow-crates 导出能力一致，不含 WebM）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MediaFormat {
    /// MP4（H.264，默认）。
    #[default]
    Mp4,
    /// GIF 动图。
    Gif,
    /// APNG 动图。
    Apng,
    /// 动画 WebP。
    Webp,
}

impl MediaFormat {
    /// 协议中的格式名，同时也是文件扩展名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Gif => "gif",
            Self::Apng => "apng",
            Self::Webp => "webp",
        }
    }

    /// 从文本解析格式；大小写不敏感。未知值（含历史遗留的 `webm`）返回 `None`。
    ///
    /// # 参数
    /// - `text`：格式名。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::MediaFormat;
    /// assert_eq!(MediaFormat::parse("GIF"), Some(MediaFormat::Gif));
    /// assert_eq!(MediaFormat::parse("webm"), None);
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "mp4" => Some(Self::Mp4),
            "gif" => Some(Self::Gif),
            "apng" => Some(Self::Apng),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }

    /// 把任意配置值归一化为受支持格式：无法识别时回落到默认格式（MP4）。
    ///
    /// # 参数
    /// - `text`：磁盘配置里读到的格式名。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::MediaFormat;
    /// assert_eq!(MediaFormat::normalize("webm"), MediaFormat::Mp4);
    /// ```
    pub fn normalize(text: &str) -> Self {
        Self::parse(text).unwrap_or_default()
    }
}

/// 开始录制请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    /// 区域左上角 X（虚拟桌面物理像素）。
    pub x: i32,
    /// 区域左上角 Y（虚拟桌面物理像素）。
    pub y: i32,
    /// 区域宽（物理像素）。
    pub width: u32,
    /// 区域高（物理像素）。
    pub height: u32,
    /// 输出格式。
    pub format: MediaFormat,
    /// 帧率。
    pub fps: u32,
    /// 是否把鼠标光标录进画面。
    pub show_cursor: bool,
    /// 最终输出路径（扩展名须与格式一致）。
    pub output: PathBuf,
}

/// 主程序发给录制进程的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 开始录制。
    Start(StartRequest),
    /// 暂停。
    Pause,
    /// 恢复。
    Resume,
    /// 停止并写出最终文件。
    Stop,
    /// 取消并丢弃产物。
    Cancel,
}

/// 录制进程回报给主程序的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 进程已启动，可接受 `Start`。
    Ready,
    /// 录制进行中（周期性）。
    Recording {
        /// 有效录制时长（毫秒，不含暂停段）。
        elapsed_ms: u64,
        /// 估算已采集帧数（按有效时长与帧率推算）。
        frames: u64,
    },
    /// 已暂停。
    Paused,
    /// 已恢复。
    Resumed,
    /// 录制完成，文件已就位。
    Finished {
        /// 最终文件路径。
        path: PathBuf,
        /// 实际编码帧数。
        frames: u64,
        /// 丢弃的采集帧数。
        dropped: u64,
    },
    /// 出错；进程随后退出。
    Error {
        /// 错误原因（单行）。
        reason: String,
    },
}

/// 协议解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    /// 输出错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "协议解析失败: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

/// 构造解析错误的便捷函数。
fn err<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(message.into()))
}

/// 把自由文本压成单行（换行替换为空格），保证一条消息一行。
///
/// # 参数
/// - `text`：原始文本。
///
/// # 示例
/// ```
/// assert_eq!(snow_recorder_protocol::single_line("a\nb\r"), "a b ");
/// ```
pub fn single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// 解析一个数值字段。
fn number<T: std::str::FromStr>(field: Option<&str>, name: &str) -> Result<T, ParseError> {
    match field.and_then(|v| v.parse::<T>().ok()) {
        Some(v) => Ok(v),
        None => err(format!("字段 {name} 缺失或不是有效数字")),
    }
}

impl Command {
    /// 序列化为一行文本（不含换行符）。
    ///
    /// # 返回
    /// 协议行。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::Command;
    /// assert_eq!(Command::Pause.to_line(), "PAUSE");
    /// ```
    pub fn to_line(&self) -> String {
        match self {
            Self::Start(r) => format!(
                "{CMD_START} {} {} {} {} {} {} {} {}",
                r.x,
                r.y,
                r.width,
                r.height,
                r.format.as_str(),
                r.fps,
                u8::from(r.show_cursor),
                single_line(&r.output.to_string_lossy()),
            ),
            Self::Pause => CMD_PAUSE.to_string(),
            Self::Resume => CMD_RESUME.to_string(),
            Self::Stop => CMD_STOP.to_string(),
            Self::Cancel => CMD_CANCEL.to_string(),
        }
    }

    /// 从一行文本解析命令。
    ///
    /// # 参数
    /// - `line`：协议行（可带行尾换行）。
    ///
    /// # 返回
    /// 命令；格式不合法时返回 [`ParseError`]。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::Command;
    /// assert_eq!(Command::parse("STOP\n").unwrap(), Command::Stop);
    /// ```
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            CMD_PAUSE => Ok(Self::Pause),
            CMD_RESUME => Ok(Self::Resume),
            CMD_STOP => Ok(Self::Stop),
            CMD_CANCEL => Ok(Self::Cancel),
            CMD_START => {
                let mut it = rest.splitn(8, ' ');
                let x = number(it.next(), "x")?;
                let y = number(it.next(), "y")?;
                let width = number(it.next(), "width")?;
                let height = number(it.next(), "height")?;
                let Some(format) = it.next().and_then(MediaFormat::parse) else {
                    return err("字段 format 缺失或不受支持");
                };
                let fps = number(it.next(), "fps")?;
                let cursor: u8 = number(it.next(), "cursor")?;
                let Some(path) = it.next().filter(|p| !p.is_empty()) else {
                    return err("字段 output 缺失");
                };
                Ok(Self::Start(StartRequest {
                    x,
                    y,
                    width,
                    height,
                    format,
                    fps,
                    show_cursor: cursor != 0,
                    output: PathBuf::from(path),
                }))
            }
            other => err(format!("未知命令: {other}")),
        }
    }
}

impl Event {
    /// 序列化为一行文本（不含换行符）。
    ///
    /// # 返回
    /// 协议行。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::Event;
    /// assert_eq!(Event::Ready.to_line(), "READY");
    /// ```
    pub fn to_line(&self) -> String {
        match self {
            Self::Ready => EVT_READY.to_string(),
            Self::Recording { elapsed_ms, frames } => {
                format!("{EVT_RECORDING} {elapsed_ms} {frames}")
            }
            Self::Paused => EVT_PAUSED.to_string(),
            Self::Resumed => EVT_RESUMED.to_string(),
            Self::Finished {
                path,
                frames,
                dropped,
            } => format!(
                "{EVT_FINISHED} {frames} {dropped} {}",
                single_line(&path.to_string_lossy())
            ),
            Self::Error { reason } => format!("{EVT_ERROR} {}", single_line(reason)),
        }
    }

    /// 从一行文本解析事件。
    ///
    /// # 参数
    /// - `line`：协议行（可带行尾换行）。
    ///
    /// # 返回
    /// 事件；格式不合法时返回 [`ParseError`]。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::Event;
    /// assert_eq!(
    ///     Event::parse("RECORDING 1500 45").unwrap(),
    ///     Event::Recording { elapsed_ms: 1500, frames: 45 }
    /// );
    /// ```
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            EVT_READY => Ok(Self::Ready),
            EVT_PAUSED => Ok(Self::Paused),
            EVT_RESUMED => Ok(Self::Resumed),
            EVT_RECORDING => {
                let mut it = rest.split(' ');
                Ok(Self::Recording {
                    elapsed_ms: number(it.next(), "elapsed_ms")?,
                    frames: number(it.next(), "frames")?,
                })
            }
            EVT_FINISHED => {
                let mut it = rest.splitn(3, ' ');
                let frames = number(it.next(), "frames")?;
                let dropped = number(it.next(), "dropped")?;
                let Some(path) = it.next().filter(|p| !p.is_empty()) else {
                    return err("字段 path 缺失");
                };
                Ok(Self::Finished {
                    path: PathBuf::from(path),
                    frames,
                    dropped,
                })
            }
            EVT_ERROR => Ok(Self::Error {
                reason: rest.to_string(),
            }),
            other => err(format!("未知事件: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个测试用开始请求。
    fn sample_start() -> StartRequest {
        StartRequest {
            x: -1920,
            y: 10,
            width: 2560,
            height: 1440,
            format: MediaFormat::Gif,
            fps: 30,
            show_cursor: true,
            output: PathBuf::from("C:\\My Videos\\a b.gif"),
        }
    }

    /// 命令往返：含空格路径与负坐标。
    #[test]
    fn command_round_trip() {
        for cmd in [
            Command::Start(sample_start()),
            Command::Pause,
            Command::Resume,
            Command::Stop,
            Command::Cancel,
        ] {
            assert_eq!(Command::parse(&cmd.to_line()).unwrap(), cmd);
        }
    }

    /// 事件往返。
    #[test]
    fn event_round_trip() {
        for evt in [
            Event::Ready,
            Event::Recording {
                elapsed_ms: 1234,
                frames: 37,
            },
            Event::Paused,
            Event::Resumed,
            Event::Finished {
                path: PathBuf::from("D:\\v ideo\\x.mp4"),
                frames: 150,
                dropped: 2,
            },
            Event::Error {
                reason: "采集失败 code 5".into(),
            },
        ] {
            assert_eq!(Event::parse(&evt.to_line()).unwrap(), evt);
        }
    }

    /// 非法输入一律返回错误，不 panic。
    #[test]
    fn malformed_lines_are_rejected() {
        for bad in [
            "",
            "START",
            "START 1 2 3",
            "START a b c d mp4 30 1 x.mp4",
            "START 0 0 10 10 webm 30 1 x.webm",
            "START 0 0 10 10 mp4 30 1 ",
            "BOGUS",
        ] {
            assert!(Command::parse(bad).is_err(), "应拒绝: {bad:?}");
        }
        for bad in ["", "RECORDING x y", "FINISHED 1 2", "NOPE 1"] {
            assert!(Event::parse(bad).is_err(), "应拒绝: {bad:?}");
        }
    }

    /// 错误原因里的换行不会破坏“一行一条”。
    #[test]
    fn error_reason_is_single_line() {
        let line = Event::Error {
            reason: "a\nb".into(),
        }
        .to_line();
        assert!(!line.contains('\n'));
    }

    /// 中间产物路径与最终文件同目录、同名。
    #[test]
    fn scratch_paths_follow_final_path() {
        let final_path = Path::new("D:/v/a b.gif");
        assert_eq!(scratch_dir(final_path, 7), PathBuf::from("D:/v/.snow-recording-7"));
        assert_eq!(
            scratch_file(final_path, 7),
            PathBuf::from("D:/v/.snow-recording-7/a b.gif")
        );
        // 无目录的相对路径也不会 panic
        assert_eq!(scratch_dir(Path::new("a.mp4"), 1), PathBuf::from(".snow-recording-1"));
    }

    /// 历史遗留的 webm 归一化为默认格式。
    #[test]
    fn webm_normalizes_to_default() {
        assert_eq!(MediaFormat::normalize("webm"), MediaFormat::Mp4);
        assert_eq!(MediaFormat::normalize("WEBP"), MediaFormat::Webp);
        assert_eq!(MediaFormat::normalize(""), MediaFormat::Mp4);
    }
}
