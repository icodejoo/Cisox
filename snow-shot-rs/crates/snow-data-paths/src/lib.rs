//! 数据根目录解析与产品常量（零依赖）。
//!
//! 托盘宿主与 UI 进程都要用同一套数据根解析（便携标记 `__data_directory`、AppData 目录、
//! upstream 目录保护），所以从 `snow-config` 抽到这里，避免宿主为了几个路径函数链接配置层
//! （方案：`docs/design/tray-host-split.md` §3）。`snow-config::paths` 与
//! `snow-app-core::{APP_ID, PRODUCT_NAME}` 均为本 crate 的重导出，原路径不变。

pub mod paths;

/// 产品显示名，唯一来源；禁止写入可翻译字符串（约定 11）。
pub const PRODUCT_NAME: &str = "Cisox";

/// 通用应用标识符（数据目录、锁名等由此派生）。
pub const APP_ID: &str = "cisox";

/// 主实例互斥体与控制管道使用的应用标识（托盘宿主、`--resident` 的 UI 进程共用，所以两种模式互斥）。
pub const SINGLE_INSTANCE_APP_ID: &str = "cisox.snow_shot.single_instance";

/// UI 进程自己的互斥体标识：同一时刻只允许一个 UI 进程，新拉起的重复 UI 直接退出。
pub const UI_INSTANCE_APP_ID: &str = "cisox.snow_shot.ui";

/// 会话管道标识前缀；完整标识为 `<前缀>.<一次性 launch_id>`。
pub const SESSION_PIPE_APP_ID_PREFIX: &str = "cisox.snow_shot.session";

/// 数据根下的日志目录名。
pub const LOG_DIR_NAME: &str = "logs";

/// 日志目录下的崩溃转储子目录名。
pub const CRASH_DIR_NAME: &str = "crash";

/// 数据根下宿主相关文件的目录名。
pub const HOST_DIR_NAME: &str = "host";

/// 宿主快照文件名（位于 [`HOST_DIR_NAME`] 下，UI 写、宿主读）。
pub const HOST_SPEC_FILE_NAME: &str = "spec.json";

/// 宿主日志文件名（位于 [`LOG_DIR_NAME`] 下，限大小滚动）。
pub const HOST_LOG_FILE_NAME: &str = "host.log";

/// 宿主快照的完整路径：`<数据根>/host/spec.json`。
///
/// # 参数
/// - `data_root`：数据根目录。
///
/// # 返回
/// 快照文件路径（不检查是否存在）。
///
/// ```rust
/// use std::path::Path;
/// let path = snow_data_paths::host_spec_path(Path::new("data"));
/// assert!(path.ends_with("host/spec.json") || path.ends_with("host\\spec.json"));
/// ```
pub fn host_spec_path(data_root: &std::path::Path) -> std::path::PathBuf {
    data_root.join(HOST_DIR_NAME).join(HOST_SPEC_FILE_NAME)
}

/// 宿主日志的完整路径：`<数据根>/logs/host.log`。
///
/// # 参数
/// - `data_root`：数据根目录。
///
/// # 返回
/// 日志文件路径（不检查是否存在）。
///
/// ```rust
/// use std::path::Path;
/// let path = snow_data_paths::host_log_path(Path::new("data"));
/// assert!(path.ends_with("host.log"));
/// ```
pub fn host_log_path(data_root: &std::path::Path) -> std::path::PathBuf {
    data_root.join(LOG_DIR_NAME).join(HOST_LOG_FILE_NAME)
}

/// 测试公用夹具（仅测试编译）。
#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 创建本进程内唯一的临时目录（进程 ID + 计数，避免并发冲突），调用方负责清理。
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snow-data-paths-test-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 产品常量应与方案 T1/T6 取值一致。
    #[test]
    fn product_constants() {
        assert_eq!(PRODUCT_NAME, "Cisox");
        assert_eq!(APP_ID, "cisox");
    }

    /// 宿主文件路径落在数据根的约定子目录下，文件名固定。
    #[test]
    fn host_paths_follow_layout() {
        let root = std::path::Path::new("root");
        assert_eq!(host_spec_path(root), root.join("host").join("spec.json"));
        assert_eq!(host_log_path(root), root.join("logs").join("host.log"));
    }

    /// 三个互斥体 / 管道标识互不相同，且都符合管道名的字符限制（字母数字与 `._-`）。
    #[test]
    fn instance_ids_are_distinct_and_pipe_safe() {
        let ids = [
            SINGLE_INSTANCE_APP_ID,
            UI_INSTANCE_APP_ID,
            SESSION_PIPE_APP_ID_PREFIX,
        ];
        for (i, a) in ids.iter().enumerate() {
            assert!(
                a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            );
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// 临时目录夹具产生的目录互不相同。
    #[test]
    fn temp_dirs_are_unique() {
        let a = test_util::temp_dir("a");
        let b = test_util::temp_dir("a");
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
