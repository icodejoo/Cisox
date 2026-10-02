//! 截图导出的命名规则：文件名模板展开、扩展名归一、重名避让。
//!
//! 规则与旧 Qt 版 `ScreenshotImageFileService` 一致（模板里 `{}` 内按 Qt `QDateTime::toString` 语法展开）。

use snow_platform::local_time::LocalDateTime;
use std::path::{Path, PathBuf};

/// 重名避让时最多尝试的序号。
const MAX_COLLISION_ATTEMPTS: u32 = 10_000;
/// 英文星期全名（周日起）。
const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
/// 英文月份全名。
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// 缩写长度（`ddd` / `MMM`）。
const ABBREVIATION_LEN: usize = 3;

/// 计算星期（0 = 周日），按公历日期推算。
fn weekday(t: &LocalDateTime) -> usize {
    // Sakamoto 算法
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = i32::from(t.year);
    let m = usize::from(t.month.clamp(1, 12)) - 1;
    if m < 2 {
        y -= 1;
    }
    ((y + y / 4 - y / 100 + y / 400 + OFFSETS[m] + i32::from(t.day)).rem_euclid(7)) as usize
}

/// 月份名（全名），月份越界时收敛到 1..=12。
fn month_name(t: &LocalDateTime) -> &'static str {
    MONTH_NAMES[usize::from(t.month.clamp(1, 12)) - 1]
}

/// 取 `chars[i..]` 中连续相同字符的个数。
fn run_len(chars: &[char], i: usize) -> usize {
    chars[i..].iter().take_while(|c| **c == chars[i]).count()
}

/// 数字字段：连续 2 个及以上补零成两位，否则不补零。返回（文本，消耗字符数）。
fn number_field(value: u8, run: usize) -> (String, usize) {
    if run >= 2 {
        (format!("{value:02}"), 2)
    } else {
        (value.to_string(), 1)
    }
}

/// 格式串里（引号外）是否出现 AM/PM 标记，决定 `h` 是否用 12 小时制。
fn uses_meridiem(chars: &[char]) -> bool {
    let mut quoted = false;
    for c in chars {
        match c {
            '\'' => quoted = !quoted,
            'A' | 'a' if !quoted => return true,
            _ => {}
        }
    }
    false
}

/// 按 Qt `QDateTime::toString` 语法展开一段日期时间格式（毫秒恒为 0）。
///
/// # 参数
/// - `format`：Qt 格式串（已把 `YYYY/YY/DD` 换成 Qt 写法）。
/// - `t`：本地时间。
///
/// # 返回
/// 展开后的文本。支持 `d dd ddd dddd M MM MMM MMMM yy yyyy h hh H HH m mm s ss z zzz AP A ap a`，
/// 单引号包裹的内容原样输出，星期 / 月份名固定为英文，其它字符原样保留。
///
/// ```ignore
/// let t = LocalDateTime { year: 2026, month: 8, day: 14, hour: 9, minute: 7, second: 6 };
/// assert_eq!(format_qt("yyyyMMdd_HHmmss", &t), "20260814_090706");
/// ```
pub fn format_qt(format: &str, t: &LocalDateTime) -> String {
    let chars: Vec<char> = format.chars().collect();
    let meridiem = uses_meridiem(&chars);
    let hour12 = match t.hour % 12 {
        0 => 12,
        h => h,
    };
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // 引号段：`''` 表示一个撇号
            if chars.get(i + 1) == Some(&'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let run = run_len(&chars, i);
        let (text, used): (String, usize) = match c {
            'd' => match run {
                1 | 2 => number_field(t.day, run),
                3 => (DAY_NAMES[weekday(t)][..ABBREVIATION_LEN].to_string(), 3),
                _ => (DAY_NAMES[weekday(t)].to_string(), 4),
            },
            'M' => match run {
                1 | 2 => number_field(t.month, run),
                3 => (month_name(t)[..ABBREVIATION_LEN].to_string(), 3),
                _ => (month_name(t).to_string(), 4),
            },
            'y' if run >= 4 => (format!("{:04}", t.year), 4),
            'y' if run == 2 => (format!("{:02}", t.year % 100), 2),
            'h' => number_field(if meridiem { hour12 } else { t.hour }, run),
            'H' => number_field(t.hour, run),
            'm' => number_field(t.minute, run),
            's' => number_field(t.second, run),
            'z' if run >= 3 => ("000".to_string(), 3),
            'z' => ("0".to_string(), 1),
            'A' | 'a' => {
                // `AP` / `ap` 为两字符写法
                let used = if chars
                    .get(i + 1)
                    .is_some_and(|n| n.eq_ignore_ascii_case(&'p'))
                {
                    2
                } else {
                    1
                };
                let word = if t.hour < 12 { "AM" } else { "PM" };
                let text = if c == 'A' {
                    word.to_string()
                } else {
                    word.to_lowercase()
                };
                (text, used)
            }
            _ => (c.to_string(), 1),
        };
        out.push_str(&text);
        i += used;
    }
    out
}

/// 展开文件名模板：`{...}` 内按日期时间格式展开，其余原样保留；先去掉首尾空白。
///
/// # 参数
/// - `template`：文件名模板（如 `Cisox_{YYYY-MM-DD_HH-mm-ss}`）。
/// - `t`：本地时间。
///
/// # 返回
/// 不含扩展名的文件名主干。`YYYY` / `YY` / `DD` 会先转成 Qt 写法（与旧版一致）。
///
/// ```ignore
/// let name = expand_template("SnowShot_{YYYY-MM-DD_HH-mm-ss}", &t);
/// assert_eq!(name, "SnowShot_2026-08-14_09-07-06");
/// ```
pub fn expand_template(template: &str, t: &LocalDateTime) -> String {
    let mut out = String::new();
    let mut rest = template.trim();
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        // 与旧版正则 `\{([^{}]+)\}` 一致：括号内不能再含 `{` / `}`，且至少一个字符
        match after.find(['{', '}']) {
            Some(end) if end > 0 && after[end..].starts_with('}') => {
                out.push_str(&rest[..open]);
                let qt = after[..end]
                    .replace("YYYY", "yyyy")
                    .replace("YY", "yy")
                    .replace("DD", "dd");
                out.push_str(&format_qt(&qt, t));
                rest = &after[end + 1..];
            }
            _ => {
                // 不是合法占位符：把这个 `{` 当普通字符继续扫描
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 文件名主干是否可用：非空且不含路径分隔符。
///
/// ```ignore
/// assert!(is_valid_base_name("a_b"));
/// assert!(!is_valid_base_name("a/b"));
/// ```
pub fn is_valid_base_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\')
}

/// 清理路径：统一为正斜杠，折叠重复分隔符与 `.`，尽量消解 `..`（对应 `QDir::cleanPath`）。
///
/// ```ignore
/// assert_eq!(clean_path("a\\b/../c//d.png"), "a/c/d.png");
/// ```
pub fn clean_path(path: &str) -> String {
    let unified = path.trim().replace('\\', "/");
    if unified.is_empty() {
        return String::new();
    }
    let absolute = unified.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts
                    .last()
                    .is_some_and(|p| *p != ".." && !p.ends_with(':'))
                {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// 让路径的扩展名与输出格式一致：已有扩展名被替换，没有则追加（对应 `normalizedPath`）。
///
/// # 参数
/// - `path`：用户选择或生成的路径。
/// - `extension`：目标扩展名（不含点）。
///
/// # 返回
/// 归一后的路径（正斜杠）；路径为空返回空串。以点开头的无扩展名文件保留原名（`.capture` → `.capture.png`）。
///
/// ```ignore
/// assert_eq!(normalized_path("capture.jpg", "png"), "capture.png");
/// assert_eq!(normalized_path(".capture", "png"), ".capture.png");
/// ```
pub fn normalized_path(path: &str, extension: &str) -> String {
    let mut cleaned = clean_path(path);
    if cleaned.is_empty() {
        return cleaned;
    }
    let name_start = cleaned.rfind('/').map_or(0, |i| i + 1);
    let file_name = &cleaned[name_start..];
    match file_name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < file_name.len() => {
            let cut = file_name.len() - dot;
            cleaned.truncate(cleaned.len() - cut);
        }
        _ if cleaned.ends_with('.') => {
            cleaned.pop();
        }
        _ => {}
    }
    format!("{cleaned}.{extension}")
}

/// 生成不与已有文件冲突的路径：`dir/base.ext`，冲突时依次尝试 `base_1.ext`、`base_2.ext`……
///
/// # 参数
/// - `dir`：目录。
/// - `base`：文件名主干。
/// - `extension`：扩展名（不含点）。
///
/// # 返回
/// 尚不存在的路径；序号耗尽返回 `None`。
///
/// ```ignore
/// let p = collision_safe_path(&dir, "shot", "png"); // shot.png / shot_1.png ...
/// ```
pub fn collision_safe_path(dir: &Path, base: &str, extension: &str) -> Option<PathBuf> {
    let first = dir.join(format!("{base}.{extension}"));
    if !first.exists() {
        return Some(first);
    }
    (1..=MAX_COLLISION_ATTEMPTS)
        .map(|n| dir.join(format!("{base}_{n}.{extension}")))
        .find(|p| !p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对照 Qt 测试用的固定时间：2026-08-14 09:07:06。
    fn golden_time() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 8,
            day: 14,
            hour: 9,
            minute: 7,
            second: 6,
        }
    }

    /// 对照 `screenshot_image_file_service_tests.cpp`：默认模板与大写令牌。
    #[test]
    fn template_matches_qt_golden_vectors() {
        let t = golden_time();
        assert_eq!(
            expand_template("SnowShot_{YYYY-MM-DD_HH-mm-ss}", &t),
            "SnowShot_2026-08-14_09-07-06"
        );
        assert_eq!(
            expand_template("Capture_{yyyyMMdd}_{HHmmss}_{zzz}", &t),
            "Capture_20260814_090706_000"
        );
        assert_eq!(
            expand_template("Auto_{yyyyMMdd_HHmmss}", &t),
            "Auto_20260814_090706"
        );
    }

    /// 产品名默认模板能展开，且首尾空白被去掉。
    #[test]
    fn template_default_and_trim() {
        let t = golden_time();
        let default = format!("{}_{{YYYY-MM-DD_HH-mm-ss}}", snow_app_core::PRODUCT_NAME);
        assert_eq!(
            expand_template(&format!("  {default}  "), &t),
            format!("{}_2026-08-14_09-07-06", snow_app_core::PRODUCT_NAME)
        );
    }

    /// Qt 其它令牌：星期、月份名、12 小时制、引号文本、不完整花括号。
    #[test]
    fn qt_tokens() {
        let t = golden_time();
        // 2026-08-14 是周五
        assert_eq!(format_qt("dddd ddd d dd", &t), "Friday Fri 14 14");
        assert_eq!(format_qt("MMMM MMM M MM yy", &t), "August Aug 8 08 26");
        assert_eq!(format_qt("h:mm AP", &t), "9:07 AM");
        let afternoon = LocalDateTime { hour: 15, ..t };
        assert_eq!(format_qt("hh ap", &afternoon), "03 pm");
        assert_eq!(format_qt("H h", &afternoon), "15 15");
        assert_eq!(format_qt("'at' HH", &t), "at 09");
        assert_eq!(expand_template("a{b", &t), "a{b");
        assert_eq!(expand_template("a{}b", &t), "a{}b");
        assert_eq!(expand_template("{{yyyy}}", &t), "{2026}");
    }

    /// 星期推算：已知日期。
    #[test]
    fn weekday_known_dates() {
        let mk = |year, month, day| LocalDateTime {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(weekday(&mk(2000, 1, 1)), 6);
        assert_eq!(weekday(&mk(2024, 2, 29)), 4);
        assert_eq!(weekday(&mk(1970, 1, 1)), 4);
    }

    /// 对照 Qt `normalizedPath` 用例。
    #[test]
    fn normalized_path_matches_qt() {
        assert_eq!(normalized_path("capture.png", "pdf"), "capture.pdf");
        assert_eq!(normalized_path("capture.unknown", "png"), "capture.png");
        assert_eq!(normalized_path("capture.jpg", "png"), "capture.png");
        assert_eq!(normalized_path("capture", "webp"), "capture.webp");
        assert_eq!(normalized_path("capture.png", "bmp"), "capture.bmp");
        assert_eq!(normalized_path(".capture", "png"), ".capture.png");
        assert_eq!(
            normalized_path("dir.v2/capture", "png"),
            "dir.v2/capture.png"
        );
        assert_eq!(normalized_path("capture.", "png"), "capture.png");
        assert_eq!(normalized_path("  ", "png"), "");
    }

    /// 路径清理：反斜杠、重复分隔符、`..`。
    #[test]
    fn clean_path_rules() {
        assert_eq!(clean_path("C:\\a\\b\\..\\c.png"), "C:/a/c.png");
        assert_eq!(clean_path("a//b/./c"), "a/b/c");
        assert_eq!(clean_path(""), "");
    }

    /// 文件名主干校验。
    #[test]
    fn base_name_validation() {
        assert!(is_valid_base_name("x"));
        assert!(!is_valid_base_name(""));
        assert!(!is_valid_base_name("a\\b"));
        assert!(!is_valid_base_name("a/b"));
    }

    /// 重名时按 `_1`、`_2` 递增（对照 Qt `collisionSafePath`）。
    #[test]
    fn collision_suffixes_increment() {
        let dir = std::env::temp_dir().join(format!("cisox-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = collision_safe_path(&dir, "shot", "png").unwrap();
        assert_eq!(first, dir.join("shot.png"));
        std::fs::write(&first, b"x").unwrap();
        let second = collision_safe_path(&dir, "shot", "png").unwrap();
        assert_eq!(second, dir.join("shot_1.png"));
        std::fs::write(&second, b"x").unwrap();
        assert_eq!(
            collision_safe_path(&dir, "shot", "png").unwrap(),
            dir.join("shot_2.png")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
