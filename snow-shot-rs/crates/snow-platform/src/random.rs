//! 系统级安全随机数（用于令牌等密钥材料）。

/// 用系统随机源填满 `buf`。
///
/// Windows 走 `BCryptGenRandom`（系统首选 RNG），其它平台读 `/dev/urandom`。
///
/// # 参数
/// - `buf`：待填充的缓冲区。
///
/// # 返回
/// 成功返回 `Ok(())`；系统调用失败返回可读原因。
///
/// ```
/// let mut key = [0u8; 32];
/// snow_platform::random::fill_random(&mut key).unwrap();
/// ```
pub fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        // SAFETY: `buf` 是有效的可写切片，句柄传 None 并使用系统首选 RNG。
        let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status.0 != 0 {
            return Err(format!("BCryptGenRandom 失败 (0x{:08X})", status.0 as u32));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(buf))
            .map_err(|e| format!("读取系统随机源失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两次取值应不同且不全为零。
    #[test]
    fn random_bytes_differ() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        fill_random(&mut a).unwrap();
        fill_random(&mut b).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }
}
