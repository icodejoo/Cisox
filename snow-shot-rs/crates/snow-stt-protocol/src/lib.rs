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
/// START 里可选的后端前缀键；缺省表示本地模型（旧版主程序不发这个字段）。
const START_BACKEND_KEY: &str = "backend=";
/// 后端取值：本地模型。
const BACKEND_LOCAL: &str = "local";
/// 后端取值：Windows 系统语音。
const BACKEND_SYSTEM: &str = "system";
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

/// 开始识别请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
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

/// 主程序发给工作进程的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
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
                match r.backend {
                    BackendKind::Local => String::new(),
                    BackendKind::System => format!("{START_BACKEND_KEY}{BACKEND_SYSTEM} "),
                },
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
                // 可选的后端前缀；没有它就是旧格式（本地模型）
                let (backend, rest) = match rest.strip_prefix(START_BACKEND_KEY) {
                    Some(tail) => {
                        let (value, after) = tail.split_once(' ').unwrap_or((tail, ""));
                        let kind = match value {
                            BACKEND_LOCAL => BackendKind::Local,
                            BACKEND_SYSTEM => BackendKind::System,
                            other => return err(format!("未知后端: {other}")),
                        };
                        (kind, after)
                    }
                    None => (BackendKind::Local, rest),
                };
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

    #[test]
    fn escape_unescape_inverse() {
        let s = "a\\n\n\\\t\r末尾\\";
        assert_eq!(unescape(&escape(s)).unwrap(), s);
    }
}
