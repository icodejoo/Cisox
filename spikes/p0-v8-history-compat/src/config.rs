//! 配置文件 config.json 的 Rust 类型定义与校验
use serde::{Deserialize, Serialize};
use crate::Extra;

/// 截图历史配置 (capture_history)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureHistoryConfig {
    pub enabled: bool,
    pub keep_permanently: bool,
    pub compression_level: String,
    pub retention_days: i64,
    pub max_entries: i64,
    pub max_disk_mib: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 存储配置 (storage)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageConfig {
    pub schema_version: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 顶层配置文件文档
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub capture_history: CaptureHistoryConfig,
    pub storage: StorageConfig,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 验证配置时的错误枚举，用于区分 FutureVersion
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    FutureVersion,
    InvalidField(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::FutureVersion => write!(f, "Future schema version ( > 3 )"),
            ConfigError::InvalidField(msg) => write!(f, "Invalid config field: {}", msg),
        }
    }
}

/// 当前期望的 config 存储版本
pub const CONFIG_CURRENT_VERSION: i64 = 3;

/// 从字节读取并解析配置
pub fn load_config(bytes: &[u8]) -> Result<ConfigDocument, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

/// 序列化为符合 Qt 风格的缩进 JSON（4 个空格，末尾换行）
pub fn save_config(cfg: &ConfigDocument) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(cfg).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&value, &mut ser).map_err(|e| e.to_string())?;
    // Qt JSON 会在末尾多一个换行符
    buf.push(b'\n');
    Ok(buf)
}

/// 校验配置。越界值仅返回 Err，不会像 C++ 那样回退默认值。
pub fn validate_config(cfg: &ConfigDocument) -> Result<(), ConfigError> {
    if cfg.storage.schema_version < 1 {
        return Err(ConfigError::InvalidField("schema_version < 1".into()));
    }
    if cfg.storage.schema_version > 2147483647 {
        return Err(ConfigError::InvalidField("schema_version > INT_MAX".into()));
    }
    if cfg.storage.schema_version > CONFIG_CURRENT_VERSION {
        return Err(ConfigError::FutureVersion);
    }

    let ch = &cfg.capture_history;
    if ch.retention_days < 1 || ch.retention_days > 365 {
        return Err(ConfigError::InvalidField("retention_days 越界 [1, 365]".into()));
    }
    if ch.max_entries < 1 || ch.max_entries > 1000 {
        return Err(ConfigError::InvalidField("max_entries 越界 [1, 1000]".into()));
    }
    if ch.max_disk_mib < 128 || ch.max_disk_mib > 10240 {
        return Err(ConfigError::InvalidField("max_disk_mib 越界 [128, 10240]".into()));
    }
    if ch.compression_level != "low" && ch.compression_level != "medium" && ch.compression_level != "high" {
        return Err(ConfigError::InvalidField("compression_level 非法".into()));
    }

    Ok(())
}
