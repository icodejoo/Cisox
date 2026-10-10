//! 系统偏好读取:深色模式与界面语言(经 `reg query`,无额外依赖)。

#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
use snow_net::quiet_command;

/// 系统个性化主题的注册表路径。
#[cfg(windows)]
const REG_PATH_PERSONALIZE: &str =
    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

/// 系统国际化设置的注册表路径。
#[cfg(windows)]
const REG_PATH_INTERNATIONAL: &str = "HKCU\\Control Panel\\International";

/// 深色模式的注册表值名。
#[cfg(windows)]
const REG_VALUE_APPS_USE_LIGHT_THEME: &str = "AppsUseLightTheme";

/// 界面语言的注册表值名。
#[cfg(windows)]
const REG_VALUE_LOCALE_NAME: &str = "LocaleName";

/// 界面语言读取失败时的默认值。
const DEFAULT_LANGUAGE: &str = "en-US";

/// `reg query` 输出中 DWORD 类型词。
const REG_TYPE_DWORD: &str = "REG_DWORD";

/// `reg query` 输出中字符串类型词。
const REG_TYPE_SZ: &str = "REG_SZ";

/// 在 `reg query` 输出里找值名匹配、类型词为 `type_word` 的那一行。
///
/// 参数:
/// - `output`: `reg query` 的完整标准输出。
/// - `type_word`: 类型词(如 `REG_SZ`)。
/// - `name`: 值名(大小写不敏感)。
///
/// 返回:`(整行, 类型词之后的各字段)`;没有匹配行返回 `None`。
fn find_reg_line<'a>(
    output: &'a str,
    type_word: &str,
    name: &str,
) -> Option<(&'a str, Vec<&'a str>)> {
    output.lines().find_map(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let type_idx = parts.iter().position(|&p| p == type_word)?;
        parts[..type_idx]
            .join(" ")
            .eq_ignore_ascii_case(name)
            .then(|| (line, parts[type_idx + 1..].to_vec()))
    })
}

/// 解析 `reg query` 输出中指定值名的 REG_DWORD 数值。
///
/// 参数:
/// - `output`: `reg query` 的完整标准输出。
/// - `name`: 值名(大小写不敏感)。
///
/// 返回:找到且形如 `0x1f` 的十六进制 DWORD 则返回数值,否则 `None`。
///
/// 示例输出行:`    AppsUseLightTheme    REG_DWORD    0x0`
pub fn parse_reg_dword(output: &str, name: &str) -> Option<u32> {
    let (_, rest) = find_reg_line(output, REG_TYPE_DWORD, name)?;
    let val = rest.first()?;
    let hex = val.strip_prefix("0x").or_else(|| val.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

/// 解析 `reg query` 输出中指定值名的 REG_SZ 字符串。
///
/// 参数:
/// - `output`: `reg query` 的完整标准输出。
/// - `name`: 值名(大小写不敏感)。
///
/// 返回:找到则返回去掉首尾空白的字符串,否则 `None`。
///
/// 示例输出行:`    LocaleName    REG_SZ    zh-CN`
pub fn parse_reg_sz(output: &str, name: &str) -> Option<String> {
    let (line, _) = find_reg_line(output, REG_TYPE_SZ, name)?;
    let pos = line.find(REG_TYPE_SZ)?;
    Some(line[pos + REG_TYPE_SZ.len()..].trim().to_string())
}

/// 执行 `reg query <path> /v <value>` 并返回标准输出,失败返回 `None`。
#[cfg(windows)]
fn reg_query(path: &str, value: &str) -> Option<String> {
    let output = quiet_command(Path::new("reg"))
        .args(["query", path, "/v", value])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// 系统是否使用深色应用主题(`AppsUseLightTheme` == 0)。
///
/// 返回:深色返回 `true`;读取失败或非 Windows 按浅色处理返回 `false`。结果不缓存。
///
/// 示例:`let dark = system_prefers_dark();`
pub fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        reg_query(REG_PATH_PERSONALIZE, REG_VALUE_APPS_USE_LIGHT_THEME)
            .and_then(|s| parse_reg_dword(&s, REG_VALUE_APPS_USE_LIGHT_THEME))
            .is_some_and(|v| v == 0)
    }
    #[cfg(not(windows))]
    false
}

/// 系统界面语言标记(如 `zh-CN`)。
///
/// 返回:语言标记;读取失败或非 Windows 返回 `en-US`。
///
/// 示例:`let lang = system_ui_language();`
pub fn system_ui_language() -> String {
    #[cfg(windows)]
    {
        reg_query(REG_PATH_INTERNATIONAL, REG_VALUE_LOCALE_NAME)
            .and_then(|s| parse_reg_sz(&s, REG_VALUE_LOCALE_NAME))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
    }
    #[cfg(not(windows))]
    DEFAULT_LANGUAGE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 示例 REG_DWORD 输出(值为 0)。
    const SAMPLE_DWORD: &str = "\r\nHKEY_X\\Y\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n\r\n";

    /// 示例 REG_DWORD 输出(值为 0x1f)。
    const SAMPLE_DWORD_HEX: &str = "\r\n    TestKey    REG_DWORD    0x1f\r\n\r\n";

    /// 示例 REG_DWORD 非十六进制输出。
    const SAMPLE_DWORD_INVALID: &str = "\r\n    TestKey    REG_DWORD    31\r\n\r\n";

    /// 示例 REG_SZ 输出。
    const SAMPLE_SZ: &str = "\r\n    LocaleName    REG_SZ    zh-CN\r\n\r\n";

    /// 示例 REG_SZ 值含空格的输出。
    const SAMPLE_SZ_SPACE: &str = "\r\n    Name    REG_SZ    a b\r\n\r\n";

    /// 示例输出应解析出 0。
    #[test]
    fn dword_zero() {
        assert_eq!(parse_reg_dword(SAMPLE_DWORD, "AppsUseLightTheme"), Some(0));
    }

    /// 十六进制 0x1f 应解析为 31。
    #[test]
    fn dword_hex() {
        assert_eq!(parse_reg_dword(SAMPLE_DWORD_HEX, "TestKey"), Some(31));
    }

    /// 值名大小写不同仍可匹配。
    #[test]
    fn dword_case_insensitive() {
        assert_eq!(parse_reg_dword(SAMPLE_DWORD, "appsuselighttheme"), Some(0));
    }

    /// 找不到值名返回 None。
    #[test]
    fn dword_not_found() {
        assert_eq!(parse_reg_dword(SAMPLE_DWORD, "Missing"), None);
    }

    /// 非十六进制数值返回 None。
    #[test]
    fn dword_invalid_hex() {
        assert_eq!(parse_reg_dword(SAMPLE_DWORD_INVALID, "TestKey"), None);
    }

    /// 示例输出应解析出 zh-CN。
    #[test]
    fn sz_found() {
        assert_eq!(parse_reg_sz(SAMPLE_SZ, "LocaleName"), Some("zh-CN".to_string()));
    }

    /// 值含空格时完整保留。
    #[test]
    fn sz_with_space() {
        assert_eq!(parse_reg_sz(SAMPLE_SZ_SPACE, "Name"), Some("a b".to_string()));
    }

    /// 找不到值名返回 None。
    #[test]
    fn sz_not_found() {
        assert_eq!(parse_reg_sz(SAMPLE_SZ, "Missing"), None);
    }
}
