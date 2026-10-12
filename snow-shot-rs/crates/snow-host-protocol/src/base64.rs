//! 最小 base64（RFC 4648 标准字母表，带 `=` 填充）。
//!
//! 只服务快照里的图标像素。为了让宿主的依赖闭包只剩 serde，这里自带实现而不引入 `base64` crate。
//! 同时提供 `serde(with = "...")` 用的字节序列化适配，见 [`bytes`]。

/// 标准字母表。
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 填充字符。
const PAD: u8 = b'=';

/// base64 解码失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// 长度不是 4 的倍数。
    BadLength,
    /// 含字母表之外的字符，或填充位置不对。
    BadCharacter,
}

impl std::fmt::Display for DecodeError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadLength => write!(f, "base64 长度不是 4 的倍数"),
            DecodeError::BadCharacter => write!(f, "base64 含非法字符或填充位置不对"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// 把字节编码为 base64 文本。
///
/// # 参数
/// - `data`：原始字节。
///
/// # 返回
/// 带 `=` 填充的 base64 字符串。
///
/// ```rust
/// assert_eq!(snow_host_protocol::base64::encode(b"Man"), "TWFu");
/// assert_eq!(snow_host_protocol::base64::encode(b"Ma"), "TWE=");
/// ```
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            PAD as char
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            PAD as char
        });
    }
    out
}

/// 字母表字符转 6 位值。
fn value_of(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// 解码 base64 文本（严格：必须带填充，不接受空白与 URL 安全字母表）。
///
/// # 参数
/// - `text`：base64 字符串；空串解码为空字节。
///
/// # 返回
/// 原始字节，或 [`DecodeError`]。
///
/// ```rust
/// assert_eq!(snow_host_protocol::base64::decode("TWE=").unwrap(), b"Ma");
/// assert!(snow_host_protocol::base64::decode("TWE").is_err());
/// ```
pub fn decode(text: &str) -> Result<Vec<u8>, DecodeError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(DecodeError::BadLength);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let last_quad = bytes.len().saturating_sub(4);
    for (i, quad) in bytes.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|&&c| c == PAD).count();
        // 填充只能出现在最后一组的末尾，且最多两个
        if pad > 2 || (pad > 0 && i * 4 != last_quad) {
            return Err(DecodeError::BadCharacter);
        }
        let mut n = 0u32;
        for (j, &c) in quad.iter().enumerate() {
            let v = if j >= 4 - pad {
                0
            } else {
                value_of(c).ok_or(DecodeError::BadCharacter)?
            };
            n = (n << 6) | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// `#[serde(with = "snow_host_protocol::base64::bytes")]`：把 `Vec<u8>` 存成 base64 字符串。
pub mod bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    /// 序列化为 base64 字符串。
    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::encode(data))
    }

    /// 从 base64 字符串反序列化。
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::decode(&text).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 第 10 节的标准测试向量。
    #[test]
    fn rfc4648_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), coded, "encode {plain:?}");
            assert_eq!(decode(coded).unwrap(), plain.as_bytes(), "decode {coded:?}");
        }
    }

    /// 全部 256 个字节值与各种长度都能往返。
    #[test]
    fn round_trips_all_byte_values() {
        let all: Vec<u8> = (0..=255).collect();
        for len in [0, 1, 2, 3, 4, 5, 255, 256] {
            let slice = &all[..len.min(all.len())];
            assert_eq!(decode(&encode(slice)).unwrap(), slice);
        }
        // 32x32 RGBA 图标大小
        let icon = vec![0xAB; 32 * 32 * 4];
        assert_eq!(decode(&encode(&icon)).unwrap(), icon);
    }

    /// 非法输入：长度、字符、填充位置。
    #[test]
    fn rejects_malformed_input() {
        assert_eq!(decode("abc"), Err(DecodeError::BadLength));
        assert_eq!(decode("ab!d"), Err(DecodeError::BadCharacter));
        assert_eq!(decode("a b="), Err(DecodeError::BadCharacter));
        assert_eq!(decode("=abc"), Err(DecodeError::BadCharacter));
        assert_eq!(decode("Zg==Zg=="), Err(DecodeError::BadCharacter));
        assert_eq!(decode("Z==="), Err(DecodeError::BadCharacter));
        // URL 安全字母表不接受
        assert_eq!(decode("-_-_"), Err(DecodeError::BadCharacter));
    }

    /// serde 适配：存成字符串，读回字节。
    #[test]
    fn serde_adapter_round_trip() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder {
            #[serde(with = "super::bytes")]
            data: Vec<u8>,
        }
        let h = Holder {
            data: vec![1, 2, 3, 4, 5],
        };
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, r#"{"data":"AQIDBAU="}"#);
        assert_eq!(serde_json::from_str::<Holder>(&json).unwrap(), h);
        assert!(serde_json::from_str::<Holder>(r#"{"data":"%%%%"}"#).is_err());
    }
}
