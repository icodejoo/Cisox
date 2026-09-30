//! 本地时间读取：用于按用户时区生成文件名时间戳。

/// 本地日期时间的各字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    /// 年（四位）。
    pub year: u16,
    /// 月（1..=12）。
    pub month: u8,
    /// 日（1..=31）。
    pub day: u8,
    /// 时（0..=23）。
    pub hour: u8,
    /// 分（0..=59）。
    pub minute: u8,
    /// 秒（0..=59）。
    pub second: u8,
}

/// 读取当前本地时间。
///
/// # 返回
/// 本地日期时间；Windows 使用系统本地时区，其它平台退化为 UTC。
///
/// # 示例
/// ```
/// let now = snow_platform::local_time::now();
/// assert!((1..=12).contains(&now.month));
/// ```
#[cfg(windows)]
pub fn now() -> LocalDateTime {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime 无前置条件，返回值为按值拷贝的 SYSTEMTIME。
    let t = unsafe { GetLocalTime() };
    LocalDateTime {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
    }
}

/// 读取当前本地时间（非 Windows：UTC）。
#[cfg(not(windows))]
pub fn now() -> LocalDateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    from_unix_utc(secs)
}

/// 由 Unix 秒换算 UTC 日期时间（公历）。
///
/// # 参数
/// - `unix_secs`：Unix 时间戳（秒）。
///
/// # 示例
/// ```
/// let t = snow_platform::local_time::from_unix_utc(86_400 + 3_661);
/// assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (1970, 1, 2, 1, 1, 1));
/// ```
pub fn from_unix_utc(unix_secs: u64) -> LocalDateTime {
    const SECONDS_PER_DAY: u64 = 86_400;
    let days = (unix_secs / SECONDS_PER_DAY) as i64;
    let rem = unix_secs % SECONDS_PER_DAY;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    LocalDateTime {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: (rem / 3600) as u8,
        minute: (rem % 3600 / 60) as u8,
        second: (rem % 60) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知时间点换算正确（含闰年 2000-02-29）。
    #[test]
    fn unix_conversion_known_points() {
        let t = from_unix_utc(951_782_400); // 2000-02-29 00:00:00 UTC
        assert_eq!((t.year, t.month, t.day), (2000, 2, 29));
        let t = from_unix_utc(0);
        assert_eq!((t.year, t.month, t.day, t.hour), (1970, 1, 1, 0));
    }

    /// 当前时间字段在合法范围内。
    #[test]
    fn now_is_sane() {
        let t = now();
        assert!(t.year >= 2024);
        assert!((1..=31).contains(&t.day));
        assert!(t.hour < 24 && t.minute < 60 && t.second < 61);
    }
}
