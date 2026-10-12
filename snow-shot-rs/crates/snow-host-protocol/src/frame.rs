//! JSON 行编解码：一条消息一行紧凑 JSON，以 `\n` 结尾。
//!
//! 管道是字节流，一次读到的可能是半行或好几行，所以除了单行的 [`encode_line`] / [`decode_line`]，
//! 还有按字节流切行的 [`FrameReader`]（带单行长度上限，超长行整行丢弃并报错）。

use crate::MAX_LINE_BYTES;
use serde::{Serialize, de::DeserializeOwned};
use std::fmt;

/// 行终止符。
const LINE_END: u8 = b'\n';

/// 回车符（容忍 CRLF）。
const CARRIAGE_RETURN: u8 = b'\r';

/// 编解码 / 切行失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// 空行（没有内容）。
    Empty,
    /// 单行超过 [`MAX_LINE_BYTES`]。
    TooLong,
    /// 行内不是合法 UTF-8。
    InvalidUtf8,
    /// 不是合法 JSON，或不是已知的消息形状（带 serde 的说明）。
    Json(String),
}

impl fmt::Display for FrameError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Empty => write!(f, "空行"),
            FrameError::TooLong => write!(f, "单行超过 {MAX_LINE_BYTES} 字节"),
            FrameError::InvalidUtf8 => write!(f, "行内不是合法 UTF-8"),
            FrameError::Json(e) => write!(f, "消息解析失败: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 把消息编码成一行（含结尾 `\n`）。
///
/// # 参数
/// - `message`：任意可序列化的协议消息。
///
/// # 返回
/// 一行文本；紧凑 JSON 不含裸换行，所以一定恰好一行。序列化失败或结果超长返回错误。
///
/// ```rust
/// use snow_host_protocol::frame::encode_line;
/// use snow_host_protocol::message::UiMessage;
/// let line = encode_line(&UiMessage::Ack { seq: 3 }).unwrap();
/// assert_eq!(line, "{\"type\":\"ack\",\"seq\":3}\n");
/// ```
pub fn encode_line<T: Serialize>(message: &T) -> Result<String, FrameError> {
    let mut line = serde_json::to_string(message).map_err(|e| FrameError::Json(e.to_string()))?;
    if line.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong);
    }
    line.push(LINE_END as char);
    Ok(line)
}

/// 解码一行（容忍结尾的 `\n` / `\r\n`）。
///
/// # 参数
/// - `line`：一行文本。
///
/// # 返回
/// 消息；空行、超长、JSON 不合法或形状不认识都返回 [`FrameError`]。
///
/// ```rust
/// use snow_host_protocol::frame::decode_line;
/// use snow_host_protocol::message::UiMessage;
/// let msg: UiMessage = decode_line("{\"type\":\"ack\",\"seq\":3}\r\n").unwrap();
/// assert_eq!(msg, UiMessage::Ack { seq: 3 });
/// ```
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T, FrameError> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.trim().is_empty() {
        return Err(FrameError::Empty);
    }
    if trimmed.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong);
    }
    serde_json::from_str(trimmed).map_err(|e| FrameError::Json(e.to_string()))
}

/// 按字节流切行的读取器。
///
/// 把每次从管道读到的字节喂给 [`FrameReader::push`]，它返回这一批里凑齐的整行；
/// 没收完的半行留在内部等下一批。超长行会一路丢弃到下一个换行，并报一次 [`FrameError::TooLong`]。
#[derive(Debug, Default)]
pub struct FrameReader {
    /// 尚未凑成整行的字节。
    pending: Vec<u8>,
    /// 当前行已超长，正在丢弃字节直到下一个换行。
    discarding: bool,
}

impl FrameReader {
    /// 新建空读取器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一批字节，取出凑齐的整行。
    ///
    /// # 参数
    /// - `chunk`：这次从管道读到的字节（任意长度，可含半行或多行）。
    ///
    /// # 返回
    /// 按出现顺序排列的结果：整行文本（不含换行与结尾回车），或该行的错误
    /// （[`FrameError::Empty`] 的空行不返回，直接跳过）。
    ///
    /// ```rust
    /// use snow_host_protocol::frame::FrameReader;
    /// let mut reader = FrameReader::new();
    /// assert!(reader.push(b"{\"a\":").is_empty());
    /// let lines = reader.push(b"1}\n{\"b\":2}\n");
    /// assert_eq!(lines.len(), 2);
    /// assert_eq!(lines[0].as_deref(), Ok("{\"a\":1}"));
    /// ```
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<String, FrameError>> {
        let mut out = Vec::new();
        for &byte in chunk {
            if byte == LINE_END {
                if self.discarding {
                    self.discarding = false;
                    self.pending.clear();
                    out.push(Err(FrameError::TooLong));
                } else if let Some(result) = self.finish_line() {
                    out.push(result);
                }
            } else if !self.discarding {
                if self.pending.len() >= MAX_LINE_BYTES {
                    self.discarding = true;
                    self.pending.clear();
                } else {
                    self.pending.push(byte);
                }
            }
        }
        out
    }

    /// 当前是否还有没凑成整行的字节（连接断开时用来判断有没有被截断的半行）。
    pub fn has_partial(&self) -> bool {
        self.discarding || !self.pending.is_empty()
    }

    /// 把 `pending` 收成一行文本；空行返回 `None`。
    fn finish_line(&mut self) -> Option<Result<String, FrameError>> {
        let mut bytes = std::mem::take(&mut self.pending);
        if bytes.last() == Some(&CARRIAGE_RETURN) {
            bytes.pop();
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        Some(String::from_utf8(bytes).map_err(|_| FrameError::InvalidUtf8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{HostMessage, UiMessage};

    /// 编码得到恰好一行，含换行结尾；中文与特殊字符不会产生裸换行。
    #[test]
    fn encode_is_exactly_one_line() {
        let msg = HostMessage::SetConfig {
            seq: 1,
            key: "k".into(),
            value: serde_json::json!("第一行\n第二行\r\"引号\""),
        };
        let line = encode_line(&msg).unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(decode_line::<HostMessage>(&line).unwrap(), msg);
    }

    /// 解码容忍 LF / CRLF / 无换行，拒绝空行、乱码与未知形状。
    #[test]
    fn decode_variants_and_errors() {
        let want = UiMessage::Ack { seq: 9 };
        for line in [
            "{\"type\":\"ack\",\"seq\":9}",
            "{\"type\":\"ack\",\"seq\":9}\n",
            "{\"type\":\"ack\",\"seq\":9}\r\n",
        ] {
            assert_eq!(decode_line::<UiMessage>(line).unwrap(), want);
        }
        assert_eq!(decode_line::<UiMessage>(""), Err(FrameError::Empty));
        assert_eq!(decode_line::<UiMessage>(" \r\n"), Err(FrameError::Empty));
        assert!(matches!(
            decode_line::<UiMessage>("nope"),
            Err(FrameError::Json(_))
        ));
        assert!(matches!(
            decode_line::<UiMessage>("{\"type\":\"teleport\"}"),
            Err(FrameError::Json(_))
        ));
        let long = format!(
            "{{\"type\":\"tooltip\",\"text\":\"{}\"}}",
            "x".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(decode_line::<UiMessage>(&long), Err(FrameError::TooLong));
    }

    /// 超长消息编码即报错，不会写出去一行读端必然丢弃的数据。
    #[test]
    fn encode_rejects_oversized_message() {
        let msg = UiMessage::Tooltip {
            text: "x".repeat(MAX_LINE_BYTES),
        };
        assert_eq!(encode_line(&msg), Err(FrameError::TooLong));
    }

    /// 读取器：一次一行、半行跨批、一批多行、CRLF、空行跳过。
    #[test]
    fn reader_splits_lines_across_chunks() {
        let mut r = FrameReader::new();
        assert!(r.push(b"ab").is_empty());
        assert!(r.has_partial());
        let lines = r.push(b"c\r\n\n\ndef\nghi");
        assert_eq!(lines, vec![Ok("abc".to_string()), Ok("def".to_string())]);
        assert!(r.has_partial(), "ghi 还没收完");
        assert_eq!(r.push(b"\n"), vec![Ok("ghi".to_string())]);
        assert!(!r.has_partial());
    }

    /// 逐字节喂入与整块喂入结果一致。
    #[test]
    fn reader_byte_by_byte_equals_whole() {
        let a = encode_line(&UiMessage::Ack { seq: 1 }).unwrap();
        let b = encode_line(&UiMessage::ExitRequest { last_seq: 2 }).unwrap();
        let data = format!("{a}{b}");
        let whole = FrameReader::new().push(data.as_bytes());
        let mut r = FrameReader::new();
        let mut piecewise = Vec::new();
        for byte in data.as_bytes() {
            piecewise.extend(r.push(std::slice::from_ref(byte)));
        }
        assert_eq!(whole, piecewise);
        assert_eq!(whole.len(), 2);
    }

    /// 超长行：报一次 TooLong，丢弃到换行，之后的行照常。
    #[test]
    fn reader_discards_oversized_line() {
        let mut r = FrameReader::new();
        let mut data = vec![b'x'; MAX_LINE_BYTES + 100];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let out = r.push(&data);
        assert_eq!(out, vec![Err(FrameError::TooLong), Ok("ok".to_string())]);
        // 恰好等于上限的行仍然合法
        let mut exact = vec![b'y'; MAX_LINE_BYTES];
        exact.push(b'\n');
        let out = FrameReader::new().push(&exact);
        assert_eq!(out.len(), 1);
        assert!(out[0].as_ref().is_ok_and(|s| s.len() == MAX_LINE_BYTES));
    }

    /// 超长行分多批到达时，丢弃状态跨批保持。
    #[test]
    fn reader_discard_spans_chunks() {
        let mut r = FrameReader::new();
        assert!(r.push("x".repeat(MAX_LINE_BYTES + 1).as_bytes()).is_empty());
        assert!(r.has_partial());
        assert!(r.push("x".repeat(1000).as_bytes()).is_empty());
        assert_eq!(
            r.push(b"\nnext\n"),
            vec![Err(FrameError::TooLong), Ok("next".to_string())]
        );
        assert!(!r.has_partial());
    }

    /// 非 UTF-8 的行报错，不影响后续行。
    #[test]
    fn reader_reports_invalid_utf8() {
        let mut r = FrameReader::new();
        let out = r.push(b"\xff\xfe\nfine\n");
        assert_eq!(
            out,
            vec![Err(FrameError::InvalidUtf8), Ok("fine".to_string())]
        );
    }
}
