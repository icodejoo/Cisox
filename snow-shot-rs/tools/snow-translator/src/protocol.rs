//! 主程序与翻译工作进程之间的行协议（JSON Lines）。
//!
//! 每条消息是一行 UTF-8 JSON，以 `\n` 结尾，单行不超过 [`MAX_LINE_BYTES`]。
//! 与录制进程的纯文本行协议同为「一行一条」；翻译文本可含换行与引号，故载荷用 JSON。
//!
//! - 主程序 → 工作进程（stdin）：[`Command`]
//! - 工作进程 → 主程序（stdout）：[`Event`]，stdout 只允许出现协议行

use std::io::{self, BufRead, Read, Write};

use serde::{Deserialize, Serialize};

/// 协议版本号，`ready` 事件携带，双方不一致时宿主应拒绝使用。
pub const PROTOCOL_VERSION: u32 = 1;

/// 单行最大字节数（1 MiB），超出视为坏请求。
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 丢弃超长行时每次读取的块大小。
const DISCARD_CHUNK_BYTES: u64 = 64 * 1024;

/// 主程序发给工作进程的命令。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// 加载模型：`model_dir` 含 `model.json`，`src`/`tgt` 为语言代码。
    Load {
        /// 模型目录。
        model_dir: String,
        /// 源语言代码，例如 `en`。
        src: String,
        /// 目标语言代码，例如 `zh-CN`。
        tgt: String,
        /// 覆盖清单的“请求后收缩内存”开关；缺省沿用清单（宿主的低内存策略用它）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trim_after_request: Option<bool>,
    },
    /// 翻译文本：`text` 与 `texts` 至少给一个，同时给则先 `text` 后 `texts`。
    Translate {
        /// 请求编号，原样回传。
        id: u64,
        /// 单条文本。
        #[serde(default)]
        text: Option<String>,
        /// 多条文本，逐条独立翻译。
        #[serde(default)]
        texts: Option<Vec<String>>,
        /// 每个片段最多生成的 token 数，缺省取模型清单值。
        #[serde(default)]
        max_len: Option<usize>,
        /// 束宽，缺省取模型清单值（通常 1 即贪心）；范围 `1..=8`。
        #[serde(default)]
        num_beams: Option<usize>,
    },
    /// 卸载并退出进程（回收全部内存）。
    Unload,
    /// 心跳，返回内存快照。
    Ping,
}

/// 错误类别，宿主据此决定降级或提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 请求本身不合法（JSON 坏、行过长、参数缺失）。
    BadRequest,
    /// 尚未加载模型。
    NotLoaded,
    /// 模型目录、清单或模型文件缺失。
    ModelMissing,
    /// 清单存在但内容非法。
    ManifestInvalid,
    /// 模型不支持请求的语言对。
    UnsupportedPair,
    /// 找不到或无法加载 onnxruntime 动态库。
    RuntimeMissing,
    /// 模型加载失败（文件损坏、算子不支持等）。
    LoadFailed,
    /// 内存不足。
    OutOfMemory,
    /// 模型文件 SHA-256 与清单声明不符。
    ChecksumMismatch,
    /// 推理或解码失败。
    DecodeFailed,
}

/// 工作进程回报给主程序的事件。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "evt", rename_all = "snake_case")]
pub enum Event {
    /// 进程就绪，可以接收命令。
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
        /// 加载后工作集内存（字节），平台不支持时为 0。
        mem_bytes: u64,
    },
    /// 翻译结果，`texts` 与请求条目一一对应。
    Result {
        /// 请求编号。
        id: u64,
        /// 译文。
        texts: Vec<String>,
        /// 本次总耗时（毫秒）。
        elapsed_ms: u64,
    },
    /// 心跳应答。
    Pong {
        /// 是否已加载模型。
        loaded: bool,
        /// 当前工作集内存（字节）。
        mem_bytes: u64,
        /// 峰值工作集内存（字节）。
        peak_bytes: u64,
    },
    /// 已卸载，紧接着进程退出。
    Unloaded,
    /// 失败。
    Error {
        /// 关联的请求编号（无则为空）。
        #[serde(default)]
        id: Option<u64>,
        /// 错误类别。
        kind: ErrorKind,
        /// 人类可读原因。
        message: String,
    },
}

/// 解析一行命令。
///
/// # 参数
/// - `line`：不含换行符的一行 JSON。
///
/// # 返回
/// 成功返回 [`Command`]，失败返回可直接回报的说明文字。
///
/// # 示例
/// ```ignore
/// let cmd = parse_command(r#"{"cmd":"ping"}"#).unwrap();
/// assert_eq!(cmd, Command::Ping);
/// ```
pub fn parse_command(line: &str) -> Result<Command, String> {
    serde_json::from_str(line.trim()).map_err(|e| format!("invalid command: {e}"))
}

/// 把事件写成一行 JSON 并刷新。
///
/// # 参数
/// - `out`：输出端（通常是 stdout）。
/// - `event`：要发送的事件。
///
/// # 返回
/// 写入失败（管道已断）时返回 IO 错误。
///
/// # 示例
/// ```ignore
/// let mut buf = Vec::new();
/// write_event(&mut buf, &Event::Unloaded).unwrap();
/// assert_eq!(buf, b"{\"evt\":\"unloaded\"}\n");
/// ```
pub fn write_event(out: &mut impl Write, event: &Event) -> io::Result<()> {
    let mut line = serde_json::to_vec(event).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// 有界读行的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum LineRead {
    /// 读到一行（已去掉行尾换行）。
    Line(String),
    /// 该行超过上限，已丢弃到行尾。
    TooLong,
    /// 输入结束。
    Eof,
}

/// 从输入读取一行，超过 `limit` 字节则丢弃整行，避免恶意大行撑爆内存。
///
/// # 参数
/// - `reader`：带缓冲的输入。
/// - `limit`：单行最大字节数。
///
/// # 返回
/// [`LineRead`]；非 UTF-8 内容按有损转换处理。
///
/// # 示例
/// ```ignore
/// let mut r = std::io::Cursor::new(b"abc\n".to_vec());
/// assert_eq!(read_bounded_line(&mut r, 16).unwrap(), LineRead::Line("abc".into()));
/// ```
pub fn read_bounded_line(reader: &mut impl BufRead, limit: usize) -> io::Result<LineRead> {
    let mut buf = Vec::new();
    let n = reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(LineRead::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        if buf.len() > limit {
            return Ok(LineRead::TooLong);
        }
        return Ok(LineRead::Line(String::from_utf8_lossy(&buf).into_owned()));
    }
    if buf.len() <= limit {
        // 输入在无换行处结束：按最后一行处理
        return Ok(LineRead::Line(String::from_utf8_lossy(&buf).into_owned()));
    }
    // 超限：丢弃到行尾
    let mut sink = Vec::new();
    loop {
        sink.clear();
        let m = reader
            .by_ref()
            .take(DISCARD_CHUNK_BYTES)
            .read_until(b'\n', &mut sink)?;
        if m == 0 || sink.last() == Some(&b'\n') {
            break;
        }
    }
    Ok(LineRead::TooLong)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 各命令都能从 JSON 解析。
    #[test]
    fn parses_all_commands() {
        assert_eq!(parse_command(r#"{"cmd":"ping"}"#), Ok(Command::Ping));
        assert_eq!(parse_command(r#"{"cmd":"unload"}"#), Ok(Command::Unload));
        assert_eq!(
            parse_command(r#"{"cmd":"load","model_dir":"D:/m","src":"en","tgt":"zh-CN"}"#),
            Ok(Command::Load {
                model_dir: "D:/m".into(),
                src: "en".into(),
                tgt: "zh-CN".into(),
                trim_after_request: None
            })
        );
        assert_eq!(
            parse_command(
                r#"{"cmd":"load","model_dir":"D:/m","src":"en","tgt":"zh-CN","trim_after_request":true}"#
            ),
            Ok(Command::Load {
                model_dir: "D:/m".into(),
                src: "en".into(),
                tgt: "zh-CN".into(),
                trim_after_request: Some(true)
            })
        );
        let t = parse_command(r#"{"cmd":"translate","id":7,"texts":["a","b"],"max_len":64}"#);
        assert_eq!(
            t,
            Ok(Command::Translate {
                id: 7,
                text: None,
                texts: Some(vec!["a".into(), "b".into()]),
                max_len: Some(64),
                num_beams: None
            })
        );
    }

    /// 坏 JSON、未知命令、缺字段都返回错误而不 panic。
    #[test]
    fn rejects_bad_commands() {
        assert!(parse_command("not json").is_err());
        assert!(parse_command(r#"{"cmd":"explode"}"#).is_err());
        assert!(parse_command(r#"{"cmd":"translate"}"#).is_err());
        assert!(parse_command("").is_err());
    }

    /// 事件序列化为单行 JSON，且可往返。
    #[test]
    fn events_roundtrip_single_line() {
        let events = vec![
            Event::Ready {
                protocol: PROTOCOL_VERSION,
                pid: 1,
            },
            Event::Loaded {
                model_id: "m".into(),
                load_ms: 5,
                mem_bytes: 9,
            },
            Event::Result {
                id: 3,
                texts: vec!["你好\n世界".into()],
                elapsed_ms: 2,
            },
            Event::Pong {
                loaded: true,
                mem_bytes: 1,
                peak_bytes: 2,
            },
            Event::Unloaded,
            Event::Error {
                id: Some(3),
                kind: ErrorKind::OutOfMemory,
                message: "x".into(),
            },
        ];
        for e in events {
            let mut buf = Vec::new();
            write_event(&mut buf, &e).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert_eq!(s.matches('\n').count(), 1, "必须恰好一行: {s:?}");
            let back: Event = serde_json::from_str(s.trim_end()).unwrap();
            assert_eq!(back, e);
        }
    }

    /// 错误类别使用 snake_case 字符串。
    #[test]
    fn error_kind_wire_names() {
        let mut buf = Vec::new();
        write_event(
            &mut buf,
            &Event::Error {
                id: None,
                kind: ErrorKind::ModelMissing,
                message: "m".into(),
            },
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(r#""kind":"model_missing""#), "{s}");
    }

    /// 有界读行：正常行、CRLF、无换行结尾、EOF。
    #[test]
    fn bounded_line_basic() {
        let mut r = Cursor::new(b"abc\r\ndef".to_vec());
        assert_eq!(
            read_bounded_line(&mut r, 16).unwrap(),
            LineRead::Line("abc".into())
        );
        assert_eq!(
            read_bounded_line(&mut r, 16).unwrap(),
            LineRead::Line("def".into())
        );
        assert_eq!(read_bounded_line(&mut r, 16).unwrap(), LineRead::Eof);
    }

    /// 超长行被丢弃且不影响下一行。
    #[test]
    fn bounded_line_too_long_then_recovers() {
        let mut data = vec![b'x'; 200];
        data.extend_from_slice(b"\nok\n");
        let mut r = Cursor::new(data);
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), LineRead::TooLong);
        assert_eq!(
            read_bounded_line(&mut r, 100).unwrap(),
            LineRead::Line("ok".into())
        );
    }

    /// 恰好等于上限的行可以通过。
    #[test]
    fn bounded_line_exact_limit() {
        let mut data = vec![b'y'; 10];
        data.push(b'\n');
        let mut r = Cursor::new(data);
        assert_eq!(
            read_bounded_line(&mut r, 10).unwrap(),
            LineRead::Line("y".repeat(10))
        );
    }
}
