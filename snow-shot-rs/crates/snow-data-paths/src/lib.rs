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
