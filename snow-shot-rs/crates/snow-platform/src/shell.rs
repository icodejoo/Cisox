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

#[cfg(test)]
mod tests {
    use super::*;

    /// 参数带引号并保留空格。
    #[test]
    fn select_arg_quotes_path() {
        assert_eq!(
            explorer_select_arg(Path::new("D:/My Videos/x.gif")),
            "/select,\"D:/My Videos/x.gif\""
        );
    }
}
