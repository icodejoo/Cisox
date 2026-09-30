//! OpenAI 兼容通道：把翻译请求发给任意 `/chat/completions` 端点（云端、Ollama、LM Studio 等）。
//!
//! 不引入 HTTP 依赖：真实传输用系统自带的 `curl.exe`（`--ssl-no-revoke`），密钥与地址经 stdin 的
//! curl 配置传入（不出现在进程命令行里），请求体写入一次性临时文件，用完即删。
//! 传输被抽象成 [`HttpPost`]，测试里用假实现，不访问网络。

use crate::{Lang, TranslateError, TranslationEngine};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 聊天补全端点后缀。
pub const CHAT_ENDPOINT_SUFFIX: &str = "/chat/completions";
/// 单次请求默认超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// 采样温度：翻译要稳定。
const TRANSLATION_TEMPERATURE: f64 = 0.3;
/// 连接超时（秒）。
const CONNECT_TIMEOUT_SECS: &str = "15";
/// 错误信息里最多带回的响应正文字符数。
const ERROR_SNIPPET_CHARS: usize = 200;
/// 一次批量翻译最多逐段请求的段数（防止 OCR 文字过多时刷爆端点）。
pub const MAX_BATCH_SEGMENTS: usize = 40;
/// 推理模型思考块的起止标记。
const THINK_OPEN: &str = "<think>";
/// 推理模型思考块的结束标记。
const THINK_CLOSE: &str = "</think>";
/// Windows 不弹控制台窗口的进程创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 临时请求文件的序号，避免同进程内重名。
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

/// 兼容 OpenAI / Ollama / LM Studio 的大模型翻译引擎配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleConfig {
    /// 接口基础地址（例如 `http://localhost:11434/v1`，不含 `/chat/completions`）。
    pub base_url: String,
    /// 认证密钥（本地服务可留空）。
    pub api_key: String,
    /// 调用的模型名称（例如 `qwen2.5`、`llama3.2`）。
    pub model: String,
}

impl OpenAiCompatibleConfig {
    /// 完整的聊天补全端点地址。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::openai::OpenAiCompatibleConfig;
    /// let cfg = OpenAiCompatibleConfig {
    ///     base_url: "http://localhost:11434/v1/".into(),
    ///     api_key: String::new(),
    ///     model: "qwen2.5".into(),
    /// };
    /// assert_eq!(cfg.endpoint(), "http://localhost:11434/v1/chat/completions");
    /// ```
    pub fn endpoint(&self) -> String {
        format!("{}{CHAT_ENDPOINT_SUFFIX}", self.base_url.trim().trim_end_matches('/'))
    }

    /// 生成符合 OpenAI Chat 格式的翻译请求体。
    ///
    /// # 参数
    /// - `text`: 待翻译文本。
    /// - `src`: 源语言（`Auto` 时让模型自行判断）。
    /// - `tgt`: 目标语言。
    ///
    /// # 返回
    /// 序列化为 JSON 字符串的请求体。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::{Lang, openai::OpenAiCompatibleConfig};
    /// let cfg = OpenAiCompatibleConfig { base_url: "http://x/v1".into(), api_key: String::new(), model: "m".into() };
    /// assert!(cfg.format_request("Hello", Lang::En, Lang::ZhHans).contains("Hello"));
    /// ```
    pub fn format_request(&self, text: &str, src: Lang, tgt: Lang) -> String {
        let direction = if src == Lang::Auto {
            format!("Translate the text into {}. Detect the source language automatically.", tgt.display_name())
        } else {
            format!("Translate the text from {} to {}.", src.display_name(), tgt.display_name())
        };
        let system = format!(
            "You are a professional translator. {direction} Output ONLY the translated text without explanations."
        );
        serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": text }
            ],
            "temperature": TRANSLATION_TEMPERATURE
        })
        .to_string()
    }
}

/// 去掉推理模型输出开头的 `<think>…</think>` 思考块。
fn strip_reasoning(content: &str) -> &str {
    let trimmed = content.trim_start();
    if trimmed.starts_with(THINK_OPEN)
        && let Some(end) = trimmed.find(THINK_CLOSE)
    {
        return trimmed[end + THINK_CLOSE.len()..].trim();
    }
    content.trim()
}

/// 截取响应正文开头一小段用于错误提示。
fn snippet(body: &str) -> String {
    let text: String = body.trim().chars().take(ERROR_SNIPPET_CHARS).collect();
    text.replace(['\r', '\n'], " ")
}

/// 解析聊天补全响应，取出译文。
///
/// # 参数
/// - `body`：响应 JSON 文本。
///
/// # 返回
/// 译文（已去首尾空白与思考块）；服务端返回 `error` 字段、缺少内容或内容为空时返回错误。
///
/// # 示例
/// ```rust
/// use snow_translate::openai::parse_chat_response;
/// let body = r#"{"choices":[{"message":{"content":" 你好 "}}]}"#;
/// assert_eq!(parse_chat_response(body).unwrap(), "你好");
/// assert!(parse_chat_response(r#"{"error":{"message":"bad key"}}"#).is_err());
/// ```
pub fn parse_chat_response(body: &str) -> Result<String, TranslateError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| TranslateError::Network(format!("响应不是有效 JSON ({e}): {}", snippet(body))))?;
    if let Some(message) = value.pointer("/error/message").and_then(|m| m.as_str()) {
        return Err(TranslateError::Network(format!("服务端报错: {message}")));
    }
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| TranslateError::Network(format!("响应缺少 choices[0].message.content: {}", snippet(body))))?;
    let text = strip_reasoning(content);
    if text.is_empty() {
        return Err(TranslateError::Inference("模型返回了空译文".into()));
    }
    Ok(text.to_string())
}

/// HTTP 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// 状态码。
    pub status: u16,
    /// 响应正文。
    pub body: String,
}

/// POST JSON 的传输抽象（真实实现是 [`CurlHttp`]，测试里用假实现）。
pub trait HttpPost: Send + Sync {
    /// 发送 POST 请求。
    ///
    /// # 参数
    /// - `url`：完整地址。
    /// - `api_key`：密钥，空串表示不带认证头。
    /// - `body`：JSON 请求体。
    /// - `timeout`：整体超时。
    ///
    /// # 返回
    /// 响应；连接失败、超时等传输层问题返回错误说明（不含密钥）。
    fn post_json(&self, url: &str, api_key: &str, body: &str, timeout: Duration) -> Result<HttpResponse, String>;
}

/// 转义 curl 配置文件里双引号字符串的内容（反斜杠、双引号与控制字符）。
///
/// # 示例
/// ```rust
/// use snow_translate::openai::escape_curl_value;
/// assert_eq!(escape_curl_value(r#"a"b\c"#), r#"a\"b\\c"#);
/// assert_eq!(escape_curl_value("x\ny"), "x\\ny");
/// ```
pub fn escape_curl_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// 生成 curl 配置文本（经 stdin 的 `-K -` 传入，密钥因此不会出现在进程命令行）。
///
/// # 参数
/// - `url`：完整地址。
/// - `api_key`：密钥，空串表示不带认证头。
/// - `body_path`：请求体文件路径。
/// - `max_time_secs`：整体超时秒数。
pub fn curl_config_text(url: &str, api_key: &str, body_path: &Path, max_time_secs: u64) -> String {
    let mut text = String::new();
    text.push_str(&format!("url = \"{}\"\n", escape_curl_value(url)));
    text.push_str("request = \"POST\"\n");
    text.push_str("header = \"Content-Type: application/json\"\n");
    if !api_key.is_empty() {
        text.push_str(&format!("header = \"Authorization: Bearer {}\"\n", escape_curl_value(api_key)));
    }
    let path = body_path.to_string_lossy().replace('\\', "/");
    text.push_str(&format!("data-binary = \"@{}\"\n", escape_curl_value(&path)));
    text.push_str(&format!("max-time = {max_time_secs}\n"));
    text.push_str("write-out = \"\\n%{http_code}\"\n");
    text
}

/// 把 curl 输出拆成 `(状态码, 正文)`：最后一行是 `write-out` 追加的状态码。
///
/// # 参数
/// - `output`：curl 标准输出。
///
/// # 返回
/// 无法识别状态码时返回 `None`。
///
/// # 示例
/// ```rust
/// use snow_translate::openai::split_curl_output;
/// let (status, body) = split_curl_output(b"{\"a\":1}\n200").unwrap();
/// assert_eq!((status, body.as_str()), (200, "{\"a\":1}"));
/// assert!(split_curl_output(b"no status").is_none());
/// ```
pub fn split_curl_output(output: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(output);
    let (body, code) = text.rsplit_once('\n')?;
    let status = code.trim().parse::<u16>().ok()?;
    Some((status, body.to_string()))
}

/// 用系统 `curl.exe` 发请求的传输实现。
pub struct CurlHttp;

/// 系统工具路径：优先 `%SystemRoot%\System32\curl.exe`，否则走 PATH。
fn curl_program() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("curl.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("curl"))
}

/// 一次性临时请求文件，析构时删除。
struct TempBody {
    /// 文件路径。
    path: PathBuf,
}

impl TempBody {
    /// 写入请求体到临时目录。
    fn create(body: &str) -> Result<Self, String> {
        let seq = REQUEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("snow-translate-req-{}-{seq}.json", std::process::id()));
        std::fs::write(&path, body.as_bytes()).map_err(|e| format!("无法写入临时请求文件: {e}"))?;
        Ok(Self { path })
    }
}

impl Drop for TempBody {
    /// 删除临时文件。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl HttpPost for CurlHttp {
    /// 经 curl 发送；配置走 stdin，请求体走临时文件。
    fn post_json(&self, url: &str, api_key: &str, body: &str, timeout: Duration) -> Result<HttpResponse, String> {
        let temp = TempBody::create(body)?;
        let config = curl_config_text(url, api_key, &temp.path, timeout.as_secs().max(1));
        let mut command = Command::new(curl_program());
        command
            .args(["--silent", "--show-error", "--ssl-no-revoke", "--connect-timeout", CONNECT_TIMEOUT_SECS])
            .args(["-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| format!("无法运行 curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(config.as_bytes()).map_err(|e| format!("无法向 curl 传入配置: {e}"))?;
        }
        let output = child.wait_with_output().map_err(|e| format!("等待 curl 结束失败: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("curl 失败 ({}): {}", output.status, snippet(&stderr)));
        }
        let (status, body) = split_curl_output(&output.stdout).ok_or_else(|| "curl 输出缺少状态码".to_string())?;
        Ok(HttpResponse { status, body })
    }
}

/// OpenAI 兼容翻译引擎。
pub struct OpenAiEngine {
    /// 端点配置。
    config: OpenAiCompatibleConfig,
    /// 传输实现。
    http: Box<dyn HttpPost>,
    /// 单次请求超时。
    timeout: Duration,
}

impl OpenAiEngine {
    /// 用真实 curl 传输创建引擎。
    ///
    /// # 参数
    /// - `config`：端点配置。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::openai::{OpenAiCompatibleConfig, OpenAiEngine};
    /// use snow_translate::TranslationEngine;
    /// let engine = OpenAiEngine::new(OpenAiCompatibleConfig {
    ///     base_url: "http://localhost:11434/v1".into(),
    ///     api_key: String::new(),
    ///     model: "qwen2.5".into(),
    /// });
    /// assert_eq!(engine.engine_name(), "OpenAiCompatible");
    /// ```
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self::with_http(config, Box::new(CurlHttp), DEFAULT_TIMEOUT)
    }

    /// 用自定义传输创建引擎（测试注入假传输）。
    ///
    /// # 参数
    /// - `config`：端点配置。
    /// - `http`：传输实现。
    /// - `timeout`：单次请求超时。
    pub fn with_http(config: OpenAiCompatibleConfig, http: Box<dyn HttpPost>, timeout: Duration) -> Self {
        Self { config, http, timeout }
    }
}

/// 把 HTTP 失败状态映射成可读错误（不含密钥）。
fn status_error(status: u16, body: &str) -> TranslateError {
    let detail = match status {
        401 | 403 => "认证失败，请检查 API 密钥".to_string(),
        404 => "端点不存在，请检查基础地址与模型名".to_string(),
        429 => "请求过于频繁或额度用尽".to_string(),
        _ => snippet(body),
    };
    TranslateError::Network(format!("HTTP {status}: {detail}"))
}

impl TranslationEngine for OpenAiEngine {
    /// 发一次聊天补全请求并解析译文。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }
        let body = self.config.format_request(text, src, tgt);
        let response = self
            .http
            .post_json(&self.config.endpoint(), &self.config.api_key, &body, self.timeout)
            .map_err(TranslateError::Network)?;
        if !(200..300).contains(&response.status) {
            return Err(status_error(response.status, &response.body));
        }
        parse_chat_response(&response.body)
    }

    /// 逐段请求（最多 [`MAX_BATCH_SEGMENTS`] 段，超出返回错误而不是静默截断）。
    fn translate_batch(&self, texts: &[String], src: Lang, tgt: Lang) -> Result<Vec<String>, TranslateError> {
        if texts.len() > MAX_BATCH_SEGMENTS {
            return Err(TranslateError::InvalidRequest(format!(
                "文本段数过多（{}，上限 {MAX_BATCH_SEGMENTS}）",
                texts.len()
            )));
        }
        texts.iter().map(|t| self.translate(t, src, tgt)).collect()
    }

    /// 大模型不限语言对：这里列出常用组合（含 `Auto` 源语言）。
    fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
        vec![(Lang::Auto, Lang::ZhHans), (Lang::Auto, Lang::En)]
    }

    /// 引擎名称。
    fn engine_name(&self) -> &'static str {
        "OpenAiCompatible"
    }

    /// 缓存标识：端点 + 模型（换模型不复用旧译文）。
    fn cache_id(&self) -> String {
        format!("openai:{}:{}", self.config.endpoint(), self.config.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 一条记录下来的请求 `(url, api_key, body)`。
    type SeenRequest = (String, String, String);

    /// 假传输：按队列回放响应，并记录请求。
    struct FakeHttp {
        /// 待回放的结果。
        replies: Mutex<Vec<Result<HttpResponse, String>>>,
        /// 收到的请求。
        seen: Arc<Mutex<Vec<SeenRequest>>>,
    }

    impl HttpPost for FakeHttp {
        fn post_json(&self, url: &str, api_key: &str, body: &str, _timeout: Duration) -> Result<HttpResponse, String> {
            self.seen.lock().unwrap().push((url.into(), api_key.into(), body.into()));
            self.replies.lock().unwrap().remove(0)
        }
    }

    /// 测试用配置。
    fn config() -> OpenAiCompatibleConfig {
        OpenAiCompatibleConfig {
            base_url: "http://localhost:11434/v1/".into(),
            api_key: "sk-secret".into(),
            model: "qwen2.5".into(),
        }
    }

    /// 构造成功响应。
    fn ok(content: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            body: serde_json::json!({"choices":[{"message":{"content":content}}]}).to_string(),
        })
    }

    /// 造引擎并返回请求记录。
    fn engine(replies: Vec<Result<HttpResponse, String>>) -> (OpenAiEngine, Arc<Mutex<Vec<SeenRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let http = FakeHttp { replies: Mutex::new(replies), seen: Arc::clone(&seen) };
        (OpenAiEngine::with_http(config(), Box::new(http), Duration::from_secs(5)), seen)
    }

    /// 请求体：模型名、文本、语言方向；`Auto` 让模型自行判断。
    #[test]
    fn request_body_shape() {
        let cfg = config();
        let body: serde_json::Value =
            serde_json::from_str(&cfg.format_request("Hello world", Lang::En, Lang::ZhHans)).unwrap();
        assert_eq!(body["model"], "qwen2.5");
        assert_eq!(body["messages"][1]["content"], "Hello world");
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("from 英语 to 简体中文"), "{system}");
        let auto: serde_json::Value = serde_json::from_str(&cfg.format_request("x", Lang::Auto, Lang::Ja)).unwrap();
        assert!(auto["messages"][0]["content"].as_str().unwrap().contains("Detect the source language"));
    }

    /// 端点拼接去掉多余斜杠。
    #[test]
    fn endpoint_joins_cleanly() {
        assert_eq!(config().endpoint(), "http://localhost:11434/v1/chat/completions");
        let mut cfg = config();
        cfg.base_url = " https://api.example.com/v1 ".into();
        assert_eq!(cfg.endpoint(), "https://api.example.com/v1/chat/completions");
    }

    /// 响应解析：成功、思考块、服务端错误、缺字段、空内容、非 JSON。
    #[test]
    fn response_parsing() {
        assert_eq!(parse_chat_response(r#"{"choices":[{"message":{"content":"\n你好\n"}}]}"#).unwrap(), "你好");
        let think = r#"{"choices":[{"message":{"content":"<think>hmm\nplan</think>\n\n你好"}}]}"#;
        assert_eq!(parse_chat_response(think).unwrap(), "你好");
        assert!(matches!(
            parse_chat_response(r#"{"error":{"message":"invalid key"}}"#),
            Err(TranslateError::Network(m)) if m.contains("invalid key")
        ));
        assert!(matches!(parse_chat_response(r#"{"choices":[]}"#), Err(TranslateError::Network(_))));
        assert!(matches!(
            parse_chat_response(r#"{"choices":[{"message":{"content":"  "}}]}"#),
            Err(TranslateError::Inference(_))
        ));
        assert!(matches!(parse_chat_response("<html>502</html>"), Err(TranslateError::Network(_))));
    }

    /// 引擎：成功路径带密钥与完整端点；空白文本不发请求。
    #[test]
    fn engine_translates_and_skips_blank() {
        let (engine, seen) = engine(vec![ok("你好")]);
        assert_eq!(engine.translate("  ", Lang::En, Lang::ZhHans).unwrap(), "  ");
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(engine.translate("hello", Lang::En, Lang::ZhHans).unwrap(), "你好");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "http://localhost:11434/v1/chat/completions");
        assert_eq!(seen[0].1, "sk-secret");
    }

    /// HTTP 错误状态映射为可读错误，且错误文本不泄露密钥。
    #[test]
    fn http_errors_are_readable_and_do_not_leak_the_key() {
        for (status, needle) in [(401, "认证失败"), (404, "端点不存在"), (429, "额度"), (500, "HTTP 500")] {
            let (engine, _) = engine(vec![Ok(HttpResponse { status, body: "boom".into() })]);
            let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err().to_string();
            assert!(err.contains(needle), "{err}");
            assert!(!err.contains("sk-secret"));
        }
        let (engine, _) = engine(vec![Err("无法运行 curl".into())]);
        assert!(matches!(engine.translate("x", Lang::En, Lang::ZhHans), Err(TranslateError::Network(_))));
    }

    /// 批量：逐段请求且保持顺序；超过上限直接报错。
    #[test]
    fn batch_is_sequential_and_bounded() {
        let (engine, seen) = engine(vec![ok("一"), ok("二")]);
        let out = engine.translate_batch(&["a".into(), "b".into()], Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(out, ["一", "二"]);
        assert_eq!(seen.lock().unwrap().len(), 2);
        let many: Vec<String> = (0..=MAX_BATCH_SEGMENTS).map(|i| i.to_string()).collect();
        assert!(matches!(
            engine.translate_batch(&many, Lang::En, Lang::ZhHans),
            Err(TranslateError::InvalidRequest(_))
        ));
    }

    /// 缓存标识含端点与模型。
    #[test]
    fn cache_id_changes_with_model() {
        let (a, _) = engine(vec![]);
        let mut cfg = config();
        cfg.model = "other".into();
        let b = OpenAiEngine::with_http(cfg, Box::new(FakeHttp { replies: Mutex::new(vec![]), seen: Arc::default() }), DEFAULT_TIMEOUT);
        assert_ne!(a.cache_id(), b.cache_id());
        assert_eq!(a.engine_name(), "OpenAiCompatible");
    }

    /// curl 配置：密钥在配置里而不是命令行；路径反斜杠被规整；特殊字符被转义。
    #[test]
    fn curl_config_layout() {
        let text = curl_config_text("http://h/v1/chat/completions", "k\"ey", Path::new("C:\\Temp\\a b\\req.json"), 30);
        assert!(text.contains("url = \"http://h/v1/chat/completions\"\n"));
        assert!(text.contains("header = \"Authorization: Bearer k\\\"ey\"\n"));
        assert!(text.contains("data-binary = \"@C:/Temp/a b/req.json\"\n"));
        assert!(text.contains("max-time = 30\n"));
        assert!(text.contains("write-out = \"\\n%{http_code}\"\n"));
        let no_key = curl_config_text("http://h", "", Path::new("x.json"), 5);
        assert!(!no_key.contains("Authorization"));
    }

    /// curl 输出拆分：正文可含换行；缺状态码返回 None。
    #[test]
    fn curl_output_splitting() {
        let (status, body) = split_curl_output("{\n\"a\": \"中\"\n}\n200".as_bytes()).unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("\"a\": \"中\""));
        assert_eq!(split_curl_output(b"\n404").unwrap(), (404, String::new()));
        assert!(split_curl_output(b"").is_none());
        assert!(split_curl_output(b"abc\nxyz").is_none());
    }

    /// 转义：反斜杠、引号、换行、回车、制表符。
    #[test]
    fn escape_covers_specials() {
        assert_eq!(escape_curl_value("a\\b\"c\r\n\t"), "a\\\\b\\\"c\\r\\n\\t");
        assert_eq!(escape_curl_value("plain"), "plain");
    }

    /// 临时请求文件写入后可读，析构后消失。
    #[test]
    fn temp_body_is_cleaned_up() {
        let path;
        {
            let temp = TempBody::create("{\"k\":1}").unwrap();
            path = temp.path.clone();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"k\":1}");
        }
        assert!(!path.exists());
    }
}
