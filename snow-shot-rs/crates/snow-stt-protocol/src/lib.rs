//! 主程序与语音转文字工作进程（snow-stt）之间的行协议。
//!
//! 每条消息占一行 UTF-8 文本，以 `\n` 结尾，字段以单个空格分隔；
//! 自由文本（模型目录、识别文本、错误原因）永远放在行尾，
//! 且其中的反斜杠、换行、回车、制表符会被转义，保证“一条消息一行”。
//! 零第三方依赖，两端共用同一份解析代码。
//!
//! - 主程序 → 工作进程（stdin）：[`Command`]
//! - 工作进程 → 主程序（stdout）：[`Event`]

use std::fmt;

/// 命令字：开始识别。
const CMD_START: &str = "START";
/// 命令字：停止并冲刷尾部。
const CMD_STOP: &str = "STOP";
/// 命令字：取消并丢弃。
const CMD_CANCEL: &str = "CANCEL";
/// 命令字：心跳探测。
const CMD_PING: &str = "PING";
/// 事件字：进程就绪。
const EVT_READY: &str = "READY";
/// 事件字：识别中的临时文本。
const EVT_PARTIAL: &str = "PARTIAL";
/// 事件字：一句话定稿。
const EVT_FINAL: &str = "FINAL";
/// 事件字：错误。
const EVT_ERROR: &str = "ERROR";
/// 事件字：心跳应答。
const EVT_PONG: &str = "PONG";
/// 事件字：已停止，进程随后退出。
const EVT_STOPPED: &str = "STOPPED";
/// START 命令的固定字段个数（不含行尾的模型目录）。
const START_FIELDS: usize = 6;
/// 后端取值：本地模型。
const BACKEND_LOCAL: &str = "local";
/// 后端取值：Windows 系统语音。
const BACKEND_SYSTEM: &str = "system";
/// START 前缀键：识别模式。
const START_MODE_KEY: &str = "mode";
/// START 前缀键：模型类型。
const START_KIND_KEY: &str = "kind";
/// START 前缀键：VAD 参数。
const START_VAD_KEY: &str = "vad";
/// START 前缀键：逆文本规整开关。
const START_ITN_KEY: &str = "itn";
/// START 前缀键：后端（不含等号）。
const START_BACKEND_NAME: &str = "backend";
/// 模式取值：流式。
const MODE_STREAMING: &str = "streaming";
/// 模式取值：离线（VAD 切句后整句识别）。
const MODE_OFFLINE: &str = "offline";
/// VAD 参数的分隔符。
const VAD_SEP: char = ':';
/// VAD 参数个数。
const VAD_FIELDS: usize = 4;
/// VAD 模型文件名约定：worker 先在 `model_dir` 里找，找不到再找其父目录（共享目录）。
pub const VAD_MODEL_FILE_NAME: &str = "silero_vad.onnx";
/// 系统语音错误在 ERROR 文本里的前缀标记。
const SYSTEM_ERROR_PREFIX: &str = "[system:";

/// 端点检测规则（单位毫秒），含义同 sherpa-onnx 的三条规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointRules {
    /// 规则 1：尚未解出任何文字时，尾部静音达到该时长即判端点。
    pub rule1_ms: u32,
    /// 规则 2：已解出文字后，尾部静音达到该时长即判端点。
    pub rule2_ms: u32,
    /// 规则 3：一句话累计时长超过该值即判端点（防止无限长句）。
    pub rule3_ms: u32,
}

impl Default for EndpointRules {
    /// sherpa 示例默认值：2.4s / 1.2s / 20s。
    fn default() -> Self {
        Self {
            rule1_ms: 2400,
            rule2_ms: 1200,
            rule3_ms: 20_000,
        }
    }
}

/// 识别后端种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// 本地模型（sherpa-onnx），需要模型目录；旧协议的唯一取值。
    #[default]
    Local,
    /// Windows 系统语音（`SpeechRecognizer`），只吃默认麦克风，不需要模型目录。
    System,
}

/// 系统语音后端的可识别错误类别，经 ERROR 文本的前缀标记传给主程序，由主程序翻译成本地化提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemError {
    /// 系统「联机语音识别」开关未开。
    OnlineSpeechOff,
    /// 麦克风被隐私设置拒绝或不可用。
    MicrophoneDenied,
    /// 所需语言的语音包缺失或语言不受支持。
    LanguageUnavailable,
    /// 联机识别时网络失败。
    Network,
    /// 无法创建系统识别器或其它未归类的失败。
    Other,
}

impl SystemError {
    /// 全部类别，便于遍历（如检查每个类别都有文案）。
    pub const ALL: [SystemError; 5] = [
        Self::OnlineSpeechOff,
        Self::MicrophoneDenied,
        Self::LanguageUnavailable,
        Self::Network,
        Self::Other,
    ];

    /// 稳定的类别标记（协议值，不翻译）。
    pub fn tag(self) -> &'static str {
        match self {
            Self::OnlineSpeechOff => "online-off",
            Self::MicrophoneDenied => "mic-denied",
            Self::LanguageUnavailable => "language",
            Self::Network => "network",
            Self::Other => "other",
        }
    }

    /// 把类别与细节编码成 ERROR 原因文本（如 `[system:mic-denied] 细节`）。
    ///
    /// # 参数
    /// - `detail`：附加细节，可为空。
    pub fn to_error_text(self, detail: &str) -> String {
        format!("{SYSTEM_ERROR_PREFIX}{}] {detail}", self.tag())
    }

    /// 从 ERROR 原因文本解析类别与细节；不带前缀标记时返回 `None`。
    ///
    /// # 参数
    /// - `text`：ERROR 事件携带的原因。
    ///
    /// # 返回
    /// 类别与细节。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::SystemError;
    /// let text = SystemError::Network.to_error_text("x");
    /// assert_eq!(SystemError::from_error_text(&text), Some((SystemError::Network, "x".to_string())));
    /// assert_eq!(SystemError::from_error_text("普通错误"), None);
    /// ```
    pub fn from_error_text(text: &str) -> Option<(Self, String)> {
        let rest = text.strip_prefix(SYSTEM_ERROR_PREFIX)?;
        let (tag, detail) = rest.split_once(']')?;
        let kind = Self::ALL.into_iter().find(|k| k.tag() == tag)?;
        Some((kind, detail.trim_start().to_string()))
    }
}

/// 识别模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecognitionMode {
    /// 流式：边说边出临时结果（缺省）。
    #[default]
    Streaming,
    /// 离线：VAD 切句后整句识别。
    Offline,
}

/// 本地模型类型，决定 worker 用哪种 sherpa 识别器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelKind {
    /// 流式 transducer（缺省，旧协议唯一取值）。
    #[default]
    OnlineTransducer,
    /// 离线 transducer。
    OfflineTransducer,
    /// 离线 Paraformer。
    OfflineParaformer,
    /// 离线 SenseVoice。
    OfflineSenseVoice,
    /// 离线 Whisper。
    OfflineWhisper,
    /// 离线 Zipformer CTC。
    OfflineZipformerCtc,
    /// 离线 NeMo CTC。
    OfflineNemoCtc,
    /// 离线 Moonshine。
    OfflineMoonshine,
}

impl ModelKind {
    /// 全部取值，便于遍历。
    pub const ALL: [ModelKind; 8] = [
        Self::OnlineTransducer,
        Self::OfflineTransducer,
        Self::OfflineParaformer,
        Self::OfflineSenseVoice,
        Self::OfflineWhisper,
        Self::OfflineZipformerCtc,
        Self::OfflineNemoCtc,
        Self::OfflineMoonshine,
    ];

    /// 协议字符串（kebab-case，不翻译）。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::ModelKind;
    /// assert_eq!(ModelKind::OfflineSenseVoice.as_str(), "offline-sense-voice");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnlineTransducer => "online-transducer",
            Self::OfflineTransducer => "offline-transducer",
            Self::OfflineParaformer => "offline-paraformer",
            Self::OfflineSenseVoice => "offline-sense-voice",
            Self::OfflineWhisper => "offline-whisper",
            Self::OfflineZipformerCtc => "offline-zipformer-ctc",
            Self::OfflineNemoCtc => "offline-nemo-ctc",
            Self::OfflineMoonshine => "offline-moonshine",
        }
    }

    /// 从协议字符串解析；未知取值返回错误。
    ///
    /// # 参数
    /// - `text`：kind 取值。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::ModelKind;
    /// assert_eq!(ModelKind::parse("offline-whisper").unwrap(), ModelKind::OfflineWhisper);
    /// assert!(ModelKind::parse("bogus").is_err());
    /// ```
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        match Self::ALL.into_iter().find(|k| k.as_str() == text) {
            Some(k) => Ok(k),
            None => err(format!("未知模型类型: {text}")),
        }
    }

    /// 是否为离线模型（需要 VAD 切句）。
    pub fn is_offline(self) -> bool {
        self != Self::OnlineTransducer
    }
}

/// 语音活动检测（VAD）参数；省略时由 worker 使用内置默认值。
///
/// 线路格式 `vad=<threshold>:<min_silence_ms>:<min_speech_ms>:<max_speech_ms>`。
/// VAD 模型文件不走协议，见 [`VAD_MODEL_FILE_NAME`]。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadParams {
    /// 判为语音的概率阈值，范围 0..=1。
    pub threshold: f32,
    /// 静音达到该时长（毫秒）即切句。
    pub min_silence_ms: u32,
    /// 短于该时长（毫秒）的语音片段丢弃。
    pub min_speech_ms: u32,
    /// 单段语音最长（毫秒），超过强制切开。
    pub max_speech_ms: u32,
}

impl VadParams {
    /// 序列化为 `threshold:silence:min:max`。
    pub fn to_wire(&self) -> String {
        format!(
            "{}{VAD_SEP}{}{VAD_SEP}{}{VAD_SEP}{}",
            self.threshold, self.min_silence_ms, self.min_speech_ms, self.max_speech_ms
        )
    }

    /// 从 `threshold:silence:min:max` 解析；阈值须为 0..=1 的有限数。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::VadParams;
    /// let v = VadParams::parse("0.5:300:250:20000").unwrap();
    /// assert_eq!(v.min_silence_ms, 300);
    /// assert!(VadParams::parse("-1:1:1:1").is_err());
    /// ```
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let parts: Vec<&str> = text.split(VAD_SEP).collect();
        if parts.len() != VAD_FIELDS {
            return err("vad 需要 4 个字段");
        }
        let threshold: f32 = number(Some(parts[0]), "vad.threshold")?;
        if !(0.0..=1.0).contains(&threshold) {
            return err("vad.threshold 必须在 0..=1 内");
        }
        Ok(Self {
            threshold,
            min_silence_ms: number(Some(parts[1]), "vad.min_silence_ms")?,
            min_speech_ms: number(Some(parts[2]), "vad.min_speech_ms")?,
            max_speech_ms: number(Some(parts[3]), "vad.max_speech_ms")?,
        })
    }
}

/// 开始识别请求。
///
/// 可选前缀键（`key=value`，顺序任意，不可重复）：`backend` `mode` `kind` `vad` `itn`；
/// 缺省值不序列化，序列化顺序固定为上述顺序。`itn` 仅接受 `1`/`0`。
#[derive(Debug, Clone, PartialEq)]
pub struct StartRequest {
    /// 识别模式；缺省流式，不写字段。
    pub mode: RecognitionMode,
    /// 模型类型；缺省 `online-transducer`，不写字段。
    pub kind: ModelKind,
    /// VAD 参数；`None` 表示用 worker 内置默认值。
    pub vad: Option<VadParams>,
    /// 逆文本规整（仅 SenseVoice 有意义）；仅为 `true` 时写 `itn=1`。
    pub itn: bool,
    /// 识别后端；序列化时本地模型不写该字段，保持旧格式。
    pub backend: BackendKind,
    /// 语言提示（如 `zh-en`），无特殊需求传 `auto`；单词，不含空白。
    pub language: String,
    /// 推理线程数，至少为 1。
    pub threads: u32,
    /// 端点检测规则。
    pub endpoint: EndpointRules,
    /// 单次录音最长秒数，超过自动当作 STOP；0 表示不限。
    pub max_seconds: u32,
    /// 模型目录（含 encoder/decoder/joiner/tokens），放在行尾；系统语音后端可为空。
    pub model_dir: String,
}

impl Default for StartRequest {
    /// 旧协议等价的本地流式请求：语言 `auto`、1 线程、默认端点、不限时、空模型目录。
    fn default() -> Self {
        Self {
            mode: RecognitionMode::default(),
            kind: ModelKind::default(),
            vad: None,
            itn: false,
            backend: BackendKind::default(),
            language: "auto".to_string(),
            threads: 1,
            endpoint: EndpointRules::default(),
            max_seconds: 0,
            model_dir: String::new(),
        }
    }
}

/// 主程序发给工作进程的命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 加载模型并开始采集识别。
    Start(StartRequest),
    /// 停止采集，冲刷尾部后发 FINAL 与 STOPPED 并退出。
    Stop,
    /// 取消：不冲刷尾部，直接发 STOPPED 并退出。
    Cancel,
    /// 心跳，工作进程回 PONG。
    Ping,
}

/// 工作进程回报给主程序的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 进程已启动，可接受 `Start`。
    Ready,
    /// 当前这句话的临时识别文本（可能被后续修正）。
    Partial(String),
    /// 一句话的定稿文本。
    Final(String),
    /// 出错；原因为单行文本，进程随后退出。
    Error(String),
    /// 心跳应答。
    Pong,
    /// 已完全停止，进程随后退出。
    Stopped,
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

/// 转义自由文本：反斜杠、换行、回车、制表符分别写成两字符序列。
///
/// # 参数
/// - `text`：原始文本。
///
/// # 返回
/// 不含换行与制表符的单行文本。
///
/// # 示例
/// ```
/// assert_eq!(snow_stt_protocol::escape("a\nb"), "a\\nb");
/// ```
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// 还原 [`escape`] 的结果；遇到未知转义或结尾孤立的反斜杠返回错误。
///
/// # 参数
/// - `text`：转义后的文本。
///
/// # 示例
/// ```
/// assert_eq!(snow_stt_protocol::unescape("a\\nb").unwrap(), "a\nb");
/// assert!(snow_stt_protocol::unescape("bad\\x").is_err());
/// ```
pub fn unescape(text: &str) -> Result<String, ParseError> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => return err(format!("未知转义: \\{other}")),
            None => return err("文本以孤立的反斜杠结尾"),
        }
    }
    Ok(out)
}

/// 解析一个数值字段。
fn number<T: std::str::FromStr>(field: Option<&str>, name: &str) -> Result<T, ParseError> {
    match field.and_then(|v| v.parse::<T>().ok()) {
        Some(v) => Ok(v),
        None => err(format!("字段 {name} 缺失或不是有效数字")),
    }
}

/// 按固定顺序拼出 START 的可选前缀（每个键后带一个空格；全缺省时为空串）。
fn start_prefix(r: &StartRequest) -> String {
    let mut out = String::new();
    if r.backend == BackendKind::System {
        out.push_str(&format!("{START_BACKEND_NAME}={BACKEND_SYSTEM} "));
    }
    if r.mode == RecognitionMode::Offline {
        out.push_str(&format!("{START_MODE_KEY}={MODE_OFFLINE} "));
    }
    if r.kind != ModelKind::default() {
        out.push_str(&format!("{START_KIND_KEY}={} ", r.kind.as_str()));
    }
    if let Some(v) = &r.vad {
        out.push_str(&format!("{START_VAD_KEY}={} ", v.to_wire()));
    }
    if r.itn {
        out.push_str(&format!("{START_ITN_KEY}=1 "));
    }
    out
}

/// 若 `rest` 以 `key=value` 令牌开头，返回该令牌与其后的剩余部分。
fn take_prefix_token(rest: &str) -> Option<(&str, &str)> {
    let (token, after) = rest.split_once(' ').unwrap_or((rest, ""));
    token.contains('=').then_some((token, after))
}

/// 解析中的前缀键集合；`None` 表示未出现，用于检测重复键。
#[derive(Default)]
struct StartOptions {
    /// 后端。
    backend: Option<BackendKind>,
    /// 模式。
    mode: Option<RecognitionMode>,
    /// 模型类型。
    kind: Option<ModelKind>,
    /// VAD 参数。
    vad: Option<VadParams>,
    /// 逆文本规整。
    itn: Option<bool>,
}

/// 写入一个前缀键；重复时返回错误。
fn set_once<T>(slot: &mut Option<T>, key: &str, value: T) -> Result<(), ParseError> {
    if slot.replace(value).is_some() {
        return err(format!("重复的前缀键: {key}"));
    }
    Ok(())
}

impl StartOptions {
    /// 应用一个 `key=value` 令牌；未知键、非法值、重复键均报错。
    fn apply(&mut self, token: &str) -> Result<(), ParseError> {
        let (key, value) = token.split_once('=').unwrap_or((token, ""));
        match key {
            START_BACKEND_NAME => {
                let v = match value {
                    BACKEND_LOCAL => BackendKind::Local,
                    BACKEND_SYSTEM => BackendKind::System,
                    other => return err(format!("未知后端: {other}")),
                };
                set_once(&mut self.backend, key, v)
            }
            START_MODE_KEY => {
                let v = match value {
                    MODE_STREAMING => RecognitionMode::Streaming,
                    MODE_OFFLINE => RecognitionMode::Offline,
                    other => return err(format!("未知模式: {other}")),
                };
                set_once(&mut self.mode, key, v)
            }
            START_KIND_KEY => set_once(&mut self.kind, key, ModelKind::parse(value)?),
            START_VAD_KEY => set_once(&mut self.vad, key, VadParams::parse(value)?),
            START_ITN_KEY => {
                let v = match value {
                    "1" => true,
                    "0" => false,
                    other => return err(format!("itn 只接受 1 或 0: {other}")),
                };
                set_once(&mut self.itn, key, v)
            }
            other => err(format!("未知前缀键: {other}")),
        }
    }
}

/// 把一行拆成命令字与剩余部分，同时去掉行尾换行。
fn split_word(line: &str) -> (&str, &str) {
    let line = line.trim_end_matches(['\r', '\n']);
    line.split_once(' ').unwrap_or((line, ""))
}

impl Command {
    /// 序列化为一行文本（不含换行符）。
    ///
    /// # 返回
    /// 协议行。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::Command;
    /// assert_eq!(Command::Stop.to_line(), "STOP");
    /// ```
    pub fn to_line(&self) -> String {
        match self {
            Self::Start(r) => format!(
                "{CMD_START} {}{} {} {} {} {} {} {}",
                start_prefix(r),
                r.language.replace(char::is_whitespace, "_"),
                r.threads,
                r.endpoint.rule1_ms,
                r.endpoint.rule2_ms,
                r.endpoint.rule3_ms,
                r.max_seconds,
                escape(&r.model_dir),
            ),
            Self::Stop => CMD_STOP.to_string(),
            Self::Cancel => CMD_CANCEL.to_string(),
            Self::Ping => CMD_PING.to_string(),
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
    /// use snow_stt_protocol::Command;
    /// assert_eq!(Command::parse("PING\n").unwrap(), Command::Ping);
    /// ```
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let (word, rest) = split_word(line);
        match word {
            CMD_STOP => Ok(Self::Stop),
            CMD_CANCEL => Ok(Self::Cancel),
            CMD_PING => Ok(Self::Ping),
            CMD_START => {
                // 可选的 key=value 前缀，直到第一个不含 '=' 的令牌（语言）
                let mut opts = StartOptions::default();
                let mut rest = rest;
                while let Some((token, after)) = take_prefix_token(rest) {
                    opts.apply(token)?;
                    rest = after;
                }
                let StartOptions {
                    backend,
                    mode,
                    kind,
                    vad,
                    itn,
                } = opts;
                let backend = backend.unwrap_or_default();
                let mut it = rest.splitn(START_FIELDS + 1, ' ');
                let language = match it.next() {
                    Some(l) if !l.is_empty() => l.to_string(),
                    _ => return err("字段 language 缺失"),
                };
                let threads: u32 = number(it.next(), "threads")?;
                if threads == 0 {
                    return err("threads 必须大于 0");
                }
                let endpoint = EndpointRules {
                    rule1_ms: number(it.next(), "rule1_ms")?,
                    rule2_ms: number(it.next(), "rule2_ms")?,
                    rule3_ms: number(it.next(), "rule3_ms")?,
                };
                let max_seconds = number(it.next(), "max_seconds")?;
                let model_dir = match (it.next(), backend) {
                    (Some(d), _) if !d.is_empty() => unescape(d)?,
                    (_, BackendKind::System) => String::new(),
                    _ => return err("字段 model_dir 缺失"),
                };
                Ok(Self::Start(StartRequest {
                    mode: mode.unwrap_or_default(),
                    kind: kind.unwrap_or_default(),
                    vad,
                    itn: itn.unwrap_or(false),
                    backend,
                    language,
                    threads,
                    endpoint,
                    max_seconds,
                    model_dir,
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
    /// 协议行；文本中的换行与制表符已转义。
    ///
    /// # 示例
    /// ```
    /// use snow_stt_protocol::Event;
    /// assert_eq!(Event::Partial("你好".into()).to_line(), "PARTIAL 你好");
    /// ```
    pub fn to_line(&self) -> String {
        match self {
            Self::Ready => EVT_READY.to_string(),
            Self::Partial(t) => format!("{EVT_PARTIAL} {}", escape(t)),
            Self::Final(t) => format!("{EVT_FINAL} {}", escape(t)),
            Self::Error(r) => format!("{EVT_ERROR} {}", escape(r)),
            Self::Pong => EVT_PONG.to_string(),
            Self::Stopped => EVT_STOPPED.to_string(),
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
    /// use snow_stt_protocol::Event;
    /// assert_eq!(Event::parse("FINAL a\\nb").unwrap(), Event::Final("a\nb".into()));
    /// ```
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let (word, rest) = split_word(line);
        match word {
            EVT_READY => Ok(Self::Ready),
            EVT_PONG => Ok(Self::Pong),
            EVT_STOPPED => Ok(Self::Stopped),
            EVT_PARTIAL => Ok(Self::Partial(unescape(rest)?)),
            EVT_FINAL => Ok(Self::Final(unescape(rest)?)),
            EVT_ERROR => Ok(Self::Error(unescape(rest)?)),
            other => err(format!("未知事件: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个带空格路径的请求。
    fn sample() -> StartRequest {
        StartRequest {
            mode: RecognitionMode::Streaming,
            kind: ModelKind::OnlineTransducer,
            vad: None,
            itn: false,
            backend: BackendKind::Local,
            language: "zh-en".into(),
            threads: 2,
            endpoint: EndpointRules {
                rule1_ms: 2400,
                rule2_ms: 800,
                rule3_ms: 20_000,
            },
            max_seconds: 60,
            model_dir: "D:\\My Models\\zip former".into(),
        }
    }

    #[test]
    fn command_roundtrip() {
        for cmd in [
            Command::Start(sample()),
            Command::Stop,
            Command::Cancel,
            Command::Ping,
        ] {
            let line = cmd.to_line();
            assert!(!line.contains('\n'));
            assert_eq!(Command::parse(&line).unwrap(), cmd);
            assert_eq!(Command::parse(&format!("{line}\r\n")).unwrap(), cmd);
        }
    }

    #[test]
    fn event_roundtrip_with_special_text() {
        for evt in [
            Event::Ready,
            Event::Pong,
            Event::Stopped,
            Event::Partial("第一行\n第二行\t制表\\反斜杠\r".into()),
            Event::Final("hello world".into()),
            Event::Final(String::new()),
            Event::Error("加载失败: C:\\m\\a.onnx\n详情".into()),
        ] {
            let line = evt.to_line();
            assert!(!line.contains(['\n', '\r', '\t']), "{line:?}");
            assert_eq!(Event::parse(&line).unwrap(), evt);
        }
    }

    #[test]
    fn model_dir_with_spaces_and_tab_roundtrips() {
        let mut r = sample();
        r.model_dir = "a b\tc\\d".into();
        let line = Command::Start(r.clone()).to_line();
        assert_eq!(Command::parse(&line).unwrap(), Command::Start(r));
    }

    #[test]
    fn local_start_keeps_legacy_wire_format() {
        let line = Command::Start(sample()).to_line();
        assert!(!line.contains("backend="), "{line}");
        // 旧版主程序发的行（无后端字段）仍解析成本地模型
        let legacy = "START zh-en 2 2400 800 20000 60 D:\\\\m";
        let Command::Start(r) = Command::parse(legacy).unwrap() else {
            panic!("应为 Start");
        };
        assert_eq!(r.backend, BackendKind::Local);
        assert_eq!(r.model_dir, "D:\\m");
    }

    #[test]
    fn system_start_roundtrips_with_and_without_model_dir() {
        for dir in ["", "D:\\x y"] {
            let mut r = sample();
            r.backend = BackendKind::System;
            r.model_dir = dir.into();
            let line = Command::Start(r.clone()).to_line();
            assert!(line.starts_with("START backend=system zh-en "), "{line}");
            assert_eq!(Command::parse(&line).unwrap(), Command::Start(r));
        }
        // 显式 local 也接受；未知后端被拒绝
        assert!(Command::parse("START backend=local zh 1 1 2 3 0 d").is_ok());
        assert!(Command::parse("START backend=cloud zh 1 1 2 3 0 d").is_err());
        // 本地模型仍必须带模型目录
        assert!(Command::parse("START backend=local zh 1 1 2 3 0").is_err());
    }

    #[test]
    fn system_error_text_roundtrips() {
        for kind in SystemError::ALL {
            let text = kind.to_error_text("细节 a\\b");
            let (back, detail) = SystemError::from_error_text(&text).unwrap();
            assert_eq!((back, detail.as_str()), (kind, "细节 a\\b"));
            // 经 ERROR 事件整行往返后依旧可解析
            let Event::Error(t) = Event::parse(&Event::Error(text).to_line()).unwrap() else {
                panic!("应为 Error");
            };
            assert_eq!(SystemError::from_error_text(&t).unwrap().0, kind);
        }
        assert_eq!(SystemError::from_error_text("模型缺失"), None);
        assert_eq!(SystemError::from_error_text("[system:bogus] x"), None);
    }

    #[test]
    fn language_whitespace_is_squashed() {
        let mut r = sample();
        r.language = "zh en".into();
        let Command::Start(back) = Command::parse(&Command::Start(r).to_line()).unwrap() else {
            panic!("应为 Start");
        };
        assert_eq!(back.language, "zh_en");
    }

    #[test]
    fn rejects_bad_commands() {
        for line in [
            "",
            "BOGUS",
            "start x",
            "START",
            "START zh-en",
            "START zh-en 0 1 2 3 4 dir",
            "START zh-en x 1 2 3 4 dir",
            "START zh-en 2 1 2 3 4",
            "START zh-en 2 1 2 3 4 ",
            "START zh-en 2 -1 2 3 4 dir",
            "START zh-en 2 1 2 3 4 bad\\q",
        ] {
            assert!(Command::parse(line).is_err(), "应当失败: {line:?}");
        }
    }

    #[test]
    fn rejects_bad_events() {
        for line in ["", "NOPE", "PARTIAL bad\\", "FINAL x\\z", "partial x"] {
            assert!(Event::parse(line).is_err(), "应当失败: {line:?}");
        }
    }

    #[test]
    fn empty_text_events_parse() {
        assert_eq!(
            Event::parse("PARTIAL").unwrap(),
            Event::Partial(String::new())
        );
        assert_eq!(Event::parse("FINAL ").unwrap(), Event::Final(String::new()));
    }

    #[test]
    fn unicode_text_is_kept_verbatim() {
        let e = Event::Final("昨天是 MONDAY 😀".into());
        assert_eq!(e.to_line(), "FINAL 昨天是 MONDAY 😀");
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
    }

    /// 解析一行 START 为请求。
    fn start_of(line: &str) -> StartRequest {
        let Command::Start(r) = Command::parse(line).unwrap() else {
            panic!("应为 Start");
        };
        r
    }

    /// 全部新键都打开的请求。
    fn full() -> StartRequest {
        StartRequest {
            mode: RecognitionMode::Offline,
            kind: ModelKind::OfflineSenseVoice,
            vad: Some(VadParams {
                threshold: 0.45,
                min_silence_ms: 300,
                min_speech_ms: 250,
                max_speech_ms: 20_000,
            }),
            itn: true,
            backend: BackendKind::System,
            ..sample()
        }
    }

    #[test]
    fn legacy_lines_are_byte_stable() {
        for line in [
            "START zh-en 2 2400 800 20000 60 D:\\\\My Models\\\\zip former",
            "START backend=system zh-en 2 2400 800 20000 60 D:\\\\x y",
        ] {
            assert_eq!(Command::parse(line).unwrap().to_line(), line);
        }
    }

    #[test]
    fn defaults_are_not_serialized() {
        let line = Command::Start(sample()).to_line();
        for key in ["mode=", "kind=", "vad=", "itn="] {
            assert!(!line.contains(key), "{line}");
        }
        assert_eq!(StartRequest::default().mode, RecognitionMode::Streaming);
    }

    #[test]
    fn each_new_key_roundtrips_alone() {
        let cases = [
            (
                StartRequest {
                    mode: RecognitionMode::Offline,
                    ..sample()
                },
                "mode=offline",
            ),
            (
                StartRequest {
                    kind: ModelKind::OfflineWhisper,
                    ..sample()
                },
                "kind=offline-whisper",
            ),
            (
                StartRequest {
                    vad: full().vad,
                    ..sample()
                },
                "vad=0.45:300:250:20000",
            ),
            (
                StartRequest {
                    itn: true,
                    ..sample()
                },
                "itn=1",
            ),
        ];
        for (req, token) in cases {
            let line = Command::Start(req.clone()).to_line();
            assert!(line.starts_with(&format!("START {token} zh-en ")), "{line}");
            assert_eq!(start_of(&line), req);
        }
    }

    #[test]
    fn combined_keys_roundtrip_in_fixed_order_any_input_order() {
        let req = full();
        let line = Command::Start(req.clone()).to_line();
        assert!(
            line.starts_with(
                "START backend=system mode=offline kind=offline-sense-voice vad=0.45:300:250:20000 itn=1 zh-en "
            ),
            "{line}"
        );
        assert_eq!(start_of(&line), req);
        let shuffled = "START itn=1 vad=0.45:300:250:20000 kind=offline-sense-voice mode=offline backend=system zh-en 2 2400 800 20000 60 D:\\\\My Models\\\\zip former";
        assert_eq!(start_of(shuffled), req);
        assert_eq!(start_of(shuffled).model_dir, "D:\\My Models\\zip former");
    }

    #[test]
    fn vad_float_roundtrips_stably() {
        for t in [0.0_f32, 0.1, 0.45, 0.5, 0.123_456_79, 1.0] {
            let v = VadParams {
                threshold: t,
                min_silence_ms: 1,
                min_speech_ms: 2,
                max_speech_ms: 3,
            };
            assert_eq!(VadParams::parse(&v.to_wire()).unwrap(), v);
        }
    }

    #[test]
    fn model_kind_roundtrips_and_classifies() {
        for k in ModelKind::ALL {
            assert_eq!(ModelKind::parse(k.as_str()).unwrap(), k);
            assert_eq!(k.is_offline(), k != ModelKind::OnlineTransducer);
        }
        assert!(ModelKind::parse("bogus").is_err());
    }

    #[test]
    fn rejects_bad_prefix_keys() {
        for line in [
            "START kind=bogus zh 1 1 2 3 0 d",
            "START mode=xxx zh 1 1 2 3 0 d",
            "START vad=0.5:1:1 zh 1 1 2 3 0 d",
            "START vad=a:1:1:1 zh 1 1 2 3 0 d",
            "START vad=-0.5:1:1:1 zh 1 1 2 3 0 d",
            "START vad=NaN:1:1:1 zh 1 1 2 3 0 d",
            "START vad=0.5:-1:1:1 zh 1 1 2 3 0 d",
            "START mode=offline mode=offline zh 1 1 2 3 0 d",
            "START itn=1 itn=0 zh 1 1 2 3 0 d",
            "START foo=bar zh 1 1 2 3 0 d",
            "START itn=2 zh 1 1 2 3 0 d",
            "START itn=true zh 1 1 2 3 0 d",
            "START mode=offline",
        ] {
            assert!(Command::parse(line).is_err(), "应当失败: {line:?}");
        }
        assert!(!start_of("START itn=0 zh 1 1 2 3 0 d").itn);
    }

    #[test]
    fn escape_unescape_inverse() {
        let s = "a\\n\n\\\t\r末尾\\";
        assert_eq!(unescape(&escape(s)).unwrap(), s);
    }
}
