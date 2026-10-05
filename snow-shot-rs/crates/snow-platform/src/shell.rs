//! 系统外壳集成：在资源管理器中定位文件。

use std::path::Path;

/// 生成 `explorer.exe` 的 `/select` 参数：路径必须整体加引号，且不能拆成两个参数。
///
/// # 参数
/// - `path`：要选中的文件。
///
/// # 示例
/// ```
/// let arg = snow_platform::shell::explorer_select_arg(std::path::Path::new("C:/a b/c.mp4"));
/// assert_eq!(arg, "/select,\"C:/a b/c.mp4\"");
/// ```
pub fn explorer_select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", path.display())
}

/// 生成 `explorer.exe` 打开目录的参数：路径整体加引号。
///
/// # 参数
/// - `dir`：要打开的目录。
///
/// # 示例
/// ```
/// let arg = snow_platform::shell::explorer_open_arg(std::path::Path::new("C:/a b"));
/// assert_eq!(arg, "\"C:/a b\"");
/// ```
pub fn explorer_open_arg(dir: &Path) -> String {
    format!("\"{}\"", dir.display())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 打开目录的参数整体加引号。
    #[test]
    fn open_arg_quotes_dir() {
        assert_eq!(explorer_open_arg(Path::new("D:/My Videos")), "\"D:/My Videos\"");
    }

    /// 参数带引号并保留空格。
    #[test]
    fn select_arg_quotes_path() {
        assert_eq!(
            explorer_select_arg(Path::new("D:/My Videos/x.gif")),
            "/select,\"D:/My Videos/x.gif\""
        );
    }
}
