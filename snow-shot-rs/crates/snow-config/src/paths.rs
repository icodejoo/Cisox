//! 数据根目录解析：自有目录（方案 §10 T1/T2）、便携模式标记、对 upstream 目录的硬拒绝。
//!
//! 目录名取自 `snow-app-core` 的产品常量，不在本 crate 写死字符串。**严禁读写 upstream 的
//! `SnowShot` 数据目录**（方案 §7 约定 10）：解析出的候选目录若含 `SnowShot` 组件一律拒绝。

use snow_app_core::{APP_ID, PRODUCT_NAME};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 配置文件名。
pub const CONFIG_FILE_NAME: &str = "config.json";
/// 便携模式标记文件名（与 upstream 同名同语义）。
pub const PORTABLE_MARKER_FILE: &str = "__data_directory";
/// upstream 数据目录的组件名（仅用于拒绝，不用于拼路径）。
const UPSTREAM_DIRECTORY_NAME: &str = "SnowShot";
/// 写权限探测文件名后缀（前缀由 `APP_ID` 派生）。
const WRITE_PROBE_SUFFIX: &str = "-write-test";
/// 目录指向 upstream 时的拒绝原因。
const UPSTREAM_REFUSED_MESSAGE: &str =
    "The storage directory points at the upstream application's data";
/// UTF-8 BOM 字符。
const BYTE_ORDER_MARK: char = '\u{feff}';

/// 目标平台（用于目录布局，可在测试中显式指定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows：`%LOCALAPPDATA%\Cisox\`。
    Windows,
    /// macOS：`~/Library/Application Support/Cisox/`。
    MacOs,
    /// Linux：`$XDG_CONFIG_HOME/cisox/`（回落 `~/.config/cisox/`）。
    Linux,
}

impl Platform {
    /// 当前编译目标对应的平台。
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// 存储模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    /// 标准应用数据目录。
    ApplicationData,
    /// 便携模式（`__data_directory` 标记指定的目录）。
    Portable,
    /// 无可用目录：只能内存运行。
    Degraded,
}

/// 目录选择结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDirectorySelection {
    /// 可执行文件所在目录。
    pub executable_directory: PathBuf,
    /// 期望使用的目录（标记指定或标准目录）。
    pub requested_directory: PathBuf,
    /// 实际生效的目录；`Degraded` 时为 `None`。
    pub effective_directory: Option<PathBuf>,
    /// 生效模式。
    pub mode: StorageMode,
    /// 回退原因，无回退时为空。
    pub fallback_reason: String,
}

/// 计算标准应用数据根目录。
///
/// # 参数
/// - `platform`：目标平台
/// - `env`：环境变量读取函数（便于测试注入）
///
/// # 返回
/// Windows 取 `LOCALAPPDATA`，macOS 取 `HOME`，Linux 取 `XDG_CONFIG_HOME` 或 `HOME/.config`；
/// 相应变量缺失时返回 `None`。
///
/// # 示例
/// ```
/// use snow_config::paths::{Platform, app_data_directory};
///
/// let env = |name: &str| (name == "LOCALAPPDATA").then(|| r"C:\Users\u\AppData\Local".to_string());
/// let dir = app_data_directory(Platform::Windows, env).unwrap();
/// assert!(dir.ends_with(snow_app_core::PRODUCT_NAME));
/// ```
pub fn app_data_directory(
    platform: Platform,
    env: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    match platform {
        Platform::Windows => Some(PathBuf::from(non_empty("LOCALAPPDATA")?).join(PRODUCT_NAME)),
        Platform::MacOs => Some(
            PathBuf::from(non_empty("HOME")?)
                .join("Library")
                .join("Application Support")
                .join(PRODUCT_NAME),
        ),
        Platform::Linux => {
            let base = match non_empty("XDG_CONFIG_HOME") {
                Some(xdg) => PathBuf::from(xdg),
                None => PathBuf::from(non_empty("HOME")?).join(".config"),
            };
            Some(base.join(APP_ID))
        }
    }
}

/// 当前进程环境下的标准应用数据根目录。
pub fn default_app_data_directory() -> Option<PathBuf> {
    app_data_directory(Platform::current(), |name| std::env::var(name).ok())
}

/// 路径是否落在 upstream 的 `SnowShot` 目录下（任一组件忽略大小写等于 `SnowShot`）。
///
/// # 示例
/// ```
/// use std::path::Path;
/// use snow_config::paths::is_upstream_location;
///
/// assert!(is_upstream_location(Path::new(r"C:\Users\u\AppData\Local\SnowShot\snow_shot")));
/// assert!(!is_upstream_location(Path::new(r"C:\Users\u\AppData\Local\Cisox")));
/// ```
pub fn is_upstream_location(path: &Path) -> bool {
    path.components().any(|component| match component {
        // Windows 会丢弃组件末尾的点与空格，`SnowShot.` 等价于 `SnowShot`
        Component::Normal(name) => name
            .to_string_lossy()
            .trim_end_matches(['.', ' '])
            .eq_ignore_ascii_case(UPSTREAM_DIRECTORY_NAME),
        _ => false,
    })
}

/// 写权限探测文件名，由 `APP_ID` 派生。
fn write_probe_file_name() -> String {
    format!(".{APP_ID}{WRITE_PROBE_SUFFIX}")
}

/// 判断路径（解析 junction/符号链接后）是否落在 upstream 目录；无法确认时按"是"处理。
///
/// 路径无需存在：对最近的已存在祖先做 canonicalize，再拼回其余组件比较；
/// canonicalize 失败一律返回 `true`（拒绝）。本函数不会创建任何目录。
///
/// # 参数
/// - `path`：待检查的目录路径
///
/// # 返回
/// `true` 表示应拒绝使用该路径。
///
/// # 示例
/// ```
/// use std::path::Path;
/// use snow_config::paths::is_upstream_location_resolved;
///
/// assert!(is_upstream_location_resolved(Path::new("SnowShot./x")));
/// ```
pub fn is_upstream_location_resolved(path: &Path) -> bool {
    if is_upstream_location(path) {
        return true;
    }
    let Ok(absolute) = std::path::absolute(path) else {
        return true;
    };
    let mut existing = absolute.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent;
            }
            _ => return true,
        }
    }
    let Ok(mut resolved) = fs::canonicalize(existing) else {
        return true;
    };
    resolved.extend(tail.into_iter().rev());
    is_upstream_location(&resolved)
}

/// 读取便携标记：返回标记指定目录（相对路径以可执行目录为基准）。
///
/// # 返回
/// `Ok(None)`：无标记文件或标记为空；`Ok(Some(dir))`：标记指定的目录；`Err`：标记存在但读不了。
fn read_marker(executable_directory: &Path) -> Result<Option<PathBuf>, String> {
    let marker = executable_directory.join(PORTABLE_MARKER_FILE);
    if !marker.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&marker)
        .map_err(|_| format!("The {PORTABLE_MARKER_FILE} marker could not be read"))?;
    let text = String::from_utf8_lossy(&bytes);
    let value = text.trim_start_matches(BYTE_ORDER_MARK).trim();
    if value.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(value);
    Ok(Some(if path.is_absolute() {
        path
    } else {
        executable_directory.join(path)
    }))
}

/// 确保目录存在且可写；返回不可用原因。
fn ensure_writable_directory(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("The storage directory path is empty".to_string());
    }
    // 先校验（含 junction/末尾点解析）再创建，避免在 upstream 下留下副作用
    if is_upstream_location_resolved(path) {
        return Err(UPSTREAM_REFUSED_MESSAGE.to_string());
    }
    fs::create_dir_all(path)
        .map_err(|_| "The storage directory could not be created".to_string())?;
    if !path.is_dir() {
        return Err("The storage path is not a directory".to_string());
    }
    // 创建后复核一次，防止创建过程中被替换成 junction
    if is_upstream_location_resolved(path) {
        return Err(UPSTREAM_REFUSED_MESSAGE.to_string());
    }
    let probe = path.join(write_probe_file_name());
    fs::write(&probe, b"").map_err(|_| "The storage directory is not writable".to_string())?;
    let _ = fs::remove_file(probe);
    Ok(())
}

/// 解析生效的数据目录（对应 C++ `ApplicationStorage::resolveDirectory`）。
///
/// 优先便携标记指定目录，不可用时回退到标准目录并记录原因；两者都不可用则降级。
/// 指向 upstream `SnowShot` 目录的候选一律视为不可用。
///
/// # 参数
/// - `executable_directory`：可执行文件所在目录
/// - `app_data_directory`：标准应用数据根目录
///
/// # 示例
/// ```no_run
/// use std::path::Path;
/// use snow_config::paths::{default_app_data_directory, resolve_directory};
///
/// let app_data = default_app_data_directory().unwrap();
/// let selection = resolve_directory(Path::new("."), &app_data);
/// println!("{:?}", selection.effective_directory);
/// ```
pub fn resolve_directory(
    executable_directory: &Path,
    app_data_directory: &Path,
) -> StorageDirectorySelection {
    let (marker_directory, marker_error) = match read_marker(executable_directory) {
        Ok(directory) => (directory, String::new()),
        Err(error) => (None, error),
    };
    let requested = marker_directory
        .clone()
        .unwrap_or_else(|| app_data_directory.to_path_buf());
    let mut selection = StorageDirectorySelection {
        executable_directory: executable_directory.to_path_buf(),
        requested_directory: requested,
        effective_directory: None,
        mode: StorageMode::Degraded,
        fallback_reason: marker_error,
    };

    if let Some(directory) = marker_directory {
        match ensure_writable_directory(&directory) {
            Ok(()) => {
                selection.effective_directory = Some(directory);
                selection.mode = StorageMode::Portable;
                return selection;
            }
            Err(reason) => selection.fallback_reason = reason,
        }
    }
    match ensure_writable_directory(app_data_directory) {
        Ok(()) => {
            selection.effective_directory = Some(app_data_directory.to_path_buf());
            selection.mode = StorageMode::ApplicationData;
        }
        Err(reason) => {
            if !selection.fallback_reason.is_empty() {
                selection.fallback_reason.push(' ');
            }
            selection.fallback_reason.push_str(&format!(
                "The application data directory is unavailable: {reason}"
            ));
        }
    }
    selection
}

/// 数据根下 `config.json` 的完整路径。
pub fn config_file_path(data_directory: &Path) -> PathBuf {
    data_directory.join(CONFIG_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 创建独立的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snow-config-paths-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 三平台数据根与 T1/T2 取值表一致，且绝不含 upstream 目录名。
    #[test]
    fn data_roots_follow_t1_t2_table() {
        let windows = app_data_directory(Platform::Windows, |name| {
            (name == "LOCALAPPDATA").then(|| r"C:\Users\u\AppData\Local".to_string())
        })
        .unwrap();
        assert_eq!(
            windows,
            PathBuf::from(r"C:\Users\u\AppData\Local").join("Cisox")
        );
        let mac = app_data_directory(Platform::MacOs, |name| {
            (name == "HOME").then(|| "/Users/u".to_string())
        })
        .unwrap();
        assert_eq!(
            mac,
            PathBuf::from("/Users/u/Library/Application Support/Cisox")
        );
        let xdg = app_data_directory(Platform::Linux, |name| {
            (name == "XDG_CONFIG_HOME").then(|| "/cfg".to_string())
        })
        .unwrap();
        assert_eq!(xdg, PathBuf::from("/cfg/cisox"));
        let home = app_data_directory(Platform::Linux, |name| {
            (name == "HOME").then(|| "/home/u".to_string())
        })
        .unwrap();
        assert_eq!(home, PathBuf::from("/home/u/.config/cisox"));
        assert!(app_data_directory(Platform::Windows, |_| None).is_none());
        for path in [&windows, &mac, &xdg, &home] {
            assert!(!is_upstream_location(path));
        }
    }

    /// 无标记：使用标准目录并创建。
    #[test]
    fn without_marker_uses_app_data() {
        let root = temp_dir("plain");
        let selection = resolve_directory(&root.join("bin"), &root.join("data"));
        assert_eq!(selection.mode, StorageMode::ApplicationData);
        assert_eq!(selection.effective_directory, Some(root.join("data")));
        assert!(selection.fallback_reason.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// 相对标记：以可执行目录为基准，便携模式生效；带 BOM 的标记同样可读。
    #[test]
    fn relative_marker_selects_portable() {
        let root = temp_dir("portable");
        let exe = root.join("bin");
        fs::create_dir_all(&exe).unwrap();
        fs::write(exe.join(PORTABLE_MARKER_FILE), "\u{feff}portable\n").unwrap();
        let selection = resolve_directory(&exe, &root.join("data"));
        assert_eq!(selection.mode, StorageMode::Portable);
        assert_eq!(selection.effective_directory, Some(exe.join("portable")));
        fs::remove_dir_all(root).unwrap();
    }

    /// 标记指向文件（不可写目录）：回退标准目录并给出原因。
    #[test]
    fn unusable_marker_falls_back() {
        let root = temp_dir("fallback");
        let exe = root.join("bin");
        fs::create_dir_all(&exe).unwrap();
        let blocker = root.join("file-target");
        fs::write(&blocker, b"file").unwrap();
        fs::write(
            exe.join(PORTABLE_MARKER_FILE),
            blocker.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let selection = resolve_directory(&exe, &root.join("data"));
        assert_eq!(selection.mode, StorageMode::ApplicationData);
        assert!(!selection.fallback_reason.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// 硬拒绝：标记或标准目录指向 upstream SnowShot 目录时不创建、不使用。
    #[test]
    fn upstream_directories_are_refused() {
        let root = temp_dir("upstream");
        let exe = root.join("bin");
        fs::create_dir_all(&exe).unwrap();
        let upstream = root.join("SnowShot").join("snow_shot");
        fs::write(
            exe.join(PORTABLE_MARKER_FILE),
            upstream.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let selection = resolve_directory(&exe, &root.join("data"));
        assert_eq!(selection.mode, StorageMode::ApplicationData);
        assert!(!upstream.exists(), "不得为 upstream 目录创建任何内容");
        let degraded = resolve_directory(&root.join("bin2"), &upstream);
        assert_eq!(degraded.mode, StorageMode::Degraded);
        assert!(degraded.effective_directory.is_none() && !upstream.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// 末尾带点/空格的 upstream 组件被拒绝，且不会先创建目录。
    #[test]
    fn trailing_dot_and_space_upstream_refused_without_creating() {
        let root = temp_dir("trail");
        for name in ["SnowShot.", "SnowShot ", "snowshot.. "] {
            let target = root.join(name).join("x");
            assert!(is_upstream_location_resolved(&target));
            assert!(ensure_writable_directory(&target).is_err());
            assert!(!root.join(name).exists(), "不得先创建后校验");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// 正常目录（含尚不存在的深层路径）不被误拒。
    #[test]
    fn ordinary_paths_are_not_upstream() {
        let root = temp_dir("ok");
        assert!(!is_upstream_location_resolved(&root.join("a").join("b")));
        fs::remove_dir_all(root).unwrap();
    }

    /// junction 指向 upstream 目录时被拒绝（仅 Windows）。
    #[cfg(windows)]
    #[test]
    fn junction_to_upstream_is_refused() {
        let root = temp_dir("junction");
        let upstream = root.join("SnowShot");
        fs::create_dir_all(&upstream).unwrap();
        let link = root.join("link");
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&upstream)
            .output()
            .unwrap();
        assert!(output.status.success(), "mklink /J 失败");
        assert!(is_upstream_location_resolved(&link.join("child")));
        assert!(ensure_writable_directory(&link.join("child")).is_err());
        assert!(!upstream.join("child").exists());
        let _ = fs::remove_dir(&link);
        fs::remove_dir_all(root).unwrap();
    }

    /// 探测文件名由 APP_ID 派生。
    #[test]
    fn write_probe_name_derives_from_app_id() {
        assert!(write_probe_file_name().contains(APP_ID));
    }
}
