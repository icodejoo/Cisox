//! 与翻译工作进程（`tools/snow-translator`）的行协议：客户端一侧的消息定义。
//!
//! 每条消息是一行 UTF-8 JSON（无 BOM），以 `\n` 结尾；工作进程启动后先发一行 `ready`。
//! 这里的类型与 worker 的 `protocol.rs` 逐字段镜像（两边是不同的 cargo workspace，无法共用代码），
//! 一致性由 `worker` 模块的真实进程集成测试与本文件的固定 JSON 样例共同保证。

use serde::{Deserialize, Serialize};

/// 客户端支持的协议版本，`ready` 携带的版本不同则拒绝使用。
pub const PROTOCOL_VERSION: u32 = 1;

/// 束宽上限（与 worker 的 `MAX_BEAMS` 一致，越界请求会被 worker 拒绝）。
pub const MAX_BEAMS: usize = 8;

/// 宿主发给工作进程的命令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// 加载模型并指定语言对。
    Load {
        /// 模型目录（含 `model.json`）。
        model_dir: String,
        /// 源语言代码，例如 `en`。
        src: String,
        /// 目标语言代码，例如 `zh-CN`。
        tgt: String,
        /// 覆盖清单的“请求后收缩内存”开关。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trim_after_request: Option<bool>,
    },
    /// 翻译一批文本（逐条独立翻译，结果与请求一一对应）。
    Translate {
        /// 请求编号，原样回传。
        id: u64,
        /// 待翻译文本。
        texts: Vec<String>,
        /// 束宽，缺省取模型清单值。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        num_beams: Option<usize>,
    },
    /// 卸载并退出进程。
    Unload,
    /// 心跳，取内存快照。
    Ping,
}

/// 工作进程报告的错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 请求不合法。
    BadRequest,
    /// 尚未加载模型。
    NotLoaded,
    /// 模型目录、清单或模型文件缺失。
    ModelMissing,
    /// 清单内容非法。
    ManifestInvalid,
    /// 模型不支持该语言对。
    UnsupportedPair,
    /// 找不到或无法加载 onnxruntime 动态库。
    RuntimeMissing,
    /// 模型加载失败。
    LoadFailed,
    /// 内存不足。
    OutOfMemory,
    /// 模型文件 SHA-256 与清单不符。
    ChecksumMismatch,
    /// 推理或解码失败。
    DecodeFailed,
}

/// 工作进程发回的事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "evt", rename_all = "snake_case")]
pub enum Event {
    /// 进程就绪。
    Ready {
        /// 协议版本。
        protocol: u32,
        /// 进程 ID。
        pid: u32,
    },
    /// 模型加载完成。
    Loaded {
        /// 模型 ID。
        model_id: String,
        /// 加载耗时（毫秒）。
        load_ms: u64,
        /// 加载后的工作集内存（字节）。
        mem_bytes: u64,
    },
    /// 翻译结果。
    Result {
        /// 请求编号。
        id: u64,
        /// 译文，与请求条目一一对应。
        texts: Vec<String>,
        /// 翻译耗时（毫秒）。
        elapsed_ms: u64,
    },
    /// 心跳应答。
    Pong {
        /// 是否已加载模型。
        loaded: bool,
        /// 当前工作集（字节）。
        mem_bytes: u64,
        /// 峰值工作集（字节）。
        peak_bytes: u64,
    },
    /// 已卸载，进程随后退出。
    Unloaded,
    /// 失败。
    Error {
        /// 关联的请求编号。
        #[serde(default)]
        id: Option<u64>,
        /// 错误类别。
        kind: ErrorKind,
        /// 可读原因。
        message: String,
    },
}

/// 把命令编码成一行 JSON（不含换行符，无 BOM）。
///
/// # 参数
/// - `command`：要发送的命令。
///
/// # 返回
/// JSON 文本；序列化失败（理论上不会发生）返回错误说明。
///
/// # 示例
/// ```
/// use snow_translate::protocol::{Command, encode_command};
/// assert_eq!(encode_command(&Command::Ping).unwrap(), r#"{"cmd":"ping"}"#);
/// ```
pub fn encode_command(command: &Command) -> Result<String, String> {
    serde_json::to_string(command).map_err(|e| format!("无法编码命令: {e}"))
}

/// 解析一行事件。
///
/// # 参数
/// - `line`：一行 JSON，允许首尾空白。
///
/// # 返回
/// 解析出的事件；非法 JSON 或未知事件返回错误说明。
///
/// # 示例
/// ```
/// use snow_translate::protocol::{Event, decode_event};
/// assert_eq!(decode_event(r#"{"evt":"unloaded"}"#).unwrap(), Event::Unloaded);
/// assert!(decode_event("not json").is_err());
/// ```
pub fn decode_event(line: &str) -> Result<Event, String> {
    serde_json::from_str(line.trim()).map_err(|e| format!("无法解析 worker 事件: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 命令编码与 worker 约定的字段名一致，可选字段缺省时不输出。
    #[test]
    fn encodes_commands_like_the_worker_expects() {
        let load = Command::Load {
            model_dir: "D:/m".into(),
            src: "en".into(),
            tgt: "zh-CN".into(),
            trim_after_request: None,
        };
        assert_eq!(
            encode_command(&load).unwrap(),
            r#"{"cmd":"load","model_dir":"D:/m","src":"en","tgt":"zh-CN"}"#
        );
        let trimmed = Command::Load {
            model_dir: "m".into(),
            src: "en".into(),
            tgt: "zh-CN".into(),
            trim_after_request: Some(true),
        };
        assert!(encode_command(&trimmed).unwrap().ends_with(r#""trim_after_request":true}"#));
        let translate = Command::Translate {
            id: 7,
            texts: vec!["a\"b\n".into(), "中文".into()],
            num_beams: Some(4),
        };
        assert_eq!(
            encode_command(&translate).unwrap(),
            r#"{"cmd":"translate","id":7,"texts":["a\"b\n","中文"],"num_beams":4}"#
        );
        assert_eq!(encode_command(&Command::Unload).unwrap(), r#"{"cmd":"unload"}"#);
    }

    /// 编码结果不含换行与 BOM（行协议的前提）。
    #[test]
    fn encoded_line_has_no_newline_or_bom() {
        let cmd = Command::Translate {
            id: 1,
            texts: vec!["第一行\n第二行\r\n".into()],
            num_beams: None,
        };
        let line = encode_command(&cmd).unwrap();
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert!(!line.starts_with('\u{feff}'));
    }

    /// 解析 worker 的真实样例事件（取自 D4a/D4b 的实测输出格式）。
    #[test]
    fn decodes_worker_events() {
        assert_eq!(
            decode_event(r#"{"evt":"ready","protocol":1,"pid":4242}"#).unwrap(),
            Event::Ready { protocol: 1, pid: 4242 }
        );
        assert_eq!(
            decode_event(r#"{"evt":"loaded","model_id":"opus","load_ms":921,"mem_bytes":309997568}"#)
                .unwrap(),
            Event::Loaded {
                model_id: "opus".into(),
                load_ms: 921,
                mem_bytes: 309_997_568
            }
        );
        assert_eq!(
            decode_event(r#"{"evt":"result","id":3,"texts":["你好"],"elapsed_ms":310}"#).unwrap(),
            Event::Result {
                id: 3,
                texts: vec!["你好".into()],
                elapsed_ms: 310
            }
        );
        assert_eq!(
            decode_event(r#"{"evt":"error","kind":"runtime_missing","message":"no dll"}"#).unwrap(),
            Event::Error {
                id: None,
                kind: ErrorKind::RuntimeMissing,
                message: "no dll".into()
            }
        );
        assert_eq!(
            decode_event(r#"{"evt":"error","id":9,"kind":"checksum_mismatch","message":"x"}"#)
                .unwrap(),
            Event::Error {
                id: Some(9),
                kind: ErrorKind::ChecksumMismatch,
                message: "x".into()
            }
        );
    }

    /// 非法行与未知事件返回错误而不是 panic。
    #[test]
    fn rejects_garbage() {
        assert!(decode_event("").is_err());
        assert!(decode_event("{").is_err());
        assert!(decode_event(r#"{"evt":"nope"}"#).is_err());
        assert!(decode_event(r#"{"evt":"error","kind":"weird","message":"m"}"#).is_err());
    }
}
