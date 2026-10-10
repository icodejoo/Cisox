//! 当前 Unix 时间的统一取法：系统时钟早于 1970 年（异常）时一律回退为 0。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 距 Unix 纪元的时长；时钟异常时为零时长。
fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// 当前 Unix 时间（秒）。
///
/// # 返回值
/// 距 1970-01-01 UTC 的秒数；时钟异常时为 0。
///
/// # 示例
/// ```
/// assert!(snow_platform::unix_time::unix_secs() > 1_600_000_000);
/// ```
pub fn unix_secs() -> u64 {
    since_epoch().as_secs()
}

/// 当前 Unix 时间（毫秒）。
///
/// # 返回值
/// 距 1970-01-01 UTC 的毫秒数；溢出时饱和为 `i64::MAX`，时钟异常时为 0。
///
/// # 示例
/// ```
/// assert!(snow_platform::unix_time::unix_millis() > 1_600_000_000_000);
/// ```
pub fn unix_millis() -> i64 {
    i64::try_from(since_epoch().as_millis()).unwrap_or(i64::MAX)
}

/// 当前 Unix 时间（纳秒），常用于拼临时名或做随机种子。
///
/// # 返回值
/// 距 1970-01-01 UTC 的纳秒数；时钟异常时为 0。
///
/// # 示例
/// ```
/// assert!(snow_platform::unix_time::unix_nanos() > 0);
/// ```
pub fn unix_nanos() -> u128 {
    since_epoch().as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个粒度的量级关系一致，且不倒退。
    #[test]
    fn units_are_consistent_and_monotonic() {
        let s = unix_secs();
        let ms = unix_millis();
        let ns = unix_nanos();
        let s2 = unix_secs();
        assert!(s2 >= s);
        assert!(ms as u64 / 1000 >= s);
        assert!(ms as u64 / 1000 <= s2 + 1);
        assert!(ns / 1_000_000 >= ms as u128);
        assert!(unix_nanos() >= ns);
    }
}
