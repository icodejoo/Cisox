//! 测试共用小工具（仅测试编译）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// 进程内自增序号，保证同一纳秒内并行调用也不撞名。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 创建并返回一个全局唯一的临时目录（系统临时目录下）。
///
/// 目录名为 `{prefix}-{tag}-{进程号}-{纳秒}-{序号}`，并行测试与进程号复用都不会撞名。
///
/// # 参数
/// - `prefix`：目录名前缀，便于排查是哪个模块留下的。
/// - `tag`：区分同一模块内不同用例的标记。
///
/// # 返回
/// 已创建好的空目录路径；创建失败直接 panic（测试环境）。
///
/// # 示例
/// ```ignore
/// let dir = unique_temp_dir("cisox-demo", "case1");
/// assert!(dir.is_dir());
/// ```
pub fn unique_temp_dir(prefix: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{tag}-{}-{}-{}",
        std::process::id(),
        snow_platform::unix_time::unix_nanos(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连续两次调用得到不同且已存在的目录。
    #[test]
    fn dirs_are_unique_and_created() {
        let a = unique_temp_dir("snow-test-util", "x");
        let b = unique_temp_dir("snow-test-util", "x");
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
