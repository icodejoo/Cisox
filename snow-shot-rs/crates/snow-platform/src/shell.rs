//! 系统外壳集成：在资源管理器中定位文件、用默认浏览器打开网页链接。

use std::path::Path;

/// 把路径规整成 explorer 认的形式：正斜杠一律换成反斜杠。
///
/// explorer 对混用分隔符（如 `C:/a\b.png`）会放弃定位并退回“此电脑”。
fn explorer_path_text(path: &Path) -> String {
    path.display().to_string().replace('/', "\\")
}

/// 生成 `explorer.exe` 的 `/select` 参数：路径规整为反斜杠并整体加引号，且不能拆成两个参数。
///
/// # 参数
/// - `path`：要选中的文件。
///
/// # 示例
/// ```
/// let arg = snow_platform::shell::explorer_select_arg(std::path::Path::new("C:/a b/c.mp4"));
/// assert_eq!(arg, r#"/select,"C:\a b\c.mp4""#);
/// ```
pub fn explorer_select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", explorer_path_text(path))
}

/// 生成 `explorer.exe` 打开目录的参数：路径规整为反斜杠并整体加引号。
///
/// # 参数
/// - `dir`：要打开的目录。
///
/// # 示例
/// ```
/// let arg = snow_platform::shell::explorer_open_arg(std::path::Path::new("C:/a b"));
/// assert_eq!(arg, r#""C:\a b""#);
/// ```
pub fn explorer_open_arg(dir: &Path) -> String {
    format!("\"{}\"", explorer_path_text(dir))
}

/// 在资源管理器中打开文件所在目录并选中该文件。
///
/// # 参数
/// - `path`：文件路径。
///
/// # 返回
/// 成功启动资源管理器返回 `Ok(())`；平台不支持或启动失败返回错误说明。
///
/// # 示例
/// ```no_run
/// snow_platform::shell::reveal_in_explorer(std::path::Path::new("C:/a.mp4")).ok();
/// ```
#[cfg(windows)]
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(explorer_select_arg(path))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开资源管理器: {e}"))
}

/// 在资源管理器中定位文件（非 Windows：不支持）。
#[cfg(not(windows))]
pub fn reveal_in_explorer(_path: &Path) -> Result<(), String> {
    Err("当前平台不支持在资源管理器中定位文件".to_string())
}

/// 在资源管理器中打开目录；目录不存在时先创建。
///
/// # 参数
/// - `dir`：目录路径。
///
/// # 返回
/// 成功启动资源管理器返回 `Ok(())`；创建目录或启动失败返回错误说明。
///
/// # 示例
/// ```no_run
/// snow_platform::shell::open_directory(std::path::Path::new("C:/Videos")).ok();
/// ```
#[cfg(windows)]
pub fn open_directory(dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("无法创建目录: {e}"))?;
    std::process::Command::new("explorer.exe")
        .raw_arg(explorer_open_arg(dir))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开资源管理器: {e}"))
}

/// 在资源管理器中打开目录（非 Windows：不支持）。
#[cfg(not(windows))]
pub fn open_directory(_dir: &Path) -> Result<(), String> {
    Err("当前平台不支持打开目录".to_string())
}

/// 可打开的网页链接允许的协议前缀（小写）。
const WEB_SCHEMES: [&str; 2] = ["http://", "https://"];
/// 网页链接最大长度（字符数），防止把整段文字当链接。
const WEB_LINK_MAX_LEN: usize = 2048;

/// 判断文本是否是可安全交给系统浏览器的网页链接，并返回规整后的链接。
///
/// # 参数
/// - `text`：任意文本（如二维码内容）。
///
/// # 返回
/// 去首尾空白后的链接；不是 `http://` / `https://`、没有主机、含空白或控制字符、过长时返回 `None`。
///
/// # 示例
/// ```
/// use snow_platform::shell::web_link;
/// assert_eq!(web_link(" https://a.b/c?x=1&y=2 ").as_deref(), Some("https://a.b/c?x=1&y=2"));
/// assert!(web_link("file:///C:/a.exe").is_none());
/// assert!(web_link("https://a b").is_none());
/// ```
pub fn web_link(text: &str) -> Option<String> {
    let url = text.trim();
    let lower = url.to_ascii_lowercase();
    let scheme = WEB_SCHEMES.iter().find(|s| lower.starts_with(**s))?;
    let host = url[scheme.len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or_default();
    let bad = url.chars().any(|c| c.is_whitespace() || c.is_control());
    (!bad && !host.is_empty() && !host.starts_with(':') && url.chars().count() <= WEB_LINK_MAX_LEN)
        .then(|| url.to_string())
}

/// 用系统默认浏览器打开网页链接（只接受 `http` / `https`）。
///
/// # 参数
/// - `url`：链接；先经 [`web_link`] 校验，不合格直接返回错误。
///
/// # 返回
/// 成功启动返回 `Ok(())`；链接不合格、平台不支持或启动失败返回错误说明。
///
/// # 示例
/// ```no_run
/// snow_platform::shell::open_url("https://example.com").ok();
/// ```
#[cfg(windows)]
pub fn open_url(url: &str) -> Result<(), String> {
    let url = web_link(url).ok_or_else(|| format!("不是可打开的网页链接: {url}"))?;
    // 直接把链接作为 explorer 的参数，由系统按默认浏览器打开；不经过 shell，`&` 等字符无需转义
    std::process::Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开浏览器: {e}"))
}

/// 用系统默认浏览器打开网页链接（非 Windows：不支持）。
#[cfg(not(windows))]
pub fn open_url(_url: &str) -> Result<(), String> {
    Err("当前平台不支持打开链接".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网页链接判定：只认带主机的 http / https，拒绝空白、控制字符与其它协议。
    #[test]
    fn web_link_accepts_only_safe_http() {
        assert_eq!(web_link("HTTP://a.b").as_deref(), Some("HTTP://a.b"));
        assert_eq!(
            web_link(
                "	https://a.b/p?q=1&r=2#f
"
            )
            .as_deref(),
            Some("https://a.b/p?q=1&r=2#f")
        );
        assert!(web_link("https://user@h.com:8080/x").is_some());
        for bad in [
            "",
            "https://",
            "https:///x",
            "http://:80",
            "ftp://a.b",
            "file:///C:/a",
            "javascript:alert(1)",
            "a.b/c",
            "https://a b",
            "https://a.b/x
y",
        ] {
            assert!(web_link(bad).is_none(), "{bad}");
        }
        assert!(web_link(&format!("https://a.b/{}", "x".repeat(WEB_LINK_MAX_LEN))).is_none());
    }

    /// 不合格的链接在打开前就被拒绝，不会启动任何进程。
    #[test]
    fn open_url_rejects_bad_links() {
        assert!(open_url("calc.exe").is_err());
        assert!(open_url("file:///C:/Windows/notepad.exe").is_err());
    }

    /// 长截图保存路径（正斜杠目录 + 反斜杠文件名混用）被规整为纯反斜杠。
    #[test]
    fn select_arg_normalizes_mixed_separators() {
        assert_eq!(
            explorer_select_arg(Path::new(r"C:/Users/me/Pictures\long 1.png")),
            r#"/select,"C:\Users\me\Pictures\long 1.png""#
        );
    }

    /// 打开目录的参数整体加引号。
    #[test]
    fn open_arg_quotes_dir() {
        assert_eq!(
            explorer_open_arg(Path::new("D:/My Videos")),
            "\"D:\\My Videos\""
        );
    }

    /// 参数带引号并保留空格。
    #[test]
    fn select_arg_quotes_path() {
        assert_eq!(
            explorer_select_arg(Path::new("D:/My Videos/x.gif")),
            "/select,\"D:\\My Videos\\x.gif\""
        );
    }
}
