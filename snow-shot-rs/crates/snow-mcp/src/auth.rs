//! 令牌生成、常量时间比较与失败退避。令牌绝不进日志（`Debug` 已脱敏）。

use std::fmt;
use std::time::Duration;

/// 令牌原始字节数。
pub const TOKEN_BYTES: usize = 32;
/// 退避基数（毫秒）。
const BACKOFF_BASE_MS: u64 = 50;
/// 退避上限。
const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// 一次启用周期内有效的访问令牌（64 位十六进制文本）。
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// 用给定随机源生成新令牌。
    ///
    /// # 参数
    /// - `fill`：把缓冲区填满安全随机字节的函数（生产用系统 RNG，测试可注入）。
    ///
    /// # 返回
    /// 新令牌；随机源失败返回其原因。
    pub fn generate(fill: impl FnOnce(&mut [u8]) -> Result<(), String>) -> Result<Self, String> {
        let mut bytes = [0u8; TOKEN_BYTES];
        fill(&mut bytes)?;
        Ok(Self(hex(&bytes)))
    }

    /// 用系统随机源生成新令牌。
    pub fn generate_system() -> Result<Self, String> {
        Self::generate(snow_platform::random::fill_random)
    }

    /// 令牌文本（只用于写描述符文件，勿记日志）。
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 常量时间比较候选值是否等于本令牌。
    ///
    /// # 参数
    /// - `candidate`：客户端提交的文本。
    pub fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl fmt::Debug for Token {
    /// 脱敏输出，避免令牌经 `{:?}` 进入日志。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// 字节转小写十六进制。
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 常量时间比较（长度不同直接不等；长度本身不是秘密）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 连续鉴权失败计数与指数退避。
#[derive(Debug, Default)]
pub struct AuthGate {
    /// 连续失败次数。
    failures: u32,
}

impl AuthGate {
    /// 登记一次失败。
    ///
    /// # 返回
    /// 回应前应等待的时长：`50ms * 2^(n-1)`，上限 5 秒。
    pub fn record_failure(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let shift = (self.failures - 1).min(16);
        Duration::from_millis(BACKOFF_BASE_MS << shift).min(BACKOFF_MAX)
    }

    /// 登记一次成功（清零）。
    pub fn record_success(&mut self) {
        self.failures = 0;
    }

    /// 当前连续失败次数。
    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注入随机源：长度 64、全十六进制；两次不同；随机源失败时报错。
    #[test]
    fn generates_hex_token() {
        let t = Token::generate(|b| {
            b.fill(0xAB);
            Ok(())
        })
        .unwrap();
        assert_eq!(t.expose().len(), TOKEN_BYTES * 2);
        assert!(t.expose().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(Token::generate(|_| Err("x".into())).is_err());
        assert_ne!(
            Token::generate_system().unwrap(),
            Token::generate_system().unwrap()
        );
    }

    /// 比较：相同通过，长度不同或一位之差失败；Debug 不泄漏。
    #[test]
    fn compare_and_redaction() {
        let t = Token::generate(|b| {
            b.fill(1);
            Ok(())
        })
        .unwrap();
        let secret = t.expose().to_string();
        assert!(t.matches(&secret));
        assert!(!t.matches(&secret[1..]));
        assert!(!t.matches(&format!("{}0", &secret[..secret.len() - 1])));
        assert!(!format!("{t:?}").contains(&secret));
    }

    /// 退避翻倍、封顶、成功后清零。
    #[test]
    fn backoff_doubles_and_caps() {
        let mut g = AuthGate::default();
        assert_eq!(g.record_failure(), Duration::from_millis(50));
        assert_eq!(g.record_failure(), Duration::from_millis(100));
        assert_eq!(g.record_failure(), Duration::from_millis(200));
        for _ in 0..40 {
            g.record_failure();
        }
        assert_eq!(g.record_failure(), BACKOFF_MAX);
        g.record_success();
        assert_eq!(g.failures(), 0);
        assert_eq!(g.record_failure(), Duration::from_millis(50));
    }
}
