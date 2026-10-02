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
/// 命令字：提交编辑任务。
const CMD_EDIT: &str = "EDIT";
/// 命令字：探测视频信息。
const CMD_PROBE: &str = "PROBE";
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
/// 事件字：编辑进度。
const EVT_EDIT_PROGRESS: &str = "EDIT_PROGRESS";
/// 事件字：编辑完成。
const EVT_EDIT_FINISHED: &str = "EDIT_FINISHED";
/// 事件字：探测结果。
const EVT_PROBE_RESULT: &str = "PROBE_RESULT";
/// 编辑命令里输入与输出路径之间的分隔符（Windows 路径不会含制表符）。
const PATH_SEP: char = '\t';

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

/// 编辑引擎选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EngineKind {
    /// 自动：系统引擎优先，不可用或不支持时回落 FFmpeg。
    #[default]
    Auto,
    /// 系统引擎（Windows 为 Media Foundation / WIC）。
    System,
    /// FFmpeg 引擎。
    Ffmpeg,
}

impl EngineKind {
    /// 协议中的引擎名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::System => "system",
            Self::Ffmpeg => "ffmpeg",
        }
    }

    /// 从文本解析引擎名；大小写不敏感，未知值返回 `None`。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::EngineKind;
    /// assert_eq!(EngineKind::parse("FFmpeg"), Some(EngineKind::Ffmpeg));
    /// assert_eq!(EngineKind::parse("x"), None);
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "system" => Some(Self::System),
            "ffmpeg" => Some(Self::Ffmpeg),
            _ => None,
        }
    }
}

/// 抽帧导出的图片格式（不含有损 WebP，见 MVP 设计 §11 第 4 条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFormat {
    /// PNG（无损）。
    #[default]
    Png,
    /// JPEG（有损，质量见请求的 `quality`）。
    Jpeg,
    /// 无损 WebP。
    WebpLossless,
}

impl ImageFormat {
    /// 协议中的格式名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::WebpLossless => "webp-lossless",
        }
    }

    /// 输出文件扩展名。
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebpLossless => "webp",
        }
    }

    /// 从文本解析格式；大小写不敏感，有损 WebP 等未知值返回 `None`。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::ImageFormat;
    /// assert_eq!(ImageFormat::parse("JPEG"), Some(ImageFormat::Jpeg));
    /// assert_eq!(ImageFormat::parse("webp"), None);
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "webp-lossless" => Some(Self::WebpLossless),
            _ => None,
        }
    }
}

/// 抽帧模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMode {
    /// 单帧：取时间戳 `at_ms` 处正在显示的那一帧。
    Single {
        /// 目标时间（毫秒，相对视频起点）。
        at_ms: u64,
    },
    /// 按固定间隔：从 0 起每隔 `every_ms` 取一帧。
    Interval {
        /// 间隔（毫秒，大于 0）。
        every_ms: u64,
    },
    /// 仅关键帧。
    Keyframes,
}

impl ExtractMode {
    /// 序列化为不含空白与逗号的记号，如 `single=1500`。
    pub fn to_token(self) -> String {
        match self {
            Self::Single { at_ms } => format!("single={at_ms}"),
            Self::Interval { every_ms } => format!("interval={every_ms}"),
            Self::Keyframes => "keyframes".to_string(),
        }
    }

    /// 从记号解析；间隔为 0 视为非法。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::ExtractMode;
    /// assert_eq!(ExtractMode::parse_token("interval=500"), Ok(ExtractMode::Interval { every_ms: 500 }));
    /// assert!(ExtractMode::parse_token("interval=0").is_err());
    /// ```
    pub fn parse_token(token: &str) -> Result<Self, ParseError> {
        if token == "keyframes" {
            return Ok(Self::Keyframes);
        }
        match token.split_once('=') {
            Some(("single", v)) => Ok(Self::Single {
                at_ms: number(Some(v), "at_ms")?,
            }),
            Some(("interval", v)) => {
                let every_ms: u64 = number(Some(v), "every_ms")?;
                if every_ms == 0 {
                    return err("间隔必须大于 0");
                }
                Ok(Self::Interval { every_ms })
            }
            _ => err(format!("未知抽帧模式: {token}")),
        }
    }
}

/// 编辑操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp {
    /// 降低帧率（按 PTS 网格丢帧，重编码为 H.264）。
    ReduceFps {
        /// 目标帧率。
        target_fps: u32,
    },
    /// 缩放（保持宽高比并偶数对齐由引擎负责）。
    Scale {
        /// 目标宽。
        width: u32,
        /// 目标高。
        height: u32,
    },
    /// 抽帧导出图片；请求的 `output` 为输出目录。
    ExtractFrames {
        /// 抽帧模式。
        mode: ExtractMode,
        /// 图片格式。
        format: ImageFormat,
        /// 质量 1..=100，仅 JPEG 使用。
        quality: u8,
    },
    /// 按关键帧无重编码裁剪。
    TrimKeyframe {
        /// 起点（毫秒）。
        start_ms: u64,
        /// 终点（毫秒）。
        end_ms: u64,
    },
}

impl EditOp {
    /// 序列化为不含空白的记号（逗号分隔），如 `frames,single=1500,png,90`。
    pub fn to_token(self) -> String {
        match self {
            Self::ReduceFps { target_fps } => format!("fps,{target_fps}"),
            Self::Scale { width, height } => format!("scale,{width},{height}"),
            Self::ExtractFrames {
                mode,
                format,
                quality,
            } => {
                format!("frames,{},{},{quality}", mode.to_token(), format.as_str())
            }
            Self::TrimKeyframe { start_ms, end_ms } => format!("trim,{start_ms},{end_ms}"),
        }
    }

    /// 从记号解析；字段缺失或非法返回 [`ParseError`]。
    ///
    /// # 示例
    /// ```
    /// use snow_recorder_protocol::EditOp;
    /// let op = EditOp::parse_token("fps,15").unwrap();
    /// assert_eq!(op, EditOp::ReduceFps { target_fps: 15 });
    /// ```
    pub fn parse_token(token: &str) -> Result<Self, ParseError> {
        let mut it = token.split(',');
        match it.next() {
            Some("fps") => Ok(Self::ReduceFps {
                target_fps: number(it.next(), "target_fps")?,
            }),
            Some("scale") => Ok(Self::Scale {
                width: number(it.next(), "width")?,
                height: number(it.next(), "height")?,
            }),
            Some("frames") => {
                let mode = ExtractMode::parse_token(it.next().unwrap_or(""))?;
                let Some(format) = it.next().and_then(ImageFormat::parse) else {
                    return err("字段 image_format 缺失或不受支持");
                };
                let quality: u8 = number(it.next(), "quality")?;
                if !(1..=100).contains(&quality) {
                    return err("quality 必须在 1..=100");
                }
                Ok(Self::ExtractFrames {
                    mode,
                    format,
                    quality,
                })
            }
            Some("trim") => Ok(Self::TrimKeyframe {
                start_ms: number(it.next(), "start_ms")?,
                end_ms: number(it.next(), "end_ms")?,
            }),
            other => err(format!("未知编辑操作: {}", other.unwrap_or(""))),
        }
    }
}

/// 编辑任务请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// 引擎选择。
    pub engine: EngineKind,
    /// 编辑操作。
    pub op: EditOp,
    /// 输入视频（本软件录制的 MP4）。
    pub input: PathBuf,
    /// 输出路径；抽帧时为输出目录。
    pub output: PathBuf,
}

/// 视频探测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeInfo {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// 时长（毫秒）。
    pub duration_ms: u64,
    /// 平均帧率 x1000（30fps 为 30000）；未知为 0。
    pub fps_milli: u32,
    /// 视频帧（包）总数。
    pub frames: u64,
    /// 关键帧数。
    pub keyframes: u64,
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
    /// 提交编辑任务（进程空闲时才接受）。
    Edit(EditRequest),
    /// 探测视频信息。
    Probe {
        /// 输入视频。
        input: PathBuf,
    },
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
    /// 编辑进度；`total` 未知时为 0。
    EditProgress {
        /// 已完成数量（帧或张）。
        done: u64,
        /// 总数。
        total: u64,
        /// 阶段名（英文标识，空格会被换成下划线，由主程序翻译）。
        stage: String,
    },
    /// 编辑完成；抽帧时 `path` 为输出目录，`frames` 为导出张数。
    EditFinished {
        /// 输出路径。
        path: PathBuf,
        /// 输出帧（张）数。
        frames: u64,
        /// 实际使用的引擎（不会是 `Auto`）。
        engine: EngineKind,
    },
    /// 探测结果。
    ProbeResult(ProbeInfo),
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

/// 路径转单行文本，并去掉会破坏输入/输出分隔的制表符。
fn path_text(path: &Path) -> String {
    single_line(&path.to_string_lossy()).replace(PATH_SEP, " ")
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
            Self::Edit(r) => format!(
                "{CMD_EDIT} {} {} {}{PATH_SEP}{}",
                r.engine.as_str(),
                r.op.to_token(),
                path_text(&r.input),
                path_text(&r.output),
            ),
            Self::Probe { input } => {
                format!("{CMD_PROBE} {}", single_line(&input.to_string_lossy()))
            }
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
            CMD_EDIT => {
                let mut it = rest.splitn(3, ' ');
                let Some(engine) = it.next().and_then(EngineKind::parse) else {
                    return err("字段 engine 缺失或不受支持");
                };
                let op = EditOp::parse_token(it.next().unwrap_or(""))?;
                let Some((input, output)) = it.next().and_then(|p| p.split_once(PATH_SEP)) else {
                    return err("字段 input/output 缺失");
                };
                if input.is_empty() || output.is_empty() {
                    return err("字段 input/output 缺失");
                }
                Ok(Self::Edit(EditRequest {
                    engine,
                    op,
                    input: PathBuf::from(input),
                    output: PathBuf::from(output),
                }))
            }
            CMD_PROBE => {
                if rest.is_empty() {
                    return err("字段 input 缺失");
                }
                Ok(Self::Probe {
                    input: PathBuf::from(rest),
                })
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
            Self::EditProgress { done, total, stage } => {
                format!(
                    "{EVT_EDIT_PROGRESS} {done} {total} {}",
                    single_line(stage).replace(' ', "_")
                )
            }
            Self::EditFinished {
                path,
                frames,
                engine,
            } => format!(
                "{EVT_EDIT_FINISHED} {frames} {} {}",
                engine.as_str(),
                single_line(&path.to_string_lossy())
            ),
            Self::ProbeResult(p) => format!(
                "{EVT_PROBE_RESULT} {} {} {} {} {} {}",
                p.width, p.height, p.duration_ms, p.fps_milli, p.frames, p.keyframes
            ),
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
            EVT_EDIT_PROGRESS => {
                let mut it = rest.splitn(3, ' ');
                Ok(Self::EditProgress {
                    done: number(it.next(), "done")?,
                    total: number(it.next(), "total")?,
                    stage: it.next().unwrap_or("").to_string(),
                })
            }
            EVT_EDIT_FINISHED => {
                let mut it = rest.splitn(3, ' ');
                let frames = number(it.next(), "frames")?;
                let Some(engine) = it
                    .next()
                    .and_then(EngineKind::parse)
                    .filter(|e| *e != EngineKind::Auto)
                else {
                    return err("字段 engine 缺失或不是具体引擎");
                };
                let Some(path) = it.next().filter(|p| !p.is_empty()) else {
                    return err("字段 path 缺失");
                };
                Ok(Self::EditFinished {
                    path: PathBuf::from(path),
                    frames,
                    engine,
                })
            }
            EVT_PROBE_RESULT => {
                let mut it = rest.split(' ');
                Ok(Self::ProbeResult(ProbeInfo {
                    width: number(it.next(), "width")?,
                    height: number(it.next(), "height")?,
                    duration_ms: number(it.next(), "duration_ms")?,
                    fps_milli: number(it.next(), "fps_milli")?,
                    frames: number(it.next(), "frames")?,
                    keyframes: number(it.next(), "keyframes")?,
                }))
            }
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
        assert_eq!(
            scratch_dir(final_path, 7),
            PathBuf::from("D:/v/.snow-recording-7")
        );
        assert_eq!(
            scratch_file(final_path, 7),
            PathBuf::from("D:/v/.snow-recording-7/a b.gif")
        );
        // 无目录的相对路径也不会 panic
        assert_eq!(
            scratch_dir(Path::new("a.mp4"), 1),
            PathBuf::from(".snow-recording-1")
        );
    }

    /// 构造覆盖全部编辑操作的请求。
    fn sample_edits() -> Vec<EditRequest> {
        let ops = [
            EditOp::ReduceFps { target_fps: 15 },
            EditOp::Scale {
                width: 1280,
                height: 720,
            },
            EditOp::ExtractFrames {
                mode: ExtractMode::Single { at_ms: 1500 },
                format: ImageFormat::Png,
                quality: 90,
            },
            EditOp::ExtractFrames {
                mode: ExtractMode::Interval { every_ms: 500 },
                format: ImageFormat::Jpeg,
                quality: 85,
            },
            EditOp::ExtractFrames {
                mode: ExtractMode::Keyframes,
                format: ImageFormat::WebpLossless,
                quality: 100,
            },
            EditOp::TrimKeyframe {
                start_ms: 1000,
                end_ms: 5000,
            },
        ];
        let engines = [EngineKind::Auto, EngineKind::System, EngineKind::Ffmpeg];
        ops.into_iter()
            .zip(engines.into_iter().cycle())
            .map(|(op, engine)| EditRequest {
                engine,
                op,
                input: PathBuf::from("C:\\My Videos\\in put.mp4"),
                output: PathBuf::from("D:\\out dir\\x y"),
            })
            .collect()
    }

    /// 编辑与探测命令往返：路径含空格。
    #[test]
    fn edit_command_round_trip() {
        for req in sample_edits() {
            let cmd = Command::Edit(req);
            assert_eq!(Command::parse(&cmd.to_line()).unwrap(), cmd);
        }
        let probe = Command::Probe {
            input: PathBuf::from("C:\\a b\\c.mp4"),
        };
        assert_eq!(Command::parse(&probe.to_line()).unwrap(), probe);
    }

    /// 编辑相关事件往返。
    #[test]
    fn edit_event_round_trip() {
        for evt in [
            Event::EditProgress {
                done: 3,
                total: 0,
                stage: "decode".into(),
            },
            Event::EditFinished {
                path: PathBuf::from("D:\\o ut"),
                frames: 12,
                engine: EngineKind::Ffmpeg,
            },
            Event::ProbeResult(ProbeInfo {
                width: 1920,
                height: 1080,
                duration_ms: 30_000,
                fps_milli: 59_940,
                frames: 1798,
                keyframes: 31,
            }),
        ] {
            assert_eq!(Event::parse(&evt.to_line()).unwrap(), evt);
        }
    }

    /// 编辑相关非法输入一律拒绝；有损 WebP 不受支持。
    #[test]
    fn malformed_edit_lines_are_rejected() {
        for bad in [
            "EDIT",
            "EDIT auto fps,15",
            "EDIT auto fps,15 a.mp4",
            "EDIT bogus fps,15 a.mp4\tb.mp4",
            "EDIT auto fps,x a.mp4\tb.mp4",
            "EDIT auto frames,single=1,webp,90 a.mp4\tb",
            "EDIT auto frames,interval=0,png,90 a.mp4\tb",
            "EDIT auto frames,keyframes,png,0 a.mp4\tb",
            "EDIT auto nope a.mp4\tb.mp4",
            "EDIT auto fps,15 \tb.mp4",
            "PROBE",
        ] {
            assert!(Command::parse(bad).is_err(), "应拒绝: {bad:?}");
        }
        for bad in [
            "EDIT_PROGRESS 1",
            "EDIT_FINISHED 1 auto x",
            "EDIT_FINISHED 1 ffmpeg",
            "PROBE_RESULT 1 2 3",
        ] {
            assert!(Event::parse(bad).is_err(), "应拒绝: {bad:?}");
        }
    }

    /// 历史遗留的 webm 归一化为默认格式。
    #[test]
    fn webm_normalizes_to_default() {
        assert_eq!(MediaFormat::normalize("webm"), MediaFormat::Mp4);
        assert_eq!(MediaFormat::normalize("WEBP"), MediaFormat::Webp);
        assert_eq!(MediaFormat::normalize(""), MediaFormat::Mp4);
    }
}
