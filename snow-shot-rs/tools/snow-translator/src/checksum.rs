//! 流式 SHA-256（不引入第三方依赖），用于加载前校验模型文件。
//!
//! 只持有 64 字节块缓冲，内存占用与文件大小无关。

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// SHA-256 分块大小（字节）。
const BLOCK_LEN: usize = 64;
/// 读文件的缓冲大小（字节）。
const READ_BUF_LEN: usize = 256 * 1024;
/// SHA-256 摘要的十六进制长度。
pub const HEX_LEN: usize = 64;

/// SHA-256 轮常量。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 初始哈希值。
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 流式 SHA-256 计算器。
///
/// # 示例
/// ```ignore
/// let mut h = Sha256::new();
/// h.update(b"abc");
/// assert_eq!(h.finish_hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
/// ```
pub struct Sha256 {
    /// 当前哈希状态。
    state: [u32; 8],
    /// 未满一块的尾部缓冲。
    buf: [u8; BLOCK_LEN],
    /// `buf` 已用字节数。
    buf_len: usize,
    /// 已输入总字节数。
    total: u64,
}

impl Default for Sha256 {
    /// 等同 [`Sha256::new`]。
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// 创建空的计算器。
    pub fn new() -> Self {
        Self {
            state: H0,
            buf: [0; BLOCK_LEN],
            buf_len: 0,
            total: 0,
        }
    }

    /// 处理一个 64 字节块。
    fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    /// 追加数据。
    ///
    /// # 参数
    /// - `data`：任意长度字节。
    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (BLOCK_LEN - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < BLOCK_LEN {
                return;
            }
            let block = self.buf;
            Self::compress(&mut self.state, &block);
            self.buf_len = 0;
        }
        let mut blocks = data.chunks_exact(BLOCK_LEN);
        for b in &mut blocks {
            let mut block = [0u8; BLOCK_LEN];
            block.copy_from_slice(b);
            Self::compress(&mut self.state, &block);
        }
        let rest = blocks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    /// 结束计算并返回小写十六进制摘要。
    ///
    /// # 返回
    /// 64 字符的小写十六进制字符串。
    pub fn finish_hex(mut self) -> String {
        let bit_len = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let used = (self.buf_len + 1) % BLOCK_LEN;
        let zeros = if used <= BLOCK_LEN - 8 {
            BLOCK_LEN - 8 - used
        } else {
            2 * BLOCK_LEN - 8 - used
        };
        pad.extend(std::iter::repeat_n(0u8, zeros));
        pad.extend_from_slice(&bit_len.to_be_bytes());
        // 填充不计入总长度
        let total = self.total;
        self.update(&pad);
        self.total = total;
        self.state.iter().map(|w| format!("{w:08x}")).collect()
    }
}

/// 流式计算文件的 SHA-256。
///
/// # 参数
/// - `path`：文件路径。
///
/// # 返回
/// 小写十六进制摘要；读文件失败返回 IO 错误。
///
/// # 示例
/// ```ignore
/// let hex = sha256_file(Path::new("encoder.onnx"))?;
/// ```
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; READ_BUF_LEN];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish_hex())
}

/// 校验文件摘要是否等于期望值（十六进制，忽略大小写与首尾空白）。
///
/// # 参数
/// - `path`：文件路径。
/// - `expected`：期望的 64 位十六进制摘要。
///
/// # 返回
/// `Ok(())` 表示一致；不一致返回带实际/期望摘要的说明，读取失败返回 IO 原因。
///
/// # 示例
/// ```ignore
/// verify_file(Path::new("a.onnx"), "d3b7…")?;
/// ```
pub fn verify_file(path: &Path, expected: &str) -> Result<(), String> {
    let expected = expected.trim().to_ascii_lowercase();
    let actual = sha256_file(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "sha256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        ))
    }
}

/// 判断字符串是否是合法的 SHA-256 十六进制摘要。
///
/// # 参数
/// - `s`：待检查文本。
///
/// # 返回
/// 64 位十六进制返回 `true`。
pub fn is_valid_hex(s: &str) -> bool {
    let s = s.trim();
    s.len() == HEX_LEN && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对整段字节计算十六进制摘要。
    fn hex_of(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish_hex()
    }

    /// 标准向量：空串、abc、56 字节跨块填充边界。
    #[test]
    fn known_vectors() {
        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// 分块喂入与一次性喂入结果一致（含一百万个 a 的标准向量）。
    #[test]
    fn chunked_update_matches() {
        let data = vec![b'a'; 1_000_000];
        let mut h = Sha256::new();
        for part in data.chunks(777) {
            h.update(part);
        }
        assert_eq!(
            h.finish_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// 文件校验：一致通过，不一致/文件缺失给出明确说明。
    #[test]
    fn verify_file_cases() {
        let path = std::env::temp_dir().join(format!("snow-translator-sha-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let good = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        assert!(verify_file(&path, good).is_ok());
        let err = verify_file(&path, &"0".repeat(HEX_LEN)).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "{err}");
        std::fs::remove_file(&path).unwrap();
        assert!(
            verify_file(&path, good)
                .unwrap_err()
                .contains("cannot read")
        );
    }

    /// 十六进制合法性判断。
    #[test]
    fn hex_validation() {
        assert!(is_valid_hex(&"a".repeat(HEX_LEN)));
        assert!(!is_valid_hex("abc"));
        assert!(!is_valid_hex(&"g".repeat(HEX_LEN)));
    }
}
