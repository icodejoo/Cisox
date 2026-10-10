//! 原子写文件：同目录唯一临时文件 + 落盘 + 改名覆盖。

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 进程内临时文件计数，配合 pid 保证同目录临时名唯一。
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成同目录唯一临时文件路径：`.<原文件名>.<pid>.<计数>.tmp`。
fn temp_path_for(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".{}.{seq}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// 原子写文件：写同目录唯一临时文件并 `sync_all`，再改名覆盖目标；失败时清理临时文件。
///
/// 不会自动创建父目录，目录不存在时返回 `NotFound` 等系统错误。
///
/// # 参数
/// - `path`：目标文件路径（父目录须已存在）。
/// - `bytes`：完整文件内容。
///
/// # 返回
/// 成功为 `Ok(())`；任一步失败返回对应 `io::Error`，且不残留临时文件。
///
/// # 示例
/// ```
/// let dir = std::env::temp_dir().join("snow-platform-doc-atomic");
/// std::fs::create_dir_all(&dir).unwrap();
/// let file = dir.join("a.json");
/// snow_platform::atomic_file::atomic_write(&file, b"{}").unwrap();
/// assert_eq!(std::fs::read(&file).unwrap(), b"{}");
/// ```
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temp_path_for(path);
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

#[cfg(test)]
mod tests {
    use super::atomic_write;

    /// 新建测试用临时目录。
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-atomic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 成功写入、覆盖旧内容，且目录里不残留临时文件。
    #[test]
    fn overwrites_and_leaves_no_temp() {
        let dir = scratch("ok");
        let file = dir.join("a.bin");
        atomic_write(&file, b"one").unwrap();
        atomic_write(&file, b"two-two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two-two");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 父目录不存在时报错，不创建目录。
    #[test]
    fn missing_parent_is_error() {
        let dir = scratch("missing");
        let file = dir.join("nope").join("a.bin");
        assert!(atomic_write(&file, b"x").is_err());
        assert!(!dir.join("nope").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目标是目录时改名失败，临时文件被清理。
    #[test]
    fn failed_rename_cleans_temp() {
        let dir = scratch("fail");
        let target = dir.join("t");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"1").unwrap();
        assert!(atomic_write(&target, b"x").is_err());
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
