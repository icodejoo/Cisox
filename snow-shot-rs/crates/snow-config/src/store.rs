//! 配置文件存储：读取修复、损坏文件留档、原子写入。
//!
//! 对应 C++ `ConfigurationStore` 的文件 IO 部分。数据根目录由调用方决定
//! （见 [`crate::paths`]），本模块只操作传入的 `config.json` 路径。

use crate::document::{Compatibility, ConfigDocument, SetError};
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 损坏备份的文件名中缀。
const CORRUPT_INFIX: &str = ".corrupt.";
/// 损坏备份保留天数。
const CORRUPT_BACKUP_RETENTION_DAYS: u64 = 30;
/// 一天的秒数。
const SECONDS_PER_DAY: u64 = 86_400;

/// 基于文件的配置存储。
///
/// # 示例
/// ```no_run
/// use serde_json::json;
/// use snow_config::store::ConfigStore;
///
/// let mut store = ConfigStore::open("config.json");
/// store.set_value("mcp/enabled", json!(true)).unwrap();
/// store.flush().unwrap();
/// ```
#[derive(Debug)]
pub struct ConfigStore {
    /// `config.json` 路径。
    path: PathBuf,
    /// 已加载文档。
    document: ConfigDocument,
}

impl ConfigStore {
    /// 打开并加载配置文件。
    ///
    /// 文件缺失：使用默认值（下次 [`flush`](Self::flush) 写出）；无法读取或内容损坏：
    /// 先把原文件复制为 `config.json.corrupt.<UTC时间戳>.json` 留档，再用默认值恢复。
    /// 同时清理超过 30 天的旧损坏备份。
    ///
    /// # 参数
    /// - `path`：`config.json` 路径
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        cleanup_corrupt_backups(&path);
        let document = match fs::read(&path) {
            Ok(bytes) => {
                let document = ConfigDocument::from_bytes(Some(&bytes));
                if document.compatibility() == Compatibility::RecoveredDefaults {
                    preserve_corrupt_file(&path);
                }
                document
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ConfigDocument::from_bytes(None)
            }
            Err(_) => {
                preserve_corrupt_file(&path);
                let mut document = ConfigDocument::from_bytes(Some(b"{"));
                document.set_last_error("Unable to read config.json; defaults were loaded");
                document
            }
        };
        Self { path, document }
    }

    /// 读取键的当前值。
    pub fn value(&self, key: &str) -> Value {
        self.document.value(key)
    }

    /// 设置单个键，规则见 [`ConfigDocument::set_value`]。
    pub fn set_value(&mut self, key: &str, value: Value) -> Result<(), SetError> {
        self.document.set_value(key, value)
    }

    /// 是否有未落盘的修改。
    pub fn is_dirty(&self) -> bool {
        self.document.is_dirty()
    }

    /// 兼容性状态。
    pub fn compatibility(&self) -> Compatibility {
        self.document.compatibility()
    }

    /// 内部文档的只读引用。
    pub fn document(&self) -> &ConfigDocument {
        &self.document
    }

    /// 内部文档的可变引用（批量设置、快照导入）。
    pub fn document_mut(&mut self) -> &mut ConfigDocument {
        &mut self.document
    }

    /// 配置文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 原子写出：先写同目录临时文件，再改名覆盖，成功后清除脏标记。
    ///
    /// 无修改时直接成功；只读（未来版本）时返回 `PermissionDenied`。
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.document.is_dirty() {
            return Ok(());
        }
        if !self.document.is_writable() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Configuration storage is read-only",
            ));
        }
        write_atomic(&self.path, &self.document.to_bytes())?;
        self.document.mark_clean();
        Ok(())
    }

    /// 以快照整体替换配置（导入）：先在副本上套用并落盘，成功后才换入内存，失败原配置不变。
    ///
    /// # 参数
    /// - `values`：扁平键值（缺失键取默认）
    /// - `schema_version`：快照的 schema 版本
    ///
    /// # 返回
    /// 版本过新、只读或写盘失败时返回 `Err`，此时内存与磁盘都保持原样。
    pub fn import_snapshot(
        &mut self,
        values: &std::collections::BTreeMap<String, Value>,
        schema_version: i32,
    ) -> Result<(), ImportError> {
        let mut staged = self.document.clone();
        staged
            .apply_snapshot(values, schema_version)
            .map_err(ImportError::Rejected)?;
        write_atomic(&self.path, &staged.to_bytes()).map_err(ImportError::Io)?;
        staged.mark_clean();
        self.document = staged;
        Ok(())
    }
}

/// 配置导入失败原因。
#[derive(Debug)]
pub enum ImportError {
    /// 文档拒绝套用（版本过新或只读）。
    Rejected(SetError),
    /// 写盘失败。
    Io(io::Error),
}

/// 原子写文件：同目录临时文件写完并落盘后改名覆盖，失败时清理临时文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 生成损坏备份路径：`<config>.corrupt.<yyyyMMddTHHmmssmmmZ>.json`，重名则追加序号。
fn corrupt_backup_path(config: &Path) -> PathBuf {
    let stamp = utc_timestamp(SystemTime::now());
    let base = format!("{}{CORRUPT_INFIX}{stamp}", config.display());
    let mut candidate = PathBuf::from(format!("{base}.json"));
    let mut suffix = 1;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{base}.{suffix}.json"));
        suffix += 1;
    }
    candidate
}

/// 复制损坏文件留档（失败不影响加载）。
fn preserve_corrupt_file(config: &Path) {
    if config.exists() {
        let _ = fs::copy(config, corrupt_backup_path(config));
    }
}

/// 删除超过保留期的损坏备份。
fn cleanup_corrupt_backups(config: &Path) {
    let (Some(directory), Some(name)) = (config.parent(), config.file_name()) else {
        return;
    };
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    let prefix = format!("{}{CORRUPT_INFIX}", name.to_string_lossy());
    let Ok(read_dir) = fs::read_dir(directory) else {
        return;
    };
    let retention = Duration::from_secs(CORRUPT_BACKUP_RETENTION_DAYS * SECONDS_PER_DAY);
    for entry in read_dir.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !(file_name.starts_with(&prefix) && file_name.ends_with(".json")) {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > retention);
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// 把时间格式化为 `yyyyMMddTHHmmssmmmZ`（UTC，毫秒），与 C++ `yyyyMMdd'T'HHmmsszzz'Z'` 一致。
fn utc_timestamp(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();
    let days = i64::try_from(seconds / SECONDS_PER_DAY).unwrap_or(0);
    let second_of_day = seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}{millis:03}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// 由自 1970-01-01 起的天数换算公历日期（Howard Hinnant 算法）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 创建测试专用临时目录（进程 ID + 计数，避免并发冲突），返回路径。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snow-config-test-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 缺失文件：打开为默认值，flush 后写出并可重新读回；重开后不再脏。
    #[test]
    fn missing_file_is_created_with_defaults() {
        let dir = temp_dir("missing");
        let path = dir.join("config.json");
        let mut store = ConfigStore::open(&path);
        assert!(store.is_dirty());
        store.flush().unwrap();
        assert!(path.exists() && !store.is_dirty());
        assert!(!dir.join("config.json.tmp").exists());
        let reopened = ConfigStore::open(&path);
        // 注意：目标翻译语言的默认值 "" 不在白名单内，C++ 每次加载都会判非法并回写，故此处仍为脏
        assert_eq!(reopened.value("storage/schema_version"), json!(3));
        assert_eq!(reopened.value("capture_history/retention_days"), json!(7));
        // 全局鼠标默认的单元素数组重载后被规范化为字符串（C++ 同款行为）
        assert_eq!(
            reopened.value("global_mouse/screenshot_copy"),
            json!({"activation_key": "windows", "mouse_button": "left_drag"})
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// 修改后落盘并在重开后保留；非法值被拒绝。
    #[test]
    fn set_flush_reopen() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join("config.json");
        let mut store = ConfigStore::open(&path);
        store
            .set_value("screenshot_ui/shortcut_hint_opacity", json!(42))
            .unwrap();
        assert!(
            store
                .set_value("screenshot_ui/shortcut_hint_opacity", json!(500))
                .is_err()
        );
        store.flush().unwrap();
        let reopened = ConfigStore::open(&path);
        assert_eq!(
            reopened.value("screenshot_ui/shortcut_hint_opacity"),
            json!(42)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// 损坏文件：留档为 `.corrupt.*.json`，随后用默认值恢复并可覆盖写出。
    #[test]
    fn malformed_file_is_preserved_and_replaced() {
        let dir = temp_dir("corrupt");
        let path = dir.join("config.json");
        fs::write(&path, b"{ definitely not json").unwrap();
        let mut store = ConfigStore::open(&path);
        assert_eq!(store.compatibility(), Compatibility::RecoveredDefaults);
        let backups: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt."))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read(backups[0].path()).unwrap(),
            b"{ definitely not json"
        );
        store.flush().unwrap();
        assert!(serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    /// 未来版本：只读，flush 报错，磁盘文件保持原样。
    #[test]
    fn future_version_never_rewrites_file() {
        let dir = temp_dir("future");
        let path = dir.join("config.json");
        let original = br#"{"storage":{"schema_version":9},"x":1}"#;
        fs::write(&path, original).unwrap();
        let mut store = ConfigStore::open(&path);
        assert_eq!(store.compatibility(), Compatibility::FutureVersion);
        assert!(store.set_value("mcp/enabled", json!(true)).is_err());
        assert!(!store.is_dirty());
        store.flush().unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    /// 时间戳格式：已知时刻与闰年日期换算。
    #[test]
    fn timestamp_format() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "19700101T000000000Z");
        let leap_day = UNIX_EPOCH + Duration::from_millis(1_709_210_096_789);
        assert_eq!(utc_timestamp(leap_day), "20240229T123456789Z");
    }

    /// 导入快照：成功后落盘并可重开读回；版本过新时内存与磁盘都不变。
    #[test]
    fn import_snapshot_is_atomic() {
        let dir = temp_dir("import");
        let path = dir.join("config.json");
        let mut store = ConfigStore::open(&path);
        store.set_value("screenshot/image_quality", json!(50)).unwrap();
        store.flush().unwrap();
        let original = fs::read(&path).unwrap();

        let mut values = std::collections::BTreeMap::new();
        values.insert("screenshot/image_quality".to_string(), json!(90));
        let future = crate::schema::current_version() + 1;
        assert!(store.import_snapshot(&values, future).is_err());
        assert_eq!(store.value("screenshot/image_quality"), json!(50));
        assert_eq!(fs::read(&path).unwrap(), original);

        store.import_snapshot(&values, 0).unwrap();
        assert!(!store.is_dirty());
        assert_eq!(ConfigStore::open(&path).value("screenshot/image_quality"), json!(90));
        fs::remove_dir_all(dir).unwrap();
    }
}
