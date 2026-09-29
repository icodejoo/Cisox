//! 极简 UTC 时间工具：解析 `Qt::ISODateWithMs` 文本，避免引入时间库。

use std::time::{SystemTime, UNIX_EPOCH};

/// 一天的毫秒数。
pub const MILLIS_PER_DAY: i64 = 86_400_000;

/// 公历日期换算为自 1970-01-01 起的天数（Howard Hinnant 算法）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 解析 `yyyy-MM-ddTHH:mm:ss[.fff]Z` 为 UTC 毫秒时间戳；格式非法返回 `None`。
///
/// # 示例
/// ```
/// use snow_history::timeutil::parse_iso_utc_ms;
/// assert_eq!(parse_iso_utc_ms("1970-01-01T00:00:01.250Z"), Some(1250));
/// assert_eq!(parse_iso_utc_ms("not a date"), None);
/// ```
pub fn parse_iso_utc_ms(text: &str) -> Option<i64> {
    let body = text.strip_suffix('Z')?;
    let (date, time) = body.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || date.len() != 10 {
        return None;
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || clock.len() != 8 {
        return None;
    }
    let millis: i64 = match fraction {
        None => 0,
        Some(f) if !f.is_empty() && f.len() <= 3 && f.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{f:0<3}").parse().ok()?
        }
        Some(_) => return None,
    };
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * MILLIS_PER_DAY + ((hour * 60 + minute) * 60 + second) * 1000 + millis)
}

/// 自 1970-01-01 起的天数换算公历日期（Howard Hinnant 算法）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// 把 UTC 毫秒时间戳格式化为 `yyyy-MM-ddTHH:mm:ss.mmmZ`（即 `Qt::ISODateWithMs`）。
///
/// # 示例
/// ```
/// use snow_history::timeutil::format_iso_utc_ms;
/// assert_eq!(format_iso_utc_ms(1250), "1970-01-01T00:00:01.250Z");
/// ```
pub fn format_iso_utc_ms(ms: i64) -> String {
    let days = ms.div_euclid(MILLIS_PER_DAY);
    let rest = ms.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let seconds = rest / 1000;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        rest % 1000
    )
}

/// 当前 UTC 毫秒时间戳。
pub fn now_utc_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知日期换算正确，含闰年与无毫秒写法。
    #[test]
    fn parses_known_instants() {
        assert_eq!(
            parse_iso_utc_ms("2000-03-01T00:00:00Z"),
            Some(951_868_800_000)
        );
        assert_eq!(
            parse_iso_utc_ms("2024-02-29T00:00:00.001Z"),
            Some(1_709_164_800_001)
        );
        assert_eq!(parse_iso_utc_ms("2023-02-29T00:00:00Z"), None);
        assert_eq!(parse_iso_utc_ms("2026-08-05T12:30:00.000"), None);
        assert_eq!(parse_iso_utc_ms("2026-08-05T24:00:00Z"), None);
    }

    /// 格式化与解析互逆。
    #[test]
    fn format_and_parse_roundtrip() {
        for ms in [0, 1, 951_868_800_123, 1_709_164_800_001, 1_785_000_000_999] {
            assert_eq!(parse_iso_utc_ms(&format_iso_utc_ms(ms)), Some(ms));
        }
    }
}
