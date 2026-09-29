//! 仓储共用的文件系统工具：原子写、路径包含校验、安全文件名。
//!
//! 对照 C++ `writeFile`（QSaveFile）、`containedPath`、`safeFileName`。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 原子写：先写同目录临时文件并落盘，再改名覆盖目标；失败时清掉临时文件。
///
/// # 参数
/// - `path`：目标文件；父目录必须已存在。
/// - `bytes`：完整内容。
///
/// # 示例
/// ```
/// let dir = std::env::temp_dir().join("snow-history-doc-write");
/// std::fs::create_dir_all(&dir).unwrap();
/// let file = dir.join("a.json");
/// snow_history::fsutil::write_atomic(&file, b"{}").unwrap();
/// assert_eq!(std::fs::read(&file).unwrap(), b"{}");
/// ```
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 比较两个已规范化路径是否相同；Windows 下忽略大小写。
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// 判断 `path` 是否位于 `root` 之内（含 `root` 自身）；仅在真实 I/O 边界前调用。
///
/// 规则同 C++ `containedPath`：`root` 必须存在；`path` 自身是符号链接/联接点则拒绝；
/// 存在时比较其规范路径，不存在时比较父目录的规范路径。
///
/// # 参数
/// - `root`：允许范围的根目录。
/// - `path`：待校验路径。
///
/// # 返回
/// 在范围内返回 `true`。
///
/// # 示例
/// ```
/// let dir = std::env::temp_dir().join("snow-history-doc-contained");
/// std::fs::create_dir_all(&dir).unwrap();
/// assert!(snow_history::fsutil::contained_path(&dir, &dir.join("new.bin")));
/// assert!(!snow_history::fsutil::contained_path(&dir, &dir.join("..").join("x")));
/// ```
pub fn contained_path(root: &Path, path: &Path) -> bool {
    let Ok(canonical_root) = fs::canonicalize(root) else {
        return false;
    };
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let canonical = if path.exists() {
        fs::canonicalize(path)
    } else {
        match path.parent() {
            Some(parent) => fs::canonicalize(parent),
            None => return false,
        }
    };
    let Ok(canonical) = canonical else {
        return false;
    };
    same_path(&canonical, &canonical_root) || {
        // 组件级前缀比较，避免 `/a/bc` 误判为 `/a/b` 的子路径。
        let lowered = |p: &Path| {
            p.components()
                .map(|c| {
                    let text = c.as_os_str().to_string_lossy().into_owned();
                    if cfg!(windows) {
                        text.to_lowercase()
                    } else {
                        text
                    }
                })
                .collect::<Vec<_>>()
        };
        let (child, parent) = (lowered(&canonical), lowered(&canonical_root));
        child.len() > parent.len() && child[..parent.len()] == parent[..]
    }
}

/// 文件名是否安全：非空、非 `.`/`..`、无路径分隔符与盘符冒号、非绝对路径。
///
/// # 示例
/// ```
/// use snow_history::fsutil::safe_file_name;
/// assert!(safe_file_name("display_0.png"));
/// assert!(!safe_file_name("../outside.png"));
/// ```
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !Path::new(name).is_absolute()
        && !name.contains(['/', '\\', ':'])
        && Path::new(name).file_name().is_some_and(|n| n == name)
}
