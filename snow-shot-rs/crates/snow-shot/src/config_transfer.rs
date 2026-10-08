//! 配置导出 / 导入（A06）：设置页“存储”分组里的两个按钮背后的纯逻辑与文案。
//!
//! 归档格式与校验在 [`snow_config::archive`]；本模块负责默认文件名、导出脱敏策略、
//! 导入时的凭据回填与启动项保留、结果文案。文件对话框由 `app_runtime` 调用。

use crate::ocr_backend::i18n_for;
use serde_json::Value;
use snow_config::archive::{
    ArchiveError, iso_utc_now, pin_startup_keys, preserve_omitted_credentials, read_archive_file,
    write_archive_bytes, write_archive_file,
};
use snow_config::schema::current_version;
use snow_config::store::{ConfigStore, ImportError};
use std::path::Path;

/// 设置页里承载导出 / 导入按钮的分组 id。
pub const TRANSFER_GROUP_ID: &str = "storage";
/// 导出文件名前缀。
const EXPORT_FILE_PREFIX: &str = "cisox-configuration-";
/// 归档扩展名（含点）。
pub const ARCHIVE_EXTENSION: &str = ".zip";
/// 归档过滤项匹配模式。
pub const ARCHIVE_PATTERN: &str = "*.zip";
/// 当前应用版本。
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 面板上的两个动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAction {
    /// 导出设置。
    Export {
        /// 是否把自定义模型的 API 密钥一并写入归档（默认不含）。
        include_credentials: bool,
    },
    /// 导入设置。
    Import,
}

/// 导出 / 导入在设置页里的界面状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TransferUiState {
    /// 尚未操作。
    #[default]
    Idle,
    /// 已出结果：本地化文案与是否失败。
    Done {
        /// 已本地化的结果文案。
        text: String,
        /// 是否为失败提示（用警示色）。
        failed: bool,
    },
}

/// 面板文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPanel {
    /// 标题。
    pub title: String,
    /// 说明行。
    pub description: String,
    /// 导出按钮文案。
    pub export_label: String,
    /// 导入按钮文案。
    pub import_label: String,
    /// “包含 API 密钥”勾选项文案。
    pub include_keys_label: String,
    /// 结果提示与是否警示。
    pub notice: Option<(String, bool)>,
}

/// 生成面板文案。
///
/// # 参数
/// - `locale`：界面语言
/// - `state`：当前界面状态
pub fn transfer_panel(locale: &str, state: &TransferUiState) -> TransferPanel {
    let i18n = i18n_for(locale);
    TransferPanel {
        title: i18n.tr("config-transfer-title"),
        description: i18n.tr("config-transfer-description"),
        export_label: i18n.tr("config-transfer-export"),
        import_label: i18n.tr("config-transfer-import"),
        include_keys_label: i18n.tr("config-transfer-include-keys"),
        notice: match state {
            TransferUiState::Idle => None,
            TransferUiState::Done { text, failed } => Some((text.clone(), *failed)),
        },
    }
}

/// 导出对话框的默认文件名，如 `cisox-configuration-20260101-120000.zip`。
///
/// # 参数
/// - `iso`：[`iso_utc_now`] 风格的时间文本
pub fn default_export_name(iso: &str) -> String {
    let digits: String = iso.chars().filter(char::is_ascii_digit).take(14).collect();
    let (date, time) = digits.split_at(digits.len().min(8));
    format!("{EXPORT_FILE_PREFIX}{date}-{time}{ARCHIVE_EXTENSION}")
}

/// 导出：把当前配置写成归档。
///
/// # 参数
/// - `store`：配置存储（只读）
/// - `path`：目标归档路径
/// - `include_credentials`：为假（默认）时清空自定义模型的 API 密钥并记入清单；为真时原样写入
pub fn export_configuration(
    store: &ConfigStore,
    path: &Path,
    include_credentials: bool,
) -> Result<(), ArchiveError> {
    let bytes = write_archive_bytes(
        store.document().values(),
        current_version(),
        APP_VERSION,
        &iso_utc_now(),
        !include_credentials,
    );
    write_archive_file(path, &bytes)
}

/// 导入失败原因。
#[derive(Debug)]
pub enum TransferError {
    /// 归档不合法。
    Archive(ArchiveError),
    /// 配置未能替换（版本、只读或写盘失败）。
    Apply(String),
}

/// 导入：校验归档后原子替换整份配置，返回有变化的键与其旧值（供上层触发生效逻辑）。
///
/// 开机自启与管理员启动两个键保持当前值（由系统事务单独提交，与旧版一致）。
///
/// # 参数
/// - `store`：配置存储
/// - `path`：归档路径
pub fn import_configuration(
    store: &mut ConfigStore,
    path: &Path,
) -> Result<Vec<(String, Value)>, TransferError> {
    let mut contents = read_archive_file(path).map_err(TransferError::Archive)?;
    let before = store.document().values().clone();
    preserve_omitted_credentials(&mut contents, &before);
    pin_startup_keys(&mut contents, &before);
    store
        .import_snapshot(&contents.values, contents.schema_version)
        .map_err(|error| {
            TransferError::Apply(match error {
                ImportError::Rejected(e) => format!("{e:?}"),
                ImportError::Io(e) => e.to_string(),
            })
        })?;
    let after = store.document().values();
    Ok(before
        .into_iter()
        .filter(|(key, old)| after.get(key) != Some(old))
        .collect())
}

/// 把导出结果转成界面状态。
///
/// # 参数
/// - `result`：导出结果
/// - `locale`：界面语言
pub fn export_state(result: &Result<(), ArchiveError>, locale: &str) -> TransferUiState {
    let i18n = i18n_for(locale);
    match result {
        Ok(()) => TransferUiState::Done {
            text: i18n.tr("config-transfer-exported"),
            failed: false,
        },
        Err(error) => archive_error_state(error, locale),
    }
}

/// 把导入结果转成界面状态。
///
/// # 参数
/// - `result`：导入结果（成功时为变化的键数）
/// - `locale`：界面语言
pub fn import_state(result: &Result<usize, TransferError>, locale: &str) -> TransferUiState {
    let i18n = i18n_for(locale);
    match result {
        Ok(_) => TransferUiState::Done {
            text: i18n.tr("config-transfer-imported"),
            failed: false,
        },
        Err(TransferError::Archive(error)) => archive_error_state(error, locale),
        Err(TransferError::Apply(_)) => TransferUiState::Done {
            text: i18n.tr("config-transfer-error-apply"),
            failed: true,
        },
    }
}

/// 归档错误对应的失败状态。
fn archive_error_state(error: &ArchiveError, locale: &str) -> TransferUiState {
    TransferUiState::Done {
        text: i18n_for(locale).tr(error.message_id()),
        failed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    /// 建一个临时目录（不碰上游数据目录）。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cisox-transfer-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 默认文件名由时间戳生成。
    #[test]
    fn export_name_from_timestamp() {
        assert_eq!(
            default_export_name("2026-01-02T03:04:05.006Z"),
            "cisox-configuration-20260102-030405.zip"
        );
    }

    /// 导出后导入另一份配置：值一致，启动项保持目标当前值，返回变化键。
    #[test]
    fn export_then_import() {
        let dir = temp_dir("roundtrip");
        let mut source = ConfigStore::open(dir.join("a").join("config.json"));
        source
            .set_value("screenshot/image_quality", json!(61))
            .unwrap();
        source
            .set_value("system/auto_start_at_boot", json!(false))
            .unwrap();
        let archive = dir.join("out.zip");
        export_configuration(&source, &archive, false).unwrap();

        let mut target = ConfigStore::open(dir.join("b").join("config.json"));
        let changed = import_configuration(&mut target, &archive).unwrap();
        assert_eq!(target.value("screenshot/image_quality"), json!(61));
        assert_eq!(target.value("system/auto_start_at_boot"), json!(true));
        assert!(
            changed
                .iter()
                .any(|(key, _)| key == "screenshot/image_quality")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// 坏归档导入失败，现有配置与磁盘文件保持不变。
    #[test]
    fn bad_archive_keeps_config() {
        let dir = temp_dir("bad");
        let path = dir.join("config.json");
        let mut store = ConfigStore::open(&path);
        store
            .set_value("screenshot/image_quality", json!(42))
            .unwrap();
        store.flush().unwrap();
        let original = fs::read(&path).unwrap();
        let bad = dir.join("bad.zip");
        fs::write(&bad, b"PK garbage").unwrap();
        assert!(matches!(
            import_configuration(&mut store, &bad),
            Err(TransferError::Archive(ArchiveError::Invalid))
        ));
        assert!(import_configuration(&mut store, &dir.join("none.zip")).is_err());
        assert_eq!(store.value("screenshot/image_quality"), json!(42));
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    /// 脱敏导出：归档里没有密钥，导入时从当前配置补回。
    #[test]
    fn credentials_redacted_and_restored() {
        let dir = temp_dir("cred");
        let mut store = ConfigStore::open(dir.join("config.json"));
        let models = json!([{
            "id": "0a1b2c3d-0000-4000-8000-000000000001",
            "name": "m",
            "base_url": "https://example.com/v1",
            "api_key": "secret-key",
            "model": "x",
            "supports_vision": false,
            "supports_reasoning": false
        }]);
        store
            .set_value("api_configuration/custom_models", models)
            .unwrap();
        let archive = dir.join("out.zip");
        export_configuration(&store, &archive, false).unwrap();
        assert!(!String::from_utf8_lossy(&fs::read(&archive).unwrap()).contains("secret-key"));
        import_configuration(&mut store, &archive).unwrap();
        let after = store.value("api_configuration/custom_models");
        assert_eq!(after[0]["api_key"], json!("secret-key"));
        fs::remove_dir_all(dir).unwrap();
    }

    /// 勾选“包含密钥”导出：归档里带密钥，且清单不记脱敏 id；导入到空配置也能带回密钥。
    #[test]
    fn credentials_included_when_requested() {
        let dir = temp_dir("cred-in");
        let mut store = ConfigStore::open(dir.join("a").join("config.json"));
        let models = json!([{
            "id": "0a1b2c3d-0000-4000-8000-000000000001",
            "name": "m",
            "base_url": "https://example.com/v1",
            "api_key": "secret-key",
            "model": "x",
            "supports_vision": false,
            "supports_reasoning": false
        }]);
        store
            .set_value("api_configuration/custom_models", models)
            .unwrap();
        let archive = dir.join("out.zip");
        export_configuration(&store, &archive, true).unwrap();
        let contents = read_archive_file(&archive).unwrap();
        assert!(contents.redacted_credential_ids.is_empty());
        let mut target = ConfigStore::open(dir.join("b").join("config.json"));
        import_configuration(&mut target, &archive).unwrap();
        let after = target.value("api_configuration/custom_models");
        assert_eq!(after[0]["api_key"], json!("secret-key"));
        fs::remove_dir_all(dir).unwrap();
    }

    /// 两种语言的面板文案都完整，错误文案已本地化（不是消息 id）。
    #[test]
    fn panel_texts_exist() {
        for locale in ["en-US", "zh-CN"] {
            let panel = transfer_panel(locale, &TransferUiState::Idle);
            assert!(!panel.title.is_empty() && !panel.export_label.is_empty());
            assert!(!panel.include_keys_label.is_empty());
            for error in [
                ArchiveError::Invalid,
                ArchiveError::NotConfigArchive,
                ArchiveError::TooNew,
                ArchiveError::NoCompatibleSettings,
                ArchiveError::UnsupportedCompression,
                ArchiveError::Io(String::new()),
            ] {
                assert!(matches!(
                    export_state(&Err(error), locale),
                    TransferUiState::Done { failed: true, ref text } if !text.starts_with("config-transfer")
                ));
            }
        }
    }
}
