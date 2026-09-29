//! 高频配置的强类型视图（历史记录保留策略等）。
//!
//! 值一律来自已修复的 [`ConfigDocument`]，因此范围与枚举合法性已由 schema 保证。

use crate::document::ConfigDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 历史分组名：截图历史。
pub const CAPTURE_HISTORY_GROUP: &str = "capture_history";
/// 历史分组名：贴图历史。
pub const PINNED_HISTORY_GROUP: &str = "pinned_history";

/// 历史记录保留策略（`capture_history/*` 与 `pinned_history/*` 共用同一形状）。
///
/// # 示例
/// ```
/// use snow_config::document::ConfigDocument;
/// use snow_config::typed::{CAPTURE_HISTORY_GROUP, HistoryPolicy};
///
/// let doc = ConfigDocument::from_bytes(None);
/// let policy = HistoryPolicy::from_document(&doc, CAPTURE_HISTORY_GROUP);
/// assert_eq!(policy.retention_days, 7);
/// assert_eq!(policy.max_entries, 100);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPolicy {
    /// 是否启用历史记录。
    pub enabled: bool,
    /// 是否永久保留（不清理）。
    pub keep_permanently: bool,
    /// 压缩等级：`low`/`medium`/`high`。
    pub compression_level: String,
    /// 保留天数（1..=365）。
    pub retention_days: i64,
    /// 最大条目数（1..=1000）。
    pub max_entries: i64,
    /// 最大磁盘占用 MiB（128..=10240）。
    pub max_disk_mib: i64,
}

impl HistoryPolicy {
    /// 从文档读取指定分组的策略。
    ///
    /// # 参数
    /// - `document`：已加载的配置文档
    /// - `group`：`capture_history` 或 `pinned_history`
    pub fn from_document(document: &ConfigDocument, group: &str) -> Self {
        let read = |name: &str| document.value(&format!("{group}/{name}"));
        let integer = |name: &str| read(name).as_i64().unwrap_or_default();
        let text = |name: &str| read(name).as_str().unwrap_or_default().to_string();
        let flag = |name: &str| matches!(read(name), Value::Bool(true));
        Self {
            enabled: flag("enabled"),
            keep_permanently: flag("keep_permanently"),
            compression_level: text("compression_level"),
            retention_days: integer("retention_days"),
            max_entries: integer("max_entries"),
            max_disk_mib: integer("max_disk_mib"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实样本读出的策略与方案 B.2 表一致，且序列化字段名即磁盘键名。
    #[test]
    fn policy_from_real_sample() {
        let sample = include_str!("../tests/fixtures/config.json");
        let doc = ConfigDocument::from_bytes(Some(sample.as_bytes()));
        for group in [CAPTURE_HISTORY_GROUP, PINNED_HISTORY_GROUP] {
            let policy = HistoryPolicy::from_document(&doc, group);
            assert!(policy.enabled && !policy.keep_permanently);
            assert_eq!(
                (
                    policy.retention_days,
                    policy.max_entries,
                    policy.max_disk_mib
                ),
                (7, 100, 1024)
            );
            assert_eq!(policy.compression_level, "medium");
        }
        let json = serde_json::to_value(HistoryPolicy::from_document(&doc, CAPTURE_HISTORY_GROUP))
            .unwrap();
        assert_eq!(json["max_disk_mib"], 1024);
    }

    /// 越界值回退默认后策略仍在合法范围内。
    #[test]
    fn policy_falls_back_for_out_of_range() {
        let doc = ConfigDocument::from_bytes(Some(
            br#"{"storage":{"schema_version":3},"capture_history":{"retention_days":0,"max_disk_mib":5}}"#,
        ));
        let policy = HistoryPolicy::from_document(&doc, CAPTURE_HISTORY_GROUP);
        assert_eq!((policy.retention_days, policy.max_disk_mib), (7, 1024));
    }
}
